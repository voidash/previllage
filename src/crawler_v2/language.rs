//! Lightweight language/quality classifier for chunk-level filtering and
//! per-source audit breakdowns.
//!
//! This is NOT a proper language-ID model — it's a character-class counter
//! that distinguishes the buckets we care about for retrieval quality:
//!
//! - `Devanagari` — mostly Nepali, good for Nepali queries
//! - `Latin` — English or Romanized Nepali
//! - `Mixed` — legit mixed, common on gov pages with bilingual labels
//! - `MojibakeSuspected` — Latin-heavy text with signature chars of legacy
//!   font encoding (Preeti/Kantipur). Should not have reached chunking;
//!   surfacing these at chunk time lets us measure the upstream fix.
//! - `Other` — punctuation/whitespace/symbol-dominant chunks. Noise.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Devanagari,
    Latin,
    Mixed,
    MojibakeSuspected,
    Other,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Language::Devanagari => "devanagari",
            Language::Latin => "latin",
            Language::Mixed => "mixed",
            Language::MojibakeSuspected => "mojibake_suspected",
            Language::Other => "other",
        }
    }
}

/// Substantive character count: Devanagari + Latin alphabetic. Excludes
/// whitespace, punctuation, digits, and symbols. Chunks with fewer than
/// ~100 substantive chars carry almost no retrieval signal.
pub fn substantive_chars(text: &str) -> usize {
    text.chars()
        .filter(|c| is_devanagari(*c) || c.is_ascii_alphabetic())
        .count()
}

pub fn classify(text: &str) -> Language {
    let chars: Vec<char> = text.chars().collect();
    let total = chars.len();
    if total == 0 {
        return Language::Other;
    }

    let mut deva = 0usize;
    let mut latin = 0usize;
    for c in &chars {
        if is_devanagari(*c) {
            deva += 1;
        } else if c.is_ascii_alphabetic() {
            latin += 1;
        }
    }

    let substantive = deva + latin;
    if substantive < 50 {
        return Language::Other;
    }

    let deva_r = deva as f64 / total as f64;
    let latin_r = latin as f64 / total as f64;

    // Mixed: BOTH scripts carry meaningful weight. But check for mojibake
    // contamination first — bilingual gov docs (Nepali body + English
    // technical terms) commonly hit this branch and we still need the
    // mojibake guard. The chunk could also be majority-Devanagari with
    // English appendices that lift latin_r above 0.10.
    if deva_r > 0.10 && latin_r > 0.10 {
        if devanagari_with_hybrid_mojibake_ratio(text) >= 0.10 {
            return Language::MojibakeSuspected;
        }
        return Language::Mixed;
    }
    // Devanagari-dominant — but check for hybrid mojibake first.
    // Some Nepal-gov PDFs mix Preeti-encoded glyphs (rendered as ASCII
    // punctuation/digits/Latin alpha) inside otherwise-Devanagari words,
    // e.g. `)ा*धकरण` (should be प्राधिकरण), `सHपादन` (सम्पादन),
    // `संग्रह` rendered as `सं3ह`. These slip past as `Devanagari`
    // because deva_r > 0.15, but they're contaminated and unrecoverable
    // (the Devanagari chars come from a different font than the Preeti-
    // encoded ones, so legacy_fonts can't fix them post-extraction).
    // Drop chunks where the contamination is severe.
    if deva_r > 0.15 {
        // Threshold 0.10: a chunk where ≥10% of Devanagari-bearing tokens are
        // hybrid-mojibake is unusable as a citation source — even if the
        // surrounding context reads, the model would learn to cite garbled
        // claim text. 0.10 maps to ~1500 corpus chunks at 2026-05-02
        // (severe + heavy + moderate-leaning), gets v3-fix.md's <1% target
        // post-rebuild.
        if devanagari_with_hybrid_mojibake_ratio(text) >= 0.10 {
            return Language::MojibakeSuspected;
        }
        return Language::Devanagari;
    }
    // Latin-dominant: distinguish real English from Preeti mojibake using
    // WORD-LEVEL structure, not global punctuation density. Preeti mojibake
    // is characterized by ASCII "words" that contain mid-word brace-family
    // characters (`{`, `}`, `|`, `[`, `]`) — rare in English where those
    // symbols appear only at phrase boundaries. Trademark filings and
    // similar punctuation-heavy English produce zero such words.
    if latin_r > 0.15 {
        if preeti_mojibake_word_ratio(text) > 0.05 {
            return Language::MojibakeSuspected;
        }
        return Language::Latin;
    }
    Language::Other
}

/// Fraction of whitespace-tokens that are HYBRID mojibake (interior
/// non-Devanagari content surrounded by Devanagari context after stripping
/// edge punctuation). Catches:
///   - embedded Latin alpha in Devanagari words: `सHपादन`, `mबचप`
///   - embedded Preeti glyph-encoded ASCII: `)ा*धकरण`, `*नण/य`, `सं3ह`
///   - doubled Devanagari matras (`ुु`, `ाा`, `ीी`): Preeti double-decode
///     signature, e.g. `अनुुवादक` (should be अनुवादक), `रुरु..ललााखखममाा`.
/// Excludes legitimate bilingual annotations like `(कानून)` or `क्यास(Cache)`
/// where the non-Devanagari content sits at the edge / inside parens.
fn devanagari_with_hybrid_mojibake_ratio(text: &str) -> f64 {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 20 {
        return 0.0;
    }
    let mut hits = 0usize;
    let mut deva_bearing = 0usize;
    for w in &words {
        let has_dev = w.chars().any(is_devanagari);
        if !has_dev {
            continue;
        }
        deva_bearing += 1;
        // Doubled-matra signature — impossible in well-formed Devanagari.
        let mut prev = '\0';
        let mut doubled = false;
        for c in w.chars() {
            if c == prev && matches!(c,
                'ा' | 'ि' | 'ी' | 'ु' | 'ू' | 'े' | 'ै' | 'ो' | 'ौ' | 'ं' | 'ः' | 'ृ'
            ) {
                doubled = true;
                break;
            }
            prev = c;
        }
        if doubled {
            hits += 1;
            continue;
        }
        // Embedded non-Devanagari interior (after stripping edge punctuation).
        let stripped = w.trim_matches(|c: char| !c.is_alphanumeric() && !is_devanagari(c));
        let interior_has_non_dev = stripped.chars().any(|c| {
            !is_devanagari(c) && !c.is_whitespace()
        });
        if interior_has_non_dev {
            hits += 1;
        }
    }
    if deva_bearing == 0 {
        return 0.0;
    }
    hits as f64 / deva_bearing as f64
}

/// Fraction of whitespace-delimited "words" that contain an internal
/// brace-family character (mid-word `{`, `}`, `|`, `[`, `]`). Preeti
/// encoding uses these to encode Devanagari half-forms and matras, so
/// real Preeti mojibake has 5–30% of words hitting this pattern. Pure
/// English text has 0%.
fn preeti_mojibake_word_ratio(text: &str) -> f64 {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < 20 {
        return 0.0;
    }
    let mut hits = 0usize;
    for w in &words {
        let chars: Vec<char> = w.chars().collect();
        let n = chars.len();
        if n < 3 {
            continue;
        }
        // Scan the INTERIOR of the word (not leading/trailing punct).
        for i in 1..(n - 1) {
            match chars[i] {
                '{' | '}' | '|' | '[' | ']' => {
                    hits += 1;
                    break;
                }
                _ => {}
            }
        }
    }
    hits as f64 / words.len() as f64
}

fn is_devanagari(c: char) -> bool {
    let cp = c as u32;
    (0x0900..=0x097F).contains(&cp)
}
