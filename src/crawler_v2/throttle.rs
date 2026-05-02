//! Per-domain politeness primitive.
//!
//! Two guarantees per domain:
//!   1. **At most one in-flight request.** A `tokio::sync::Semaphore` of
//!      capacity 1 per domain serializes fetches. Concurrency across domains
//!      is unbounded (the worker pool caps it at N workers).
//!   2. **Minimum interval between consecutive requests.** A recorded
//!      `last_fetched` timestamp plus `tokio::time::sleep` enforces a polite
//!      gap (default 1s + up to 300ms jitter).
//!
//! The throttle is cheap to clone (Arc inside); the daemon constructs one
//! instance and shares it across all workers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

#[derive(Debug, Clone, Copy)]
pub struct ThrottleConfig {
    pub min_interval: Duration,
    pub jitter: Duration,
}

impl Default for ThrottleConfig {
    fn default() -> Self {
        Self {
            min_interval: Duration::from_millis(1000),
            jitter: Duration::from_millis(300),
        }
    }
}

#[derive(Default)]
struct Entry {
    sem: Option<Arc<Semaphore>>,
    last_fetched: Option<Instant>,
}

#[derive(Clone)]
pub struct DomainThrottle {
    state: Arc<Mutex<HashMap<String, Entry>>>,
    config: ThrottleConfig,
}

impl DomainThrottle {
    pub fn new(config: ThrottleConfig) -> Self {
        Self {
            state: Arc::new(Mutex::new(HashMap::new())),
            config,
        }
    }

    /// Acquire the per-domain permit and wait any required politeness
    /// interval. The returned permit MUST be held for the lifetime of the
    /// fetch + parse; dropping it releases the next waiter on this domain.
    pub async fn wait(&self, domain: &str) -> OwnedSemaphorePermit {
        // 1. Get (or create) the semaphore for this domain. Short-lived
        //    std Mutex: no .await while holding it.
        let sem = {
            let mut g = self.state.lock().expect("throttle state poisoned");
            let entry = g.entry(domain.to_string()).or_default();
            entry
                .sem
                .get_or_insert_with(|| Arc::new(Semaphore::new(1)))
                .clone()
        };

        // 2. Serialize same-domain fetches.
        let permit = sem.acquire_owned().await.expect("semaphore closed");

        // 3. Enforce polite interval. Compute under the lock, then sleep.
        let wait_until = {
            let g = self.state.lock().expect("throttle state poisoned");
            g.get(domain)
                .and_then(|e| e.last_fetched)
                .map(|last| last + self.config.min_interval + random_jitter(self.config.jitter))
        };
        if let Some(target) = wait_until {
            let now = Instant::now();
            if target > now {
                tokio::time::sleep(target - now).await;
            }
        }

        // 4. Stamp the new fetch time.
        {
            let mut g = self.state.lock().expect("throttle state poisoned");
            g.entry(domain.to_string()).or_default().last_fetched = Some(Instant::now());
        }

        permit
    }

    /// Forget all recorded state (useful in tests).
    #[cfg(test)]
    pub fn reset(&self) {
        self.state.lock().expect("poisoned").clear();
    }
}

fn random_jitter(max: Duration) -> Duration {
    if max.is_zero() {
        return Duration::ZERO;
    }
    let nanos = rand::random::<u64>() % max.as_nanos() as u64;
    Duration::from_nanos(nanos)
}
