//! Research phase — proactive information gathering before main response.
//!
//! When enabled, the agent runs a focused "research turn" using available tools
//! to gather context before generating its main response. This creates a
//! "thinking" phase where the agent explores the codebase, searches memory,
//! or fetches external data.
//!
//! Supports both:
//! - Native tool calling (OpenAI, Anthropic, Bedrock, etc.)
//! - Prompt-guided tool calling (Gemini and other providers without native support)

use crate::agent::dispatcher::{ToolDispatcher, XmlToolDispatcher};
use crate::config::{ResearchPhaseConfig, ResearchTrigger};
use crate::observability::Observer;
use crate::providers::traits::build_tool_instructions_text;
use crate::providers::{ChatMessage, ChatRequest, ChatResponse, Provider, ToolCall};
use crate::tools::{Tool, ToolResult, ToolSpec};
use anyhow::Result;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Result of the research phase.
#[derive(Debug, Clone)]
pub struct ResearchResult {
    /// Collected context from research (formatted for injection into main prompt).
    pub context: String,
    /// Number of tool calls made during research.
    pub tool_call_count: usize,
    /// Duration of the research phase.
    pub duration: Duration,
    /// Summary of tools called and their results.
    pub tool_summaries: Vec<ToolSummary>,
}

/// Summary of a single tool call during research.
#[derive(Debug, Clone)]
pub struct ToolSummary {
    pub tool_name: String,
    pub arguments_preview: String,
    pub result_preview: String,
    pub success: bool,
}

/// Check if research phase should be triggered for this message.
pub fn should_trigger(config: &ResearchPhaseConfig, message: &str) -> bool {
    if !config.enabled {
        return false;
    }

    match config.trigger {
        ResearchTrigger::Never => false,
        ResearchTrigger::Always => true,
        ResearchTrigger::Keywords => {
            let message_lower = message.to_lowercase();
            config
                .keywords
                .iter()
                .any(|kw| message_lower.contains(&kw.to_lowercase()))
        }
        ResearchTrigger::Length => message.len() >= config.min_message_length,
        ResearchTrigger::Question => message.contains('?'),
    }
}

/// Default system prompt for research phase.
const RESEARCH_SYSTEM_PROMPT: &str = r#"You are in RESEARCH MODE. Your task is to gather information that will help answer the user's question.

RULES:
1. Use tools to search, read files, check status, or fetch data
2. Focus on gathering FACTS, not answering yet
3. Be efficient — only gather what's needed
4. After gathering enough info, respond with a summary starting with "[RESEARCH COMPLETE]"

DO NOT:
- Answer the user's question directly
- Make changes to files
- Execute destructive commands

When you have enough information, summarize what you found in this format:
[RESEARCH COMPLETE]
- Finding 1: ...
- Finding 2: ...
- Finding 3: ...
"#;

/// Stop only repeated semantic non-progress, not productive research. Three
/// consecutive iterations with the same tool calls and byte-identical results
/// indicate that the model is stuck rather than gathering new evidence.
const IDENTICAL_RESEARCH_ITERATION_STALL_THRESHOLD: usize = 3;

/// Run the research phase.
///
/// This executes a focused LLM + tools loop to gather information before
/// the main response. The collected context is returned for injection
/// into the main conversation.
pub async fn run_research_phase(
    config: &ResearchPhaseConfig,
    provider: &dyn Provider,
    tools: &[Box<dyn Tool>],
    user_message: &str,
    model: &str,
    temperature: f64,
    _observer: Arc<dyn Observer>,
) -> Result<ResearchResult> {
    let start = Instant::now();
    let mut tool_summaries = Vec::new();
    let mut collected_context = String::new();
    let mut iteration = 0usize;
    let mut previous_iteration_fingerprint: Option<
        Vec<(String, String, bool, String, Option<String>)>,
    > = None;
    let mut consecutive_identical_iterations = 0usize;

    let uses_native_tools = provider.supports_native_tools();

    // Build tool specs for native OR prompt-guided tool calling
    let tool_specs: Vec<ToolSpec> = tools
        .iter()
        .map(|t| ToolSpec {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
        })
        .collect();

    // Build system prompt
    // For prompt-guided providers, include tool instructions in system prompt
    let base_prompt = if config.system_prompt_prefix.is_empty() {
        RESEARCH_SYSTEM_PROMPT.to_string()
    } else {
        format!(
            "{}\n\n{}",
            config.system_prompt_prefix, RESEARCH_SYSTEM_PROMPT
        )
    };

    let system_prompt = if uses_native_tools {
        base_prompt
    } else {
        // Prompt-guided: append tool instructions
        format!(
            "{}\n\n{}",
            base_prompt,
            build_tool_instructions_text(&tool_specs)
        )
    };

    // Conversation history for research phase
    let mut messages = vec![ChatMessage::user(format!(
        "Research the following question to gather relevant information:\n\n{}",
        user_message
    ))];

    // A zero max_iterations value is unlimited. Productive research continues
    // until the model finishes; positive values remain explicit operator caps.
    loop {
        if config.max_iterations > 0 && iteration >= config.max_iterations {
            break;
        }
        iteration = iteration.saturating_add(1);

        // Log research iteration if showing progress
        if config.show_progress {
            tracing::info!(iteration, "Research phase iteration");
        }

        // Build messages with system prompt as first message
        let mut full_messages = vec![ChatMessage::system(&system_prompt)];
        full_messages.extend(messages.iter().cloned());

        // Call LLM
        let request = ChatRequest {
            messages: &full_messages,
            tools: if uses_native_tools {
                Some(&tool_specs)
            } else {
                None // Prompt-guided: tools are in system prompt
            },
        };

        let response: ChatResponse = provider.chat(request, model, temperature).await?;

        // Check if research is complete
        if let Some(ref text) = response.text {
            if text.contains("[RESEARCH COMPLETE]") {
                // Extract the summary
                if let Some(idx) = text.find("[RESEARCH COMPLETE]") {
                    collected_context = text[idx..].to_string();
                }
                break;
            }
        }

        // Parse tool calls: native OR from XML in response text
        let tool_calls: Vec<ToolCall> = if uses_native_tools {
            response.tool_calls.clone()
        } else {
            // Parse XML <tool_call> tags from response text using XmlToolDispatcher
            let dispatcher = XmlToolDispatcher;
            let (_, parsed) = dispatcher.parse_response(&response);
            parsed
                .into_iter()
                .enumerate()
                .map(|(i, p)| ToolCall {
                    id: p
                        .tool_call_id
                        .unwrap_or_else(|| format!("tc_{}_{}", iteration, i)),
                    name: p.name,
                    arguments: serde_json::to_string(&p.arguments).unwrap_or_default(),
                })
                .collect()
        };

        // If no tool calls, we're done
        if tool_calls.is_empty() {
            if let Some(text) = response.text {
                collected_context = text;
            }
            break;
        }

        // Execute tool calls
        let mut iteration_fingerprint = Vec::with_capacity(tool_calls.len());
        for tool_call in &tool_calls {
            let tool_result = execute_tool_call(tools, tool_call).await;
            iteration_fingerprint.push((
                tool_call.name.clone(),
                tool_call.arguments.clone(),
                tool_result.success,
                tool_result.output.clone(),
                tool_result.error.clone(),
            ));

            let summary = ToolSummary {
                tool_name: tool_call.name.clone(),
                arguments_preview: truncate(&tool_call.arguments, 100),
                result_preview: truncate(&tool_result.output, 200),
                success: tool_result.success,
            };

            if config.show_progress {
                tracing::info!(
                    tool = %summary.tool_name,
                    success = summary.success,
                    "Research tool call"
                );
            }

            tool_summaries.push(summary);

            // Add tool result to conversation
            messages.push(ChatMessage::assistant(format!(
                "Called tool `{}` with arguments: {}",
                tool_call.name, tool_call.arguments
            )));
            messages.push(ChatMessage::user(format!(
                "Tool result:\n{}",
                tool_result.output
            )));
        }

        if previous_iteration_fingerprint.as_ref() == Some(&iteration_fingerprint) {
            consecutive_identical_iterations = consecutive_identical_iterations.saturating_add(1);
        } else {
            consecutive_identical_iterations = 1;
            previous_iteration_fingerprint = Some(iteration_fingerprint);
        }

        if consecutive_identical_iterations >= IDENTICAL_RESEARCH_ITERATION_STALL_THRESHOLD {
            anyhow::bail!(
                "Research phase stalled after {consecutive_identical_iterations} consecutive identical tool-call/result iterations"
            );
        }
    }

    let duration = start.elapsed();

    Ok(ResearchResult {
        context: collected_context,
        tool_call_count: tool_summaries.len(),
        duration,
        tool_summaries,
    })
}

/// Execute a single tool call.
async fn execute_tool_call(tools: &[Box<dyn Tool>], tool_call: &ToolCall) -> ToolResult {
    // Find the tool
    let tool = tools.iter().find(|t| t.name() == tool_call.name);

    match tool {
        Some(t) => {
            // Parse arguments
            let args: serde_json::Value = serde_json::from_str(&tool_call.arguments)
                .unwrap_or(serde_json::Value::Object(serde_json::Map::new()));

            // Execute
            match t.execute(args).await {
                Ok(result) => result,
                Err(e) => ToolResult {
                    success: false,
                    output: format!("Error: {}", e),
                    error: Some(e.to_string()),
                },
            }
        }
        None => ToolResult {
            success: false,
            output: format!("Unknown tool: {}", tool_call.name),
            error: Some(format!("Unknown tool: {}", tool_call.name)),
        },
    }
}

/// Truncate string with ellipsis.
fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observability::NoopObserver;
    use crate::providers::traits::ProviderCapabilities;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    struct ScriptedResearchProvider {
        responses: Mutex<VecDeque<ChatResponse>>,
        calls: Arc<AtomicUsize>,
    }

    impl ScriptedResearchProvider {
        fn new(responses: Vec<ChatResponse>, calls: Arc<AtomicUsize>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
                calls,
            }
        }
    }

    #[async_trait]
    impl Provider for ScriptedResearchProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: true,
                ..ProviderCapabilities::default()
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system is not used by the research phase")
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.responses
                .lock()
                .expect("scripted responses lock")
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted research provider exhausted responses"))
        }
    }

    struct EchoResearchTool {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Tool for EchoResearchTool {
        fn name(&self) -> &str {
            "research_echo"
        }

        fn description(&self) -> &str {
            "Returns the supplied value"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": {"type": "integer"}
                }
            })
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(ToolResult {
                success: true,
                output: args
                    .get("value")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null)
                    .to_string(),
                error: None,
            })
        }
    }

    fn tool_response(value: usize) -> ChatResponse {
        ChatResponse {
            text: None,
            tool_calls: vec![ToolCall {
                id: format!("research_call_{value}"),
                name: "research_echo".to_string(),
                arguments: serde_json::json!({"value": value}).to_string(),
            }],
            usage: None,
            metrics: None,
            reasoning_content: None,
        }
    }

    fn complete_response() -> ChatResponse {
        ChatResponse {
            text: Some("[RESEARCH COMPLETE]\n- verified".to_string()),
            tool_calls: Vec::new(),
            usage: None,
            metrics: None,
            reasoning_content: None,
        }
    }

    fn config_with_limit(max_iterations: usize) -> ResearchPhaseConfig {
        ResearchPhaseConfig {
            enabled: true,
            max_iterations,
            show_progress: false,
            ..ResearchPhaseConfig::default()
        }
    }

    #[test]
    fn should_trigger_never() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Never,
            ..Default::default()
        };
        assert!(!should_trigger(&config, "find something"));
    }

    #[test]
    fn should_trigger_always() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Always,
            ..Default::default()
        };
        assert!(should_trigger(&config, "hello"));
    }

    #[test]
    fn should_trigger_keywords() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Keywords,
            keywords: vec!["find".into(), "search".into()],
            ..Default::default()
        };
        assert!(should_trigger(&config, "please find the file"));
        assert!(should_trigger(&config, "SEARCH for errors"));
        assert!(!should_trigger(&config, "hello world"));
    }

    #[test]
    fn should_trigger_length() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Length,
            min_message_length: 20,
            ..Default::default()
        };
        assert!(!should_trigger(&config, "short"));
        assert!(should_trigger(
            &config,
            "this is a longer message that exceeds the minimum"
        ));
    }

    #[test]
    fn should_trigger_question() {
        let config = ResearchPhaseConfig {
            enabled: true,
            trigger: ResearchTrigger::Question,
            ..Default::default()
        };
        assert!(should_trigger(&config, "what is this?"));
        assert!(!should_trigger(&config, "do this now"));
    }

    #[test]
    fn disabled_never_triggers() {
        let config = ResearchPhaseConfig {
            enabled: false,
            trigger: ResearchTrigger::Always,
            ..Default::default()
        };
        assert!(!should_trigger(&config, "anything"));
    }

    #[test]
    fn research_default_is_unlimited() {
        assert_eq!(ResearchPhaseConfig::default().max_iterations, 0);
    }

    #[tokio::test]
    async fn zero_iteration_limit_researches_until_completion() {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let mut responses = (0..4).map(tool_response).collect::<Vec<_>>();
        responses.push(complete_response());
        let provider = ScriptedResearchProvider::new(responses, Arc::clone(&provider_calls));
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoResearchTool {
            calls: Arc::clone(&tool_calls),
        })];

        let result = run_research_phase(
            &config_with_limit(0),
            &provider,
            &tools,
            "gather four facts",
            "test-model",
            0.0,
            Arc::new(NoopObserver),
        )
        .await
        .expect("zero must allow productive research to finish");

        assert_eq!(provider_calls.load(Ordering::SeqCst), 5);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 4);
        assert_eq!(result.tool_call_count, 4);
        assert!(result.context.starts_with("[RESEARCH COMPLETE]"));
    }

    #[tokio::test]
    async fn positive_iteration_limit_remains_a_hard_cap() {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let provider = ScriptedResearchProvider::new(
            vec![tool_response(1), tool_response(2), tool_response(3)],
            Arc::clone(&provider_calls),
        );
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoResearchTool {
            calls: Arc::clone(&tool_calls),
        })];

        let result = run_research_phase(
            &config_with_limit(2),
            &provider,
            &tools,
            "gather facts",
            "test-model",
            0.0,
            Arc::new(NoopObserver),
        )
        .await
        .expect("positive research limit should stop cleanly");

        assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 2);
        assert_eq!(result.tool_call_count, 2);
    }

    #[tokio::test]
    async fn unlimited_research_stops_on_identical_non_progress() {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let tool_calls = Arc::new(AtomicUsize::new(0));
        let provider = ScriptedResearchProvider::new(
            vec![tool_response(1), tool_response(1), tool_response(1)],
            Arc::clone(&provider_calls),
        );
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(EchoResearchTool {
            calls: Arc::clone(&tool_calls),
        })];

        let error = run_research_phase(
            &config_with_limit(0),
            &provider,
            &tools,
            "gather facts",
            "test-model",
            0.0,
            Arc::new(NoopObserver),
        )
        .await
        .expect_err("identical call/result iterations must be treated as a stall");

        assert!(error.to_string().contains("Research phase stalled"));
        assert_eq!(provider_calls.load(Ordering::SeqCst), 3);
        assert_eq!(tool_calls.load(Ordering::SeqCst), 3);
    }
}
