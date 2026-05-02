//! Tests for the segment-aware `best_effort_convert_mixed` that replaced
//! the all-or-nothing conversion for PDF extraction.

use gemma_god::legacy_fonts::{best_effort_convert_mixed, nepali_word_hits};

#[test]
fn converts_pure_preeti_same_as_before() {
    // Sample from Constitution Article 39. The mixed version must recover
    // it at least as well as the all-or-nothing version did, otherwise
    // this change regresses PDF extraction on pure-Preeti documents.
    let mojibake = "s[lts jf wfld{s k|rngsf gfddf s'g} klg dfWod jf k|sf/n] \
                    b'Jo{jxf/, pk]Iff jf zf/Ll/s, dfgl;s, of}ghGo jf cGo s'g} \
                    k|sf/sf] zf]if0f ug{ jf cg'lrt k|of]u ug{ kfOg] 5}g . \
                    k|To]s afnaflnsfnfO{ afn cg's\"n Gofosf] xs x'g]5 .";
    let r = best_effort_convert_mixed(mojibake);
    let accept = r.nepali_word_hits >= 3
        || (r.nepali_word_hits >= 1 && r.devanagari_ratio >= 0.40);
    assert!(
        accept,
        "conversion rejected: hits={} deva_ratio={:.2} preview={:?}",
        r.nepali_word_hits,
        r.devanagari_ratio,
        r.text.chars().take(60).collect::<String>(),
    );
}

#[test]
fn preserves_unicode_devanagari_when_mixed_with_preeti() {
    // Realistic mixed-content PDF shape: some pages already Unicode, some
    // pages Preeti. The mixed converter must NOT rewrite the Unicode
    // portions (classify_word returns Keep on anything containing a
    // Devanagari codepoint), while still recovering the Preeti portions.
    let input = "नेपाल सरकारले आर्थिक वर्ष २०८० को बजेट प्रस्तुत गर्यो। यो \
                 महत्वपूर्ण निर्णय हो। e\"ld ;DaGwL sfg\"gL ;|f]t ;fdu|L110";
    let r = best_effort_convert_mixed(input);
    // The clean Devanagari head must survive verbatim.
    assert!(
        r.text.contains("नेपाल सरकार"),
        "clean Unicode was altered: preview={:?}",
        r.text.chars().take(60).collect::<String>()
    );
    assert!(
        r.text.contains("आर्थिक वर्ष") || r.text.contains("आर्थिक"),
        "clean Unicode corrupted: preview={:?}",
        r.text.chars().take(60).collect::<String>()
    );
}

#[test]
fn pure_english_is_not_rewritten() {
    // convert_mixed uses classify_word; classify_word returns Keep for
    // English dictionary words, acronyms, title-case proper nouns. So a
    // block of English must pass through unchanged and produce zero
    // nepali_word_hits above the raw baseline.
    let english = "The Office of the Attorney General is a constitutional body \
                   of Nepal established under Article 157 of the Constitution. \
                   Its primary function is to represent the government in \
                   legal matters before all courts.";
    let r = best_effort_convert_mixed(english);
    let raw_hits = nepali_word_hits(english);
    assert!(
        r.nepali_word_hits <= raw_hits,
        "English wrongly 'recovered' to Devanagari: hits={} (raw={}) preview={:?}",
        r.nepali_word_hits,
        raw_hits,
        r.text.chars().take(80).collect::<String>(),
    );
}

#[test]
fn trademark_filing_punctuation_does_not_pass_acceptance_gate() {
    // The English-with-heavy-punctuation case that false-positived before.
    // Two properties we need:
    //   (1) the converter output has very low Devanagari ratio (<5%), so
    //       the `deva_ratio >= 0.40` branch of acceptance can't fire.
    //   (2) `nepali_word_hits` on converter output isn't meaningfully above
    //       raw — so the `hits >= raw + 2` branch can't fire either.
    // Either property holding prevents extract_pdf from wrongly accepting.
    let t = "Filing Date : 2080.01.06 NICE Class : 25 Goods/ Services : \
             AS PER HRC 294. Applicant : BEIJING NEW ORIENTAL XUNCHENG \
             NETWORK TECHNOLOGY CO., LTD.";
    let r = best_effort_convert_mixed(t);
    let raw_hits = nepali_word_hits(t);
    let accept = r.nepali_word_hits >= raw_hits + 2;
    assert!(
        !accept,
        "English wrongly accepted: hits={} (raw={}) deva_ratio={:.2} \
         preview={:?}",
        r.nepali_word_hits,
        raw_hits,
        r.devanagari_ratio,
        r.text.chars().take(120).collect::<String>(),
    );
}
