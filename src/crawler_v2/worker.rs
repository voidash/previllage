//! Per-source crawl loop. One Worker is spawned per source per polling cycle.
//!
//! Responsibilities:
//!   1. Seed the frontier from `recipe.entry_points`
//!   2. Pop highest-priority URL, respect per-domain throttle, fetch
//!   3. Parse the response; on HTML, run shell detection
//!   4. Persist raw blob, extracted text, and a `documents` row (with diff
//!      detection yielding Inserted / Unchanged / Superseded)
//!   5. Enqueue discovered outbound links after applying recipe filters
//!      (same-site, deny_paths, allow_paths, allowed_subdomains)
//!   6. Stop on any budget cap and record a [`StopReason`]
//!   7. Write the post-cycle source lifecycle updates (last_polled_at,
//!      consecutive_failures, last_changed_at, next_poll_at)
//!
//! Error semantics: network/HTTP-level failures bump `consecutive_failures`
//! and become `fetch_events` rows but don't abort the cycle unless we hit
//! the 20-in-a-row ceiling. Store-layer errors abort the cycle (they mean
//! the daemon state is broken and continuing would produce corrupt data).

use super::blobs::BlobStore;
use super::fetch::{FetchError, FetchResponse, Fetcher};
use super::frontier::Frontier;
use super::parse::{parse, BinaryKind, ParsedDoc, ParsedHtml};
use super::recipe::Recipe;
use super::shell_detect;
use super::store::{DocumentOutcome, Store, StoreError};
use super::throttle::DomainThrottle;
use super::types::{DocType, Document, FetchEvent, PollCycle, Source, SourceStatus};
use super::url as crawler_url;
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::sync::Arc;
use std::time::Instant;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("fatal: {0}")]
    Fatal(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// Frontier drained naturally — the full reachable set was crawled.
    FrontierDrained,
    /// Hit the per-source HTML page cap.
    HtmlCap,
    /// Hit the per-source total-fetches cap (all categories).
    TotalCap,
    /// Hit the wall-clock time cap.
    ElapsedCap,
    /// 20+ consecutive fetch failures; source likely dead or blocking us.
    ConsecutiveFailures,
    /// Homepage shell-detected; marked js_only and aborted.
    ShellDetected,
}

impl StopReason {
    pub fn as_str(self) -> &'static str {
        match self {
            StopReason::FrontierDrained => "frontier_drained",
            StopReason::HtmlCap => "html_cap",
            StopReason::TotalCap => "total_cap",
            StopReason::ElapsedCap => "elapsed_cap",
            StopReason::ConsecutiveFailures => "consecutive_failures",
            StopReason::ShellDetected => "shell_detected",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct WorkerReport {
    pub source_id: String,
    pub stop_reason: StopReason,
    pub elapsed_sec: u64,
    pub html_fetched: u32,
    pub binaries_fetched: u32,
    pub other_fetched: u32,
    pub errors: u32,
    pub docs_inserted: u32,
    pub docs_superseded: u32,
    pub docs_unchanged: u32,
    pub shell_flagged: bool,
}

pub struct Worker {
    source: Source,
    recipe: Recipe,
    fetcher: Arc<Fetcher>,
    throttle: Arc<DomainThrottle>,
    blobs: Arc<BlobStore>,
    store: Store,
}

impl Worker {
    pub fn new(
        source: Source,
        recipe: Recipe,
        fetcher: Arc<Fetcher>,
        throttle: Arc<DomainThrottle>,
        blobs: Arc<BlobStore>,
        store: Store,
    ) -> Self {
        Self {
            source,
            recipe,
            fetcher,
            throttle,
            blobs,
            store,
        }
    }

    pub async fn poll(mut self) -> Result<WorkerReport, WorkerError> {
        let started = Instant::now();
        let now = Utc::now();

        // Seed the frontier from entry_points. Each entry point enters at
        // depth 0 with the base score (adjusted by its classification).
        let mut frontier = Frontier::new();
        for ep in &self.recipe.entry_points {
            match crawler_url::canonicalize(ep, ep) {
                Ok(canon) => {
                    let s = crawler_url::score(&canon, 0);
                    frontier.push(canon, 0, s);
                }
                Err(e) => {
                    eprintln!(
                        "worker[{}]: skipping bad entry_point {ep}: {e:?}",
                        self.source.source_id
                    );
                }
            }
        }

        let mut html_fetched = 0u32;
        let mut binaries_fetched = 0u32;
        let mut other_fetched = 0u32;
        let mut errors = 0u32;
        let mut docs_inserted = 0u32;
        let mut docs_superseded = 0u32;
        let mut docs_unchanged = 0u32;
        let mut consecutive_failures = 0u32;
        let mut shell_flagged = false;
        let mut any_change = false;

        let stop_reason: StopReason = loop {
            // ---- budget gates (checked before every pop) ----
            if html_fetched >= self.recipe.max_html_fetches {
                break StopReason::HtmlCap;
            }
            let total = html_fetched + binaries_fetched + other_fetched + errors;
            if total >= self.recipe.max_total_fetches {
                break StopReason::TotalCap;
            }
            if started.elapsed().as_secs() >= self.recipe.max_elapsed_sec {
                break StopReason::ElapsedCap;
            }
            if consecutive_failures >= 20 {
                break StopReason::ConsecutiveFailures;
            }

            let item = match frontier.pop() {
                Some(i) => i,
                None => break StopReason::FrontierDrained,
            };

            let domain = match ::url::Url::parse(&item.url).ok().and_then(|u| {
                u.host_str().map(|s| s.to_string())
            }) {
                Some(d) => d,
                None => continue,
            };

            // Per-domain politeness. Drop permit as soon as the fetch
            // returns so the next waiter on this domain can proceed while
            // we parse + persist.
            let permit = self.throttle.wait(&domain).await;
            let fetch_result = self.fetcher.fetch(&item.url).await;
            drop(permit);

            let resp = match fetch_result {
                Ok(r) => {
                    consecutive_failures = 0;
                    self.record_fetch_ok(&item.url, &r)?;
                    r
                }
                Err(e) => {
                    consecutive_failures += 1;
                    errors += 1;
                    self.record_fetch_err(&item.url, &e)?;
                    continue;
                }
            };

            if resp.status >= 400 {
                errors += 1;
                continue;
            }

            let parsed = parse(&resp.content_type, &resp.final_url, &resp.body);
            match parsed {
                ParsedDoc::Html(html) => {
                    let verdict = shell_detect::evaluate(&html);
                    if verdict.is_shell && item.depth == 0 {
                        // Homepage shell — flag + stop. Subsequent poll
                        // cycles will route through the Chromium fetcher
                        // (Phase 3b) via the persisted js_only status.
                        self.store.mark_source_status(
                            &self.source.source_id,
                            SourceStatus::JsOnly,
                        )?;
                        shell_flagged = true;
                        break StopReason::ShellDetected;
                    }

                    let hash = sha256_hex(&resp.body);
                    let raw_path = self
                        .blobs
                        .write_raw(&self.source.source_id, &hash, "html", &resp.body)?;
                    let extracted_path = if html.extracted_text.is_empty() {
                        None
                    } else {
                        Some(self.blobs.write_extracted(
                            &self.source.source_id,
                            &hash,
                            &html.extracted_text,
                        )?)
                    };

                    let doc = Document {
                        doc_id: compute_doc_id(&self.source.source_id, &resp.final_url, &hash),
                        source_id: self.source.source_id.clone(),
                        url: resp.final_url.clone(),
                        content_hash: hash,
                        fetched_at: now,
                        superseded_by: None,
                        removed_at: None,
                        doc_type: DocType::Html,
                        status_code: resp.status as i32,
                        title: html.title.clone(),
                        language: html.lang.clone(),
                        date_published: None,
                        raw_blob_path: self.blobs.to_relative(&raw_path),
                        extracted_text_path: extracted_path
                            .as_ref()
                            .map(|p| self.blobs.to_relative(p)),
                        text_chars: html.extracted_text.chars().count() as u32,
                        size_bytes: resp.body.len() as u64,
                        depth: item.depth,
                        priority_at_fetch: Some(item.score),
                    };

                    match self.store.upsert_document(&doc)? {
                        DocumentOutcome::Inserted => {
                            docs_inserted += 1;
                            any_change = true;
                        }
                        DocumentOutcome::Superseded { .. } => {
                            docs_superseded += 1;
                            any_change = true;
                        }
                        DocumentOutcome::Unchanged => {
                            docs_unchanged += 1;
                        }
                    }
                    html_fetched += 1;

                    // ---- link discovery + recipe filtering ----
                    enqueue_discovered_links(
                        &mut frontier,
                        &self.source,
                        &self.recipe,
                        &html,
                        item.depth,
                    );
                }
                ParsedDoc::Binary { kind, .. } => {
                    let hash = sha256_hex(&resp.body);
                    let ext = binary_ext(kind);
                    let raw_path = self.blobs.write_raw(
                        &self.source.source_id,
                        &hash,
                        ext,
                        &resp.body,
                    )?;
                    let doc = Document {
                        doc_id: compute_doc_id(&self.source.source_id, &resp.final_url, &hash),
                        source_id: self.source.source_id.clone(),
                        url: resp.final_url.clone(),
                        content_hash: hash,
                        fetched_at: now,
                        superseded_by: None,
                        removed_at: None,
                        doc_type: map_binary_kind(kind),
                        status_code: resp.status as i32,
                        title: None,
                        language: None,
                        date_published: None,
                        raw_blob_path: self.blobs.to_relative(&raw_path),
                        extracted_text_path: None,
                        text_chars: 0,
                        size_bytes: resp.body.len() as u64,
                        depth: item.depth,
                        priority_at_fetch: Some(item.score),
                    };
                    match self.store.upsert_document(&doc)? {
                        DocumentOutcome::Inserted => {
                            docs_inserted += 1;
                            any_change = true;
                        }
                        DocumentOutcome::Superseded { .. } => {
                            docs_superseded += 1;
                            any_change = true;
                        }
                        DocumentOutcome::Unchanged => {
                            docs_unchanged += 1;
                        }
                    }
                    binaries_fetched += 1;
                }
                ParsedDoc::Unsupported { .. } => {
                    other_fetched += 1;
                }
            }
        };

        // ---- post-cycle source state updates ----
        let success = errors == 0 || html_fetched + binaries_fetched > 0;
        self.store
            .mark_source_fetch_attempt(&self.source.source_id, now, success)?;
        if any_change {
            self.store
                .mark_source_changed(&self.source.source_id, now)?;
        }
        let next_poll = now
            + chrono::Duration::hours(self.source.poll_interval_hours as i64);
        self.store
            .set_next_poll_at(&self.source.source_id, next_poll)?;

        // ---- persist poll cycle for health analysis (Phase 6) -------------
        let finished = Utc::now();
        let elapsed_sec = started.elapsed().as_secs();
        let cycle = PollCycle {
            cycle_id: None,
            source_id: self.source.source_id.clone(),
            started_at: now,
            finished_at: finished,
            stop_reason: stop_reason.as_str().to_string(),
            elapsed_sec,
            html_fetched,
            binaries_fetched,
            other_fetched,
            errors,
            docs_inserted,
            docs_superseded,
            docs_unchanged,
            shell_flagged,
        };
        // Failure to record telemetry is loud but non-fatal: the actual data
        // is already in `documents` + `fetch_events`. Bubble it up so the
        // daemon log surfaces the disk/SQLite issue.
        self.store.insert_poll_cycle(&cycle)?;

        Ok(WorkerReport {
            source_id: self.source.source_id.clone(),
            stop_reason,
            elapsed_sec,
            html_fetched,
            binaries_fetched,
            other_fetched,
            errors,
            docs_inserted,
            docs_superseded,
            docs_unchanged,
            shell_flagged,
        })
    }

    fn record_fetch_ok(
        &self,
        url: &str,
        resp: &FetchResponse,
    ) -> Result<(), StoreError> {
        self.store.record_fetch_event(&FetchEvent {
            source_id: self.source.source_id.clone(),
            url: url.to_string(),
            fetched_at: Utc::now(),
            status: resp.status as i32,
            elapsed_ms: Some(resp.elapsed_ms),
            error: None,
            doc_type: None,
            bytes: Some(resp.body.len() as u64),
        })?;
        Ok(())
    }

    fn record_fetch_err(&self, url: &str, err: &FetchError) -> Result<(), StoreError> {
        self.store.record_fetch_event(&FetchEvent {
            source_id: self.source.source_id.clone(),
            url: url.to_string(),
            fetched_at: Utc::now(),
            status: 0,
            elapsed_ms: None,
            error: Some(err.to_string()),
            doc_type: None,
            bytes: None,
        })?;
        Ok(())
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(&h.finalize()[..8]) // 16 hex chars, enough to avoid real-world collisions at 850×10k scale
}

fn compute_doc_id(source_id: &str, url: &str, content_hash: &str) -> String {
    let mut h = Sha256::new();
    h.update(source_id.as_bytes());
    h.update([0u8]);
    h.update(url.as_bytes());
    h.update([0u8]);
    h.update(content_hash.as_bytes());
    hex::encode(&h.finalize()[..12]) // 24 hex chars
}

fn binary_ext(kind: BinaryKind) -> &'static str {
    match kind {
        BinaryKind::Pdf => "pdf",
        BinaryKind::Docx => "docx",
        BinaryKind::Doc => "doc",
        BinaryKind::Xlsx => "xlsx",
        BinaryKind::Xls => "xls",
        BinaryKind::Pptx => "pptx",
        BinaryKind::Ppt => "ppt",
    }
}

fn map_binary_kind(kind: BinaryKind) -> DocType {
    match kind {
        BinaryKind::Pdf => DocType::Pdf,
        BinaryKind::Docx | BinaryKind::Doc => DocType::Docx,
        BinaryKind::Xlsx | BinaryKind::Xls => DocType::Xlsx,
        BinaryKind::Pptx | BinaryKind::Ppt => DocType::Pptx,
    }
}

fn enqueue_discovered_links(
    frontier: &mut Frontier,
    source: &Source,
    recipe: &Recipe,
    html: &ParsedHtml,
    parent_depth: u32,
) {
    let new_depth = parent_depth + 1;
    for link in &html.links {
        if !within_site(source, recipe, link) {
            continue;
        }
        if !passes_recipe_path_filters(recipe, link) {
            continue;
        }
        let cls = crawler_url::classify(link);
        let is_binary = matches!(cls, crawler_url::ContentClass::Document);
        let depth_cap = if is_binary {
            recipe.max_pdf_depth
        } else {
            recipe.max_depth
        };
        if new_depth > depth_cap {
            continue;
        }
        let score = crawler_url::score(link, new_depth);
        frontier.push(link.clone(), new_depth, score);
    }
}

fn within_site(source: &Source, recipe: &Recipe, link: &str) -> bool {
    let Some(link_host) = ::url::Url::parse(link)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_ascii_lowercase()))
    else {
        return false;
    };

    if let Some(allowed) = &recipe.allowed_subdomains {
        // Strict: host must equal source.domain or be <allowed>.<source.domain>.
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

    // Default: public-suffix same-site.
    crawler_url::same_site(&source.homepage_url, link)
}

fn passes_recipe_path_filters(recipe: &Recipe, link: &str) -> bool {
    let Some(path) = ::url::Url::parse(link).ok().map(|u| u.path().to_string()) else {
        return false;
    };
    let segs: Vec<String> = path
        .split('/')
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect();

    for d in &recipe.deny_paths {
        let d_lc = d.to_ascii_lowercase();
        if segs.iter().any(|s| s == &d_lc) {
            return false;
        }
    }

    if let Some(allow) = &recipe.allow_paths {
        if !allow.is_empty() {
            let allow_lc: Vec<String> = allow.iter().map(|s| s.to_ascii_lowercase()).collect();
            let has_match = segs.iter().any(|s| allow_lc.contains(s));
            if !has_match {
                return false;
            }
        }
    }

    true
}
