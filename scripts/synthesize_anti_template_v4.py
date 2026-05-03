#!/usr/bin/env python3
"""SFT v4 anti-template slice — chunks cover topic A, question asks A AND B,
gold answer covers A and refuses B with [unverified].

v3a had 270 of these synthesized with FAKE chunks (DeepSeek invented
"https://moha.gov.np/citizenship-replacement" URLs that don't exist in the
corpus). v4 fix: use REAL retrieved chunks from the cleaned k2 corpus, so
the model learns the pattern against grounded evidence.

Pipeline:
    Step 1: Pick a seed chunk (topic A's authoritative source).
    Step 2: Retrieve top-K chunks for A — these are the "grounded" evidence.
    Step 3: Generate a question that asks about A AND B (where B is a
            related-but-not-covered topic). Done in 2 DeepSeek calls:
              (a) given A's chunks, propose a related B
              (b) compose a 2-part question
    Step 4: Have teacher answer ONLY from A's chunks. Should cite for A,
            mark B with [unverified].

Cost estimate: 600 items × 3 DeepSeek calls (B-propose + Q-compose +
answer) ≈ $2 wallclock ~30 min with --concurrency 8.

Usage on k2:
    python3 scripts/synthesize_anti_template_v4.py \\
        --db /Volumes/T9/gemma-god/corpus_v2/index.db \\
        --n 600 \\
        --out corpora/sft_v4_anti_template.jsonl \\
        --concurrency 8 --seed 42
"""
from __future__ import annotations

import argparse
import json
import logging
import random
import re
import sqlite3
import sys
import time
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from threading import Lock

sys.path.insert(0, str(Path(__file__).parent))
from distill_grounded_v4 import (  # noqa: E402
    DeepSeek, fts_search, detect_lang, SYSTEM_GROUNDED,
    select_seed_chunks,
)


SYSTEM_B_TOPIC = """\
Given a chunk from a Nepal-government document that covers TOPIC A, propose \
ONE related topic B that:
  - Is in the same broad domain (e.g. if A is "citizenship application", \
B could be "passport renewal" — both ID-document procedures)
  - Is NOT covered by the chunk
  - A citizen might plausibly bundle into the same question as A

Output ONLY topic B as a short phrase (3-8 words). No preamble."""

SYSTEM_QGEN = """\
Given topic A and topic B, write ONE realistic citizen question that asks \
about BOTH. The question should:
  - Sound natural, like someone typing into a search box
  - Use 2 sentences OR a sentence with "and ... also ..." structure
  - Vary across English, Devanagari, Roman-Nepali

Examples:
  - "How do I replace my citizenship certificate? Also, what is the process \
for replacing a lost passport?"
  - "नागरिकता हराएमा के गर्ने? र राहदानी हराए ब्याक कसरी पाउने?"
  - "Bibaha registration kasari garne ra mero passport bigreko cha bhane ke garne?"

ONE question only — no preamble, no labels."""


def gen_b_topic(seed: dict, ds: DeepSeek) -> str | None:
    text = (seed["text"] or "")[:1200]
    user = f"URL: {seed['url']}\n\nChunk (covers TOPIC A):\n{text}\n\nPropose TOPIC B:"
    try:
        b = ds.chat(SYSTEM_B_TOPIC, user, max_tokens=80)
    except Exception as e:
        logging.warning("b-topic gen failed: %s", e)
        return None
    b = b.strip().split("\n")[0].strip()
    b = re.sub(r"^[\"“]|[\"”]$|^[Bb][:.]\s*|^Topic\s+B[:.]\s*", "", b).strip()
    if len(b) < 4:
        return None
    return b


def gen_q(seed: dict, b_topic: str, ds: DeepSeek) -> str | None:
    seed_excerpt = (seed["text"] or "")[:600]
    user = (
        f"TOPIC A (covered by the source chunk):\n{seed_excerpt[:200]}\n"
        f"...source: {seed['url']}\n\n"
        f"TOPIC B (NOT covered by source):\n{b_topic}\n\n"
        f"Write the question:"
    )
    try:
        q = ds.chat(SYSTEM_QGEN, user, max_tokens=200)
    except Exception as e:
        logging.warning("q gen failed: %s", e)
        return None
    q = q.strip().split("\n")[0].strip()
    q = re.sub(r"^(Q[:.]\s*|Question[:.]\s*)", "", q, flags=re.I).strip()
    if len(q) < 12:
        return None
    return q


def gen_answer(question: str, chunks: list[dict], ds: DeepSeek) -> str | None:
    if not chunks:
        sources_block = "(no candidate sources surfaced)"
    else:
        sources_block = "\n\n".join(
            f"[{c['rank']}] {c['url']}\n{c['text']}" for c in chunks
        )
    user = f"Question: {question}\n\nSources:\n{sources_block}"
    try:
        return ds.chat(SYSTEM_GROUNDED, user, max_tokens=600)
    except Exception as e:
        logging.warning("answer gen failed: %s", e)
        return None


def process_one(seed: dict, ds: DeepSeek, db_path: Path, top_k: int) -> dict | None:
    b_topic = gen_b_topic(seed, ds)
    if not b_topic:
        return None
    q = gen_q(seed, b_topic, ds)
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
    return {
        "id": f"sft_v4_at_{seed['chunk_id'][:12]}",
        "source": "v4_anti_template",
        "question": q,
        "question_lang": detect_lang(q),
        "category": "anti_template",
        "chunks": [
            {"rank": c["rank"], "url": c["url"], "text": c["text"]}
            for c in retrieved
        ],
        "answer": answer,
        "seed_chunk_id": seed["chunk_id"],
        "seed_url": seed["url"],
        "topic_b": b_topic,
        "skip": False,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", default="/Volumes/T9/gemma-god/corpus_v2/index.db")
    ap.add_argument("--n", type=int, default=600)
    ap.add_argument("--out", default="corpora/sft_v4_anti_template.jsonl")
    ap.add_argument("--top-k", type=int, default=5)
    ap.add_argument("--concurrency", type=int, default=8)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--resume", action="store_true")
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s"
    )

    db_path = Path(args.db)
    if not db_path.exists():
        print(f"ERROR: db not found: {db_path}", file=sys.stderr)
        return 1
    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    rng = random.Random(args.seed)
    seeds = select_seed_chunks(db_path, args.n, rng)
    logging.info("selected %d seed chunks", len(seeds))

    done_ids: set[str] = set()
    if args.resume and out_path.exists():
        for line in out_path.open(encoding="utf-8"):
            try:
                done_ids.add(json.loads(line).get("seed_chunk_id", ""))
            except Exception:
                pass

    pending = [s for s in seeds if s["chunk_id"] not in done_ids]
    logging.info("processing %d items", len(pending))

    ds = DeepSeek()
    write_lock = Lock()
    n_ok = 0
    n_err = 0
    started = time.time()

    with out_path.open("a", encoding="utf-8") as fout, \
         ThreadPoolExecutor(max_workers=args.concurrency) as ex:
        futs = {ex.submit(process_one, s, ds, db_path, args.top_k): s for s in pending}
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
            if i % 25 == 0:
                elapsed = time.time() - started
                logging.info(
                    "[%d/%d] ok=%d err=%d (%.1fs elapsed)",
                    i, len(pending), n_ok, n_err, elapsed,
                )

    print(f"\n=== synthesize_anti_template_v4 summary ===", file=sys.stderr)
    print(f"  attempted   : {len(pending)}", file=sys.stderr)
    print(f"  ok          : {n_ok}", file=sys.stderr)
    print(f"  errors      : {n_err}", file=sys.stderr)
    print(f"  output      : {out_path}", file=sys.stderr)
    print(f"  wallclock   : {time.time()-started:.0f}s", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
