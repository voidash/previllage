//! Sync corpora/sources_tiered.jsonl into the SQLite `sources` table.
//!
//! The JSONL file is the human-edited source of truth (registry seed + tier
//! overrides via scripts/apply_tier_overrides.py). The daemon re-runs this
//! sync on startup so manual edits propagate without touching the daemon.

use super::store::{Store, StoreError};
use super::types::RegistryRow;
use chrono::Utc;
use std::io::{BufRead, BufReader};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Default, Clone)]
pub struct RegistrySyncReport {
    pub total_rows: u64,
    pub inserted: u64,
    pub updated: u64,
    pub skipped_bad_json: u64,
    pub skipped_missing_fields: u64,
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Read a JSONL file of [`RegistryRow`] and upsert each into the store.
///
/// Lifecycle columns (last_polled_at, consecutive_failures, status) are
/// preserved on existing rows — the sync only overwrites registry-owned
/// fields like tier, poll_interval_hours, and names.
pub fn sync_registry<P: AsRef<Path>>(
    store: &Store,
    path: P,
) -> Result<RegistrySyncReport, RegistryError> {
    let file = std::fs::File::open(&path)?;
    let reader = BufReader::new(file);
    let now = Utc::now();
    let mut report = RegistrySyncReport::default();

    for (lineno, line) in reader.lines().enumerate() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        report.total_rows += 1;

        let row: RegistryRow = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                eprintln!(
                    "registry: skipping line {} (bad json: {})",
                    lineno + 1,
                    e
                );
                report.skipped_bad_json += 1;
                continue;
            }
        };

        if row.source_id.is_empty() || row.homepage_url.is_empty() {
            eprintln!(
                "registry: skipping line {} (missing source_id/homepage_url)",
                lineno + 1
            );
            report.skipped_missing_fields += 1;
            continue;
        }

        let inserted = store.upsert_source_from_registry(&row, now)?;
        if inserted {
            report.inserted += 1;
        } else {
            report.updated += 1;
        }
    }

    Ok(report)
}
