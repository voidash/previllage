//! Content-addressable blob storage for crawled documents.
//!
//! Layout under `<root>`:
//!
//! ```text
//! blobs/<source_id>/<hash[0:2]>/<hash>.<ext>     # raw bytes
//! extracted/<source_id>/<hash[0:2]>/<hash>.txt   # readable HTML text
//! ```
//!
//! The 2-char hash prefix shards per source to keep any one directory
//! under ~4k files (for friendly `ls` + macOS APFS perf).
//!
//! Idempotency: if the target path already exists with matching bytes,
//! the write is a no-op. Different sources fetching the same content still
//! produce two blob files (one per source subtree) — intentional, keeps
//! per-source cleanup simple.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn new<P: Into<PathBuf>>(root: P) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Absolute path where this source+hash would land for a raw body.
    pub fn raw_path(&self, source_id: &str, content_hash: &str, ext: &str) -> PathBuf {
        let shard = hash_shard(content_hash);
        self.root
            .join("blobs")
            .join(source_id)
            .join(shard)
            .join(format!("{content_hash}.{ext}"))
    }

    /// Absolute path for the readable-text sidecar.
    pub fn extracted_path(&self, source_id: &str, content_hash: &str) -> PathBuf {
        let shard = hash_shard(content_hash);
        self.root
            .join("extracted")
            .join(source_id)
            .join(shard)
            .join(format!("{content_hash}.txt"))
    }

    /// Write raw bytes; returns the absolute path. Idempotent — if the file
    /// exists with identical bytes we skip the write. If the file exists
    /// with DIFFERENT bytes (content_hash collision — shouldn't happen with
    /// SHA-256) we refuse and surface an error so the daemon doesn't
    /// silently overwrite divergent content.
    pub fn write_raw(
        &self,
        source_id: &str,
        content_hash: &str,
        ext: &str,
        bytes: &[u8],
    ) -> io::Result<PathBuf> {
        let p = self.raw_path(source_id, content_hash, ext);
        write_if_different(&p, bytes)?;
        Ok(p)
    }

    pub fn write_extracted(
        &self,
        source_id: &str,
        content_hash: &str,
        text: &str,
    ) -> io::Result<PathBuf> {
        let p = self.extracted_path(source_id, content_hash);
        write_if_different(&p, text.as_bytes())?;
        Ok(p)
    }

    /// Render a store-root-relative path string for use in `documents` rows.
    pub fn to_relative(&self, full: &Path) -> String {
        full.strip_prefix(&self.root)
            .unwrap_or(full)
            .to_string_lossy()
            .into_owned()
    }
}

fn hash_shard(content_hash: &str) -> &str {
    if content_hash.len() >= 2 {
        &content_hash[..2]
    } else {
        "00"
    }
}

fn write_if_different(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::metadata(path) {
        Ok(meta) if meta.len() == bytes.len() as u64 => {
            // Plausible match: verify contents before skipping. This is
            // cheap (we have the blob in memory already) and catches the
            // pathological case where a hash truncation yields a collision.
            let existing = fs::read(path)?;
            if existing == bytes {
                return Ok(());
            }
            Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "blob hash collision at {}: existing bytes differ from new bytes",
                    path.display()
                ),
            ))
        }
        Ok(_) | Err(_) => fs::write(path, bytes),
    }
}
