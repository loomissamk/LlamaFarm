//! WebSocket agent chat handler.
//!
//! Protocol:
//! ```text
//! Client -> Server: {"type":"message","content":"Hello"}
//! Server -> Client: {"type":"chunk","content":"Hi! "}
//! Server -> Client: {"type":"tool_call","name":"shell","args":{...}}
//! Server -> Client: {"type":"tool_result","name":"shell","output":"..."}
//! Server -> Client: {"type":"done","full_response":"..."}
//! ```

use super::AppState;
use crate::agent::loop_::{run_tool_call_loop, DRAFT_CLEAR_SENTINEL, DRAFT_PROGRESS_SENTINEL};
use crate::approval::ApprovalManager;
use crate::providers::ChatMessage;
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{header, HeaderMap},
    response::IntoResponse,
};
use serde_json::json;

const EMPTY_WS_RESPONSE_FALLBACK: &str =
    "Tool execution completed, but the model returned no final text response. Please ask me to summarize the result.";
const WS_CHAT_SUBPROTOCOL: &str = "zeroclaw.v1";

#[derive(Debug, Clone, PartialEq, Eq)]
enum WsDeltaEvent {
    ContentChunk(String),
    ToolCall {
        name: String,
        hint: Option<String>,
    },
    ToolResult {
        name: String,
        success: bool,
        duration_secs: Option<u64>,
    },
}

fn sanitize_ws_response(response: &str, tools: &[Box<dyn crate::tools::Tool>]) -> String {
    let sanitized = crate::channels::sanitize_channel_response(response, tools);
    if sanitized.is_empty() && !response.trim().is_empty() {
        "I encountered malformed tool-call output and could not produce a safe reply. Please try again."
            .to_string()
    } else {
        sanitized
    }
}

fn normalize_prompt_tool_results(content: &str) -> Option<String> {
    let mut cleaned_lines: Vec<&str> = Vec::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("<tool_result") || trimmed == "</tool_result>" {
            continue;
        }
        cleaned_lines.push(line.trim_end());
    }

    if cleaned_lines.is_empty() {
        None
    } else {
        Some(cleaned_lines.join("\n"))
    }
}

fn extract_latest_tool_output(history: &[ChatMessage]) -> Option<String> {
    for msg in history.iter().rev() {
        match msg.role.as_str() {
            "tool" => {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&msg.content) {
                    if let Some(content) = value
                        .get("content")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                    {
                        return Some(content.to_string());
                    }
                }

                let trimmed = msg.content.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
            "user" => {
                if let Some(payload) = msg.content.strip_prefix("[Tool results]") {
                    let payload = payload.trim_start_matches('\n');
                    if let Some(cleaned) = normalize_prompt_tool_results(payload) {
                        return Some(cleaned);
                    }
                }
            }
            _ => {}
        }
    }

    None
}

fn finalize_ws_response(
    response: &str,
    history: &[ChatMessage],
    tools: &[Box<dyn crate::tools::Tool>],
) -> String {
    let sanitized = sanitize_ws_response(response, tools);
    if !sanitized.trim().is_empty() {
        return sanitized;
    }

    if let Some(tool_output) = extract_latest_tool_output(history) {
        let excerpt = crate::util::truncate_with_ellipsis(tool_output.trim(), 1200);
        return format!(
            "Tool execution completed, but the model returned no final text response.\n\nLatest tool output:\n{excerpt}"
        );
    }

    EMPTY_WS_RESPONSE_FALLBACK.to_string()
}

fn parse_tool_completion_payload(raw: &str) -> Option<(String, Option<u64>)> {
    let trimmed = raw.trim();
    let (name_part, duration_part) = trimmed.rsplit_once(" (")?;
    let duration_part = duration_part.strip_suffix(')')?;
    let secs = duration_part.strip_suffix('s')?.parse::<u64>().ok();
    Some((name_part.trim().to_string(), secs))
}

fn parse_ws_delta_event(delta: &str) -> Option<WsDeltaEvent> {
    if delta == DRAFT_CLEAR_SENTINEL {
        return None;
    }

    if let Some(progress) = delta.strip_prefix(DRAFT_PROGRESS_SENTINEL) {
        let progress = progress.trim();
        if let Some(rest) = progress.strip_prefix("⏳ ") {
            let rest = rest.trim();
            if rest.is_empty() {
                return None;
            }
            let (name, hint) = match rest.split_once(": ") {
                Some((name, hint)) => {
                    let hint = hint.trim();
                    (
                        name.trim().to_string(),
                        if hint.is_empty() {
                            None
                        } else {
                            Some(hint.to_string())
                        },
                    )
                }
                None => (rest.to_string(), None),
            };
            return Some(WsDeltaEvent::ToolCall { name, hint });
        }

        if let Some(rest) = progress.strip_prefix("✅ ") {
            if let Some((name, duration_secs)) = parse_tool_completion_payload(rest) {
                return Some(WsDeltaEvent::ToolResult {
                    name,
                    success: true,
                    duration_secs,
                });
            }
        }

        if let Some(rest) = progress.strip_prefix("❌ ") {
            if let Some((name, duration_secs)) = parse_tool_completion_payload(rest) {
                return Some(WsDeltaEvent::ToolResult {
                    name,
                    success: false,
                    duration_secs,
                });
            }
        }

        return None;
    }

    if delta.is_empty() {
        None
    } else {
        Some(WsDeltaEvent::ContentChunk(delta.to_string()))
    }
}

async fn emit_ws_delta_event(socket: &mut WebSocket, event: WsDeltaEvent) {
    let payload = match event {
        WsDeltaEvent::ContentChunk(content) => json!({
            "type": "chunk",
            "content": content,
        }),
        WsDeltaEvent::ToolCall { name, hint } => json!({
            "type": "tool_call",
            "name": name,
            "args": {
                "hint": hint,
            },
        }),
        WsDeltaEvent::ToolResult {
            name,
            success,
            duration_secs,
        } => {
            let status = if success { "ok" } else { "error" };
            let output = match duration_secs {
                Some(secs) => format!("{status} ({secs}s)"),
                None => status.to_string(),
            };
            json!({
                "type": "tool_result",
                "name": name,
                "success": success,
                "duration_secs": duration_secs,
                "output": output,
            })
        }
    };

    let _ = socket.send(Message::Text(payload.to_string().into())).await;
}

/// GET /ws/chat — WebSocket upgrade for agent chat
pub async fn handle_ws_chat(
    State(state): State<AppState>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    // Auth via Authorization header or websocket protocol token.
    if state.pairing.require_pairing() {
        let token = extract_ws_bearer_token(&headers).unwrap_or_default();
        if !state.pairing.is_authenticated(&token) {
            return (
                axum::http::StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization: Bearer <token> or Sec-WebSocket-Protocol: zeroclaw.v1, bearer.<token>",
            )
                .into_response();
        }
    }

    ws.protocols([WS_CHAT_SUBPROTOCOL])
        .on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    let mut history: Vec<ChatMessage> = Vec::new();
    while let Some(msg) = socket.recv().await {
        let runtime = state.runtime_snapshot();
        let (provider_label, parallel_tools, native_tools, approval_manager, system_prompt) = {
        let config_guard = state.config.lock();
        let provider_label = config_guard
            .default_provider
            .clone()
            .unwrap_or_else(|| "unknown".to_string());
        let tool_descs: Vec<(&str, &str)> = runtime
            .tools_registry
            .iter()
            .map(|spec| (spec.name.as_str(), spec.description.as_str()))
            .collect();
        let skills =
            crate::skills::load_skills_with_config(&config_guard.workspace_dir, &config_guard);
        let bootstrap_max_chars = if config_guard.agent.compact_context {
            Some(6000)
        } else {
            None
        };
        let native_tools = crate::agent::loop_::configured_native_tools_enabled(
            &config_guard.agent.tool_dispatcher,
            runtime.provider.supports_native_tools(),
        );
        let mut system_prompt = crate::channels::build_system_prompt_with_mode(
            &config_guard.workspace_dir,
            &runtime.model,
            &tool_descs,
            &skills,
            Some(&config_guard.identity),
            bootstrap_max_chars,
            native_tools,
            config_guard.skills.prompt_injection_mode,
        );
        if !native_tools {
            system_prompt.push_str(&crate::agent::loop_::build_tool_instructions(
                runtime.tools_registry_exec.as_ref(),
            ));
        }
        system_prompt.push_str(&crate::agent::loop_::build_shell_policy_instructions(
            &config_guard.autonomy,
        ));
        system_prompt.push_str(
            &crate::agent::loop_::build_runtime_tool_availability_notice(
                runtime.tools_registry_exec.as_ref(),
            ),
        );

        (
            provider_label,
            config_guard.agent.parallel_tools,
            native_tools,
            ApprovalManager::from_config(&config_guard.autonomy),
            system_prompt,
        )
    };
        if let Some(first) = history.first_mut() {
            if first.role == "system" {
                *first = ChatMessage::system(&system_prompt);
            } else {
                history.insert(0, ChatMessage::system(&system_prompt));
            }
        } else {
            history.push(ChatMessage::system(&system_prompt));
        }

        let msg = match msg {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) | Err(_) => break,
            _ => continue,
        };

        // Parse incoming message
        let parsed: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => {
                let err = serde_json::json!({"type": "error", "message": "Invalid JSON"});
                let _ = socket.send(Message::Text(err.to_string().into())).await;
                continue;
            }
        };

        let msg_type = parsed["type"].as_str().unwrap_or("");
        if msg_type != "message" {
            continue;
        }

        let content = parsed["content"].as_str().unwrap_or("").to_string();
        if content.is_empty() {
            continue;
        }

        history.push(ChatMessage::user(&content));

        // Broadcast agent_start event
        let _ = state.event_tx.send(serde_json::json!({
            "type": "agent_start",
            "provider": provider_label,
            "model": runtime.model,
        }));

        let result =
            crate::agent::loop_::with_tool_loop_settings(parallel_tools, native_tools, async {
                let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(128);
                let mut loop_future = std::pin::pin!(run_tool_call_loop(
                    runtime.provider.as_ref(),
                    &mut history,
                    runtime.tools_registry_exec.as_ref(),
                    state.observer.as_ref(),
                    &provider_label,
                    &runtime.model,
                    runtime.temperature,
                    true,
                    Some(&approval_manager),
                    "webchat",
                    &state.multimodal,
                    state.max_tool_iterations,
                    None,
                    Some(delta_tx),
                    None,
                    &[],
                ));

                loop {
                    tokio::select! {
                        maybe_delta = delta_rx.recv() => {
                            if let Some(delta) = maybe_delta {
                                if let Some(event) = parse_ws_delta_event(&delta) {
                                    emit_ws_delta_event(&mut socket, event).await;
                                }
                            } else {
                                break loop_future.await;
                            }
                        }
                        response = &mut loop_future => {
                            while let Ok(delta) = delta_rx.try_recv() {
                                if let Some(event) = parse_ws_delta_event(&delta) {
                                    emit_ws_delta_event(&mut socket, event).await;
                                }
                            }
                            break response;
                        }
                    }
                }
            })
            .await;

        match result {
            Ok(response) => {
                let safe_response =
                    finalize_ws_response(&response, &history, runtime.tools_registry_exec.as_ref());
                history.push(ChatMessage::assistant(&safe_response));

                // Send the full response as a done message
                let done = serde_json::json!({
                    "type": "done",
                    "full_response": safe_response,
                });
                let _ = socket.send(Message::Text(done.to_string().into())).await;

                // Broadcast agent_end event
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "agent_end",
                    "provider": provider_label,
                    "model": runtime.model,
                }));
            }
            Err(e) => {
                let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                let err = serde_json::json!({
                    "type": "error",
                    "message": sanitized,
                });
                let _ = socket.send(Message::Text(err.to_string().into())).await;

                // Broadcast error event
                let _ = state.event_tx.send(serde_json::json!({
                    "type": "error",
                    "component": "ws_chat",
                    "message": sanitized,
                }));
            }
        }
    }
}

fn extract_ws_bearer_token(headers: &HeaderMap) -> Option<String> {
    if let Some(auth_header) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
    {
        if let Some(token) = auth_header.strip_prefix("Bearer ") {
            if !token.trim().is_empty() {
                return Some(token.trim().to_string());
            }
        }
    }

    let offered = headers
        .get(header::SEC_WEBSOCKET_PROTOCOL)
        .and_then(|value| value.to_str().ok())?;

    for protocol in offered.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(token) = protocol.strip_prefix("bearer.") {
            if !token.trim().is_empty() {
                return Some(token.trim().to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{Tool, ToolResult};
    use async_trait::async_trait;
    use axum::http::HeaderValue;

    #[test]
    fn extract_ws_bearer_token_prefers_authorization_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer from-auth-header"),
        );
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("zeroclaw.v1, bearer.from-protocol"),
        );

        assert_eq!(
            extract_ws_bearer_token(&headers).as_deref(),
            Some("from-auth-header")
        );
    }

    #[test]
    fn parse_ws_delta_event_maps_tool_start() {
        let delta = format!("{DRAFT_PROGRESS_SENTINEL}⏳ shell: ls -la\n");
        assert_eq!(
            parse_ws_delta_event(&delta),
            Some(WsDeltaEvent::ToolCall {
                name: "shell".to_string(),
                hint: Some("ls -la".to_string()),
            })
        );
    }

    #[test]
    fn parse_ws_delta_event_maps_tool_success() {
        let delta = format!("{DRAFT_PROGRESS_SENTINEL}✅ shell (2s)\n");
        assert_eq!(
            parse_ws_delta_event(&delta),
            Some(WsDeltaEvent::ToolResult {
                name: "shell".to_string(),
                success: true,
                duration_secs: Some(2),
            })
        );
    }

    #[test]
    fn parse_ws_delta_event_treats_plain_text_as_chunk() {
        let delta = "partial response ".to_string();
        assert_eq!(
            parse_ws_delta_event(&delta),
            Some(WsDeltaEvent::ContentChunk(delta))
        );
    }

    #[test]
    fn extract_ws_bearer_token_reads_websocket_protocol_token() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("zeroclaw.v1, bearer.protocol-token"),
        );

        assert_eq!(
            extract_ws_bearer_token(&headers).as_deref(),
            Some("protocol-token")
        );
    }

    #[test]
    fn extract_ws_bearer_token_ignores_protocol_without_bearer_value() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("zeroclaw.v1"),
        );

        assert!(extract_ws_bearer_token(&headers).is_none());
    }

    #[test]
    fn extract_ws_bearer_token_rejects_empty_tokens() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer    "),
        );
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("zeroclaw.v1, bearer."),
        );

        assert!(extract_ws_bearer_token(&headers).is_none());
    }

    struct MockScheduleTool;

    #[async_trait]
    impl Tool for MockScheduleTool {
        fn name(&self) -> &str {
            "schedule"
        }

        fn description(&self) -> &str {
            "Mock schedule tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string" }
                }
            })
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "ok".to_string(),
                error: None,
            })
        }
    }

    #[test]
    fn sanitize_ws_response_removes_tool_call_tags() {
        let input = r#"Before
<tool_call>
{"name":"schedule","arguments":{"action":"create"}}
</tool_call>
After"#;

        let result = sanitize_ws_response(input, &[]);
        let normalized = result
            .lines()
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        assert_eq!(normalized, "Before\nAfter");
        assert!(!result.contains("<tool_call>"));
        assert!(!result.contains("\"name\":\"schedule\""));
    }

    #[test]
    fn sanitize_ws_response_removes_isolated_tool_json_artifacts() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];
        let input = r#"{"name":"schedule","parameters":{"action":"create"}}
{"result":{"status":"scheduled"}}
Reminder set successfully."#;

        let result = sanitize_ws_response(input, &tools);
        assert_eq!(result, "Reminder set successfully.");
        assert!(!result.contains("\"name\":\"schedule\""));
        assert!(!result.contains("\"result\""));
    }

    #[test]
    fn finalize_ws_response_uses_prompt_mode_tool_output_when_final_text_empty() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user(
                "[Tool results]\n<tool_result name=\"schedule\">\nDisk usage: 72%\n</tool_result>",
            ),
        ];

        let result = finalize_ws_response("", &history, &tools);
        assert!(result.contains("Latest tool output:"));
        assert!(result.contains("Disk usage: 72%"));
        assert!(!result.contains("<tool_result"));
    }

    #[test]
    fn finalize_ws_response_uses_native_tool_message_output_when_final_text_empty() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];
        let history = vec![ChatMessage {
            role: "tool".to_string(),
            content: r#"{"tool_call_id":"call_1","content":"Filesystem /dev/disk3s1: 210G free"}"#
                .to_string(),
        }];

        let result = finalize_ws_response("", &history, &tools);
        assert!(result.contains("Latest tool output:"));
        assert!(result.contains("/dev/disk3s1"));
    }

    #[test]
    fn finalize_ws_response_uses_static_fallback_when_nothing_available() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(MockScheduleTool)];
        let history = vec![ChatMessage::system("sys")];

        let result = finalize_ws_response("", &history, &tools);
        assert_eq!(result, EMPTY_WS_RESPONSE_FALLBACK);
    }
}
