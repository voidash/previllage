#!/usr/bin/env python3
"""CLI review tool for the groundedness eval drafts.

Walks through `eval/gov_helpdesk_v1_drafts.jsonl`, presents each draft
(question + retrieved chunks + draft answer + cited URLs) for human verdict,
and writes the reviewed gold standard to `eval/gov_helpdesk_gold_v1.jsonl`.

Verdicts:
    a — approve as-is (draft becomes gold)
    e — edit the answer in $EDITOR (draft becomes gold after edit)
    d — drop this item (excluded from gold)
    s — skip for now (return to it later)
    q — quit (resumable)

Resumable: items already in the gold file (by id) are skipped on next run.
"""
from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


def load_jsonl(path: Path) -> list[dict]:
    if not path.exists():
        return []
    out = []
    with path.open(encoding="utf-8") as f:
        for line in f:
            out.append(json.loads(line))
    return out


def append_jsonl(path: Path, record: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("a", encoding="utf-8") as f:
        f.write(json.dumps(record, ensure_ascii=False) + "\n")
        f.flush()


def edit_text(initial: str, suffix: str = ".md") -> str:
    """Open $EDITOR with `initial`, return user-edited content."""
    editor = os.environ.get("EDITOR") or os.environ.get("VISUAL") or "vi"
    # Verify editor exists.
    if shutil.which(editor.split()[0]) is None:
        print(f"  [warn] editor {editor!r} not found; falling back to vi", file=sys.stderr)
        editor = "vi"
    with tempfile.NamedTemporaryFile(
        mode="w+", suffix=suffix, delete=False, encoding="utf-8"
    ) as tf:
        tf.write(initial)
        tmp_path = tf.name
    try:
        subprocess.call(f"{editor} {tmp_path}", shell=True)
        with open(tmp_path, encoding="utf-8") as f:
            return f.read().rstrip()
    finally:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass


def fmt_chunk(c: dict, max_text: int = 600) -> str:
    text = c.get("text", "")
    if len(text) > max_text:
        text = text[:max_text] + " ... [truncated]"
    return (
        f"  [{c.get('rank')}] score={c.get('score')} tier={c.get('tier')}\n"
        f"      url: {c.get('url')}\n"
        f"      {text}"
    )


def render_item(item: dict) -> str:
    """Type-aware rendering for the unified eval schema (see scripts/merge_eval.py).

    type=grounded            → constructed from a known chunk; show that chunk
                               and ask "is the question + answer_summary good?"
    type=refusal             → reddit-based, model said NO_SOURCE_AVAILABLE; show
                               retrieved chunks and ask "is the refusal correct?"
    type=ungrounded_attempt  → reddit-based, model wove an answer from imperfect
                               chunks; show both and ask "is this grounded enough?"
    """
    typ = item.get("type", "?")
    type_marker = {
        "grounded": "[GROUNDED ↳ verify question + answer match the chunk]",
        "refusal": "[REFUSAL ↳ verify refusal is correct OR provide right URL]",
        "ungrounded_attempt": "[UNGROUNDED ATTEMPT ↳ verify grounding carefully]",
    }.get(typ, f"[{typ}]")

    lines = [
        "=" * 78,
        f"id          : {item.get('id')}  {type_marker}",
        f"source      : {item.get('source')} / orig_id={item.get('original_id')}",
        f"lang/cat    : {item.get('question_lang')} / {item.get('question_category')}"
        + (f"  diff={item.get('difficulty')}" if item.get("difficulty") else ""),
        "",
        "QUESTION:",
        f"  {item.get('question','').strip()}",
        "",
    ]

    chunks = item.get("candidate_chunks") or []
    chunk_label = "GOLD CHUNK" if typ == "grounded" else f"RETRIEVED CHUNKS ({len(chunks)})"
    lines.append(f"{chunk_label}:")
    show_n = 1 if typ == "grounded" else 5
    for c in chunks[:show_n]:
        lines.append(fmt_chunk(c, max_text=900 if typ == "grounded" else 600))
        lines.append("")
    if len(chunks) > show_n:
        lines.append(f"  ... and {len(chunks) - show_n} more (suppressed)")
    lines.append("")

    if typ == "grounded":
        lines.append("ANSWER SUMMARY (Sonnet's one-liner from the gold chunk):")
    else:
        lines.append("DRAFT ANSWER:")
    ans = item.get("draft_answer") or "(no draft)"
    lines.append(ans)
    lines.append("")
    lines.append(f"DRAFT CITATIONS: {item.get('draft_citations') or []}")
    return "\n".join(lines)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", default="eval/gov_helpdesk_v1_unified.jsonl")
    ap.add_argument("--output", default="eval/gov_helpdesk_gold_v1.jsonl")
    ap.add_argument(
        "--type",
        default=None,
        choices=["grounded", "refusal", "ungrounded_attempt"],
        help="filter to only one item type (default: all)",
    )
    ap.add_argument(
        "--start",
        default=None,
        help="resume at a specific id (otherwise auto-resume from output)",
    )
    args = ap.parse_args()

    in_path = Path(args.input)
    out_path = Path(args.output)
    if not in_path.exists():
        print(f"input not found: {in_path}", file=sys.stderr)
        return 1

    drafts = load_jsonl(in_path)
    if args.type:
        before = len(drafts)
        drafts = [d for d in drafts if d.get("type") == args.type]
        print(
            f"--type={args.type}: filtered {before} → {len(drafts)} items",
            file=sys.stderr,
        )

    reviewed_ids = {r["id"] for r in load_jsonl(out_path)}
    print(
        f"loaded {len(drafts)} drafts; {len(reviewed_ids)} already reviewed", file=sys.stderr
    )

    if args.start:
        try:
            i_start = next(i for i, d in enumerate(drafts) if d["id"] == args.start)
            drafts = drafts[i_start:]
        except StopIteration:
            print(f"--start id not found: {args.start}", file=sys.stderr)
            return 2

    pending = [d for d in drafts if d["id"] not in reviewed_ids]
    if not pending:
        print("all drafts already reviewed", file=sys.stderr)
        return 0

    print(f"\n{len(pending)} drafts pending review.\n", file=sys.stderr)
    n_approved = 0
    n_edited = 0
    n_dropped = 0

    for idx, item in enumerate(pending, 1):
        print(f"\n[{idx}/{len(pending)}]")
        print(render_item(item))

        while True:
            choice = input("\nverdict [a/e/d/s/q] > ").strip().lower()
            if choice == "a":
                rec = dict(item)
                rec["review"] = {
                    "verdict": "approved",
                    "gold_answer": item.get("draft_answer"),
                    "gold_source_urls": list(item.get("draft_citations") or []),
                    "notes": "",
                }
                append_jsonl(out_path, rec)
                n_approved += 1
                break
            elif choice == "e":
                edited = edit_text(item.get("draft_answer") or "", suffix=".md")
                # Re-extract citations from the edited text.
                import re

                citations = list(
                    dict.fromkeys(re.findall(r"\[(https?://[^\]\s]+)\]", edited))
                )
                notes = input("notes (optional) > ").strip()
                rec = dict(item)
                rec["review"] = {
                    "verdict": "edited",
                    "gold_answer": edited,
                    "gold_source_urls": citations,
                    "notes": notes,
                }
                append_jsonl(out_path, rec)
                n_edited += 1
                break
            elif choice == "d":
                reason = input("drop reason (optional) > ").strip()
                rec = dict(item)
                rec["review"] = {
                    "verdict": "dropped",
                    "gold_answer": None,
                    "gold_source_urls": [],
                    "notes": reason,
                }
                append_jsonl(out_path, rec)
                n_dropped += 1
                break
            elif choice == "s":
                print("  skipped (return to it later via re-run)")
                break
            elif choice == "q":
                print("\nquitting; resumable next run.", file=sys.stderr)
                _print_summary(n_approved, n_edited, n_dropped, out_path)
                return 0
            else:
                print("  invalid choice — use one of: a / e / d / s / q")

    _print_summary(n_approved, n_edited, n_dropped, out_path)
    return 0


def _print_summary(n_approved: int, n_edited: int, n_dropped: int, out_path: Path) -> None:
    print(
        f"\n=== review summary ===\n"
        f"  approved: {n_approved}\n"
        f"  edited  : {n_edited}\n"
        f"  dropped : {n_dropped}\n"
        f"  output  : {out_path}",
        file=sys.stderr,
    )


if __name__ == "__main__":
    raise SystemExit(main())
