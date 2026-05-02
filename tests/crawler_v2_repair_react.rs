//! Integration tests for `react_to_verdict`: every verdict branch wires
//! through to the right Store side-effect (status flip, repair enqueue, or
//! no-op) and is idempotent on repeated calls.

use chrono::Utc;
use gemma_god::crawler_v2::health::HealthMetrics;
use gemma_god::crawler_v2::types::{DocType, Document, RegistryRow, SourceStatus};
use gemma_god::crawler_v2::{react_to_verdict, HealthVerdict, ReactionOutcome, Store};

fn seed_source(store: &Store, sid: &str) {
    store
        .upsert_source_from_registry(
            &RegistryRow {
                source_id: sid.into(),
                domain: format!("{sid}.gov.np"),
                homepage_url: format!("https://{sid}.gov.np/"),
                name_en: None,
                name_np: None,
                office_type: None,
                province: None,
                tier: 4,
                poll_interval_hours: None,
                status: None,
                first_seen: None,
            },
            Utc::now(),
        )
        .unwrap();
}

fn metrics_for(sid: &str, verdict: HealthVerdict) -> HealthMetrics {
    let now = Utc::now();
    HealthMetrics {
        source_id: sid.into(),
        window_start: now - chrono::Duration::days(7),
        window_end: now,
        n_cycles: 5,
        successes: 5,
        empty_extractions: 5,
        error_rate: 0.0,
        content_hash_change_rate: 0.0,
        structural_failure_streak: 5,
        verdict,
    }
}

fn seed_html_doc(store: &mut Store, sid: &str, raw_path: &str, extracted_path: Option<&str>) {
    let hash = format!("h_{sid}");
    let doc = Document {
        doc_id: format!("d_{sid}"),
        source_id: sid.into(),
        url: format!("https://{sid}.gov.np/"),
        content_hash: hash,
        fetched_at: Utc::now(),
        superseded_by: None,
        removed_at: None,
        doc_type: DocType::Html,
        status_code: 200,
        title: None,
        language: None,
        date_published: None,
        raw_blob_path: raw_path.into(),
        extracted_text_path: extracted_path.map(|s| s.to_string()),
        text_chars: 1000,
        size_bytes: 4096,
        depth: 0,
        priority_at_fetch: Some(100),
    };
    store.upsert_document(&doc).unwrap();
}

#[test]
fn healthy_verdict_is_no_change() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    let source = store.get_source("a").unwrap().unwrap();
    let m = metrics_for("a", HealthVerdict::Healthy);

    let r = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    assert_eq!(r, ReactionOutcome::NoChange);
    // No status change, no repair queued.
    assert_eq!(store.get_source("a").unwrap().unwrap().status, SourceStatus::Active);
    assert_eq!(store.pending_repair_count().unwrap(), 0);
}

#[test]
fn shell_detected_is_no_change_at_repair_layer() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    // Worker would have already flipped to JsOnly. Pretend.
    store.mark_source_status("a", SourceStatus::JsOnly).unwrap();
    let source = store.get_source("a").unwrap().unwrap();
    let m = metrics_for("a", HealthVerdict::ShellDetected);

    let r = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    assert_eq!(r, ReactionOutcome::NoChange);
    assert_eq!(store.get_source("a").unwrap().unwrap().status, SourceStatus::JsOnly);
}

#[test]
fn dead_verdict_marks_source_dead() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    let source = store.get_source("a").unwrap().unwrap();
    let m = metrics_for(
        "a",
        HealthVerdict::Dead {
            reason: "domain expired".into(),
        },
    );

    let r = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    assert_eq!(r, ReactionOutcome::StatusUpdated(SourceStatus::Dead));
    assert_eq!(store.get_source("a").unwrap().unwrap().status, SourceStatus::Dead);
}

#[test]
fn dormant_verdict_marks_source_dormant() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    let source = store.get_source("a").unwrap().unwrap();
    let m = metrics_for(
        "a",
        HealthVerdict::DormantBlocked {
            reason: "100% 403".into(),
        },
    );

    let r = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    assert_eq!(r, ReactionOutcome::StatusUpdated(SourceStatus::Dormant));
    assert_eq!(store.get_source("a").unwrap().unwrap().status, SourceStatus::Dormant);
}

#[test]
fn structural_failure_enqueues_repair_with_evidence() {
    let mut store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    seed_html_doc(
        &mut store,
        "a",
        "blobs/a/he/h_a.html",
        Some("extracted/a/he/h_a.txt"),
    );
    let source = store.get_source("a").unwrap().unwrap();
    let m = metrics_for(
        "a",
        HealthVerdict::StructurallyFailed {
            reason: "5 consecutive non-productive cycles despite 12 historical inserts".into(),
        },
    );

    let r = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    let qid = match r {
        ReactionOutcome::RepairEnqueued(q) => q,
        other => panic!("expected RepairEnqueued, got {other:?}"),
    };
    let item = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(item.source_id, "a");
    assert_eq!(
        item.sample_html_path.as_deref(),
        Some("extracted/a/he/h_a.txt"),
        "should prefer extracted-text sidecar over raw HTML",
    );
    assert!(item.failure_evidence.contains("structurally_failed"));
    assert!(item.failure_evidence.contains("non-productive"));
    // Source itself stays Active — we want the daemon to keep polling so the
    // dispatcher can retry once a recipe is applied.
    assert_eq!(store.get_source("a").unwrap().unwrap().status, SourceStatus::Active);
}

#[test]
fn structural_failure_falls_back_to_raw_blob_when_no_extracted_sidecar() {
    let mut store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    seed_html_doc(&mut store, "a", "blobs/a/he/h_a.html", None);
    let source = store.get_source("a").unwrap().unwrap();
    let m = metrics_for(
        "a",
        HealthVerdict::StructurallyFailed {
            reason: "no recent docs".into(),
        },
    );

    let r = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    let qid = match r {
        ReactionOutcome::RepairEnqueued(q) => q,
        other => panic!("expected RepairEnqueued, got {other:?}"),
    };
    let item = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(item.sample_html_path.as_deref(), Some("blobs/a/he/h_a.html"));
}

#[test]
fn structural_failure_with_no_documents_still_queues_with_null_sample() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    let source = store.get_source("a").unwrap().unwrap();
    let m = metrics_for(
        "a",
        HealthVerdict::StructurallyFailed {
            reason: "no recent docs".into(),
        },
    );

    let r = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    let qid = match r {
        ReactionOutcome::RepairEnqueued(q) => q,
        other => panic!("expected RepairEnqueued, got {other:?}"),
    };
    let item = store.get_repair(qid).unwrap().unwrap();
    assert!(
        item.sample_html_path.is_none(),
        "sample_html_path should be NULL when source has no live HTML docs",
    );
}

#[test]
fn structural_failure_is_idempotent_when_pending_already() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    let source = store.get_source("a").unwrap().unwrap();
    let m = metrics_for(
        "a",
        HealthVerdict::StructurallyFailed {
            reason: "first".into(),
        },
    );

    let r1 = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    assert!(matches!(r1, ReactionOutcome::RepairEnqueued(_)));

    // Second call → must return AlreadyPending and NOT insert another row.
    let r2 = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    assert_eq!(r2, ReactionOutcome::RepairAlreadyPending);
    assert_eq!(store.pending_repair_count().unwrap(), 1);
}

#[test]
fn insufficient_data_is_no_change() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    let source = store.get_source("a").unwrap().unwrap();
    let m = metrics_for("a", HealthVerdict::InsufficientData);
    let r = react_to_verdict(&store, &source, &m, Utc::now()).unwrap();
    assert_eq!(r, ReactionOutcome::NoChange);
}
