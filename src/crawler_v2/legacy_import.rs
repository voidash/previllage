//! One-shot import of Python-prototype crawl output into the SQLite store.
//!
//! The Python crawler at `scripts/crawl_sources.py` wrote per-source JSONL
//! manifests under `/Volumes/T9/gemma-god/corpus_v2/manifests/<sid>.jsonl`
//! with a slightly different schema than our `Document` struct. This module
//! reads those manifests, constructs `Document` rows, and upserts them into
//! the live SQLite database so the Rust daemon's re-polling cycles can
//! supersede them naturally (via content_hash diff).
//!
//! Blobs are NOT moved. The Python layout is `raw/<sid>/<hash>.<ext>` (flat),
//! while the Rust `BlobStore` uses `blobs/<sid>/<hh>/<hash>.<ext>` (sharded).
//! We store the legacy path as-is in `Document.raw_blob_path`; downstream
//! consumers (chunker, embedder) read from whatever path the row holds.
//!
//! Errors recorded by the Python crawler (status >= 400 or transport error)
//! are skipped — we're importing documents, not fetch events.

use super::store::{DocumentOutcome, Store, StoreError};
use super::types::{DocType, Document};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ImportError {
    #[error("io {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("source_id {sid} not in registry (use --lenient to skip)")]
    UnknownSource { sid: String },
}

#[derive(Debug, Default)]
pub struct ImportReport {
    pub files_read: u64,
    pub rows_total: u64,
    pub rows_inserted: u64,
    pub rows_superseded: u64,
    pub rows_unchanged: u64,
    pub rows_skipped_error: u64,
    pub rows_skipped_no_hash: u64,
    pub rows_skipped_malformed: u64,
    pub unknown_sources: Vec<String>,
}

/// Permissive schema matching what `scripts/crawl_sources.py` writes. Every
/// field is optional because Python's error rows drop most fields and the
/// schema wasn't strict.
#[derive(Deserialize)]
struct LegacyRow {
    url: Option<String>,
    #[serde(default)]
    depth: u32,
    fetched_at: Option<String>,
    #[serde(default)]
    status: u16,
    // Kept for forward-compat even though we don't use it — doc_type is already
    // hinted separately by the Python crawler and we have URL-extension fallback.
    #[serde(default)]
    #[allow(dead_code)]
    content_type: Option<String>,
    doc_type: Option<String>,
    content_hash: Option<String>,
    #[serde(default)]
    size_bytes: u64,
    raw_path: Option<String>,
    extracted_path: Option<String>,
    #[serde(default)]
    text_chars: u32,
    #[serde(default)]
    error: Option<String>,
}

pub struct ImportOptions {
    pub manifests_dir: PathBuf,
    pub lenient: bool,
}

pub fn import_legacy(store: &mut Store, opts: &ImportOptions) -> Result<ImportReport, ImportError> {
    let mut report = ImportReport::default();

    let entries = fs::read_dir(&opts.manifests_dir).map_err(|e| ImportError::Io {
        path: opts.manifests_dir.display().to_string(),
        source: e,
    })?;

    // Preload known source_ids so we can skip rows for sources that never
    // got into the registry.
    let known: std::collections::HashSet<String> = store
        .list_sources()?
        .into_iter()
        .map(|s| s.source_id)
        .collect();

    for entry in entries {
        let entry = entry.map_err(|e| ImportError::Io {
            path: opts.manifests_dir.display().to_string(),
            source: e,
        })?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let source_id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) => s.to_string(),
            None => continue,
        };

        if !known.contains(&source_id) {
            if opts.lenient {
                report.unknown_sources.push(source_id);
                continue;
            } else {
                return Err(ImportError::UnknownSource { sid: source_id });
            }
        }

        report.files_read += 1;
        import_one_manifest(store, &source_id, &path, &mut report)?;
    }

    Ok(report)
}

fn import_one_manifest(
    store: &mut Store,
    source_id: &str,
    path: &Path,
    report: &mut ImportReport,
) -> Result<(), ImportError> {
    let f = fs::File::open(path).map_err(|e| ImportError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let reader = BufReader::new(f);

    for line in reader.lines() {
        let line = line.map_err(|e| ImportError::Io {
            path: path.display().to_string(),
            source: e,
        })?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        report.rows_total += 1;

        let row: LegacyRow = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(_) => {
                report.rows_skipped_malformed += 1;
                continue;
            }
        };

        // Skip error rows — nothing to persist as a Document.
        if row.error.is_some() || row.status >= 400 || row.status == 0 {
            report.rows_skipped_error += 1;
            continue;
        }
        // Skip rows missing the bits that make them a document.
        let (Some(url), Some(hash)) = (row.url.clone(), row.content_hash.clone()) else {
            report.rows_skipped_no_hash += 1;
            continue;
        };
        if row.raw_path.is_none() {
            report.rows_skipped_no_hash += 1;
            continue;
        }

        let fetched_at = row
            .fetched_at
            .as_deref()
            .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(Utc::now);

        let doc_type = classify_doc_type(row.doc_type.as_deref(), &url);
        let doc_id = compute_doc_id(source_id, &url, &hash);

        let doc = Document {
            doc_id,
            source_id: source_id.to_string(),
            url,
            content_hash: hash,
            fetched_at,
            superseded_by: None,
            removed_at: None,
            doc_type,
            status_code: row.status as i32,
            title: None,
            language: None,
            date_published: None,
            raw_blob_path: row.raw_path.unwrap(),
            extracted_text_path: row.extracted_path,
            text_chars: row.text_chars,
            size_bytes: row.size_bytes,
            depth: row.depth,
            priority_at_fetch: None,
        };

        match store.upsert_document(&doc)? {
            DocumentOutcome::Inserted => report.rows_inserted += 1,
            DocumentOutcome::Superseded { .. } => report.rows_superseded += 1,
            DocumentOutcome::Unchanged => report.rows_unchanged += 1,
        }
    }

    Ok(())
}

fn classify_doc_type(hint: Option<&str>, url: &str) -> DocType {
    if let Some(h) = hint {
        match h.to_ascii_lowercase().as_str() {
            "html" => return DocType::Html,
            "pdf" => return DocType::Pdf,
            "docx" => return DocType::Docx,
            "xlsx" => return DocType::Xlsx,
            "pptx" => return DocType::Pptx,
            "txt" => return DocType::Txt,
            _ => {}
        }
    }
    // Fallback: extension sniff.
    let lc = url.to_ascii_lowercase();
    if lc.ends_with(".pdf") {
        DocType::Pdf
    } else if lc.ends_with(".docx") {
        DocType::Docx
    } else if lc.ends_with(".xlsx") {
        DocType::Xlsx
    } else if lc.ends_with(".pptx") {
        DocType::Pptx
    } else if lc.ends_with(".txt") {
        DocType::Txt
    } else if lc.ends_with(".html") || lc.ends_with(".htm") {
        DocType::Html
    } else {
        DocType::Other
    }
}

fn compute_doc_id(source_id: &str, url: &str, content_hash: &str) -> String {
    let mut h = Sha256::new();
    h.update(source_id.as_bytes());
    h.update([0u8]);
    h.update(url.as_bytes());
    h.update([0u8]);
    h.update(content_hash.as_bytes());
    hex::encode(&h.finalize()[..12])
}
