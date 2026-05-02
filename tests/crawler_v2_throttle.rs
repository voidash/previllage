//! Tests for crawler_v2::throttle — per-domain politeness.

use gemma_god::crawler_v2::throttle::{DomainThrottle, ThrottleConfig};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fast_throttle(min_ms: u64) -> DomainThrottle {
    DomainThrottle::new(ThrottleConfig {
        min_interval: Duration::from_millis(min_ms),
        jitter: Duration::from_millis(0),
    })
}

#[tokio::test(flavor = "current_thread")]
async fn first_call_does_not_wait() {
    let t = fast_throttle(200);
    let t0 = Instant::now();
    let _permit = t.wait("a.gov.np").await;
    // Allow 30ms slop for test-env scheduling.
    assert!(
        t0.elapsed() < Duration::from_millis(30),
        "first wait took {:?}",
        t0.elapsed()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn second_call_same_domain_respects_min_interval() {
    let t = fast_throttle(200);
    {
        let _p1 = t.wait("a.gov.np").await;
    } // permit dropped here
    let t0 = Instant::now();
    let _p2 = t.wait("a.gov.np").await;
    let elapsed = t0.elapsed();
    assert!(
        elapsed >= Duration::from_millis(180),
        "second wait only took {elapsed:?}, expected >=180ms"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn different_domains_independent() {
    let t = fast_throttle(200);
    let _p1 = t.wait("a.gov.np").await;
    let t0 = Instant::now();
    let _p2 = t.wait("b.gov.np").await;
    assert!(
        t0.elapsed() < Duration::from_millis(30),
        "different-domain wait blocked: {:?}",
        t0.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_same_domain_serializes() {
    let t = Arc::new(fast_throttle(150));

    let t_a = t.clone();
    let a = tokio::spawn(async move {
        let _permit = t_a.wait("s.gov.np").await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        Instant::now()
    });

    // Small stagger so A gets its permit first.
    tokio::time::sleep(Duration::from_millis(10)).await;

    let t_b = t.clone();
    let b = tokio::spawn(async move {
        let start = Instant::now();
        let _permit = t_b.wait("s.gov.np").await;
        (start, Instant::now())
    });

    let a_end = a.await.unwrap();
    let (b_start, b_acquired) = b.await.unwrap();

    // B must wait for A to release + min_interval.
    assert!(
        b_acquired >= a_end,
        "B acquired before A released: a_end={a_end:?} b_acq={b_acquired:?}",
    );
    // Min wait should be ~150ms (min_interval). Allow slack for test-runner
    // scheduling: treat anything >= 120ms as "waited the expected amount".
    assert!(
        b_acquired - b_start >= Duration::from_millis(120),
        "B waited only {:?}, expected >= 120ms",
        b_acquired - b_start
    );
}
