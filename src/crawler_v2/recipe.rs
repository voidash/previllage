//! Per-source crawl recipe: sparse JSON overrides on a baked-in default.
//!
//! Recipes live at `recipes/<source_id>.json` (flat layout; see CRAWLER.md
//! for the rationale). Every field is optional — a recipe can specify only
//! the fields that need to deviate from the default policy. Missing recipes,
//! malformed JSON, and unknown fields all degrade to defaults with a
//! warning so the daemon never crashes on a human-edited recipe.
//!
//! The resolved [`Recipe`] is what workers consume. Build it with
//! [`load_recipe`], which overlays the file (if any) on the source-aware
//! default.
//!
//! ## Invariants
//!
//! 1. `source_id` on a loaded recipe always equals the Source's `source_id`.
//!    If the file disagrees, we log and override — the registry is authoritative.
//! 2. `entry_points` is non-empty after resolution. If the file omits it or
//!    sets it to `[]`, we fill with `[source.homepage_url]`.
//! 3. All numeric bounds are positive. A recipe that sets `max_depth: 0`
//!    gets clamped to 1 (a depth-0 crawl would fetch exactly the homepage;
//!    meaningful but usually not intended — clamp + log).

use super::types::Source;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RecipeError {
    #[error("io reading {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("malformed recipe at {path}: {source}")]
    Parse {
        path: String,
        #[source]
        source: serde_json::Error,
    },
}

/// The fully-resolved recipe a worker actually uses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recipe {
    pub source_id: String,
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub entry_points: Vec<String>,
    /// Path segments (exact, case-insensitive) to reject. Merged with the
    /// built-in [`crate::crawler_v2::url`] trap segments at enqueue time;
    /// the recipe list is additive, not replacement.
    #[serde(default)]
    pub deny_paths: Vec<String>,
    /// If set and non-empty, the frontier only enqueues URLs whose path
    /// contains at least one of these segments. Combined with the default
    /// URL filtering pipeline.
    #[serde(default)]
    pub allow_paths: Option<Vec<String>>,
    #[serde(default = "default_max_depth")]
    pub max_depth: u32,
    #[serde(default = "default_max_pdf_depth")]
    pub max_pdf_depth: u32,
    #[serde(default = "default_max_html_fetches")]
    pub max_html_fetches: u32,
    #[serde(default = "default_max_total_fetches")]
    pub max_total_fetches: u32,
    #[serde(default = "default_max_elapsed_sec")]
    pub max_elapsed_sec: u64,
    #[serde(default = "default_rate_limit_ms")]
    pub rate_limit_ms: u64,
    #[serde(default = "default_respect_robots")]
    pub respect_robots: bool,
    /// Subdomain labels to follow (in addition to the bare domain itself).
    /// `None` means follow any same-site subdomain via the public-suffix
    /// rule. `Some(vec![])` means follow only the bare domain, no subdomains.
    #[serde(default)]
    pub allowed_subdomains: Option<Vec<String>>,
    #[serde(default)]
    pub custom_user_agent: Option<String>,
    #[serde(default)]
    pub js_render_required: bool,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub last_repaired_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub repaired_by: Option<String>,
}

fn default_version() -> u32 {
    1
}
fn default_max_depth() -> u32 {
    2
}
fn default_max_pdf_depth() -> u32 {
    3
}
fn default_max_html_fetches() -> u32 {
    250
}
fn default_max_total_fetches() -> u32 {
    1500
}
fn default_max_elapsed_sec() -> u64 {
    1200
}
fn default_rate_limit_ms() -> u64 {
    1000
}
fn default_respect_robots() -> bool {
    true
}

impl Recipe {
    /// The baked-in default for `source`. Used when no recipe file exists.
    pub fn default_for(source: &Source) -> Self {
        Self {
            source_id: source.source_id.clone(),
            version: default_version(),
            entry_points: vec![source.homepage_url.clone()],
            deny_paths: Vec::new(),
            allow_paths: None,
            max_depth: default_max_depth(),
            max_pdf_depth: default_max_pdf_depth(),
            max_html_fetches: default_max_html_fetches(),
            max_total_fetches: default_max_total_fetches(),
            max_elapsed_sec: default_max_elapsed_sec(),
            rate_limit_ms: default_rate_limit_ms(),
            respect_robots: default_respect_robots(),
            allowed_subdomains: None,
            custom_user_agent: None,
            js_render_required: false,
            notes: String::new(),
            last_repaired_at: None,
            repaired_by: None,
        }
    }

    /// Normalize a recipe post-load: enforce invariants, clamp out-of-range
    /// values, and back-fill entry_points if the file omitted them.
    pub fn normalize(&mut self, source: &Source) {
        // Invariant 1: source_id matches.
        if self.source_id != source.source_id {
            eprintln!(
                "recipe: source_id '{}' does not match expected '{}'; overriding",
                self.source_id, source.source_id
            );
            self.source_id = source.source_id.clone();
        }
        // Invariant 2: entry_points non-empty.
        if self.entry_points.is_empty() {
            self.entry_points.push(source.homepage_url.clone());
        }
        // Invariant 3: numeric bounds positive (clamp).
        if self.max_depth == 0 {
            eprintln!(
                "recipe {}: max_depth=0 clamped to 1",
                source.source_id
            );
            self.max_depth = 1;
        }
        if self.max_pdf_depth == 0 {
            self.max_pdf_depth = 1;
        }
        if self.max_html_fetches == 0 {
            self.max_html_fetches = 1;
        }
        if self.max_total_fetches == 0 {
            self.max_total_fetches = 1;
        }
        if self.max_elapsed_sec == 0 {
            self.max_elapsed_sec = 60;
        }
        if self.rate_limit_ms == 0 {
            self.rate_limit_ms = 250; // still polite, just faster
        }
    }
}

/// Load the resolved recipe for a source. Reads `recipes/<source_id>.json`
/// if it exists; overlays on the default; normalizes; returns.
///
/// Missing file is expected (most sources use defaults) and produces no
/// warning. Malformed file produces a warning on stderr and returns the
/// default.
pub fn load_recipe(source: &Source, recipes_dir: &Path) -> Recipe {
    let path = recipes_dir.join(format!("{}.json", source.source_id));
    let raw = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Recipe::default_for(source);
        }
        Err(e) => {
            eprintln!(
                "recipe: failed to read {}: {}; using defaults",
                path.display(),
                e
            );
            return Recipe::default_for(source);
        }
    };

    match serde_json::from_str::<Recipe>(&raw) {
        Ok(mut r) => {
            r.normalize(source);
            r
        }
        Err(e) => {
            eprintln!(
                "recipe: malformed {}: {}; using defaults",
                path.display(),
                e
            );
            Recipe::default_for(source)
        }
    }
}

/// Explicit-error variant for tooling (CLI validation, tests). The daemon
/// itself uses [`load_recipe`] which swallows errors into warnings.
pub fn load_recipe_strict(source: &Source, recipes_dir: &Path) -> Result<Recipe, RecipeError> {
    let path = recipes_dir.join(format!("{}.json", source.source_id));
    let raw = std::fs::read_to_string(&path).map_err(|e| RecipeError::Io {
        path: path.display().to_string(),
        source: e,
    })?;
    let mut r: Recipe = serde_json::from_str(&raw).map_err(|e| RecipeError::Parse {
        path: path.display().to_string(),
        source: e,
    })?;
    r.normalize(source);
    Ok(r)
}
