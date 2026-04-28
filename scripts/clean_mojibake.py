#!/usr/bin/env python3
"""Strip mojibake tokens (Latin+Devanagari conjoined inside a single
whitespace-separated word) from eval items.

Definition of a mojibake token: any whitespace-separated token that contains
BOTH a Latin letter (A-Z/a-z) AND a Devanagari character (U+0900–U+097F)
without an intervening whitespace. These are nearly always PDF-extraction or
Reddit-encoding artifacts. Legitimate code-mixed Nepali (e.g., "PAN
प्रमाणपत्र") uses spaces between scripts and is preserved.

Cleans, in place:
  - eval/gov_helpdesk_v1_drafts.jsonl
  - eval/gov_helpdesk_v1_grounded.jsonl

Then re-runs scripts/merge_eval.py to refresh the unified file.

Usage:
    python scripts/clean_mojibake.py                # clean both source files + remerge
    python scripts/clean_mojibake.py --dry-run      # report counts only
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path
from typing import Iterable

LATIN_RE = re.compile(r"[A-Za-z]")
# Devanagari LETTERS / vowel signs / combining marks only.
# Excludes U+0964-U+096F (danda, double-danda, digits ०१२३४५६७८९) since those
# legitimately appear adjacent to Latin in Roman-Nepali sentences.
DEVA_RE = re.compile(r"[ऀ-ॣ॰-ॿ]")


def is_mojibake_token(tok: str) -> bool:
    """True if this whitespace-separated token contains both Latin and
    Devanagari characters with no whitespace between them."""
    return bool(LATIN_RE.search(tok)) and bool(DEVA_RE.search(tok))


def clean_text(text: str) -> tuple[str, int]:
    """Return (cleaned_text, n_dropped_tokens). Preserves whitespace
    structure: dropped tokens become a single space."""
    if not text or not isinstance(text, str):
        return text, 0
    # Split preserving whitespace runs as separators by interleaving with
    # re.split's keep-delim trick. Simpler: use re.findall to grab tokens
    # and whitespace, then rebuild.
    pieces = re.findall(r"\S+|\s+", text)
    n_dropped = 0
    out = []
    for p in pieces:
        if p.isspace():
            out.append(p)
        elif is_mojibake_token(p):
            n_dropped += 1
            out.append(" ")  # preserve word-boundary spacing
        else:
            out.append(p)
    cleaned = "".join(out)
    # Collapse multi-spaces left behind (but keep newlines).
    cleaned = re.sub(r"[ \t]{2,}", " ", cleaned)
    return cleaned, n_dropped


def clean_record(record: dict, fields: Iterable[str]) -> tuple[dict, int]:
    """Clean specified top-level string fields. Returns (record, total dropped tokens)."""
    total = 0
    for fname in fields:
        if fname in record and isinstance(record[fname], str):
            new_val, n = clean_text(record[fname])
            record[fname] = new_val
            total += n
    return record, total


def clean_chunks(chunks_list: list, text_field: str = "text") -> int:
    """Clean text inside a list of chunk dicts. Returns count of dropped tokens."""
    if not isinstance(chunks_list, list):
        return 0
    total = 0
    for c in chunks_list:
        if isinstance(c, dict) and isinstance(c.get(text_field), str):
            new_val, n = clean_text(c[text_field])
            c[text_field] = new_val
            total += n
    return total


def clean_file(path: Path, dry_run: bool, schema: str) -> dict:
    """Clean a JSONL file in place. schema=`drafts` or `grounded`. Returns stats."""
    if not path.exists():
        return {"path": str(path), "exists": False}
    records: list[dict] = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            records.append(json.loads(line))

    n_records = len(records)
    n_records_touched = 0
    total_dropped = 0
    for r in records:
        before = total_dropped

        if schema == "drafts":
            # Reddit-based draft schema (build_groundedness_eval / enrich_refusals)
            r, n = clean_record(r, ["question", "body", "draft_answer"])
            total_dropped += n
            total_dropped += clean_chunks(r.get("candidate_chunks") or [], "text")
        elif schema == "grounded":
            # Reverse-instruction schema (generate_grounded_eval)
            r, n = clean_record(r, ["question", "answer_summary"])
            total_dropped += n
            gc = r.get("gold_chunk") or {}
            if isinstance(gc.get("text"), str):
                new_text, n = clean_text(gc["text"])
                gc["text"] = new_text
                total_dropped += n
        else:
            raise ValueError(f"unknown schema: {schema}")

        if total_dropped > before:
            n_records_touched += 1

    if not dry_run:
        with path.open("w", encoding="utf-8") as f:
            for r in records:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")

    return {
        "path": str(path),
        "exists": True,
        "records": n_records,
        "records_touched": n_records_touched,
        "tokens_dropped": total_dropped,
    }


def clean_gold_file(path: Path, dry_run: bool) -> dict:
    """Clean a gold-review file (eval/gov_helpdesk_gold_v1.jsonl) in place.

    Each record carries a frozen snapshot of the unified item plus a `review`
    block. We clean the same fields as on the unified items, plus the
    `review.gold_answer` if present (since the user's edited gold answer may
    contain mojibake too).
    """
    if not path.exists():
        return {"path": str(path), "exists": False}
    records: list[dict] = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                continue

    total_dropped = 0
    n_records_touched = 0
    for r in records:
        before = total_dropped
        # Same fields as unified items.
        r, n = clean_record(r, ["question", "draft_answer"])
        total_dropped += n
        total_dropped += clean_chunks(r.get("candidate_chunks") or [], "text")
        gc = r.get("gold_chunk") or {}
        if isinstance(gc.get("text"), str):
            new_text, n = clean_text(gc["text"])
            gc["text"] = new_text
            total_dropped += n
        rev = r.get("review") or {}
        if isinstance(rev.get("gold_answer"), str):
            new_answer, n = clean_text(rev["gold_answer"])
            rev["gold_answer"] = new_answer
            total_dropped += n
        if total_dropped > before:
            n_records_touched += 1

    if not dry_run:
        with path.open("w", encoding="utf-8") as f:
            for r in records:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")

    return {
        "path": str(path),
        "exists": True,
        "records": len(records),
        "records_touched": n_records_touched,
        "tokens_dropped": total_dropped,
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--drafts", default="eval/gov_helpdesk_v1_drafts.jsonl")
    ap.add_argument("--grounded", default="eval/gov_helpdesk_v1_grounded.jsonl")
    ap.add_argument("--gold", default="eval/gov_helpdesk_gold_v1.jsonl")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--no-remerge", action="store_true")
    args = ap.parse_args()

    print(f"=== clean_mojibake (dry_run={args.dry_run}) ===", file=sys.stderr)

    stats_drafts = clean_file(Path(args.drafts), args.dry_run, schema="drafts")
    stats_grounded = clean_file(Path(args.grounded), args.dry_run, schema="grounded")
    stats_gold = clean_gold_file(Path(args.gold), args.dry_run)

    for s in (stats_drafts, stats_grounded, stats_gold):
        if not s.get("exists"):
            print(f"  {s['path']}: not found", file=sys.stderr)
            continue
        print(
            f"  {s['path']}: {s['records']} records, "
            f"{s['records_touched']} touched, "
            f"{s['tokens_dropped']} mojibake tokens dropped",
            file=sys.stderr,
        )

    if args.dry_run:
        print("\ndry-run: no changes written.", file=sys.stderr)
        return 0

    if args.no_remerge:
        print("\nskipping re-merge (--no-remerge).", file=sys.stderr)
        return 0

    print("\nre-merging unified...", file=sys.stderr)
    rc = subprocess.call(
        [sys.executable, "scripts/merge_eval.py"],
    )
    return rc


if __name__ == "__main__":
    raise SystemExit(main())
