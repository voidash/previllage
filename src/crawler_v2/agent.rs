//! Agent runtime — abstraction over CLI/SDK back-ends that propose a
//! crawler recipe given a failure context.
//!
//! ## Why a trait
//!
//! The repair dispatcher (Phase 6.4) needs to invoke "an agent that can drive
//! a browser, read the recipe schema, and emit a JSON proposal." That
//! capability ships in several real systems — claude-code, opencode, codex,
//! a direct HTTP call to Anthropic with a Playwright MCP — and which one
//! the operator has installed depends on their setup. We abstract those
//! behind a single trait so the dispatcher doesn't care.
//!
//! ## Adapter set
//!
//! Phase 6.3 (this commit) ships:
//!   - [`ClaudeCodeAgent`] — spawns `claude --print --output-format json -p ...`
//!
//! Subsequent commits add `OpenCodeAgent`, `CodexAgent`, and a direct-HTTP
//! `AnthropicAgent` (no subprocess) for low-latency cases. All implement
//! [`AgentRuntime`] and are wire-compatible at the call site.
//!
//! ## Output contract
//!
//! Agents must emit their final answer as JSON wrapped in
//! `<recipe>{...}</recipe>`. The parser tolerates surrounding prose (the
//! agent often narrates) but rejects missing/unbalanced tags or non-JSON
//! contents. See [`parse_recipe_from_output`].

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use tokio::io::AsyncWriteExt;

/// Inputs the dispatcher hands to an agent for one repair attempt.
///
/// `current_recipe_json` is what the agent should think of as "the failing
/// recipe"; for sources that have no file under `recipes/`, pass the
/// JSON-serialized default `Recipe::default_for(source)`.
#[derive(Debug, Clone)]
pub struct AgentContext {
    pub source_id: String,
    pub source_url: String,
    pub current_recipe_json: String,
    pub failure_evidence: String,
    pub sample_html_path: Option<PathBuf>,
    pub recipe_schema: String,
    pub example_recipes: Vec<ExampleRecipe>,
    pub timeout: Duration,
}

#[derive(Debug, Clone)]
pub struct ExampleRecipe {
    pub source_id: String,
    pub json: String,
}

/// Successful agent output.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentProposal {
    /// JSON of the proposed recipe — *not* yet validated against the schema.
    /// Validation lives in the dispatcher (Phase 6.4) so the same checker is
    /// used regardless of which agent generated it.
    pub proposed_recipe_json: String,
    /// Raw stdout for audit / debugging. Capped at ~1 MB by the runtime to
    /// avoid logs ballooning when an agent narrates excessively.
    pub agent_log: Option<String>,
    pub elapsed_sec: u64,
}

#[derive(Debug, Error)]
pub enum AgentError {
    #[error("agent timed out after {0:?}")]
    Timeout(Duration),
    #[error("agent process failed (exit {0:?}): {1}")]
    ProcessFailed(Option<i32>, String),
    #[error("agent output missing <recipe> tags: {hint}")]
    MissingRecipeTags { hint: String },
    #[error("agent recipe is not valid JSON: {0}")]
    InvalidJson(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[async_trait]
pub trait AgentRuntime: Send + Sync {
    async fn propose_recipe(
        &self,
        ctx: &AgentContext,
    ) -> Result<AgentProposal, AgentError>;
}

// ----- Prompt assembly -------------------------------------------------------

/// Build the prompt the agent sees. Same content regardless of adapter so
/// behavior is consistent across runtimes; each adapter wraps with its own
/// system-prompt / role plumbing.
pub fn build_prompt(ctx: &AgentContext) -> String {
    let mut s = String::with_capacity(8192);
    s.push_str(
        "You are a crawler-recipe repair agent for a Nepal government \
         knowledge-base project. A site we previously crawled is no longer \
         producing new documents. Investigate why, then propose an updated \
         recipe.\n\n",
    );

    s.push_str("## Source\n");
    s.push_str(&format!("- source_id: `{}`\n", ctx.source_id));
    s.push_str(&format!("- url: {}\n\n", ctx.source_url));

    s.push_str("## Failure evidence (from the health evaluator)\n```json\n");
    s.push_str(&ctx.failure_evidence);
    s.push_str("\n```\n\n");

    s.push_str("## Current recipe\n```json\n");
    s.push_str(&ctx.current_recipe_json);
    s.push_str("\n```\n\n");

    s.push_str("## Recipe schema (Rust)\n```rust\n");
    s.push_str(&ctx.recipe_schema);
    s.push_str("\n```\n\n");

    if !ctx.example_recipes.is_empty() {
        s.push_str("## Example working recipes for similar sources\n\n");
        for ex in &ctx.example_recipes {
            s.push_str(&format!("### {}\n```json\n{}\n```\n\n", ex.source_id, ex.json));
        }
    }

    if let Some(p) = &ctx.sample_html_path {
        s.push_str(&format!(
            "## Sample HTML from most recent successful crawl\n\
             Path on disk (read-only): `{}`\n\n",
            p.display()
        ));
    }

    s.push_str(
        "## What to do\n\n\
         1. Investigate. Use the Playwright tool to render the live URL and \
            inspect the rendered DOM if needed. If the failure is a JS-shell \
            (the page renders content only after JS), set `js_render_required: \
            true` in the proposed recipe.\n\
         2. Propose a recipe that fixes the failure. Keep it as **sparse** as \
            possible — only include fields that deviate from defaults.\n\
         3. Output exactly one `<recipe>...</recipe>` block at the end of \
            your reply. The contents must be valid JSON conforming to the \
            schema above. No prose inside the tags.\n",
    );
    s
}

// ----- Output parsing --------------------------------------------------------

/// Extract the JSON inside the agent's `<recipe>...</recipe>` tags and
/// validate it parses as JSON. Recipe-schema validation happens later, in
/// the dispatcher.
pub fn parse_recipe_from_output(raw: &str) -> Result<String, AgentError> {
    let open = raw.find("<recipe>").ok_or_else(|| AgentError::MissingRecipeTags {
        hint: "no <recipe> opening tag in output".into(),
    })?;
    let after_open = open + "<recipe>".len();
    let close_relative = raw[after_open..]
        .find("</recipe>")
        .ok_or_else(|| AgentError::MissingRecipeTags {
            hint: "no </recipe> closing tag after the opening tag".into(),
        })?;
    let close = after_open + close_relative;
    let inner = raw[after_open..close].trim();

    // Validate it's parseable as JSON. Don't normalize — preserve the agent's
    // formatting so the operator can review verbatim.
    if let Err(e) = serde_json::from_str::<serde_json::Value>(inner) {
        return Err(AgentError::InvalidJson(format!("{e}")));
    }
    Ok(inner.to_string())
}

// ----- ClaudeCodeAgent -------------------------------------------------------

#[derive(Debug, Clone)]
pub struct ClaudeCodeConfig {
    /// Command name; `claude` on a stock install.
    pub command: String,
    /// Extra args appended after the prompt arg. Useful for
    /// `--allowed-tools` lists or `--permission-mode` flags. Empty by default.
    pub extra_args: Vec<String>,
}

impl Default for ClaudeCodeConfig {
    fn default() -> Self {
        Self {
            command: "claude".to_string(),
            extra_args: Vec::new(),
        }
    }
}

pub struct ClaudeCodeAgent {
    config: ClaudeCodeConfig,
}

impl ClaudeCodeAgent {
    pub fn new(config: ClaudeCodeConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl AgentRuntime for ClaudeCodeAgent {
    async fn propose_recipe(
        &self,
        ctx: &AgentContext,
    ) -> Result<AgentProposal, AgentError> {
        let prompt = build_prompt(ctx);
        let started = std::time::Instant::now();

        // We pipe the prompt over stdin rather than passing it as an argv
        // entry — gov-site failure evidence can include URLs, quotes, etc.
        // that would exhaust shell-quoting safety.
        let mut cmd = tokio::process::Command::new(&self.config.command);
        cmd.arg("--print")
            .arg("--output-format")
            .arg("text")
            .args(&self.config.extra_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd.spawn().map_err(|e| {
            AgentError::ProcessFailed(None, format!("spawn {}: {e}", self.config.command))
        })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(prompt.as_bytes()).await?;
            stdin.shutdown().await?;
        }

        let output = match tokio::time::timeout(ctx.timeout, child.wait_with_output()).await {
            Ok(o) => o?,
            Err(_) => return Err(AgentError::Timeout(ctx.timeout)),
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            return Err(AgentError::ProcessFailed(output.status.code(), stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let recipe_json = parse_recipe_from_output(&stdout)?;

        // Cap the log at 1 MB so a verbose agent doesn't blow up SQLite.
        let cap = 1_000_000usize;
        let agent_log = if stdout.len() > cap {
            Some(format!("{}...[truncated {} bytes]", &stdout[..cap], stdout.len() - cap))
        } else {
            Some(stdout)
        };

        Ok(AgentProposal {
            proposed_recipe_json: recipe_json,
            agent_log,
            elapsed_sec: started.elapsed().as_secs(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> AgentContext {
        AgentContext {
            source_id: "jirimun_gov_np".into(),
            source_url: "https://jirimun.gov.np/".into(),
            current_recipe_json: "{}".into(),
            failure_evidence: "{\"verdict\":\"structurally_failed\"}".into(),
            sample_html_path: Some(PathBuf::from("/tmp/sample.html")),
            recipe_schema: "pub struct Recipe { ... }".into(),
            example_recipes: vec![ExampleRecipe {
                source_id: "moha_gov_np".into(),
                json: "{}".into(),
            }],
            timeout: Duration::from_secs(60),
        }
    }

    #[test]
    fn parse_extracts_json_from_recipe_tags() {
        let raw = r#"I looked at the site and propose:
<recipe>{"source_id":"x","js_render_required":true}</recipe>
end"#;
        let got = parse_recipe_from_output(raw).unwrap();
        assert_eq!(got, r#"{"source_id":"x","js_render_required":true}"#);
    }

    #[test]
    fn parse_tolerates_surrounding_prose_and_whitespace() {
        let raw = "Final answer:\n\n<recipe>\n  {\n  \"source_id\": \"x\"\n  }\n</recipe>\n\nDone.";
        let got = parse_recipe_from_output(raw).unwrap();
        assert!(got.contains("source_id"));
    }

    #[test]
    fn parse_rejects_missing_tags() {
        let raw = "Here is the recipe: { \"source_id\": \"x\" }";
        let r = parse_recipe_from_output(raw);
        assert!(matches!(r, Err(AgentError::MissingRecipeTags { .. })));
    }

    #[test]
    fn parse_rejects_unbalanced_tags() {
        let raw = "<recipe>{ unclosed";
        let r = parse_recipe_from_output(raw);
        assert!(matches!(r, Err(AgentError::MissingRecipeTags { .. })));
    }

    #[test]
    fn parse_rejects_non_json_inside_tags() {
        let raw = "<recipe>not json at all</recipe>";
        let r = parse_recipe_from_output(raw);
        assert!(matches!(r, Err(AgentError::InvalidJson(_))));
    }

    #[test]
    fn parse_takes_first_recipe_block() {
        // If the agent emits multiple, we take the first. Operator can review
        // the agent_log to see all of them; defensive choice — most likely
        // the agent shows iterations and lands on the final.
        let raw = "<recipe>{\"source_id\":\"first\"}</recipe> later <recipe>{\"source_id\":\"second\"}</recipe>";
        let got = parse_recipe_from_output(raw).unwrap();
        assert!(got.contains("first"));
    }

    #[test]
    fn prompt_includes_all_required_sections() {
        let p = build_prompt(&ctx());
        assert!(p.contains("source_id: `jirimun_gov_np`"));
        assert!(p.contains("https://jirimun.gov.np/"));
        assert!(p.contains("Failure evidence"));
        assert!(p.contains("Current recipe"));
        assert!(p.contains("Recipe schema"));
        assert!(p.contains("moha_gov_np"));
        assert!(p.contains("/tmp/sample.html"));
        assert!(p.contains("<recipe>"));
        assert!(p.contains("</recipe>"));
    }

    #[test]
    fn prompt_omits_sample_path_when_none() {
        let mut c = ctx();
        c.sample_html_path = None;
        let p = build_prompt(&c);
        assert!(!p.contains("Sample HTML"));
        // But everything else must still be there.
        assert!(p.contains("Recipe schema"));
    }

    #[test]
    fn prompt_omits_examples_section_when_empty() {
        let mut c = ctx();
        c.example_recipes = Vec::new();
        let p = build_prompt(&c);
        assert!(!p.contains("Example working recipes"));
    }
}
