//! Tests for crawler_v2::chunk — char-based chunking with soft boundary snap.

use gemma_god::crawler_v2::chunk::{chunk_text, ChunkConfig};

fn tiny_config() -> ChunkConfig {
    // Smaller windows so tests don't need multi-KB input.
    ChunkConfig {
        target_chars: 80,
        overlap_chars: 10,
        min_chars: 50,
        snap_window: 20,
    }
}

#[test]
fn short_text_becomes_single_chunk() {
    let text = "short text";
    let chunks = chunk_text("doc1", text, ChunkConfig::default());
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, text);
    assert_eq!(chunks[0].char_start, 0);
    assert_eq!(chunks[0].char_end, text.chars().count() as u32);
}

#[test]
fn empty_text_returns_no_chunks() {
    let chunks = chunk_text("doc1", "", ChunkConfig::default());
    assert!(chunks.is_empty());
}

#[test]
fn long_text_splits_with_overlap() {
    // Build text with a clear sentence boundary near the target, so we can
    // verify snapping. 200 ascii chars, periods every 40.
    let text = (0..5)
        .map(|i| format!("The sentence number {i} is exactly forty cha."))
        .collect::<Vec<_>>()
        .join(" ");
    let chunks = chunk_text("doc", &text, tiny_config());
    assert!(chunks.len() >= 2, "got {} chunks", chunks.len());
    // Sequential chunk_index
    for (i, c) in chunks.iter().enumerate() {
        assert_eq!(c.chunk_index as usize, i);
    }
    // Non-empty chunks
    for c in &chunks {
        assert!(!c.text.is_empty());
        assert!(c.char_end > c.char_start);
    }
    // Every char_start <= previous char_end (overlap allowed) — no gaps.
    for w in chunks.windows(2) {
        let prev = &w[0];
        let cur = &w[1];
        assert!(
            cur.char_start <= prev.char_end,
            "gap between chunks {} and {}: prev_end={}, cur_start={}",
            prev.chunk_index,
            cur.chunk_index,
            prev.char_end,
            cur.char_start,
        );
    }
}

#[test]
fn devanagari_not_split_mid_codepoint() {
    // The chunker operates in char-index space. Pack Devanagari densely.
    let devanagari = "नेपाल सरकारको गृह मन्त्रालय जानकारी पत्र हो। ";
    let text = devanagari.repeat(8);
    let config = ChunkConfig {
        target_chars: 60,
        overlap_chars: 10,
        min_chars: 30,
        snap_window: 10,
    };
    let chunks = chunk_text("doc", &text, config);
    assert!(chunks.len() >= 2);
    // Each chunk text must be valid UTF-8 (Rust's String guarantees this,
    // but we still verify the chunker doesn't slip in malformed bytes via
    // some edge case).
    for c in &chunks {
        assert!(std::str::from_utf8(c.text.as_bytes()).is_ok());
        // Contains at least one Devanagari codepoint (no empty all-space).
        assert!(c.text.chars().any(|ch| (0x0900..=0x097F).contains(&(ch as u32))));
    }
}

#[test]
fn boundary_snap_prefers_sentence_terminator() {
    // Boundary at char 80; after it, a "." + " " appears at char 85. Snap
    // must cut there instead of mid-sentence.
    let text = format!(
        "{}. {}",
        "x".repeat(85), // 85 xs then `.` then ` `
        "y".repeat(400)
    );
    let chunks = chunk_text("doc", &text, tiny_config());
    assert!(chunks.len() >= 2);
    // The first chunk should end at char 87 (after ". "), not at 80.
    let first_end = chunks[0].char_end;
    assert!(
        first_end >= 85 && first_end <= 100,
        "snap didn't move to sentence end: first_end={first_end}"
    );
}

#[test]
fn devanagari_danda_is_a_sentence_terminator() {
    // Danda (`।`) should also trigger boundary snap.
    let text = format!(
        "{}।{}",
        "a".repeat(85),
        "b".repeat(400)
    );
    let chunks = chunk_text("doc", &text, tiny_config());
    assert!(chunks.len() >= 2);
    let first = &chunks[0];
    // Last char of first chunk should be the danda (or just after it).
    assert!(
        first.text.ends_with('।') || first.text.chars().last() == Some('।'),
        "first chunk didn't end at danda: {:?}",
        first.text.chars().rev().take(5).collect::<String>()
    );
}

#[test]
fn chunk_ids_are_stable_and_unique() {
    let text = "The quick brown fox jumps over the lazy dog. ".repeat(50);
    let a = chunk_text("doc1", &text, tiny_config());
    let b = chunk_text("doc1", &text, tiny_config());
    // Same input → same ids.
    let ids_a: Vec<_> = a.iter().map(|c| c.chunk_id.clone()).collect();
    let ids_b: Vec<_> = b.iter().map(|c| c.chunk_id.clone()).collect();
    assert_eq!(ids_a, ids_b);
    // Ids unique within a doc.
    let mut seen = std::collections::HashSet::new();
    for id in &ids_a {
        assert!(seen.insert(id.clone()), "duplicate chunk_id {id}");
    }
    // Different doc_id → different ids.
    let c = chunk_text("doc2", &text, tiny_config());
    assert_ne!(ids_a[0], c[0].chunk_id);
}

#[test]
fn last_chunk_covers_tail() {
    let text = "word ".repeat(100);
    let chunks = chunk_text("doc", &text, tiny_config());
    // Last chunk must include the last character.
    let last = chunks.last().unwrap();
    assert_eq!(last.char_end as usize, text.chars().count());
}
