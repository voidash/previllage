//! Tests for the intra-chunk repetition detector.

use gemma_god::crawler_v2::chunk::is_internally_repetitive;

#[test]
fn land_law_page_header_repetition_detected() {
    // Real shape from ag_gov_np land-law PDF: the page header
    // `भूमि सम्बन्धी कानूनी स्रोत सामग्री` repeats on every page with a
    // cycling page-number token. Boilerplate dedup doesn't catch it
    // because each chunk is textually distinct. The internal-repetition
    // detector must.
    let header = "भूमि सम्बन्धी कानूनी स्रोत सामग्री ";
    let text = [
        "द्दटज्ञ", "द्दटद्द", "द्दटघ", "द्दटद्ध", "द्दटछ", "द्दटट",
        "द्दटठ", "द्दटड",
    ]
    .iter()
    .map(|pg| format!("{header}{pg}"))
    .collect::<Vec<_>>()
    .join(" ");
    // Window 30 is smaller than the 32-char header so common prefix fits.
    assert!(
        is_internally_repetitive(&text, 4, 30),
        "header repetition not detected: {text:?}"
    );
}

#[test]
fn legitimate_prose_is_not_flagged() {
    // A normal paragraph of gov content — each sentence is unique prose.
    // Must NOT fire.
    let text = "नेपाल सरकारको गृह मन्त्रालयले नयाँ नियमावली जारी गरेको छ। \
                यो नियमावली सम्बन्धी सम्पूर्ण जानकारी मन्त्रालयको \
                आधिकारिक वेबसाइटमा उपलब्ध छ। नागरिकहरूले कुनै पनि प्रकारको \
                समस्या आइपरेमा कार्यालयमा सम्पर्क गर्न सकिनेछ।";
    assert!(
        !is_internally_repetitive(text, 4, 30),
        "legitimate prose flagged"
    );
}

#[test]
fn english_boilerplate_list_not_flagged_for_normal_repeats() {
    // A list of distinct items — even though a common prefix exists, it
    // shouldn't fire under the 4-repeat, 30-char threshold (prefix too
    // short to form a 30-char window that repeats).
    let text = "Item 1: apple. Item 2: banana. Item 3: cherry. \
                Item 4: date. Item 5: elderberry. Item 6: fig.";
    assert!(!is_internally_repetitive(text, 4, 30));
}

#[test]
fn heavy_repetition_at_3_times_not_enough_4_is() {
    let fragment = "The same thirty one character prefix here. ";
    let three_times = fragment.repeat(3);
    let four_times = fragment.repeat(4);
    assert!(!is_internally_repetitive(&three_times, 4, 30));
    assert!(is_internally_repetitive(&four_times, 4, 30));
}

#[test]
fn short_chunks_never_flagged() {
    // Window 30 × min_repeats 4 = 120 chars minimum to even test.
    let text = "too short ".repeat(10); // 100 chars
    assert!(!is_internally_repetitive(&text, 4, 30));
}
