#!/usr/bin/env python3
"""SFT v4 refusal slice — retrieval-realistic refusal training data.

The v3a analysis (SFT_V3A_RESULTS.md §4.1) showed refusal_correct stuck at
18% with the long tail entirely at 0% (education, land, pan_vat, business,
tax, visa_immigration). Codex's diagnosis: "synthetic empty/partial refusal
mostly teaches the phrase, not the decision boundary." The v4 fix is
retrieval-realistic refusals — queries that go through actual retrieval
(bilingual FTS against the cleaned corpus) and let the teacher decide
whether to refuse based on what surfaces.

Pipeline (mirrors distill_grounded_v4 but with refusal-targeting queries):
    Step 1: Generate diverse refusal-target queries via DeepSeek across
            5 query families (each tagged `query_family` for analysis):
              - ood_celebrity        (1000)  "Who is Brad Pitt?"
              - ood_general_knowledge ( 800)  geography/science/history
              - tangential_gov        ( 800)  topics adjacent to gov.np scope
                                              but not actually covered (e.g.
                                              foreign embassy procedures)
              - latest_fee_date       ( 500)  "What's the citizenship fee
                                              today?" — chunks are typically
                                              old, teacher should mark
                                              [unverified]
              - wrong_language_topic  ( 500)  Roman-NE asking about deeply
                                              Devanagari-only legal terms
            Total target: 3,600 (matches codex spec).
    Step 2: For each query, run BILINGUAL FTS retrieval against the
            cleaned k2 corpus.
    Step 3: Teacher (DeepSeek) sees the retrieved chunks + question,
            answers under SYSTEM_GROUNDED. Most should refuse. Some
            may attempt partial answers — both are valid training data,
            since the model needs to learn WHEN to refuse vs cite.
    Step 4: Tag each tuple with the retrieval outcome:
              - n_retrieved == 0       → "no_hit"
              - top1 score is bad      → "tangential"
              - teacher refused        → check actual answer
            This metadata survives in the training record for downstream
            ablations.

Cost estimate: ~3600 items × (1 qgen call batched + 1 answer call) at
DeepSeek V4-Flash ≈ $3-4 wallclock ~1h with --concurrency 12.

Usage on k2:
    python3 scripts/synthesize_refusal_v4.py \\
        --db /Volumes/T9/gemma-god/corpus_v2/index.db \\
        --out corpora/sft_v4_refusal.jsonl \\
        --concurrency 12 --seed 42

    # Smoke (20 items per family):
    python3 scripts/synthesize_refusal_v4.py \\
        --db /Volumes/T9/gemma-god/corpus_v2/index.db \\
        --counts ood_celebrity=20,ood_general_knowledge=20,tangential_gov=20,latest_fee_date=20,wrong_language_topic=20
"""
from __future__ import annotations

import argparse
import json
import logging
import re
import sqlite3
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from threading import Lock

# Reuse retrieval + DeepSeek client from distill_grounded_v4 by importing
# (same directory).
sys.path.insert(0, str(Path(__file__).parent))
from distill_grounded_v4 import (  # noqa: E402
    DeepSeek, fts_search, detect_lang, SYSTEM_GROUNDED,
)


# ---- Default counts per query family (matches codex v4 spec) ---------------

DEFAULT_COUNTS: dict[str, int] = {
    "ood_celebrity":         1000,
    "ood_general_knowledge":  800,
    "tangential_gov":         800,
    "latest_fee_date":        500,
    "wrong_language_topic":   500,
}


# ---- Per-family system prompts ---------------------------------------------

SYSTEM_QGEN_OOD_CELEBRITY = """\
Generate ONE search query that a confused user might type into a Nepal-government \
helpdesk by mistake. The query asks about a CELEBRITY, athlete, musician, or \
public figure (not Nepal-government). Examples of good output:
  - "Who is Brad Pitt?"
  - "बेन्जामिन फ्रैंकलिन को थिए?"
  - "Lionel Messi ko ho?"
  - "Tell me about Albert Einstein"
Vary across English, Devanagari, Roman-Nepali. ONE query only, no preamble."""

SYSTEM_QGEN_OOD_GENERAL = """\
Generate ONE search query unrelated to Nepal-government services. Topic should be \
geography, science, history, sports, tech, or general knowledge. Examples:
  - "How do black holes form?"
  - "What's the capital of Brazil?"
  - "विश्वमा कति महादेश छन्?"
  - "Photosynthesis kasari hunchha?"
Vary across English, Devanagari, Roman-Nepali. ONE query only."""

SYSTEM_QGEN_TANGENTIAL_GOV = """\
Generate ONE search query about a GOVERNMENT-ish topic that is NOT covered by \
Nepal-government services. Examples:
  - "How do I apply for a US visa from outside the embassy?"
  - "Indian passport renewal in Delhi"
  - "UK driving license requirements"
  - "Bangladesh ko nagrikta kasari linne?"
The query should be plausible enough that retrieval might surface tangentially-\
relevant Nepal-gov chunks, but the answer can't actually be derived from them.
Vary across English, Devanagari, Roman-Nepali. ONE query only."""

SYSTEM_QGEN_LATEST_FEE_DATE = """\
Generate ONE search query asking about the CURRENT/LATEST fee, deadline, or \
date for a Nepal-government service. The corpus has older PDFs that may state \
fees but a model should be cautious about whether they're current. Examples:
  - "What is the citizenship certificate fee today?"
  - "नवीकरण शुल्क २०८२ मा कति हो?"
  - "Passport fee 2025 ma kati cha?"
  - "Latest tax filing deadline this year"
Vary across English, Devanagari, Roman-Nepali. ONE query only."""

SYSTEM_QGEN_WRONG_LANG_TOPIC = """\
Generate ONE search query in ROMAN-NEPALI or ENGLISH about a Nepal-government \
topic that is typically only documented in pure Devanagari (constitutional \
law, supreme court decisions, gazette notices). The query should hit the \
language-mismatch case where retrieval may surface Devanagari chunks but \
answering in the query's language requires understanding them. Examples:
  - "What does Article 39 of the Constitution say?"
  - "Sambidhan ko dhara 39 ke ho?"
  - "Supreme court decision on land reform 2079"
ONE query only — Roman-NE or English ONLY (NOT Devanagari)."""


FAMILY_PROMPTS = {
    "ood_celebrity":         SYSTEM_QGEN_OOD_CELEBRITY,
    "ood_general_knowledge": SYSTEM_QGEN_OOD_GENERAL,
    "tangential_gov":        SYSTEM_QGEN_TANGENTIAL_GOV,
    "latest_fee_date":       SYSTEM_QGEN_LATEST_FEE_DATE,
    "wrong_language_topic":  SYSTEM_QGEN_WRONG_LANG_TOPIC,
}


REFUSAL_MARKERS = (
    "मलाई यो प्रश्नको आधिकारिक स्रोत",
    "Yo prashnako adhikarik",
    "I cannot find an authoritative source",
)


def gen_query(family: str, ds: DeepSeek) -> str | None:
    sysp = FAMILY_PROMPTS[family]
    try:
        q = ds.chat(sysp, "Generate one query now.", max_tokens=150)
    except Exception as e:
        logging.warning("qgen failed for family %s: %s", family, e)
        return None
    q = q.strip().split("\n")[0].strip()
    q = re.sub(r"^[\"“]|[\"”]$", "", q).strip()
    q = re.sub(r"^(Q[:.]\s*|Query[:.]\s*|Question[:.]\s*)", "", q, flags=re.I).strip()
    if len(q) < 6:
        return None
    return q


def gen_answer(question: str, chunks: list[dict], ds: DeepSeek) -> str | None:
    if chunks:
        sources_block = "\n\n".join(
            f"[{c['rank']}] {c['url']}\n{c['text']}" for c in chunks
        )
    else:
        sources_block = "(no candidate sources surfaced)"
    user = f"Question: {question}\n\nSources:\n{sources_block}"
    try:
        return ds.chat(SYSTEM_GROUNDED, user, max_tokens=600)
    except Exception as e:
        logging.warning("answer gen failed for %r: %s", question[:60], e)
        return None


def process_one(family: str, idx: int, ds: DeepSeek, db_path: Path, top_k: int) -> dict | None:
    q = gen_query(family, ds)
    if not q:
        return None
    conn = sqlite3.connect(db_path)
    try:
        retrieved = fts_search(conn, q, top_k=top_k)
    finally:
        conn.close()
    answer = gen_answer(q, retrieved, ds)
    if not answer:
        return None
    refused = any(m in answer for m in REFUSAL_MARKERS)
    # Empirical retrieval outcome — tags help downstream ablation analysis.
    if not retrieved:
        outcome = "no_hit"
    elif retrieved[0].get("score") is not None and retrieved[0]["score"] > -0.5:
        # FTS5 bm25 returns negative; closer-to-0 = worse match
        outcome = "tangential"
    else:
        outcome = "some_hit"
    return {
        "id": f"sft_v4_ref_{family}_{idx:05d}",
        "source": "v4_refusal_retrieval",
        "question": q,
        "question_lang": detect_lang(q),
        "category": "other",
        "chunks": [
            {"rank": c["rank"], "url": c["url"], "text": c["text"]}
            for c in retrieved
        ],
        "answer": answer,
        "query_family": family,
        "retrieval_outcome": outcome,
        "model_refused": refused,
        "n_retrieved": len(retrieved),
        "skip": False,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default="/Volumes/T9/gemma-god/corpus_v2/index.db")
    ap.add_argument("--out", default="corpora/sft_v4_refusal.jsonl")
    ap.add_argument("--top-k", type=int, default=5)
    ap.add_argument("--concurrency", type=int, default=8)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument(
        "--counts", default="",
        help="override per-family counts, e.g. 'ood_celebrity=10,tangential_gov=10'. "
        "Unspecified families use DEFAULT_COUNTS.",
    )
    ap.add_argument("--resume", action="store_true",
                    help="skip ids already in --out")
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s"
    )

    counts = dict(DEFAULT_COUNTS)
    if args.counts:
        for kv in args.counts.split(","):
            k, v = kv.split("=", 1)
            counts[k.strip()] = int(v.strip())

    db_path = Path(args.db)
    if not db_path.exists():
        print(f"ERROR: db not found: {db_path}", file=sys.stderr)
        return 1
    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    done_ids: set[str] = set()
    if args.resume and out_path.exists():
        for line in out_path.open(encoding="utf-8"):
            try:
                done_ids.add(json.loads(line).get("id", ""))
            except Exception:
                pass

    # Build the work list
    work: list[tuple[str, int]] = []
    for family, n in counts.items():
        if family not in FAMILY_PROMPTS:
            logging.warning("unknown family %r — skipping", family)
            continue
        for i in range(n):
            wid = f"sft_v4_ref_{family}_{i:05d}"
            if wid in done_ids:
                continue
            work.append((family, i))
    logging.info("work: %d items across %d families", len(work), len(set(f for f, _ in work)))

    ds = DeepSeek()
    write_lock = Lock()
    n_ok = 0
    n_err = 0
    n_refused = 0
    by_family_ok: dict[str, int] = {}
    by_outcome: dict[str, int] = {}
    started = time.time()

    with out_path.open("a", encoding="utf-8") as fout, \
         ThreadPoolExecutor(max_workers=args.concurrency) as ex:
        futs = {
            ex.submit(process_one, fam, idx, ds, db_path, args.top_k): (fam, idx)
            for fam, idx in work
        }
        for i, fut in enumerate(as_completed(futs), 1):
            try:
                rec = fut.result()
            except Exception as e:
                logging.warning("worker exception: %s", e)
                n_err += 1
                continue
            if rec is None:
                n_err += 1
                continue
            with write_lock:
                fout.write(json.dumps(rec, ensure_ascii=False) + "\n")
                fout.flush()
            n_ok += 1
            by_family_ok[rec["query_family"]] = by_family_ok.get(rec["query_family"], 0) + 1
            by_outcome[rec["retrieval_outcome"]] = by_outcome.get(rec["retrieval_outcome"], 0) + 1
            if rec["model_refused"]:
                n_refused += 1
            if i % 50 == 0:
                elapsed = time.time() - started
                rate = i / elapsed if elapsed > 0 else 0
                logging.info(
                    "[%d/%d] ok=%d err=%d refused=%d (%.1f items/sec)",
                    i, len(work), n_ok, n_err, n_refused, rate,
                )

    print(f"\n=== synthesize_refusal_v4 summary ===", file=sys.stderr)
    print(f"  attempted   : {len(work)}", file=sys.stderr)
    print(f"  ok          : {n_ok}", file=sys.stderr)
    print(f"  errors      : {n_err}", file=sys.stderr)
    print(f"  teacher refused: {n_refused} ({100*n_refused/max(n_ok,1):.1f}%)", file=sys.stderr)
    print(f"  by family   : {by_family_ok}", file=sys.stderr)
    print(f"  by outcome  : {by_outcome}", file=sys.stderr)
    print(f"  output      : {out_path}", file=sys.stderr)
    print(f"  wallclock   : {time.time()-started:.0f}s", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
