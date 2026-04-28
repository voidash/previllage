#!/usr/bin/env python3
"""Merge the two draft eval files into one review-ready unified file.

Inputs:
  - eval/gov_helpdesk_v1_grounded.jsonl  (reverse-instruction; gold source by construction)
  - eval/gov_helpdesk_v1_drafts.jsonl    (reddit-based; mostly refusal)

Output (unified):
  - eval/gov_helpdesk_v1_unified.jsonl

Each unified record:
    {
      "id":                "u_grnd_001" | "u_ref_001",
      "type":              "grounded" | "refusal" | "ungrounded_attempt",
      "source":            "reverse_instruction" | "reddit",
      "original_id":       "<eval_001 / eval_g_xxx>",
      "question":          "...",
      "question_lang":     "...",
      "question_category": "...",
      "candidate_chunks":  [...],
      "draft_answer":      "...",
      "draft_citations":   [...],
      "gold_chunk":        {...},        # populated for grounded items
      "review":            {"verdict": null, "gold_answer": null, "gold_source_urls": [], "notes": ""}
    }

Refusal-vs-grounded classification rules (for type field):
  - source=reverse_instruction AND skip=False  →  type=grounded
  - source=reverse_instruction AND skip=True   →  excluded (model rejected the chunk)
  - source=reddit, draft starts with NO_SOURCE_AVAILABLE  →  type=refusal
  - source=reddit, draft is non-empty otherwise            →  type=ungrounded_attempt
                                                              (Sonnet did weave an answer
                                                               from imperfect chunks; needs
                                                               human verification of grounding)
  - source=reddit, no draft (errors)                       →  excluded
"""
from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path


def map_grounded(record: dict, idx: int) -> dict | None:
    """Map a reverse-instruction record to unified shape."""
    if record.get("skip"):
        return None
    gc = record.get("gold_chunk") or {}
    # Single-element candidate_chunks built from the gold chunk so the review
    # UI doesn't need a special case.
    candidate = {
        "rank": 1,
        "score": None,  # gold chunk by construction, not a retrieval score
        "url": gc.get("url"),
        "doc_id": None,
        "tier": gc.get("tier"),
        "text": gc.get("text"),
    }
    return {
        "id": f"u_grnd_{idx:03d}",
        "type": "grounded",
        "source": "reverse_instruction",
        "original_id": gc.get("chunk_id"),
        "question": record["question"],
        "question_lang": record.get("question_lang"),
        "question_category": record.get("question_category"),
        "difficulty": record.get("difficulty"),
        "candidate_chunks": [candidate],
        "draft_answer": record.get("answer_summary") or "",
        "draft_citations": [gc["url"]] if gc.get("url") else [],
        "gold_chunk": gc,
        "review": {
            "verdict": None,
            "gold_answer": None,
            "gold_source_urls": [],
            "notes": "",
        },
    }


def map_reddit(record: dict, idx: int) -> dict | None:
    """Map a reddit-based draft record to unified shape."""
    draft = (record.get("draft_answer") or "").strip()
    if not draft:
        return None  # error / empty — drop
    # Enriched items started life as NO_SOURCE_AVAILABLE refusals; the
    # enrich_refusals.py rewrite replaces the marker with localized text but
    # keeps the `enriched` flag so we can still classify them as refusal.
    if record.get("enriched"):
        typ = "refusal"
    elif draft.startswith("NO_SOURCE_AVAILABLE"):
        typ = "refusal"
    else:
        typ = "ungrounded_attempt"
    return {
        "id": f"u_ref_{idx:03d}",
        "type": typ,
        "source": "reddit",
        "original_id": record.get("id"),
        "question": record["question"],
        "question_lang": record.get("question_lang"),
        "question_category": record.get("question_category"),
        "difficulty": None,
        "candidate_chunks": record.get("candidate_chunks") or [],
        "draft_answer": draft,
        "draft_citations": record.get("draft_citations") or [],
        "gold_chunk": None,
        "review": {
            "verdict": None,
            "gold_answer": None,
            "gold_source_urls": [],
            "notes": "",
        },
    }


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--grounded-input", default="eval/gov_helpdesk_v1_grounded.jsonl")
    ap.add_argument("--reddit-input", default="eval/gov_helpdesk_v1_drafts.jsonl")
    ap.add_argument("--output", default="eval/gov_helpdesk_v1_unified.jsonl")
    args = ap.parse_args()

    grounded_path = Path(args.grounded_input)
    reddit_path = Path(args.reddit_input)
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)

    out_records: list[dict] = []
    counts = {"grounded": 0, "refusal": 0, "ungrounded_attempt": 0}
    excluded = {"reverse_instruction_skip": 0, "reddit_no_draft": 0}

    if grounded_path.exists():
        idx = 0
        with grounded_path.open(encoding="utf-8") as f:
            for line in f:
                r = json.loads(line)
                idx += 1
                out = map_grounded(r, idx)
                if out is None:
                    excluded["reverse_instruction_skip"] += 1
                else:
                    out_records.append(out)
                    counts[out["type"]] += 1

    if reddit_path.exists():
        idx = 0
        with reddit_path.open(encoding="utf-8") as f:
            for line in f:
                r = json.loads(line)
                idx += 1
                out = map_reddit(r, idx)
                if out is None:
                    excluded["reddit_no_draft"] += 1
                else:
                    out_records.append(out)
                    counts[out["type"]] += 1

    with out_path.open("w", encoding="utf-8") as f:
        for r in out_records:
            f.write(json.dumps(r, ensure_ascii=False) + "\n")

    print(f"=== merge summary ===", file=sys.stderr)
    print(f"  total unified items: {len(out_records)}", file=sys.stderr)
    for t, n in counts.items():
        print(f"    {t:>20}: {n}", file=sys.stderr)
    print(f"  excluded:", file=sys.stderr)
    for k, v in excluded.items():
        print(f"    {k:>30}: {v}", file=sys.stderr)
    print(f"  output: {out_path}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
