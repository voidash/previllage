#!/usr/bin/env python3
"""Audit chunks for Preeti-mojibake leakage.

Implements the v3-fix.md §2 heuristic: a chunk is "mojibake-suspect" if any
whitespace-token contains BOTH Latin alpha [A-Za-z] AND Devanagari (U+0900..U+097F)
characters. This catches the half-decoded PDF pattern where the chunker only
unmapped some glyphs and left the rest as Latin remnants.

Output: aggregate stats, per-source breakdown, and N samples per source.

Usage on k2:
    python3 scripts/audit_mojibake.py \\
        --db /Volumes/T9/gemma-god/corpus_v2/index.db \\
        --samples-per-source 3 \\
        --out-jsonl /tmp/mojibake_audit.jsonl
"""
from __future__ import annotations

import argparse
import json
import re
import sqlite3
import sys
from collections import Counter, defaultdict


_LATIN_ALPHA = re.compile(r"[A-Za-z]")
_DEVANAGARI = re.compile(r"[ऀ-ॿ]")
# Doubled vowel signs / repeated matras — strong Preeti-mojibake signal.
# Real Devanagari almost never has identical adjacent dependent vowel signs.
_DOUBLED_MATRA = re.compile(r"([ािीुूेैोौंःृ])\1")
# Strip parenthesized substrings (recursively, simple non-nested approximation):
# legit bilingual text writes Devanagari(Latin) — parens carry the English.
_PAREN_STRIP = re.compile(r"\([^()]*\)")


def _strip_parens(tok: str) -> str:
    # Apply repeatedly to handle pathological multi-level (rare in practice).
    prev = None
    cur = tok
    while prev != cur:
        prev = cur
        cur = _PAREN_STRIP.sub("", cur)
    return cur


def is_mojibake_token(tok: str) -> bool:
    """Token is suspect if EITHER:
      A) After stripping `(...)` content, it has BOTH Latin alpha AND Devanagari
         (real embedded-Latin mojibake, not legit bilingual `Devanagari(English)`)
      B) Has doubled-matra pattern (ाा, ीी, etc.) — Preeti double-decode signal
    """
    if _DOUBLED_MATRA.search(tok):
        return True
    stripped = _strip_parens(tok)
    return bool(_LATIN_ALPHA.search(stripped) and _DEVANAGARI.search(stripped))


def chunk_mojibake_score(text: str) -> tuple[int, int, list[str]]:
    """Return (n_suspect_tokens, n_total_tokens, sample_suspect_tokens[:5])."""
    toks = text.split()
    suspect = [t for t in toks if is_mojibake_token(t)]
    return len(suspect), len(toks), suspect[:5]


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--db", required=True, help="path to corpus_v2 index.db")
    ap.add_argument("--samples-per-source", type=int, default=3,
                    help="how many sample chunks to record per source (default 3)")
    ap.add_argument("--min-suspect-tokens", type=int, default=1,
                    help="chunk is mojibake if suspect_tokens >= this (default 1)")
    ap.add_argument("--out-jsonl", default=None,
                    help="optional jsonl of all mojibake-flagged chunks (id + source + score + samples)")
    args = ap.parse_args()

    conn = sqlite3.connect(args.db)
    conn.row_factory = sqlite3.Row
    cur = conn.cursor()

    cur.execute("""
        SELECT c.chunk_id, c.text, d.source_id, d.url, d.doc_id
          FROM chunks c
          JOIN documents d ON c.doc_id = d.doc_id
         WHERE d.superseded_by IS NULL AND d.removed_at IS NULL
    """)

    total_chunks = 0
    mojibake_chunks = 0
    by_source_total: Counter = Counter()
    by_source_mojibake: Counter = Counter()
    samples_by_source: dict[str, list[dict]] = defaultdict(list)
    out_rows: list[dict] = []

    for row in cur:
        total_chunks += 1
        src = row["source_id"]
        by_source_total[src] += 1
        n_susp, n_total, samp = chunk_mojibake_score(row["text"])
        if n_susp >= args.min_suspect_tokens:
            mojibake_chunks += 1
            by_source_mojibake[src] += 1
            ratio = n_susp / max(n_total, 1)
            if len(samples_by_source[src]) < args.samples_per_source:
                samples_by_source[src].append({
                    "chunk_id": row["chunk_id"],
                    "doc_id": row["doc_id"],
                    "url": row["url"],
                    "n_suspect": n_susp,
                    "n_total": n_total,
                    "ratio": round(ratio, 3),
                    "suspect_tokens": samp,
                    "text_excerpt": row["text"][:300],
                })
            if args.out_jsonl is not None:
                out_rows.append({
                    "chunk_id": row["chunk_id"],
                    "source_id": src,
                    "doc_id": row["doc_id"],
                    "url": row["url"],
                    "n_suspect": n_susp,
                    "n_total": n_total,
                    "ratio": round(ratio, 3),
                })

    if args.out_jsonl is not None:
        with open(args.out_jsonl, "w", encoding="utf-8") as f:
            for r in out_rows:
                f.write(json.dumps(r, ensure_ascii=False) + "\n")
        print(f"wrote {len(out_rows)} mojibake rows -> {args.out_jsonl}", file=sys.stderr)

    pct = 100.0 * mojibake_chunks / max(total_chunks, 1)
    print(f"\n=== Mojibake audit: {args.db} ===", file=sys.stderr)
    print(f"  total chunks (live):  {total_chunks}", file=sys.stderr)
    print(f"  mojibake-suspect:     {mojibake_chunks} ({pct:.2f}%)", file=sys.stderr)
    print(f"  threshold:            >= {args.min_suspect_tokens} suspect token(s)/chunk", file=sys.stderr)

    # Per-source table sorted by absolute mojibake count desc.
    print("\n  per-source breakdown (top sources by mojibake count):", file=sys.stderr)
    print(f"  {'source':<35s} {'mojibake':>10s} {'total':>8s} {'share':>8s}", file=sys.stderr)
    for src, n_moji in sorted(by_source_mojibake.items(), key=lambda x: -x[1])[:20]:
        n_tot = by_source_total[src]
        share = 100.0 * n_moji / max(n_tot, 1)
        print(f"  {src:<35s} {n_moji:>10d} {n_tot:>8d} {share:>7.1f}%", file=sys.stderr)

    # Optional: dump samples for the top 5 sources.
    print("\n  samples (first per source for top 5):", file=sys.stderr)
    for src, _ in sorted(by_source_mojibake.items(), key=lambda x: -x[1])[:5]:
        s = samples_by_source[src]
        if not s:
            continue
        first = s[0]
        print(f"\n  --- {src} ---", file=sys.stderr)
        print(f"  url: {first['url']}", file=sys.stderr)
        print(f"  ratio: {first['ratio']} ({first['n_suspect']}/{first['n_total']} tokens)", file=sys.stderr)
        print(f"  suspect tokens: {first['suspect_tokens']}", file=sys.stderr)
        print(f"  excerpt: {first['text_excerpt'][:200]!r}", file=sys.stderr)

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
