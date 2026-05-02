//! Split a document's text into overlapping chunks for retrieval.
//!
//! ## Approach
//!
//! v1 uses **character-based chunking with soft sentence-boundary snapping**.
//! A real tokenizer-driven approach would be slightly more accurate at the
//! edges (since BGE-M3 has a 512-token input cap and we don't want to
//! truncate mid-word) but char-based is portable and deterministic.
//!
//! Token count is approximated at embed-time via the tokenizer; if a chunk
//! exceeds the model's cap it gets gracefully truncated there.
//!
//! ## Parameters
//!
//! - `target_chars`: ~1800 (aim ≈500 tokens for mixed Devanagari/Latin).
//! - `overlap_chars`: ~180 (10% overlap preserves cross-boundary context).
//! - `min_chars`: 200 (anything shorter than this isn't worth a chunk row;
//!   the whole doc becomes a single chunk).
//! - **Soft boundary snap**: after reaching `target_chars`, seek forward
//!   up to `snap_window` chars for a sentence terminator (`. `, `। `, `?`,
//!   `!`, `\n\n`) and cut there instead of mid-sentence.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy)]
pub struct ChunkConfig {
    pub target_chars: usize,
    pub overlap_chars: usize,
    pub min_chars: usize,
    pub snap_window: usize,
}

impl Default for ChunkConfig {
    fn default() -> Self {
        Self {
            target_chars: 1800,
            overlap_chars: 180,
            min_chars: 200,
            snap_window: 120,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    pub chunk_id: String, // stable hash of (doc_id, chunk_index, text)
    pub chunk_index: u32,
    pub text: String,
    pub char_start: u32,
    pub char_end: u32,
}

/// Split `text` into overlapping chunks per `config`. Returns one chunk if
/// input is shorter than `min_chars`.
pub fn chunk_text(doc_id: &str, text: &str, config: ChunkConfig) -> Vec<Chunk> {
    // Work in char-index space, not byte-index space, so Devanagari doesn't
    // split mid-codepoint. We collect once up front.
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();

    if total == 0 {
        return Vec::new();
    }
    if total < config.min_chars {
        return vec![Chunk {
            chunk_id: chunk_id(doc_id, 0, text),
            chunk_index: 0,
            text: text.to_string(),
            char_start: 0,
            char_end: total as u32,
        }];
    }

    let mut out = Vec::new();
    let mut cursor = 0usize;
    let mut idx = 0u32;

    while cursor < total {
        let want_end = (cursor + config.target_chars).min(total);
        // Soft-snap to sentence boundary within snap_window AFTER want_end.
        let snap_end = (want_end + config.snap_window).min(total);
        let end = find_sentence_end(&chars, want_end, snap_end).unwrap_or(want_end);

        // Final chunk absorbs the tail (no new chunk started if the
        // remaining piece is smaller than min_chars — that would just be
        // duplicated content from overlap).
        let text_slice: String = chars[cursor..end].iter().collect();
        out.push(Chunk {
            chunk_id: chunk_id(doc_id, idx, &text_slice),
            chunk_index: idx,
            text: text_slice,
            char_start: cursor as u32,
            char_end: end as u32,
        });
        idx += 1;

        if end >= total {
            break;
        }
        // Advance cursor leaving the overlap behind.
        let step = config.target_chars.saturating_sub(config.overlap_chars);
        cursor = cursor.saturating_add(step);
        if cursor >= end {
            // Safety: overlap must always make forward progress.
            cursor = end;
        }
    }

    out
}

/// Look for a sentence terminator in `chars[want..cap]`. Returns the index
/// AFTER the terminator (i.e., the slice end) if found; None otherwise.
///
/// Terminators: `.`, `?`, `!`, `।` (Devanagari danda), `\n\n` (paragraph).
fn find_sentence_end(chars: &[char], want: usize, cap: usize) -> Option<usize> {
    let mut i = want;
    while i < cap {
        let c = chars[i];
        if c == '।' || c == '?' || c == '!' {
            return Some(i + 1);
        }
        if c == '.' {
            // Prefer `. ` to avoid splitting mid-abbreviation (e.g. `U.S.`).
            if i + 1 < chars.len() && chars[i + 1].is_whitespace() {
                return Some(i + 1);
            }
        }
        if c == '\n' && i + 1 < chars.len() && chars[i + 1] == '\n' {
            return Some(i + 2);
        }
        i += 1;
    }
    None
}

/// Detect chunks dominated by a single repeating substring — the signature
/// of PDF page headers/footers that recur on every page with a changing
/// page-number token. Each chunk is textually unique (different page
/// number per copy) so cross-chunk dedup can't catch them, but the chunk
/// itself contains the header fragment multiple times.
///
/// Algorithm: slide a `window`-character window over the chunk *at every
/// position* and count identical substrings. Returns true if any substring
/// appears `>= min_repeats` times. Counting uses borrowed `&str` slices
/// (no allocation) keyed on byte boundaries derived from `char_indices`.
pub fn is_internally_repetitive(text: &str, min_repeats: usize, window: usize) -> bool {
    // Pre-compute character-start byte offsets so we can form char-aligned
    // `&str` slices cheaply.
    let mut boundaries: Vec<usize> = text.char_indices().map(|(i, _)| i).collect();
    let total_chars = boundaries.len();
    if total_chars < window.saturating_mul(min_repeats) {
        return false;
    }
    boundaries.push(text.len()); // sentinel for last-slice end

    use std::collections::HashMap;
    let mut counts: HashMap<&str, usize> = HashMap::new();
    // Step of 1 — every position. Cost is O(total_chars) HashMap operations
    // with zero allocation per slice; fast enough for 2k-char chunks
    // (~2k ops × µs each).
    for i in 0..=(total_chars - window) {
        let slice = &text[boundaries[i]..boundaries[i + window]];
        let c = counts.entry(slice).or_insert(0);
        *c += 1;
        if *c >= min_repeats {
            return true;
        }
    }
    false
}

fn chunk_id(doc_id: &str, chunk_index: u32, text: &str) -> String {
    let mut h = Sha256::new();
    h.update(doc_id.as_bytes());
    h.update([0u8]);
    h.update(chunk_index.to_le_bytes());
    h.update([0u8]);
    h.update(text.as_bytes());
    hex::encode(&h.finalize()[..12])
}
