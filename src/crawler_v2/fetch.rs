//! Async HTTP fetch for the crawler.
//!
//! Plain reqwest; chromiumoxide path lives in a separate module (Phase 3b).
//!
//! Behavior:
//!   - TLS-tolerant (many .gov.np certs are expired). Configurable; default on.
//!   - Bounded body size (default 50 MB) via streamed reads; the fetcher
//!     never holds unbounded bytes in memory.
//!   - Per-request timeout (default 30s) applies to connect + TLS + body.
//!   - Redirects followed (up to 5) by default.
//!   - 2xx–4xx responses return `Ok(FetchResponse)` with the status attached
//!     so the caller can decide (e.g., 410 → mark dead, 404 → record miss).
//!     Only transport errors surface as [`FetchError`].
//!   - Cookies disabled, Referer disabled — we don't want to leak our
//!     crawl pattern and we don't need to maintain sessions.
//!
//! The fetcher takes no rate limit itself. Politeness is the caller's job
//! via [`crate::crawler_v2::throttle::DomainThrottle`] — that separation lets
//! tests exercise fetch deterministically and lets one throttle guard many
//! fetchers (if we ever run multiple).

use std::time::{Duration, Instant};
use thiserror::Error;

pub const DEFAULT_USER_AGENT: &str = "gemma-god-crawler/0.1 (+Nepal gov RAG; \
     contact github.com/voidash/gemma-god)";
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone)]
pub struct FetchConfig {
    pub user_agent: String,
    pub max_bytes: u64,
    pub timeout: Duration,
    pub accept_invalid_certs: bool,
    pub max_redirects: usize,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            user_agent: DEFAULT_USER_AGENT.into(),
            max_bytes: DEFAULT_MAX_BYTES,
            timeout: DEFAULT_TIMEOUT,
            accept_invalid_certs: true,
            max_redirects: 5,
        }
    }
}

#[derive(Debug, Clone)]
pub struct FetchResponse {
    /// Final URL after any redirects (canonicalize before storing).
    pub final_url: String,
    pub status: u16,
    pub content_type: String,
    pub body: Vec<u8>,
    pub elapsed_ms: u32,
    /// True when we stopped reading because we hit `max_bytes`. Body is
    /// truncated but still returned — PDF extractors and parsers can
    /// sometimes make do.
    pub truncated: bool,
}

#[derive(Debug, Error)]
pub enum FetchError {
    #[error("timeout after {secs}s")]
    Timeout { secs: u64 },
    #[error("network: {0}")]
    Network(String),
    #[error("tls: {0}")]
    Tls(String),
    #[error("body read: {0}")]
    BodyRead(String),
    #[error("build: {0}")]
    BuildClient(String),
    #[error("bad url: {0}")]
    BadUrl(String),
    #[error("other: {0}")]
    Other(String),
}

pub struct Fetcher {
    client: reqwest::Client,
    config: FetchConfig,
}

impl Fetcher {
    pub fn new(config: FetchConfig) -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .user_agent(&config.user_agent)
            .timeout(config.timeout)
            .danger_accept_invalid_certs(config.accept_invalid_certs)
            .redirect(reqwest::redirect::Policy::limited(config.max_redirects))
            .referer(false)
            // No cookie store: reqwest's `cookies` feature is disabled in
            // our Cargo.toml, so cookies aren't persisted anyway. Leaving
            // the builder line out (it would require the feature to compile).
            .gzip(true)
            .brotli(true)
            .build()
            .map_err(|e| FetchError::BuildClient(e.to_string()))?;
        Ok(Self { client, config })
    }

    pub async fn fetch(&self, url: &str) -> Result<FetchResponse, FetchError> {
        let t0 = Instant::now();

        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(classify_reqwest_error(self.config.timeout))?;

        let status = resp.status().as_u16();
        let final_url = resp.url().to_string();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .map(|s| s.split(';').next().unwrap_or(s).trim().to_ascii_lowercase())
            .unwrap_or_default();

        // Stream body, capping at max_bytes. We reserve by declared
        // Content-Length only if it's within the cap, to avoid one huge
        // allocation for a truncated response.
        let declared = resp.content_length().unwrap_or(0);
        let cap = self.config.max_bytes;
        let mut body = Vec::with_capacity(declared.min(cap) as usize);
        let mut truncated = false;

        let mut stream = resp;
        while let Some(chunk) = stream
            .chunk()
            .await
            .map_err(|e| FetchError::BodyRead(e.to_string()))?
        {
            if (body.len() as u64).saturating_add(chunk.len() as u64) > cap {
                let remaining = cap.saturating_sub(body.len() as u64) as usize;
                body.extend_from_slice(&chunk[..remaining]);
                truncated = true;
                break;
            }
            body.extend_from_slice(&chunk);
        }

        Ok(FetchResponse {
            final_url,
            status,
            content_type,
            body,
            elapsed_ms: t0.elapsed().as_millis().min(u32::MAX as u128) as u32,
            truncated,
        })
    }
}

fn classify_reqwest_error(
    timeout: Duration,
) -> impl FnOnce(reqwest::Error) -> FetchError {
    move |e| {
        if e.is_timeout() {
            return FetchError::Timeout {
                secs: timeout.as_secs(),
            };
        }
        if e.is_connect() {
            return FetchError::Network(e.to_string());
        }
        let s = e.to_string();
        if s.to_ascii_lowercase().contains("tls")
            || s.to_ascii_lowercase().contains("certificate")
        {
            return FetchError::Tls(s);
        }
        if e.is_request() || e.is_body() {
            return FetchError::BodyRead(s);
        }
        if e.is_builder() {
            return FetchError::BadUrl(s);
        }
        FetchError::Other(s)
    }
}
