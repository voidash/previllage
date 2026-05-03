#!/usr/bin/env python3
"""Build the v4 dev-guard set — 60 fixed items used by the trainer's
in-loop checkpoint eval (per codex v4 spec).

Why a separate dev set vs. the existing 167-item gold:
  - Trainer's in-loop eval needs to be FAST (~1-2 min), not 30 min
  - Per-step gates check a balanced subset, not the full distribution
  - Specifically over-weights LOW-COVERAGE refusal categories where
    v3a hit 0% (education, land, pan_vat, business, tax) — codex's
    pushback was that "more refusal slice" doesn't fix the decision
    boundary unless eval actually measures the long tail

Composition (60 items):
   22 grounded         — stratified across categories from existing gold
   25 refusal          — over-weighted on low-coverage categories
   10 anti-template    — sampled from corpora/sft_v3_anti_template.jsonl
    3 partial          — ungrounded_attempt items from existing gold

Output: eval/dev_guard_v4.jsonl, same schema as gov_helpdesk_gold_v1.jsonl
so existing eval scripts (eval_sft_v1.py, eval_groundedness.py) work
unchanged via --gold dev_guard_v4.jsonl.

Usage:
    python scripts/build_dev_guard_v4.py
    # Custom seed for reproducibility:
    python scripts/build_dev_guard_v4.py --seed 42
"""
from __future__ import annotations

import argparse
import json
import logging
import random
import sys
from collections import defaultdict
from pathlib import Path


# Refusal categories where v3a was at 0% — see SFT_V3A_RESULTS.md §4.1.
# Codex's pushback: "no eval category at 0% if n>=2" — so we want at
# least 2 items per low-coverage category.
LOW_COVERAGE_REFUSAL_CATS = ["education", "land", "pan_vat", "business", "tax", "visa_immigration"]


def load_jsonl(path: Path) -> list[dict]:
    out = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                out.append(json.loads(line))
    return out


def stratified_pick(items: list[dict], n: int, by_key: str, rng: random.Random) -> list[dict]:
    """Pick n items, distributed across categories. Categories with
    fewer than n/k items contribute everything they have."""
    by_cat: dict[str, list[dict]] = defaultdict(list)
    for r in items:
        by_cat[r.get(by_key) or "other"].append(r)
    cats = sorted(by_cat)
    per_cat = max(1, n // len(cats))
    picked: list[dict] = []
    for cat in cats:
        pool = by_cat[cat]
        rng.shuffle(pool)
        picked.extend(pool[:per_cat])
    rng.shuffle(picked)
    if len(picked) > n:
        picked = picked[:n]
    elif len(picked) < n:
        # Top up from a flat shuffle of remaining
        remaining = [r for r in items if r not in picked]
        rng.shuffle(remaining)
        picked.extend(remaining[:n - len(picked)])
    return picked


def pick_refusal_balanced(refusal_items: list[dict], n: int, rng: random.Random) -> list[dict]:
    """Refusal subsample with explicit slots for low-coverage cats.
    Allocation: 2 items per low-coverage cat (12 items), then fill
    remaining with stratified pick across remaining cats."""
    by_cat: dict[str, list[dict]] = defaultdict(list)
    for r in refusal_items:
        by_cat[r.get("question_category") or "other"].append(r)
    picked: list[dict] = []
    used_ids: set[str] = set()
    # Phase 1: low-coverage cat slots
    for cat in LOW_COVERAGE_REFUSAL_CATS:
        pool = by_cat.get(cat, [])
        rng.shuffle(pool)
        take = min(2, len(pool))
        for r in pool[:take]:
            if r["id"] not in used_ids:
                picked.append(r)
                used_ids.add(r["id"])
    # Phase 2: top-up across all cats stratified
    remaining_cats = sorted(by_cat.keys())
    if len(picked) < n:
        per_cat = max(1, (n - len(picked)) // max(1, len(remaining_cats)))
        for cat in remaining_cats:
            pool = [r for r in by_cat[cat] if r["id"] not in used_ids]
            rng.shuffle(pool)
            for r in pool[:per_cat]:
                if len(picked) >= n:
                    break
                picked.append(r)
                used_ids.add(r["id"])
    rng.shuffle(picked)
    if len(picked) > n:
        picked = picked[:n]
    return picked


def normalize_anti_template(rec: dict, idx: int) -> dict:
    """Anti-template items don't have eval-style review/draft fields.
    Re-shape into the gold schema so eval_sft_v1.py reads them."""
    return {
        "id": f"dev_guard_v4_anti_{idx:03d}",
        "type": "grounded",  # treated as grounded since chunks ARE provided
        "question": rec["question"],
        "question_lang": rec.get("question_lang", "english"),
        "question_category": "anti_template",
        "candidate_chunks": rec.get("chunks", []),
        "draft_answer": rec.get("answer", ""),
        "draft_citations": [],
        "review": {
            "gold_answer": rec.get("answer", ""),
            "gold_source_urls": [c.get("url") for c in rec.get("chunks", []) if c.get("url")],
        },
        "_anti_template_seed": rec.get("id"),
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--gold", default="eval/gov_helpdesk_gold_v1.jsonl")
    ap.add_argument("--anti-template", default="corpora/sft_v3_anti_template.jsonl")
    ap.add_argument("--out", default="eval/dev_guard_v4.jsonl")
    ap.add_argument("--seed", type=int, default=42)
    ap.add_argument("--n-grounded", type=int, default=22)
    ap.add_argument("--n-refusal", type=int, default=25)
    ap.add_argument("--n-anti-template", type=int, default=10)
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s"
    )

    rng = random.Random(args.seed)
    gold_path = Path(args.gold)
    if not gold_path.exists():
        print(f"ERROR: gold not found: {gold_path}", file=sys.stderr)
        return 1
    gold = load_jsonl(gold_path)
    grounded = [r for r in gold if r["type"] == "grounded"]
    refusal = [r for r in gold if r["type"] == "refusal"]
    ungrounded = [r for r in gold if r["type"] == "ungrounded_attempt"]
    logging.info(
        "gold: %d total | %d grounded | %d refusal | %d ungrounded",
        len(gold), len(grounded), len(refusal), len(ungrounded),
    )

    picked: list[dict] = []
    picked.extend(stratified_pick(grounded, args.n_grounded, "question_category", rng))
    picked.extend(pick_refusal_balanced(refusal, args.n_refusal, rng))
    picked.extend(ungrounded)  # take all 3

    # Anti-template
    at_path = Path(args.anti_template)
    if at_path.exists():
        at_items = load_jsonl(at_path)
        rng.shuffle(at_items)
        for i, rec in enumerate(at_items[:args.n_anti_template]):
            picked.append(normalize_anti_template(rec, i))
    else:
        logging.warning("anti-template file missing: %s — skipping that slice", at_path)

    rng.shuffle(picked)

    out_path = Path(args.out)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with out_path.open("w", encoding="utf-8") as f:
        for r in picked:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")

    # Summary
    from collections import Counter
    by_type = Counter(r["type"] for r in picked)
    by_ref_cat = Counter(
        r.get("question_category") for r in picked if r["type"] == "refusal"
    )
    print(f"\n=== dev_guard_v4 ===", file=sys.stderr)
    print(f"  total: {len(picked)} → {out_path}", file=sys.stderr)
    print(f"  by type: {dict(by_type)}", file=sys.stderr)
    print(f"  refusal categories ({sum(by_ref_cat.values())} items):", file=sys.stderr)
    for cat, n in sorted(by_ref_cat.items(), key=lambda x: -x[1]):
        marker = "  *" if cat in LOW_COVERAGE_REFUSAL_CATS else "   "
        print(f"  {marker} {cat:<20s} {n}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
