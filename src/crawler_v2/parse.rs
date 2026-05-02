//! Content-type-driven dispatch: given a raw response body, extract the
//! useful bits.
//!
//! - **HTML** → drop script/style/nav/footer/aside/iframe, pick main content
//!   region via common selectors, collapse whitespace, discover outbound
//!   links (and canonicalize each via [`crate::crawler_v2::url::canonicalize`]).
//! - **PDF / Office docs** → preserve raw bytes; text extraction is deferred
//!   to the existing Rust ingest pipeline (pdftotext + Preeti + OCR).
//! - **Everything else** → record presence, no processing.
//!
//! The extractor is deliberately modest (~50 lines of hand-rolled heuristics).
//! Nepal gov sites tend to have simple markup; a full Readability port would
//! be gold-plating. Swap in a real one if a future audit shows extraction
//! quality problems.

use super::url as crawler_url;
use scraper::{Html, Selector};
use std::collections::BTreeSet;

/// Post-parse output for an HTML page.
#[derive(Debug, Clone, Default)]
pub struct ParsedHtml {
    pub title: Option<String>,
    pub lang: Option<String>,
    /// Readable content text with whitespace collapsed.
    pub extracted_text: String,
    /// Canonicalized (and filtered) outbound links discovered in <a href>.
    /// Rejected URLs are silently dropped; the caller is responsible for
    /// same-site filtering (it needs the home domain).
    pub links: Vec<String>,
    /// Number of `<script>` elements in the raw HTML. Used by shell-detect.
    pub script_count: usize,
    /// Number of raw `<a href>` elements before canonicalization + filtering.
    /// Delta vs `links.len()` reveals how many URLs the filter rejected.
    pub raw_link_count: usize,
}

#[derive(Debug, Clone)]
pub enum ParsedDoc {
    Html(ParsedHtml),
    Binary { kind: BinaryKind, size: usize },
    Unsupported { content_type: String, size: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinaryKind {
    Pdf,
    Docx,
    Xlsx,
    Pptx,
    Doc,
    Xls,
    Ppt,
}

/// Entry point: dispatch on content-type. `url` is the fetched URL (post-
/// redirect); used as the base for relative-link resolution and as an
/// extension-sniff fallback when content-type is missing or generic.
pub fn parse(content_type: &str, url: &str, body: &[u8]) -> ParsedDoc {
    let ct = content_type.to_ascii_lowercase();
    if ct.contains("html") {
        return ParsedDoc::Html(parse_html(url, body));
    }
    if let Some(kind) = classify_binary(&ct, url) {
        return ParsedDoc::Binary {
            kind,
            size: body.len(),
        };
    }
    ParsedDoc::Unsupported {
        content_type: ct,
        size: body.len(),
    }
}

fn classify_binary(ct: &str, url: &str) -> Option<BinaryKind> {
    // Content-Type is sometimes a lie (application/octet-stream for everything).
    // Fall back to URL extension sniffing via the canonicalizer's helper.
    if ct.contains("pdf") {
        return Some(BinaryKind::Pdf);
    }
    if ct.contains("wordprocessingml") {
        return Some(BinaryKind::Docx);
    }
    if ct.contains("spreadsheetml") {
        return Some(BinaryKind::Xlsx);
    }
    if ct.contains("presentationml") {
        return Some(BinaryKind::Pptx);
    }
    if ct.contains("msword") {
        return Some(BinaryKind::Doc);
    }
    if ct.contains("vnd.ms-excel") {
        return Some(BinaryKind::Xls);
    }
    if ct.contains("vnd.ms-powerpoint") {
        return Some(BinaryKind::Ppt);
    }

    // Extension sniff fallback.
    let parsed = ::url::Url::parse(url).ok()?;
    let ext = crawler_url::extension_of(&parsed)?;
    match ext.as_str() {
        ".pdf" => Some(BinaryKind::Pdf),
        ".docx" => Some(BinaryKind::Docx),
        ".xlsx" => Some(BinaryKind::Xlsx),
        ".pptx" => Some(BinaryKind::Pptx),
        ".doc" => Some(BinaryKind::Doc),
        ".xls" => Some(BinaryKind::Xls),
        ".ppt" => Some(BinaryKind::Ppt),
        _ => None,
    }
}

/// Parse an HTML document: pull out title, lang, readable text, and outbound
/// links. Non-fatal on malformed markup — scraper tolerates a lot.
pub fn parse_html(base_url: &str, body: &[u8]) -> ParsedHtml {
    let text = String::from_utf8_lossy(body);
    let doc = Html::parse_document(&text);

    let title = select_first_text(&doc, "title").filter(|s| !s.trim().is_empty());

    let lang = Selector::parse("html")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
        .and_then(|el| el.value().attr("lang"))
        .map(|s| s.to_string());

    let script_count = Selector::parse("script")
        .map(|sel| doc.select(&sel).count())
        .unwrap_or(0);

    let links = extract_links(&doc, base_url);
    let raw_link_count = Selector::parse("a[href]")
        .map(|sel| doc.select(&sel).count())
        .unwrap_or(0);

    let extracted_text = extract_readable_text(&doc);

    ParsedHtml {
        title,
        lang,
        extracted_text,
        links: links.links,
        script_count,
        raw_link_count,
    }
}

fn select_first_text(doc: &Html, sel_str: &str) -> Option<String> {
    let sel = Selector::parse(sel_str).ok()?;
    let el = doc.select(&sel).next()?;
    Some(el.text().collect::<String>().trim().to_string())
}

struct DiscoveredLinks {
    links: Vec<String>,
}

fn extract_links(doc: &Html, base_url: &str) -> DiscoveredLinks {
    let sel = match Selector::parse("a[href]") {
        Ok(s) => s,
        Err(_) => return DiscoveredLinks { links: Vec::new() },
    };
    // BTreeSet for deterministic ordering + dedup at extraction time.
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for el in doc.select(&sel) {
        let href = match el.value().attr("href") {
            Some(h) => h.trim(),
            None => continue,
        };
        if href.is_empty() || href.starts_with(['#']) {
            continue;
        }
        if let Ok(canon) = crawler_url::canonicalize(base_url, href) {
            seen.insert(canon);
        }
    }
    DiscoveredLinks {
        links: seen.into_iter().collect(),
    }
}

/// Noise selectors removed wholesale before text extraction. Keeping this
/// list conservative — anything unique to a site goes in a per-source recipe.
const NOISE_SELECTORS: &[&str] = &[
    "script",
    "style",
    "noscript",
    "nav",
    "header",
    "footer",
    "aside",
    "iframe",
    "form",
    "button",
];

/// Try these in order and use the first that returns non-empty content.
/// Falls back to `<body>` if nothing matches.
const CONTENT_SELECTORS: &[&str] = &[
    "main",
    "article",
    "[role=\"main\"]",
    "#content",
    "#main-content",
    "#main",
    ".content",
    ".post",
    ".entry-content",
    ".post-content",
];

fn extract_readable_text(doc: &Html) -> String {
    for sel_str in CONTENT_SELECTORS {
        if let Ok(sel) = Selector::parse(sel_str) {
            if let Some(el) = doc.select(&sel).next() {
                let text = collect_text_excluding_noise(el);
                if text.len() >= 100 {
                    return text;
                }
            }
        }
    }
    if let Ok(body_sel) = Selector::parse("body") {
        if let Some(body) = doc.select(&body_sel).next() {
            return collect_text_excluding_noise(body);
        }
    }
    String::new()
}

fn collect_text_excluding_noise(root: scraper::ElementRef<'_>) -> String {
    let noise: Vec<Selector> = NOISE_SELECTORS
        .iter()
        .filter_map(|s| Selector::parse(s).ok())
        .collect();

    // Walk descendants via text() but skip any node whose ancestor chain
    // includes a noise element. Easiest way in scraper: run over descendants
    // and check ancestry inline.
    let mut out = String::new();
    for node in root.descendants() {
        if let Some(text) = node.value().as_text() {
            let raw: &str = text;
            if raw.trim().is_empty() {
                continue;
            }
            // Check ancestry for noise membership.
            let mut skip = false;
            for anc in node.ancestors() {
                if let Some(e) = scraper::ElementRef::wrap(anc) {
                    if noise.iter().any(|n| n.matches(&e)) {
                        skip = true;
                        break;
                    }
                }
            }
            if skip {
                continue;
            }
            // Collapse whitespace as we go.
            let mut prev_ws = out.ends_with(char::is_whitespace) || out.is_empty();
            for c in raw.chars() {
                if c.is_whitespace() {
                    if !prev_ws {
                        out.push(' ');
                        prev_ws = true;
                    }
                } else {
                    out.push(c);
                    prev_ws = false;
                }
            }
        }
    }
    out.trim().to_string()
}
