#!/usr/bin/env python3
"""Assemble the deterministic v4 carryover slices — no API spend, just file ops.

Handles:
  - sft_v4_grounded_terse.jsonl: filter sft_v3_terse.jsonl to keep ONLY
    grounded-terse items (have chunks). Drop the 139 open-ended terse items
    that v3a's eval suggested were the Roman-NE degen culprit.
  - sft_v4_mc.jsonl: copy v2's mc slice. (Top-up to 550 deferred — v2's 443
    is acceptable; format_sft_v4 will warn but not bail.)
  - sft_v4_brief_qa.jsonl: copy v2's brief_qa (300 items). Top-up to 700
    requires DeepSeek calls — separate task.

What this DOESN'T do:
  - Generate new grounded-terse via API (deferred — 58 items vs target 350
    is acceptable for v4 prep; the format composer will warn but not bail)
  - Pull TULU subsets (separate script handles that)
  - Re-distill grounded (that's distill_grounded_v4.py)

Usage:
    python scripts/assemble_v4_carryovers.py
"""
from __future__ import annotations

import argparse
import json
import logging
import shutil
import sys
from pathlib import Path


def filter_terse_keep_grounded(in_path: Path, out_path: Path) -> tuple[int, int, int]:
    """Read sft_v3_terse.jsonl, write only items with non-empty chunks
    (grounded-terse) to out_path. Returns (total_in, kept, dropped)."""
    total = 0
    kept = 0
    dropped = 0
    out_path.parent.mkdir(parents=True, exist_ok=True)
    with in_path.open(encoding="utf-8") as fin, out_path.open("w", encoding="utf-8") as fout:
        for line in fin:
            line = line.strip()
            if not line:
                continue
            total += 1
            r = json.loads(line)
            chunks = r.get("chunks") or []
            if chunks:
                # Update source field so the v4 composer attribues correctly.
                r["source"] = "v4_grounded_terse"
                fout.write(json.dumps(r, ensure_ascii=False) + "\n")
                kept += 1
            else:
                dropped += 1
    return total, kept, dropped


def copy_through(in_path: Path, out_path: Path, new_source: str | None = None) -> int:
    """Copy a JSONL through, optionally rewriting `source` field per-record."""
    n = 0
    out_path.parent.mkdir(parents=True, exist_ok=True)
    if new_source is None:
        shutil.copyfile(in_path, out_path)
        with in_path.open(encoding="utf-8") as f:
            for line in f:
                if line.strip():
                    n += 1
        return n
    with in_path.open(encoding="utf-8") as fin, out_path.open("w", encoding="utf-8") as fout:
        for line in fin:
            line = line.strip()
            if not line:
                continue
            r = json.loads(line)
            r["source"] = new_source
            fout.write(json.dumps(r, ensure_ascii=False) + "\n")
            n += 1
    return n


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpora-dir", default="corpora")
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s"
    )

    corp = Path(args.corpora_dir)
    summary: list[tuple[str, str]] = []

    # 1) Terse: drop open-ended, keep grounded-terse
    src = corp / "sft_v3_terse.jsonl"
    dst = corp / "sft_v4_grounded_terse.jsonl"
    if src.exists():
        total, kept, dropped = filter_terse_keep_grounded(src, dst)
        logging.info(
            "terse: total=%d → kept grounded-terse=%d (dropped open-ended=%d)",
            total, kept, dropped,
        )
        summary.append((str(dst), f"{kept} grounded-terse (dropped {dropped} open-ended)"))
    else:
        logging.warning("missing %s — skipping terse pruning", src)

    # 2) MC: copy v2 forward (top-up deferred)
    src = corp / "sft_v2_mc.jsonl"
    dst = corp / "sft_v4_mc.jsonl"
    if src.exists():
        n = copy_through(src, dst)
        logging.info("mc: copied %d records", n)
        summary.append((str(dst), f"{n} records (target 550 — top-up deferred)"))
    else:
        logging.warning("missing %s — skipping mc copy", src)

    # 3) Brief QA: copy v2 forward
    src = corp / "sft_v2_brief_qa.jsonl"
    dst = corp / "sft_v4_brief_qa.jsonl"
    if src.exists():
        n = copy_through(src, dst)
        logging.info("brief_qa: copied %d records", n)
        summary.append((str(dst), f"{n} records (target 700 — top-up deferred)"))
    else:
        logging.warning("missing %s — skipping brief_qa copy", src)

    print(f"\n=== assemble_v4_carryovers ===", file=sys.stderr)
    for path, note in summary:
        print(f"  {path:<40s} {note}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
