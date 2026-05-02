#!/usr/bin/env python3
"""Pull an English replay subset from `allenai/tulu-3-sft-mixture` for the
SFT v1 training mix's anti-forgetting slice.

Per recipe v0.4: 15% English replay prevents catastrophic forgetting of
English instruction-following. TULU 3 SFT is the well-known general
instruction mixture (FLAN, No Robots, OpenAssistant, Numina, etc.).

Output schema matches `generate_sft_grounded.py`:
    {
      "id": "sft_english_00001",
      "source": "tulu3_sft",
      "question": "...",
      "question_lang": "english",
      "category": "other",
      "chunks": [],
      "answer": "...",
      "skip": false
    }

Usage:
    python scripts/pull_tulu_subset.py --n 1500 --seed 42
"""
from __future__ import annotations

import argparse
import json
import logging
import random
import re
import sys
from pathlib import Path

# TULU 3 mixes English with Spanish/German/Thai/Russian/Hindi/etc. The
# Latin-char ratio filter doesn't help (Spanish/German are Latin alphabet).
# Filter by source dataset instead — the OASST converted subset is multilingual,
# the rest are reliably English. Confirm with a token-overlap heuristic.
ENGLISH_SOURCE_ALLOW = re.compile(
    r"(flan|no_robots|numinamath|coconot|wildchat.*english|tulu-3-personas|"
    r"tulu-3-IF|tulu-3-instruction-following|hard-coded|sciriff|aya_dataset_dolma_v0_5|"
    r"tulu-3-wildguard|tulu-3-wildjailbreak|table_gpt|cssbench|hardcoded)",
    re.I,
)
ENGLISH_SOURCE_DENY = re.compile(r"oasst1|aya_dataset(?!.*english)|multilingual|nllb", re.I)

# Common English function words — at least 2 should appear in any real
# English passage of length >= ~50 chars.
ENGLISH_FUNCTION_WORDS = re.compile(
    r"\b(?:the|of|and|to|in|is|that|it|for|on|with|as|are|this|be|by|or|"
    r"an|at|from|but|not|have|has|was|were|will|can|you|your|i|we|they|"
    r"what|how|when|where|why|which|who)\b",
    re.I,
)


def is_english(source: str | None, text: str) -> bool:
    src = source or ""
    if ENGLISH_SOURCE_DENY.search(src):
        return False
    if not ENGLISH_SOURCE_ALLOW.search(src):
        return False
    # Belt-and-suspenders — even allowed sources occasionally smuggle in
    # non-English. Require >= 3 distinct English function words in
    # text >= 50 chars.
    if len(text) < 50:
        return False
    matches = set(m.group(0).lower() for m in ENGLISH_FUNCTION_WORDS.finditer(text))
    return len(matches) >= 3


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--n", type=int, default=1500)
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--output", default="corpora/sft_v1_english_replay.jsonl")
    ap.add_argument("--min-chars", type=int, default=20)
    ap.add_argument("--max-chars-question", type=int, default=2000)
    ap.add_argument("--max-chars-answer", type=int, default=4000)
    # Per-source cap prevents the streaming-order winner from dominating.
    # TULU 3 streams flan_v2_converted first; without a cap we never see
    # numinamath / no_robots / sciriff / etc. before hitting the read budget.
    ap.add_argument("--per-source-cap", type=int, default=400,
                    help="max records to keep per src_dataset (default 400)")
    ap.add_argument("--max-stream", type=int, default=250_000,
                    help="cap raw streaming reads (default 250k of TULU 3's 939k)")
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s"
    )

    try:
        from datasets import load_dataset
    except ImportError:
        print("missing: pip install datasets", file=sys.stderr)
        return 1

    # Stream the dataset (it's ~939K records, no need to download all).
    # Shuffle with a buffer so we don't see all of flan_v2_converted first
    # (TULU 3's shard order puts flan early; without shuffling, the per-source
    # cap fills on flan + no_robots and starves us of numinamath, sciriff, etc).
    #
    # CAVEAT: TULU 3's parquet shards are source-clustered AND streaming.shuffle()
    # only randomizes within the currently-loaded shard's buffered window. So a
    # single seed lands you on whichever cluster the seed's shard contains
    # (e.g. seed=42 → numinamath_tir cluster). For a true cross-source mix call
    # this script multiple times with different seeds and concatenate, OR pull
    # each subset (numinamath, no_robots, flan, sciriff, ...) explicitly.
    logging.info("streaming allenai/tulu-3-sft-mixture …")
    ds = load_dataset("allenai/tulu-3-sft-mixture", split="train", streaming=True)
    ds = ds.shuffle(buffer_size=20_000, seed=args.seed)

    rng = random.Random(args.seed)
    # Reservoir-sample 5x our target so we can filter for length, then pick n.
    pool_target = args.n * 5
    pool: list[dict] = []
    per_source_count: dict[str, int] = {}
    seen = 0

    for rec in ds:
        seen += 1
        if seen > args.max_stream:
            break
        msgs = rec.get("messages") or []
        # First user message + first assistant message
        user_msg = next((m for m in msgs if m.get("role") == "user"), None)
        asst_msg = next((m for m in msgs if m.get("role") == "assistant"), None)
        if not user_msg or not asst_msg:
            continue
        q = (user_msg.get("content") or "").strip()
        a = (asst_msg.get("content") or "").strip()
        if not q or not a:
            continue
        if len(q) < args.min_chars or len(a) < args.min_chars:
            continue
        if len(q) > args.max_chars_question:
            continue
        if len(a) > args.max_chars_answer:
            continue
        # English-only filter — requires source matches ALLOW (drops sources
        # not on the curated English list) AND has English function words.
        if not is_english(rec.get("source"), q + " " + a):
            continue
        src = rec.get("source") or "tulu3"
        if per_source_count.get(src, 0) >= args.per_source_cap:
            continue
        per_source_count[src] = per_source_count.get(src, 0) + 1
        pool.append(
            {
                "question": q,
                "answer": a,
                "src_dataset": src,
            }
        )
        if len(pool) >= pool_target:
            break

    rng.shuffle(pool)
    sampled = pool[: args.n]

    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8") as f:
        for i, p in enumerate(sampled, 1):
            rec = {
                "id": f"sft_english_{i:05d}",
                "source": "tulu3_sft",
                "src_dataset": p["src_dataset"],
                "question": p["question"],
                "question_lang": "english",
                "category": "other",
                "chunks": [],
                "answer": p["answer"],
                "skip": False,
                "skip_reason": None,
                "gold_chunk_id": None,
            }
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")

    sampled_by_src: dict[str, int] = {}
    for r in sampled:
        sampled_by_src[r["src_dataset"]] = sampled_by_src.get(r["src_dataset"], 0) + 1
    print(f"\n=== TULU subset pull summary ===", file=sys.stderr)
    print(f"  streamed : {seen}", file=sys.stderr)
    print(f"  pool     : {len(pool)}", file=sys.stderr)
    print(f"  sampled  : {len(sampled)}", file=sys.stderr)
    print(f"  output   : {out_path}", file=sys.stderr)
    print(f"  sampled by src_dataset (top 12):", file=sys.stderr)
    for src, cnt in sorted(sampled_by_src.items(), key=lambda x: -x[1])[:12]:
        print(f"    {cnt:>5} {src}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
