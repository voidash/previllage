//! Tests for crawler_v2::legacy_import — reads Python-prototype JSONL
//! manifests into SQLite.

use chrono::Utc;
use gemma_god::crawler_v2::legacy_import::{import_legacy, ImportOptions};
use gemma_god::crawler_v2::types::RegistryRow;
use gemma_god::crawler_v2::{DocType, ImportError, Store};
use std::fs;
use tempfile::TempDir;

fn seed_source(store: &Store, sid: &str) {
    store
        .upsert_source_from_registry(
            &RegistryRow {
                source_id: sid.to_string(),
                domain: format!("{sid}.gov.np"),
                homepage_url: format!("https://{sid}.gov.np/"),
                name_en: None,
                name_np: None,
                office_type: None,
                province: None,
                tier: 2,
                poll_interval_hours: None,
                status: None,
                first_seen: None,
            },
            Utc::now(),
        )
        .unwrap();
}

#[test]
fn imports_healthy_rows_into_documents() {
    let dir = TempDir::new().unwrap();
    let manifests = dir.path().join("manifests");
    fs::create_dir_all(&manifests).unwrap();

    // Two rows: one HTML, one PDF. Schema matches scripts/crawl_sources.py.
    fs::write(
        manifests.join("moha_gov_np.jsonl"),
        concat!(
            r#"{"url":"https://moha.gov.np/a","depth":0,"fetched_at":"2026-04-20T00:00:00+00:00","status":200,"content_type":"text/html","doc_type":"html","content_hash":"aaaa1111","size_bytes":1024,"raw_path":"raw/moha_gov_np/aaaa1111.html","extracted_path":"extracted/moha_gov_np/aaaa1111.txt","text_chars":300}"#,
            "\n",
            r#"{"url":"https://moha.gov.np/docs/1.pdf","depth":2,"fetched_at":"2026-04-20T00:01:00+00:00","status":200,"content_type":"application/pdf","doc_type":"pdf","content_hash":"bbbb2222","size_bytes":20480,"raw_path":"raw/moha_gov_np/bbbb2222.pdf","text_chars":0}"#,
            "\n",
        ),
    )
    .unwrap();

    let db_tmp = TempDir::new().unwrap();
    let db_path = db_tmp.path().join("index.db");
    let mut store = Store::open(&db_path).unwrap();
    seed_source(&store, "moha_gov_np");

    let report = import_legacy(
        &mut store,
        &ImportOptions {
            manifests_dir: manifests,
            lenient: false,
        },
    )
    .unwrap();

    assert_eq!(report.rows_total, 2);
    assert_eq!(report.rows_inserted, 2);
    assert_eq!(report.rows_skipped_error, 0);

    // Verify documents actually landed with correct types + paths.
    let docs = store
        .conn()
        .prepare("SELECT url, doc_type, raw_blob_path FROM documents ORDER BY url")
        .unwrap()
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(docs.len(), 2);
    // Legacy paths preserved verbatim (no sharded relocation).
    assert!(docs[0].2.starts_with("raw/moha_gov_np/"));
    assert!(docs[1].2.starts_with("raw/moha_gov_np/"));
}

#[test]
fn skips_error_rows_and_malformed_lines() {
    let dir = TempDir::new().unwrap();
    let manifests = dir.path().join("manifests");
    fs::create_dir_all(&manifests).unwrap();

    fs::write(
        manifests.join("moha_gov_np.jsonl"),
        concat!(
            // Healthy row
            r#"{"url":"https://moha.gov.np/a","status":200,"doc_type":"html","content_hash":"aa","raw_path":"raw/m/aa.html"}"#,
            "\n",
            // 404
            r#"{"url":"https://moha.gov.np/gone","status":404}"#,
            "\n",
            // Transport error
            r#"{"url":"https://moha.gov.np/x","status":0,"error":"timeout"}"#,
            "\n",
            // Missing content_hash
            r#"{"url":"https://moha.gov.np/n","status":200,"doc_type":"html"}"#,
            "\n",
            // Malformed JSON
            r#"{bogus"#,
            "\n",
            // Blank
            "",
            "\n",
        ),
    )
    .unwrap();

    let db_tmp = TempDir::new().unwrap();
    let db_path = db_tmp.path().join("index.db");
    let mut store = Store::open(&db_path).unwrap();
    seed_source(&store, "moha_gov_np");

    let report = import_legacy(
        &mut store,
        &ImportOptions {
            manifests_dir: manifests,
            lenient: false,
        },
    )
    .unwrap();

    assert_eq!(report.rows_inserted, 1);
    assert_eq!(report.rows_skipped_error, 2); // 404 + transport error
    assert_eq!(report.rows_skipped_no_hash, 1);
    assert_eq!(report.rows_skipped_malformed, 1);
}

#[test]
fn unknown_source_errors_by_default_skips_with_lenient() {
    let dir = TempDir::new().unwrap();
    let manifests = dir.path().join("manifests");
    fs::create_dir_all(&manifests).unwrap();

    // Manifest file for a source that's not in the registry.
    fs::write(
        manifests.join("stray_source.jsonl"),
        r#"{"url":"https://stray.gov.np/","status":200,"doc_type":"html","content_hash":"11","raw_path":"raw/s/11.html"}"#,
    )
    .unwrap();

    let db_tmp = TempDir::new().unwrap();
    let db_path = db_tmp.path().join("index.db");
    let mut store = Store::open(&db_path).unwrap();
    // Seed a DIFFERENT source so the registry isn't empty.
    seed_source(&store, "moha_gov_np");

    // Strict mode errors.
    let err = import_legacy(
        &mut store,
        &ImportOptions {
            manifests_dir: manifests.clone(),
            lenient: false,
        },
    )
    .unwrap_err();
    match err {
        ImportError::UnknownSource { sid } => assert_eq!(sid, "stray_source"),
        other => panic!("expected UnknownSource, got {other:?}"),
    }

    // Lenient mode records + skips.
    let report = import_legacy(
        &mut store,
        &ImportOptions {
            manifests_dir: manifests,
            lenient: true,
        },
    )
    .unwrap();
    assert_eq!(report.unknown_sources, vec!["stray_source"]);
    assert_eq!(report.rows_inserted, 0);
}

#[test]
fn rerun_import_is_idempotent_via_diff_detection() {
    let dir = TempDir::new().unwrap();
    let manifests = dir.path().join("manifests");
    fs::create_dir_all(&manifests).unwrap();
    fs::write(
        manifests.join("moha_gov_np.jsonl"),
        r#"{"url":"https://moha.gov.np/a","status":200,"doc_type":"html","content_hash":"aa","raw_path":"raw/m/aa.html"}"#,
    )
    .unwrap();

    let db_tmp = TempDir::new().unwrap();
    let db_path = db_tmp.path().join("index.db");
    let mut store = Store::open(&db_path).unwrap();
    seed_source(&store, "moha_gov_np");

    let r1 = import_legacy(
        &mut store,
        &ImportOptions {
            manifests_dir: manifests.clone(),
            lenient: false,
        },
    )
    .unwrap();
    assert_eq!(r1.rows_inserted, 1);

    // Second pass — same manifest, same hash. Should show Unchanged, not
    // a duplicate insertion.
    let r2 = import_legacy(
        &mut store,
        &ImportOptions {
            manifests_dir: manifests,
            lenient: false,
        },
    )
    .unwrap();
    assert_eq!(r2.rows_inserted, 0);
    assert_eq!(r2.rows_unchanged, 1);
}

#[test]
fn doc_type_sniffed_from_url_when_hint_missing() {
    // The Python crawler sometimes emits rows without a doc_type (older
    // versions or edge cases). Verify URL-extension sniffing kicks in.
    let dir = TempDir::new().unwrap();
    let manifests = dir.path().join("manifests");
    fs::create_dir_all(&manifests).unwrap();
    fs::write(
        manifests.join("moha_gov_np.jsonl"),
        r#"{"url":"https://moha.gov.np/a/b.pdf","status":200,"content_hash":"cc","raw_path":"raw/m/cc.pdf"}"#,
    )
    .unwrap();

    let db_tmp = TempDir::new().unwrap();
    let db_path = db_tmp.path().join("index.db");
    let mut store = Store::open(&db_path).unwrap();
    seed_source(&store, "moha_gov_np");

    import_legacy(
        &mut store,
        &ImportOptions {
            manifests_dir: manifests,
            lenient: false,
        },
    )
    .unwrap();

    let t: String = store
        .conn()
        .query_row("SELECT doc_type FROM documents LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(t, DocType::Pdf.as_str());
}
