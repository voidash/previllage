#!/usr/bin/env python3
"""Pre-classify reddit_gov_questions.jsonl by whether each record is an actual
Nepal-government procedure/info question.

The keyword + interrogative-marker filter (filter_gov_questions.py) has high
recall but low precision: ~75% of its 1,898 outputs are political rants,
social commentary, or off-topic. This script invokes Sonnet via Meridian to
label each record so we can sample a clean eval set.

Output schema (one line per input record):
    {
        "id":         <reddit id>,
        "lang":       "roman_nepali" | "devanagari" | "code_mixed",
        "kind":       "comment" | "post",
        "score":      <int>,
        "class":      "yes_procedure" | "yes_info" | "adjacent" | "no_topic" | "no_format" | "error",
        "category":   "passport" | "citizenship" | "tax" | "land" | "business"
                      | "education" | "driving_license" | "pan_vat"
                      | "birth_registration" | "marriage" | "visa_immigration"
                      | "police" | "other",
        "rationale":  "<one sentence>",
        "body":       "<original body, truncated to BODY_MAX_CHARS>"
    }

Resumable: existing output ids are skipped on restart.

Usage:
    python scripts/classify_gov_questions.py             # all 1,898 records
    python scripts/classify_gov_questions.py --limit 30  # smoke test
    python scripts/classify_gov_questions.py --concurrency 8
"""
from __future__ import annotations

import argparse
import json
import logging
import os
import sys
import time
import urllib.error
import urllib.request
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from threading import Lock
from typing import Optional

MERIDIAN_URL = os.environ.get("MERIDIAN_URL", "http://127.0.0.1:3456")
MODEL = os.environ.get("CLASSIFIER_MODEL", "claude-sonnet-4-6")
BODY_MAX_CHARS = 2000
MAX_RETRIES = 3
TIMEOUT_S = 60

VALID_CLASSES = {
    "yes_procedure",
    "yes_info",
    "adjacent",
    "no_topic",
    "no_format",
}
VALID_CATEGORIES = {
    "passport",
    "citizenship",
    "tax",
    "land",
    "business",
    "education",
    "driving_license",
    "pan_vat",
    "birth_registration",
    "marriage",
    "visa_immigration",
    "police",
    "other",
}

PROMPT_TEMPLATE = """\
You are classifying a Reddit comment/post for an eval set. The eval set is for
a Nepal-government helpdesk that answers questions using gov.np sources.

Return STRICT JSON only (no other text, no markdown fences). Schema:
{{
    "class": "yes_procedure" | "yes_info" | "adjacent" | "no_topic" | "no_format",
    "category": "passport" | "citizenship" | "tax" | "land" | "business" | "education" | "driving_license" | "pan_vat" | "birth_registration" | "marriage" | "visa_immigration" | "police" | "other",
    "rationale": "<one short sentence>"
}}

Class definitions:
- yes_procedure: actionable "how do I X with gov?" / "where do I go for Y?" / "what documents for Z?" question
- yes_info: a question about gov rules / policies / status answerable from gov.np
- adjacent: gov-related but not a procedure question (rant, political opinion, news commentary)
- no_topic: keyword present but unrelated to gov procedure (mentions gov in passing)
- no_format: not actually a question

Text:
{body}

JSON:"""

BATCH_PROMPT_TEMPLATE = """\
You are classifying Reddit comments/posts for a Nepal-government helpdesk eval set.

Return STRICT JSON array of EXACTLY {n} items, in the same order as the inputs. No other text, no markdown fences. Each item:
{{
  "id": "<the id from the input>",
  "class": "yes_procedure" | "yes_info" | "adjacent" | "no_topic" | "no_format",
  "category": "passport" | "citizenship" | "tax" | "land" | "business" | "education" | "driving_license" | "pan_vat" | "birth_registration" | "marriage" | "visa_immigration" | "police" | "other",
  "rationale": "<one short sentence>"
}}

Class definitions:
- yes_procedure: actionable "how do I X with gov?" / "where do I go for Y?" / "what documents for Z?" question
- yes_info: a question about gov rules / policies / status answerable from gov.np
- adjacent: gov-related but not a procedure question (rant, political opinion, news commentary)
- no_topic: keyword present but unrelated to gov procedure
- no_format: not actually a question

Items to classify ({n} total):

{items}

JSON array (length {n}):"""


def call_meridian(prompt: str, model: str = MODEL, max_tokens: int = 200) -> str:
    """POST a single user-prompt to Meridian's Anthropic Messages endpoint.

    Returns the assistant's text content. Raises RuntimeError on persistent
    failure after retries.
    """
    payload = json.dumps(
        {
            "model": model,
            "max_tokens": max_tokens,
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
                    "x-api-key": "x",  # placeholder; Meridian uses OAuth internally
                    "anthropic-version": "2023-06-01",
                },
                method="POST",
            )
            with urllib.request.urlopen(req, timeout=TIMEOUT_S) as resp:
                body = resp.read()
            data = json.loads(body)
            if "content" not in data:
                raise RuntimeError(f"Meridian response missing 'content': {data}")
            parts = [b.get("text", "") for b in data["content"] if b.get("type") == "text"]
            return "".join(parts).strip()
        except urllib.error.HTTPError as e:
            err_body = e.read().decode("utf-8", errors="replace")
            last_err = RuntimeError(f"HTTP {e.code}: {err_body}")
            # 429 / 5xx: backoff and retry
            if e.code in (429, 500, 502, 503, 504):
                time.sleep(2 ** attempt)
                continue
            raise last_err
        except (urllib.error.URLError, TimeoutError, OSError) as e:
            last_err = e
            time.sleep(2 ** attempt)
        except json.JSONDecodeError as e:
            last_err = e
            time.sleep(2 ** attempt)
    raise RuntimeError(f"Meridian call failed after {MAX_RETRIES} attempts: {last_err}")


def parse_classification(raw: str) -> dict:
    """Parse Sonnet's JSON output. Raises on malformed.

    Sonnet occasionally wraps the JSON in a single-element array — handle that
    by unwrapping. Anything else (string, number, deeper nesting) is an error.
    """
    s = raw.strip()
    # Strip possible code fences if the model added them despite instructions.
    if s.startswith("```"):
        s = s.split("\n", 1)[1] if "\n" in s else s
        if s.endswith("```"):
            s = s.rsplit("```", 1)[0]
        s = s.strip()
    obj = json.loads(s)
    if isinstance(obj, list) and obj:
        obj = obj[0]
    if not isinstance(obj, dict):
        raise ValueError(f"unexpected JSON shape: {type(obj).__name__}")
    cls = obj.get("class")
    cat = obj.get("category")
    if cls not in VALID_CLASSES:
        raise ValueError(f"invalid class: {cls!r}")
    if cat not in VALID_CATEGORIES:
        raise ValueError(f"invalid category: {cat!r}")
    return {
        "class": cls,
        "category": cat,
        "rationale": (obj.get("rationale") or "").strip()[:300],
    }


def classify_one(record: dict) -> dict:
    """Classify a single reddit record. Returns a result row.

    On any error, sets class='error' and stores the error message in rationale.
    Never raises — keeps the batch moving.
    """
    body = record.get("body", "")
    truncated = body[:BODY_MAX_CHARS]
    prompt = PROMPT_TEMPLATE.format(body=truncated)
    out = {
        "id": record["id"],
        "lang": record.get("lang"),
        "kind": record.get("kind"),
        "score": record.get("score"),
        "body": truncated,
    }
    try:
        raw = call_meridian(prompt)
        out.update(parse_classification(raw))
    except Exception as e:
        logging.warning("classify failed id=%s: %s", record.get("id"), e)
        out.update(
            {
                "class": "error",
                "category": "other",
                "rationale": f"{type(e).__name__}: {str(e)[:200]}",
            }
        )
    return out


def parse_batch(raw: str, expected_n: int) -> list[dict]:
    """Parse a JSON array response. Raises on malformed."""
    s = raw.strip()
    # Strip code fences if present.
    if s.startswith("```"):
        s = s.split("\n", 1)[1] if "\n" in s else s
        if s.endswith("```"):
            s = s.rsplit("```", 1)[0]
        s = s.strip()
    parsed = json.loads(s)
    if not isinstance(parsed, list):
        raise ValueError(f"expected JSON array, got {type(parsed).__name__}")
    if len(parsed) != expected_n:
        raise ValueError(f"expected {expected_n} items, got {len(parsed)}")
    return parsed


def classify_batch(records: list[dict]) -> list[dict]:
    """Classify N records in one Meridian call.

    On batch-level failure (HTTP, parse, length mismatch), falls back to
    `classify_one` per record so we don't lose the whole batch.

    On per-item validation failure (invalid class/category), that single item
    falls back to `classify_one` independently.
    """
    if not records:
        return []
    if len(records) == 1:
        return [classify_one(records[0])]

    items_text = "\n\n".join(
        f"[{i + 1}] id={r['id']}\n{r['body'][:BODY_MAX_CHARS]}"
        for i, r in enumerate(records)
    )
    prompt = BATCH_PROMPT_TEMPLATE.format(n=len(records), items=items_text)
    # Each item ~80-180 output tokens (rationale + boilerplate). Pad generously.
    max_out = max(800, 220 * len(records))

    try:
        raw = call_meridian(prompt, max_tokens=max_out)
        parsed = parse_batch(raw, len(records))
    except Exception as e:
        logging.warning(
            "batch of %d failed (%s: %s); falling back to per-record",
            len(records),
            type(e).__name__,
            str(e)[:120],
        )
        return [classify_one(r) for r in records]

    out_records: list[dict] = []
    for r, p in zip(records, parsed):
        truncated = r["body"][:BODY_MAX_CHARS]
        cls = p.get("class") if isinstance(p, dict) else None
        cat = p.get("category") if isinstance(p, dict) else None
        if cls in VALID_CLASSES and cat in VALID_CATEGORIES:
            out_records.append(
                {
                    "id": r["id"],
                    "lang": r.get("lang"),
                    "kind": r.get("kind"),
                    "score": r.get("score"),
                    "body": truncated,
                    "class": cls,
                    "category": cat,
                    "rationale": (p.get("rationale") or "").strip()[:300],
                }
            )
        else:
            # Per-item retry as a single classification.
            logging.debug(
                "item id=%s invalid in batch (class=%r cat=%r); single retry",
                r["id"],
                cls,
                cat,
            )
            out_records.append(classify_one(r))
    return out_records


def already_done(out_path: Path) -> set[str]:
    """Read existing output, return the set of already-classified ids."""
    if not out_path.exists():
        return set()
    seen: set[str] = set()
    with out_path.open(encoding="utf-8") as f:
        for line in f:
            try:
                seen.add(json.loads(line)["id"])
            except (json.JSONDecodeError, KeyError):
                continue
    return seen


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", default="corpora/reddit_gov_questions.jsonl")
    ap.add_argument("--output", default="corpora/reddit_gov_questions_classified.jsonl")
    ap.add_argument("--limit", type=int, default=0, help="0 = process all")
    ap.add_argument(
        "--concurrency",
        type=int,
        default=4,
        help="number of in-flight batches (each batch = --batch-size records)",
    )
    ap.add_argument(
        "--batch-size",
        type=int,
        default=10,
        help="records per Meridian call (1 = single-question mode)",
    )
    ap.add_argument("--model", default=MODEL)
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(message)s",
    )

    in_path = Path(args.input)
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    # Load all input records.
    records: list[dict] = []
    with in_path.open(encoding="utf-8") as f:
        for line in f:
            records.append(json.loads(line))
    logging.info("loaded %d records from %s", len(records), in_path)

    done = already_done(out_path)
    remaining = [r for r in records if r["id"] not in done]
    logging.info(
        "already classified: %d; remaining: %d", len(done), len(remaining)
    )

    if args.limit > 0:
        remaining = remaining[: args.limit]
        logging.info("limit=%d; processing %d records", args.limit, len(remaining))

    if not remaining:
        logging.info("nothing to do; output is up to date")
        return 0

    write_lock = Lock()
    counts = {c: 0 for c in VALID_CLASSES | {"error"}}
    n_done = 0
    t0 = time.time()
    batch_size = max(1, args.batch_size)

    # Chunk remaining into batches. Each batch is one Meridian call.
    batches = [
        remaining[i : i + batch_size] for i in range(0, len(remaining), batch_size)
    ]
    logging.info(
        "submitting %d batches of up to %d records, concurrency=%d",
        len(batches),
        batch_size,
        args.concurrency,
    )

    # Append-mode write: every result lands on disk immediately so a crash
    # leaves a resumable state.
    with out_path.open("a", encoding="utf-8") as f_out, ThreadPoolExecutor(
        max_workers=args.concurrency
    ) as pool:
        if batch_size > 1:
            futures = {pool.submit(classify_batch, b): b for b in batches}
        else:
            futures = {pool.submit(classify_one, r): r for r in remaining}
        for fut in as_completed(futures):
            res = fut.result()
            results = res if isinstance(res, list) else [res]
            with write_lock:
                for row in results:
                    f_out.write(json.dumps(row, ensure_ascii=False) + "\n")
                    counts[row["class"]] = counts.get(row["class"], 0) + 1
                    n_done += 1
                f_out.flush()
                if n_done % batch_size == 0 or n_done >= len(remaining):
                    elapsed = time.time() - t0
                    rate = n_done / elapsed if elapsed > 0 else 0
                    eta = (len(remaining) - n_done) / rate if rate > 0 else 0
                    logging.info(
                        "%d/%d (%.1f rec/s, eta %.0fs) | %s",
                        n_done,
                        len(remaining),
                        rate,
                        eta,
                        {k: v for k, v in counts.items() if v},
                    )

    print("=== classification summary ===", file=sys.stderr)
    for k in sorted(counts):
        if counts[k]:
            print(f"  {k:>15}: {counts[k]:>5}", file=sys.stderr)
    print(f"  output: {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
