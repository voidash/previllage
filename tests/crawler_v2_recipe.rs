//! Tests for crawler_v2::recipe — load + merge + normalize semantics.

use chrono::Utc;
use gemma_god::crawler_v2::recipe::{
    load_recipe, load_recipe_strict, Recipe, RecipeError,
};
use gemma_god::crawler_v2::types::{Source, SourceStatus, Tier};
use std::fs;
use tempfile::TempDir;

fn sample_source(id: &str) -> Source {
    Source {
        source_id: id.to_string(),
        domain: format!("{id}.gov.np"),
        homepage_url: format!("https://{id}.gov.np/"),
        name_en: Some(format!("Ministry of {id}")),
        name_np: None,
        office_type: Some("Federal".into()),
        province: None,
        tier: Tier(2),
        poll_interval_hours: 12,
        status: SourceStatus::Active,
        first_seen: Utc::now(),
        last_polled_at: None,
        last_changed_at: None,
        last_failure_at: None,
        consecutive_failures: 0,
        next_poll_at: None,
        notes: None,
    }
}

#[test]
fn missing_recipe_file_yields_default_with_source_entry_point() {
    let dir = TempDir::new().unwrap();
    let src = sample_source("moha");
    let r = load_recipe(&src, dir.path());
    assert_eq!(r.source_id, "moha");
    assert_eq!(r.entry_points, vec!["https://moha.gov.np/".to_string()]);
    assert_eq!(r.max_depth, 2);
    assert_eq!(r.max_pdf_depth, 3);
    assert_eq!(r.max_html_fetches, 250);
    assert_eq!(r.max_total_fetches, 1500);
    assert_eq!(r.max_elapsed_sec, 1200);
    assert_eq!(r.rate_limit_ms, 1000);
    assert!(r.respect_robots);
    assert!(!r.js_render_required);
    assert!(r.deny_paths.is_empty());
    assert!(r.allow_paths.is_none());
    assert!(r.custom_user_agent.is_none());
}

#[test]
fn partial_recipe_merges_onto_defaults() {
    let dir = TempDir::new().unwrap();
    let src = sample_source("moha");
    // Only overrides max_html_fetches; everything else must stay default.
    fs::write(
        dir.path().join("moha.json"),
        r#"{"source_id": "moha", "max_html_fetches": 500}"#,
    )
    .unwrap();

    let r = load_recipe(&src, dir.path());
    assert_eq!(r.max_html_fetches, 500);
    assert_eq!(r.max_depth, 2); // still default
    assert_eq!(r.entry_points, vec!["https://moha.gov.np/".to_string()]); // backfilled
}

#[test]
fn entry_points_override_replaces_homepage_fallback() {
    let dir = TempDir::new().unwrap();
    let src = sample_source("moha");
    fs::write(
        dir.path().join("moha.json"),
        r#"{
            "source_id": "moha",
            "entry_points": [
                "https://moha.gov.np/en/notices",
                "https://moha.gov.np/en/circulars"
            ]
        }"#,
    )
    .unwrap();

    let r = load_recipe(&src, dir.path());
    assert_eq!(r.entry_points.len(), 2);
    assert!(r.entry_points[0].contains("/notices"));
    assert!(r.entry_points[1].contains("/circulars"));
}

#[test]
fn empty_entry_points_fall_back_to_source_homepage() {
    let dir = TempDir::new().unwrap();
    let src = sample_source("moha");
    fs::write(
        dir.path().join("moha.json"),
        r#"{"source_id": "moha", "entry_points": []}"#,
    )
    .unwrap();

    let r = load_recipe(&src, dir.path());
    assert_eq!(r.entry_points, vec!["https://moha.gov.np/".to_string()]);
}

#[test]
fn malformed_json_falls_back_to_default_without_panic() {
    let dir = TempDir::new().unwrap();
    let src = sample_source("moha");
    fs::write(dir.path().join("moha.json"), "{this is not json").unwrap();

    // The lenient loader must not panic and must return defaults.
    let r = load_recipe(&src, dir.path());
    assert_eq!(r.source_id, "moha");
    assert_eq!(r.max_depth, 2);

    // The strict loader surfaces the parse error with a path breadcrumb.
    let err = load_recipe_strict(&src, dir.path()).unwrap_err();
    assert!(matches!(err, RecipeError::Parse { .. }));
}

#[test]
fn strict_loader_io_error_on_missing_file() {
    let dir = TempDir::new().unwrap();
    let src = sample_source("unknown");
    let err = load_recipe_strict(&src, dir.path()).unwrap_err();
    assert!(matches!(err, RecipeError::Io { .. }));
}

#[test]
fn mismatched_source_id_is_overridden_not_fatal() {
    let dir = TempDir::new().unwrap();
    let src = sample_source("moha");
    fs::write(
        dir.path().join("moha.json"),
        r#"{"source_id": "typo_source_id", "max_depth": 5}"#,
    )
    .unwrap();

    // Registry is authoritative; source_id gets corrected to match.
    let r = load_recipe(&src, dir.path());
    assert_eq!(r.source_id, "moha");
    assert_eq!(r.max_depth, 5); // other fields still apply
}

#[test]
fn zero_bounds_are_clamped_to_sane_minimums() {
    let dir = TempDir::new().unwrap();
    let src = sample_source("moha");
    fs::write(
        dir.path().join("moha.json"),
        r#"{
            "source_id": "moha",
            "max_depth": 0,
            "max_pdf_depth": 0,
            "max_html_fetches": 0,
            "max_total_fetches": 0,
            "max_elapsed_sec": 0,
            "rate_limit_ms": 0
        }"#,
    )
    .unwrap();

    let r = load_recipe(&src, dir.path());
    // Clamps: depth → 1, fetches → 1, elapsed → 60s, rate_limit → 250ms.
    // Values come from recipe::normalize; failure here flags a silent policy drift.
    assert_eq!(r.max_depth, 1);
    assert_eq!(r.max_pdf_depth, 1);
    assert_eq!(r.max_html_fetches, 1);
    assert_eq!(r.max_total_fetches, 1);
    assert_eq!(r.max_elapsed_sec, 60);
    assert_eq!(r.rate_limit_ms, 250);
}

#[test]
fn recipe_json_roundtrips() {
    // Ensure every field we store survives serialize → deserialize without
    // loss or schema drift. Catches accidental serde rename collisions.
    let src = sample_source("moha");
    let original = Recipe {
        source_id: src.source_id.clone(),
        version: 2,
        entry_points: vec!["https://moha.gov.np/en".into()],
        deny_paths: vec!["gallery".into(), "team".into()],
        allow_paths: Some(vec!["notices".into()]),
        max_depth: 3,
        max_pdf_depth: 4,
        max_html_fetches: 500,
        max_total_fetches: 2000,
        max_elapsed_sec: 1800,
        rate_limit_ms: 500,
        respect_robots: false,
        allowed_subdomains: Some(vec!["www".into(), "en".into()]),
        custom_user_agent: Some("custom-ua/1.0".into()),
        js_render_required: true,
        notes: "agent-repaired 2026-04-20".into(),
        last_repaired_at: Some(Utc::now()),
        repaired_by: Some("claude-code".into()),
    };
    let text = serde_json::to_string(&original).unwrap();
    let back: Recipe = serde_json::from_str(&text).unwrap();
    // Spot-check a few fields explicitly for clearer test failure output.
    assert_eq!(back.source_id, original.source_id);
    assert_eq!(back.version, 2);
    assert_eq!(back.deny_paths, original.deny_paths);
    assert_eq!(back.allow_paths, original.allow_paths);
    assert_eq!(back.js_render_required, true);
    assert_eq!(back.repaired_by.as_deref(), Some("claude-code"));
}

#[test]
fn unknown_fields_in_recipe_json_are_ignored() {
    // Forward-compat: a recipe file authored against a newer schema must
    // still load (with unknown fields dropped) rather than erroring.
    let dir = TempDir::new().unwrap();
    let src = sample_source("moha");
    fs::write(
        dir.path().join("moha.json"),
        r#"{
            "source_id": "moha",
            "max_depth": 3,
            "future_field_we_dont_know_about": "should be ignored"
        }"#,
    )
    .unwrap();

    let r = load_recipe(&src, dir.path());
    assert_eq!(r.max_depth, 3);
}

#[test]
fn js_render_required_override_is_preserved() {
    // This is how agent-repair flips a source to the Chromium path.
    let dir = TempDir::new().unwrap();
    let src = sample_source("oag");
    fs::write(
        dir.path().join("oag.json"),
        r#"{"source_id": "oag", "js_render_required": true}"#,
    )
    .unwrap();

    let r = load_recipe(&src, dir.path());
    assert!(r.js_render_required);
}
