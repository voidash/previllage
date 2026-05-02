//! Per-source priority frontier with seen-set deduplication.
//!
//! The worker owns one [`Frontier`] per source, seeded from the source's
//! existing manifest (to preserve resumability across daemon restarts) and
//! grown by link discovery during the crawl.
//!
//! Ordering: `BinaryHeap` is a max-heap on [`FrontierItem::cmp`]. Items with
//! the highest [`crate::crawler_v2::url::score`] pop first; ties break by
//! shallower depth, then by lexical URL (for deterministic ordering, which
//! matters in tests).

use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

/// One entry in the frontier: a canonicalized URL to fetch, its BFS depth
/// from the entry point, and its priority score at time-of-enqueue.
///
/// Score is captured at enqueue time rather than recomputed at pop time so
/// that a recipe change mid-crawl doesn't reshuffle the queue unexpectedly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontierItem {
    pub url: String,
    pub depth: u32,
    pub score: i32,
}

impl Ord for FrontierItem {
    fn cmp(&self, other: &Self) -> Ordering {
        // Max-heap contract: `a > b` ⇒ a pops first.
        // Primary key: score descending (via natural Ord on i32).
        // Tie-break 1: depth ascending (shallower preferred — reverse via
        //              `other.depth.cmp(&self.depth)`).
        // Tie-break 2: url ascending (deterministic; reverse via
        //              `other.url.cmp(&self.url)`).
        self.score
            .cmp(&other.score)
            .then_with(|| other.depth.cmp(&self.depth))
            .then_with(|| other.url.cmp(&self.url))
    }
}

impl PartialOrd for FrontierItem {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Default)]
pub struct Frontier {
    pq: BinaryHeap<FrontierItem>,
    seen: HashSet<String>,
}

impl Frontier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Construct with a pre-populated seen-set. Used when resuming from a
    /// stored manifest — the worker loads every previously-fetched URL into
    /// the seen-set so re-runs don't re-enqueue them.
    pub fn with_seen(seen: HashSet<String>) -> Self {
        Self {
            pq: BinaryHeap::new(),
            seen,
        }
    }

    /// Enqueue a canonical URL. Returns `true` if newly added, `false` if
    /// the URL was already seen (and therefore skipped).
    pub fn push(&mut self, url: String, depth: u32, score: i32) -> bool {
        if !self.seen.insert(url.clone()) {
            return false;
        }
        self.pq.push(FrontierItem { url, depth, score });
        true
    }

    /// Pop the highest-priority item. None when drained.
    pub fn pop(&mut self) -> Option<FrontierItem> {
        self.pq.pop()
    }

    pub fn peek(&self) -> Option<&FrontierItem> {
        self.pq.peek()
    }

    pub fn is_empty(&self) -> bool {
        self.pq.is_empty()
    }

    pub fn len(&self) -> usize {
        self.pq.len()
    }

    pub fn seen_count(&self) -> usize {
        self.seen.len()
    }

    /// Record a URL as seen without enqueuing it. Used when a URL came out
    /// of the DB manifest rather than from link discovery — we know we've
    /// handled it, we just don't want to re-visit.
    pub fn mark_seen(&mut self, url: String) -> bool {
        self.seen.insert(url)
    }

    pub fn contains(&self, url: &str) -> bool {
        self.seen.contains(url)
    }
}
