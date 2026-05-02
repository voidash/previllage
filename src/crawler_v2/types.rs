//! Core types shared across crawler_v2.
//!
//! Kept intentionally flat: no business logic here, just data shapes that
//! round-trip via serde (JSONL registry) and rusqlite (index.db).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Authority tier driving poll cadence (see PIPELINE.md).
///
/// 1 = constitutional/legal, 2 = ministries/regulators, 3 = departments,
/// 4 = province-level, 5 = local palikas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Tier(pub u8);

impl Tier {
    /// Default poll interval in hours for this tier (PIPELINE.md table).
    /// Overrideable per-source via recipe.
    pub fn default_poll_hours(self) -> u32 {
        match self.0 {
            1 => 6,
            2 => 12,
            3 => 24,
            4 => 84, // 3.5 days
            _ => 48, // tier 5 + unknown
        }
    }
}

impl fmt::Display for Tier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "T{}", self.0)
    }
}

/// Lifecycle of a source as tracked by the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    /// Healthy; scheduler polls per cadence.
    Active,
    /// Temporarily out — WAF blocked us, robots newly forbade, etc. Don't poll
    /// until operator/agent flips back to Active.
    Dormant,
    /// Permanently out — 410, domain expired, too many consecutive failures.
    Dead,
    /// Shell-detected on first fetch. Next cycle uses chromiumoxide path.
    JsOnly,
}

impl SourceStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceStatus::Active => "active",
            SourceStatus::Dormant => "dormant",
            SourceStatus::Dead => "dead",
            SourceStatus::JsOnly => "js_only",
        }
    }

    pub fn from_str(s: &str) -> Option<SourceStatus> {
        match s {
            "active" => Some(SourceStatus::Active),
            "dormant" => Some(SourceStatus::Dormant),
            "dead" => Some(SourceStatus::Dead),
            "js_only" => Some(SourceStatus::JsOnly),
            _ => None,
        }
    }
}

/// What kind of document we fetched. Unknown types flag as `Other`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DocType {
    Html,
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    Txt,
    Other,
}

impl DocType {
    pub fn as_str(self) -> &'static str {
        match self {
            DocType::Html => "html",
            DocType::Pdf => "pdf",
            DocType::Docx => "docx",
            DocType::Xlsx => "xlsx",
            DocType::Pptx => "pptx",
            DocType::Txt => "txt",
            DocType::Other => "other",
        }
    }

    pub fn from_str(s: &str) -> Option<DocType> {
        match s {
            "html" => Some(DocType::Html),
            "pdf" => Some(DocType::Pdf),
            "docx" => Some(DocType::Docx),
            "xlsx" => Some(DocType::Xlsx),
            "pptx" => Some(DocType::Pptx),
            "txt" => Some(DocType::Txt),
            "other" => Some(DocType::Other),
            _ => None,
        }
    }
}

/// A gov source we poll. Seeded from sources_tiered.jsonl, then mutated by
/// the daemon (next_poll_at, consecutive_failures, status transitions).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub source_id: String,
    pub domain: String,
    pub homepage_url: String,
    #[serde(default)]
    pub name_en: Option<String>,
    #[serde(default)]
    pub name_np: Option<String>,
    #[serde(default)]
    pub office_type: Option<String>,
    #[serde(default)]
    pub province: Option<String>,
    pub tier: Tier,
    pub poll_interval_hours: u32,
    pub status: SourceStatus,
    pub first_seen: DateTime<Utc>,
    #[serde(default)]
    pub last_polled_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_changed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_failure_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default)]
    pub next_poll_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub notes: Option<String>,
}

/// A fetched document (one version). Content-hashed; changes produce new
/// rows that set the previous row's `superseded_by`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Document {
    pub doc_id: String,
    pub source_id: String,
    pub url: String,
    pub content_hash: String,
    pub fetched_at: DateTime<Utc>,
    pub superseded_by: Option<String>,
    pub removed_at: Option<DateTime<Utc>>,
    pub doc_type: DocType,
    pub status_code: i32,
    pub title: Option<String>,
    pub language: Option<String>,
    pub date_published: Option<String>, // kept as string; site formats vary
    pub raw_blob_path: String,
    pub extracted_text_path: Option<String>,
    pub text_chars: u32,
    pub size_bytes: u64,
    pub depth: u32,
    pub priority_at_fetch: Option<i32>,
}

/// Append-only log of each fetch attempt for diagnostics + health stats.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchEvent {
    pub source_id: String,
    pub url: String,
    pub fetched_at: DateTime<Utc>,
    pub status: i32,
    pub elapsed_ms: Option<u32>,
    pub error: Option<String>,
    pub doc_type: Option<DocType>,
    pub bytes: Option<u64>,
}

/// Persisted record of a single poll cycle (mirrors `WorkerReport`).
///
/// Health evaluation reads rolling windows of these to decide whether a
/// previously-productive source has gone structurally broken (HTTP 200 but
/// 0 docs for ≥5 cycles → enqueue repair).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PollCycle {
    /// `None` before insert; assigned by SQLite on insert.
    pub cycle_id: Option<i64>,
    pub source_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub stop_reason: String,
    pub elapsed_sec: u64,
    pub html_fetched: u32,
    pub binaries_fetched: u32,
    pub other_fetched: u32,
    pub errors: u32,
    pub docs_inserted: u32,
    pub docs_superseded: u32,
    pub docs_unchanged: u32,
    pub shell_flagged: bool,
}

/// Lifecycle of a repair-queue entry. Status flows roughly:
///   Pending → Dispatched → (Applied | HumanReview | Deadletter)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairStatus {
    /// Health evaluator has flagged the source; awaiting agent dispatch.
    Pending,
    /// Agent runtime invoked; awaiting agent's structured-JSON output.
    Dispatched,
    /// Proposed recipe passed dry-run; auto-committed (tiers 3-5).
    Applied,
    /// Proposed recipe is plausible but tier 1-2 requires manual review.
    HumanReview,
    /// Agent failed, dry-run failed, or proposal was identical to current.
    Deadletter,
}

impl RepairStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RepairStatus::Pending => "pending",
            RepairStatus::Dispatched => "dispatched",
            RepairStatus::Applied => "applied",
            RepairStatus::HumanReview => "human_review",
            RepairStatus::Deadletter => "deadletter",
        }
    }

    pub fn from_str(s: &str) -> Option<RepairStatus> {
        match s {
            "pending" => Some(RepairStatus::Pending),
            "dispatched" => Some(RepairStatus::Dispatched),
            "applied" => Some(RepairStatus::Applied),
            "human_review" => Some(RepairStatus::HumanReview),
            "deadletter" => Some(RepairStatus::Deadletter),
            _ => None,
        }
    }
}

/// Persisted shape of one row in `source_health` (latest snapshot per source).
///
/// Distinct from [`crate::crawler_v2::health::HealthMetrics`] — that struct
/// is the in-memory result of a fresh evaluation and includes the full
/// [`crate::crawler_v2::health::HealthVerdict`]; this one only has the
/// flag-level `is_structural_failure` bit + free-text `failure_reason` we
/// can faithfully round-trip through SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersistedHealth {
    pub source_id: String,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub fetches: u32,
    pub successes: u32,
    pub empty_extractions: u32,
    pub avg_text_chars: f64,
    pub content_hash_change_rate: f64,
    pub is_structural_failure: bool,
    pub failure_reason: Option<String>,
}

/// One entry in the repair queue. The agent dispatcher drains pending entries,
/// invokes the configured agent runtime (claude-code etc.) with failure
/// evidence + recipe schema + 3 example recipes, captures the agent's
/// structured-JSON proposal, and dry-runs it before applying.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepairItem {
    /// `None` before insert; assigned by SQLite.
    pub queue_id: Option<i64>,
    pub source_id: String,
    pub queued_at: DateTime<Utc>,
    pub status: RepairStatus,
    pub dispatched_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    /// JSON-serialized failure evidence (recent poll cycles + verdict reason).
    pub failure_evidence: String,
    /// Path (corpus-root-relative) to a fresh HTML snapshot the agent works against.
    pub sample_html_path: Option<String>,
    /// Agent's proposed recipe JSON (whole-file content), if dispatch succeeded.
    pub proposed_recipe: Option<String>,
    /// JSON of the dry-run scoring (extracted_doc_count, junk_score, etc.).
    pub dry_run_result: Option<String>,
    /// `auto` | `human_review` | `deadletter` — the apply gate's decision.
    pub apply_outcome: Option<String>,
    /// If deadletter, the reason. Otherwise `None`.
    pub error_log: Option<String>,
}

/// Schema for the human-edited `corpora/sources_tiered.jsonl` file. Slightly
/// looser than [`Source`] because upstream ingest scripts use `tier_guess`
/// and don't write all lifecycle fields.
#[derive(Debug, Clone, Deserialize)]
pub struct RegistryRow {
    pub source_id: String,
    pub domain: String,
    pub homepage_url: String,
    #[serde(default)]
    pub name_en: Option<String>,
    #[serde(default)]
    pub name_np: Option<String>,
    #[serde(default)]
    pub office_type: Option<String>,
    #[serde(default)]
    pub province: Option<String>,
    #[serde(alias = "tier_guess")]
    pub tier: u8,
    #[serde(default)]
    pub poll_interval_hours: Option<u32>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub first_seen: Option<String>,
}
