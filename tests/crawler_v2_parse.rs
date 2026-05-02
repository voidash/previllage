//! Tests for crawler_v2::parse — HTML extraction + link discovery + dispatch.

use gemma_god::crawler_v2::parse::{parse, parse_html, BinaryKind, ParsedDoc};

#[test]
fn extracts_title_lang_text_and_links() {
    let html = r##"
        <!doctype html>
        <html lang="ne">
          <head><title>नेपाल राजपत्र</title></head>
          <body>
            <nav><a href="/about">About</a></nav>
            <article>
              <h1>निर्देशिका</h1>
              <p>नेपाल सरकारको आधिकारिक राजपत्र। यो एक परीक्षण लेख हो जसमा
                 १०० भन्दा बढी अक्षरहरू छन्, ताकि एक्स्ट्रक्टर ले यसलाई मुख्य
                 सामग्री को रुपमा पहिचान गर्छ।</p>
              <a href="/acts/1.pdf">Act 1</a>
              <a href="https://other.gov.np/x">External</a>
            </article>
            <footer>copyright 2026</footer>
          </body>
        </html>
    "##;
    let parsed = parse_html("https://rajpatra.dop.gov.np/", html.as_bytes());

    assert_eq!(parsed.title.as_deref(), Some("नेपाल राजपत्र"));
    assert_eq!(parsed.lang.as_deref(), Some("ne"));
    // Article text lands in extraction; footer + nav do not.
    assert!(parsed.extracted_text.contains("निर्देशिका"));
    assert!(parsed.extracted_text.contains("१००"));
    assert!(!parsed.extracted_text.contains("copyright"));
    assert!(!parsed.extracted_text.contains("About"));
    // Links canonicalized + deduped; /about resolves relative to base.
    assert!(parsed.links.iter().any(|l| l.ends_with("/about")));
    assert!(parsed.links.iter().any(|l| l.ends_with("/acts/1.pdf")));
    assert!(parsed.links.iter().any(|l| l.starts_with("https://other.gov.np/")));
}

#[test]
fn devanagari_text_char_count_preserved() {
    let html = r##"
        <html>
          <body><article>
            <p>नेपाली भाषामा लेखिएको एक छोटो वाक्य।</p>
          </article></body>
        </html>"##;
    let parsed = parse_html("https://x.gov.np/", html.as_bytes());
    // Char count counts Devanagari codepoints, not UTF-8 bytes.
    assert!(parsed.extracted_text.chars().any(|c| (0x0900..=0x097F).contains(&(c as u32))));
}

#[test]
fn collapses_whitespace() {
    let html = r##"
        <html><body><main>
          <p>one\n\n\n  two\t\t  three</p>
          <p>four</p>
        </main></body></html>"##;
    let parsed = parse_html("https://x.gov.np/", html.as_bytes());
    // Multiple spaces/tabs collapse; no double spaces in output.
    assert!(!parsed.extracted_text.contains("  "));
    // All four tokens appear.
    for w in ["one", "two", "three", "four"] {
        assert!(parsed.extracted_text.contains(w), "missing {w}");
    }
}

#[test]
fn script_count_and_raw_link_count_are_tracked() {
    let html = r##"
        <html><head>
          <script>a()</script><script src="x"></script>
        </head><body>
          <main><p>Hi</p></main>
          <a href="/one">1</a><a href="/two">2</a><a href="#">frag</a>
        </body></html>"##;
    let p = parse_html("https://x.gov.np/", html.as_bytes());
    assert_eq!(p.script_count, 2);
    // raw_link_count includes href="#" (still a discovered anchor). Canon
    // filter rejects fragments + missing schemes, so p.links is smaller.
    assert_eq!(p.raw_link_count, 3);
    assert!(p.links.iter().any(|l| l.ends_with("/one")));
    assert!(p.links.iter().any(|l| l.ends_with("/two")));
}

#[test]
fn falls_back_to_body_when_no_main_selector_matches() {
    // No <main>, <article>, or #content — must still extract from body.
    let html = r##"
        <html><body>
          <div><p>The body has content but no semantic container. We still
                extract text from body when no main/article selector matches,
                so a site that doesn't use HTML5 semantics still gets its
                content captured. At least 100 chars so we don't drop it.</p></div>
        </body></html>"##;
    let p = parse_html("https://x.gov.np/", html.as_bytes());
    assert!(p.extracted_text.contains("no semantic container"));
    assert!(p.extracted_text.len() >= 100);
}

#[test]
fn drops_nav_header_footer_aside_iframe_form() {
    let html = r##"
        <html><body>
          <header>HEADER_JUNK</header>
          <nav>NAV_JUNK</nav>
          <main>
            <p>Primary content goes here and is long enough to survive.
               Primary content goes here and is long enough to survive.</p>
          </main>
          <aside>ASIDE_JUNK</aside>
          <footer>FOOTER_JUNK</footer>
          <form>FORM_JUNK</form>
          <iframe>IFRAME_JUNK</iframe>
        </body></html>"##;
    let p = parse_html("https://x.gov.np/", html.as_bytes());
    for junk in ["HEADER_JUNK", "NAV_JUNK", "ASIDE_JUNK", "FOOTER_JUNK", "FORM_JUNK", "IFRAME_JUNK"] {
        assert!(!p.extracted_text.contains(junk), "leaked: {junk}");
    }
    assert!(p.extracted_text.contains("Primary content"));
}

#[test]
fn anchor_fragments_and_nonhttp_links_dropped() {
    let html = r##"
        <html><body>
          <a href="#top">anchor</a>
          <a href="mailto:a@b.c">email</a>
          <a href="javascript:void(0)">js</a>
          <a href="/real">real</a>
        </body></html>"##;
    let p = parse_html("https://x.gov.np/", html.as_bytes());
    // Only /real survives canonicalization.
    assert_eq!(p.links.len(), 1, "links: {:?}", p.links);
    assert!(p.links[0].ends_with("/real"));
    // raw_link_count still reflects all four anchors.
    assert_eq!(p.raw_link_count, 4);
}

#[test]
fn empty_body_returns_empty_parsed() {
    let p = parse_html("https://x.gov.np/", b"");
    assert_eq!(p.extracted_text, "");
    assert_eq!(p.links.len(), 0);
    assert_eq!(p.script_count, 0);
    assert_eq!(p.raw_link_count, 0);
}

// -------- dispatch tests --------

#[test]
fn dispatch_html_returns_parsedhtml() {
    let doc = parse(
        "text/html; charset=utf-8",
        "https://x.gov.np/",
        b"<html><body><main><p>hi</p></main></body></html>",
    );
    assert!(matches!(doc, ParsedDoc::Html(_)));
}

#[test]
fn dispatch_pdf_by_content_type() {
    let doc = parse("application/pdf", "https://x.gov.np/a", b"\x25PDF-1.4...");
    match doc {
        ParsedDoc::Binary { kind, size } => {
            assert_eq!(kind, BinaryKind::Pdf);
            assert!(size > 0);
        }
        other => panic!("expected Binary(Pdf), got {other:?}"),
    }
}

#[test]
fn dispatch_pdf_by_url_extension_when_content_type_is_octet_stream() {
    let doc = parse(
        "application/octet-stream",
        "https://x.gov.np/downloads/notice.pdf",
        b"fake bytes",
    );
    match doc {
        ParsedDoc::Binary { kind, .. } => assert_eq!(kind, BinaryKind::Pdf),
        other => panic!("expected Binary(Pdf) via ext sniff, got {other:?}"),
    }
}

#[test]
fn dispatch_docx_xlsx_pptx_recognized() {
    let cases = [
        ("application/vnd.openxmlformats-officedocument.wordprocessingml.document", BinaryKind::Docx),
        ("application/vnd.openxmlformats-officedocument.spreadsheetml.sheet", BinaryKind::Xlsx),
        ("application/vnd.openxmlformats-officedocument.presentationml.presentation", BinaryKind::Pptx),
        ("application/msword", BinaryKind::Doc),
        ("application/vnd.ms-excel", BinaryKind::Xls),
        ("application/vnd.ms-powerpoint", BinaryKind::Ppt),
    ];
    for (ct, expected) in cases {
        match parse(ct, "https://x.gov.np/a", b"data") {
            ParsedDoc::Binary { kind, .. } => assert_eq!(kind, expected, "ct={ct}"),
            other => panic!("ct={ct} got {other:?}"),
        }
    }
}

#[test]
fn dispatch_unknown_content_returns_unsupported() {
    let doc = parse("application/x-tar", "https://x.gov.np/a.tar", b"bytes");
    match doc {
        ParsedDoc::Unsupported { content_type, .. } => {
            assert!(content_type.contains("tar"));
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}
