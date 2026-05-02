//! Integration tests for the health evaluator wired through Store I/O:
//! seed `sources` + `poll_cycles`, call `evaluate_health`, assert the verdict
//! and persisted snapshot.

use chrono::{Duration, Utc};
use gemma_god::crawler_v2::types::{PollCycle, RegistryRow, SourceStatus};
use gemma_god::crawler_v2::{evaluate_health, HealthVerdict, Store};

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

fn cyc(
    sid: &str,
    started: chrono::DateTime<Utc>,
    html: u32,
    errors: u32,
    inserted: u32,
    superseded: u32,
    unchanged: u32,
    shell_flagged: bool,
) -> PollCycle {
    PollCycle {
        cycle_id: None,
        source_id: sid.into(),
        started_at: started,
        finished_at: started + Duration::seconds(30),
        stop_reason: if shell_flagged {
            "shell_detected".into()
        } else {
            "frontier_drained".into()
        },
        elapsed_sec: 30,
        html_fetched: html,
        binaries_fetched: 0,
        other_fetched: 0,
        errors,
        docs_inserted: inserted,
        docs_superseded: superseded,
        docs_unchanged: unchanged,
        shell_flagged,
    }
}

#[test]
fn healthy_source_yields_healthy_verdict_and_persists() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    let now = Utc::now();
    for i in 0..5 {
        store
            .insert_poll_cycle(&cyc(
                "moha",
                now - Duration::hours(i * 12),
                10,
                0,
                if i == 0 { 1 } else { 0 },
                0,
                10,
                false,
            ))
            .unwrap();
    }
    let source = store.get_source("moha").unwrap().unwrap();
    let metrics = evaluate_health(&store, &source, Duration::days(7), now).unwrap();
    assert_eq!(metrics.verdict, HealthVerdict::Healthy);
    assert_eq!(metrics.n_cycles, 5);
    // Persistence round-trip.
    store.upsert_source_health(&metrics).unwrap();
    let persisted = store.get_source_health("moha").unwrap().unwrap();
    assert!(!persisted.is_structural_failure);
    assert_eq!(persisted.fetches, 5);
    assert!(persisted.failure_reason.is_none());
}

#[test]
fn structurally_failed_source_persists_with_reason() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    let now = Utc::now();

    // 6 productive cycles in deep history, building total_docs_inserted = 6.
    for i in 5..11 {
        store
            .insert_poll_cycle(&cyc(
                "moha",
                now - Duration::days(i),
                10,
                0,
                1,
                0,
                9,
                false,
            ))
            .unwrap();
    }
    // 5 recent non-productive cycles inside the 7-day window.
    for i in 0..5 {
        store
            .insert_poll_cycle(&cyc(
                "moha",
                now - Duration::hours(i * 12),
                10,
                0,
                0,
                0,
                10,
                false,
            ))
            .unwrap();
    }

    let source = store.get_source("moha").unwrap().unwrap();
    let metrics = evaluate_health(&store, &source, Duration::days(7), now).unwrap();
    assert!(metrics.verdict.is_structural_failure(), "verdict={:?}", metrics.verdict);
    assert_eq!(metrics.structural_failure_streak, 5);

    store.upsert_source_health(&metrics).unwrap();
    let persisted = store.get_source_health("moha").unwrap().unwrap();
    assert!(persisted.is_structural_failure);
    let reason = persisted.failure_reason.unwrap();
    assert!(reason.contains("non-productive"), "reason={reason:?}");
    assert!(reason.contains("historical"), "reason={reason:?}");
}

#[test]
fn js_only_source_yields_shell_detected_regardless_of_cycles() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "palika");
    let now = Utc::now();
    // Even with healthy-looking cycles, status=JsOnly wins.
    for i in 0..3 {
        store
            .insert_poll_cycle(&cyc(
                "palika",
                now - Duration::hours(i * 12),
                5,
                0,
                1,
                0,
                4,
                false,
            ))
            .unwrap();
    }
    store
        .mark_source_status("palika", SourceStatus::JsOnly)
        .unwrap();

    let source = store.get_source("palika").unwrap().unwrap();
    let metrics = evaluate_health(&store, &source, Duration::days(7), now).unwrap();
    assert_eq!(metrics.verdict, HealthVerdict::ShellDetected);
}

#[test]
fn out_of_window_cycles_are_excluded() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    let now = Utc::now();

    // Two cycles inside the window, but both productive → not enough for
    // any negative verdict; stays Healthy. (Verifies that ancient cycles
    // outside the window aren't accidentally pulled in to push us past
    // MIN_CYCLES_FOR_VERDICT.)
    for i in 0..2 {
        store
            .insert_poll_cycle(&cyc(
                "moha",
                now - Duration::hours(i * 12),
                10,
                0,
                1,
                0,
                9,
                false,
            ))
            .unwrap();
    }
    // Many older cycles outside the 7-day window.
    for i in 0..20 {
        store
            .insert_poll_cycle(&cyc(
                "moha",
                now - Duration::days(30 + i),
                10,
                0,
                1,
                0,
                9,
                false,
            ))
            .unwrap();
    }

    let source = store.get_source("moha").unwrap().unwrap();
    let metrics = evaluate_health(&store, &source, Duration::days(7), now).unwrap();
    // Only 2 cycles in window → InsufficientData.
    assert_eq!(metrics.verdict, HealthVerdict::InsufficientData);
    assert_eq!(metrics.n_cycles, 2);
}

#[test]
fn upsert_source_health_replaces_prior_snapshot() {
    let store = Store::open_in_memory().unwrap();
    seed_source(&store, "moha");
    let now = Utc::now();
    for i in 0..3 {
        store
            .insert_poll_cycle(&cyc(
                "moha",
                now - Duration::hours(i * 12),
                10,
                0,
                1,
                0,
                9,
                false,
            ))
            .unwrap();
    }
    let source = store.get_source("moha").unwrap().unwrap();

    // First evaluation: Healthy.
    let m1 = evaluate_health(&store, &source, Duration::days(7), now).unwrap();
    store.upsert_source_health(&m1).unwrap();
    assert!(!store.get_source_health("moha").unwrap().unwrap().is_structural_failure);

    // Force a structural-failure verdict in-memory and re-upsert.
    let mut m2 = m1.clone();
    m2.verdict = HealthVerdict::StructurallyFailed {
        reason: "synthetic".into(),
    };
    store.upsert_source_health(&m2).unwrap();

    let p = store.get_source_health("moha").unwrap().unwrap();
    assert!(p.is_structural_failure);
    assert_eq!(p.failure_reason.as_deref(), Some("synthetic"));
}
