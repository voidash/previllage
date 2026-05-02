//! `crawl` — admin CLI for the crawler_v2 daemon.
//!
//! Subcommands land as the phases do (see CRAWLER.md §Phased build plan):
//!   phase 1: init           populate index.db from sources_tiered.jsonl
//!            status          show source counts / tier breakdown
//!   phase 2+: poll, describe, health, daemon, import-legacy

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use chrono::Utc;
use gemma_god::crawler_v2::{
    chunk_text, classify_language, dispatch_one, extract_text, import_legacy,
    is_internally_repetitive, load_recipe, load_recipe_strict, run_until, substantive_chars,
    sync_registry, ApplyOptions, BlobStore, ChunkConfig, ClaudeCodeAgent, ClaudeCodeConfig,
    CrawlerTickHandler, DaemonConfig, DispatchOptions, DispatchOutcome, DomainThrottle,
    ExampleRecipe, ExtractStatus, FetchConfig, Fetcher, GitAuthor, ImportOptions, Language, Pool,
    StopCondition, Store, ThrottleConfig,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

#[derive(Parser)]
#[command(name = "crawl", version, about = "Nepal gov crawler daemon admin CLI")]
struct Cli {
    /// Path to the SQLite index file.
    #[arg(long, default_value = "/Volumes/T9/gemma-god/corpus_v2/index.db",
          global = true)]
    db: PathBuf,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Create the SQLite schema and sync the source registry into it.
    Init {
        /// Path to corpora/sources_tiered.jsonl.
        #[arg(long, default_value = "corpora/sources_tiered.jsonl")]
        sources: PathBuf,
    },
    /// Print a summary of the current index.
    Status {
        /// Optional source_id for a single-source report.
        #[arg(long)]
        source: Option<String>,
    },
    /// Print the fully-resolved recipe for a source (defaults + overrides).
    Describe {
        source_id: String,
        #[arg(long, default_value = "recipes")]
        recipes_dir: PathBuf,
    },
    /// Import manifests produced by the Python prototype crawler
    /// (`scripts/crawl_sources.py`) into the SQLite store. One-shot;
    /// re-running is idempotent.
    ImportLegacy {
        #[arg(long, default_value = "/Volumes/T9/gemma-god/corpus_v2/manifests")]
        manifests_dir: PathBuf,
        /// Skip rows whose source_id isn't in the registry (instead of erroring).
        #[arg(long)]
        lenient: bool,
    },
    /// Extract text + chunk documents. Reads all live documents with no
    /// chunks yet, runs text extraction (HTML sidecar read or PDF parse),
    /// splits into overlapping char-based chunks, and writes a row per
    /// chunk into SQLite. Idempotent — re-run to catch up new documents.
    IndexChunks {
        /// Corpus root (raw/extracted paths in documents table are relative
        /// to this).
        #[arg(long, default_value = "/Volumes/T9/gemma-god/corpus_v2")]
        corpus_root: PathBuf,
        /// Optional per-run cap on documents processed (for sanity runs).
        #[arg(long)]
        limit: Option<u64>,
        /// If set, only process docs from this source. Lets you re-chunk
        /// one source at a time without retriggering work for stale
        /// failures across the rest of the corpus.
        #[arg(long)]
        source: Option<String>,
    },
    /// Long-running scheduled daemon. Each tick polls every due source,
    /// recomputes health, and drains the repair queue. Holds an exclusive
    /// PID lock so a second invocation refuses to start. Exits on
    /// SIGINT/SIGTERM (graceful — finishes current tick first).
    Daemon {
        /// State directory for the PID lock + (future) log files.
        #[arg(long, default_value = "/tmp/crawler-v2-daemon")]
        state_dir: PathBuf,
        /// Interval between ticks in seconds.
        #[arg(long, default_value = "60")]
        tick_interval_sec: u64,
        /// Run exactly N ticks then exit. Useful for tests/smoke runs;
        /// `0` (default) means "run until signal".
        #[arg(long, default_value = "0")]
        max_ticks: u32,
        /// Per-tick poll concurrency.
        #[arg(long, default_value = "10")]
        poll_concurrency: usize,
        /// Recipes directory — relative to repo_root.
        #[arg(long, default_value = "recipes")]
        recipes_dir: PathBuf,
        /// Repository root for git auto-commits.
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        /// Corpus root (where blobs/ and extracted/ live).
        #[arg(long, default_value = "/Volumes/T9/gemma-god/corpus_v2")]
        corpus_root: PathBuf,
        /// Health pass cadence: every Nth tick. `0` disables health.
        #[arg(long, default_value = "1")]
        health_every_n_ticks: u32,
        /// Cap on repairs drained per tick.
        #[arg(long, default_value = "10")]
        max_repairs_per_tick: u32,
        /// Rolling-window for health analysis (days).
        #[arg(long, default_value = "7")]
        health_window_days: i64,
        /// Tier `>=` this auto-applies repairs; lower → human review.
        #[arg(long, default_value = "3")]
        auto_apply_tier_min: u8,
        /// Per-call agent timeout (seconds).
        #[arg(long, default_value = "600")]
        agent_timeout_sec: u64,
        /// Agent CLI command.
        #[arg(long, default_value = "claude")]
        claude_cmd: String,
    },
    /// Drain pending repair-queue items by invoking the configured agent
    /// runtime. The agent inspects the failure evidence + sample HTML,
    /// proposes an updated recipe, we dry-run it, then either auto-commit
    /// (tier 3-5) or route to human review (tier 1-2). One call per
    /// item; `--drain` repeats until the queue is empty.
    Repair {
        /// Drain just one item then exit. Mutually exclusive with --drain.
        #[arg(long)]
        once: bool,
        /// Drain until the queue is empty.
        #[arg(long)]
        drain: bool,
        /// Repository root for git auto-commits. Defaults to the cwd.
        #[arg(long, default_value = ".")]
        repo_root: PathBuf,
        /// Recipes directory, relative to repo_root.
        #[arg(long, default_value = "recipes")]
        recipes_dir: PathBuf,
        /// Corpus root — same path the worker writes blobs into.
        #[arg(long, default_value = "/Volumes/T9/gemma-god/corpus_v2")]
        corpus_root: PathBuf,
        /// Per-call agent timeout in seconds. 600s = 10 min, generous for
        /// Playwright-driven investigation.
        #[arg(long, default_value = "600")]
        agent_timeout_sec: u64,
        /// Tier `>= this` auto-applies; lower tiers route to human review.
        #[arg(long, default_value = "3")]
        auto_apply_tier_min: u8,
        /// Override the agent CLI command. Default: `claude` from PATH.
        #[arg(long, default_value = "claude")]
        claude_cmd: String,
    },
    /// Run a polling cycle — either against a single source or every source
    /// whose next_poll_at has elapsed.
    Poll {
        /// Target one source by id. Mutually exclusive with --all.
        #[arg(long)]
        source: Option<String>,
        /// Poll every source whose schedule has elapsed.
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "recipes")]
        recipes_dir: PathBuf,
        /// Root under which blobs/ and extracted/ live.
        #[arg(long, default_value = "/Volumes/T9/gemma-god/corpus_v2")]
        out_root: PathBuf,
        /// Max simultaneous workers when polling multiple sources.
        #[arg(long, default_value = "10")]
        concurrency: usize,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Cmd::Init { sources } => cmd_init(&cli.db, &sources),
        Cmd::Status { source } => cmd_status(&cli.db, source.as_deref()),
        Cmd::Describe {
            source_id,
            recipes_dir,
        } => cmd_describe(&cli.db, &source_id, &recipes_dir),
        Cmd::ImportLegacy {
            manifests_dir,
            lenient,
        } => cmd_import_legacy(&cli.db, &manifests_dir, lenient),
        Cmd::IndexChunks {
            corpus_root,
            limit,
            source,
        } => cmd_index_chunks(&cli.db, &corpus_root, limit, source.as_deref()),
        Cmd::Daemon {
            state_dir,
            tick_interval_sec,
            max_ticks,
            poll_concurrency,
            recipes_dir,
            repo_root,
            corpus_root,
            health_every_n_ticks,
            max_repairs_per_tick,
            health_window_days,
            auto_apply_tier_min,
            agent_timeout_sec,
            claude_cmd,
        } => cmd_daemon(
            &cli.db,
            state_dir,
            tick_interval_sec,
            max_ticks,
            poll_concurrency,
            recipes_dir,
            repo_root,
            corpus_root,
            health_every_n_ticks,
            max_repairs_per_tick,
            health_window_days,
            auto_apply_tier_min,
            agent_timeout_sec,
            claude_cmd,
        ),
        Cmd::Repair {
            once,
            drain,
            repo_root,
            recipes_dir,
            corpus_root,
            agent_timeout_sec,
            auto_apply_tier_min,
            claude_cmd,
        } => cmd_repair(
            &cli.db,
            once,
            drain,
            repo_root,
            recipes_dir,
            corpus_root,
            agent_timeout_sec,
            auto_apply_tier_min,
            claude_cmd,
        ),
        Cmd::Poll {
            source,
            all,
            recipes_dir,
            out_root,
            concurrency,
        } => cmd_poll(&cli.db, source.as_deref(), all, &recipes_dir, &out_root, concurrency),
    }
}

#[allow(clippy::too_many_arguments)]
fn cmd_daemon(
    db: &PathBuf,
    state_dir: PathBuf,
    tick_interval_sec: u64,
    max_ticks: u32,
    poll_concurrency: usize,
    recipes_dir: PathBuf,
    repo_root: PathBuf,
    corpus_root: PathBuf,
    health_every_n_ticks: u32,
    max_repairs_per_tick: u32,
    health_window_days: i64,
    auto_apply_tier_min: u8,
    agent_timeout_sec: u64,
    claude_cmd: String,
) -> Result<()> {
    // Apply schema migrations + ensure DB exists before the loop kicks in.
    {
        let _store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
    }

    let abs_recipes_dir = repo_root.join(&recipes_dir);
    let example_recipes = load_example_recipes(&abs_recipes_dir);

    let blobs = Arc::new(BlobStore::new(corpus_root.clone())?);
    let fetcher = Arc::new(
        Fetcher::new(FetchConfig::default()).map_err(|e| anyhow::anyhow!("fetcher: {e}"))?,
    );
    let throttle = Arc::new(DomainThrottle::new(ThrottleConfig::default()));
    let pool = Arc::new(Pool::new(
        db.clone(),
        recipes_dir.clone(),
        blobs,
        fetcher,
        throttle,
    ));

    let dispatch_opts = DispatchOptions {
        recipe_schema: RECIPE_SCHEMA_DOC.to_string(),
        example_recipes,
        apply_options: ApplyOptions {
            repo_root,
            recipes_dir,
            review_dir: PathBuf::from("recipes/review"),
            worktree_root: PathBuf::from(".repair-worktrees"),
            agent_label: format!("claude-code-via-{}", claude_cmd),
            auto_apply_tier_min,
            git_author: Some(GitAuthor {
                name: "crawler-repair-bot".into(),
                email: "crawler-repair@local".into(),
            }),
        },
        agent_timeout: Duration::from_secs(agent_timeout_sec),
        corpus_root: corpus_root.clone(),
    };

    let agent: Arc<dyn gemma_god::crawler_v2::AgentRuntime> =
        Arc::new(ClaudeCodeAgent::new(ClaudeCodeConfig {
            command: claude_cmd,
            extra_args: Vec::new(),
        }));

    let handler = CrawlerTickHandler::new(
        pool,
        db.clone(),
        poll_concurrency,
        Duration::from_secs((health_window_days as u64).saturating_mul(86_400)),
        health_every_n_ticks,
        max_repairs_per_tick,
        dispatch_opts,
        agent,
    );

    let stop = if max_ticks == 0 {
        StopCondition::Signal
    } else {
        StopCondition::MaxTicks(max_ticks)
    };
    let cfg = DaemonConfig {
        tick_interval: Duration::from_secs(tick_interval_sec),
        state_dir,
        stop_condition: stop,
    };

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    eprintln!("[daemon] starting (stop={:?}, tick={tick_interval_sec}s)", cfg.stop_condition);
    let ticks = rt.block_on(run_until(&handler, &cfg))?;
    eprintln!("[daemon] exited after {ticks} ticks");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_repair(
    db: &PathBuf,
    once: bool,
    drain: bool,
    repo_root: PathBuf,
    recipes_dir: PathBuf,
    corpus_root: PathBuf,
    agent_timeout_sec: u64,
    auto_apply_tier_min: u8,
    claude_cmd: String,
) -> Result<()> {
    if once == drain {
        anyhow::bail!("specify exactly one of --once or --drain");
    }

    // Open once to apply migrations on first call; we hand `dispatch_one`
    // the path so it can open scoped Stores around its `.await`s (rusqlite
    // Connection is `!Sync`).
    let _store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
    drop(_store);

    // Load up to 3 example recipes for the agent's few-shot context. We
    // pick whichever .json files exist under recipes_dir (alphabetically
    // first 3). If recipes_dir is empty, the agent works without examples
    // — still functional, just less anchored.
    let abs_recipes_dir = repo_root.join(&recipes_dir);
    let example_recipes = load_example_recipes(&abs_recipes_dir);

    let opts = DispatchOptions {
        recipe_schema: RECIPE_SCHEMA_DOC.to_string(),
        example_recipes,
        apply_options: ApplyOptions {
            repo_root: repo_root.clone(),
            recipes_dir: recipes_dir.clone(),
            review_dir: recipes_dir.join("review"),
            worktree_root: PathBuf::from(".repair-worktrees"),
            agent_label: format!("claude-code-via-{}", claude_cmd),
            auto_apply_tier_min,
            git_author: Some(GitAuthor {
                name: "crawler-repair-bot".into(),
                email: "crawler-repair@local".into(),
            }),
        },
        agent_timeout: Duration::from_secs(agent_timeout_sec),
        corpus_root,
    };

    let agent = ClaudeCodeAgent::new(ClaudeCodeConfig {
        command: claude_cmd,
        extra_args: Vec::new(),
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    if once {
        let r = rt.block_on(dispatch_one(db, &agent, &opts, Utc::now()))?;
        print_dispatch_outcome(&r);
    } else {
        // --drain
        let mut drained = 0u64;
        loop {
            let r = rt.block_on(dispatch_one(db, &agent, &opts, Utc::now()))?;
            match &r {
                DispatchOutcome::NoPending => {
                    eprintln!("[repair] queue empty; drained {drained} item(s)");
                    break;
                }
                _ => {
                    print_dispatch_outcome(&r);
                    drained += 1;
                }
            }
        }
    }
    Ok(())
}

fn print_dispatch_outcome(o: &DispatchOutcome) {
    match o {
        DispatchOutcome::NoPending => println!("no pending repairs"),
        DispatchOutcome::Dispatched {
            queue_id,
            apply_outcome,
        } => {
            println!(
                "queue_id={queue_id} → {}",
                serde_json::to_string(apply_outcome).unwrap_or_default()
            );
        }
        DispatchOutcome::AgentFailed { queue_id, error } => {
            println!("queue_id={queue_id} agent_failed: {error}");
        }
    }
}

fn load_example_recipes(recipes_dir: &PathBuf) -> Vec<ExampleRecipe> {
    let mut entries: Vec<PathBuf> = match std::fs::read_dir(recipes_dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "json"))
            .collect(),
        Err(_) => Vec::new(),
    };
    entries.sort();
    entries
        .into_iter()
        .take(3)
        .filter_map(|p| {
            let json = std::fs::read_to_string(&p).ok()?;
            let sid = p.file_stem()?.to_string_lossy().into_owned();
            // Make sure it parses against a synthetic source so we don't
            // ship malformed examples to the agent.
            let synth = synth_source_for_validation(&sid);
            load_recipe_strict(&synth, recipes_dir).ok()?;
            Some(ExampleRecipe { source_id: sid, json })
        })
        .collect()
}

fn synth_source_for_validation(sid: &str) -> gemma_god::crawler_v2::types::Source {
    // We just need a Source struct sufficient for `load_recipe_strict` to
    // run normalize() without panicking. The fields we don't read get any
    // safe value.
    use gemma_god::crawler_v2::types::{Source, SourceStatus, Tier};
    Source {
        source_id: sid.into(),
        domain: format!("{sid}.gov.np"),
        homepage_url: format!("https://{sid}.gov.np/"),
        name_en: None,
        name_np: None,
        office_type: None,
        province: None,
        tier: Tier(3),
        poll_interval_hours: 24,
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

fn cmd_poll(
    db: &PathBuf,
    source: Option<&str>,
    all: bool,
    recipes_dir: &PathBuf,
    out_root: &PathBuf,
    concurrency: usize,
) -> Result<()> {
    if source.is_some() == all {
        anyhow::bail!("specify exactly one of --source <id> or --all");
    }

    let blobs = Arc::new(BlobStore::new(out_root.clone())?);
    let fetcher = Arc::new(
        Fetcher::new(FetchConfig::default())
            .map_err(|e| anyhow::anyhow!("fetcher init: {e}"))?,
    );
    let throttle = Arc::new(DomainThrottle::new(ThrottleConfig::default()));
    let pool = Pool::new(
        db.clone(),
        recipes_dir.clone(),
        blobs,
        fetcher,
        throttle,
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    if let Some(sid) = source {
        let report = rt
            .block_on(pool.poll_source(sid))
            .with_context(|| format!("polling source {sid}"))?;
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let reports = rt
            .block_on(pool.poll_all_due(concurrency))
            .context("polling all due sources")?;
        println!("{}", serde_json::to_string_pretty(&reports)?);
        eprintln!(
            "[poll] {} sources crawled; concurrency={concurrency}",
            reports.len()
        );
    }
    Ok(())
}

/// Schema text the repair agent receives as part of its prompt.
/// Hand-crafted (rather than `include_str!("recipe.rs")`) so the agent sees
/// only the public-facing shape — no `#[serde]` internals or rust-only
/// helpers it would need to reverse-engineer.
const RECIPE_SCHEMA_DOC: &str = r#"
Recipe JSON schema — sparse overrides on the default crawl policy.
All fields optional except source_id; missing fields fall back to defaults.

  {
    "source_id": "string (must match the registry entry)",
    "version": int,                   // recipe revision; default 1
    "entry_points": ["url", ...],     // default: [<source.homepage_url>]
    "deny_paths": ["seg", ...],       // case-insensitive path segments to reject
    "allow_paths": ["seg", ...]?,     // null or array; if non-empty, path must
                                      // contain at least one segment from list
    "max_depth": int,                 // default 2
    "max_pdf_depth": int,             // default 3
    "max_html_fetches": int,          // default 250
    "max_total_fetches": int,         // default 1500
    "max_elapsed_sec": int,           // default 1200
    "rate_limit_ms": int,             // default 1000 (per-domain)
    "respect_robots": bool,           // default true
    "allowed_subdomains": ["sub", ...]?, // null = same-site rule
    "custom_user_agent": "string"?,
    "js_render_required": bool,       // default false; set true for SPA shells
    "notes": "string",
    "last_repaired_at": null,         // stamped by the dispatcher; do not set
    "repaired_by": null               // stamped by the dispatcher; do not set
  }
"#;

/// Minimum substantive chars (Devanagari + Latin alpha) for a chunk to be
/// worth indexing. Below this, retrieval signal is negligible.
const MIN_SUBSTANTIVE_CHARS: usize = 100;
/// Cross-doc boilerplate threshold: if a chunk's text has appeared this many
/// times already within the same source (nav headers, footers), stop indexing
/// further copies.
const MAX_BOILERPLATE_REPEATS: u32 = 3;

fn cmd_index_chunks(
    db: &PathBuf,
    corpus_root: &PathBuf,
    limit: Option<u64>,
    source: Option<&str>,
) -> Result<()> {
    let mut store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;

    let docs = store.list_unchunked_documents(limit, source)?;
    eprintln!(
        "[chunks] {} documents to process{}",
        docs.len(),
        source.map(|s| format!(" (filter: source={s})")).unwrap_or_default()
    );

    let config = ChunkConfig::default();
    let now = Utc::now();

    let mut docs_ok = 0u64;
    let mut docs_html_skip_empty = 0u64;
    let mut docs_pdf_skip_notext = 0u64;
    let mut docs_unsupported = 0u64;
    let mut docs_extract_err = 0u64;
    let mut chunks_written = 0u64;
    let mut chunks_dropped_short = 0u64;
    let mut chunks_dropped_other = 0u64;
    let mut chunks_dropped_boilerplate = 0u64;
    let mut chunks_dropped_mojibake = 0u64;
    let mut chunks_dropped_repetitive = 0u64;
    let mut lang_counts: HashMap<Language, u64> = HashMap::new();

    // Per-source nav/boilerplate detector: hash(text) -> count seen in this
    // source during this run. After MAX_BOILERPLATE_REPEATS, additional
    // copies are dropped.
    let mut per_source_seen: HashMap<String, HashMap<u64, u32>> = HashMap::new();

    for (i, doc) in docs.iter().enumerate() {
        let extracted = match extract_text(doc, corpus_root) {
            Ok(e) => e,
            Err(e) => {
                docs_extract_err += 1;
                if docs_extract_err <= 10 {
                    eprintln!("  extract_err [{}]: {}", doc.url, e);
                }
                continue;
            }
        };
        match extracted.status {
            ExtractStatus::Ok => {}
            ExtractStatus::EmptyExtraction => {
                docs_html_skip_empty += 1;
                continue;
            }
            ExtractStatus::PdfNoText => {
                docs_pdf_skip_notext += 1;
                continue;
            }
            ExtractStatus::SkippedUnsupported => {
                docs_unsupported += 1;
                continue;
            }
        }

        let raw_chunks = chunk_text(&doc.doc_id, &extracted.text, config);
        if raw_chunks.is_empty() {
            continue;
        }

        // Per-doc filter + dedup pass before SQLite insert.
        let source_seen = per_source_seen
            .entry(doc.source_id.clone())
            .or_default();
        let mut keep = Vec::with_capacity(raw_chunks.len());
        let mut keep_langs: Vec<&'static str> = Vec::with_capacity(raw_chunks.len());
        for c in raw_chunks {
            let lang = classify_language(&c.text);
            let sub = substantive_chars(&c.text);

            if sub < MIN_SUBSTANTIVE_CHARS {
                chunks_dropped_short += 1;
                continue;
            }
            if lang == Language::Other {
                chunks_dropped_other += 1;
                continue;
            }
            // MojibakeSuspected = chunks we know are non-Preeti legacy-font
            // garbage our converter can't recover. Keeping them poisons
            // embeddings with near-random vectors.
            if lang == Language::MojibakeSuspected {
                chunks_dropped_mojibake += 1;
                continue;
            }
            // Intra-chunk repetition (PDF page headers repeating with
            // varying page numbers on every page of a long legal PDF).
            // 4 repeats of a 30-char window matches the real ag_gov_np
            // land-law header `भूमि सम्बन्धी कानूनी स्रोत सामग्री` and
            // doesn't fire on normal prose (verified by unit tests).
            if is_internally_repetitive(&c.text, 4, 30) {
                chunks_dropped_repetitive += 1;
                continue;
            }
            let text_hash = hash_text(&c.text);
            let count = source_seen.entry(text_hash).or_insert(0);
            if *count >= MAX_BOILERPLATE_REPEATS {
                chunks_dropped_boilerplate += 1;
                continue;
            }
            *count += 1;

            *lang_counts.entry(lang).or_insert(0) += 1;
            keep.push(c);
            keep_langs.push(lang.as_str());
        }

        if keep.is_empty() {
            continue;
        }
        let n = store.insert_chunks(&doc.doc_id, &keep, &keep_langs, now)?;
        chunks_written += n as u64;
        docs_ok += 1;

        if (i + 1) % 500 == 0 {
            eprintln!(
                "  [{}/{}] docs_ok={docs_ok} chunks={chunks_written} \
                 dropped_short={chunks_dropped_short} boilerplate={chunks_dropped_boilerplate}",
                i + 1,
                docs.len()
            );
        }
    }

    eprintln!("=== index-chunks summary ===");
    eprintln!("  docs_attempted:        {}", docs.len());
    eprintln!("  docs_ok:               {docs_ok}");
    eprintln!("  chunks_written:        {chunks_written}");
    eprintln!("  chunks_dropped_short:     {chunks_dropped_short}");
    eprintln!("  chunks_dropped_other:     {chunks_dropped_other}");
    eprintln!("  chunks_dropped_mojibake:  {chunks_dropped_mojibake}");
    eprintln!("  chunks_dropped_repetitive:{chunks_dropped_repetitive}");
    eprintln!("  chunks_dropped_boilerplate:{chunks_dropped_boilerplate}");
    eprintln!("  skipped_html_empty:    {docs_html_skip_empty}");
    eprintln!("  skipped_pdf_no_text:   {docs_pdf_skip_notext}");
    eprintln!("  skipped_unsupported:   {docs_unsupported}");
    eprintln!("  extraction_errors:     {docs_extract_err}");
    eprintln!(
        "  total chunks in db:    {}",
        store.chunk_count_total()?
    );
    eprintln!("  language breakdown (kept chunks):");
    let mut langs: Vec<_> = lang_counts.iter().collect();
    langs.sort_by_key(|(_, n)| std::cmp::Reverse(**n));
    for (lang, n) in langs {
        eprintln!("    {:<18} {:>8}", lang.as_str(), n);
    }
    Ok(())
}

fn hash_text(text: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

fn cmd_import_legacy(db: &PathBuf, manifests_dir: &PathBuf, lenient: bool) -> Result<()> {
    let mut store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
    let opts = ImportOptions {
        manifests_dir: manifests_dir.clone(),
        lenient,
    };
    let report = import_legacy(&mut store, &opts)
        .with_context(|| format!("importing from {}", manifests_dir.display()))?;
    eprintln!("=== import summary ===");
    eprintln!("  files_read:           {}", report.files_read);
    eprintln!("  rows_total:           {}", report.rows_total);
    eprintln!("  rows_inserted:        {}", report.rows_inserted);
    eprintln!("  rows_superseded:      {}", report.rows_superseded);
    eprintln!("  rows_unchanged:       {}", report.rows_unchanged);
    eprintln!("  rows_skipped_error:   {}", report.rows_skipped_error);
    eprintln!(
        "  rows_skipped_no_hash: {}",
        report.rows_skipped_no_hash
    );
    eprintln!(
        "  rows_skipped_malformed: {}",
        report.rows_skipped_malformed
    );
    if !report.unknown_sources.is_empty() {
        eprintln!(
            "  unknown_sources (lenient mode skipped): {}",
            report.unknown_sources.len()
        );
    }
    Ok(())
}

fn cmd_describe(db: &PathBuf, source_id: &str, recipes_dir: &PathBuf) -> Result<()> {
    let store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
    let source = store
        .get_source(source_id)?
        .with_context(|| format!("no source_id={source_id}"))?;
    let recipe = load_recipe(&source, recipes_dir);
    println!("{}", serde_json::to_string_pretty(&recipe)?);
    Ok(())
}

fn cmd_init(db: &PathBuf, sources_file: &PathBuf) -> Result<()> {
    let store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
    eprintln!("[init] db ready at {}", db.display());

    let report = sync_registry(&store, sources_file)
        .with_context(|| format!("syncing {}", sources_file.display()))?;
    eprintln!(
        "[init] registry sync: {} rows  ({} inserted, {} updated, \
         {} bad-json, {} missing-fields)",
        report.total_rows,
        report.inserted,
        report.updated,
        report.skipped_bad_json,
        report.skipped_missing_fields,
    );
    print_tier_breakdown(&store)?;
    Ok(())
}

fn cmd_status(db: &PathBuf, source: Option<&str>) -> Result<()> {
    let store = Store::open(db).with_context(|| format!("opening {}", db.display()))?;
    if let Some(sid) = source {
        let s = store
            .get_source(sid)?
            .with_context(|| format!("no source_id={sid}"))?;
        println!("{:#?}", s);
    } else {
        println!("total sources: {}", store.source_count()?);
        print_tier_breakdown(&store)?;
    }
    Ok(())
}

fn print_tier_breakdown(store: &Store) -> Result<()> {
    eprintln!("tier breakdown:");
    for (tier, count) in store.source_count_by_tier()? {
        eprintln!("  T{tier}: {count} sources");
    }
    Ok(())
}
