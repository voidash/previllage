//! Integration tests for `apply_recipe`. Each test sets up a fresh git
//! repo in a tempdir + drives the apply-decision tree end-to-end.
//!
//! Why not unit tests in the module: apply_recipe shells out to `git`, so
//! tests need a real worktree, real HEAD commit, real branches. Tempdirs +
//! `Command::new("git")` is the cleanest way.

use chrono::Utc;
use gemma_god::crawler_v2::types::{RepairItem, RepairStatus, Source, SourceStatus, Tier};
use gemma_god::crawler_v2::{
    apply_recipe, ApplyOptions, ApplyOutcome, DryRunResult, GitAuthor,
};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use tempfile::TempDir;

/// Build a fresh git repo with a single empty commit so HEAD exists. The
/// caller works inside the returned tempdir.
fn fresh_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    run_git(dir.path(), &["init", "--initial-branch=main"]);
    run_git(dir.path(), &["config", "user.email", "test@test"]);
    run_git(dir.path(), &["config", "user.name", "test"]);
    run_git(dir.path(), &["commit", "--allow-empty", "-m", "init"]);
    dir
}

fn run_git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git").current_dir(cwd).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_git_capture(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git").current_dir(cwd).args(args).output().unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn tier_n_source(sid: &str, tier: u8) -> Source {
    Source {
        source_id: sid.into(),
        domain: format!("{sid}.gov.np"),
        homepage_url: format!("https://{sid}.gov.np/"),
        name_en: None,
        name_np: None,
        office_type: None,
        province: None,
        tier: Tier(tier),
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

fn item_with_proposal(sid: &str, recipe_json: &str) -> RepairItem {
    RepairItem {
        queue_id: Some(1),
        source_id: sid.into(),
        queued_at: Utc::now(),
        status: RepairStatus::Dispatched,
        dispatched_at: Some(Utc::now()),
        completed_at: None,
        failure_evidence: "{}".into(),
        sample_html_path: None,
        proposed_recipe: Some(recipe_json.into()),
        dry_run_result: None,
        apply_outcome: None,
        error_log: None,
    }
}

fn passing_dry_run() -> DryRunResult {
    DryRunResult {
        recipe_valid: true,
        recipe_validation_error: None,
        extracted_text_chars: 1500,
        raw_link_count: 8,
        link_count_after_filters: 6,
        script_count: 2,
        passes_quality_gate: true,
        gate_failure_reason: None,
    }
}

fn failing_dry_run(reason: &str) -> DryRunResult {
    DryRunResult {
        recipe_valid: true,
        recipe_validation_error: None,
        extracted_text_chars: 12,
        raw_link_count: 0,
        link_count_after_filters: 0,
        script_count: 5,
        passes_quality_gate: false,
        gate_failure_reason: Some(reason.into()),
    }
}

fn opts(repo: &Path) -> ApplyOptions {
    ApplyOptions {
        repo_root: repo.to_path_buf(),
        recipes_dir: PathBuf::from("recipes"),
        review_dir: PathBuf::from("recipes/review"),
        worktree_root: PathBuf::from(".repair-worktrees"),
        agent_label: "claude-code-test".into(),
        auto_apply_tier_min: 3,
        git_author: Some(GitAuthor {
            name: "test-bot".into(),
            email: "test@bot".into(),
        }),
    }
}

#[test]
fn deadletter_when_dry_run_gate_fails() {
    let repo = fresh_repo();
    let s = tier_n_source("a", 5);
    let item = item_with_proposal("a", r#"{"source_id":"a"}"#);
    let dr = failing_dry_run("0 same-site links survived");
    let r = apply_recipe(&item, &s, &dr, &opts(repo.path()), Utc::now()).unwrap();
    match r {
        ApplyOutcome::Deadletter { reason } => {
            assert!(reason.contains("survived"), "reason={reason}");
        }
        other => panic!("expected Deadletter, got {other:?}"),
    }
    // No files should have been written.
    assert!(!repo.path().join("recipes").exists());
    assert!(!repo.path().join(".repair-worktrees").exists());
    // No new branches.
    let branches = run_git_capture(repo.path(), &["branch", "--list"]);
    assert!(!branches.contains("repair/"), "branches: {branches}");
}

#[test]
fn human_review_for_tier_1_source() {
    let repo = fresh_repo();
    let s = tier_n_source("ag_gov_np", 1);
    let item = item_with_proposal("ag_gov_np", r#"{"source_id":"ag_gov_np"}"#);
    let r =
        apply_recipe(&item, &s, &passing_dry_run(), &opts(repo.path()), Utc::now()).unwrap();
    match r {
        ApplyOutcome::HumanReview { review_path } => {
            // Path is repo-relative; resolve it absolute via the repo root.
            let abs = repo.path().join(&review_path);
            assert!(abs.exists(), "review file missing at {}", abs.display());
            assert!(review_path.starts_with("recipes/review/ag_gov_np."));
            assert!(review_path.ends_with(".json"));
        }
        other => panic!("expected HumanReview, got {other:?}"),
    }
    // No git activity.
    let branches = run_git_capture(repo.path(), &["branch", "--list"]);
    assert!(!branches.contains("repair/"), "branches: {branches}");
    assert!(!repo.path().join(".repair-worktrees").exists());
}

#[test]
fn auto_apply_tier_5_creates_branch_and_commit() {
    let repo = fresh_repo();
    let s = tier_n_source("jirimun_gov_np", 5);
    let item = item_with_proposal(
        "jirimun_gov_np",
        r#"{"source_id":"jirimun_gov_np","js_render_required":true}"#,
    );
    let r =
        apply_recipe(&item, &s, &passing_dry_run(), &opts(repo.path()), Utc::now()).unwrap();
    let (branch, commit, worktree_path, recipe_path) = match r {
        ApplyOutcome::AutoApplied {
            branch,
            commit,
            worktree_path,
            recipe_path,
        } => (branch, commit, worktree_path, recipe_path),
        other => panic!("expected AutoApplied, got {other:?}"),
    };
    assert!(branch.starts_with("repair/jirimun_gov_np-"));
    // 40-char SHA-1 (or longer for SHA-256 repos, but git init defaults SHA-1 in 2.x).
    assert!(commit.len() >= 7, "commit looks short: {commit}");
    // Branch ref must exist in the main repo (not just inside the worktree).
    let branches = run_git_capture(repo.path(), &["branch", "--list"]);
    assert!(branches.contains(&branch), "branches: {branches}");
    // Worktree directory exists.
    assert!(Path::new(&worktree_path).exists(), "worktree path missing: {worktree_path}");
    // Recipe file exists at the worktree-relative path.
    let abs_recipe = Path::new(&worktree_path).join(&recipe_path);
    assert!(abs_recipe.exists(), "recipe missing at {}", abs_recipe.display());
    // Stamped last_repaired_at + repaired_by.
    let written = std::fs::read_to_string(&abs_recipe).unwrap();
    assert!(written.contains("last_repaired_at"));
    assert!(written.contains("claude-code-test"));
    // The new branch must NOT have leaked into main HEAD's working tree.
    let main_recipe = repo.path().join("recipes").join("jirimun_gov_np.json");
    assert!(
        !main_recipe.exists(),
        "auto-apply must not modify main worktree, but {} exists",
        main_recipe.display()
    );
}

#[test]
fn auto_apply_commit_message_attributes_agent() {
    let repo = fresh_repo();
    let s = tier_n_source("p", 5);
    let item = item_with_proposal("p", r#"{"source_id":"p"}"#);
    let r = apply_recipe(&item, &s, &passing_dry_run(), &opts(repo.path()), Utc::now()).unwrap();
    let branch = match &r {
        ApplyOutcome::AutoApplied { branch, .. } => branch.clone(),
        other => panic!("expected AutoApplied, got {other:?}"),
    };
    let log = run_git_capture(
        repo.path(),
        &["log", "-1", "--format=%s%n%an%n%ae", &branch],
    );
    let lines: Vec<&str> = log.lines().collect();
    assert!(lines[0].contains("repair(p)"), "subject: {:?}", lines[0]);
    assert!(lines[0].contains("claude-code-test"));
    assert_eq!(lines[1], "test-bot");
    assert_eq!(lines[2], "test@bot");
}

#[test]
fn invalid_proposed_recipe_returns_error() {
    let repo = fresh_repo();
    let s = tier_n_source("x", 5);
    let item = item_with_proposal("x", "{not json");
    let r = apply_recipe(&item, &s, &passing_dry_run(), &opts(repo.path()), Utc::now());
    assert!(matches!(
        r,
        Err(gemma_god::crawler_v2::ApplyError::InvalidRecipeJson(_))
    ));
}

#[test]
fn missing_proposed_recipe_returns_error() {
    let repo = fresh_repo();
    let s = tier_n_source("x", 5);
    let mut item = item_with_proposal("x", "");
    item.proposed_recipe = None;
    let r = apply_recipe(&item, &s, &passing_dry_run(), &opts(repo.path()), Utc::now());
    assert!(matches!(
        r,
        Err(gemma_god::crawler_v2::ApplyError::InvalidRecipeJson(_))
    ));
}

#[test]
fn auto_apply_tier_min_can_be_raised_to_force_review_for_all_tiers() {
    let repo = fresh_repo();
    let s = tier_n_source("jirimun_gov_np", 5);
    let item = item_with_proposal("jirimun_gov_np", r#"{"source_id":"jirimun_gov_np"}"#);
    let mut o = opts(repo.path());
    o.auto_apply_tier_min = 99; // nothing auto-applies
    let r = apply_recipe(&item, &s, &passing_dry_run(), &o, Utc::now()).unwrap();
    assert!(matches!(r, ApplyOutcome::HumanReview { .. }));
}

#[test]
fn two_apply_runs_for_same_source_use_unique_branch_names() {
    // Sanity: timestamps in branch names mean back-to-back runs don't collide.
    // But "back to back within a second" can produce identical timestamps;
    // we sleep 1s to guarantee different ts.
    let repo = fresh_repo();
    let s = tier_n_source("p", 5);
    let item = item_with_proposal("p", r#"{"source_id":"p"}"#);
    let now1 = Utc::now();
    let r1 = apply_recipe(&item, &s, &passing_dry_run(), &opts(repo.path()), now1).unwrap();
    let now2 = now1 + chrono::Duration::seconds(1);
    let r2 = apply_recipe(&item, &s, &passing_dry_run(), &opts(repo.path()), now2).unwrap();
    let b1 = match r1 {
        ApplyOutcome::AutoApplied { branch, .. } => branch,
        _ => panic!(),
    };
    let b2 = match r2 {
        ApplyOutcome::AutoApplied { branch, .. } => branch,
        _ => panic!(),
    };
    assert_ne!(b1, b2);
}
