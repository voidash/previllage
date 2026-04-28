#!/usr/bin/env python3
"""Sample N questions from the classified pool for the groundedness eval set.

Reads `corpora/reddit_gov_questions_classified.jsonl`, filters to genuine
questions (`yes_procedure` and `yes_info`), and samples N items stratified
by (category, lang) so the eval set reflects a balanced mix.

Cap-per-category prevents one bucket (e.g., passport) from dominating.

Usage:
    python scripts/sample_eval_questions.py                          # N=100
    python scripts/sample_eval_questions.py --n 50 --seed 7
    python scripts/sample_eval_questions.py --include-classes yes_procedure  # stricter
"""
from __future__ import annotations

import argparse
import json
import random
import sys
from collections import defaultdict
from pathlib import Path


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument(
        "--input", default="corpora/reddit_gov_questions_classified.jsonl"
    )
    ap.add_argument("--output", default="corpora/eval_sample_v1.jsonl")
    ap.add_argument("--n", type=int, default=100, help="target sample size")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument(
        "--include-classes",
        default="yes_procedure,yes_info",
        help="comma-separated classes to retain",
    )
    ap.add_argument(
        "--cap-per-category",
        type=int,
        default=20,
        help="max samples per category to keep diversity",
    )
    args = ap.parse_args()

    classes = {c.strip() for c in args.include_classes.split(",") if c.strip()}
    rng = random.Random(args.seed)

    in_path = Path(args.input)
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    if not in_path.exists():
        print(f"input not found: {in_path}", file=sys.stderr)
        print(
            "Run scripts/classify_gov_questions.py first.", file=sys.stderr
        )
        return 1

    pool: list[dict] = []
    raw_count = 0
    by_class: dict[str, int] = defaultdict(int)
    with in_path.open(encoding="utf-8") as f:
        for line in f:
            r = json.loads(line)
            raw_count += 1
            by_class[r["class"]] += 1
            if r["class"] in classes:
                pool.append(r)

    print(f"=== input ===", file=sys.stderr)
    print(f"  total classified: {raw_count}", file=sys.stderr)
    print(f"  by class: {dict(by_class)}", file=sys.stderr)
    print(
        f"  pool (after class filter): {len(pool)} (classes={sorted(classes)})",
        file=sys.stderr,
    )

    if not pool:
        print("empty pool — relax --include-classes or run classifier first", file=sys.stderr)
        return 2

    # Bucket by category. Within each bucket, shuffle then truncate to cap.
    buckets: dict[str, list[dict]] = defaultdict(list)
    for r in pool:
        buckets[r["category"]].append(r)
    for cat in buckets:
        rng.shuffle(buckets[cat])
        buckets[cat] = buckets[cat][: args.cap_per_category]

    # Round-robin across categories until N items collected. Within a bucket,
    # rotate by lang so we don't drain one language first.
    by_cat_lang: dict[tuple[str, str], list[dict]] = defaultdict(list)
    for cat, items in buckets.items():
        for it in items:
            by_cat_lang[(cat, it.get("lang", "unknown"))].append(it)

    cells = list(by_cat_lang.keys())
    rng.shuffle(cells)
    sampled: list[dict] = []
    while len(sampled) < args.n and any(by_cat_lang[c] for c in cells):
        for c in cells:
            if not by_cat_lang[c]:
                continue
            sampled.append(by_cat_lang[c].pop())
            if len(sampled) >= args.n:
                break

    rng.shuffle(sampled)

    # Assemble eval-item records.
    out_records: list[dict] = []
    cat_count: dict[str, int] = defaultdict(int)
    lang_count: dict[str, int] = defaultdict(int)
    for i, r in enumerate(sampled, 1):
        out_records.append(
            {
                "id": f"eval_{i:03d}",
                "source_id": r["id"],
                "question": r["body"],
                "question_lang": r.get("lang"),
                "question_category": r["category"],
                "question_class": r["class"],
                "reddit_score_hint": r.get("score"),
                "rationale": r.get("rationale"),
            }
        )
        cat_count[r["category"]] += 1
        lang_count[r.get("lang", "unknown")] += 1

    with out_path.open("w", encoding="utf-8") as f_out:
        for rec in out_records:
            f_out.write(json.dumps(rec, ensure_ascii=False) + "\n")

    print(f"\n=== sampled ===", file=sys.stderr)
    print(f"  total: {len(out_records)}", file=sys.stderr)
    print(f"  by category: {dict(sorted(cat_count.items()))}", file=sys.stderr)
    print(f"  by lang: {dict(sorted(lang_count.items()))}", file=sys.stderr)
    print(f"  output: {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
