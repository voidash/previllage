//! Health evaluator — read recent `poll_cycles`, decide a [`HealthVerdict`],
//! and (via [`Store::upsert_source_health`]) persist a snapshot to the
//! `source_health` table.
//!
//! ## Verdict rules
//!
//! Order of evaluation matters; first match wins:
//!
//! 1. **`ShellDetected`** if `source.status == JsOnly` already, OR the
//!    most-recent cycle has `shell_flagged = true`. The worker has already
//!    flipped the source to `js_only` in this case; the verdict is just an
//!    audit trail.
//! 2. **`Dead`** if `source.consecutive_failures >= DEAD_CONSECUTIVE_FAILURES`.
//!    The fetcher has bottomed out on this source repeatedly with nothing
//!    coming back — domain probably expired or returned 410 permanently.
//! 3. **`InsufficientData`** if there are fewer than `MIN_CYCLES_FOR_VERDICT`
//!    cycles in the window. We refuse to call a brand-new source structurally
//!    failed.
//! 4. **`DormantBlocked`** if the error rate over the window meets
//!    `DORMANT_ERROR_RATE`. Indicates WAF / robots-deny / blanket 403.
//! 5. **`StructurallyFailed`** if the most-recent cycles form a non-productive
//!    streak of length ≥ `STRUCTURAL_FAILURE_STREAK` AND the source has
//!    historically inserted ≥ `PREVIOUSLY_PRODUCTIVE_THRESHOLD` documents.
//!    Without the precondition we'd false-positive on sources that have
//!    been empty since day 1 (just badly-seeded). With it, we only fire on
//!    sources that *used* to work and now don't.
//! 6. Otherwise **`Healthy`**.
//!
//! ## "Non-productive" cycle, defined
//!
//! A cycle is non-productive when:
//!   - the site was responsive: `html_fetched + binaries_fetched > 0`
//!   - and yet no docs changed: `docs_inserted == 0 && docs_superseded == 0`.
//!
//! `docs_unchanged > 0` is fine — that's the signature of a fully-crawled,
//! stable site. The bad case is "we fetched HTML but produced no NEW or
//! SUPERSEDED row," which suggests every page is a wrapper / soft-404 / etc.
//!
//! ## Pure verdict logic
//!
//! [`decide_verdict`] is a pure function over its inputs — easy to unit-test
//! without an SQLite handle. [`evaluate_health`] is the I/O wrapper that
//! pulls inputs from the [`Store`].

use super::store::{Store, StoreError};
use super::types::{PollCycle, Source, SourceStatus};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

/// Default rolling window for health analysis. PIPELINE.md cadences run
/// 6h–48h, so 7 days gives every tier ≥ 3 cycles in the typical case.
pub const DEFAULT_WINDOW_DAYS: i64 = 7;

/// Minimum cycles in window before we'll commit to a non-Healthy verdict.
pub const MIN_CYCLES_FOR_VERDICT: usize = 3;

/// Consecutive non-productive cycles that flip a source to StructurallyFailed.
pub const STRUCTURAL_FAILURE_STREAK: u32 = 5;

/// Total docs_inserted (over all history) below which a source is considered
/// "never productive" — it gets InsufficientData / Healthy rather than
/// StructurallyFailed.
pub const PREVIOUSLY_PRODUCTIVE_THRESHOLD: u64 = 5;

/// `consecutive_failures` ceiling that flips a source to Dead.
pub const DEAD_CONSECUTIVE_FAILURES: u32 = 20;

/// Fraction of cycles in window where errors >= fetched-content; ≥ this
/// flips a source to DormantBlocked.
pub const DORMANT_ERROR_RATE: f64 = 0.8;

/// What the health evaluator decided. The verdict drives downstream
/// actions — repair-queue insertion (StructurallyFailed) or status-flip
/// (Dead / DormantBlocked).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HealthVerdict {
    Healthy,
    InsufficientData,
    StructurallyFailed { reason: String },
    DormantBlocked { reason: String },
    Dead { reason: String },
    ShellDetected,
}

impl HealthVerdict {
    pub fn is_structural_failure(&self) -> bool {
        matches!(self, HealthVerdict::StructurallyFailed { .. })
    }

    /// Free-text reason if the verdict carries one (for logging / source_health.failure_reason).
    pub fn reason(&self) -> Option<&str> {
        match self {
            HealthVerdict::StructurallyFailed { reason }
            | HealthVerdict::DormantBlocked { reason }
            | HealthVerdict::Dead { reason } => Some(reason),
            _ => None,
        }
    }

    /// Lowercase tag for source_health columns and admin output.
    pub fn tag(&self) -> &'static str {
        match self {
            HealthVerdict::Healthy => "healthy",
            HealthVerdict::InsufficientData => "insufficient_data",
            HealthVerdict::StructurallyFailed { .. } => "structurally_failed",
            HealthVerdict::DormantBlocked { .. } => "dormant_blocked",
            HealthVerdict::Dead { .. } => "dead",
            HealthVerdict::ShellDetected => "shell_detected",
        }
    }
}

/// Snapshot of a source's recent operational shape. Persisted into
/// `source_health` (one row per source, latest evaluation only — history
/// is reconstructable from the `poll_cycles` table).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HealthMetrics {
    pub source_id: String,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub n_cycles: u32,
    pub successes: u32,
    pub empty_extractions: u32,
    pub error_rate: f64,
    pub content_hash_change_rate: f64,
    pub structural_failure_streak: u32,
    pub verdict: HealthVerdict,
}

/// Pull cycles + history for `source` and decide the verdict. Pure-input
/// function (no I/O on success path beyond two SELECTs).
pub fn evaluate_health(
    store: &Store,
    source: &Source,
    window: Duration,
    now: DateTime<Utc>,
) -> Result<HealthMetrics, StoreError> {
    let window_start = now - window;
    // Newest-first; passing `limit = 0` for "no cap".
    let cycles = store.list_poll_cycles_for_source(&source.source_id, window_start, 0)?;
    let total_docs_ever = store.total_docs_inserted_for_source(&source.source_id)?;

    let mut metrics = compute_metrics(&source.source_id, window_start, now, &cycles);
    metrics.verdict = decide_verdict(
        &cycles,
        source.consecutive_failures,
        total_docs_ever,
        source.status,
    );
    Ok(metrics)
}

/// Pure verdict computation. Cycles must be sorted **newest-first**.
pub fn decide_verdict(
    cycles: &[PollCycle],
    consecutive_failures: u32,
    total_docs_ever: u64,
    current_status: SourceStatus,
) -> HealthVerdict {
    // 1. Shell-detected wins outright. The worker already flipped the source
    //    to JsOnly; the chromiumoxide path takes over next cycle.
    if current_status == SourceStatus::JsOnly {
        return HealthVerdict::ShellDetected;
    }
    if let Some(last) = cycles.first() {
        if last.shell_flagged {
            return HealthVerdict::ShellDetected;
        }
    }

    // 2. Hard dead.
    if consecutive_failures >= DEAD_CONSECUTIVE_FAILURES {
        return HealthVerdict::Dead {
            reason: format!("{consecutive_failures} consecutive cycle failures"),
        };
    }

    // 3. Not enough signal yet.
    if cycles.len() < MIN_CYCLES_FOR_VERDICT {
        return HealthVerdict::InsufficientData;
    }

    // 4. Error-dominated window → block-class problem.
    let error_dominated = cycles
        .iter()
        .filter(|c| c.errors > c.html_fetched + c.binaries_fetched)
        .count();
    let error_rate = error_dominated as f64 / cycles.len() as f64;
    if error_rate >= DORMANT_ERROR_RATE {
        return HealthVerdict::DormantBlocked {
            reason: format!(
                "{:.0}% of {} recent cycles dominated by errors",
                error_rate * 100.0,
                cycles.len()
            ),
        };
    }

    // 5. Structural failure: streak of non-productive cycles AND the source
    //    historically did emit content.
    let streak = cycles
        .iter()
        .take_while(|c| is_non_productive(c))
        .count() as u32;
    if streak >= STRUCTURAL_FAILURE_STREAK
        && total_docs_ever >= PREVIOUSLY_PRODUCTIVE_THRESHOLD
    {
        return HealthVerdict::StructurallyFailed {
            reason: format!(
                "{streak} consecutive non-productive cycles despite {total_docs_ever} historical inserts"
            ),
        };
    }

    HealthVerdict::Healthy
}

fn compute_metrics(
    source_id: &str,
    window_start: DateTime<Utc>,
    window_end: DateTime<Utc>,
    cycles: &[PollCycle],
) -> HealthMetrics {
    let n_cycles = cycles.len() as u32;

    let successes = cycles
        .iter()
        .filter(|c| c.errors == 0 || c.html_fetched + c.binaries_fetched > 0)
        .count() as u32;

    let empty_extractions = cycles
        .iter()
        .filter(|c| is_non_productive(c))
        .count() as u32;

    let error_dominated = cycles
        .iter()
        .filter(|c| c.errors > c.html_fetched + c.binaries_fetched)
        .count();
    let error_rate = if n_cycles == 0 {
        0.0
    } else {
        error_dominated as f64 / n_cycles as f64
    };

    let total_changed: u32 = cycles
        .iter()
        .map(|c| c.docs_inserted + c.docs_superseded)
        .sum();
    let total_unchanged: u32 = cycles.iter().map(|c| c.docs_unchanged).sum();
    let denom = total_changed + total_unchanged;
    let content_hash_change_rate = if denom == 0 {
        0.0
    } else {
        total_changed as f64 / denom as f64
    };

    let structural_failure_streak = cycles
        .iter()
        .take_while(|c| is_non_productive(c))
        .count() as u32;

    HealthMetrics {
        source_id: source_id.to_string(),
        window_start,
        window_end,
        n_cycles,
        successes,
        empty_extractions,
        error_rate,
        content_hash_change_rate,
        structural_failure_streak,
        // Filled in by the caller with the result of decide_verdict.
        verdict: HealthVerdict::InsufficientData,
    }
}

fn is_non_productive(c: &PollCycle) -> bool {
    let touched = c.html_fetched + c.binaries_fetched > 0;
    let no_change = c.docs_inserted == 0 && c.docs_superseded == 0;
    touched && no_change
}

// ----- unit tests on the pure verdict logic -----------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn cyc(
        offset_min: i64,
        html_fetched: u32,
        errors: u32,
        inserted: u32,
        superseded: u32,
        unchanged: u32,
        shell_flagged: bool,
    ) -> PollCycle {
        let t = Utc.with_ymd_and_hms(2026, 4, 28, 12, 0, 0).unwrap()
            + Duration::minutes(-offset_min);
        PollCycle {
            cycle_id: None,
            source_id: "x".into(),
            started_at: t,
            finished_at: t + Duration::seconds(30),
            stop_reason: "frontier_drained".into(),
            elapsed_sec: 30,
            html_fetched,
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
    fn healthy_when_recent_cycles_show_change() {
        let cycles = vec![
            cyc(10, 10, 0, 2, 0, 8, false),
            cyc(70, 10, 0, 0, 1, 9, false),
            cyc(130, 10, 0, 1, 0, 9, false),
            cyc(190, 10, 0, 0, 0, 10, false), // unchanged-only is fine
            cyc(250, 10, 0, 1, 0, 9, false),
        ];
        let v = decide_verdict(&cycles, 0, 100, SourceStatus::Active);
        assert_eq!(v, HealthVerdict::Healthy);
    }

    #[test]
    fn insufficient_data_below_three_cycles() {
        let cycles = vec![cyc(10, 10, 0, 1, 0, 9, false), cyc(70, 10, 0, 0, 0, 10, false)];
        let v = decide_verdict(&cycles, 0, 50, SourceStatus::Active);
        assert_eq!(v, HealthVerdict::InsufficientData);
    }

    #[test]
    fn dead_at_twenty_consecutive_failures() {
        let v = decide_verdict(&[], 20, 100, SourceStatus::Active);
        assert!(matches!(v, HealthVerdict::Dead { .. }));
    }

    #[test]
    fn dead_overrides_insufficient_data() {
        // Even with no recent cycles, hard-dead trips on consecutive_failures.
        let v = decide_verdict(&[], 25, 0, SourceStatus::Active);
        assert!(matches!(v, HealthVerdict::Dead { .. }));
    }

    #[test]
    fn shell_detected_via_status() {
        let v = decide_verdict(&[], 0, 0, SourceStatus::JsOnly);
        assert_eq!(v, HealthVerdict::ShellDetected);
    }

    #[test]
    fn shell_detected_via_last_cycle_flag() {
        let cycles = vec![cyc(5, 1, 0, 0, 0, 0, true)];
        let v = decide_verdict(&cycles, 0, 0, SourceStatus::Active);
        assert_eq!(v, HealthVerdict::ShellDetected);
    }

    #[test]
    fn dormant_blocked_when_errors_dominate() {
        // 5 cycles where errors > content fetched.
        let cycles = (0..5)
            .map(|i| cyc((i * 60) as i64, 0, 5, 0, 0, 0, false))
            .collect::<Vec<_>>();
        let v = decide_verdict(&cycles, 0, 100, SourceStatus::Active);
        assert!(matches!(v, HealthVerdict::DormantBlocked { .. }));
    }

    #[test]
    fn structurally_failed_at_five_non_productive_with_history() {
        let cycles = (0..5)
            .map(|i| cyc((i * 60) as i64, 10, 0, 0, 0, 10, false))
            .collect::<Vec<_>>();
        let v = decide_verdict(&cycles, 0, 100, SourceStatus::Active);
        assert!(matches!(v, HealthVerdict::StructurallyFailed { .. }));
    }

    #[test]
    fn not_structurally_failed_without_history() {
        // 5 non-productive cycles but the source has only 1 ever-inserted doc.
        let cycles = (0..5)
            .map(|i| cyc((i * 60) as i64, 10, 0, 0, 0, 10, false))
            .collect::<Vec<_>>();
        let v = decide_verdict(&cycles, 0, 1, SourceStatus::Active);
        // Below PREVIOUSLY_PRODUCTIVE_THRESHOLD → falls through to Healthy.
        assert_eq!(v, HealthVerdict::Healthy);
    }

    #[test]
    fn streak_breaks_on_productive_cycle() {
        // Last cycle is productive (newest first) → streak = 0.
        let cycles = vec![
            cyc(10, 10, 0, 1, 0, 9, false),  // productive (newest)
            cyc(70, 10, 0, 0, 0, 10, false), // non-productive
            cyc(130, 10, 0, 0, 0, 10, false),
            cyc(190, 10, 0, 0, 0, 10, false),
            cyc(250, 10, 0, 0, 0, 10, false),
        ];
        let v = decide_verdict(&cycles, 0, 100, SourceStatus::Active);
        assert_eq!(v, HealthVerdict::Healthy);
    }
}
