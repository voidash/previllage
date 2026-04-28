#!/usr/bin/env python3
"""Browser-based review UI for the unified eval drafts.

Serves a single-page app on http://127.0.0.1:8765 that renders Devanagari /
Roman-Nepali / English text properly via Google-Fonts Noto Sans Devanagari.
Verdicts persist to `eval/gov_helpdesk_gold_v1.jsonl` (append-only; last
write per id wins on reload).

Usage:
    python scripts/review_web.py
    open http://127.0.0.1:8765

Keyboard shortcuts (in browser):
    a = approve    e = edit    d = drop    s = skip
    n / j = next   p / k = prev
    Esc = cancel edit
"""
from __future__ import annotations

import argparse
import datetime as dt
import http.server
import json
import logging
import socket
import socketserver
import sys
import threading
from pathlib import Path
from urllib.parse import urlparse


# ---- Storage layer ---------------------------------------------------------


class Store:
    """In-memory + on-disk store for unified drafts and reviewed gold records."""

    def __init__(self, drafts_path: Path, gold_path: Path):
        self.drafts_path = drafts_path
        self.gold_path = gold_path
        self.lock = threading.Lock()
        self.items: list[dict] = []
        self.by_id: dict[str, dict] = {}
        self.review_by_id: dict[str, dict] = {}  # id -> latest gold record

        self._load_drafts()
        self._load_gold()

    def _load_drafts(self) -> None:
        with self.drafts_path.open(encoding="utf-8") as f:
            for line in f:
                r = json.loads(line)
                self.items.append(r)
                self.by_id[r["id"]] = r
        logging.info("loaded %d drafts from %s", len(self.items), self.drafts_path)

    def _load_gold(self) -> None:
        if not self.gold_path.exists():
            return
        with self.gold_path.open(encoding="utf-8") as f:
            for line in f:
                try:
                    r = json.loads(line)
                except json.JSONDecodeError:
                    continue
                # Last write per id wins.
                self.review_by_id[r["id"]] = r
        logging.info(
            "loaded %d existing gold reviews from %s",
            len(self.review_by_id),
            self.gold_path,
        )

    def list_summary(self, type_filter: str | None) -> list[dict]:
        out = []
        for r in self.items:
            if type_filter and r.get("type") != type_filter:
                continue
            rev = self.review_by_id.get(r["id"])
            out.append(
                {
                    "id": r["id"],
                    "type": r.get("type"),
                    "question_lang": r.get("question_lang"),
                    "question_category": r.get("question_category"),
                    "verdict": (rev or {}).get("review", {}).get("verdict"),
                    "question_preview": (r.get("question") or "")[:120],
                }
            )
        return out

    def get(self, item_id: str) -> dict | None:
        item = self.by_id.get(item_id)
        if not item:
            return None
        # Layer the latest review on top, if any.
        rev = self.review_by_id.get(item_id)
        out = dict(item)
        if rev:
            out["review"] = rev["review"]
        return out

    def save_review(self, item_id: str, review: dict) -> dict:
        """Append a reviewed record to the gold file. Returns the saved record."""
        item = self.by_id.get(item_id)
        if not item:
            raise KeyError(item_id)
        record = dict(item)
        record["review"] = {
            "verdict": review.get("verdict"),
            "gold_answer": review.get("gold_answer"),
            "gold_source_urls": review.get("gold_source_urls") or [],
            "notes": review.get("notes") or "",
            "reviewed_at": dt.datetime.now(dt.timezone.utc)
            .replace(microsecond=0)
            .isoformat()
            .replace("+00:00", "Z"),
        }
        with self.lock:
            self.gold_path.parent.mkdir(parents=True, exist_ok=True)
            with self.gold_path.open("a", encoding="utf-8") as f:
                f.write(json.dumps(record, ensure_ascii=False) + "\n")
                f.flush()
            self.review_by_id[item_id] = record
        return record

    def stats(self) -> dict:
        counts_by_type: dict[str, dict[str, int]] = {}
        for r in self.items:
            t = r.get("type", "?")
            cell = counts_by_type.setdefault(
                t, {"total": 0, "approved": 0, "edited": 0, "dropped": 0, "pending": 0}
            )
            cell["total"] += 1
            verdict = (
                self.review_by_id.get(r["id"], {})
                .get("review", {})
                .get("verdict")
            )
            if verdict in cell:
                cell[verdict] += 1
            else:
                cell["pending"] += 1
        return counts_by_type


# ---- HTTP handler ----------------------------------------------------------


HTML_PAGE = r"""<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>Eval Review</title>
  <style>
    :root {
      --bg: #fafafa;
      --fg: #1a1a1a;
      --muted: #666;
      --border: #d0d0d0;
      --chunk-bg: #f3f3f3;
      --accent: #1f6feb;
      --green: #1a7f37;
      --orange: #bf6900;
      --red: #cf222e;
      --grey: #6e7781;
    }
    * { box-sizing: border-box; }
    body {
      font-family: -apple-system, BlinkMacSystemFont, "Kohinoor Devanagari", "Devanagari MT", "Noto Sans Devanagari", "Helvetica Neue", system-ui, sans-serif;
      background: var(--bg);
      color: var(--fg);
      margin: 0;
      padding: 0;
      font-size: 15px;
      line-height: 1.5;
    }
    code, pre, .mono {
      font-family: ui-monospace, "SF Mono", Menlo, "JetBrains Mono", monospace;
      font-size: 13px;
    }
    header {
      position: sticky; top: 0; z-index: 5;
      background: white;
      border-bottom: 1px solid var(--border);
      padding: 10px 18px;
      display: flex;
      gap: 18px;
      align-items: center;
      flex-wrap: wrap;
    }
    header h1 { font-size: 16px; margin: 0; }
    .stats { color: var(--muted); font-size: 13px; }
    .stats b { color: var(--fg); }
    select, button, input[type=text] {
      font-family: inherit;
      font-size: 14px;
      padding: 6px 10px;
      border: 1px solid var(--border);
      border-radius: 4px;
      background: white;
    }
    button { cursor: pointer; }
    button:hover { background: #f0f0f0; }
    button.approve { color: white; background: var(--green); border-color: var(--green); }
    button.approve:hover { background: #15692c; }
    button.edit { color: white; background: var(--orange); border-color: var(--orange); }
    button.edit:hover { background: #9c5500; }
    button.drop { color: white; background: var(--red); border-color: var(--red); }
    button.drop:hover { background: #a8161f; }
    button.skip { background: white; }
    main { max-width: 940px; margin: 0 auto; padding: 18px; }
    .item-meta {
      color: var(--muted);
      font-size: 13px;
      margin-bottom: 6px;
    }
    .type-badge {
      display: inline-block;
      padding: 2px 8px;
      border-radius: 10px;
      font-size: 11px;
      font-weight: 500;
      margin-right: 8px;
      text-transform: uppercase;
    }
    .type-badge.grounded { background: #dcfce7; color: var(--green); }
    .type-badge.refusal { background: #fef3c7; color: var(--orange); }
    .type-badge.ungrounded_attempt { background: #fee2e2; color: var(--red); }
    .verdict-badge {
      display: inline-block;
      padding: 2px 8px;
      border-radius: 10px;
      font-size: 11px;
      margin-left: 6px;
    }
    .verdict-badge.approved { background: #dcfce7; color: var(--green); }
    .verdict-badge.edited { background: #fef3c7; color: var(--orange); }
    .verdict-badge.dropped { background: #fee2e2; color: var(--red); }
    .verdict-badge.pending { background: #e5e5e5; color: var(--grey); }
    .question {
      font-size: 18px;
      padding: 12px 16px;
      background: white;
      border: 1px solid var(--border);
      border-radius: 6px;
      margin: 8px 0 18px;
      white-space: pre-wrap;
      word-break: break-word;
    }
    h3 {
      font-size: 13px;
      text-transform: uppercase;
      letter-spacing: 0.5px;
      color: var(--muted);
      margin: 18px 0 6px;
    }
    .chunk {
      background: var(--chunk-bg);
      border-left: 3px solid var(--border);
      padding: 8px 12px;
      margin-bottom: 8px;
      border-radius: 0 4px 4px 0;
    }
    .chunk.gold { border-left-color: var(--green); }
    .chunk-meta {
      font-size: 11px;
      color: var(--muted);
      margin-bottom: 4px;
      font-family: 'JetBrains Mono', monospace;
    }
    .chunk-text {
      font-size: 14px;
      white-space: pre-wrap;
      word-break: break-word;
      max-height: 280px;
      overflow-y: auto;
    }
    .chunk-text.full { max-height: none; }
    .draft {
      background: white;
      border: 1px solid var(--border);
      padding: 12px 16px;
      border-radius: 6px;
      white-space: pre-wrap;
      word-break: break-word;
      font-size: 15px;
    }
    .draft.no-source { background: #fef3c7; border-color: var(--orange); }
    .actions {
      position: sticky;
      bottom: 0;
      background: white;
      border-top: 1px solid var(--border);
      padding: 12px 18px;
      margin: 18px -18px -18px;
      display: flex;
      gap: 10px;
      flex-wrap: wrap;
    }
    .actions .spacer { flex: 1; }
    .editor {
      display: none;
      margin-top: 12px;
    }
    .editor.active { display: block; }
    .editor textarea {
      width: 100%;
      min-height: 200px;
      font-family: inherit;
      font-size: 15px;
      padding: 10px;
      border: 1px solid var(--border);
      border-radius: 6px;
      resize: vertical;
    }
    .editor input[type=text] {
      width: 100%;
      margin-top: 6px;
    }
    .kbd-hint {
      font-size: 11px;
      color: var(--muted);
      margin-left: 4px;
    }
    .empty { text-align: center; color: var(--muted); padding: 40px; }
  </style>
</head>
<body>
<header>
  <h1>Eval Review</h1>
  <span class="stats" id="stats">loading…</span>
  <span style="flex:1"></span>
  <label>type:
    <select id="type-filter">
      <option value="">all</option>
      <option value="grounded">grounded</option>
      <option value="refusal">refusal</option>
      <option value="ungrounded_attempt">ungrounded_attempt</option>
    </select>
  </label>
  <label>show:
    <select id="show-filter">
      <option value="pending">pending</option>
      <option value="all">all</option>
      <option value="reviewed">reviewed</option>
    </select>
  </label>
  <button id="prev">← prev <span class="kbd-hint">[p]</span></button>
  <button id="next">next → <span class="kbd-hint">[n]</span></button>
</header>

<main>
  <div id="root"><div class="empty">loading…</div></div>
</main>

<script>
const state = {
  items: [],
  filtered: [],
  cursor: 0,
  current: null,
  editing: false,
};

async function api(path, opts={}) {
  const r = await fetch(path, opts);
  if (!r.ok) throw new Error(`${r.status} ${await r.text()}`);
  return r.json();
}

function applyFilters() {
  const t = document.getElementById('type-filter').value;
  const show = document.getElementById('show-filter').value;
  state.filtered = state.items.filter(it => {
    if (t && it.type !== t) return false;
    const reviewed = !!it.verdict;
    if (show === 'pending' && reviewed) return false;
    if (show === 'reviewed' && !reviewed) return false;
    return true;
  });
  if (state.cursor >= state.filtered.length) state.cursor = Math.max(0, state.filtered.length - 1);
}

async function refreshList() {
  state.items = await api('/api/items');
  applyFilters();
  await renderCurrent();
  await renderStats();
}

async function renderStats() {
  const stats = await api('/api/stats');
  const out = [];
  for (const t of ['grounded','refusal','ungrounded_attempt']) {
    const c = stats[t];
    if (!c) continue;
    out.push(`<b>${t}</b> ${c.total - c.pending}/${c.total}  (✓${c.approved} ✎${c.edited} ✗${c.dropped})`);
  }
  document.getElementById('stats').innerHTML = out.join(' &nbsp; ');
}

function escapeHtml(s) {
  return (s ?? '').replace(/[&<>"']/g, c => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'})[c]);
}

function chunkBlock(c, gold=false) {
  return `<div class="chunk ${gold ? 'gold' : ''}">
    <div class="chunk-meta">[${c.rank ?? '?'}] score=${c.score === null || c.score === undefined ? '?' : c.score} tier=${c.tier ?? '?'} <a href="${escapeHtml(c.url)}" target="_blank" rel="noopener">${escapeHtml(c.url)}</a></div>
    <div class="chunk-text">${escapeHtml(c.text)}</div>
  </div>`;
}

async function renderCurrent() {
  const root = document.getElementById('root');
  if (!state.filtered.length) {
    root.innerHTML = `<div class="empty">no items match the current filters.</div>`;
    return;
  }
  const summary = state.filtered[state.cursor];
  state.current = await api(`/api/item/${encodeURIComponent(summary.id)}`);
  state.editing = false;
  const item = state.current;
  const review = item.review || {};
  const verdict = review.verdict;
  const verdictBadge = verdict
    ? `<span class="verdict-badge ${verdict}">✓ ${verdict}</span>`
    : `<span class="verdict-badge pending">pending</span>`;
  const chunks = item.candidate_chunks || [];
  const isGrounded = item.type === 'grounded';
  const draftIsNoSource = (item.draft_answer || '').startsWith('NO_SOURCE_AVAILABLE');
  const draftPrefill = isGrounded
    ? (item.draft_answer || '')
    : (item.draft_answer || '');

  root.innerHTML = `
    <div class="item-meta">
      <span class="type-badge ${item.type}">${item.type}</span>
      ${verdictBadge}
      <code>${escapeHtml(item.id)}</code> · ${escapeHtml(item.question_lang || '?')} / ${escapeHtml(item.question_category || '?')}
      · src=${escapeHtml(item.source || '?')} (orig ${escapeHtml(item.original_id || '?')})
      <span style="float:right; color:var(--muted)">${state.cursor + 1} / ${state.filtered.length}</span>
    </div>
    <div class="question">${escapeHtml(item.question)}</div>

    <h3>${isGrounded ? 'Gold chunk' : `Retrieved chunks (${chunks.length})`}</h3>
    ${chunks.slice(0, isGrounded ? 1 : 5).map((c, i) => chunkBlock(c, isGrounded)).join('')}
    ${chunks.length > 5 && !isGrounded ? `<div class="chunk-meta">... and ${chunks.length - 5} more (suppressed)</div>` : ''}

    <h3>${isGrounded ? 'Answer summary (Sonnet from gold chunk)' : 'Draft answer'}</h3>
    <div class="draft ${draftIsNoSource ? 'no-source' : ''}">${escapeHtml(item.draft_answer || '(no draft)')}</div>
    ${item.draft_citations && item.draft_citations.length ? `<div class="chunk-meta" style="margin-top:6px">cited URLs: ${item.draft_citations.map(u => `<a href="${escapeHtml(u)}" target="_blank" rel="noopener">${escapeHtml(u)}</a>`).join(', ')}</div>` : ''}

    <div class="editor" id="editor">
      <h3>Edit gold answer</h3>
      <textarea id="edit-text">${escapeHtml(review.gold_answer || draftPrefill)}</textarea>
      <input type="text" id="edit-urls" placeholder="gold source URLs, comma-separated"
             value="${escapeHtml((review.gold_source_urls || item.draft_citations || []).join(', '))}">
      <input type="text" id="edit-notes" placeholder="notes (optional)"
             value="${escapeHtml(review.notes || '')}">
      <div style="margin-top:8px; display:flex; gap:8px;">
        <button class="approve" id="save-edit">save edit</button>
        <button id="cancel-edit">cancel <span class="kbd-hint">[Esc]</span></button>
      </div>
    </div>

    <div class="actions">
      <button class="approve" id="approve-btn">✓ Approve <span class="kbd-hint">[a]</span></button>
      <button class="edit" id="edit-btn">✎ Edit <span class="kbd-hint">[e]</span></button>
      <button class="drop" id="drop-btn">✗ Drop <span class="kbd-hint">[d]</span></button>
      <button class="skip" id="skip-btn">↷ Skip <span class="kbd-hint">[s]</span></button>
      <span class="spacer"></span>
      <button id="prev2">← prev</button>
      <button id="next2">next →</button>
    </div>
  `;

  document.getElementById('approve-btn').onclick = () => doApprove();
  document.getElementById('edit-btn').onclick = () => toggleEditor(true);
  document.getElementById('drop-btn').onclick = () => doDrop();
  document.getElementById('skip-btn').onclick = () => navigate(+1);
  document.getElementById('prev2').onclick = () => navigate(-1);
  document.getElementById('next2').onclick = () => navigate(+1);
  document.getElementById('save-edit').onclick = () => doSaveEdit();
  document.getElementById('cancel-edit').onclick = () => toggleEditor(false);
}

function toggleEditor(on) {
  state.editing = on;
  document.getElementById('editor').classList.toggle('active', on);
  if (on) document.getElementById('edit-text').focus();
}

async function postReview(payload) {
  await api('/api/review', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(payload),
  });
  // Update local item summary
  const summary = state.filtered[state.cursor];
  summary.verdict = payload.verdict;
  await renderStats();
}

async function doApprove() {
  const item = state.current;
  await postReview({
    id: item.id,
    verdict: 'approved',
    gold_answer: item.draft_answer,
    gold_source_urls: item.draft_citations || [],
    notes: '',
  });
  navigate(+1);
}

async function doDrop() {
  const reason = prompt('drop reason (optional):', '') || '';
  await postReview({
    id: state.current.id,
    verdict: 'dropped',
    gold_answer: null,
    gold_source_urls: [],
    notes: reason,
  });
  navigate(+1);
}

async function doSaveEdit() {
  const text = document.getElementById('edit-text').value;
  const urls = document.getElementById('edit-urls').value
    .split(',').map(s => s.trim()).filter(Boolean);
  const notes = document.getElementById('edit-notes').value;
  await postReview({
    id: state.current.id,
    verdict: 'edited',
    gold_answer: text,
    gold_source_urls: urls,
    notes: notes,
  });
  toggleEditor(false);
  navigate(+1);
}

function navigate(delta) {
  if (state.editing) return;
  const next = state.cursor + delta;
  if (next < 0 || next >= state.filtered.length) return;
  state.cursor = next;
  renderCurrent();
}

document.addEventListener('keydown', (e) => {
  if (state.editing && e.key === 'Escape') {
    toggleEditor(false);
    return;
  }
  if (state.editing) return;
  if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA') return;
  switch (e.key.toLowerCase()) {
    case 'a': doApprove(); break;
    case 'e': toggleEditor(true); break;
    case 'd': doDrop(); break;
    case 's': navigate(+1); break;
    case 'n': case 'j': navigate(+1); break;
    case 'p': case 'k': navigate(-1); break;
  }
});

document.getElementById('type-filter').addEventListener('change', () => {
  applyFilters();
  state.cursor = 0;
  renderCurrent();
});
document.getElementById('show-filter').addEventListener('change', () => {
  applyFilters();
  state.cursor = 0;
  renderCurrent();
});
document.getElementById('prev').onclick = () => navigate(-1);
document.getElementById('next').onclick = () => navigate(+1);

(async () => {
  try {
    await refreshList();
  } catch (e) {
    document.getElementById('root').innerHTML =
      `<div class="empty" style="color:var(--red)">error: ${e && e.message ? e.message : e}</div>`;
    document.getElementById('stats').textContent = 'error';
    console.error('refreshList failed:', e);
  }
})();
</script>
</body>
</html>
"""


class ReviewHandler(http.server.BaseHTTPRequestHandler):
    store: Store = None  # type: ignore  (set by serve())

    def log_message(self, fmt, *args):
        # quieter than default
        logging.debug("%s - %s", self.address_string(), fmt % args)

    def _send_json(self, code: int, payload) -> None:
        body = json.dumps(payload, ensure_ascii=False).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json; charset=utf-8")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _send_html(self, body: str) -> None:
        data = body.encode("utf-8")
        self.send_response(200)
        self.send_header("Content-Type", "text/html; charset=utf-8")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self) -> None:
        path = urlparse(self.path).path
        if path == "/" or path == "/index.html":
            self._send_html(HTML_PAGE)
            return
        if path == "/api/items":
            qs = urlparse(self.path).query
            type_filter = None
            for kv in qs.split("&"):
                if kv.startswith("type="):
                    type_filter = kv[5:] or None
            self._send_json(200, self.store.list_summary(type_filter))
            return
        if path.startswith("/api/item/"):
            item_id = path[len("/api/item/"):]
            item = self.store.get(item_id)
            if item is None:
                self._send_json(404, {"error": "not found"})
            else:
                self._send_json(200, item)
            return
        if path == "/api/stats":
            self._send_json(200, self.store.stats())
            return
        self._send_json(404, {"error": f"unknown path: {path}"})

    def do_POST(self) -> None:
        path = urlparse(self.path).path
        if path == "/api/review":
            length = int(self.headers.get("Content-Length", "0"))
            try:
                payload = json.loads(self.rfile.read(length).decode("utf-8"))
            except json.JSONDecodeError as e:
                self._send_json(400, {"error": f"bad JSON: {e}"})
                return
            try:
                rec = self.store.save_review(payload["id"], payload)
                self._send_json(200, {"ok": True, "id": rec["id"], "verdict": rec["review"]["verdict"]})
            except KeyError:
                self._send_json(404, {"error": f"unknown item id: {payload.get('id')}"})
            except Exception as e:
                logging.exception("save_review failed")
                self._send_json(500, {"error": str(e)})
            return
        self._send_json(404, {"error": f"unknown path: {path}"})


# ---- Server bootstrap ------------------------------------------------------


class _ReuseServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True


def find_free_port(host: str, start: int) -> int:
    for p in range(start, start + 50):
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
            try:
                s.bind((host, p))
                return p
            except OSError:
                continue
    raise RuntimeError(f"no free port near {start} on {host}")


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--input", default="eval/gov_helpdesk_v1_unified.jsonl")
    ap.add_argument("--output", default="eval/gov_helpdesk_gold_v1.jsonl")
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--verbose", action="store_true")
    args = ap.parse_args()

    logging.basicConfig(
        level=logging.DEBUG if args.verbose else logging.INFO,
        format="%(asctime)s %(levelname)s %(message)s",
    )

    in_path = Path(args.input)
    out_path = Path(args.output)
    if not in_path.exists():
        print(f"input not found: {in_path}", file=sys.stderr)
        return 1

    store = Store(in_path, out_path)
    ReviewHandler.store = store

    port = args.port
    try:
        server = _ReuseServer((args.host, port), ReviewHandler)
    except OSError:
        port = find_free_port(args.host, args.port + 1)
        server = _ReuseServer((args.host, port), ReviewHandler)

    url = f"http://{args.host}:{port}"
    print(f"\n  review UI: {url}\n  input    : {in_path}\n  output   : {out_path}\n", file=sys.stderr)
    print("  Ctrl-C to stop. Output is append-only; safe to restart.\n", file=sys.stderr)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nbye.", file=sys.stderr)
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
