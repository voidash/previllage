//! Tests for shell_detect — the JS-render trip-wire.

use gemma_god::crawler_v2::parse::ParsedHtml;
use gemma_god::crawler_v2::shell_detect::evaluate;

fn make(text: &str, scripts: usize, raw_links: usize) -> ParsedHtml {
    ParsedHtml {
        title: None,
        lang: None,
        extracted_text: text.into(),
        links: vec![],
        script_count: scripts,
        raw_link_count: raw_links,
    }
}

#[test]
fn classic_shell_page_flagged() {
    // ~8 chars of text, 10 script tags, 0 links — the analytics-heavy shape.
    let p = make("Loading…", 10, 0);
    let v = evaluate(&p);
    assert!(v.is_shell);
    assert_eq!(v.text_chars, "Loading…".chars().count());
    assert_eq!(v.script_count, 10);
    assert_eq!(v.link_count, 0);
}

#[test]
fn modern_vue_spa_with_two_scripts_is_flagged() {
    // The actual oag.gov.np / psc.gov.np shape: 0 extracted chars, 0 links,
    // just 2 <script type="module"> tags for Vite bundle. An earlier spec
    // required script_count > 5 and missed these; real data forced the fix.
    let p = make("", 2, 0);
    let v = evaluate(&p);
    assert!(v.is_shell, "SPA with 2 scripts must be flagged: {v:?}");
}

#[test]
fn page_with_no_scripts_is_thin_not_shell() {
    // An empty page with zero scripts is a thin / 404 page, not a shell.
    // Escalating it to Chromium wouldn't render more of it — there's no JS.
    let p = make("Not found", 0, 0);
    assert!(!evaluate(&p).is_shell);
}

#[test]
fn page_with_enough_text_is_not_shell_even_with_many_scripts() {
    // Real content + many scripts (analytics-heavy site) is still real.
    let long_text = "a".repeat(500);
    let p = make(&long_text, 20, 0);
    assert!(!evaluate(&p).is_shell);
}

#[test]
fn page_with_enough_text_is_not_shell_even_with_one_script() {
    // Symmetrical case — the script count isn't a "more is more shell" signal;
    // it's a minimum filter.
    let long_text = "a".repeat(500);
    let p = make(&long_text, 1, 30);
    assert!(!evaluate(&p).is_shell);
}

#[test]
fn link_rich_page_is_not_shell_even_if_scripts_many() {
    // Landing page with few words but many links → not a shell.
    let p = make("Home page", 10, 30);
    assert!(!evaluate(&p).is_shell);
}


#[test]
fn devanagari_text_is_counted_in_chars_not_bytes() {
    // 200 bytes of Devanagari ≈ 66 chars. If we counted bytes we'd miss the
    // shell. This test pins that we use .chars().count().
    let devanagari = "नेपाल सरकारको गृह मन्त्रालय";
    let short = devanagari.repeat(3);
    assert!(
        short.len() > 200,
        "byte len {} should exceed threshold",
        short.len()
    );
    assert!(
        short.chars().count() < 200,
        "char count {} should be below",
        short.chars().count()
    );
    let p = make(&short, 10, 0);
    let v = evaluate(&p);
    // The text is short (char-wise), scripts are many, links are few — shell.
    assert!(v.is_shell);
}

#[test]
fn each_signal_individually_is_not_enough() {
    // Only short text + 0 scripts → thin page, not shell.
    assert!(!evaluate(&make("short", 0, 0)).is_shell);
    // Only many scripts on a long page → real page, not shell.
    let long_text = "x".repeat(500);
    assert!(!evaluate(&make(&long_text, 99, 0)).is_shell);
    // Long text + zero links + zero scripts → thin but with content,
    // not shell.
    assert!(!evaluate(&make(&long_text, 0, 0)).is_shell);
}
