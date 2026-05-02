//! Tests for Preeti-mojibake detection inside the PDF text path.
//!
//! We don't fabricate PDFs here; instead we probe the hand-rolled helpers
//! via the public `classify_language` + `substantive_chars` functions on
//! known-good and known-bad samples, and we verify the `legacy_fonts`
//! integration round-trips a real Preeti string to correct Devanagari.

use gemma_god::legacy_fonts::{best_effort_convert, nepali_word_hits};

#[test]
fn best_effort_recovers_constitution_article_39() {
    // A real slice from the 2015 Constitution as extracted-via-pdf-extract
    // from a Preeti-embedded PDF. If this loses its "real Nepali" status,
    // the Preeti pipeline is silently broken.
    let mojibake = "s[lts jf wfld{s k|rngsf gfddf s'g} klg dfWod jf k|sf/n] \
                    b'Jo{jxf/, pk]Iff jf zf/Ll/s, dfgl;s, of}ghGo jf cGo s'g} \
                    k|sf/sf] zf]if0f ug{ jf cg'lrt k|of]u ug{ kfOg] 5}g . \
                    k|To]s afnaflnsfnfO{ afn cg's\"n Gofosf] xs x'g]5 .";
    let r = best_effort_convert(mojibake);
    // The acceptance logic in extract_pdf accepts either:
    //   (a) >=3 high-frequency-word hits, OR
    //   (b) >=1 hit AND devanagari_ratio >= 0.40.
    // The constitutional sample tends to hit (b) — domain vocab (hk, धारा,
    // स्वतन्त्रता, etc.) is in our expanded dictionary but at low density.
    let accept = r.nepali_word_hits >= 3
        || (r.nepali_word_hits >= 1 && r.devanagari_ratio >= 0.40);
    assert!(
        accept,
        "Preeti conversion rejected: hits={} deva_ratio={:.2} font={} preview={:?}",
        r.nepali_word_hits,
        r.devanagari_ratio,
        r.font,
        r.text.chars().take(60).collect::<String>(),
    );
    assert!(
        r.devanagari_ratio > 0.3,
        "low devanagari ratio {} after conversion",
        r.devanagari_ratio
    );
}

#[test]
fn pure_english_is_not_recoverable_as_nepali() {
    // A real English sample — should score zero or near-zero nepali-word
    // hits. If this fires, our acceptance threshold of >=3 would trigger
    // on innocent English and corrupt it to Devanagari gibberish.
    let english = "The Office of the Attorney General is a constitutional body \
                   of Nepal established under Article 157 of the Constitution. \
                   Its primary function is to represent the government in legal \
                   matters before all courts.";
    let r = best_effort_convert(english);
    assert!(
        r.nepali_word_hits < 3,
        "English text produced {} fake nepali_word_hits (font={}, converted={:?})",
        r.nepali_word_hits,
        r.font,
        r.text.chars().take(80).collect::<String>(),
    );
}

#[test]
fn nepali_word_hits_counts_unicode_devanagari() {
    // Direct check of the scoring function — belt and braces around the
    // acceptance threshold.
    let text = "यो नेपाल सरकारको कार्यालय हो। विभाग र मन्त्रालय मिलेर काम गर्छन्।";
    let hits = nepali_word_hits(text);
    assert!(hits >= 3, "expected >=3, got {hits}");
}
