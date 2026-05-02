//! Tests for the chunk-level language classifier.

use gemma_god::crawler_v2::language::{classify, substantive_chars, Language};

#[test]
fn classifies_pure_devanagari() {
    let t = "नेपाल सरकारको गृह मन्त्रालयको आधिकारिक वेबसाइट। \
             यस वेबसाइटमा सरकारका सूचना र नियमहरू उपलब्ध छन्।";
    assert_eq!(classify(t), Language::Devanagari);
}

#[test]
fn classifies_pure_english() {
    let t = "The Office of the Attorney General is a constitutional body of Nepal \
             established under Article 157 of the Constitution. Its primary function \
             is to represent the government in legal matters before all courts.";
    assert_eq!(classify(t), Language::Latin);
}

#[test]
fn classifies_mixed_english_and_devanagari() {
    let t = "Contact: फोन नम्बर 01-4200800 or email ags@ag.gov.np for more information. \
             सम्पर्क गर्नुहोस् विस्तृत जानकारीका लागि।";
    assert_eq!(classify(t), Language::Mixed);
}

#[test]
fn classifies_preeti_mojibake_as_suspected() {
    // Actual Preeti mojibake sample from our corpus audit — a chunk of the
    // 2015 Constitution that pdf-extract read as Latin. The distinguishing
    // signal is words containing mid-word `{`, `}`, `|`, `[`, `]`
    // characters — empirically zero in English, ~10-20% of words in Preeti.
    let t = "s[lts jf wfld{s k|rngsf gfddf s'g} klg dfWod jf k|sf/n] b'Jo{jxf/, \
             pk]Iff jf zf/Ll/s, dfgl;s, of}ghGo jf cGo s'g} k|sf/sf] zf]if0f ug{ \
             jf cg'lrt k|of]u ug{ kfOg] 5}g . k|To]s afnaflnsfnfO{ afn cg's\"n \
             Gofosf] xs x'g]5 . $)= blntsf] xs M -!_ /fHosf ;a} lgsfodf blntnfO{.";
    assert_eq!(classify(t), Language::MojibakeSuspected);
}

#[test]
fn trademark_filings_are_latin_not_mojibake() {
    // Real false-positive from the prior run: DOI trademark registry text
    // was heavy on commas/periods/parens but contained zero mid-word brace
    // characters. Earlier classifier tagged this as MojibakeSuspected; the
    // tightened classifier must leave it as Latin.
    let t = "Filing Date : 2080.01.06 NICE Class : 25 Goods/ Services : \
             AS PER HRC 294. Applicant : BEIJING NEW ORIENTAL XUNCHENG \
             NETWORK TECHNOLOGY CO., LTD., ROOM 1801-08, FLOOR 18, NO. 2, \
             HAIDIAN EAST THIRD STREET, HAIDIAN DISTRICT, BEIJING, \
             People's Republic of China Mark Name : dongfangzhenxuan \
             Mark Type : D Application No. : 108157 Filing Date : \
             2080.01.06 NICE Class : 25 Goods/ Services : AS PER HRC don.";
    assert_eq!(classify(t), Language::Latin);
}

#[test]
fn punctuation_heavy_tails_classify_as_other() {
    // A chunk of mostly whitespace/bullets/numbers — no retrieval signal.
    let t = "- - - - - - 1. 2. 3. 4. 5. 6. 7.        ";
    assert_eq!(classify(t), Language::Other);
}

#[test]
fn substantive_chars_excludes_noise() {
    let t = "नेपाल Nepal 2026 - - - ";
    let n = substantive_chars(t);
    // "नेपाल" = 5 Devanagari chars; "Nepal" = 5 Latin alpha; digits+dashes+spaces excluded.
    assert_eq!(n, 10);
}

#[test]
fn short_chunks_classify_as_other_regardless_of_content() {
    // Even legitimate Devanagari below the 50-substantive-char floor is
    // treated as Other — retrieval signal is too thin to rank.
    let t = "नेपाल।";
    assert_eq!(classify(t), Language::Other);
}
