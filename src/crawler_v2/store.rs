//! SQLite persistence for crawler_v2.
//!
//! Schema lives in [`SCHEMA_SQL`] below and is applied on first connect;
//! subsequent schema changes go through [`MIGRATIONS`] keyed by version
//! integer in the `_meta` table.
//!
//! The `Store` is !Send by default because `rusqlite::Connection` is !Sync;
//! that's fine — the daemon gives each worker its own connection.
//!
//! ## Invariants the rest of the daemon relies on
//!
//! 1. **`source_id` is the stable primary key** for a source. Domain changes
//!    (e.g., `opmcm.gov.np/en` → `opmcm.gov.np`) are expressed as `homepage_url`
//!    updates on the same `source_id`, never as a new row.
//! 2. **`first_seen` is sacred.** Once a row exists, `upsert_source_from_registry`
//!    never overwrites `first_seen` — origin time is an audit fact, not a
//!    registry field. Only the initial INSERT reads `first_seen` from JSONL.
//! 3. **`documents` supports at most one live version per (source_id, url).**
//!    Enforced by the partial UNIQUE index `ux_documents_source_url_live`.
//!    Supersede writes a new row and flips the old row's `superseded_by`.
//! 4. **Supersede transactions defer FK+UNIQUE checks to commit.** Required
//!    because both the `superseded_by` FK and the partial UNIQUE index flip
//!    state mid-transaction. See `PRAGMA defer_foreign_keys` in `upsert_document`.
//! 5. **`superseded_by` forms a DAG.** Each revision points at its successor;
//!    there is at most one successor per predecessor (no merges). No cycles.
//! 6. **Enum columns store the lowercase snake-case form** of the Rust variant
//!    (`status`, `doc_type`). If you add a variant, update both
//!    [`SourceStatus::from_str`]/[`as_str`] and [`DocType::from_str`]/[`as_str`].
//! 7. **Timestamps are RFC 3339 strings in UTC.** SQLite has no datetime type;
//!    we serialize to text and parse on read. Non-parseable values in columns
//!    that should be dates surface as [`StoreError::BadEnum`] on read.

// Note: DocType isn't imported by name here even though it's conceptually
// used — we only ever invoke methods on values of type DocType via field
// access (ev.doc_type, doc.doc_type), so the type name never appears at the
// call site. Importing it would produce an unused-import warning.
use super::types::{
    Document, FetchEvent, PersistedHealth, PollCycle, RegistryRow, RepairItem, RepairStatus,
    Source, SourceStatus, Tier,
};
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid enum value in db: {0}")]
    BadEnum(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Full initial schema. Idempotent via `IF NOT EXISTS`.
///
/// Keep in sync with CRAWLER.md §Data model.
const SCHEMA_SQL: &str = r#"
CREATE TABLE IF NOT EXISTS _meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sources (
    source_id            TEXT PRIMARY KEY,
    domain               TEXT NOT NULL,
    homepage_url         TEXT NOT NULL,
    name_en              TEXT,
    name_np              TEXT,
    office_type          TEXT,
    province             TEXT,
    tier                 INTEGER NOT NULL,
    poll_interval_hours  INTEGER NOT NULL,
    status               TEXT NOT NULL DEFAULT 'active',
    first_seen           TEXT NOT NULL,
    last_polled_at       TEXT,
    last_changed_at      TEXT,
    last_failure_at      TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0,
    next_poll_at         TEXT,
    notes                TEXT
);
CREATE INDEX IF NOT EXISTS ix_sources_next_poll   ON sources(next_poll_at);
CREATE INDEX IF NOT EXISTS ix_sources_tier_status ON sources(tier, status);

CREATE TABLE IF NOT EXISTS documents (
    doc_id               TEXT PRIMARY KEY,
    source_id            TEXT NOT NULL REFERENCES sources(source_id),
    url                  TEXT NOT NULL,
    content_hash         TEXT NOT NULL,
    fetched_at           TEXT NOT NULL,
    superseded_by        TEXT REFERENCES documents(doc_id),
    removed_at           TEXT,
    doc_type             TEXT NOT NULL,
    status_code          INTEGER NOT NULL,
    title                TEXT,
    language             TEXT,
    date_published       TEXT,
    raw_blob_path        TEXT NOT NULL,
    extracted_text_path  TEXT,
    text_chars           INTEGER NOT NULL DEFAULT 0,
    size_bytes           INTEGER NOT NULL,
    depth                INTEGER NOT NULL,
    priority_at_fetch    INTEGER
);
CREATE UNIQUE INDEX IF NOT EXISTS ux_documents_source_url_live
    ON documents(source_id, url) WHERE superseded_by IS NULL AND removed_at IS NULL;
CREATE INDEX IF NOT EXISTS ix_documents_source_hash ON documents(source_id, content_hash);

CREATE TABLE IF NOT EXISTS fetch_events (
    event_id   INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id  TEXT NOT NULL,
    url        TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    status     INTEGER NOT NULL,
    elapsed_ms INTEGER,
    error      TEXT,
    doc_type   TEXT,
    bytes      INTEGER
);
CREATE INDEX IF NOT EXISTS ix_fetch_events_source_time ON fetch_events(source_id, fetched_at);

CREATE TABLE IF NOT EXISTS chunks (
    chunk_id     TEXT PRIMARY KEY,
    doc_id       TEXT NOT NULL REFERENCES documents(doc_id),
    chunk_index  INTEGER NOT NULL,
    text         TEXT NOT NULL,
    char_start   INTEGER NOT NULL,
    char_end     INTEGER NOT NULL,
    token_count  INTEGER,
    created_at   TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_chunks_doc ON chunks(doc_id);

CREATE TABLE IF NOT EXISTS source_health (
    source_id                TEXT PRIMARY KEY REFERENCES sources(source_id),
    window_start             TEXT NOT NULL,
    window_end               TEXT NOT NULL,
    fetches                  INTEGER NOT NULL,
    successes                INTEGER NOT NULL,
    empty_extractions        INTEGER NOT NULL,
    avg_text_chars           REAL NOT NULL,
    content_hash_change_rate REAL NOT NULL,
    is_structural_failure    INTEGER NOT NULL DEFAULT 0,
    failure_reason           TEXT
);

CREATE TABLE IF NOT EXISTS poll_cycles (
    cycle_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id         TEXT NOT NULL REFERENCES sources(source_id),
    started_at        TEXT NOT NULL,
    finished_at       TEXT NOT NULL,
    stop_reason       TEXT NOT NULL,
    elapsed_sec       INTEGER NOT NULL,
    html_fetched      INTEGER NOT NULL,
    binaries_fetched  INTEGER NOT NULL,
    other_fetched     INTEGER NOT NULL,
    errors            INTEGER NOT NULL,
    docs_inserted     INTEGER NOT NULL,
    docs_superseded   INTEGER NOT NULL,
    docs_unchanged    INTEGER NOT NULL,
    shell_flagged     INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS ix_poll_cycles_source_time
    ON poll_cycles(source_id, started_at);

CREATE TABLE IF NOT EXISTS repair_queue (
    queue_id          INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id         TEXT NOT NULL REFERENCES sources(source_id),
    queued_at         TEXT NOT NULL,
    status            TEXT NOT NULL DEFAULT 'pending',
    dispatched_at     TEXT,
    completed_at      TEXT,
    failure_evidence  TEXT NOT NULL,
    sample_html_path  TEXT,
    proposed_recipe   TEXT,
    dry_run_result    TEXT,
    apply_outcome     TEXT,
    error_log         TEXT
);
CREATE INDEX IF NOT EXISTS ix_repair_queue_status ON repair_queue(status);
CREATE INDEX IF NOT EXISTS ix_repair_queue_source ON repair_queue(source_id);
"#;

/// Numbered schema migrations beyond the initial [`SCHEMA_SQL`]. Each runs
/// exactly once in version order. Add new ones at the bottom, never rewrite.
///
/// Idempotency note: every migration uses `IF NOT EXISTS` / nullable
/// `ALTER TABLE` so it's safe to re-apply on a fresh DB where SCHEMA_SQL
/// has already created the same artefacts. The version-gate in
/// `apply_migrations` makes that even harder to hit, but the belt-and-braces
/// matters when developers run schema-altering integration tests.
const MIGRATIONS: &[(u32, &str)] = &[
    // v1: add language column to chunks (for an existing DB whose chunks
    // table pre-dates the classifier).
    (
        1,
        "ALTER TABLE chunks ADD COLUMN language TEXT; \
         CREATE INDEX IF NOT EXISTS ix_chunks_lang ON chunks(language);",
    ),
    // v2: poll_cycles — per-cycle WorkerReport persistence so health
    // evaluation can read rolling windows of outcomes (Phase 6).
    (
        2,
        "CREATE TABLE IF NOT EXISTS poll_cycles (
            cycle_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id         TEXT NOT NULL REFERENCES sources(source_id),
            started_at        TEXT NOT NULL,
            finished_at       TEXT NOT NULL,
            stop_reason       TEXT NOT NULL,
            elapsed_sec       INTEGER NOT NULL,
            html_fetched      INTEGER NOT NULL,
            binaries_fetched  INTEGER NOT NULL,
            other_fetched     INTEGER NOT NULL,
            errors            INTEGER NOT NULL,
            docs_inserted     INTEGER NOT NULL,
            docs_superseded   INTEGER NOT NULL,
            docs_unchanged    INTEGER NOT NULL,
            shell_flagged     INTEGER NOT NULL DEFAULT 0
         );
         CREATE INDEX IF NOT EXISTS ix_poll_cycles_source_time
             ON poll_cycles(source_id, started_at);",
    ),
    // v3: repair_queue — agent-dispatcher work list. A row lands here when
    // the health evaluator transitions a source to StructurallyFailed; the
    // dispatcher drains it via a configured agent runtime.
    (
        3,
        "CREATE TABLE IF NOT EXISTS repair_queue (
            queue_id          INTEGER PRIMARY KEY AUTOINCREMENT,
            source_id         TEXT NOT NULL REFERENCES sources(source_id),
            queued_at         TEXT NOT NULL,
            status            TEXT NOT NULL DEFAULT 'pending',
            dispatched_at     TEXT,
            completed_at      TEXT,
            failure_evidence  TEXT NOT NULL,
            sample_html_path  TEXT,
            proposed_recipe   TEXT,
            dry_run_result    TEXT,
            apply_outcome     TEXT,
            error_log         TEXT
         );
         CREATE INDEX IF NOT EXISTS ix_repair_queue_status ON repair_queue(status);
         CREATE INDEX IF NOT EXISTS ix_repair_queue_source ON repair_queue(source_id);",
    ),
];

const CURRENT_VERSION: u32 = 3;

pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (or create) the SQLite database at `path`. Runs schema + pending
    /// migrations. Enables WAL so admin CLI readers don't block the writer.
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self, StoreError> {
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let conn = Connection::open(path)?;
        let s = Store { conn };
        s.init_schema()?;
        Ok(s)
    }

    /// In-memory store, useful for tests.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()?;
        let s = Store { conn };
        s.init_schema()?;
        Ok(s)
    }

    fn init_schema(&self) -> Result<(), StoreError> {
        self.conn.pragma_update(None, "journal_mode", "WAL")?;
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        self.conn.execute_batch(SCHEMA_SQL)?;
        self.apply_migrations()?;
        Ok(())
    }

    fn apply_migrations(&self) -> Result<(), StoreError> {
        let applied: u32 = self
            .conn
            .query_row(
                "SELECT value FROM _meta WHERE key = 'schema_version'",
                [],
                |r| r.get::<_, String>(0),
            )
            .optional()?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        for (version, sql) in MIGRATIONS {
            if *version > applied {
                self.conn.execute_batch(sql)?;
            }
        }

        let target = CURRENT_VERSION.max(MIGRATIONS.last().map(|(v, _)| *v).unwrap_or(0));
        self.conn.execute(
            "INSERT INTO _meta(key, value) VALUES('schema_version', ?1) \
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![target.to_string()],
        )?;
        Ok(())
    }

    /// Insert a new source or overlay an existing one with registry fields,
    /// preserving lifecycle state (last_polled_at, consecutive_failures, etc.).
    ///
    /// Returns true if the row was inserted, false if it was updated.
    pub fn upsert_source_from_registry(
        &self,
        row: &RegistryRow,
        now: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let existing: Option<i64> = self
            .conn
            .query_row(
                "SELECT 1 FROM sources WHERE source_id = ?1",
                params![row.source_id],
                |r| r.get(0),
            )
            .optional()?;

        let tier = Tier(row.tier);
        let poll_hours = row
            .poll_interval_hours
            .unwrap_or_else(|| tier.default_poll_hours());
        let status = row
            .status
            .as_deref()
            .and_then(SourceStatus::from_str)
            .unwrap_or(SourceStatus::Active);

        if existing.is_none() {
            // first_seen is authoritative once a row exists; this block only
            // runs on INSERT. Preserves the JSONL value if present and parseable,
            // otherwise stamps `now`. Malformed timestamp falls through silently
            // rather than rejecting the registry row.
            let first_seen = row
                .first_seen
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|d| d.with_timezone(&Utc))
                .unwrap_or(now);
            self.conn.execute(
                "INSERT INTO sources (
                    source_id, domain, homepage_url, name_en, name_np,
                    office_type, province, tier, poll_interval_hours,
                    status, first_seen, next_poll_at
                ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    row.source_id,
                    row.domain,
                    row.homepage_url,
                    row.name_en,
                    row.name_np,
                    row.office_type,
                    row.province,
                    tier.0,
                    poll_hours,
                    status.as_str(),
                    first_seen.to_rfc3339(),
                    // New sources are due immediately so the daemon picks
                    // them up on the next tick rather than waiting a full
                    // cadence before first fetch.
                    now.to_rfc3339(),
                ],
            )?;
            Ok(true)
        } else {
            // Overlay registry-owned columns only; do NOT clobber lifecycle.
            self.conn.execute(
                "UPDATE sources SET
                    domain             = ?2,
                    homepage_url       = ?3,
                    name_en            = ?4,
                    name_np            = ?5,
                    office_type        = ?6,
                    province           = ?7,
                    tier               = ?8,
                    poll_interval_hours = ?9
                 WHERE source_id = ?1",
                params![
                    row.source_id,
                    row.domain,
                    row.homepage_url,
                    row.name_en,
                    row.name_np,
                    row.office_type,
                    row.province,
                    tier.0,
                    poll_hours,
                ],
            )?;
            Ok(false)
        }
    }

    pub fn source_count(&self) -> Result<u64, StoreError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM sources", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    pub fn source_count_by_tier(&self) -> Result<Vec<(u8, u64)>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT tier, COUNT(*) FROM sources GROUP BY tier ORDER BY tier")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)? as u8, r.get::<_, i64>(1)? as u64)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_source(&self, source_id: &str) -> Result<Option<Source>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT source_id, domain, homepage_url, name_en, name_np,
                        office_type, province, tier, poll_interval_hours,
                        status, first_seen, last_polled_at, last_changed_at,
                        last_failure_at, consecutive_failures, next_poll_at,
                        notes
                 FROM sources WHERE source_id = ?1",
                params![source_id],
                row_to_source,
            )
            .optional()?;
        Ok(row)
    }

    /// Sources whose scheduler timestamp has elapsed. Returns in tier order
    /// (higher-authority sources polled first when work is queued).
    pub fn list_sources_due(&self, now: DateTime<Utc>) -> Result<Vec<Source>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT source_id, domain, homepage_url, name_en, name_np,
                    office_type, province, tier, poll_interval_hours,
                    status, first_seen, last_polled_at, last_changed_at,
                    last_failure_at, consecutive_failures, next_poll_at,
                    notes
             FROM sources
             WHERE status = 'active'
               AND (next_poll_at IS NULL OR next_poll_at <= ?1)
             ORDER BY tier, source_id",
        )?;
        let iter = stmt.query_map(params![now.to_rfc3339()], row_to_source)?;
        let mut out = Vec::new();
        for r in iter {
            out.push(r?);
        }
        Ok(out)
    }

    pub fn list_sources(&self) -> Result<Vec<Source>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT source_id, domain, homepage_url, name_en, name_np,
                    office_type, province, tier, poll_interval_hours,
                    status, first_seen, last_polled_at, last_changed_at,
                    last_failure_at, consecutive_failures, next_poll_at,
                    notes
             FROM sources ORDER BY tier, source_id",
        )?;
        let iter = stmt.query_map([], row_to_source)?;
        let mut out = Vec::new();
        for r in iter {
            out.push(r?);
        }
        Ok(out)
    }

    /// Insert a FetchEvent. Returns the auto-assigned event_id.
    pub fn record_fetch_event(&self, ev: &FetchEvent) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO fetch_events
                (source_id, url, fetched_at, status, elapsed_ms, error, doc_type, bytes)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                ev.source_id,
                ev.url,
                ev.fetched_at.to_rfc3339(),
                ev.status,
                ev.elapsed_ms,
                ev.error,
                ev.doc_type.map(|d| d.as_str()),
                ev.bytes.map(|b| b as i64),
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Insert or skip a document, handling diff detection:
    ///   - same url + same hash as existing live doc -> touch fetched_at, no new row
    ///   - same url + different hash                 -> new row, old gets superseded
    ///   - new url                                    -> straight insert
    pub fn upsert_document(&mut self, doc: &Document) -> Result<DocumentOutcome, StoreError> {
        let tx = self.conn.transaction()?;
        // Both the FK constraint (superseded_by -> documents.doc_id) and the
        // partial UNIQUE index on (source_id, url) WHERE superseded_by IS NULL
        // would fail mid-transaction on supersede. Deferring lets us update
        // the old row and insert the new row in either order, and sqlite
        // checks constraints at commit.
        tx.pragma_update(None, "defer_foreign_keys", "ON")?;

        let existing: Option<(String, String)> = tx
            .query_row(
                "SELECT doc_id, content_hash FROM documents
                 WHERE source_id = ?1 AND url = ?2
                   AND superseded_by IS NULL AND removed_at IS NULL",
                params![doc.source_id, doc.url],
                |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
            )
            .optional()?;

        let outcome = match existing {
            Some((prev_id, prev_hash)) if prev_hash == doc.content_hash => {
                tx.execute(
                    "UPDATE documents SET fetched_at = ?1 WHERE doc_id = ?2",
                    params![doc.fetched_at.to_rfc3339(), prev_id],
                )?;
                DocumentOutcome::Unchanged
            }
            Some((prev_id, _)) => {
                // Mark old row superseded first so the partial UNIQUE index
                // on live rows doesn't fire when we insert the new one.
                tx.execute(
                    "UPDATE documents SET superseded_by = ?1 WHERE doc_id = ?2",
                    params![doc.doc_id, prev_id],
                )?;
                insert_document(&tx, doc)?;
                DocumentOutcome::Superseded { prev_id }
            }
            None => {
                insert_document(&tx, doc)?;
                DocumentOutcome::Inserted
            }
        };

        tx.commit()?;
        Ok(outcome)
    }

    /// Expose the underlying connection for callers that need custom queries
    /// (e.g., the admin `status` CLI). Keep the escape hatch narrow.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    // ---------- chunks -----------------------------------------------------

    /// Batch-insert chunks for one document. Inside a single transaction so
    /// a failure part-way leaves the doc's chunk set in its pre-insert state.
    /// Duplicates (same chunk_id) are INSERT OR IGNORE — re-running the
    /// chunker with unchanged content is a no-op.
    ///
    /// `languages` must be parallel to `chunks` (same length). Pass an empty
    /// slice to skip language persistence.
    pub fn insert_chunks(
        &mut self,
        doc_id: &str,
        chunks: &[crate::crawler_v2::chunk::Chunk],
        languages: &[&str],
        now: DateTime<Utc>,
    ) -> Result<usize, StoreError> {
        if !languages.is_empty() && languages.len() != chunks.len() {
            return Err(StoreError::BadEnum(format!(
                "languages len {} != chunks len {}",
                languages.len(),
                chunks.len()
            )));
        }
        let tx = self.conn.transaction()?;
        let now_str = now.to_rfc3339();
        let mut inserted = 0usize;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO chunks
                    (chunk_id, doc_id, chunk_index, text, char_start, char_end,
                     token_count, language, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,NULL,?7,?8)",
            )?;
            for (i, c) in chunks.iter().enumerate() {
                let lang: Option<&str> = languages.get(i).copied();
                let n = stmt.execute(params![
                    c.chunk_id,
                    doc_id,
                    c.chunk_index,
                    c.text,
                    c.char_start,
                    c.char_end,
                    lang,
                    now_str,
                ])?;
                inserted += n;
            }
        }
        tx.commit()?;
        Ok(inserted)
    }

    /// How many chunks exist for this doc already? Used to decide whether
    /// to re-chunk.
    pub fn chunk_count_for_doc(&self, doc_id: &str) -> Result<u64, StoreError> {
        let n: i64 = self
            .conn
            .query_row(
                "SELECT COUNT(*) FROM chunks WHERE doc_id = ?1",
                params![doc_id],
                |r| r.get(0),
            )?;
        Ok(n as u64)
    }

    pub fn chunk_count_total(&self) -> Result<u64, StoreError> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        Ok(n as u64)
    }

    // ---------- document iteration for indexing ----------------------------

    /// List docs that don't yet have any chunks. Used by `crawl index-chunks`
    /// as the work queue. `source_id` filters to one source if `Some`.
    pub fn list_unchunked_documents(
        &self,
        limit: Option<u64>,
        source_id: Option<&str>,
    ) -> Result<Vec<Document>, StoreError> {
        let base = "SELECT d.doc_id, d.source_id, d.url, d.content_hash,
                           d.fetched_at, d.superseded_by, d.removed_at, d.doc_type,
                           d.status_code, d.title, d.language, d.date_published,
                           d.raw_blob_path, d.extracted_text_path, d.text_chars,
                           d.size_bytes, d.depth, d.priority_at_fetch
                    FROM documents d
                    WHERE d.superseded_by IS NULL AND d.removed_at IS NULL
                      AND NOT EXISTS (SELECT 1 FROM chunks c WHERE c.doc_id = d.doc_id)";
        let where_source = if source_id.is_some() {
            " AND d.source_id = ?1"
        } else {
            ""
        };
        let order = " ORDER BY d.source_id, d.fetched_at";
        let mut sql = format!("{base}{where_source}{order}");
        if let Some(n) = limit {
            sql.push_str(&format!(" LIMIT {n}"));
        }
        let mut stmt = self.conn.prepare(&sql)?;
        let iter = if let Some(sid) = source_id {
            stmt.query_map(params![sid], row_to_document)?.collect::<Vec<_>>()
        } else {
            stmt.query_map([], row_to_document)?.collect::<Vec<_>>()
        };
        let mut out = Vec::with_capacity(iter.len());
        for r in iter {
            out.push(r?);
        }
        Ok(out)
    }

    // ---------- poll_cycles -----------------------------------------------

    /// Persist a single poll cycle's outcome. Returns the assigned `cycle_id`.
    /// Health evaluation reads rolling windows of these to spot structural
    /// failures (HTTP 200 + 0 docs over ≥5 cycles).
    pub fn insert_poll_cycle(&self, cycle: &PollCycle) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO poll_cycles
                (source_id, started_at, finished_at, stop_reason, elapsed_sec,
                 html_fetched, binaries_fetched, other_fetched, errors,
                 docs_inserted, docs_superseded, docs_unchanged, shell_flagged)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)",
            params![
                cycle.source_id,
                cycle.started_at.to_rfc3339(),
                cycle.finished_at.to_rfc3339(),
                cycle.stop_reason,
                cycle.elapsed_sec as i64,
                cycle.html_fetched as i64,
                cycle.binaries_fetched as i64,
                cycle.other_fetched as i64,
                cycle.errors as i64,
                cycle.docs_inserted as i64,
                cycle.docs_superseded as i64,
                cycle.docs_unchanged as i64,
                if cycle.shell_flagged { 1i64 } else { 0i64 },
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Return cycles for a source whose `started_at` is in [`since`, now],
    /// newest first. `limit = 0` means no limit.
    pub fn list_poll_cycles_for_source(
        &self,
        source_id: &str,
        since: DateTime<Utc>,
        limit: usize,
    ) -> Result<Vec<PollCycle>, StoreError> {
        let sql = "SELECT cycle_id, source_id, started_at, finished_at,
                          stop_reason, elapsed_sec, html_fetched, binaries_fetched,
                          other_fetched, errors, docs_inserted, docs_superseded,
                          docs_unchanged, shell_flagged
                   FROM poll_cycles
                   WHERE source_id = ?1 AND started_at >= ?2
                   ORDER BY started_at DESC";
        let sql_with_limit = if limit > 0 {
            format!("{sql} LIMIT {limit}")
        } else {
            sql.to_string()
        };
        let mut stmt = self.conn.prepare(&sql_with_limit)?;
        let rows = stmt
            .query_map(params![source_id, since.to_rfc3339()], row_to_poll_cycle)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn poll_cycle_count(&self, source_id: &str) -> Result<u64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM poll_cycles WHERE source_id = ?1",
            params![source_id],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    // ---------- repair_queue ----------------------------------------------

    /// Enqueue a new repair item (status=Pending). Returns the `queue_id`.
    /// Idempotency is the caller's responsibility: the health evaluator
    /// should refuse to enqueue a second pending row for the same source.
    pub fn enqueue_repair(&self, item: &RepairItem) -> Result<i64, StoreError> {
        self.conn.execute(
            "INSERT INTO repair_queue
                (source_id, queued_at, status, dispatched_at, completed_at,
                 failure_evidence, sample_html_path, proposed_recipe,
                 dry_run_result, apply_outcome, error_log)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                item.source_id,
                item.queued_at.to_rfc3339(),
                item.status.as_str(),
                item.dispatched_at.map(|d| d.to_rfc3339()),
                item.completed_at.map(|d| d.to_rfc3339()),
                item.failure_evidence,
                item.sample_html_path,
                item.proposed_recipe,
                item.dry_run_result,
                item.apply_outcome,
                item.error_log,
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Pending repairs in FIFO order.
    pub fn list_pending_repairs(&self) -> Result<Vec<RepairItem>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT queue_id, source_id, queued_at, status, dispatched_at,
                    completed_at, failure_evidence, sample_html_path,
                    proposed_recipe, dry_run_result, apply_outcome, error_log
             FROM repair_queue
             WHERE status = 'pending'
             ORDER BY queued_at",
        )?;
        let rows = stmt
            .query_map([], row_to_repair_item)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    pub fn get_repair(&self, queue_id: i64) -> Result<Option<RepairItem>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT queue_id, source_id, queued_at, status, dispatched_at,
                        completed_at, failure_evidence, sample_html_path,
                        proposed_recipe, dry_run_result, apply_outcome, error_log
                 FROM repair_queue WHERE queue_id = ?1",
                params![queue_id],
                row_to_repair_item,
            )
            .optional()?;
        Ok(row)
    }

    /// Whole-row update. Caller passes a [`RepairItem`] with the desired final
    /// state; `queue_id` must be `Some(...)`. Use this for status transitions
    /// and proposal/dry-run/outcome attachments.
    pub fn update_repair(&self, item: &RepairItem) -> Result<(), StoreError> {
        let qid = item.queue_id.ok_or_else(|| {
            StoreError::BadEnum("update_repair: queue_id is None".to_string())
        })?;
        self.conn.execute(
            "UPDATE repair_queue SET
                status            = ?1,
                dispatched_at     = ?2,
                completed_at      = ?3,
                failure_evidence  = ?4,
                sample_html_path  = ?5,
                proposed_recipe   = ?6,
                dry_run_result    = ?7,
                apply_outcome     = ?8,
                error_log         = ?9
             WHERE queue_id = ?10",
            params![
                item.status.as_str(),
                item.dispatched_at.map(|d| d.to_rfc3339()),
                item.completed_at.map(|d| d.to_rfc3339()),
                item.failure_evidence,
                item.sample_html_path,
                item.proposed_recipe,
                item.dry_run_result,
                item.apply_outcome,
                item.error_log,
                qid,
            ],
        )?;
        Ok(())
    }

    pub fn pending_repair_count(&self) -> Result<u64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM repair_queue WHERE status = 'pending'",
            [],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// True if there's already a Pending row for this source. Health evaluator
    /// checks this before enqueuing to avoid duplicate work.
    pub fn has_pending_repair(&self, source_id: &str) -> Result<bool, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM repair_queue
             WHERE source_id = ?1 AND status = 'pending'",
            params![source_id],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    // ---------- health evaluator inputs / outputs --------------------------

    /// Sum of `docs_inserted` over all `poll_cycles` rows for this source.
    /// The "previously-productive" precondition for the StructurallyFailed
    /// verdict reads this — a source that has never inserted real content
    /// can't be flagged as having "regressed."
    pub fn total_docs_inserted_for_source(&self, source_id: &str) -> Result<u64, StoreError> {
        let n: Option<i64> = self
            .conn
            .query_row(
                "SELECT SUM(docs_inserted) FROM poll_cycles WHERE source_id = ?1",
                params![source_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        Ok(n.unwrap_or(0).max(0) as u64)
    }

    /// Upsert the latest health snapshot for a source. The `source_health`
    /// table holds one row per source — history is reconstructable from
    /// `poll_cycles`, so we don't keep snapshots across evaluations.
    pub fn upsert_source_health(
        &self,
        metrics: &crate::crawler_v2::health::HealthMetrics,
    ) -> Result<(), StoreError> {
        let is_struct_fail = if metrics.verdict.is_structural_failure() {
            1i64
        } else {
            0i64
        };
        let reason = metrics.verdict.reason();
        // `avg_text_chars` lives in the schema for forward-compat with a
        // documents-table join in a later phase; for now we write 0 and
        // populate when we wire that join.
        self.conn.execute(
            "INSERT INTO source_health
                (source_id, window_start, window_end, fetches, successes,
                 empty_extractions, avg_text_chars, content_hash_change_rate,
                 is_structural_failure, failure_reason)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)
             ON CONFLICT(source_id) DO UPDATE SET
                window_start             = excluded.window_start,
                window_end               = excluded.window_end,
                fetches                  = excluded.fetches,
                successes                = excluded.successes,
                empty_extractions        = excluded.empty_extractions,
                avg_text_chars           = excluded.avg_text_chars,
                content_hash_change_rate = excluded.content_hash_change_rate,
                is_structural_failure    = excluded.is_structural_failure,
                failure_reason           = excluded.failure_reason",
            params![
                metrics.source_id,
                metrics.window_start.to_rfc3339(),
                metrics.window_end.to_rfc3339(),
                metrics.n_cycles as i64,
                metrics.successes as i64,
                metrics.empty_extractions as i64,
                0.0_f64, // avg_text_chars: deferred (needs documents JOIN)
                metrics.content_hash_change_rate,
                is_struct_fail,
                reason,
            ],
        )?;
        Ok(())
    }

    /// Most-recent live HTML document for a source. Used by
    /// `repair::react_to_verdict` to attach a sample-html-path to a queued
    /// repair entry — the agent dispatcher renders this with Playwright as
    /// part of its investigation.
    pub fn latest_html_doc_for_source(
        &self,
        source_id: &str,
    ) -> Result<Option<Document>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT doc_id, source_id, url, content_hash, fetched_at,
                        superseded_by, removed_at, doc_type, status_code, title,
                        language, date_published, raw_blob_path, extracted_text_path,
                        text_chars, size_bytes, depth, priority_at_fetch
                 FROM documents
                 WHERE source_id = ?1
                   AND superseded_by IS NULL AND removed_at IS NULL
                   AND doc_type = 'html'
                 ORDER BY fetched_at DESC
                 LIMIT 1",
                params![source_id],
                row_to_document,
            )
            .optional()?;
        Ok(row)
    }

    /// Read back the persisted snapshot, if any. Used by `crawl health` CLI
    /// and by integration tests; the daemon itself recomputes per tick.
    pub fn get_source_health(
        &self,
        source_id: &str,
    ) -> Result<Option<PersistedHealth>, StoreError> {
        let row = self
            .conn
            .query_row(
                "SELECT source_id, window_start, window_end, fetches, successes,
                        empty_extractions, avg_text_chars, content_hash_change_rate,
                        is_structural_failure, failure_reason
                 FROM source_health WHERE source_id = ?1",
                params![source_id],
                |r| {
                    let window_start_str: String = r.get("window_start")?;
                    let window_end_str: String = r.get("window_end")?;
                    Ok(PersistedHealth {
                        source_id: r.get("source_id")?,
                        window_start: DateTime::parse_from_rfc3339(&window_start_str)
                            .map_err(|_| corrupt(format!("window_start={window_start_str}")))?
                            .with_timezone(&Utc),
                        window_end: DateTime::parse_from_rfc3339(&window_end_str)
                            .map_err(|_| corrupt(format!("window_end={window_end_str}")))?
                            .with_timezone(&Utc),
                        fetches: r.get::<_, i64>("fetches")? as u32,
                        successes: r.get::<_, i64>("successes")? as u32,
                        empty_extractions: r.get::<_, i64>("empty_extractions")? as u32,
                        avg_text_chars: r.get("avg_text_chars")?,
                        content_hash_change_rate: r.get("content_hash_change_rate")?,
                        is_structural_failure: r.get::<_, i64>("is_structural_failure")? != 0,
                        failure_reason: r.get("failure_reason")?,
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    // ---------- source lifecycle updates (used by the worker) --------------

    /// Record a fetch-cycle outcome. Resets `consecutive_failures` on success,
    /// increments on failure. Always updates `last_polled_at`.
    pub fn mark_source_fetch_attempt(
        &self,
        source_id: &str,
        now: DateTime<Utc>,
        success: bool,
    ) -> Result<(), StoreError> {
        if success {
            self.conn.execute(
                "UPDATE sources
                    SET last_polled_at = ?1,
                        consecutive_failures = 0
                  WHERE source_id = ?2",
                params![now.to_rfc3339(), source_id],
            )?;
        } else {
            self.conn.execute(
                "UPDATE sources
                    SET last_polled_at = ?1,
                        last_failure_at = ?1,
                        consecutive_failures = consecutive_failures + 1
                  WHERE source_id = ?2",
                params![now.to_rfc3339(), source_id],
            )?;
        }
        Ok(())
    }

    /// Update `last_changed_at` when a re-poll detected a document change.
    /// No-op if no document actually superseded; the worker decides.
    pub fn mark_source_changed(
        &self,
        source_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sources SET last_changed_at = ?1 WHERE source_id = ?2",
            params![now.to_rfc3339(), source_id],
        )?;
        Ok(())
    }

    /// Transition the source lifecycle. Used when shell-detection flags a
    /// source `js_only`, when a WAF block forces `dormant`, or when the
    /// consecutive-failure ceiling puts a source into `dead`.
    pub fn mark_source_status(
        &self,
        source_id: &str,
        status: crate::crawler_v2::types::SourceStatus,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sources SET status = ?1 WHERE source_id = ?2",
            params![status.as_str(), source_id],
        )?;
        Ok(())
    }

    /// Schedule the next poll for a source. The caller computes the target
    /// time (`last_polled_at + poll_interval_hours`) and writes it here so
    /// the scheduler can pick up ready sources via
    /// `SELECT ... WHERE next_poll_at <= now`.
    pub fn set_next_poll_at(
        &self,
        source_id: &str,
        next: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE sources SET next_poll_at = ?1 WHERE source_id = ?2",
            params![next.to_rfc3339(), source_id],
        )?;
        Ok(())
    }
}

/// Result of [`Store::upsert_document`]. The caller uses this to decide
/// whether to write a new blob file and to update source.last_changed_at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentOutcome {
    /// URL is new for this source; row inserted.
    Inserted,
    /// URL already existed, content hash matches; only fetched_at was bumped.
    Unchanged,
    /// URL already existed with different content; new row inserted and the
    /// previous row now has `superseded_by = new.doc_id`.
    Superseded { prev_id: String },
}

fn insert_document(conn: &Connection, doc: &Document) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO documents (
            doc_id, source_id, url, content_hash, fetched_at,
            superseded_by, removed_at, doc_type, status_code, title,
            language, date_published, raw_blob_path, extracted_text_path,
            text_chars, size_bytes, depth, priority_at_fetch
         ) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        params![
            doc.doc_id,
            doc.source_id,
            doc.url,
            doc.content_hash,
            doc.fetched_at.to_rfc3339(),
            doc.superseded_by,
            doc.removed_at.map(|d| d.to_rfc3339()),
            doc.doc_type.as_str(),
            doc.status_code,
            doc.title,
            doc.language,
            doc.date_published,
            doc.raw_blob_path,
            doc.extracted_text_path,
            doc.text_chars,
            doc.size_bytes as i64,
            doc.depth,
            doc.priority_at_fetch,
        ],
    )?;
    Ok(())
}

/// Map a corrupted DB value to a rusqlite error so query_row/query_map return
/// `rusqlite::Result<Source>` (and StoreError captures it via `From`).
fn corrupt(msg: String) -> rusqlite::Error {
    // Column index 0 is a lie — rusqlite doesn't expose the offending column
    // for named lookups, and the wrapped error message carries the detail.
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(CorruptValue(msg)),
    )
}

#[derive(Debug)]
struct CorruptValue(String);

impl std::fmt::Display for CorruptValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "corrupted db value: {}", self.0)
    }
}

impl std::error::Error for CorruptValue {}

fn row_to_document(r: &rusqlite::Row<'_>) -> rusqlite::Result<Document> {
    use crate::crawler_v2::types::DocType;
    let dt_str: String = r.get("doc_type")?;
    let doc_type = DocType::from_str(&dt_str)
        .ok_or_else(|| corrupt(format!("doc_type={dt_str}")))?;
    let fetched_at_str: String = r.get("fetched_at")?;
    let fetched_at = DateTime::parse_from_rfc3339(&fetched_at_str)
        .map_err(|_| corrupt(format!("fetched_at={fetched_at_str}")))?
        .with_timezone(&Utc);

    Ok(Document {
        doc_id: r.get("doc_id")?,
        source_id: r.get("source_id")?,
        url: r.get("url")?,
        content_hash: r.get("content_hash")?,
        fetched_at,
        superseded_by: r.get("superseded_by")?,
        removed_at: parse_opt_rfc3339(r.get::<_, Option<String>>("removed_at")?),
        doc_type,
        status_code: r.get::<_, i64>("status_code")? as i32,
        title: r.get("title")?,
        language: r.get("language")?,
        date_published: r.get("date_published")?,
        raw_blob_path: r.get("raw_blob_path")?,
        extracted_text_path: r.get("extracted_text_path")?,
        text_chars: r.get::<_, i64>("text_chars")? as u32,
        size_bytes: r.get::<_, i64>("size_bytes")? as u64,
        depth: r.get::<_, i64>("depth")? as u32,
        priority_at_fetch: r.get::<_, Option<i64>>("priority_at_fetch")?.map(|i| i as i32),
    })
}

fn row_to_source(r: &rusqlite::Row<'_>) -> rusqlite::Result<Source> {
    let status_str: String = r.get("status")?;
    let status = SourceStatus::from_str(&status_str)
        .ok_or_else(|| corrupt(format!("status={status_str}")))?;

    let first_seen_str: String = r.get("first_seen")?;
    let first_seen = DateTime::parse_from_rfc3339(&first_seen_str)
        .map_err(|_| corrupt(format!("first_seen={first_seen_str}")))?
        .with_timezone(&Utc);

    Ok(Source {
        source_id: r.get("source_id")?,
        domain: r.get("domain")?,
        homepage_url: r.get("homepage_url")?,
        name_en: r.get("name_en")?,
        name_np: r.get("name_np")?,
        office_type: r.get("office_type")?,
        province: r.get("province")?,
        tier: Tier(r.get::<_, i64>("tier")? as u8),
        poll_interval_hours: r.get::<_, i64>("poll_interval_hours")? as u32,
        status,
        first_seen,
        last_polled_at: parse_opt_rfc3339(r.get::<_, Option<String>>("last_polled_at")?),
        last_changed_at: parse_opt_rfc3339(r.get::<_, Option<String>>("last_changed_at")?),
        last_failure_at: parse_opt_rfc3339(r.get::<_, Option<String>>("last_failure_at")?),
        consecutive_failures: r.get::<_, i64>("consecutive_failures")? as u32,
        next_poll_at: parse_opt_rfc3339(r.get::<_, Option<String>>("next_poll_at")?),
        notes: r.get("notes")?,
    })
}

fn parse_opt_rfc3339(s: Option<String>) -> Option<DateTime<Utc>> {
    s.as_deref()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|d| d.with_timezone(&Utc))
}

fn row_to_poll_cycle(r: &rusqlite::Row<'_>) -> rusqlite::Result<PollCycle> {
    let started_str: String = r.get("started_at")?;
    let finished_str: String = r.get("finished_at")?;
    let started_at = DateTime::parse_from_rfc3339(&started_str)
        .map_err(|_| corrupt(format!("started_at={started_str}")))?
        .with_timezone(&Utc);
    let finished_at = DateTime::parse_from_rfc3339(&finished_str)
        .map_err(|_| corrupt(format!("finished_at={finished_str}")))?
        .with_timezone(&Utc);
    Ok(PollCycle {
        cycle_id: Some(r.get::<_, i64>("cycle_id")?),
        source_id: r.get("source_id")?,
        started_at,
        finished_at,
        stop_reason: r.get("stop_reason")?,
        elapsed_sec: r.get::<_, i64>("elapsed_sec")? as u64,
        html_fetched: r.get::<_, i64>("html_fetched")? as u32,
        binaries_fetched: r.get::<_, i64>("binaries_fetched")? as u32,
        other_fetched: r.get::<_, i64>("other_fetched")? as u32,
        errors: r.get::<_, i64>("errors")? as u32,
        docs_inserted: r.get::<_, i64>("docs_inserted")? as u32,
        docs_superseded: r.get::<_, i64>("docs_superseded")? as u32,
        docs_unchanged: r.get::<_, i64>("docs_unchanged")? as u32,
        shell_flagged: r.get::<_, i64>("shell_flagged")? != 0,
    })
}

fn row_to_repair_item(r: &rusqlite::Row<'_>) -> rusqlite::Result<RepairItem> {
    let queued_str: String = r.get("queued_at")?;
    let queued_at = DateTime::parse_from_rfc3339(&queued_str)
        .map_err(|_| corrupt(format!("queued_at={queued_str}")))?
        .with_timezone(&Utc);
    let status_str: String = r.get("status")?;
    let status = RepairStatus::from_str(&status_str)
        .ok_or_else(|| corrupt(format!("repair status={status_str}")))?;
    Ok(RepairItem {
        queue_id: Some(r.get::<_, i64>("queue_id")?),
        source_id: r.get("source_id")?,
        queued_at,
        status,
        dispatched_at: parse_opt_rfc3339(r.get::<_, Option<String>>("dispatched_at")?),
        completed_at: parse_opt_rfc3339(r.get::<_, Option<String>>("completed_at")?),
        failure_evidence: r.get("failure_evidence")?,
        sample_html_path: r.get("sample_html_path")?,
        proposed_recipe: r.get("proposed_recipe")?,
        dry_run_result: r.get("dry_run_result")?,
        apply_outcome: r.get("apply_outcome")?,
        error_log: r.get("error_log")?,
    })
}
