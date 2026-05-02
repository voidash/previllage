//! Integration tests for crawler_v2::url — the canonicalization pipeline
//! plus classification + scoring + same-site.
//!
//! Organized by pipeline step so a regression points straight at the
//! offending stage.

use gemma_god::crawler_v2::url::{
    canonicalize, classify, extension_of, normalize_path, registrable_domain, same_site, score,
    ContentClass, Rejection, BLOCKED_EXTS, DOC_EXTS, TRACKING_PARAMS,
};

// ---------------------------------------------------------------- basics

#[test]
fn absolute_http_passes_through_with_lowercased_host() {
    let out = canonicalize("https://a.gov.np/", "https://Moha.GOV.np/notice/1").unwrap();
    // host lowercased, path preserved (case-sensitive on origin)
    assert!(out.starts_with("https://moha.gov.np/"));
    assert!(out.ends_with("/notice/1"));
}

#[test]
fn relative_href_joined_against_base() {
    let out = canonicalize("https://moha.gov.np/en/", "./post/123").unwrap();
    assert_eq!(out, "https://moha.gov.np/en/post/123");
}

#[test]
fn scheme_filter_rejects_non_http() {
    for href in ["ftp://x.com/", "mailto:a@b.c", "javascript:void(0)",
                 "file:///etc/passwd", "tel:+977123"] {
        let e = canonicalize("https://moha.gov.np/", href).unwrap_err();
        // mailto etc. may parse as opaque URL with a scheme; we reject at
        // the scheme check. url::Url rejects some as malformed at parse.
        assert!(
            matches!(e, Rejection::NonHttp | Rejection::BadUrl),
            "{href} got {e:?}"
        );
    }
}

#[test]
fn bad_url_is_bad_url() {
    let e = canonicalize("not a valid base", "whatever").unwrap_err();
    assert_eq!(e, Rejection::BadUrl);
}

// ------------------------------------------------------------ fragments

#[test]
fn fragment_is_stripped() {
    let out = canonicalize("https://x.gov.np/", "https://x.gov.np/a/b#section").unwrap();
    assert!(!out.contains('#'));
    assert_eq!(out, "https://x.gov.np/a/b");
}

// ------------------------------------------ tracking params + sort

#[test]
fn tracking_params_are_stripped() {
    let out = canonicalize(
        "https://x.gov.np/",
        "https://x.gov.np/a?utm_source=news&utm_medium=email&fbclid=abc&real=keep",
    )
    .unwrap();
    assert!(!out.contains("utm_"));
    assert!(!out.contains("fbclid"));
    assert!(out.contains("real=keep"));
}

#[test]
fn query_params_are_sorted_lexicographically() {
    let a = canonicalize("https://x.gov.np/", "https://x.gov.np/?b=2&a=1&c=3").unwrap();
    let b = canonicalize("https://x.gov.np/", "https://x.gov.np/?c=3&a=1&b=2").unwrap();
    // Both inputs must produce the same canonical form (dedup depends on this).
    assert_eq!(a, b);
    assert!(a.contains("a=1&b=2&c=3"));
}

#[test]
fn tracking_params_table_covers_common_suspects() {
    for param in TRACKING_PARAMS {
        assert!(!param.is_empty());
    }
    // utm_* is matched by prefix, not by table; sanity-check the prefix form.
    let out = canonicalize("https://x.gov.np/", "https://x.gov.np/a?utm_campaign=q1").unwrap();
    assert!(!out.contains("utm_campaign"));
}

// ------------------------------------------------ path normalization

#[test]
fn normalize_path_strips_index_files() {
    assert_eq!(normalize_path("/foo/index.html"), "/foo");
    assert_eq!(normalize_path("/index.html"), "/");
    assert_eq!(normalize_path("/a/index.php"), "/a");
    assert_eq!(normalize_path("/a/index.htm"), "/a");
}

#[test]
fn normalize_path_collapses_duplicate_slashes() {
    assert_eq!(normalize_path("//foo///bar//"), "/foo/bar");
}

#[test]
fn normalize_path_strips_trailing_slash_but_preserves_root() {
    assert_eq!(normalize_path("/foo/bar/"), "/foo/bar");
    assert_eq!(normalize_path("/"), "/");
    assert_eq!(normalize_path(""), "/");
}

// -------------------------------------------------------- pathology

#[test]
fn supremecourt_relative_path_bomb_rejected() {
    // The exact trap pattern that killed our overnight Python run.
    let bomb = "https://supremecourt.gov.np/web/assets/downloads/judgements\
                /assets/downloads/judgements/assets/downloads/judgements/1.pdf";
    let e = canonicalize("https://supremecourt.gov.np/", bomb).unwrap_err();
    assert_eq!(e, Rejection::RepeatedSegment);
}

#[test]
fn repeated_segment_triggers_even_without_full_compounding() {
    // A segment repeated 3 times is already a rejection (MAX_REPEATED_SEGMENT=2).
    let e = canonicalize("https://x.gov.np/", "https://x.gov.np/a/b/a/c/a/d").unwrap_err();
    assert_eq!(e, Rejection::RepeatedSegment);
}

#[test]
fn repeated_segment_twice_is_allowed() {
    // A segment repeated exactly twice is OK (e.g., /category/category-id).
    canonicalize("https://x.gov.np/", "https://x.gov.np/a/b/a/c").unwrap();
}

#[test]
fn path_too_long_rejected() {
    let long = "x".repeat(501);
    let url = format!("https://x.gov.np/{}", long);
    let e = canonicalize("https://x.gov.np/", &url).unwrap_err();
    assert_eq!(e, Rejection::PathTooLong);
}

#[test]
fn too_many_segments_rejected() {
    // 16 path segments > 15.
    let path = (1..=16).map(|i| format!("s{i}")).collect::<Vec<_>>().join("/");
    let url = format!("https://x.gov.np/{}", path);
    let e = canonicalize("https://x.gov.np/", &url).unwrap_err();
    assert_eq!(e, Rejection::TooManySegments);
}

// ---------------------------------------------------- trap patterns

#[test]
fn admin_login_traps_rejected() {
    for href in [
        "https://x.gov.np/admin/",
        "https://x.gov.np/wp-admin/",
        "https://x.gov.np/wp-login.php",
        "https://x.gov.np/login",
        "https://x.gov.np/signin",
    ] {
        let e = canonicalize("https://x.gov.np/", href).unwrap_err();
        assert_eq!(e, Rejection::TrapPattern, "href={href}");
    }
}

#[test]
fn feed_and_print_variants_rejected() {
    for href in [
        "https://x.gov.np/feed/",
        "https://x.gov.np/rss",
        "https://x.gov.np/print/page1",
        "https://x.gov.np/amp/notice",
    ] {
        let e = canonicalize("https://x.gov.np/", href).unwrap_err();
        assert_eq!(e, Rejection::TrapPattern, "href={href}");
    }
}

#[test]
fn unbounded_pagination_rejected() {
    // ≤20 passes, >20 rejected.
    canonicalize("https://x.gov.np/", "https://x.gov.np/list?page=20").unwrap();
    let e = canonicalize("https://x.gov.np/", "https://x.gov.np/list?page=21").unwrap_err();
    assert_eq!(e, Rejection::PaginationTooDeep);
    let e = canonicalize("https://x.gov.np/", "https://x.gov.np/list?page=9999").unwrap_err();
    assert_eq!(e, Rejection::PaginationTooDeep);
}

#[test]
fn calendar_combinatorics_rejected() {
    // year + month = calendar bomb.
    let e = canonicalize(
        "https://x.gov.np/",
        "https://x.gov.np/notices?year=2026&month=03",
    )
    .unwrap_err();
    assert_eq!(e, Rejection::CalendarCombinatorics);
    // year alone is fine (annual archive page).
    canonicalize("https://x.gov.np/", "https://x.gov.np/notices?year=2026").unwrap();
}

// --------------------------------------------- blocked extensions

#[test]
fn image_and_asset_extensions_blocked() {
    for ext in BLOCKED_EXTS {
        let href = format!("https://x.gov.np/assets/logo{ext}");
        let e = canonicalize("https://x.gov.np/", &href).unwrap_err();
        assert_eq!(e, Rejection::BlockedExtension, "ext={ext}");
    }
}

#[test]
fn doc_extensions_pass_through() {
    for ext in DOC_EXTS {
        let href = format!("https://x.gov.np/downloads/circular{ext}");
        canonicalize("https://x.gov.np/", &href).expect(ext);
    }
}

// --------------------------------------------------- extension_of

#[test]
fn extension_of_extracts_lowercase_with_dot() {
    let u = url::Url::parse("https://x.gov.np/foo/BAR.PDF").unwrap();
    assert_eq!(extension_of(&u), Some(".pdf".to_string()));
}

#[test]
fn extension_of_ignores_dotfiles() {
    let u = url::Url::parse("https://x.gov.np/.env").unwrap();
    assert_eq!(extension_of(&u), None);
}

#[test]
fn extension_of_ignores_absurd_extensions() {
    let u = url::Url::parse("https://x.gov.np/foo.toolongext").unwrap();
    assert_eq!(extension_of(&u), None);
}

// ------------------------------------------------- idempotence

#[test]
fn canonicalize_is_idempotent() {
    // Running canonicalize on its own output must produce the same string.
    // The frontier's seen-set dedup depends on this property.
    let inputs = [
        "https://moha.gov.np/",
        "https://MOHA.gov.np/en/post/1",
        "https://x.gov.np/a?utm_source=z&b=2&a=1#frag",
        "https://x.gov.np//foo///bar//index.html",
        "https://x.gov.np/a/b/c.pdf",
    ];
    for href in inputs {
        let c1 = canonicalize("https://x.gov.np/", href).unwrap();
        let c2 = canonicalize("https://x.gov.np/", &c1).unwrap();
        assert_eq!(c1, c2, "non-idempotent on {href}: {c1} -> {c2}");
    }
}

// ------------------------------------------------- classification

#[test]
fn classification_buckets_urls_correctly() {
    // Must pass canonicalization first (these are all legal URLs).
    assert_eq!(classify("https://x.gov.np/acts/1.pdf"), ContentClass::Document);
    assert_eq!(classify("https://x.gov.np/content/foo"), ContentClass::ContentPage);
    assert_eq!(classify("https://x.gov.np/notices/"), ContentClass::ContentPage);
    assert_eq!(classify("https://x.gov.np/category/acts"), ContentClass::Listing);
    assert_eq!(
        classify("https://x.gov.np/archive?page=3"),
        ContentClass::Listing
    );
    assert_eq!(classify("https://x.gov.np/"), ContentClass::Navigation);
    assert_eq!(classify("https://x.gov.np/gallery/2024"), ContentClass::LowValue);
    assert_eq!(classify("https://x.gov.np/contact"), ContentClass::LowValue);
}

#[test]
fn pagination_gt_5_is_not_listing() {
    // ?page=3 is a useful listing navigation; ?page=15 probably isn't —
    // we fall back to Navigation (0 boost) rather than Listing (+10).
    assert_eq!(
        classify("https://x.gov.np/list?page=3"),
        ContentClass::Listing
    );
    assert_eq!(
        classify("https://x.gov.np/list?page=15"),
        ContentClass::Navigation
    );
}

// ------------------------------------------------------- scoring

#[test]
fn score_ordering_matches_policy() {
    // Depth 0 baseline.
    let pdf = score("https://x.gov.np/acts/1.pdf", 0);
    let content = score("https://x.gov.np/content/foo", 0);
    let listing = score("https://x.gov.np/category/acts", 0);
    let nav = score("https://x.gov.np/", 0);
    let low = score("https://x.gov.np/gallery/a", 0);
    assert_eq!(pdf, 130);
    assert_eq!(content, 120);
    assert_eq!(listing, 110);
    assert_eq!(nav, 100);
    assert_eq!(low, 95);
    assert!(pdf > content && content > listing && listing > nav && nav > low);
}

#[test]
fn depth_penalty_is_two_per_hop() {
    let s0 = score("https://x.gov.np/acts/1.pdf", 0);
    let s5 = score("https://x.gov.np/acts/1.pdf", 5);
    assert_eq!(s0 - s5, 10);
}

// ----------------------------------------------- same-site (PSL)

#[test]
fn same_site_accepts_subdomains_of_same_registrable() {
    assert!(same_site("https://moha.gov.np/", "https://www.moha.gov.np/"));
    assert!(same_site(
        "https://moha.gov.np/",
        "https://aaosatbise.moha.gov.np/a"
    ));
}

#[test]
fn same_site_rejects_siblings_under_gov_np() {
    // Both have gov.np as public suffix → registrable is the label before it.
    assert!(!same_site("https://moha.gov.np/", "https://mofa.gov.np/"));
    assert!(!same_site(
        "https://moha.gov.np/",
        "https://giwmscdnone.gov.np/"
    ));
}

#[test]
fn same_site_handles_province_subdomains() {
    // moitfe.p1.gov.np — the public suffix is gov.np, registrable is p1.gov.np.
    // So moitfe.p1.gov.np and mosd.p1.gov.np are same-site (both under p1.gov.np).
    assert!(same_site("https://moitfe.p1.gov.np/", "https://mosd.p1.gov.np/"));
    // Different provinces should not.
    assert!(!same_site(
        "https://moitfe.p1.gov.np/",
        "https://moitfe.p2.gov.np/"
    ));
}

#[test]
fn same_site_rejects_invalid_input_gracefully() {
    assert!(!same_site("not a url", "also not a url"));
    assert!(!same_site("https://moha.gov.np/", "not a url"));
}

#[test]
fn registrable_domain_extraction() {
    assert_eq!(
        registrable_domain("https://moha.gov.np/").as_deref(),
        Some("moha.gov.np")
    );
    assert_eq!(
        registrable_domain("https://www.moha.gov.np/").as_deref(),
        Some("moha.gov.np")
    );
    // Naked host input also works.
    assert_eq!(
        registrable_domain("aaosatbise.moha.gov.np").as_deref(),
        Some("moha.gov.np")
    );
}
