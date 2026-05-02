//! Tests for `Store::insert_poll_cycle` + `list_poll_cycles_for_source`.
//!
//! Health evaluation reads rolling windows of cycles to decide whether a
//! source has gone structurally broken, so the storage round-trip and
//! time-window filtering are load-bearing.

use chrono::{Duration, Utc};
use gemma_god::crawler_v2::types::{PollCycle, RegistryRow};
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
                tier: 2,
                poll_interval_hours: None,
                status: None,
                first_seen: None,
            },
            Utc::now(),
        )
        .unwrap();
}

fn cycle(sid: &str, started: chrono::DateTime<Utc>, docs_inserted: u32) -> PollCycle {
    PollCycle {
        cycle_id: None,
        source_id: sid.into(),
        started_at: started,
        finished_at: started + Duration::seconds(30),
        stop_reason: "frontier_drained".into(),
        elapsed_sec: 30,
        html_fetched: 5,
        binaries_fetched: 0,
        other_fetched: 0,
        errors: 0,
        docs_inserted,
        docs_superseded: 0,
        docs_unchanged: 0,
        shell_flagged: false,
    }
}

#[test]
fn insert_assigns_id_and_round_trips() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");

    let now = Utc::now();
    let id = store.insert_poll_cycle(&cycle("moha", now, 3)).unwrap();
    assert!(id > 0);
    assert_eq!(store.poll_cycle_count("moha").unwrap(), 1);

    let cycles = store
        .list_poll_cycles_for_source("moha", now - Duration::hours(1), 0)
        .unwrap();
    assert_eq!(cycles.len(), 1);
    let c = &cycles[0];
    assert_eq!(c.cycle_id, Some(id));
    assert_eq!(c.source_id, "moha");
    assert_eq!(c.docs_inserted, 3);
    assert_eq!(c.stop_reason, "frontier_drained");
    // RFC 3339 round-trips with nanosecond precision; the helper truncates
    // to second-level since we only stored 30s offsets.
    assert_eq!(c.elapsed_sec, 30);
}

#[test]
fn list_orders_newest_first_and_respects_window() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    let now = Utc::now();

    // 3 cycles spread across the last 12 hours.
    let t12h = now - Duration::hours(12);
    let t6h = now - Duration::hours(6);
    let t1h = now - Duration::hours(1);
    store.insert_poll_cycle(&cycle("moha", t12h, 1)).unwrap();
    store.insert_poll_cycle(&cycle("moha", t6h, 2)).unwrap();
    store.insert_poll_cycle(&cycle("moha", t1h, 3)).unwrap();

    // Window = last 8 hours → expect t6h and t1h, newest first.
    let cycles = store
        .list_poll_cycles_for_source("moha", now - Duration::hours(8), 0)
        .unwrap();
    assert_eq!(cycles.len(), 2);
    assert_eq!(cycles[0].docs_inserted, 3); // newest
    assert_eq!(cycles[1].docs_inserted, 2);
}

#[test]
fn limit_caps_returned_rows() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    let now = Utc::now();
    for i in 0..6 {
        store
            .insert_poll_cycle(&cycle("moha", now - Duration::minutes(i * 10), i as u32))
            .unwrap();
    }
    let cycles = store
        .list_poll_cycles_for_source("moha", now - Duration::hours(2), 3)
        .unwrap();
    assert_eq!(cycles.len(), 3);
    // Newest first, so the most-recent three were i=0,1,2.
    assert_eq!(cycles[0].docs_inserted, 0);
    assert_eq!(cycles[1].docs_inserted, 1);
    assert_eq!(cycles[2].docs_inserted, 2);
}

#[test]
fn cycles_are_isolated_per_source() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    seed_source(&store, "ird");
    let now = Utc::now();
    store.insert_poll_cycle(&cycle("moha", now, 1)).unwrap();
    store.insert_poll_cycle(&cycle("ird", now, 2)).unwrap();
    store.insert_poll_cycle(&cycle("ird", now - Duration::minutes(5), 3)).unwrap();

    assert_eq!(store.poll_cycle_count("moha").unwrap(), 1);
    assert_eq!(store.poll_cycle_count("ird").unwrap(), 2);
}

#[test]
fn shell_flagged_round_trips() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "palika");
    let mut c = cycle("palika", Utc::now(), 0);
    c.stop_reason = "shell_detected".into();
    c.shell_flagged = true;
    store.insert_poll_cycle(&c).unwrap();

    let got = store
        .list_poll_cycles_for_source("palika", Utc::now() - Duration::hours(1), 0)
        .unwrap();
    assert_eq!(got.len(), 1);
    assert!(got[0].shell_flagged);
    assert_eq!(got[0].stop_reason, "shell_detected");
}
