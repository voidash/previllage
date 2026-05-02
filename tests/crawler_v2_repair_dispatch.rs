//! End-to-end tests for `dispatch_one`: agent → dry-run → apply → store
//! update. Each test seeds an in-memory Store, queues a repair, swaps in a
//! mock AgentRuntime, and asserts the row's terminal state + the returned
//! DispatchOutcome.

use async_trait::async_trait;
use chrono::Utc;
use gemma_god::crawler_v2::{
    dispatch_one, ApplyOptions, DispatchOptions, DispatchOutcome, GitAuthor,
};
use gemma_god::crawler_v2::agent::{AgentContext, AgentError, AgentProposal, AgentRuntime};
use gemma_god::crawler_v2::types::{DocType, Document, RegistryRow, RepairItem, RepairStatus};
use gemma_god::crawler_v2::Store;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;
use tempfile::TempDir;

// ----- Mock agents ---------------------------------------------------------

struct CannedAgent {
    proposal_json: String,
}

#[async_trait]
impl AgentRuntime for CannedAgent {
    async fn propose_recipe(&self, _ctx: &AgentContext) -> Result<AgentProposal, AgentError> {
        Ok(AgentProposal {
            proposed_recipe_json: self.proposal_json.clone(),
            agent_log: Some("mock".into()),
            elapsed_sec: 1,
        })
    }
}

struct FailingAgent {
    err: String,
}

#[async_trait]
impl AgentRuntime for FailingAgent {
    async fn propose_recipe(&self, _ctx: &AgentContext) -> Result<AgentProposal, AgentError> {
        Err(AgentError::ProcessFailed(Some(1), self.err.clone()))
    }
}

// ----- Helpers --------------------------------------------------------------

fn fresh_repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    run_git(p, &["init", "--initial-branch=main"]);
    run_git(p, &["config", "user.email", "test@test"]);
    run_git(p, &["config", "user.name", "test"]);
    run_git(p, &["commit", "--allow-empty", "-m", "init"]);
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

fn seed_source(store: &Store, sid: &str, tier: u8) {
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
                tier,
                poll_interval_hours: None,
                status: None,
                first_seen: None,
            },
            Utc::now(),
        )
        .unwrap();
}

fn seed_html_doc(store: &mut Store, sid: &str, raw_path: &str, extracted_path: Option<&str>) {
    let hash = format!("h_{sid}");
    let doc = Document {
        doc_id: format!("d_{sid}"),
        source_id: sid.into(),
        url: format!("https://{sid}.gov.np/"),
        content_hash: hash,
        fetched_at: Utc::now(),
        superseded_by: None,
        removed_at: None,
        doc_type: DocType::Html,
        status_code: 200,
        title: None,
        language: None,
        date_published: None,
        raw_blob_path: raw_path.into(),
        extracted_text_path: extracted_path.map(|s| s.to_string()),
        text_chars: 1500,
        size_bytes: 4096,
        depth: 0,
        priority_at_fetch: Some(100),
    };
    store.upsert_document(&doc).unwrap();
}

fn enqueue(store: &Store, sid: &str, sample_path: Option<&str>) -> i64 {
    let item = RepairItem {
        queue_id: None,
        source_id: sid.into(),
        queued_at: Utc::now(),
        status: RepairStatus::Pending,
        dispatched_at: None,
        completed_at: None,
        failure_evidence: r#"{"verdict":"structurally_failed"}"#.into(),
        sample_html_path: sample_path.map(|s| s.to_string()),
        proposed_recipe: None,
        dry_run_result: None,
        apply_outcome: None,
        error_log: None,
    };
    store.enqueue_repair(&item).unwrap()
}

fn opts(repo: &Path, corpus_root: &Path) -> DispatchOptions {
    DispatchOptions {
        recipe_schema: "(schema text)".into(),
        example_recipes: Vec::new(),
        apply_options: ApplyOptions {
            repo_root: repo.to_path_buf(),
            recipes_dir: PathBuf::from("recipes"),
            review_dir: PathBuf::from("recipes/review"),
            worktree_root: PathBuf::from(".repair-worktrees"),
            agent_label: "mock".into(),
            auto_apply_tier_min: 3,
            git_author: Some(GitAuthor {
                name: "test-bot".into(),
                email: "test@bot".into(),
            }),
        },
        agent_timeout: Duration::from_secs(5),
        corpus_root: corpus_root.to_path_buf(),
    }
}

/// Write a sane HTML sample under corpus_root and return the relative path
/// for use as `sample_html_path` on the queue row.
fn write_sample(corpus_root: &Path, sid: &str) -> String {
    let rel = format!("extracted/{sid}/sample.txt");
    let abs = corpus_root.join(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    let html = format!(
        "<html><body><h1>{sid}</h1>\
         <p>This is a long paragraph of body content explaining the citizenship \
         registration process at the {sid} office. We include enough text to \
         clear the dry-run extracted-text gate without triggering the JS-shell \
         heuristic.</p>\
         <a href=\"https://{sid}.gov.np/services\">Services</a></body></html>"
    );
    std::fs::write(&abs, html).unwrap();
    rel
}

// ----- Tests ----------------------------------------------------------------

/// Open a Store at a tempfile-backed path. Returns the Store handle (for
/// seeding) and the path (for passing to `dispatch_one`).
fn fresh_store() -> (Store, PathBuf, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("test.db");
    let store = Store::open(&db_path).unwrap();
    (store, db_path, dir)
}

#[tokio::test]
async fn no_pending_returns_no_pending() {
    let (_store, db_path, _dir) = fresh_store();
    let repo = fresh_repo();
    let corpus = tempfile::tempdir().unwrap();
    let agent = CannedAgent {
        proposal_json: "<recipe>{}</recipe>".into(),
    };
    let r = dispatch_one(&db_path, &agent, &opts(repo.path(), corpus.path()), Utc::now())
        .await
        .unwrap();
    assert!(matches!(r, DispatchOutcome::NoPending));
}

#[tokio::test]
async fn happy_path_tier_5_auto_applies_and_records_terminal_state() {
    let (mut store, db_path, _dir) = fresh_store();
    seed_source(&store, "jirimun_gov_np", 5);
    let corpus = tempfile::tempdir().unwrap();
    let sample_rel = write_sample(corpus.path(), "jirimun_gov_np");
    seed_html_doc(
        &mut store,
        "jirimun_gov_np",
        "blobs/jirimun_gov_np/h.html",
        Some(&sample_rel),
    );
    let qid = enqueue(&store, "jirimun_gov_np", Some(&sample_rel));

    let repo = fresh_repo();
    let agent = CannedAgent {
        proposal_json: r#"{"source_id":"jirimun_gov_np","js_render_required":true}"#.into(),
    };

    let r = dispatch_one(&db_path, &agent, &opts(repo.path(), corpus.path()), Utc::now())
        .await
        .unwrap();
    let returned_qid = match r {
        DispatchOutcome::Dispatched { queue_id, .. } => queue_id,
        other => panic!("expected Dispatched, got {other:?}"),
    };
    assert_eq!(returned_qid, qid);

    let row = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(row.status, RepairStatus::Applied);
    assert_eq!(row.apply_outcome.as_deref(), Some("auto"));
    assert!(row.proposed_recipe.unwrap().contains("js_render_required"));
    assert!(row.dry_run_result.unwrap().contains("passes_quality_gate"));
    assert!(row.completed_at.is_some());
    assert!(row.error_log.is_none());
}

#[tokio::test]
async fn tier_1_source_routes_to_human_review() {
    let (store, db_path, _dir) = fresh_store();
    seed_source(&store, "ag_gov_np", 1);
    let corpus = tempfile::tempdir().unwrap();
    let sample_rel = write_sample(corpus.path(), "ag_gov_np");
    let qid = enqueue(&store, "ag_gov_np", Some(&sample_rel));

    let repo = fresh_repo();
    let agent = CannedAgent {
        proposal_json: r#"{"source_id":"ag_gov_np"}"#.into(),
    };

    let r = dispatch_one(&db_path, &agent, &opts(repo.path(), corpus.path()), Utc::now())
        .await
        .unwrap();
    match r {
        DispatchOutcome::Dispatched { .. } => {}
        other => panic!("expected Dispatched, got {other:?}"),
    }
    let row = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(row.status, RepairStatus::HumanReview);
    assert_eq!(row.apply_outcome.as_deref(), Some("human_review"));
    assert!(row.error_log.is_none());
}

#[tokio::test]
async fn agent_error_marks_deadletter_with_error_log() {
    let (store, db_path, _dir) = fresh_store();
    seed_source(&store, "x", 5);
    let corpus = tempfile::tempdir().unwrap();
    let qid = enqueue(&store, "x", None);

    let repo = fresh_repo();
    let agent = FailingAgent {
        err: "auth not configured".into(),
    };

    let r = dispatch_one(&db_path, &agent, &opts(repo.path(), corpus.path()), Utc::now())
        .await
        .unwrap();
    match r {
        DispatchOutcome::AgentFailed { queue_id, error } => {
            assert_eq!(queue_id, qid);
            assert!(error.contains("auth"), "{error}");
        }
        other => panic!("expected AgentFailed, got {other:?}"),
    }
    let row = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(row.status, RepairStatus::Deadletter);
    let err = row.error_log.unwrap();
    assert!(err.contains("auth"), "{err}");
    assert!(row.proposed_recipe.is_none());
}

#[tokio::test]
async fn invalid_proposed_recipe_falls_through_to_deadletter() {
    // Agent succeeds but emits unparseable JSON. dry_run rejects → Deadletter.
    let (store, db_path, _dir) = fresh_store();
    seed_source(&store, "x", 5);
    let corpus = tempfile::tempdir().unwrap();
    let sample_rel = write_sample(corpus.path(), "x");
    let qid = enqueue(&store, "x", Some(&sample_rel));

    let repo = fresh_repo();
    let agent = CannedAgent {
        proposal_json: "this is not json at all".into(),
    };

    let r = dispatch_one(&db_path, &agent, &opts(repo.path(), corpus.path()), Utc::now())
        .await
        .unwrap();
    match r {
        DispatchOutcome::Dispatched { queue_id, .. } => assert_eq!(queue_id, qid),
        other => panic!("expected Dispatched, got {other:?}"),
    }
    let row = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(row.status, RepairStatus::Deadletter);
    assert!(row.error_log.unwrap().contains("not valid JSON"));
}

#[tokio::test]
async fn dry_run_gate_failure_routes_to_deadletter() {
    // Agent succeeds, JSON is valid, but the proposal denies the only
    // path on the sample — gate fails on "0 links survived".
    let (store, db_path, _dir) = fresh_store();
    seed_source(&store, "x", 5);
    let corpus = tempfile::tempdir().unwrap();
    let sample_rel = write_sample(corpus.path(), "x");
    let qid = enqueue(&store, "x", Some(&sample_rel));

    let repo = fresh_repo();
    let agent = CannedAgent {
        proposal_json: r#"{"source_id":"x","deny_paths":["services"]}"#.into(),
    };

    dispatch_one(&db_path, &agent, &opts(repo.path(), corpus.path()), Utc::now())
        .await
        .unwrap();
    let row = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(row.status, RepairStatus::Deadletter);
    let err = row.error_log.unwrap();
    assert!(
        err.contains("survived") || err.contains("excluding"),
        "deadletter reason: {err}"
    );
    // The dry_run_result blob captures the diagnostic stats so the operator
    // can audit later.
    let dr = row.dry_run_result.unwrap();
    assert!(dr.contains("link_count_after_filters"));
}

#[tokio::test]
async fn dispatch_consumes_one_at_a_time() {
    // Two pending items. First call drains one; second call drains the next.
    let (store, db_path, _dir) = fresh_store();
    seed_source(&store, "a", 5);
    seed_source(&store, "b", 5);
    let corpus = tempfile::tempdir().unwrap();
    let sa = write_sample(corpus.path(), "a");
    let sb = write_sample(corpus.path(), "b");
    let qa = enqueue(&store, "a", Some(&sa));
    let qb = enqueue(&store, "b", Some(&sb));
    assert_eq!(store.pending_repair_count().unwrap(), 2);

    let repo = fresh_repo();
    let agent = CannedAgent {
        proposal_json: r#"{"source_id":"a"}"#.into(),
    };

    // First call: drains qa.
    let r1 = dispatch_one(&db_path, &agent, &opts(repo.path(), corpus.path()), Utc::now())
        .await
        .unwrap();
    let q1 = match r1 {
        DispatchOutcome::Dispatched { queue_id, .. } => queue_id,
        other => panic!("expected Dispatched, got {other:?}"),
    };
    assert_eq!(q1, qa);
    assert_eq!(store.pending_repair_count().unwrap(), 1);

    // Second call: drains qb.
    let agent2 = CannedAgent {
        proposal_json: r#"{"source_id":"b"}"#.into(),
    };
    let r2 = dispatch_one(
        &db_path,
        &agent2,
        &opts(repo.path(), corpus.path()),
        Utc::now(),
    )
    .await
    .unwrap();
    let q2 = match r2 {
        DispatchOutcome::Dispatched { queue_id, .. } => queue_id,
        other => panic!("expected Dispatched, got {other:?}"),
    };
    assert_eq!(q2, qb);
    assert_eq!(store.pending_repair_count().unwrap(), 0);

    // Third call: empty queue.
    let r3 = dispatch_one(&db_path, &agent2, &opts(repo.path(), corpus.path()), Utc::now())
        .await
        .unwrap();
    assert!(matches!(r3, DispatchOutcome::NoPending));
}
