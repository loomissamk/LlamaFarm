//! Vertical-slice integration test for autonomous operator flow.
//!
//! Covers: objective -> RAG retrieval -> tool chain -> autonomous completion ->
//! per-run trace file emission.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::json;
use std::sync::{Arc, Mutex};

use llamafarm::agent::AutonomousLoop;
use llamafarm::config::{AgentExecutionMode, MultimodalConfig};
use llamafarm::observability::{runtime_trace, NoopObserver};
use llamafarm::providers::{ChatMessage, ChatRequest, ChatResponse, Provider, ToolCall};
use llamafarm::rag::DocRag;
use llamafarm::tools::{Tool, ToolResult};

struct ScriptedProvider {
    responses: Mutex<Vec<ChatResponse>>,
}

impl ScriptedProvider {
    fn new(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    async fn chat_with_system(
        &self,
        _system_prompt: Option<&str>,
        _message: &str,
        _model: &str,
        _temperature: f64,
    ) -> Result<String> {
        Ok("fallback".to_string())
    }

    async fn chat(
        &self,
        _request: ChatRequest<'_>,
        _model: &str,
        _temperature: f64,
    ) -> Result<ChatResponse> {
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Ok(ChatResponse {
                text: Some("completed successfully".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                metrics: None,
                reasoning_content: None,
            });
        }
        Ok(guard.remove(0))
    }
}

#[derive(Default)]
struct RagState {
    last_context: Mutex<String>,
}

struct RagLookupTool {
    rag: Arc<DocRag>,
    state: Arc<RagState>,
}

#[async_trait]
impl Tool for RagLookupTool {
    fn name(&self) -> &str {
        "rag_lookup"
    }

    fn description(&self) -> &str {
        "Retrieve local documentation snippets with citations"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string" }
            },
            "required": ["query"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let mut results = self.rag.retrieve_hybrid(query, None, 3);
        if results.is_empty() {
            results = self.rag.retrieve_bm25("gateway", 3);
        }
        if results.is_empty() {
            results = self.rag.retrieve_bm25("runtime", 3);
        }
        if results.is_empty() {
            *self.state.last_context.lock().unwrap() = format!(
                "NO_HITS query={query:?} args={args}",
            );
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("no retrieval hits".to_string()),
            });
        }

        let context = DocRag::build_context(&results, 2_000);
        let citations = DocRag::build_citation_list(&results);
        let output = format!("{context}\n{citations}");
        *self.state.last_context.lock().unwrap() = output.clone();

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

struct VerifyRagTool {
    state: Arc<RagState>,
}

#[async_trait]
impl Tool for VerifyRagTool {
    fn name(&self) -> &str {
        "verify_rag"
    }

    fn description(&self) -> &str {
        "Verify retrieved context includes an expected string"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "expected": { "type": "string" }
            },
            "required": ["expected"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let expected = args
            .get("expected")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        let last = self.state.last_context.lock().unwrap().clone();
        let contains = last.contains(expected);
        Ok(ToolResult {
            success: contains,
            output: format!("verify expected='{expected}' => {contains}"),
            error: if contains {
                None
            } else {
                Some(format!("expected value not found: {expected}"))
            },
        })
    }
}

#[tokio::test]
async fn autonomous_vertical_slice_writes_run_trace_after_rag_tool_chain() {
    let mut rag = DocRag::new();
    rag.ingest_text(
        "docs/runtime.md",
        "# Runtime\n\nGateway default local URL is http://127.0.0.1:42617.\n",
    );
    rag.ingest_text(
        "docs/ops.md",
        "# Ops\n\nUse `llamafarm trace replay --latest` to inspect recent autonomous runs.\n",
    );
    let rag = Arc::new(rag);
    let state = Arc::new(RagState::default());

    let provider = ScriptedProvider::new(vec![
        ChatResponse {
            text: Some(String::new()),
            tool_calls: vec![ToolCall {
                id: "tc-rag".to_string(),
                name: "rag_lookup".to_string(),
                arguments: r#"{"query":"gateway default local URL 42617"}"#.to_string(),
            }],
            usage: None,
            metrics: None,
            reasoning_content: None,
        },
        ChatResponse {
            text: Some(String::new()),
            tool_calls: vec![ToolCall {
                id: "tc-verify".to_string(),
                name: "verify_rag".to_string(),
                arguments: r#"{"expected":"42617"}"#.to_string(),
            }],
            usage: None,
            metrics: None,
            reasoning_content: None,
        },
        ChatResponse {
            text: Some(
                "Completed successfully: gateway port 42617 retrieved and verified.".to_string(),
            ),
            tool_calls: Vec::new(),
            usage: None,
            metrics: None,
            reasoning_content: None,
        },
    ]);

    let tools: Vec<Box<dyn Tool>> = vec![
        Box::new(RagLookupTool {
            rag: Arc::clone(&rag),
            state: Arc::clone(&state),
        }),
        Box::new(VerifyRagTool {
            state: Arc::clone(&state),
        }),
    ];
    let observer = NoopObserver {};
    let multimodal = MultimodalConfig::default();
    let excluded_tools: Vec<String> = Vec::new();
    let workspace = tempfile::tempdir().expect("temp workspace should be created");

    let loop_driver = AutonomousLoop::new(
        AgentExecutionMode::AutonomousOperator,
        2,
        0,
        &provider,
        &tools,
        &observer,
        "ollama",
        "qwen3.5:9b",
        0.0,
        "repo_workflow",
        &multimodal,
        6,
        None,
        &excluded_tools,
    )
    .with_workspace(workspace.path());

    let run_id = loop_driver.run_id().to_string();
    let mut history = vec![
        ChatMessage::system("You are an autonomous local operator."),
        ChatMessage::user("Find the default gateway port from docs."),
    ];

    let outcome = loop_driver
        .run(&mut history)
        .await
        .expect("autonomous loop should succeed");
    assert!(outcome.is_success(), "expected successful completion, got {outcome:?}");

    let history_dump = history
        .iter()
        .map(|m| format!("{}:{}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        history_dump.contains("rag_lookup"),
        "history should contain rag_lookup tool call; got: {history_dump}"
    );
    assert!(
        history_dump.contains("verify_rag"),
        "history should contain verify_rag tool call; got: {history_dump}"
    );

    let trace_path = workspace
        .path()
        .join("state")
        .join("runs")
        .join(format!("{run_id}.jsonl"));
    assert!(
        trace_path.exists(),
        "autonomous run should write a per-run trace file"
    );

    let replay = runtime_trace::format_run_trace(&trace_path)
        .expect("trace replay formatting should succeed");
    assert!(replay.contains("run_start"), "trace replay should include run_start");
    assert!(
        replay.contains("run_summary"),
        "trace replay should include run_summary"
    );
}
