//! Integration tests for crawler_v2::frontier — priority queue + seen-set.

use gemma_god::crawler_v2::frontier::{Frontier, FrontierItem};
use std::collections::HashSet;

#[test]
fn pop_order_is_score_descending() {
    let mut f = Frontier::new();
    f.push("https://x/a".into(), 0, 100);
    f.push("https://x/b".into(), 0, 130);
    f.push("https://x/c".into(), 0, 110);

    let order: Vec<_> = std::iter::from_fn(|| f.pop().map(|i| i.score)).collect();
    assert_eq!(order, vec![130, 110, 100]);
}

#[test]
fn tie_on_score_breaks_by_shallower_depth() {
    let mut f = Frontier::new();
    f.push("https://x/a".into(), 5, 100);
    f.push("https://x/b".into(), 0, 100);
    f.push("https://x/c".into(), 3, 100);

    let depths: Vec<_> = std::iter::from_fn(|| f.pop().map(|i| i.depth)).collect();
    assert_eq!(depths, vec![0, 3, 5]);
}

#[test]
fn tie_on_score_and_depth_breaks_lexically() {
    let mut f = Frontier::new();
    // Same score + depth → lex-ascending url order.
    f.push("https://x/z".into(), 0, 100);
    f.push("https://x/a".into(), 0, 100);
    f.push("https://x/m".into(), 0, 100);

    let urls: Vec<_> = std::iter::from_fn(|| f.pop().map(|i| i.url)).collect();
    assert_eq!(
        urls,
        vec!["https://x/a", "https://x/m", "https://x/z"],
    );
}

#[test]
fn duplicate_push_returns_false_and_does_not_double_enqueue() {
    let mut f = Frontier::new();
    assert!(f.push("https://x/a".into(), 0, 100));
    assert!(!f.push("https://x/a".into(), 0, 100));
    assert_eq!(f.len(), 1);
    assert!(f.contains("https://x/a"));
}

#[test]
fn with_seen_prevents_reenqueue_of_known_urls() {
    // Simulates resuming from a manifest: URLs previously fetched are loaded
    // as seen; link-discovery then tries to enqueue them and gets rejected.
    let mut seen = HashSet::new();
    seen.insert("https://x/alpha".to_string());
    let mut f = Frontier::with_seen(seen);

    assert!(!f.push("https://x/alpha".into(), 0, 100)); // blocked
    assert!(f.push("https://x/beta".into(), 0, 100)); // fresh
    assert_eq!(f.len(), 1);
}

#[test]
fn mark_seen_records_without_enqueue() {
    let mut f = Frontier::new();
    assert!(f.mark_seen("https://x/a".into()));
    assert_eq!(f.len(), 0);
    assert!(f.contains("https://x/a"));
    // Subsequent push is blocked.
    assert!(!f.push("https://x/a".into(), 0, 100));
}

#[test]
fn frontier_item_ordering_is_total_and_deterministic() {
    // Build a concrete set of items and sort them; compare to expected order.
    let items = vec![
        FrontierItem { url: "https://x/c".into(), depth: 2, score: 100 },
        FrontierItem { url: "https://x/a".into(), depth: 2, score: 100 },
        FrontierItem { url: "https://x/b".into(), depth: 1, score: 100 },
        FrontierItem { url: "https://x/d".into(), depth: 2, score: 130 },
    ];
    let mut v = items.clone();
    // BinaryHeap pop order == sort descending via Ord.
    v.sort_by(|a, b| b.cmp(a));
    let urls: Vec<_> = v.iter().map(|i| i.url.clone()).collect();
    assert_eq!(
        urls,
        vec!["https://x/d", "https://x/b", "https://x/a", "https://x/c"],
    );
}

#[test]
fn peek_matches_next_pop() {
    let mut f = Frontier::new();
    f.push("https://x/a".into(), 0, 100);
    f.push("https://x/b".into(), 0, 130);
    let peeked = f.peek().unwrap().url.clone();
    let popped = f.pop().unwrap().url;
    assert_eq!(peeked, popped);
}

#[test]
fn empty_and_len_track_state() {
    let mut f = Frontier::new();
    assert!(f.is_empty());
    assert_eq!(f.len(), 0);
    f.push("https://x/a".into(), 0, 100);
    assert!(!f.is_empty());
    assert_eq!(f.len(), 1);
    f.pop();
    assert!(f.is_empty());
}

#[test]
fn seen_count_includes_pushed_and_marked() {
    let mut f = Frontier::new();
    f.push("https://x/a".into(), 0, 100);
    f.mark_seen("https://x/b".into());
    assert_eq!(f.seen_count(), 2);
    assert_eq!(f.len(), 1);
}
