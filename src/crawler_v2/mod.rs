//! crawler_v2 — the Rust daemon that replaces scripts/crawl_sources.py.
//!
//! Full design in `CRAWLER.md` at the repo root. This module is the library
//! core; the `crawl` binary (src/bin/crawl.rs) is the thin CLI shell.
//!
//! Phase 1 (this commit) ships:
//!   - types.rs     : Source / Document / FetchEvent / SourceStatus / DocType
//!   - store.rs     : rusqlite-backed persistence + schema migrations
//!   - registry.rs  : sync corpora/sources_tiered.jsonl into SQLite
//!
//! Later phases add frontier/url (#37), fetch/parse (#38), chromium (#39),
//! recipes (#40), worker/pool (#41), health (#42), daemon (#43).

pub mod agent;
pub mod blobs;
pub mod chunk;
pub mod daemon;
pub mod fetch;
pub mod frontier;
pub mod health;
pub mod language;
pub mod legacy_import;
pub mod parse;
pub mod pool;
pub mod recipe;
pub mod registry;
pub mod repair;
pub mod shell_detect;
pub mod store;
pub mod text_extract;
pub mod throttle;
pub mod types;
pub mod url;
pub mod worker;

pub use agent::{
    build_prompt, parse_recipe_from_output, AgentContext, AgentError, AgentProposal, AgentRuntime,
    ClaudeCodeAgent, ClaudeCodeConfig, ExampleRecipe,
};
pub use blobs::BlobStore;
pub use chunk::{chunk_text, is_internally_repetitive, Chunk, ChunkConfig};
pub use daemon::{
    acquire_pid_lock, run_until, CrawlerTickHandler, DaemonConfig, DaemonError, NoopHandler,
    PidLock, StopCondition, TickHandler, TickReport,
};
pub use fetch::{FetchConfig, FetchError, FetchResponse, Fetcher};
pub use frontier::{Frontier, FrontierItem};
pub use health::{
    decide_verdict, evaluate_health, HealthMetrics, HealthVerdict, DEAD_CONSECUTIVE_FAILURES,
    DEFAULT_WINDOW_DAYS, DORMANT_ERROR_RATE, MIN_CYCLES_FOR_VERDICT,
    PREVIOUSLY_PRODUCTIVE_THRESHOLD, STRUCTURAL_FAILURE_STREAK,
};
pub use language::{classify as classify_language, substantive_chars, Language};
pub use legacy_import::{import_legacy, ImportError, ImportOptions, ImportReport};
pub use parse::{parse, parse_html, BinaryKind, ParsedDoc, ParsedHtml};
pub use pool::{Pool, PoolError};
pub use recipe::{load_recipe, load_recipe_strict, Recipe, RecipeError};
pub use registry::{sync_registry, RegistrySyncReport};
pub use repair::{
    apply_recipe, dispatch_one, dry_run_recipe, react_to_verdict, ApplyError, ApplyOptions,
    ApplyOutcome, DispatchError, DispatchOptions, DispatchOutcome, DryRunResult, GitAuthor,
    ReactionOutcome,
};
pub use shell_detect::{evaluate as evaluate_shell, ShellVerdict};
pub use store::{Store, StoreError};
pub use text_extract::{extract as extract_text, ExtractError, ExtractStatus, ExtractedText};
pub use throttle::{DomainThrottle, ThrottleConfig};
pub use types::{
    DocType, Document, FetchEvent, PersistedHealth, PollCycle, RepairItem, RepairStatus, Source,
    SourceStatus, Tier,
};
pub use url::{canonicalize, classify, score, same_site, ContentClass, Rejection};
pub use worker::{StopReason, Worker, WorkerError, WorkerReport};
