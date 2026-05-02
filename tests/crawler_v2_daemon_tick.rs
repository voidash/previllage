//! End-to-end test for `CrawlerTickHandler` driven through the full
//! `run_until` loop. Verifies the per-tick orchestration: poll → health →
//! drain. We control state precisely (no due sources, one queued repair,
//! mock agent) so the tick is deterministic.

use async_trait::async_trait;
use chrono::Utc;
use gemma_god::crawler_v2::agent::{AgentContext, AgentError, AgentProposal, AgentRuntime};
use gemma_god::crawler_v2::types::{
    DocType, Document, RegistryRow, RepairItem, RepairStatus,
};
use gemma_god::crawler_v2::{
    run_until, ApplyOptions, BlobStore, CrawlerTickHandler, DaemonConfig, DispatchOptions,
    DomainThrottle, FetchConfig, Fetcher, GitAuthor, Pool, StopCondition, Store, ThrottleConfig,
};
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

struct CannedAgent {
    proposal_json: String,
}

#[async_trait]
impl AgentRuntime for CannedAgent {
    async fn propose_recipe(&self, _ctx: &AgentContext) -> Result<AgentProposal, AgentError> {
        Ok(AgentProposal {
            proposed_recipe_json: self.proposal_json.clone(),
            agent_log: Some("mock".into()),
            elapsed_sec: 0,
        })
    }
}

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

fn seed_source_not_due(store: &Store, sid: &str, tier: u8) {
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
    // Push next_poll_at to the future so the tick's poll_all_due picks
    // nothing — keeps the test deterministic on offline machines.
    let future = Utc::now() + chrono::Duration::days(7);
    store.set_next_poll_at(sid, future).unwrap();
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

fn write_sample(corpus_root: &Path, sid: &str) -> String {
    let rel = format!("extracted/{sid}/sample.txt");
    let abs = corpus_root.join(&rel);
    std::fs::create_dir_all(abs.parent().unwrap()).unwrap();
    let html = format!(
        "<html><body><h1>{sid}</h1>\
         <p>This is a long paragraph with at least two hundred characters of body \
         content describing the citizenship registration process and ward services \
         offered by {sid} so the dry-run extracted-text gate has signal to work with.</p>\
         <a href=\"https://{sid}.gov.np/services\">Services</a></body></html>"
    );
    std::fs::write(&abs, html).unwrap();
    rel
}

#[tokio::test]
async fn daemon_tick_drains_pending_repair() {
    // 1. Set up tempdirs + fresh repo.
    let repo = fresh_repo();
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();

    // 2. Open Store at a real file path.
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("test.db");
    let mut store = Store::open(&db_path).unwrap();

    // 3. Seed: tier-5 source, not due. One pending repair with a real
    //    sample HTML on disk that will pass dry-run.
    seed_source_not_due(&store, "jirimun_gov_np", 5);
    let sample_rel = write_sample(corpus.path(), "jirimun_gov_np");
    seed_html_doc(
        &mut store,
        "jirimun_gov_np",
        "blobs/jirimun_gov_np/h.html",
        Some(&sample_rel),
    );
    let qid = enqueue(&store, "jirimun_gov_np", Some(&sample_rel));
    drop(store); // release exclusive handle so the daemon can re-open.

    // 4. Build the orchestration plumbing.
    let blobs = Arc::new(BlobStore::new(corpus.path().to_path_buf()).unwrap());
    let fetcher = Arc::new(Fetcher::new(FetchConfig::default()).unwrap());
    let throttle = Arc::new(DomainThrottle::new(ThrottleConfig::default()));
    let pool = Arc::new(Pool::new(
        db_path.clone(),
        repo.path().join("recipes"),
        blobs,
        fetcher,
        throttle,
    ));

    let agent: Arc<dyn AgentRuntime> = Arc::new(CannedAgent {
        proposal_json: r#"{"source_id":"jirimun_gov_np"}"#.into(),
    });

    let dispatch_opts = DispatchOptions {
        recipe_schema: "(schema)".into(),
        example_recipes: Vec::new(),
        apply_options: ApplyOptions {
            repo_root: repo.path().to_path_buf(),
            recipes_dir: PathBuf::from("recipes"),
            review_dir: PathBuf::from("recipes/review"),
            worktree_root: PathBuf::from(".repair-worktrees"),
            agent_label: "test".into(),
            auto_apply_tier_min: 3,
            git_author: Some(GitAuthor {
                name: "test-bot".into(),
                email: "test@bot".into(),
            }),
        },
        agent_timeout: Duration::from_secs(5),
        corpus_root: corpus.path().to_path_buf(),
    };

    let handler = CrawlerTickHandler::new(
        pool,
        db_path.clone(),
        2,                                 // poll concurrency
        Duration::from_secs(86_400 * 7),  // 7-day health window
        0,                                 // health every N ticks: 0 = disabled for this test
        10,                                // max repairs per tick
        dispatch_opts,
        agent,
    );

    // 5. Run the daemon for one tick.
    let cfg = DaemonConfig {
        tick_interval: Duration::from_millis(50),
        state_dir: state.path().to_path_buf(),
        stop_condition: StopCondition::MaxTicks(1),
    };
    let n = run_until(&handler, &cfg).await.unwrap();
    assert_eq!(n, 1);

    // 6. Verify: queue empty, the row reached terminal state Applied.
    let store = Store::open(&db_path).unwrap();
    assert_eq!(store.pending_repair_count().unwrap(), 0);
    let row = store.get_repair(qid).unwrap().unwrap();
    assert_eq!(row.status, RepairStatus::Applied);
    assert_eq!(row.apply_outcome.as_deref(), Some("auto"));
    assert!(row.completed_at.is_some());
    // PID lock dropped on graceful exit.
    assert!(!state.path().join("daemon.pid").exists());
}

#[tokio::test]
async fn daemon_tick_with_no_work_completes_cleanly() {
    let repo = fresh_repo();
    let corpus = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let db_dir = tempfile::tempdir().unwrap();
    let db_path = db_dir.path().join("test.db");
    {
        // Open + close to apply migrations.
        let store = Store::open(&db_path).unwrap();
        // Seed a source but make it not-due, so poll_all_due returns empty.
        seed_source_not_due(&store, "x", 5);
    }

    let blobs = Arc::new(BlobStore::new(corpus.path().to_path_buf()).unwrap());
    let fetcher = Arc::new(Fetcher::new(FetchConfig::default()).unwrap());
    let throttle = Arc::new(DomainThrottle::new(ThrottleConfig::default()));
    let pool = Arc::new(Pool::new(
        db_path.clone(),
        repo.path().join("recipes"),
        blobs,
        fetcher,
        throttle,
    ));
    let agent: Arc<dyn AgentRuntime> = Arc::new(CannedAgent {
        proposal_json: "{}".into(),
    });
    let dispatch_opts = DispatchOptions {
        recipe_schema: "".into(),
        example_recipes: Vec::new(),
        apply_options: ApplyOptions {
            repo_root: repo.path().to_path_buf(),
            recipes_dir: PathBuf::from("recipes"),
            review_dir: PathBuf::from("recipes/review"),
            worktree_root: PathBuf::from(".repair-worktrees"),
            agent_label: "test".into(),
            auto_apply_tier_min: 3,
            git_author: None,
        },
        agent_timeout: Duration::from_secs(5),
        corpus_root: corpus.path().to_path_buf(),
    };
    let handler = CrawlerTickHandler::new(
        pool,
        db_path.clone(),
        2,
        Duration::from_secs(86_400 * 7),
        1, // run health every tick
        10,
        dispatch_opts,
        agent,
    );

    let cfg = DaemonConfig {
        tick_interval: Duration::from_millis(20),
        state_dir: state.path().to_path_buf(),
        stop_condition: StopCondition::MaxTicks(2),
    };
    let n = run_until(&handler, &cfg).await.unwrap();
    assert_eq!(n, 2);
    // Source health should now have a row (InsufficientData since 0 cycles).
    let store = Store::open(&db_path).unwrap();
    let h = store.get_source_health("x").unwrap();
    assert!(h.is_some(), "health snapshot should be persisted after the health pass");
}
