//! Multi-source orchestration. Spawns per-source [`Worker`] tasks with a
//! global concurrency cap; each task opens its own [`Store`] connection
//! (rusqlite is Send but !Sync) and runs a full polling cycle.
//!
//! ## Concurrency model
//!
//! N concurrent workers, each owning one source for its whole cycle. The
//! per-source throttle serializes same-domain requests *within* a worker;
//! the pool's semaphore caps *how many sources are in flight at once*.
//!
//! Since `source_id` is keyed by domain, no two active workers ever hit
//! the same domain simultaneously. The per-domain throttle in [`crate::crawler_v2::throttle`]
//! is load-bearing only on weird data (e.g., source_id misalignment or
//! discovered cross-source links — which we *don't* follow) but costs
//! nothing to keep correct.

use super::blobs::BlobStore;
use super::fetch::Fetcher;
use super::recipe::load_recipe;
use super::store::{Store, StoreError};
use super::throttle::DomainThrottle;
use super::worker::{Worker, WorkerError, WorkerReport};
use chrono::Utc;
use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;
use tokio::sync::Semaphore;

#[derive(Debug, Error)]
pub enum PoolError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("worker: {0}")]
    Worker(#[from] WorkerError),
    #[error("no such source: {0}")]
    UnknownSource(String),
    #[error("tokio join: {0}")]
    Join(#[from] tokio::task::JoinError),
}

pub struct Pool {
    db_path: PathBuf,
    recipes_dir: PathBuf,
    blobs: Arc<BlobStore>,
    fetcher: Arc<Fetcher>,
    throttle: Arc<DomainThrottle>,
}

impl Pool {
    pub fn new(
        db_path: PathBuf,
        recipes_dir: PathBuf,
        blobs: Arc<BlobStore>,
        fetcher: Arc<Fetcher>,
        throttle: Arc<DomainThrottle>,
    ) -> Self {
        Self {
            db_path,
            recipes_dir,
            blobs,
            fetcher,
            throttle,
        }
    }

    /// Poll one source by id. The caller is responsible for knowing the
    /// source exists; missing IDs surface as [`PoolError::UnknownSource`].
    pub async fn poll_source(&self, source_id: &str) -> Result<WorkerReport, PoolError> {
        let store = Store::open(&self.db_path)?;
        let source = store
            .get_source(source_id)?
            .ok_or_else(|| PoolError::UnknownSource(source_id.to_string()))?;
        let recipe = load_recipe(&source, &self.recipes_dir);
        let worker = Worker::new(
            source,
            recipe,
            self.fetcher.clone(),
            self.throttle.clone(),
            self.blobs.clone(),
            store,
        );
        Ok(worker.poll().await?)
    }

    /// Poll every source whose `next_poll_at` has elapsed (or is null).
    /// Runs up to `max_concurrent` workers in parallel; the rest wait on
    /// the semaphore.
    pub async fn poll_all_due(&self, max_concurrent: usize) -> Result<Vec<WorkerReport>, PoolError> {
        let due = {
            let store = Store::open(&self.db_path)?;
            store.list_sources_due(Utc::now())?
        };
        if due.is_empty() {
            return Ok(Vec::new());
        }

        let sem = Arc::new(Semaphore::new(max_concurrent.max(1)));
        let mut handles = Vec::with_capacity(due.len());

        for source in due {
            let sem = sem.clone();
            let db_path = self.db_path.clone();
            let recipes_dir = self.recipes_dir.clone();
            let blobs = self.blobs.clone();
            let fetcher = self.fetcher.clone();
            let throttle = self.throttle.clone();
            let source_id = source.source_id.clone();

            let h = tokio::spawn(async move {
                let _permit = match sem.acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => return Err(PoolError::UnknownSource(source_id)),
                };
                let store = Store::open(&db_path)?;
                let recipe = load_recipe(&source, &recipes_dir);
                let worker = Worker::new(source, recipe, fetcher, throttle, blobs, store);
                Ok::<WorkerReport, PoolError>(worker.poll().await?)
            });
            handles.push(h);
        }

        let mut reports = Vec::with_capacity(handles.len());
        for h in handles {
            match h.await {
                Ok(Ok(r)) => reports.push(r),
                Ok(Err(e)) => eprintln!("pool: worker error: {e}"),
                Err(e) => eprintln!("pool: join error: {e}"),
            }
        }
        Ok(reports)
    }
}
