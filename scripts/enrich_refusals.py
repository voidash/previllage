#!/usr/bin/env python3
"""Rewrite the 92 refusal drafts to include real contact details for the
relevant gov office plus the universal Hello Sarkar 1111 fallback.

For each refusal item:
  1. Identify the primary office mentioned in the existing draft (first gov.np
     domain appearing in the answer text).
  2. Look up that office in `corpora/gov_office_contacts.json` (with alias
     resolution — e.g. `passport.gov.np` → `nepalpassport.gov.np`).
  3. Send the question + retrieved chunks + office contact info to Sonnet via
     Meridian, asking it to compose a refusal answer in the question's
     language using a fixed template (refusal phrase → office name + phone
     + URL → "phone unreachable → website" → "all else fails → 1111").
  4. Identity-scrub and write to `eval/gov_helpdesk_v1_drafts.jsonl`,
     overwriting only the refusal items (grounded items left intact).

Usage:
    python scripts/enrich_refusals.py --limit 5     # smoke test
    python scripts/enrich_refusals.py               # all 92 refusals
"""
from __future__ import annotations

import argparse
import json
import logging
import os
import re
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from threading import Lock
from typing import Optional

MERIDIAN_URL = os.environ.get("MERIDIAN_URL", "http://127.0.0.1:3456")
MODEL = os.environ.get("ENRICHER_MODEL", "claude-sonnet-4-6")
CONTACTS_PATH = Path("corpora/gov_office_contacts.json")
DRAFTS_PATH = Path("eval/gov_helpdesk_v1_drafts.jsonl")
MAX_RETRIES = 3
TIMEOUT_S = 60

DOMAIN_RE = re.compile(
    r"(?:https?://)?(?:www\.)?([a-zA-Z0-9_-]+(?:\.[a-zA-Z0-9_-]+)*\.(?:gov\.np|org\.np|edu\.np|com\.np|net\.np))",
    re.I,
)


def load_contacts() -> tuple[dict, dict]:
    """Load contacts; return (canonical_dict, alias_to_canonical map)."""
    contacts = json.loads(CONTACTS_PATH.read_text(encoding="utf-8"))
    aliases: dict[str, str] = {}
    for canonical, info in contacts.items():
        if canonical.startswith("_"):
            continue
        for a in info.get("aliases", []) or []:
            aliases[a.lower()] = canonical
    return contacts, aliases


def find_primary_office(text: str, contacts: dict, aliases: dict) -> str | None:
    """Find the first matching gov.np domain in the draft text. Returns
    canonical key into `contacts`, or None if no known office is mentioned."""
    if not text:
        return None
    for m in DOMAIN_RE.finditer(text):
        d = m.group(1).lower()
        # Strip leading subdomain that doesn't match (e.g. "old.mof.gov.np" → "mof.gov.np")
        # Try exact, then dropping leading labels until we find a known canonical/alias.
        candidate = d
        while True:
            if candidate in contacts and not candidate.startswith("_"):
                return candidate
            if candidate in aliases:
                return aliases[candidate]
            if "." not in candidate:
                break
            candidate = candidate.split(".", 1)[1]
    return None


def office_brief(canonical: str, contacts: dict) -> str:
    """Render an office's contact info as a concise prompt-ready brief
    (Markdown). Sonnet will weave this into the answer. Empty fields skipped."""
    info = contacts[canonical]
    lines = [
        f"office_id: {canonical}",
        f"name_np: {info.get('name_np','')}",
        f"name_en: {info.get('name_en','')}",
        f"url: {info.get('url','')}",
    ]
    if info.get("address_np"):
        lines.append(f"address_np: {info['address_np']}")
    phones = info.get("phones", [])
    if phones:
        lines.append("phones:")
        for p in phones:
            lines.append(
                f"  - label_np={p.get('label_np','')} | label_en={p.get('label_en','')} | number={p['number']}"
            )
    if info.get("emails"):
        lines.append(f"emails: {', '.join(info['emails'])}")
    if info.get("hours"):
        lines.append(f"hours: {info['hours']}")
    if info.get("verified") is False:
        lines.append("verified: false  (phones not yet verified — recommend website + 1111 fallback only)")
    if info.get("notes_np"):
        lines.append(f"notes_np: {info['notes_np']}")
    return "\n".join(lines)


def hello_sarkar_brief(contacts: dict) -> str:
    fb = contacts["_universal_fallback"]
    return (
        f"name_np: {fb['name_np']}\n"
        f"name_en: {fb['name_en']}\n"
        f"phone: {fb['phone']} ({fb['phone_label_np']})\n"
        f"url: {fb['url']}\n"
        f"notes: {fb['notes']}"
    )


SYSTEM_PROMPT = """\
You are rewriting a refusal answer for a Nepal-government helpdesk eval set.

The user's question cannot be answered from the corpus we have. Your job is
to compose a HELPFUL refusal: tell the user the right office, give the actual
contact details we know, and provide a fallback path if the contact is
unreachable.

Hard rules:
1. Match the question's language EXACTLY: Devanagari → Devanagari, Roman-Nepali → Roman-Nepali, code-mixed → code-mixed. Never English.
2. Open with a brief refusal phrase ("मलाई यो प्रश्नको आधिकारिक स्रोत भेटिनँ" or "Yo prashnako adhikarik srot bhetina").
3. Provide the relevant office name (Nepali) + phone(s) + website. Use ONLY the contact details supplied below — do NOT invent phone numbers or addresses. If `verified: false` is shown, OMIT the phone and only give the website.
4. Add a one-line disclaimer: if the phone doesn't connect, check the website.
5. Always end with the **हेलो सरकार 1111** (Hello Sarkar) fallback as the universal helpline.
6. Do NOT introduce yourself, do NOT mention being an AI, do NOT use vendor names. Speak as a neutral helpdesk.
7. Be concise — 4–7 short lines. No filler.

Output format (markdown allowed; preserve URLs verbatim):

<refusal phrase in question's language>

**सम्बन्धित कार्यालय / Relevant office:** <name>
<phone(s) if verified, else omit>
**Website:** <url>

<one-line phone-unreachable note>
**हेलो सरकार 1111** (टोल-फ्री सरकारी सूचना हेल्पलाइन) मा सम्पर्क गर्न सक्नुहुन्छ।"""


def call_meridian(prompt: str, system: str) -> str:
    payload = json.dumps(
        {
            "model": MODEL,
            "max_tokens": 700,
            "system": system,
            "messages": [{"role": "user", "content": prompt}],
        }
    ).encode("utf-8")
    last_err: Optional[Exception] = None
    for attempt in range(MAX_RETRIES):
        try:
            req = urllib.request.Request(
                f"{MERIDIAN_URL}/v1/messages",
                data=payload,
                headers={
                    "Content-Type": "application/json",
                    "x-api-key": "x",
                    "anthropic-version": "2023-06-01",
                },
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
                data = json.loads(resp.read())
            parts = [b.get("text", "") for b in data["content"] if b.get("type") == "text"]
            return "".join(parts).strip()
        except urllib.error.HTTPError as e:
            err_body = e.read().decode("utf-8", errors="replace")
            last_err = RuntimeError(f"HTTP {e.code}: {err_body[:200]}")
            if e.code in (429, 500, 502, 503, 504):
                time.sleep(2 ** attempt)
                continue
            raise last_err
        except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as e:
            last_err = e
            time.sleep(2 ** attempt)
    raise RuntimeError(f"Meridian call failed after {MAX_RETRIES} attempts: {last_err}")


SCRUB_PATTERNS = [
    (re.compile(r"\bI(?:'m| am)\s+(?:Claude|Sonnet|Opus|Haiku|an? AI assistant|an? AI model|an? language model)\b", re.I), ""),
    (re.compile(r"\b(?:Claude|Sonnet 4\.\d|Opus 4\.\d|Haiku 4\.\d|Anthropic|Moonshot|Kimi)\b"), ""),
    (re.compile(r"\bas an? AI(?:\s+(?:assistant|language model|model|chatbot))?\b", re.I), ""),
]


def scrub(text: str) -> str:
    out = text
    for pat, repl in SCRUB_PATTERNS:
        out = pat.sub(repl, out)
    return re.sub(r"[ \t]+", " ", out).strip()


def enrich_one(item: dict, contacts: dict, aliases: dict, fallback_brief: str) -> dict:
    """Rewrite one refusal item. Returns updated record."""
    original_draft = item.get("draft_answer") or ""
    canonical = find_primary_office(original_draft, contacts, aliases)

    parts = [f"Question (lang={item.get('question_lang')}):\n{item['question'].strip()}", ""]
    if canonical:
        parts.append("Relevant office (verified contact details below):")
        parts.append(office_brief(canonical, contacts))
        parts.append("")
    else:
        parts.append("(No specific office identified from the original draft; rely on the Hello Sarkar fallback only.)")
        parts.append("")
    parts.append("Universal fallback (always include at the end):")
    parts.append(fallback_brief)
    parts.append("")
    parts.append("Compose the refusal in the question's language, following the system rules.")
    user_prompt = "\n".join(parts)

    out = dict(item)
    try:
        raw = call_meridian(user_prompt, SYSTEM_PROMPT)
        new_answer = scrub(raw)
        out["draft_answer"] = new_answer
        # New citations: the office URL + 1111 (Hello Sarkar)
        cites = []
        if canonical:
            cites.append(contacts[canonical].get("url"))
        cites.append(contacts["_universal_fallback"]["url"])
        out["draft_citations"] = [c for c in cites if c]
        out["enriched"] = True
        out["enriched_office"] = canonical
        out["enriched_meta"] = {"model": MODEL}
    except Exception as e:
        logging.warning("enrich failed id=%s: %s", item.get("id"), e)
        out["enriched_error"] = f"{type(e).__name__}: {str(e)[:200]}"
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", default=str(DRAFTS_PATH))
    ap.add_argument("--output", default=str(DRAFTS_PATH))
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--concurrency", type=int, default=3)
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(message)s",
    )

    contacts, aliases = load_contacts()
    fallback_brief = hello_sarkar_brief(contacts)
    logging.info(
        "loaded %d offices (+ %d aliases) from %s",
        sum(1 for k in contacts if not k.startswith("_")),
        len(aliases),
        CONTACTS_PATH,
    )

    in_path = Path(args.input)
    out_path = Path(args.output)
    if not in_path.exists():
        print(f"input not found: {in_path}", file=sys.stderr)
        return 1

    items: list[dict] = []
    with in_path.open(encoding="utf-8") as f:
        for line in f:
            items.append(json.loads(line))
    logging.info("loaded %d total drafts", len(items))

    # We only enrich items whose answer starts with NO_SOURCE_AVAILABLE.
    # Grounded items and ungrounded_attempt items are left alone.
    refusal_idxs = [
        i for i, r in enumerate(items)
        if (r.get("draft_answer") or "").startswith("NO_SOURCE_AVAILABLE")
    ]
    logging.info("found %d refusal items to enrich", len(refusal_idxs))

    if args.limit > 0:
        refusal_idxs = refusal_idxs[: args.limit]
        logging.info("--limit=%d; processing %d", args.limit, len(refusal_idxs))

    if not refusal_idxs:
        logging.info("nothing to do")
        return 0

    write_lock = Lock()
    n_done = 0
    n_with_office = 0
    n_no_office = 0
    n_error = 0
    t0 = time.time()

    with ThreadPoolExecutor(max_workers=args.concurrency) as pool:
        futs = {
            pool.submit(enrich_one, items[i], contacts, aliases, fallback_brief): i
            for i in refusal_idxs
        }
        for fut in as_completed(futs):
            idx = futs[fut]
            res = fut.result()
            with write_lock:
                items[idx] = res
                n_done += 1
                if res.get("enriched_error"):
                    n_error += 1
                elif res.get("enriched_office"):
                    n_with_office += 1
                else:
                    n_no_office += 1
                if n_done % 5 == 0 or n_done == len(refusal_idxs):
                    elapsed = time.time() - t0
                    rate = n_done / elapsed if elapsed > 0 else 0
                    eta = (len(refusal_idxs) - n_done) / rate if rate > 0 else 0
                    logging.info(
                        "%d/%d (%.2f rec/s, eta %.0fs) | with_office=%d no_office=%d errors=%d",
                        n_done, len(refusal_idxs), rate, eta, n_with_office, n_no_office, n_error,
                    )

    # Write the entire file back (drafts are append-only logically but
    # enrichment is replacement-in-place; we rewrite to be safe).
    with out_path.open("w", encoding="utf-8") as f:
        for r in items:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")

    print(f"\n=== enrich summary ===", file=sys.stderr)
    print(f"  refusals processed: {n_done}", file=sys.stderr)
    print(f"  with office contact: {n_with_office}", file=sys.stderr)
    print(f"  no office matched : {n_no_office}", file=sys.stderr)
    print(f"  errors            : {n_error}", file=sys.stderr)
    print(f"  output            : {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
