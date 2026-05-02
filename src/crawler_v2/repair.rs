//! Repair pipeline — what the daemon does in response to a [`HealthVerdict`].
//!
//! Phase 6.2 (this commit) ships the **reaction layer**: given a freshly
//! computed [`HealthMetrics`], decide whether to flip source status, enqueue
//! an agent-repair, or do nothing. The function is split out from
//! `health.rs` so that:
//!   - health.rs stays purely read-only / decision-only;
//!   - repair.rs owns every write that follows from a verdict — the audit
//!     trail of "what we changed because of which verdict" lives here.
//!
//! Phase 6.3+ will add the actual agent-runtime adapters (claude-code,
//! opencode, codex) that drain the queue this phase produces.
//!
//! ## What the verdict translates into
//!
//! | Verdict | Reaction |
//! |---|---|
//! | `Healthy` / `InsufficientData` | nothing |
//! | `ShellDetected` | nothing — worker already flipped status to JsOnly |
//! | `Dead` | mark source `Dead` (scheduler stops polling) |
//! | `DormantBlocked` | mark source `Dormant` |
//! | `StructurallyFailed` | enqueue repair (idempotent on `has_pending_repair`) |
//!
//! The repair-enqueue path is idempotent: if the source already has a
//! pending row, we return `RepairAlreadyPending` and don't double-queue.
//! That matters because the daemon recomputes health every tick — without
//! the guard, a stuck site would accumulate dozens of identical entries.

use super::agent::{AgentContext, AgentRuntime, ExampleRecipe};
use super::health::{HealthMetrics, HealthVerdict};
use super::parse::{parse, ParsedDoc};
use super::recipe::Recipe;
use super::store::{Store, StoreError};
use super::types::{RepairItem, RepairStatus, Source, SourceStatus};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Outcome of [`react_to_verdict`]. Returned for logging / status output;
/// the daemon doesn't branch on it but ops humans want to see what happened.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReactionOutcome {
    /// Verdict was Healthy, InsufficientData, or ShellDetected — nothing to do.
    NoChange,
    /// Source status was flipped (Dead / Dormant). Carries the new status.
    StatusUpdated(SourceStatus),
    /// New repair queued. Carries the assigned `queue_id`.
    RepairEnqueued(i64),
    /// StructurallyFailed verdict but a Pending row already exists. Idempotent skip.
    RepairAlreadyPending,
}

/// Apply the verdict to the persistence layer. Pure dispatcher — does the
/// minimum write work for each branch and returns what was done.
///
/// `now` is taken as a parameter rather than `Utc::now()` so callers can
/// supply a deterministic clock in tests.
pub fn react_to_verdict(
    store: &Store,
    source: &Source,
    metrics: &HealthMetrics,
    now: DateTime<Utc>,
) -> Result<ReactionOutcome, StoreError> {
    match &metrics.verdict {
        HealthVerdict::Healthy
        | HealthVerdict::InsufficientData
        | HealthVerdict::ShellDetected => Ok(ReactionOutcome::NoChange),

        HealthVerdict::Dead { .. } => {
            store.mark_source_status(&source.source_id, SourceStatus::Dead)?;
            Ok(ReactionOutcome::StatusUpdated(SourceStatus::Dead))
        }

        HealthVerdict::DormantBlocked { .. } => {
            store.mark_source_status(&source.source_id, SourceStatus::Dormant)?;
            Ok(ReactionOutcome::StatusUpdated(SourceStatus::Dormant))
        }

        HealthVerdict::StructurallyFailed { .. } => {
            // Idempotency guard: don't double-queue.
            if store.has_pending_repair(&source.source_id)? {
                return Ok(ReactionOutcome::RepairAlreadyPending);
            }

            let evidence = build_failure_evidence(metrics);
            let sample_html_path = store
                .latest_html_doc_for_source(&source.source_id)?
                .and_then(|d| {
                    // Prefer the extracted-text sidecar (lighter for the
                    // agent to skim); fall back to raw HTML blob.
                    d.extracted_text_path.or(Some(d.raw_blob_path))
                });

            let item = RepairItem {
                queue_id: None,
                source_id: source.source_id.clone(),
                queued_at: now,
                status: RepairStatus::Pending,
                dispatched_at: None,
                completed_at: None,
                failure_evidence: evidence,
                sample_html_path,
                proposed_recipe: None,
                dry_run_result: None,
                apply_outcome: None,
                error_log: None,
            };
            let qid = store.enqueue_repair(&item)?;
            Ok(ReactionOutcome::RepairEnqueued(qid))
        }
    }
}

/// JSON-serialize a compact failure-evidence summary for the agent to read.
/// Doesn't include the recent cycles themselves — the agent has the SQLite
/// available via tooling and can query directly if it wants more depth.
fn build_failure_evidence(metrics: &HealthMetrics) -> String {
    // Hand-rolled JSON to avoid a struct just for this. Keeps the schema
    // visible at the point of construction.
    let reason = metrics
        .verdict
        .reason()
        .unwrap_or("(no reason recorded)")
        .replace('"', "\\\"");
    format!(
        "{{\"verdict\":\"{tag}\",\"reason\":\"{reason}\",\
          \"window_start\":\"{ws}\",\"window_end\":\"{we}\",\
          \"n_cycles\":{n},\"successes\":{s},\"empty_extractions\":{e},\
          \"error_rate\":{er},\"content_hash_change_rate\":{cc},\
          \"structural_failure_streak\":{sk}}}",
        tag = metrics.verdict.tag(),
        ws = metrics.window_start.to_rfc3339(),
        we = metrics.window_end.to_rfc3339(),
        n = metrics.n_cycles,
        s = metrics.successes,
        e = metrics.empty_extractions,
        er = metrics.error_rate,
        cc = metrics.content_hash_change_rate,
        sk = metrics.structural_failure_streak,
    )
}

// ---------- Dispatcher (Phase 6.4c) ------------------------------------------

#[derive(Debug, Clone)]
pub struct DispatchOptions {
    /// Canonical recipe-schema text shipped to the agent. Typically
    /// `include_str!("recipe.rs")` excerpt; the daemon CLI builds this once
    /// at startup and reuses it.
    pub recipe_schema: String,
    /// Few-shot examples for the agent. Loaded from `recipes/<sid>.json`
    /// at daemon startup; the dispatcher just forwards them.
    pub example_recipes: Vec<ExampleRecipe>,
    /// Apply-step config (recipes_dir, review_dir, git author, tier threshold).
    pub apply_options: ApplyOptions,
    /// Per-call agent timeout. Long enough that Playwright can render a few
    /// pages; short enough that a hung agent doesn't stall the daemon tick.
    pub agent_timeout: Duration,
    /// Where blob/extracted paths in the queue resolve from. Same root the
    /// worker writes to.
    pub corpus_root: PathBuf,
}

/// Result of a single `dispatch_one` invocation.
#[derive(Debug, Clone)]
pub enum DispatchOutcome {
    /// Queue was empty; nothing to do.
    NoPending,
    /// We picked up a queue row and walked it through the agent → dry-run →
    /// apply pipeline. The terminal state is encoded inside `apply_outcome`.
    Dispatched {
        queue_id: i64,
        apply_outcome: ApplyOutcome,
    },
    /// The agent itself failed (timeout, process error, malformed proposal).
    /// The queue row is now Deadletter with error_log set.
    AgentFailed { queue_id: i64, error: String },
}

#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("apply: {0}")]
    Apply(#[from] ApplyError),
}

/// Drain one pending repair item through the full pipeline:
///   1. Pick the oldest Pending row.
///   2. Mark it Dispatched.
///   3. Build the [`AgentContext`] and invoke the agent.
///   4. On agent error → mark Deadletter + return.
///   5. Read the cached sample HTML, run `dry_run_recipe`.
///   6. Run `apply_recipe` (auto-commit / human-review / deadletter).
///   7. Persist final state on the row.
///
/// "One repair per call" matches the daemon's tick semantics — the daemon
/// drains as many as it can per tick by calling this in a loop until
/// `NoPending` comes back.
///
/// **Why `db_path` and not `&Store`:** rusqlite `Connection` is `!Sync`, so
/// `&Store` held across an `.await` would make the resulting future `!Send`
/// and break `tokio::spawn` and the daemon's `Send`-bound `TickHandler`
/// trait. Opening the Store in scoped non-await blocks dodges that
/// constraint cleanly. SQLite's WAL handles the brief overlap at no cost.
pub async fn dispatch_one(
    db_path: &Path,
    agent: &dyn AgentRuntime,
    opts: &DispatchOptions,
    now: DateTime<Utc>,
) -> Result<DispatchOutcome, DispatchError> {
    // ----- Phase 1: pick a Pending row + mark it Dispatched. Store-bound. -----
    let (mut item, source) = {
        let store = Store::open(db_path)?;
        let pending = store.list_pending_repairs()?;
        let mut item = match pending.into_iter().next() {
            Some(i) => i,
            None => return Ok(DispatchOutcome::NoPending),
        };
        item.status = RepairStatus::Dispatched;
        item.dispatched_at = Some(now);
        store.update_repair(&item)?;
        let source = store
            .get_source(&item.source_id)?
            .ok_or_else(|| {
                StoreError::BadEnum(format!(
                    "repair_queue references unknown source_id={}",
                    item.source_id
                ))
            })?;
        (item, source)
    };
    let queue_id = item.queue_id.expect("Store always returns queue_id");

    // ----- Phase 2: build context, await agent. NO Store held. -----
    let current_recipe_json = match crate::crawler_v2::recipe::load_recipe_strict(
        &source,
        &opts.apply_options.repo_root.join(&opts.apply_options.recipes_dir),
    ) {
        Ok(r) => serde_json::to_string_pretty(&r).unwrap_or_else(|_| "{}".into()),
        Err(_) => serde_json::to_string_pretty(&Recipe::default_for(&source))
            .unwrap_or_else(|_| "{}".into()),
    };
    let sample_path = item
        .sample_html_path
        .as_ref()
        .map(|p| opts.corpus_root.join(p));
    let ctx = AgentContext {
        source_id: source.source_id.clone(),
        source_url: source.homepage_url.clone(),
        current_recipe_json,
        failure_evidence: item.failure_evidence.clone(),
        sample_html_path: sample_path.clone(),
        recipe_schema: opts.recipe_schema.clone(),
        example_recipes: opts.example_recipes.clone(),
        timeout: opts.agent_timeout,
    };
    let proposal = match agent.propose_recipe(&ctx).await {
        Ok(p) => p,
        Err(e) => {
            // Re-open Store to mark deadletter.
            let store = Store::open(db_path)?;
            mark_deadletter_in(&store, &mut item, &format!("agent error: {e}"), now)?;
            return Ok(DispatchOutcome::AgentFailed {
                queue_id,
                error: e.to_string(),
            });
        }
    };

    // ----- Phase 3: dry-run + apply (sync, no Store needed). -----
    item.proposed_recipe = Some(proposal.proposed_recipe_json.clone());
    let sample_bytes: Vec<u8> = match &sample_path {
        Some(p) => std::fs::read(p).unwrap_or_default(),
        None => Vec::new(),
    };
    let dry_run = dry_run_recipe(
        &proposal.proposed_recipe_json,
        &source,
        &sample_bytes,
        &source.homepage_url,
    );
    item.dry_run_result = Some(
        serde_json::to_string(&dry_run)
            .unwrap_or_else(|_| "{\"error\":\"dry_run not serializable\"}".into()),
    );
    let outcome = apply_recipe(&item, &source, &dry_run, &opts.apply_options, now)?;
    let (final_status, apply_outcome_label, error_log) = match &outcome {
        ApplyOutcome::AutoApplied { .. } => (RepairStatus::Applied, "auto", None),
        ApplyOutcome::HumanReview { .. } => (RepairStatus::HumanReview, "human_review", None),
        ApplyOutcome::Deadletter { reason } => (
            RepairStatus::Deadletter,
            "deadletter",
            Some(reason.clone()),
        ),
    };
    item.status = final_status;
    item.completed_at = Some(now);
    item.apply_outcome = Some(apply_outcome_label.to_string());
    item.error_log = error_log;

    // ----- Phase 4: finalize. Store-bound, no awaits remaining. -----
    {
        let store = Store::open(db_path)?;
        store.update_repair(&item)?;
    }

    Ok(DispatchOutcome::Dispatched {
        queue_id,
        apply_outcome: outcome,
    })
}

fn mark_deadletter_in(
    store: &Store,
    item: &mut RepairItem,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    item.status = RepairStatus::Deadletter;
    item.completed_at = Some(now);
    item.apply_outcome = Some("deadletter".into());
    item.error_log = Some(reason.to_string());
    store.update_repair(item)
}

// ---------- Apply logic (Phase 6.4b) -----------------------------------------

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub repo_root: PathBuf,
    /// Path to the recipes directory, relative to `repo_root`. Default
    /// `"recipes"`.
    pub recipes_dir: PathBuf,
    /// Path to the human-review directory, relative to `repo_root`.
    /// Tier 1-2 proposals land here as plain JSON (no commit). Default
    /// `"recipes/review"`.
    pub review_dir: PathBuf,
    /// Where to anchor the auto-applied git worktrees. Default
    /// `".repair-worktrees"`. Each commit gets its own subdirectory so
    /// operators can inspect / test the proposed recipe in isolation
    /// before merging.
    pub worktree_root: PathBuf,
    /// Stamped into `repaired_by` on the recipe and into the commit
    /// author. Identifies which agent runtime produced the proposal.
    pub agent_label: String,
    /// Tier `>= auto_apply_tier_min` → auto-applied. Lower → human review.
    /// Default 3 (palika + autonomous offices auto, ministries reviewed).
    pub auto_apply_tier_min: u8,
    /// Override git author. If `None`, the worktree's resolved git config
    /// supplies the values. The daemon should set these explicitly so that
    /// commits are clearly attributable to the agent and not to whatever
    /// shell user happens to start the daemon.
    pub git_author: Option<GitAuthor>,
}

#[derive(Debug, Clone)]
pub struct GitAuthor {
    pub name: String,
    pub email: String,
}

impl Default for ApplyOptions {
    fn default() -> Self {
        Self {
            repo_root: PathBuf::from("."),
            recipes_dir: PathBuf::from("recipes"),
            review_dir: PathBuf::from("recipes/review"),
            worktree_root: PathBuf::from(".repair-worktrees"),
            agent_label: "claude-code".into(),
            auto_apply_tier_min: 3,
            git_author: Some(GitAuthor {
                name: "crawler-repair-bot".into(),
                email: "crawler-repair@local".into(),
            }),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ApplyOutcome {
    /// Tier 3-5 + gate passed. Recipe committed to a new branch in a
    /// detached worktree; the operator merges (or deletes) at their
    /// convenience.
    AutoApplied {
        branch: String,
        commit: String,
        worktree_path: String,
        recipe_path: String,
    },
    /// Tier 1-2 (or `auto_apply_tier_min` raised). Recipe written to the
    /// review directory; no git activity.
    HumanReview { review_path: String },
    /// Gate failed in dry-run, or the proposed JSON was malformed. No file
    /// written, no git activity.
    Deadletter { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("recipe is invalid JSON: {0}")]
    InvalidRecipeJson(String),
    #[error("git command `git {args}` failed (exit {exit:?}): {stderr}")]
    Git {
        args: String,
        exit: Option<i32>,
        stderr: String,
    },
}

/// Apply (or refuse) a proposed recipe. Pure dispatch over its inputs;
/// returns `Ok(Deadletter | HumanReview | AutoApplied)`. Only fails with
/// `ApplyError` if the operation **itself** errored — the policy decision
/// of "we won't auto-apply this" is encoded in `Ok(Deadletter)`.
pub fn apply_recipe(
    item: &RepairItem,
    source: &Source,
    dry_run: &DryRunResult,
    opts: &ApplyOptions,
    now: DateTime<Utc>,
) -> Result<ApplyOutcome, ApplyError> {
    if !dry_run.passes_quality_gate {
        let reason = dry_run
            .gate_failure_reason
            .clone()
            .unwrap_or_else(|| "dry-run gate failed without a recorded reason".into());
        return Ok(ApplyOutcome::Deadletter { reason });
    }
    let proposed_json = item.proposed_recipe.as_deref().ok_or_else(|| {
        ApplyError::InvalidRecipeJson("RepairItem has no proposed_recipe".into())
    })?;

    // Parse → stamp lifecycle fields → re-serialize. The agent's JSON might
    // omit last_repaired_at/repaired_by; we always stamp them at apply time.
    let mut recipe: Recipe = serde_json::from_str(proposed_json)
        .map_err(|e| ApplyError::InvalidRecipeJson(e.to_string()))?;
    recipe.normalize(source);
    recipe.last_repaired_at = Some(now);
    recipe.repaired_by = Some(opts.agent_label.clone());
    let final_json = serde_json::to_string_pretty(&recipe)
        .map_err(|e| ApplyError::InvalidRecipeJson(e.to_string()))?;

    if source.tier.0 < opts.auto_apply_tier_min {
        write_review(source, &final_json, opts, now)
    } else {
        write_auto_apply(source, &final_json, opts, now)
    }
}

fn write_review(
    source: &Source,
    json: &str,
    opts: &ApplyOptions,
    now: DateTime<Utc>,
) -> Result<ApplyOutcome, ApplyError> {
    let ts = now.format("%Y%m%dT%H%M%S").to_string();
    let abs_review_dir = opts.repo_root.join(&opts.review_dir);
    std::fs::create_dir_all(&abs_review_dir)?;
    let filename = format!("{}.{}.json", source.source_id, ts);
    let abs_path = abs_review_dir.join(&filename);
    std::fs::write(&abs_path, json)?;

    // Return the repo-relative path so callers can render it consistently.
    let rel_path = opts.review_dir.join(&filename);
    Ok(ApplyOutcome::HumanReview {
        review_path: rel_path.to_string_lossy().into_owned(),
    })
}

fn write_auto_apply(
    source: &Source,
    json: &str,
    opts: &ApplyOptions,
    now: DateTime<Utc>,
) -> Result<ApplyOutcome, ApplyError> {
    let ts = now.format("%Y%m%dT%H%M%S").to_string();
    let branch = format!("repair/{}-{}", source.source_id, ts);
    let worktree_subdir = format!("{}-{}", source.source_id, ts);
    let abs_worktree_root = opts.repo_root.join(&opts.worktree_root);
    std::fs::create_dir_all(&abs_worktree_root)?;
    let abs_worktree_path = abs_worktree_root.join(&worktree_subdir);

    // 1. Create a fresh worktree on a new branch off HEAD. `worktree add` is
    //    the right primitive: doesn't touch the main worktree's index/HEAD.
    git(
        &opts.repo_root,
        &[
            "worktree",
            "add",
            "-b",
            &branch,
            abs_worktree_path.to_string_lossy().as_ref(),
            "HEAD",
        ],
    )?;

    // 2. Write the recipe inside that worktree.
    let abs_recipes_dir = abs_worktree_path.join(&opts.recipes_dir);
    std::fs::create_dir_all(&abs_recipes_dir)?;
    let abs_recipe_path = abs_recipes_dir.join(format!("{}.json", source.source_id));
    std::fs::write(&abs_recipe_path, json)?;

    // 3. Stage + commit. Use -c overrides for author so the daemon's
    //    commits are unambiguous regardless of the host machine's git config.
    let rel_recipe_path = opts.recipes_dir.join(format!("{}.json", source.source_id));
    git(&abs_worktree_path, &["add", rel_recipe_path.to_string_lossy().as_ref()])?;

    let mut commit_args: Vec<String> = Vec::new();
    if let Some(a) = &opts.git_author {
        commit_args.extend([
            "-c".into(),
            format!("user.name={}", a.name),
            "-c".into(),
            format!("user.email={}", a.email),
        ]);
    }
    let commit_msg = format!(
        "repair({}): agent-proposed recipe via {}",
        source.source_id, opts.agent_label
    );
    commit_args.extend(["commit".into(), "-m".into(), commit_msg]);
    let commit_args_ref: Vec<&str> = commit_args.iter().map(|s| s.as_str()).collect();
    git(&abs_worktree_path, &commit_args_ref)?;

    // 4. Capture the resulting commit SHA.
    let commit_sha = git_capture(&abs_worktree_path, &["rev-parse", "HEAD"])?
        .trim()
        .to_string();

    Ok(ApplyOutcome::AutoApplied {
        branch,
        commit: commit_sha,
        worktree_path: abs_worktree_path.to_string_lossy().into_owned(),
        recipe_path: rel_recipe_path.to_string_lossy().into_owned(),
    })
}

fn git(cwd: &Path, args: &[&str]) -> Result<(), ApplyError> {
    let out = Command::new("git").current_dir(cwd).args(args).output()?;
    if !out.status.success() {
        return Err(ApplyError::Git {
            args: args.join(" "),
            exit: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

fn git_capture(cwd: &Path, args: &[&str]) -> Result<String, ApplyError> {
    let out = Command::new("git").current_dir(cwd).args(args).output()?;
    if !out.status.success() {
        return Err(ApplyError::Git {
            args: args.join(" "),
            exit: out.status.code(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------- Dry-run executor (Phase 6.4a) ------------------------------------

/// Result of running a proposed recipe against a captured HTML sample.
///
/// "Dry-run" here means: parse the sample, apply the proposed recipe's path
/// filters to the discovered links, and check whether the recipe would
/// emit any signal at all. We don't actually fetch from the network; the
/// sample is the cached blob from the most-recent successful crawl.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DryRunResult {
    pub recipe_valid: bool,
    pub recipe_validation_error: Option<String>,
    pub extracted_text_chars: usize,
    pub raw_link_count: usize,
    pub link_count_after_filters: usize,
    pub script_count: usize,
    pub passes_quality_gate: bool,
    pub gate_failure_reason: Option<String>,
}

/// Quality gate thresholds. Set deliberately permissive — we want to catch
/// "this recipe extracts nothing" not "this recipe extracts less than the
/// previous one". Tightening happens after we have repair-pipeline data.
const MIN_EXTRACTED_TEXT_CHARS: usize = 200;
const MIN_LINKS_AFTER_FILTERS: usize = 1;

/// Validate `proposed_json` parses as a [`Recipe`], then run the parser +
/// recipe filters against `sample_html`. Returns the per-check stats plus
/// a single boolean gate. Pure function — no I/O.
pub fn dry_run_recipe(
    proposed_json: &str,
    source: &Source,
    sample_html: &[u8],
    sample_url: &str,
) -> DryRunResult {
    let mut recipe: Recipe = match serde_json::from_str(proposed_json) {
        Ok(r) => r,
        Err(e) => {
            return DryRunResult {
                recipe_valid: false,
                recipe_validation_error: Some(e.to_string()),
                extracted_text_chars: 0,
                raw_link_count: 0,
                link_count_after_filters: 0,
                script_count: 0,
                passes_quality_gate: false,
                gate_failure_reason: Some(format!("recipe is not valid JSON: {e}")),
            };
        }
    };
    recipe.normalize(source);

    let parsed = parse("text/html", sample_url, sample_html);
    let html = match parsed {
        ParsedDoc::Html(h) => h,
        ParsedDoc::Binary { .. } => {
            return DryRunResult {
                recipe_valid: true,
                recipe_validation_error: None,
                extracted_text_chars: 0,
                raw_link_count: 0,
                link_count_after_filters: 0,
                script_count: 0,
                passes_quality_gate: false,
                gate_failure_reason: Some(
                    "sample is a binary doc, not HTML — can't dry-run a recipe".into(),
                ),
            };
        }
        ParsedDoc::Unsupported { content_type, .. } => {
            return DryRunResult {
                recipe_valid: true,
                recipe_validation_error: None,
                extracted_text_chars: 0,
                raw_link_count: 0,
                link_count_after_filters: 0,
                script_count: 0,
                passes_quality_gate: false,
                gate_failure_reason: Some(format!("sample content-type unsupported: {content_type}")),
            };
        }
    };

    let extracted_chars = html.extracted_text.chars().count();
    let kept_after_filters = html
        .links
        .iter()
        .filter(|link| within_site(source, &recipe, link) && passes_path_filters(&recipe, link))
        .count();

    let (passes, reason) = if extracted_chars < MIN_EXTRACTED_TEXT_CHARS {
        (
            false,
            Some(format!(
                "extracted only {extracted_chars} chars (< {MIN_EXTRACTED_TEXT_CHARS}); \
                 site likely a JS shell or the parser misfires"
            )),
        )
    } else if kept_after_filters < MIN_LINKS_AFTER_FILTERS {
        (
            false,
            Some(format!(
                "{} same-site links survived the proposed recipe's filters; \
                 the recipe is excluding too aggressively",
                kept_after_filters
            )),
        )
    } else {
        (true, None)
    };

    DryRunResult {
        recipe_valid: true,
        recipe_validation_error: None,
        extracted_text_chars: extracted_chars,
        raw_link_count: html.raw_link_count,
        link_count_after_filters: kept_after_filters,
        script_count: html.script_count,
        passes_quality_gate: passes,
        gate_failure_reason: reason,
    }
}

/// Same-site test mirroring `worker::within_site`. Duplicated rather than
/// re-exposed: worker's version takes a mutable Frontier context; here we
/// only need the boolean.
fn within_site(source: &Source, recipe: &Recipe, link: &str) -> bool {
    let Some(link_host) = ::url::Url::parse(link)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_ascii_lowercase()))
    else {
        return false;
    };
    if let Some(allowed) = &recipe.allowed_subdomains {
        if link_host == source.domain {
            return true;
        }
        for sub in allowed {
            let expected = format!("{sub}.{}", source.domain);
            if link_host == expected {
                return true;
            }
        }
        return false;
    }
    crate::crawler_v2::url::same_site(&source.homepage_url, link)
}

/// Path-filter test mirroring `worker::passes_recipe_path_filters`.
fn passes_path_filters(recipe: &Recipe, link: &str) -> bool {
    let Some(path) = ::url::Url::parse(link).ok().map(|u| u.path().to_string()) else {
        return false;
    };
    let segs: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();

    for d in &recipe.deny_paths {
        if segs.iter().any(|s| s == &d.to_ascii_lowercase()) {
            return false;
        }
    }

    if let Some(allow) = &recipe.allow_paths {
        if !allow.is_empty() {
            let allow_lc: Vec<String> = allow.iter().map(|s| s.to_ascii_lowercase()).collect();
            if !segs.iter().any(|s| allow_lc.contains(s)) {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn metrics_with(verdict: HealthVerdict) -> HealthMetrics {
        HealthMetrics {
            source_id: "x".into(),
            window_start: Utc.with_ymd_and_hms(2026, 4, 21, 0, 0, 0).unwrap(),
            window_end: Utc.with_ymd_and_hms(2026, 4, 28, 0, 0, 0).unwrap(),
            n_cycles: 5,
            successes: 5,
            empty_extractions: 5,
            error_rate: 0.0,
            content_hash_change_rate: 0.0,
            structural_failure_streak: 5,
            verdict,
        }
    }

    #[test]
    fn evidence_json_includes_verdict_tag_and_reason() {
        let m = metrics_with(HealthVerdict::StructurallyFailed {
            reason: "5 consecutive non-productive cycles despite 12 historical inserts".into(),
        });
        let j = build_failure_evidence(&m);
        assert!(j.contains("\"verdict\":\"structurally_failed\""), "{j}");
        assert!(
            j.contains("non-productive"),
            "reason should be embedded: {j}"
        );
        // Must be parseable by serde_json since the agent will round-trip it.
        let _: serde_json::Value = serde_json::from_str(&j).expect("valid JSON");
    }

    #[test]
    fn evidence_json_handles_no_reason_verdicts() {
        let m = metrics_with(HealthVerdict::Healthy);
        let j = build_failure_evidence(&m);
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["verdict"], "healthy");
        assert_eq!(v["reason"], "(no reason recorded)");
    }

    #[test]
    fn evidence_json_escapes_quotes_in_reason() {
        let m = metrics_with(HealthVerdict::StructurallyFailed {
            reason: r#"site reports "page not found""#.into(),
        });
        let j = build_failure_evidence(&m);
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["reason"], "site reports \"page not found\"");
    }

    // ---------- dry_run_recipe tests --------------------------------------

    use crate::crawler_v2::types::{Source, Tier};
    use chrono::Utc;

    fn synth_source(sid: &str) -> Source {
        Source {
            source_id: sid.into(),
            domain: format!("{sid}.gov.np"),
            homepage_url: format!("https://{sid}.gov.np/"),
            name_en: None,
            name_np: None,
            office_type: None,
            province: None,
            tier: Tier(4),
            poll_interval_hours: 48,
            status: SourceStatus::Active,
            first_seen: Utc::now(),
            last_polled_at: None,
            last_changed_at: None,
            last_failure_at: None,
            consecutive_failures: 0,
            next_poll_at: None,
            notes: None,
        }
    }

    /// HTML rich enough to clear the 200-char extracted-text threshold AND
    /// carry a same-site link the recipe will keep.
    fn healthy_html(sid: &str) -> String {
        format!(
            "<html><head><title>Welcome to {sid}</title></head><body>\
             <main><h1>{sid} Municipality</h1><p>This is the Jiri Municipality home page \
             with a real paragraph of explanation about citizenship procedures, ward \
             registration, and frequently asked questions answered for residents. \
             The text continues for several lines so the parser produces enough body \
             content to clear the dry-run extracted-text gate.</p>\
             <a href=\"https://{sid}.gov.np/services\">Services</a> \
             <a href=\"https://{sid}.gov.np/notices\">Notices</a></main></body></html>",
            sid = sid
        )
    }

    #[test]
    fn dry_run_passes_for_sane_recipe_and_html() {
        let s = synth_source("jirimun_gov_np");
        let html = healthy_html("jirimun_gov_np");
        let proposed = r#"{"source_id":"jirimun_gov_np"}"#;
        let r = dry_run_recipe(proposed, &s, html.as_bytes(), &s.homepage_url);
        assert!(r.recipe_valid, "{:?}", r);
        assert!(r.passes_quality_gate, "{:?}", r);
        assert!(r.extracted_text_chars >= 200);
        assert!(r.link_count_after_filters >= 1);
        assert!(r.gate_failure_reason.is_none());
    }

    #[test]
    fn dry_run_fails_on_malformed_recipe_json() {
        let s = synth_source("x");
        let r = dry_run_recipe("not json", &s, b"<html></html>", &s.homepage_url);
        assert!(!r.recipe_valid);
        assert!(r.recipe_validation_error.is_some());
        assert!(!r.passes_quality_gate);
    }

    #[test]
    fn dry_run_fails_on_js_shell_sample() {
        let s = synth_source("x");
        // Realistic JS-shell shape: <script> heavy, body content empty until
        // hydration. extracted_text will be near-zero.
        let shell = r#"<html><head><title>App</title></head>
            <body><div id="root"></div>
            <script src="/static/js/main.js"></script>
            <script src="/static/js/vendor.js"></script>
            </body></html>"#;
        let proposed = r#"{"source_id":"x"}"#;
        let r = dry_run_recipe(proposed, &s, shell.as_bytes(), &s.homepage_url);
        assert!(r.recipe_valid);
        assert!(!r.passes_quality_gate);
        let reason = r.gate_failure_reason.unwrap();
        assert!(reason.contains("chars") || reason.contains("shell"), "reason={reason}");
    }

    #[test]
    fn dry_run_fails_when_filters_exclude_every_link() {
        // Recipe denies the only path segment present on the sample's links.
        let s = synth_source("jirimun_gov_np");
        let html = healthy_html("jirimun_gov_np");
        let proposed = r#"{
            "source_id":"jirimun_gov_np",
            "deny_paths":["services","notices"]
        }"#;
        let r = dry_run_recipe(proposed, &s, html.as_bytes(), &s.homepage_url);
        assert!(r.recipe_valid);
        assert!(!r.passes_quality_gate);
        let reason = r.gate_failure_reason.unwrap();
        assert!(reason.contains("survived"), "reason={reason}");
        assert_eq!(r.link_count_after_filters, 0);
    }

    #[test]
    fn dry_run_fails_when_allow_paths_match_nothing() {
        let s = synth_source("jirimun_gov_np");
        let html = healthy_html("jirimun_gov_np");
        let proposed = r#"{
            "source_id":"jirimun_gov_np",
            "allow_paths":["nonexistent"]
        }"#;
        let r = dry_run_recipe(proposed, &s, html.as_bytes(), &s.homepage_url);
        assert!(r.recipe_valid);
        assert!(!r.passes_quality_gate);
        assert_eq!(r.link_count_after_filters, 0);
    }

    #[test]
    fn dry_run_fails_on_empty_sample() {
        // Edge case: cached sample is empty or near-empty (e.g., the source
        // returned a 200 with an empty body the previous cycle). Parse
        // succeeds vacuously; gate must reject on text-too-short.
        let s = synth_source("x");
        let proposed = r#"{"source_id":"x"}"#;
        let r = dry_run_recipe(proposed, &s, b"", &s.homepage_url);
        assert!(r.recipe_valid);
        assert!(!r.passes_quality_gate);
        assert_eq!(r.extracted_text_chars, 0);
        assert_eq!(r.link_count_after_filters, 0);
    }

    #[test]
    fn dry_run_normalizes_recipe_source_id_mismatch_silently() {
        // The agent might emit the wrong source_id — our normalize() pass
        // overrides without erroring. Dry-run should still succeed.
        let s = synth_source("jirimun_gov_np");
        let html = healthy_html("jirimun_gov_np");
        let proposed = r#"{"source_id":"WRONG"}"#;
        let r = dry_run_recipe(proposed, &s, html.as_bytes(), &s.homepage_url);
        assert!(r.recipe_valid);
        assert!(r.passes_quality_gate);
    }

    #[test]
    fn dry_run_records_script_count_for_audit() {
        let s = synth_source("x");
        let html = "<html><body><p>Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. <a href=\"https://x.gov.np/page\">L</a></p><script>1</script><script>2</script><script>3</script></body></html>";
        let proposed = r#"{"source_id":"x"}"#;
        let r = dry_run_recipe(proposed, &s, html.as_bytes(), &s.homepage_url);
        assert!(r.passes_quality_gate, "{:?}", r);
        assert_eq!(r.script_count, 3);
    }
}
