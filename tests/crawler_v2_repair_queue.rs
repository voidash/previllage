//! Tests for the repair_queue persistence: enqueue, list_pending, get,
//! update (status transitions), has_pending, and pending_repair_count.
//!
//! Status flow under test:
//!   Pending → Dispatched → (Applied | HumanReview | Deadletter)

use chrono::{Duration, Utc};
use gemma_god::crawler_v2::types::{RegistryRow, RepairItem, RepairStatus};
use gemma_god::crawler_v2::Store;

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
                tier: 5,
                poll_interval_hours: None,
                status: None,
                first_seen: None,
            },
            Utc::now(),
        )
        .unwrap();
}

fn pending_item(sid: &str, evidence: &str) -> RepairItem {
    RepairItem {
        queue_id: None,
        source_id: sid.into(),
        queued_at: Utc::now(),
        status: RepairStatus::Pending,
        dispatched_at: None,
        completed_at: None,
        failure_evidence: evidence.into(),
        sample_html_path: None,
        proposed_recipe: None,
        dry_run_result: None,
        apply_outcome: None,
        error_log: None,
    }
}

#[test]
fn enqueue_and_round_trip() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "jirimun_gov_np");

    let qid = store
        .enqueue_repair(&pending_item("jirimun_gov_np", "{\"reason\":\"empty_5\"}"))
        .unwrap();
    assert!(qid > 0);

    let got = store.get_repair(qid).unwrap().expect("row exists");
    assert_eq!(got.queue_id, Some(qid));
    assert_eq!(got.source_id, "jirimun_gov_np");
    assert_eq!(got.status, RepairStatus::Pending);
    assert_eq!(got.failure_evidence, "{\"reason\":\"empty_5\"}");
    assert!(got.dispatched_at.is_none());
}

#[test]
fn list_pending_excludes_dispatched_and_terminal() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    seed_source(&store, "b");
    seed_source(&store, "c");

    let qa = store.enqueue_repair(&pending_item("a", "{}")).unwrap();
    let _qb = store.enqueue_repair(&pending_item("b", "{}")).unwrap();
    let qc = store.enqueue_repair(&pending_item("c", "{}")).unwrap();

    // Move qa → Dispatched, qc → Applied. Only b should remain pending.
    let mut a = store.get_repair(qa).unwrap().unwrap();
    a.status = RepairStatus::Dispatched;
    a.dispatched_at = Some(Utc::now());
    store.update_repair(&a).unwrap();

    let mut c = store.get_repair(qc).unwrap().unwrap();
    c.status = RepairStatus::Applied;
    c.completed_at = Some(Utc::now());
    c.apply_outcome = Some("auto".into());
    store.update_repair(&c).unwrap();

    let pending = store.list_pending_repairs().unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].source_id, "b");
    assert_eq!(store.pending_repair_count().unwrap(), 1);
}

#[test]
fn list_pending_orders_by_queued_at_oldest_first() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    seed_source(&store, "b");

    // Create items with explicit queued_at to control ordering.
    let now = Utc::now();
    let mut older = pending_item("a", "{}");
    older.queued_at = now - Duration::hours(2);
    let mut newer = pending_item("b", "{}");
    newer.queued_at = now;

    // Insert newer first to prove ordering isn't insertion-order.
    store.enqueue_repair(&newer).unwrap();
    store.enqueue_repair(&older).unwrap();

    let pending = store.list_pending_repairs().unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].source_id, "a"); // older first (FIFO)
    assert_eq!(pending[1].source_id, "b");
}

#[test]
fn has_pending_repair_distinguishes_sources() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");
    seed_source(&store, "b");

    store.enqueue_repair(&pending_item("a", "{}")).unwrap();
    assert!(store.has_pending_repair("a").unwrap());
    assert!(!store.has_pending_repair("b").unwrap());
}

#[test]
fn has_pending_ignores_terminal_states() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");

    let qid = store.enqueue_repair(&pending_item("a", "{}")).unwrap();
    assert!(store.has_pending_repair("a").unwrap());

    // Terminal: Applied. Must no longer count as pending.
    let mut item = store.get_repair(qid).unwrap().unwrap();
    item.status = RepairStatus::Applied;
    store.update_repair(&item).unwrap();
    assert!(!store.has_pending_repair("a").unwrap());
}

#[test]
fn update_persists_proposed_recipe_and_dry_run_result() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");

    let qid = store.enqueue_repair(&pending_item("a", "{}")).unwrap();

    let mut item = store.get_repair(qid).unwrap().unwrap();
    item.status = RepairStatus::Dispatched;
    item.dispatched_at = Some(Utc::now());
    item.proposed_recipe = Some(r#"{"source_id":"a","entry_points":["..."]}"#.into());
    item.dry_run_result = Some(r#"{"extracted_count":12,"junk_score":0.1}"#.into());
    store.update_repair(&item).unwrap();

    let got = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(got.status, RepairStatus::Dispatched);
    assert!(got.proposed_recipe.unwrap().contains("entry_points"));
    assert!(got.dry_run_result.unwrap().contains("extracted_count"));
    assert!(got.dispatched_at.is_some());
}

#[test]
fn deadletter_records_error_log() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "a");

    let qid = store.enqueue_repair(&pending_item("a", "{}")).unwrap();
    let mut item = store.get_repair(qid).unwrap().unwrap();
    item.status = RepairStatus::Deadletter;
    item.completed_at = Some(Utc::now());
    item.error_log = Some("agent timed out after 600s".into());
    item.apply_outcome = Some("deadletter".into());
    store.update_repair(&item).unwrap();

    let got = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(got.status, RepairStatus::Deadletter);
    assert_eq!(got.error_log.unwrap(), "agent timed out after 600s");
}

#[test]
fn update_without_queue_id_errors() {
    let store = Store::open_in_memory().unwrap();
    let mut item = pending_item("a", "{}");
    item.queue_id = None;
    item.status = RepairStatus::Applied;
    let r = store.update_repair(&item);
    assert!(r.is_err(), "update_repair must reject queue_id=None");
}

#[test]
fn pending_repair_count_on_empty_db_is_zero() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.pending_repair_count().unwrap(), 0);
    assert!(store.list_pending_repairs().unwrap().is_empty());
}
