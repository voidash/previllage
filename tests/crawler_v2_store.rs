//! Integration tests for crawler_v2 store + registry sync.

use chrono::Utc;
use gemma_god::crawler_v2::types::{DocType, Document, FetchEvent, RegistryRow, SourceStatus};
use gemma_god::crawler_v2::{Store, sync_registry};
use gemma_god::crawler_v2::store::DocumentOutcome;
use std::io::Write;
use tempfile::NamedTempFile;

fn sample_row(sid: &str, tier: u8) -> RegistryRow {
    RegistryRow {
        source_id: sid.to_string(),
        domain: format!("{sid}.gov.np"),
        homepage_url: format!("https://{sid}.gov.np/"),
        name_en: Some(format!("Ministry of {sid}")),
        name_np: None,
        office_type: Some("Federal".into()),
        province: None,
        tier,
        poll_interval_hours: None,
        status: None,
        first_seen: None,
    }
}

#[test]
fn schema_is_idempotent() {
    let store = Store::open_in_memory().unwrap();
    // Re-running init shouldn't fail (the store's ctor already runs schema
    // once; prove re-calling CRUD works after).
    let n = store.source_count().unwrap();
    assert_eq!(n, 0);
}

#[test]
fn upsert_source_inserts_then_updates() {
    let store = Store::open_in_memory().unwrap();
    let now = Utc::now();

    let r1 = sample_row("moha", 2);
    assert!(store.upsert_source_from_registry(&r1, now).unwrap());
    assert_eq!(store.source_count().unwrap(), 1);

    // Same source, different tier — should UPDATE, not insert.
    let mut r2 = r1.clone();
    r2.tier = 1;
    assert!(!store.upsert_source_from_registry(&r2, now).unwrap());
    assert_eq!(store.source_count().unwrap(), 1);

    let fetched = store.get_source("moha").unwrap().unwrap();
    assert_eq!(fetched.tier.0, 1);
    // Default poll hours for tier 1 = 6.
    assert_eq!(fetched.poll_interval_hours, 6);
}

#[test]
fn upsert_source_preserves_lifecycle_columns() {
    let store = Store::open_in_memory().unwrap();
    let now = Utc::now();

    let r = sample_row("moha", 2);
    store.upsert_source_from_registry(&r, now).unwrap();

    // Simulate daemon writing lifecycle state.
    store
        .conn()
        .execute(
            "UPDATE sources SET consecutive_failures = 7, last_polled_at = ?1 \
             WHERE source_id = 'moha'",
            rusqlite::params![now.to_rfc3339()],
        )
        .unwrap();

    // Re-sync the row with an edited name.
    let mut r2 = r.clone();
    r2.name_en = Some("Ministry of Home Affairs (renamed)".into());
    store.upsert_source_from_registry(&r2, now).unwrap();

    let after = store.get_source("moha").unwrap().unwrap();
    assert_eq!(
        after.name_en.as_deref(),
        Some("Ministry of Home Affairs (renamed)"),
    );
    assert_eq!(after.consecutive_failures, 7);
    assert!(after.last_polled_at.is_some());
}

#[test]
fn sync_registry_from_jsonl() {
    let store = Store::open_in_memory().unwrap();
    let mut tmp = NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        r#"{{"source_id":"a","domain":"a.gov.np","homepage_url":"https://a.gov.np/","tier_guess":1}}"#
    )
    .unwrap();
    writeln!(
        tmp,
        r#"{{"source_id":"b","domain":"b.gov.np","homepage_url":"https://b.gov.np/","tier_guess":3}}"#
    )
    .unwrap();
    // Blank line + malformed line should be tolerated.
    writeln!(tmp, "").unwrap();
    writeln!(tmp, "not-json").unwrap();
    tmp.flush().unwrap();

    let report = sync_registry(&store, tmp.path()).unwrap();
    assert_eq!(report.inserted, 2);
    assert_eq!(report.skipped_bad_json, 1);
    assert_eq!(store.source_count().unwrap(), 2);

    let a = store.get_source("a").unwrap().unwrap();
    assert_eq!(a.tier.0, 1);
    assert_eq!(a.poll_interval_hours, 6);
}

#[test]
fn upsert_document_insert_then_unchanged_then_superseded() {
    let mut store = Store::open_in_memory().unwrap();
    let now = Utc::now();
    store
        .upsert_source_from_registry(&sample_row("moha", 2), now)
        .unwrap();

    let d1 = Document {
        doc_id: "docA-v1".into(),
        source_id: "moha".into(),
        url: "https://moha.gov.np/notices/1".into(),
        content_hash: "hash_alpha".into(),
        fetched_at: now,
        superseded_by: None,
        removed_at: None,
        doc_type: DocType::Html,
        status_code: 200,
        title: None,
        language: None,
        date_published: None,
        raw_blob_path: "blobs/moha/ha/hash_alpha.html".into(),
        extracted_text_path: None,
        text_chars: 0,
        size_bytes: 1234,
        depth: 0,
        priority_at_fetch: Some(110),
    };
    assert_eq!(store.upsert_document(&d1).unwrap(), DocumentOutcome::Inserted);

    // Same URL, same hash: Unchanged.
    let mut d2 = d1.clone();
    d2.doc_id = "docA-v1-dup".into();
    d2.fetched_at = now;
    assert_eq!(store.upsert_document(&d2).unwrap(), DocumentOutcome::Unchanged);

    // Same URL, different hash: Superseded.
    let mut d3 = d1.clone();
    d3.doc_id = "docA-v2".into();
    d3.content_hash = "hash_beta".into();
    d3.raw_blob_path = "blobs/moha/ha/hash_beta.html".into();
    match store.upsert_document(&d3).unwrap() {
        DocumentOutcome::Superseded { prev_id } => assert_eq!(prev_id, "docA-v1"),
        other => panic!("expected Superseded, got {other:?}"),
    }
}

#[test]
fn malformed_first_seen_falls_back_to_now() {
    let store = Store::open_in_memory().unwrap();
    let now = Utc::now();
    let mut r = sample_row("moha", 2);
    r.first_seen = Some("not-a-date".into());

    store.upsert_source_from_registry(&r, now).unwrap();
    let s = store.get_source("moha").unwrap().unwrap();
    // Garbage input shouldn't reject the row; fall-back to `now` is the
    // documented behaviour (see store.rs §Invariants #2).
    assert!((s.first_seen - now).num_seconds().abs() < 2);
}

#[test]
fn unknown_status_string_defaults_to_active() {
    let store = Store::open_in_memory().unwrap();
    let now = Utc::now();
    let mut r = sample_row("moha", 2);
    r.status = Some("totally-bogus".into());

    store.upsert_source_from_registry(&r, now).unwrap();
    let s = store.get_source("moha").unwrap().unwrap();
    // Loader tolerates unknown status strings and stamps Active. This is a
    // deliberate forgiving-import stance; a typo in a manually-edited
    // override shouldn't refuse to load the registry.
    assert_eq!(s.status, SourceStatus::Active);
}

#[test]
fn out_of_range_tier_stored_with_fallback_poll_hours() {
    let store = Store::open_in_memory().unwrap();
    let now = Utc::now();

    // Tier 0 and tier 99 are both out of the [1,5] policy range.
    let mut r0 = sample_row("a", 0);
    r0.poll_interval_hours = None;
    let mut r99 = sample_row("b", 99);
    r99.poll_interval_hours = None;
    store.upsert_source_from_registry(&r0, now).unwrap();
    store.upsert_source_from_registry(&r99, now).unwrap();

    // Both stored; poll_interval_hours falls back to Tier::default_poll_hours
    // which returns 48h for anything outside [1,5].
    let a = store.get_source("a").unwrap().unwrap();
    let b = store.get_source("b").unwrap().unwrap();
    assert_eq!(a.tier.0, 0);
    assert_eq!(a.poll_interval_hours, 48);
    assert_eq!(b.tier.0, 99);
    assert_eq!(b.poll_interval_hours, 48);
}

#[test]
fn duplicate_source_id_in_jsonl_last_wins() {
    let store = Store::open_in_memory().unwrap();
    let mut tmp = NamedTempFile::new().unwrap();
    // Two rows for "moha" — second should overwrite first.
    writeln!(tmp, r#"{{"source_id":"moha","domain":"moha.gov.np","homepage_url":"https://moha.gov.np/","tier_guess":3,"name_en":"first"}}"#).unwrap();
    writeln!(tmp, r#"{{"source_id":"moha","domain":"moha.gov.np","homepage_url":"https://moha.gov.np/","tier_guess":2,"name_en":"second"}}"#).unwrap();
    tmp.flush().unwrap();

    let report = sync_registry(&store, tmp.path()).unwrap();
    // First line inserts, second line updates — total_rows=2.
    assert_eq!(report.total_rows, 2);
    assert_eq!(report.inserted, 1);
    assert_eq!(report.updated, 1);
    assert_eq!(store.source_count().unwrap(), 1);

    let s = store.get_source("moha").unwrap().unwrap();
    assert_eq!(s.tier.0, 2);
    assert_eq!(s.name_en.as_deref(), Some("second"));
}

#[test]
fn row_missing_required_fields_is_skipped() {
    let store = Store::open_in_memory().unwrap();
    let mut tmp = NamedTempFile::new().unwrap();
    writeln!(tmp, r#"{{"source_id":"","domain":"x.gov.np","homepage_url":"https://x.gov.np/","tier_guess":1}}"#).unwrap();
    writeln!(tmp, r#"{{"source_id":"y","domain":"y.gov.np","homepage_url":"","tier_guess":1}}"#).unwrap();
    writeln!(tmp, r#"{{"source_id":"z","domain":"z.gov.np","homepage_url":"https://z.gov.np/","tier_guess":1}}"#).unwrap();
    tmp.flush().unwrap();

    let report = sync_registry(&store, tmp.path()).unwrap();
    assert_eq!(report.total_rows, 3);
    assert_eq!(report.skipped_missing_fields, 2);
    assert_eq!(report.inserted, 1);
    assert_eq!(store.source_count().unwrap(), 1);
}

#[test]
fn source_roundtrips_unicode_and_status_cleanly() {
    // After the row_to_source refactor, readback with Devanagari in name_np
    // and non-default status should not panic or error.
    let store = Store::open_in_memory().unwrap();
    let now = Utc::now();

    let mut r = sample_row("rajpatra", 1);
    r.name_np = Some("नेपाल राजपत्र".into());
    r.province = Some("काठमाडौँ".into());
    store.upsert_source_from_registry(&r, now).unwrap();

    // Mutate to a non-Active status directly (simulating what the daemon
    // would do when flagging a shell-detected source).
    store.conn().execute(
        "UPDATE sources SET status = 'js_only' WHERE source_id = 'rajpatra'",
        rusqlite::params![],
    ).unwrap();

    let s = store.get_source("rajpatra").unwrap().unwrap();
    assert_eq!(s.name_np.as_deref(), Some("नेपाल राजपत्र"));
    assert_eq!(s.province.as_deref(), Some("काठमाडौँ"));
    assert_eq!(s.status, SourceStatus::JsOnly);
}

#[test]
fn corrupted_status_in_db_surfaces_as_error_on_read() {
    // If something outside the code path writes a bogus status string (e.g.,
    // hand-edit of index.db), get_source should surface an error, not silently
    // return a fabricated default.
    let store = Store::open_in_memory().unwrap();
    let now = Utc::now();
    store.upsert_source_from_registry(&sample_row("x", 1), now).unwrap();
    store.conn().execute(
        "UPDATE sources SET status = 'totally-wrong' WHERE source_id = 'x'",
        rusqlite::params![],
    ).unwrap();

    let err = store.get_source("x");
    assert!(err.is_err(), "expected error, got {err:?}");
}

#[test]
fn list_sources_orders_by_tier_then_id() {
    let store = Store::open_in_memory().unwrap();
    let now = Utc::now();
    // Insert out of order.
    store.upsert_source_from_registry(&sample_row("zzz", 5), now).unwrap();
    store.upsert_source_from_registry(&sample_row("aaa", 3), now).unwrap();
    store.upsert_source_from_registry(&sample_row("bbb", 1), now).unwrap();
    store.upsert_source_from_registry(&sample_row("aaaa", 1), now).unwrap();

    let all = store.list_sources().unwrap();
    let ids: Vec<_> = all.iter().map(|s| s.source_id.clone()).collect();
    assert_eq!(ids, vec!["aaaa", "bbb", "aaa", "zzz"]);
}

#[test]
fn duplicate_doc_id_insert_is_rejected() {
    let mut store = Store::open_in_memory().unwrap();
    let now = Utc::now();
    store.upsert_source_from_registry(&sample_row("moha", 2), now).unwrap();

    let d1 = sample_document("moha", "https://moha.gov.np/a", "dup-id", "hash1");
    store.upsert_document(&d1).unwrap();

    // Second insert with the SAME doc_id but a different URL should hit the
    // PRIMARY KEY constraint on documents.doc_id. The daemon computes
    // doc_id = hash(source, url, content_hash) so a collision here would
    // indicate an upstream bug; we want it loud, not silent.
    let d2 = sample_document("moha", "https://moha.gov.np/b", "dup-id", "hash2");
    let err = store.upsert_document(&d2);
    assert!(err.is_err(), "expected PK violation, got {err:?}");
}

#[test]
fn empty_content_hash_still_treated_as_value() {
    // Defensive: an upstream bug could emit a document with empty content_hash.
    // Document the current behavior so we notice if it ever changes.
    let mut store = Store::open_in_memory().unwrap();
    let now = Utc::now();
    store.upsert_source_from_registry(&sample_row("moha", 2), now).unwrap();

    let d1 = sample_document("moha", "https://moha.gov.np/a", "doc-1", "");
    assert_eq!(store.upsert_document(&d1).unwrap(), DocumentOutcome::Inserted);

    // Second fetch of the same URL with empty hash again: strict equality
    // says "Unchanged" — NOT what we'd want in production. This test pins
    // the current behavior so a future fix (treat empty-hash as "always
    // re-insert") is a deliberate change, not an accident.
    let mut d2 = d1.clone();
    d2.doc_id = "doc-2".into();
    assert_eq!(store.upsert_document(&d2).unwrap(), DocumentOutcome::Unchanged);
}

fn sample_document(
    source_id: &str,
    url: &str,
    doc_id: &str,
    hash: &str,
) -> Document {
    Document {
        doc_id: doc_id.to_string(),
        source_id: source_id.to_string(),
        url: url.to_string(),
        content_hash: hash.to_string(),
        fetched_at: Utc::now(),
        superseded_by: None,
        removed_at: None,
        doc_type: DocType::Html,
        status_code: 200,
        title: None,
        language: None,
        date_published: None,
        raw_blob_path: format!("blobs/{source_id}/{hash}.html"),
        extracted_text_path: None,
        text_chars: 0,
        size_bytes: 1024,
        depth: 0,
        priority_at_fetch: Some(100),
    }
}

#[test]
fn concurrent_reader_sees_writer_commits_without_deadlock() {
    // WAL mode (set in init_schema) should let a reader query while a writer
    // holds an open transaction. Validates that the admin CLI can `status`
    // a live database while the daemon is mid-crawl.
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);

    // Seed one row so the reader has something to find.
    {
        let seed = Store::open(&path).unwrap();
        seed.upsert_source_from_registry(&sample_row("seed", 2), Utc::now())
            .unwrap();
    }

    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        let store = Store::open(&writer_path).unwrap();
        for i in 0..20 {
            let sid = format!("w{i}");
            store
                .upsert_source_from_registry(&sample_row(&sid, 3), Utc::now())
                .unwrap();
        }
        store.source_count().unwrap()
    });

    let reader_path = path.clone();
    let reader = std::thread::spawn(move || {
        let store = Store::open(&reader_path).unwrap();
        // Spin-read; WAL should never raise SQLITE_BUSY here.
        let mut last = 0u64;
        for _ in 0..50 {
            let n = store.source_count().unwrap();
            if n > last {
                last = n;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        last
    });

    let w_count = writer.join().unwrap();
    let r_count = reader.join().unwrap();
    assert_eq!(w_count, 21, "writer didn't insert everything");
    // Reader should observe at least the initial seed; reading commits as
    // they land is best-effort depending on scheduling, but we must at least
    // see the committed seed row.
    assert!(r_count >= 1, "reader saw {r_count} rows, expected >= 1");

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
}

#[test]
fn dropping_mid_transaction_rolls_back() {
    // Simulates the daemon getting SIGKILL'd mid-write. The in-flight
    // transaction must not leave partial state; reopening the DB should show
    // only the committed rows.
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);

    {
        let store = Store::open(&path).unwrap();
        store
            .upsert_source_from_registry(&sample_row("committed", 1), Utc::now())
            .unwrap();

        // Open a raw transaction, insert into fetch_events, then drop without
        // committing. rusqlite's Transaction::drop rolls back by default.
        let conn = store.conn();
        conn.execute("BEGIN", []).unwrap();
        conn.execute(
            "INSERT INTO fetch_events (source_id, url, fetched_at, status)
             VALUES ('committed','uncommitted-url', ?1, 500)",
            rusqlite::params![Utc::now().to_rfc3339()],
        )
        .unwrap();
        // Simulate crash: force a ROLLBACK to stand in for the process dying
        // before COMMIT. (rusqlite's Connection can't actually be killed
        // mid-tx within one process without unsafe tricks; ROLLBACK verifies
        // the invariant that uncommitted writes don't survive.)
        conn.execute("ROLLBACK", []).unwrap();
    }

    let reopened = Store::open(&path).unwrap();
    let committed_count: i64 = reopened
        .conn()
        .query_row(
            "SELECT COUNT(*) FROM fetch_events WHERE url='uncommitted-url'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        committed_count, 0,
        "uncommitted fetch_event leaked across reopen"
    );
    // The committed source row should still be there.
    assert!(reopened.get_source("committed").unwrap().is_some());

    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
}

#[test]
fn migrations_record_schema_version() {
    // Open twice; schema_version row should exist and not grow duplicates.
    let tmp = NamedTempFile::new().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp); // release the file so Store can create it fresh

    let s1 = Store::open(&path).unwrap();
    let v1: String = s1.conn()
        .query_row("SELECT value FROM _meta WHERE key='schema_version'", [], |r| r.get(0))
        .unwrap();
    drop(s1);

    let s2 = Store::open(&path).unwrap();
    let v2: String = s2.conn()
        .query_row("SELECT value FROM _meta WHERE key='schema_version'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v1, v2);
    let count: i64 = s2.conn()
        .query_row("SELECT COUNT(*) FROM _meta WHERE key='schema_version'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);

    std::fs::remove_file(&path).ok();
    let _ = std::fs::remove_file(path.with_extension("db-shm"));
    let _ = std::fs::remove_file(path.with_extension("db-wal"));
}

#[test]
fn record_fetch_event_roundtrips() {
    let store = Store::open_in_memory().unwrap();
    let now = Utc::now();
    store
        .upsert_source_from_registry(&sample_row("moha", 2), now)
        .unwrap();

    let id = store
        .record_fetch_event(&FetchEvent {
            source_id: "moha".into(),
            url: "https://moha.gov.np/".into(),
            fetched_at: now,
            status: 200,
            elapsed_ms: Some(432),
            error: None,
            doc_type: Some(DocType::Html),
            bytes: Some(10240),
        })
        .unwrap();
    assert!(id >= 1);

    let count: i64 = store
        .conn()
        .query_row("SELECT COUNT(*) FROM fetch_events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
}
