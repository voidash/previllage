#!/usr/bin/env python3
"""Compose grounded draft answers for the groundedness eval set.

For each sampled question:
  1. Score candidate documents in `gov_nepali.jsonl` via simple keyword
     overlap. Take top-K.
  2. Call Opus via Meridian with strict-grounding instructions.
  3. Identity-scrub the response (remove teacher self-references).
  4. Emit a draft JSONL record for human review.

The retrieval here is intentionally crude (keyword scoring on a flat dump,
no LanceDB / no BM25). The eval set's gold standard comes from the human
review step, not from this draft composer's retrieval quality. Once the
real hybrid retrieval (Phase 28) lands, we can re-score against the same
question set to compute recall@k.

Usage:
    python scripts/build_groundedness_eval.py --limit 5    # smoke test
    python scripts/build_groundedness_eval.py              # all sampled
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
COMPOSER_MODEL = os.environ.get("COMPOSER_MODEL", "claude-sonnet-4-6")
TOP_K = 5
CHUNK_TEXT_MAX_CHARS = 1000  # per chunk in the prompt
MAX_RETRIES = 3
TIMEOUT_S = 90

# ---- Identity scrub --------------------------------------------------------

# Patterns that leak the teacher model's identity. Replaced with empty or
# generic phrasing so the SFT student doesn't memorize "I am Claude".
IDENTITY_PATTERNS = [
    # Direct self-references.
    (re.compile(r"\bI(?:'m| am)\s+(?:Claude|Sonnet|Opus|Haiku|Kimi|Moonshot|an? AI|an? AI assistant|an? AI (?:language )?model|an? assistant|an? language model)\b", re.I), "I"),
    # "as an AI ..."
    (re.compile(r"\bas an? AI(?:\s+(?:assistant|language model|model|chatbot))?\b", re.I), ""),
    # Vendor/product mentions when not part of a citation URL.
    (re.compile(r"\b(?:Anthropic|Moonshot AI|Moonshot)\b", re.I), ""),
    # Bare model names — only outside a [URL] citation. We do this naively;
    # if it ever clobbers a legitimate use, the human review catches it.
    (re.compile(r"\b(?:Claude|Sonnet 4\.\d|Opus 4\.\d|Haiku 4\.\d)\b"), ""),
    # AI disclaimer boilerplate.
    (re.compile(r"\bI(?:'m| am) (?:not able to|unable to|cannot|can't) (?:provide|access)\b.*?(?:knowledge|model|training)\.?", re.I | re.S), ""),
]


def scrub_identity(text: str) -> tuple[str, list[str]]:
    """Strip teacher self-references. Returns (scrubbed_text, list_of_hits)."""
    hits: list[str] = []
    out = text
    for pat, repl in IDENTITY_PATTERNS:
        for m in pat.finditer(out):
            hits.append(m.group(0))
        out = pat.sub(repl, out)
    # Collapse whitespace runs left by removals.
    out = re.sub(r"[ \t]+", " ", out)
    out = re.sub(r"\n{3,}", "\n\n", out)
    return out.strip(), hits


# ---- Retrieval (keyword overlap) ------------------------------------------

# Strip Reddit / English noise tokens that don't help retrieval.
STOP_TOKENS_LATIN = frozenset(
    [
        "the", "a", "an", "is", "are", "was", "were", "be", "been", "being",
        "i", "you", "he", "she", "it", "we", "they", "this", "that", "these",
        "those", "of", "in", "on", "at", "for", "with", "by", "to", "from",
        "and", "or", "but", "if", "so", "as", "than", "then", "do", "does",
        "did", "have", "has", "had", "can", "could", "will", "would", "should",
        "ko", "ma", "le", "huncha", "hunchha", "cha", "chha", "ho", "hau",
        "k", "kasari", "kaha", "kati", "kahile", "kun", "thau", "office",
        "garna", "garne", "janu", "parcha", "parchha", "garera", "gareko",
        # Reddit chatter
        "bro", "yaar", "bhai", "didi", "sir", "madam",
    ]
)


def tokenize_query(text: str) -> list[str]:
    """Tokenize question text into a keyword set for retrieval."""
    # Latin (Roman-NE / English) tokens.
    latin_toks = re.findall(r"[A-Za-z]{3,}", text.lower())
    latin_toks = [t for t in latin_toks if t not in STOP_TOKENS_LATIN]
    # Devanagari tokens. Word-boundary handling is unreliable; split on
    # whitespace + punctuation.
    deva_toks = re.findall(r"[ऀ-ॿ]+", text)
    return latin_toks + deva_toks


def score_chunk(query_tokens: list[str], chunk_text: str) -> float:
    """Sum occurrences of query tokens in chunk text. Lowercased Latin tokens
    matched case-insensitively; Devanagari matched as-is.
    """
    if not query_tokens:
        return 0.0
    score = 0.0
    chunk_lower = chunk_text.lower()
    seen_tokens = 0
    for tok in query_tokens:
        if re.search(r"[ऀ-ॿ]", tok):
            count = chunk_text.count(tok)
        else:
            count = chunk_lower.count(tok)
        if count > 0:
            seen_tokens += 1
            # Soft cap per-token: a chunk that mentions one token 50 times
            # shouldn't dominate. Cap at 3 hits per token.
            score += min(count, 3)
    # Bonus for diversity: chunks that mention more distinct tokens are better.
    score += seen_tokens * 0.5
    return score


def load_corpus(corpus_path: Path) -> list[dict]:
    """Load gov_nepali.jsonl into memory."""
    docs: list[dict] = []
    with corpus_path.open(encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            docs.append(
                {
                    "doc_id": r.get("doc_id"),
                    "url": r.get("source_url"),
                    "tier": r.get("tier"),
                    "text": r.get("text", ""),
                }
            )
    return docs


def retrieve(
    question: str, corpus: list[dict], k: int = TOP_K
) -> list[dict]:
    """Score every doc; return top-k as candidate chunks."""
    qtoks = tokenize_query(question)
    if not qtoks:
        return []
    scored: list[tuple[float, dict]] = []
    for doc in corpus:
        s = score_chunk(qtoks, doc["text"])
        if s > 0:
            scored.append((s, doc))
    scored.sort(key=lambda t: t[0], reverse=True)
    out: list[dict] = []
    for rank, (s, doc) in enumerate(scored[:k], 1):
        text = doc["text"]
        if len(text) > CHUNK_TEXT_MAX_CHARS:
            text = text[:CHUNK_TEXT_MAX_CHARS] + "..."
        out.append(
            {
                "rank": rank,
                "score": round(s, 2),
                "url": doc["url"],
                "doc_id": doc["doc_id"],
                "tier": doc["tier"],
                "text": text,
            }
        )
    return out


# ---- Prompting + Meridian call --------------------------------------------

SYSTEM_PROMPT = """\
You are answering Nepal-government procedure questions for a helpdesk eval set.

HARD RULES:
1. Use ONLY the provided sources. Do not draw on outside knowledge.
2. After every factual claim, cite the source URL in square brackets, e.g. [https://www.moha.gov.np/...].
3. If a claim is not directly supported by ANY provided source, drop it or mark `[unverified]`.
4. If NO source meaningfully addresses the question, respond with exactly: "NO_SOURCE_AVAILABLE" followed by a one-line note about what gov.np page would be needed. Do not invent facts to fill the gap.
5. Respond in the same language/script as the question. Roman-Nepali question → Roman-Nepali answer. Devanagari → Devanagari. Mixed → mixed.
6. Be concise and procedural. No preamble. Direct steps when applicable.
7. Do NOT introduce yourself, do NOT mention what model or system you are, do NOT use phrases like "I'm an AI", "as a language model", or vendor names. Speak as a neutral helpdesk."""


def build_user_prompt(question: str, candidates: list[dict]) -> str:
    """Construct the user-turn content for a single eval item."""
    parts = [f"Question: {question.strip()}", "", "Sources:"]
    if not candidates:
        parts.append("(no candidate sources surfaced)")
    for c in candidates:
        parts.append(f"\n[{c['rank']}] {c['url']}")
        parts.append(c["text"])
    parts.append("\nCompose a grounded answer following the hard rules.")
    return "\n".join(parts)


def call_meridian(
    system: str, user: str, model: str = COMPOSER_MODEL
) -> tuple[str, dict]:
    """POST to Meridian. Returns (text, meta).

    Raises RuntimeError on persistent failure.
    """
    payload = json.dumps(
        {
            "model": model,
            "max_tokens": 1500,
            "system": system,
            "messages": [{"role": "user", "content": user}],
        }
    ).encode("utf-8")

    last_err: Optional[Exception] = None
    for attempt in range(MAX_RETRIES):
        try:
            t0 = time.time()
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
                body = resp.read()
            elapsed_ms = int((time.time() - t0) * 1000)
            data = json.loads(body)
            if "content" not in data:
                raise RuntimeError(f"missing 'content' in response: {data}")
            parts = [
                b.get("text", "")
                for b in data["content"]
                if b.get("type") == "text"
            ]
            text = "".join(parts).strip()
            meta = {
                "model": data.get("model", model),
                "elapsed_ms": elapsed_ms,
                "stop_reason": data.get("stop_reason"),
                "usage": data.get("usage"),
            }
            return text, meta
        except urllib.error.HTTPError as e:
            err_body = e.read().decode("utf-8", errors="replace")
            last_err = RuntimeError(f"HTTP {e.code}: {err_body[:300]}")
            if e.code in (429, 500, 502, 503, 504):
                time.sleep(2 ** attempt)
                continue
            raise last_err
        except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as e:
            last_err = e
            time.sleep(2 ** attempt)
    raise RuntimeError(f"Meridian call failed after {MAX_RETRIES} attempts: {last_err}")


CITATION_RE = re.compile(r"\[(https?://[^\]\s]+)\]")


def extract_citations(text: str) -> list[str]:
    """Pull bracketed URLs out of the answer."""
    return list(dict.fromkeys(CITATION_RE.findall(text)))  # dedup, preserve order


# ---- Per-item driver ------------------------------------------------------


def compose_one(item: dict, corpus: list[dict]) -> dict:
    """Process one sampled question into a draft eval record."""
    question = item["question"]
    candidates = retrieve(question, corpus, k=TOP_K)
    user_prompt = build_user_prompt(question, candidates)

    out = {
        **item,
        "candidate_chunks": candidates,
        "draft_answer": None,
        "draft_citations": [],
        "draft_meta": {},
        "scrub_hits": [],
        "review": {
            "verdict": None,
            "gold_answer": None,
            "gold_source_urls": [],
            "notes": "",
        },
    }
    try:
        raw, meta = call_meridian(SYSTEM_PROMPT, user_prompt)
        scrubbed, hits = scrub_identity(raw)
        out["draft_answer"] = scrubbed
        out["draft_citations"] = extract_citations(scrubbed)
        out["draft_meta"] = meta
        out["scrub_hits"] = hits
    except Exception as e:
        logging.warning("compose failed id=%s: %s", item["id"], e)
        out["draft_answer"] = None
        out["draft_meta"] = {"error": f"{type(e).__name__}: {str(e)[:200]}"}
    return out


def already_done(out_path: Path) -> set[str]:
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
    ap.add_argument("--input", default="corpora/eval_sample_v1.jsonl")
    ap.add_argument("--corpus", default="corpora/gov_chunks_v2.jsonl")
    ap.add_argument("--output", default="eval/gov_helpdesk_v1_drafts.jsonl")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--concurrency", type=int, default=4)
    ap.add_argument("--model", default=COMPOSER_MODEL)
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(message)s",
    )

    in_path = Path(args.input)
    corpus_path = Path(args.corpus)
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    if not in_path.exists():
        print(f"input not found: {in_path}", file=sys.stderr)
        print(
            "Run scripts/sample_eval_questions.py first.", file=sys.stderr
        )
        return 1
    if not corpus_path.exists():
        print(f"corpus not found: {corpus_path}", file=sys.stderr)
        return 2

    items: list[dict] = []
    with in_path.open(encoding="utf-8") as f:
        for line in f:
            items.append(json.loads(line))
    logging.info("loaded %d sample items", len(items))

    logging.info("loading corpus from %s ...", corpus_path)
    corpus = load_corpus(corpus_path)
    logging.info("corpus: %d documents", len(corpus))

    done = already_done(out_path)
    remaining = [it for it in items if it["id"] not in done]
    logging.info(
        "already drafted: %d; remaining: %d", len(done), len(remaining)
    )
    if args.limit > 0:
        remaining = remaining[: args.limit]
        logging.info("limit=%d; processing %d items", args.limit, len(remaining))
    if not remaining:
        logging.info("nothing to do")
        return 0

    write_lock = Lock()
    n_done = 0
    n_no_source = 0
    n_error = 0
    t0 = time.time()
    with out_path.open("a", encoding="utf-8") as f_out, ThreadPoolExecutor(
        max_workers=args.concurrency
    ) as pool:
        futs = {pool.submit(compose_one, it, corpus): it for it in remaining}
        for fut in as_completed(futs):
            res = fut.result()
            with write_lock:
                f_out.write(json.dumps(res, ensure_ascii=False) + "\n")
                f_out.flush()
                n_done += 1
                if (res.get("draft_answer") or "").startswith("NO_SOURCE_AVAILABLE"):
                    n_no_source += 1
                if not res.get("draft_answer"):
                    n_error += 1
                if n_done % 5 == 0 or n_done == len(remaining):
                    elapsed = time.time() - t0
                    rate = n_done / elapsed if elapsed > 0 else 0
                    eta = (len(remaining) - n_done) / rate if rate > 0 else 0
                    logging.info(
                        "%d/%d (%.2f rec/s, eta %.0fs) | no_source=%d errors=%d",
                        n_done,
                        len(remaining),
                        rate,
                        eta,
                        n_no_source,
                        n_error,
                    )

    print(f"\n=== draft summary ===", file=sys.stderr)
    print(f"  total: {n_done}", file=sys.stderr)
    print(f"  NO_SOURCE_AVAILABLE: {n_no_source}", file=sys.stderr)
    print(f"  errors: {n_error}", file=sys.stderr)
    print(f"  output: {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
