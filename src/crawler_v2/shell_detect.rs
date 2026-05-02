//! Heuristic detection of JS-rendered "shell" pages.
//!
//! A JS-rendered shell serves bootstrap HTML whose visible content is injected
//! client-side by JavaScript. Our HTTP fetch only sees the shell: a few
//! hundred bytes of `<div id="app"></div>` plus one or two `<script>` tags
//! that load the Vue/React bundle.
//!
//! ## Signals (all three must hold)
//!
//!   1. extracted text < 200 chars
//!   2. raw link count < 3
//!   3. script count >= 1
//!
//! The first two are the strong signals — a shell has essentially no visible
//! content and no static outbound links. The third signal (any script
//! present) filters genuinely-broken or static-only pages from being
//! escalated to the Chromium fetcher: a 404 page with no JS is "thin," not
//! "shell," and Chromium wouldn't render more of it anyway.
//!
//! **Why not `script_count > 5`?** The original CRAWLER.md spec required
//! many scripts on the theory that shell pages are analytics-heavy. Real
//! data (oag.gov.np, psc.gov.np — modern Vue SPAs) ship with exactly 1-2
//! `<script type="module">` tags that bootstrap a Vite bundle. The looser
//! `>= 1` catches those while the tight text/link thresholds prevent false
//! positives on legitimate pages.
//!
//! When flagged, the policy is: mark the source `js_only` in the registry,
//! stop this fetch cycle, and on the next cycle route through the
//! chromiumoxide fetcher (Phase 3b).

use super::parse::ParsedHtml;

const MAX_TEXT_CHARS_BEFORE_SHELL: usize = 200;
const MIN_SCRIPT_COUNT_FOR_SHELL: usize = 1;
const MAX_LINKS_BEFORE_SHELL: usize = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellVerdict {
    pub is_shell: bool,
    pub text_chars: usize,
    pub script_count: usize,
    pub link_count: usize,
}

pub fn evaluate(parsed: &ParsedHtml) -> ShellVerdict {
    // Use .chars() count, not .len() — a page rendered in Devanagari that
    // contains 200 bytes might only be ~70 characters, and we shouldn't
    // mistake that for a shell.
    let text_chars = parsed.extracted_text.chars().count();
    let script_count = parsed.script_count;
    // Use raw_link_count (pre-canonicalization) so that an extraction that
    // filtered out-of-site links doesn't make us think the page is empty.
    let link_count = parsed.raw_link_count;

    let is_shell = text_chars < MAX_TEXT_CHARS_BEFORE_SHELL
        && link_count < MAX_LINKS_BEFORE_SHELL
        && script_count >= MIN_SCRIPT_COUNT_FOR_SHELL;

    ShellVerdict {
        is_shell,
        text_chars,
        script_count,
        link_count,
    }
}
