//! Long-running daemon for the crawler. Owns the tick loop that drives
//! `poll_all_due` → `recompute_health` → `drain_repair_queue` (Phase 7.1
//! wires those in; this module ships the **framework**: tick interval,
//! PID-file lock, graceful SIGINT/SIGTERM shutdown, `--max-ticks` for
//! testability).
//!
//! ## Invariants
//!
//! 1. **One daemon per (state_dir, db) tuple.** The PID file at
//!    `<state_dir>/daemon.pid` enforces this. If a previous daemon's PID
//!    is still alive we refuse to start; if it's stale (process gone) we
//!    overwrite.
//! 2. **Ticks never overlap.** Each tick runs to completion before the
//!    next interval starts. If a tick takes longer than the interval, the
//!    next one fires immediately after — we don't queue up backlog.
//! 3. **Shutdown is graceful.** A SIGINT/SIGTERM mid-tick lets the current
//!    tick finish before the daemon exits. Signal received between ticks
//!    exits immediately. We never abandon partial work.
//! 4. **Per-tick errors are logged but don't kill the daemon.** A SQLite
//!    hiccup or a network blip on one source must not take down 876
//!    others' polling.
//!
//! ## Test interface
//!
//! `run_until` takes a [`StopCondition`]: `Signal` (production), or
//! `MaxTicks(N)` (tests). The same code path is exercised either way; the
//! only difference is what makes the loop exit.

use super::agent::AgentRuntime;
use super::health::{evaluate_health, HealthVerdict};
use super::pool::Pool;
use super::repair::{dispatch_one, DispatchOptions, DispatchOutcome};
use super::store::Store;
use chrono::Utc;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
use tokio::time::interval;

#[derive(Debug, Error)]
pub enum DaemonError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("another daemon (pid {0}) is already running; refusing to start")]
    AlreadyRunning(i32),
    #[error("invalid PID file at {path}: {reason}")]
    InvalidPidFile { path: String, reason: String },
}

/// What ends the daemon loop.
#[derive(Debug, Clone, Copy)]
pub enum StopCondition {
    /// Run forever; exit on SIGINT/SIGTERM.
    Signal,
    /// Run exactly `n` ticks then exit cleanly. Used in tests so the loop
    /// is deterministic.
    MaxTicks(u32),
}

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    /// How often to fire `tick`. Production default 60s; tests use 100ms.
    pub tick_interval: Duration,
    /// Where to keep the PID file. Created if missing.
    pub state_dir: PathBuf,
    /// What ends the loop.
    pub stop_condition: StopCondition,
}

impl DaemonConfig {
    pub fn production(state_dir: impl Into<PathBuf>) -> Self {
        Self {
            tick_interval: Duration::from_secs(60),
            state_dir: state_dir.into(),
            stop_condition: StopCondition::Signal,
        }
    }
}

/// Outcome of a single tick. The `Daemon::run_until` driver doesn't branch
/// on this directly — it's exposed for log + test inspection.
#[derive(Debug, Clone, Default)]
pub struct TickReport {
    pub tick_index: u32,
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: chrono::DateTime<chrono::Utc>,
    pub elapsed_ms: u64,
    /// Phase 7.1 will fill these. For 7.0 they're zero.
    pub sources_polled: u32,
    pub health_evaluated: u32,
    pub repairs_drained: u32,
    pub errors: Vec<String>,
}

/// Per-tick callback. Returning a `TickReport` lets the daemon log + tests
/// assert on what happened. Errors inside the tick body should be captured
/// in `report.errors` rather than bubbled — a tick that returns `Err` is a
/// hard daemon-level failure (SQLite corruption etc.) and exits the loop.
#[async_trait::async_trait]
pub trait TickHandler: Send + Sync {
    async fn tick(&self, tick_index: u32) -> Result<TickReport, DaemonError>;
}

/// No-op handler — for Phase 7.0 startup tests + as a sanity baseline. The
/// real handler ships in Phase 7.1 wiring poll/health/drain.
pub struct NoopHandler;

#[async_trait::async_trait]
impl TickHandler for NoopHandler {
    async fn tick(&self, tick_index: u32) -> Result<TickReport, DaemonError> {
        let now = Utc::now();
        Ok(TickReport {
            tick_index,
            started_at: now,
            finished_at: now,
            elapsed_ms: 0,
            ..Default::default()
        })
    }
}

/// Acquire the daemon's exclusive PID lock.
///
/// Behaviour:
///   - If `<state_dir>/daemon.pid` doesn't exist: write our PID, return Ok.
///   - If the file exists and the PID is alive: return `AlreadyRunning`.
///   - If the file exists and the PID is dead: overwrite, return Ok.
pub fn acquire_pid_lock(state_dir: &Path) -> Result<PidLock, DaemonError> {
    std::fs::create_dir_all(state_dir)?;
    let path = state_dir.join("daemon.pid");
    if let Ok(contents) = std::fs::read_to_string(&path) {
        let pid: i32 = contents
            .trim()
            .parse()
            .map_err(|_| DaemonError::InvalidPidFile {
                path: path.display().to_string(),
                reason: format!("non-integer contents: {contents:?}"),
            })?;
        if pid_is_alive(pid) {
            return Err(DaemonError::AlreadyRunning(pid));
        }
        // Stale — fall through and overwrite.
    }
    let our_pid = std::process::id() as i32;
    std::fs::write(&path, our_pid.to_string())?;
    Ok(PidLock {
        path,
        owner_pid: our_pid,
    })
}

pub struct PidLock {
    path: PathBuf,
    owner_pid: i32,
}

impl Drop for PidLock {
    fn drop(&mut self) {
        // Best-effort cleanup — only delete if we still own it. A second
        // daemon that detected staleness and took over would have a
        // different owner_pid in the file.
        if let Ok(contents) = std::fs::read_to_string(&self.path) {
            if contents.trim().parse::<i32>().ok() == Some(self.owner_pid) {
                let _ = std::fs::remove_file(&self.path);
            }
        }
    }
}

/// Cross-platform "is this PID alive?". Linux/macOS use `kill -0` semantics
/// via `libc::kill(pid, 0)`. Without `libc` as a dep we shell out to `ps`.
fn pid_is_alive(pid: i32) -> bool {
    // `kill -0 <pid>` returns 0 if alive, non-zero otherwise. POSIX-portable.
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Run the tick loop until `stop_condition` triggers. Holds the PID lock
/// for the duration; releases on graceful exit.
pub async fn run_until(
    handler: &dyn TickHandler,
    config: &DaemonConfig,
) -> Result<u32, DaemonError> {
    let _lock = acquire_pid_lock(&config.state_dir)?;
    let stop = Arc::new(AtomicBool::new(false));

    // Wire signal handling — only when running until signal, since tests
    // shouldn't install Ctrl-C handlers in the runtime.
    if matches!(config.stop_condition, StopCondition::Signal) {
        spawn_signal_watcher(Arc::clone(&stop));
    }

    let mut interval = interval(config.tick_interval);
    // First tick fires immediately. interval()'s default behavior already
    // is to fire on first .tick(); leave it that way so a fresh start
    // begins working without delay.

    let mut tick_index: u32 = 0;
    loop {
        interval.tick().await;
        if stop.load(Ordering::Relaxed) {
            eprintln!("[daemon] stop signal received before tick {tick_index}; exiting");
            break;
        }
        let report = handler.tick(tick_index).await?;
        log_tick(&report);
        tick_index += 1;
        if let StopCondition::MaxTicks(n) = config.stop_condition {
            if tick_index >= n {
                break;
            }
        }
    }

    Ok(tick_index)
}

fn spawn_signal_watcher(stop: Arc<AtomicBool>) {
    tokio::spawn(async move {
        // tokio::signal::ctrl_c covers SIGINT (and SIGBREAK on Windows).
        // SIGTERM needs the signal_unix path; we install both on Unix.
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut sigterm = signal(SignalKind::terminate()).expect("install SIGTERM handler");
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {
                    eprintln!("[daemon] SIGINT received");
                }
                _ = sigterm.recv() => {
                    eprintln!("[daemon] SIGTERM received");
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
            eprintln!("[daemon] Ctrl-C received");
        }
        stop.store(true, Ordering::Relaxed);
    });
}

fn log_tick(report: &TickReport) {
    eprintln!(
        "[daemon] tick={} elapsed={}ms polled={} health={} repairs={} errors={}",
        report.tick_index,
        report.elapsed_ms,
        report.sources_polled,
        report.health_evaluated,
        report.repairs_drained,
        report.errors.len(),
    );
    for err in &report.errors {
        eprintln!("[daemon]   error: {err}");
    }
}

// ----- CrawlerTickHandler ----------------------------------------------------

/// The real per-tick body for the crawler daemon. Wires three pipeline
/// stages in order:
///   1. **Poll** every source whose `next_poll_at <= now`. Worker writes
///      poll_cycles + documents + fetch_events.
///   2. **Health pass** (every Nth tick) re-evaluates all active sources
///      and reacts to verdicts (status flips, repair-queue inserts).
///   3. **Repair drain** loops `dispatch_one` until the queue is empty
///      (capped at `max_repairs_per_tick` to avoid one stuck source
///      hogging a tick).
///
/// All three steps are wrapped in their own error capture so a SQLite
/// blip in one doesn't kill the daemon. Errors land in `report.errors`
/// and the daemon log surfaces them; the loop continues.
pub struct CrawlerTickHandler {
    pool: Arc<Pool>,
    db_path: PathBuf,
    poll_concurrency: usize,
    health_window: Duration,
    health_every_n_ticks: u32,
    max_repairs_per_tick: u32,
    dispatch_opts: DispatchOptions,
    agent: Arc<dyn AgentRuntime>,
}

impl CrawlerTickHandler {
    /// `health_every_n_ticks = 1` for prod; tests typically pass 1 too.
    /// Set to `0` to disable the health pass entirely (useful when running
    /// the daemon purely as a poller).
    pub fn new(
        pool: Arc<Pool>,
        db_path: PathBuf,
        poll_concurrency: usize,
        health_window: Duration,
        health_every_n_ticks: u32,
        max_repairs_per_tick: u32,
        dispatch_opts: DispatchOptions,
        agent: Arc<dyn AgentRuntime>,
    ) -> Self {
        Self {
            pool,
            db_path,
            poll_concurrency,
            health_window,
            health_every_n_ticks,
            max_repairs_per_tick,
            dispatch_opts,
            agent,
        }
    }

    fn open_store(&self) -> Result<Store, DaemonError> {
        Store::open(&self.db_path).map_err(|e| {
            DaemonError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("open store {}: {e}", self.db_path.display()),
            ))
        })
    }

    async fn run_health_pass(&self, errors: &mut Vec<String>) -> u32 {
        let store = match self.open_store() {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("health: open_store: {e}"));
                return 0;
            }
        };
        let sources = match store.list_sources() {
            Ok(s) => s,
            Err(e) => {
                errors.push(format!("health: list_sources: {e}"));
                return 0;
            }
        };
        let active: Vec<_> = sources
            .into_iter()
            .filter(|s| {
                use super::types::SourceStatus;
                matches!(s.status, SourceStatus::Active | SourceStatus::JsOnly)
            })
            .collect();

        let now = Utc::now();
        let mut evaluated = 0u32;
        for source in active {
            let metrics =
                match evaluate_health(&store, &source, chrono::Duration::from_std(self.health_window).unwrap_or(chrono::Duration::days(7)), now) {
                    Ok(m) => m,
                    Err(e) => {
                        errors.push(format!(
                            "health[{}]: evaluate: {e}",
                            source.source_id
                        ));
                        continue;
                    }
                };
            if let Err(e) = store.upsert_source_health(&metrics) {
                errors.push(format!(
                    "health[{}]: upsert_source_health: {e}",
                    source.source_id
                ));
                continue;
            }
            // React only on non-Healthy outcomes — keeps logs quiet for the
            // common case.
            if !matches!(metrics.verdict, HealthVerdict::Healthy | HealthVerdict::InsufficientData) {
                if let Err(e) = super::repair::react_to_verdict(&store, &source, &metrics, now) {
                    errors.push(format!(
                        "health[{}]: react_to_verdict: {e}",
                        source.source_id
                    ));
                    continue;
                }
            }
            evaluated += 1;
        }
        evaluated
    }

    async fn drain_repairs(&self, errors: &mut Vec<String>) -> u32 {
        let mut drained = 0u32;
        loop {
            match dispatch_one(&self.db_path, &*self.agent, &self.dispatch_opts, Utc::now())
                .await
            {
                Ok(DispatchOutcome::NoPending) => break,
                Ok(_) => {
                    drained += 1;
                }
                Err(e) => {
                    errors.push(format!("dispatch: {e}"));
                    break;
                }
            }
            if drained >= self.max_repairs_per_tick {
                errors.push(format!(
                    "repair queue drain capped at {} per tick",
                    self.max_repairs_per_tick
                ));
                break;
            }
        }
        drained
    }
}

#[async_trait::async_trait]
impl TickHandler for CrawlerTickHandler {
    async fn tick(&self, tick_index: u32) -> Result<TickReport, DaemonError> {
        let started = Utc::now();
        let mut report = TickReport {
            tick_index,
            started_at: started,
            ..Default::default()
        };

        match self.pool.poll_all_due(self.poll_concurrency).await {
            Ok(reports) => report.sources_polled = reports.len() as u32,
            Err(e) => report.errors.push(format!("poll_all_due: {e}")),
        }

        if self.health_every_n_ticks > 0 && tick_index % self.health_every_n_ticks == 0 {
            report.health_evaluated = self.run_health_pass(&mut report.errors).await;
        }

        report.repairs_drained = self.drain_repairs(&mut report.errors).await;

        let finished = Utc::now();
        report.finished_at = finished;
        report.elapsed_ms = (finished - started).num_milliseconds().max(0) as u64;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn run_until_max_ticks_exits_after_n_ticks() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = DaemonConfig {
            tick_interval: Duration::from_millis(50),
            state_dir: dir.path().to_path_buf(),
            stop_condition: StopCondition::MaxTicks(3),
        };
        let n = run_until(&NoopHandler, &cfg).await.unwrap();
        assert_eq!(n, 3);
    }

    #[tokio::test]
    async fn pid_lock_refuses_concurrent_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let _first = acquire_pid_lock(dir.path()).expect("first lock");
        let r = acquire_pid_lock(dir.path());
        assert!(matches!(r, Err(DaemonError::AlreadyRunning(_))));
    }

    #[tokio::test]
    async fn pid_lock_overwrites_stale_pidfile() {
        let dir = tempfile::tempdir().unwrap();
        // Fake a PID file claiming PID 1 — process 1 is init/launchd which
        // is always alive on macOS, so we can't use that. Use a clearly
        // dead-pid placeholder (4-byte signed max, which `kill -0` will
        // reject as ESRCH).
        std::fs::write(dir.path().join("daemon.pid"), "2147483646").unwrap();
        let lock = acquire_pid_lock(dir.path()).expect("should overtake stale pid");
        // After acquire, the file contains our PID.
        let contents = std::fs::read_to_string(dir.path().join("daemon.pid")).unwrap();
        assert_eq!(contents.trim(), std::process::id().to_string());
        drop(lock);
    }

    #[tokio::test]
    async fn pid_lock_drop_removes_file() {
        let dir = tempfile::tempdir().unwrap();
        let pid_path = dir.path().join("daemon.pid");
        let lock = acquire_pid_lock(dir.path()).unwrap();
        assert!(pid_path.exists());
        drop(lock);
        assert!(!pid_path.exists());
    }

    /// Counting handler — verifies the tick callback fires the right number
    /// of times and receives a monotonically increasing tick_index.
    struct CountingHandler {
        seen: std::sync::Mutex<Vec<u32>>,
    }

    #[async_trait::async_trait]
    impl TickHandler for CountingHandler {
        async fn tick(&self, tick_index: u32) -> Result<TickReport, DaemonError> {
            self.seen.lock().unwrap().push(tick_index);
            Ok(TickReport {
                tick_index,
                started_at: Utc::now(),
                finished_at: Utc::now(),
                ..Default::default()
            })
        }
    }

    #[tokio::test]
    async fn tick_index_is_monotonic_starting_from_zero() {
        let dir = tempfile::tempdir().unwrap();
        let handler = CountingHandler {
            seen: std::sync::Mutex::new(Vec::new()),
        };
        let cfg = DaemonConfig {
            tick_interval: Duration::from_millis(20),
            state_dir: dir.path().to_path_buf(),
            stop_condition: StopCondition::MaxTicks(5),
        };
        run_until(&handler, &cfg).await.unwrap();
        let seen = handler.seen.lock().unwrap().clone();
        assert_eq!(seen, vec![0, 1, 2, 3, 4]);
    }
}
