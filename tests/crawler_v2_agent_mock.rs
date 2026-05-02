//! Trait-surface verification for `AgentRuntime`. We define a mock impl
//! here (parallel to ClaudeCodeAgent) and exercise it through the same
//! call site the dispatcher will use, so the trait is provably wide enough
//! to hold real adapters AND test doubles.

use async_trait::async_trait;
use gemma_god::crawler_v2::{
    AgentContext, AgentError, AgentProposal, AgentRuntime, ExampleRecipe,
};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// In-memory adapter — returns a canned proposal, recording calls. Useful
/// for dispatcher tests in Phase 6.4 where we don't want a real subprocess.
struct CannedAgent {
    response: String,
    calls: Mutex<Vec<String>>,
}

impl CannedAgent {
    fn new(response: impl Into<String>) -> Self {
        Self {
            response: response.into(),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[async_trait]
impl AgentRuntime for CannedAgent {
    async fn propose_recipe(&self, ctx: &AgentContext) -> Result<AgentProposal, AgentError> {
        self.calls.lock().unwrap().push(ctx.source_id.clone());
        Ok(AgentProposal {
            proposed_recipe_json: self.response.clone(),
            agent_log: Some(format!("mock for {}", ctx.source_id)),
            elapsed_sec: 0,
        })
    }
}

fn ctx_for(sid: &str) -> AgentContext {
    AgentContext {
        source_id: sid.into(),
        source_url: format!("https://{sid}.gov.np/"),
        current_recipe_json: "{}".into(),
        failure_evidence: r#"{"verdict":"structurally_failed"}"#.into(),
        sample_html_path: Some(PathBuf::from("/tmp/sample.html")),
        recipe_schema: "...".into(),
        example_recipes: vec![ExampleRecipe {
            source_id: "moha_gov_np".into(),
            json: "{}".into(),
        }],
        timeout: Duration::from_secs(10),
    }
}

#[tokio::test]
async fn mock_adapter_round_trips_via_trait_object() {
    let agent: Box<dyn AgentRuntime> =
        Box::new(CannedAgent::new(r#"{"source_id":"jirimun_gov_np"}"#));
    let proposal = agent.propose_recipe(&ctx_for("jirimun_gov_np")).await.unwrap();
    assert!(proposal.proposed_recipe_json.contains("jirimun_gov_np"));
    assert!(proposal.agent_log.unwrap().contains("mock"));
}

#[tokio::test]
async fn mock_adapter_can_track_invocations() {
    let agent = CannedAgent::new(r#"{"source_id":"x"}"#);
    agent.propose_recipe(&ctx_for("a")).await.unwrap();
    agent.propose_recipe(&ctx_for("b")).await.unwrap();
    assert_eq!(agent.call_count(), 2);
}

/// Adapter that always fails — used in Phase 6.4 to test deadletter paths.
struct FailingAgent {
    err: String,
}

#[async_trait]
impl AgentRuntime for FailingAgent {
    async fn propose_recipe(&self, _ctx: &AgentContext) -> Result<AgentProposal, AgentError> {
        Err(AgentError::ProcessFailed(Some(1), self.err.clone()))
    }
}

#[tokio::test]
async fn failing_adapter_surfaces_process_failed_error() {
    let agent: Box<dyn AgentRuntime> = Box::new(FailingAgent {
        err: "auth not configured".into(),
    });
    let r = agent.propose_recipe(&ctx_for("x")).await;
    match r {
        Err(AgentError::ProcessFailed(Some(1), msg)) => {
            assert!(msg.contains("auth"));
        }
        other => panic!("expected ProcessFailed(Some(1), ...), got {other:?}"),
    }
}
