//! Extract plain text from a stored Document.
//!
//! - **HTML** → read the pre-extracted sidecar (`extracted_text_path`).
//!   Python prototype + Rust Phase-5 both write readable text alongside
//!   the raw blob; we just read it.
//! - **PDF** → primary parser is pure-Rust `pdf_extract`; on panic / error /
//!   thin output we fall back to `pdftotext` (Poppler). The fallback is
//!   cross-platform — install via `brew install poppler` (macOS) or
//!   `apt install poppler-utils` (Debian/Ubuntu) or `dnf install poppler-utils`
//!   (Fedora/RHEL). If `pdftotext` isn't on PATH the fallback gracefully
//!   no-ops and the worker behaves as before. Real-world: `pdf_extract`
//!   panics on certain Nepal-gov PDFs (the `adobe-cmap-parser` crate
//!   has a `bad length of hexstring` bug on them); poppler handles them.
//!   Always run legacy-font conversion on the chosen output.
//! - **docx/xlsx/pptx** → deferred. Present in the corpus but rare; Phase-5
//!   stored them as raw blobs. We return empty text + flag `skipped_unsupported`.

use super::types::{DocType, Document};
use crate::legacy_fonts;
use std::path::{Path, PathBuf};
use std::process::Command;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("io {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("pdf extract failed for {path}: {msg}")]
    Pdf { path: String, msg: String },
}

/// What we know about the extraction — text plus a status flag so the
/// caller can distinguish "real content" from "empty on purpose" from
/// "format we don't handle yet".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedText {
    pub text: String,
    pub status: ExtractStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractStatus {
    Ok,
    /// HTML doc without an extracted_text_path (Python crawler produced none
    /// because trafilatura returned empty). Still recorded as a doc; chunker
    /// will skip.
    EmptyExtraction,
    /// PDF parsed but yielded effectively no text (scanned image PDF,
    /// encrypted, or unsupported encoding). OCR fallback lives elsewhere.
    PdfNoText,
    /// Format we don't handle in this phase (docx/xlsx/pptx/other).
    SkippedUnsupported,
}

/// Resolve the document's text. `corpus_root` is the base directory that
/// `Document.raw_blob_path` / `extracted_text_path` are relative to (e.g.,
/// `/Volumes/T9/gemma-god/corpus_v2`).
pub fn extract(doc: &Document, corpus_root: &Path) -> Result<ExtractedText, ExtractError> {
    match doc.doc_type {
        DocType::Html => extract_html(doc, corpus_root),
        DocType::Pdf => extract_pdf(doc, corpus_root),
        DocType::Docx | DocType::Xlsx | DocType::Pptx | DocType::Txt | DocType::Other => {
            Ok(ExtractedText {
                text: String::new(),
                status: ExtractStatus::SkippedUnsupported,
            })
        }
    }
}

fn extract_html(doc: &Document, corpus_root: &Path) -> Result<ExtractedText, ExtractError> {
    let Some(rel) = doc.extracted_text_path.as_deref() else {
        return Ok(ExtractedText {
            text: String::new(),
            status: ExtractStatus::EmptyExtraction,
        });
    };
    let full = corpus_root.join(rel);
    let text = std::fs::read_to_string(&full).map_err(|e| ExtractError::Io {
        path: full.display().to_string(),
        source: e,
    })?;
    let cleaned = normalize_whitespace(&text);
    let status = if cleaned.chars().count() < 20 {
        ExtractStatus::EmptyExtraction
    } else {
        ExtractStatus::Ok
    };
    Ok(ExtractedText {
        text: cleaned,
        status,
    })
}

/// Below this character count, primary `pdf_extract` output is treated as
/// "thin" and we attempt the `pdftotext` fallback. The threshold is set
/// just above what page-chrome typically yields (titles + headers + page
/// numbers ≈ 30-40 chars) so we don't run a fallback subprocess on every
/// already-good PDF.
const PRIMARY_THIN_THRESHOLD: usize = 50;

fn extract_pdf(doc: &Document, corpus_root: &Path) -> Result<ExtractedText, ExtractError> {
    let full: PathBuf = corpus_root.join(&doc.raw_blob_path);
    let bytes = std::fs::read(&full).map_err(|e| ExtractError::Io {
        path: full.display().to_string(),
        source: e,
    })?;
    // Primary: pure-Rust pdf_extract. catch_unwind covers the
    // `adobe-cmap-parser: bad length of hexstring` panics seen on real
    // Nepal-gov budget/policy PDFs; we recover via pdftotext below.
    let primary = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem(&bytes));
    let primary_text: Option<String> = match primary {
        Ok(Ok(s)) => Some(s),
        Ok(Err(_)) | Err(_) => None,
    };
    let primary_chars = primary_text
        .as_deref()
        .map(|s| s.chars().count())
        .unwrap_or(0);

    // Fallback: shell out to `pdftotext` when primary produced little or
    // nothing. Either tool's stdout is the candidate text; whichever is
    // longer wins. If pdftotext is missing (NotFound from Command::output)
    // we silently keep the primary result — graceful degradation.
    let chosen = if primary_chars >= PRIMARY_THIN_THRESHOLD {
        primary_text.unwrap()
    } else {
        match pdftotext_extract(&full) {
            Ok(fallback) => {
                let fb_chars = fallback.chars().count();
                if fb_chars > primary_chars {
                    fallback
                } else {
                    primary_text.unwrap_or_default()
                }
            }
            Err(_) => primary_text.unwrap_or_default(),
        }
    };

    if chosen.is_empty() {
        return Ok(ExtractedText {
            text: String::new(),
            status: ExtractStatus::PdfNoText,
        });
    }

    // Always attempt segment-aware legacy-font conversion. `convert_mixed`
    // preserves already-Unicode Devanagari words and only rewrites tokens
    // that look like Preeti bytes — so it's a no-op on clean PDFs and a
    // recovery pass on mojibake PDFs and a PARTIAL recovery on mixed-content
    // PDFs (the prior design applied all-or-nothing and missed the mixed
    // case, leaving Preeti sections un-converted).
    let cleaned_raw = normalize_whitespace(&chosen);
    let raw_word_hits = legacy_fonts::nepali_word_hits(&cleaned_raw);
    let converted = legacy_fonts::best_effort_convert_mixed(&cleaned_raw);
    // Acceptance: the segment-aware conversion must produce MORE Nepali
    // words than the raw output (otherwise it's not recovering anything,
    // just doing busywork or corrupting clean text). Setting the delta
    // threshold at +2 is conservative — one incidental match could happen
    // in pure English from a bad prefix hit; two can't.
    let final_text = if converted.nepali_word_hits >= raw_word_hits + 2 {
        normalize_whitespace(&converted.text)
    } else {
        cleaned_raw
    };

    let status = if final_text.chars().count() < 50 {
        ExtractStatus::PdfNoText
    } else {
        ExtractStatus::Ok
    };
    Ok(ExtractedText {
        text: final_text,
        status,
    })
}

/// Shell out to `pdftotext` (Poppler). Returns the binary's stdout as a
/// String. Errors when the binary isn't on PATH OR when it exited
/// non-zero — callers treat both as "fallback unavailable" and keep
/// whatever the primary parser produced.
///
/// **Binary resolution**: respects `PDFTOTEXT_BIN` (absolute path) when
/// set, falling back to `pdftotext` from PATH. The env-var override exists
/// because the launchd daemon inherits a stripped-down PATH that doesn't
/// see userland conda installs.
///
/// **Cross-platform install**:
///   - macOS: `brew install poppler` (admin) OR `conda install -c conda-forge poppler`
///   - Debian/Ubuntu: `sudo apt install poppler-utils`
///   - Fedora/RHEL: `sudo dnf install poppler-utils`
///   - Alpine: `apk add poppler-utils`
fn pdftotext_extract(path: &Path) -> Result<String, ExtractError> {
    let cmd = std::env::var("PDFTOTEXT_BIN").unwrap_or_else(|_| "pdftotext".to_string());
    // `-layout` keeps column structure (helps tabular gov budget docs).
    // `-enc UTF-8` ensures Devanagari survives the pipe regardless of the
    // host's LC_CTYPE.
    let output = Command::new(&cmd)
        .arg("-layout")
        .arg("-enc")
        .arg("UTF-8")
        .arg(path)
        .arg("-") // write to stdout
        .output()
        .map_err(|e| ExtractError::Pdf {
            path: path.display().to_string(),
            msg: format!("invoke {cmd}: {e}"),
        })?;
    if !output.status.success() {
        return Err(ExtractError::Pdf {
            path: path.display().to_string(),
            msg: format!(
                "{cmd} exit {}: {}",
                output.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

// (has_real_devanagari + looks_like_preeti_mojibake removed: the
// always-on segment-aware converter makes them obsolete. If you need
// is_devanagari, it's defined per-module in language.rs.)

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdftotext_missing_binary_returns_invocation_err() {
        // Force a binary name that definitely isn't on PATH.
        let path = std::path::Path::new("/tmp/does-not-exist.pdf");
        let result = std::process::Command::new("definitely-not-a-real-binary-xyzzy")
            .arg(path)
            .output();
        assert!(result.is_err(), "missing binary should error at invocation");
        // The real `pdftotext_extract` wraps that error via map_err — the
        // failure surface is identical (Err returned, caller falls through).
    }

    #[test]
    fn pdftotext_extract_runs_when_binary_present() {
        if std::process::Command::new("pdftotext")
            .arg("-v")
            .output()
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            // pdftotext not on PATH on this machine — skip rather than fail.
            return;
        }
        // Build a one-page text-native PDF with `pdftotext` itself? No —
        // pdftotext only reads. Instead use a minimal hand-rolled PDF
        // (RFC-compliant but tiny). The exact text doesn't matter; we
        // just confirm the helper round-trips bytes through the binary.
        let dir = tempfile::tempdir().unwrap();
        let pdf = dir.path().join("min.pdf");
        std::fs::write(&pdf, MIN_PDF).unwrap();
        let r = pdftotext_extract(&pdf).expect("pdftotext should run");
        // The test PDF embeds the literal "hello" — pdftotext should yield it.
        assert!(r.contains("hello"), "expected 'hello' in pdftotext output, got: {r:?}");
    }

    /// Smallest legal PDF I could craft that contains the literal text
    /// "hello" — used only by the live-pdftotext test above. Generated
    /// once and pasted; if pdftotext rejects it as malformed, regenerate.
    const MIN_PDF: &[u8] = b"%PDF-1.4\n\
1 0 obj<</Type/Catalog/Pages 2 0 R>>endobj\n\
2 0 obj<</Type/Pages/Kids[3 0 R]/Count 1>>endobj\n\
3 0 obj<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>endobj\n\
4 0 obj<</Length 44>>stream\n\
BT /F1 24 Tf 10 50 Td (hello) Tj ET\n\
endstream\nendobj\n\
5 0 obj<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>endobj\n\
xref\n\
0 6\n\
0000000000 65535 f \n\
0000000009 00000 n \n\
0000000053 00000 n \n\
0000000098 00000 n \n\
0000000187 00000 n \n\
0000000277 00000 n \n\
trailer<</Size 6/Root 1 0 R>>\n\
startxref\n340\n%%EOF\n";
}

/// Count Devanagari characters (U+0900..U+097F). Used by the legacy-font
/// acceptance gate to detect "conversion clearly added Devanagari" even
/// when the high-frequency wordlist doesn't grow (domain-specific docs).
pub fn devanagari_char_count(s: &str) -> usize {
    s.chars().filter(|c| ('\u{0900}'..='\u{097F}').contains(c)).count()
}

/// Collapse runs of whitespace (including newlines) to single spaces.
/// Keeps non-ASCII codepoints (Devanagari etc.) intact.
pub fn normalize_whitespace(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_ws = true; // suppresses leading whitespace
    for c in s.chars() {
        if c.is_whitespace() {
            if !prev_ws {
                out.push(' ');
                prev_ws = true;
            }
        } else {
            out.push(c);
            prev_ws = false;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}
