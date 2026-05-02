//! URL canonicalization, classification, and priority scoring.
//!
//! All functions are deterministic and side-effect-free: same input ⇒ same
//! output, every call. That guarantee is load-bearing for the frontier's
//! seen-set (which dedups canonical strings) and for diff detection (which
//! keys on `(source_id, url)`).
//!
//! ## Pipeline (see CRAWLER.md §Algorithm §2)
//!
//! The order matters — don't reshuffle without reading the comments on each
//! step. For every discovered link we apply:
//!
//! 1. parse + scheme check (http/https only)
//! 2. percent-encode (handled by the `url` crate on parse)
//! 3. strip fragment
//! 4. strip tracking params (utm_*, fbclid, gclid, ref, source, _ga)
//! 5. sort remaining query params lexicographically
//! 6. lowercase scheme+host (url crate does this)
//! 7. path normalization: strip /index.html, collapse //, strip trailing /
//! 8. reject pathological paths (length, segments, repeated-segment bomb)
//! 9. reject trap patterns (admin, login, print, feed, calendar, paging>20)
//! 10. reject blocked extensions (images, video, archives, css, js)
//!
//! The output string is stable: parsing it again and re-canonicalizing it
//! produces the exact same string. Property-tested.

use std::collections::HashMap;
use url::Url;

/// Why a URL was rejected during canonicalization. Diagnostic; workers can
/// bucket by reason to surface per-site filtering patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rejection {
    NonHttp,
    BadUrl,
    PathTooLong,
    TooManySegments,
    RepeatedSegment,
    TrapPattern,
    PaginationTooDeep,
    CalendarCombinatorics,
    BlockedExtension,
}

/// High-level category used for priority scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentClass {
    Document,    // PDFs, docx, xlsx — what we most want
    ContentPage, // /content/, /act/, /notice/, /circular/, etc.
    Listing,     // /category/, /archive/, ?page=N for N≤5
    Navigation,  // default catch-all
    LowValue,    // /gallery/, /team/, /contact/, etc.
}

// --- policy tables --------------------------------------------------------

/// File extensions we treat as "target documents" for scoring purposes.
pub const DOC_EXTS: &[&str] = &[
    ".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".txt", ".csv",
];

/// File extensions we refuse to fetch. Binary assets we don't index (images,
/// fonts, video, archives) plus web build artifacts (.css, .js, source maps).
pub const BLOCKED_EXTS: &[&str] = &[
    ".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".svg", ".ico",
    ".mp3", ".mp4", ".avi", ".mov", ".mkv", ".webm", ".wav", ".ogg",
    ".woff", ".woff2", ".ttf", ".otf", ".eot",
    ".zip", ".rar", ".tar", ".gz", ".7z",
    ".css", ".js", ".mjs", ".map",
];

/// Query params to strip unconditionally — analytics/affiliate noise that
/// produces false "different URL, same content" views.
pub const TRACKING_PARAMS: &[&str] = &[
    "fbclid", "gclid", "msclkid", "dclid",
    "ref", "referrer", "source", "_ga", "_gl",
    "mc_cid", "mc_eid",
    // utm_* handled by prefix match
];

/// Path segments (exact, case-insensitive) that indicate a crawler trap.
/// We match segment-exact rather than substring so `/login` and `/login/`
/// both match, while a content URL like `/posts/login-tutorial` does not.
const TRAP_SEGMENTS: &[&str] = &[
    "admin", "wp-admin", "wp-login.php", "wp-json", "admin.php",
    "login", "login.php", "logout", "signin", "signout",
    "print", "amp",
    "feed", "feed.xml", "rss", "rss.xml", "atom.xml",
    "cgi-bin",
];

/// Path segments (exact, case-insensitive) marking a content leaf page
/// worth prioritizing in the frontier.
const CONTENT_SEGMENTS: &[&str] = &[
    "content", "post", "posts", "download", "downloads",
    "notice", "notices", "circular", "circulars",
    "act", "acts", "rule", "rules",
    "bulletin", "bulletins", "announcement", "announcements",
];

/// Path segments marking a listing/archive page.
const LISTING_SEGMENTS: &[&str] = &["category", "categories", "archive", "archives"];

/// Path segments marking low-value nav pages.
const LOW_VALUE_SEGMENTS: &[&str] = &[
    "gallery", "carousel", "team", "staff",
    "contact", "contacts", "map", "tag", "tags",
];

const MAX_PATH_LEN: usize = 500;
const MAX_PATH_SEGMENTS: usize = 15;
const MAX_REPEATED_SEGMENT: u32 = 2; // any segment may appear at most this many times
const MAX_PAGINATION: u32 = 20;

// --- canonicalization -----------------------------------------------------

/// Canonicalize a discovered link relative to `base`.
///
/// Returns the canonical URL string on success, or a [`Rejection`] reason
/// on failure. Does **not** enforce same-site — that's a separate step
/// (the worker knows the home host).
pub fn canonicalize(base: &str, href: &str) -> Result<String, Rejection> {
    let base_url = Url::parse(base).map_err(|_| Rejection::BadUrl)?;
    let mut u = base_url.join(href).map_err(|_| Rejection::BadUrl)?;

    match u.scheme() {
        "http" | "https" => {}
        _ => return Err(Rejection::NonHttp),
    }

    // step 3: drop fragment
    u.set_fragment(None);

    // step 4+5: tracking-param strip + lex-sort
    let kept: Vec<(String, String)> = u
        .query_pairs()
        .filter(|(k, _)| !is_tracking_param(k))
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let mut sorted = kept;
    sorted.sort();
    u.set_query(None);
    if !sorted.is_empty() {
        let mut q = u.query_pairs_mut();
        for (k, v) in &sorted {
            q.append_pair(k, v);
        }
    }

    // step 7: path normalization
    let normalized = normalize_path(u.path());
    u.set_path(&normalized);

    // step 8: pathology
    if normalized.len() > MAX_PATH_LEN {
        return Err(Rejection::PathTooLong);
    }
    let segs: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
    if segs.len() > MAX_PATH_SEGMENTS {
        return Err(Rejection::TooManySegments);
    }
    let mut counts: HashMap<&str, u32> = HashMap::with_capacity(segs.len());
    for s in &segs {
        let c = counts.entry(s).or_insert(0);
        *c += 1;
        if *c > MAX_REPEATED_SEGMENT {
            return Err(Rejection::RepeatedSegment);
        }
    }

    // step 9: trap patterns (order matters — specific checks before generic)
    if is_trap_path(&normalized) {
        return Err(Rejection::TrapPattern);
    }
    if let Some(page) = pagination_page(&u) {
        if page > MAX_PAGINATION {
            return Err(Rejection::PaginationTooDeep);
        }
    }
    if is_calendar_combinatorics(&u) {
        return Err(Rejection::CalendarCombinatorics);
    }

    // step 10: blocked extensions
    if let Some(ext) = extension_of(&u) {
        if BLOCKED_EXTS.contains(&ext.as_str()) {
            return Err(Rejection::BlockedExtension);
        }
    }

    Ok(u.to_string())
}

/// Normalize a URL path:
///   - collapse consecutive slashes (`//`)
///   - strip trailing `/index.html` / `/index.htm`
///   - strip a single trailing `/` (unless it's the root)
///
/// Returns "/" for empty input.
pub fn normalize_path(path: &str) -> String {
    let mut s = path.to_string();
    // Strip trailing index files before collapsing slashes so we don't miss
    // `//index.html` after collapse.
    for suffix in ["/index.html", "/index.htm", "/index.php"] {
        if s.ends_with(suffix) {
            s.truncate(s.len() - suffix.len());
            break;
        }
    }
    // Collapse duplicate slashes.
    while s.contains("//") {
        s = s.replace("//", "/");
    }
    // Strip single trailing slash, preserving root.
    if s.len() > 1 && s.ends_with('/') {
        s.pop();
    }
    if s.is_empty() {
        "/".to_string()
    } else {
        s
    }
}

fn is_tracking_param(k: &str) -> bool {
    k.starts_with("utm_") || TRACKING_PARAMS.contains(&k)
}

/// True if any path segment (case-insensitive, exact) is in `wanted`.
///
/// Segment-based matching avoids false positives on slugs that happen to
/// contain a banned word (e.g., `/posts/login-tutorial` against `login`).
fn path_has_any_segment(path: &str, wanted: &[&str]) -> bool {
    let lc = path.to_ascii_lowercase();
    for seg in lc.split('/').filter(|s| !s.is_empty()) {
        if wanted.contains(&seg) {
            return true;
        }
    }
    false
}

fn is_trap_path(path: &str) -> bool {
    path_has_any_segment(path, TRAP_SEGMENTS)
}

fn pagination_page(u: &Url) -> Option<u32> {
    for (k, v) in u.query_pairs() {
        if k == "page" || k == "p" || k == "pg" || k == "pageno" {
            if let Ok(n) = v.parse::<u32>() {
                return Some(n);
            }
        }
    }
    None
}

/// A URL is a calendar-combinatorics trap if the query carries a year AND a
/// month/day/date. Those combinatorially explode (12 × 31 × N years) without
/// adding content value.
fn is_calendar_combinatorics(u: &Url) -> bool {
    let mut has_year = false;
    let mut has_month_or_day = false;
    for (k, _) in u.query_pairs() {
        match k.as_ref() {
            "year" | "yr" | "y" => has_year = true,
            "month" | "mon" | "m" | "day" | "d" | "date" => has_month_or_day = true,
            _ => {}
        }
    }
    has_year && has_month_or_day
}

/// Lowercased file extension (including dot) of the URL path's last segment,
/// or None if no extension.
pub fn extension_of(u: &Url) -> Option<String> {
    let path = u.path();
    let last = path.rsplit('/').next()?;
    let idx = last.rfind('.')?;
    if idx == 0 {
        return None; // dotfile, not an extension
    }
    let ext = &last[idx..];
    // Basic sanity: 1-6 chars after the dot, alphanumeric.
    if ext.len() < 2 || ext.len() > 7 {
        return None;
    }
    if !ext[1..].chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(ext.to_ascii_lowercase())
}

// --- same-site check ------------------------------------------------------

/// Same-site check using the Public Suffix List.
///
/// Two URLs are same-site iff they share a registrable domain. For
/// `moha.gov.np`, the registrable is `moha.gov.np` (since `gov.np` is a
/// PSL entry). Subdomains like `aaosatbise.moha.gov.np` resolve to the
/// same registrable and therefore count as same-site.
pub fn same_site(a: &str, b: &str) -> bool {
    match (registrable_domain(a), registrable_domain(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Extract the registrable-domain portion of a URL or host string. Returns
/// None if the input can't be parsed or has no recognizable public suffix.
pub fn registrable_domain(url_or_host: &str) -> Option<String> {
    let host = Url::parse(url_or_host)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| url_or_host.to_string());
    let host_lc = host.to_ascii_lowercase();
    let domain = psl::domain(host_lc.as_bytes())?;
    Some(String::from_utf8_lossy(domain.as_bytes()).to_string())
}

// --- classification + scoring --------------------------------------------

/// Classify a canonical URL into a [`ContentClass`]. Used by [`score`]; also
/// useful for telemetry (how many Document vs Navigation URLs did we see?).
pub fn classify(url: &str) -> ContentClass {
    let u = match Url::parse(url) {
        Ok(u) => u,
        Err(_) => return ContentClass::Navigation,
    };

    if let Some(ext) = extension_of(&u) {
        if DOC_EXTS.contains(&ext.as_str()) {
            return ContentClass::Document;
        }
    }

    let path = u.path();
    // Listing before Content so a URL like /category/acts (both a "category"
    // segment and an "acts" segment) is classified as Listing. A single act
    // lives at /acts/N and would not hit a listing segment.
    if path_has_any_segment(path, LISTING_SEGMENTS) {
        return ContentClass::Listing;
    }
    if path_has_any_segment(path, CONTENT_SEGMENTS) {
        return ContentClass::ContentPage;
    }
    if let Some(page) = pagination_page(&u) {
        if page <= 5 {
            return ContentClass::Listing;
        }
    }
    if path_has_any_segment(path, LOW_VALUE_SEGMENTS) {
        return ContentClass::LowValue;
    }

    ContentClass::Navigation
}

/// Priority score for a canonical URL at a given BFS depth. Higher = more
/// interesting. See CRAWLER.md §Algorithm §1.
pub fn score(url: &str, depth: u32) -> i32 {
    const BASE: i32 = 100;
    const DEPTH_PENALTY: i32 = 2;
    let boost = match classify(url) {
        ContentClass::Document => 30,
        ContentClass::ContentPage => 20,
        ContentClass::Listing => 10,
        ContentClass::Navigation => 0,
        ContentClass::LowValue => -5,
    };
    BASE + boost - DEPTH_PENALTY * depth as i32
}
