//! WebSocket agent chat handler.
//!
//! Protocol:
//! ```text
//! Client -> Server: {"type":"message","content":"Hello"}
//! Client -> Server: {"type":"cancel","session_id":"chat-1"}
//! Server -> Client: {"type":"chunk","content":"Hi! "}
//! Server -> Client: {"type":"tool_call","name":"shell","args":{...}}
//! Server -> Client: {"type":"tool_result","name":"shell","output":"..."}
//! Server -> Client: {"type":"done","full_response":"..."}
//! Server -> Client: {"type":"cancelled","message":"Stopped by user..."}
//! ```

use super::{AppState, GatewayRuntimeSnapshot};
use crate::agent::loop_::{DRAFT_CLEAR_SENTINEL, DRAFT_PROGRESS_SENTINEL, run_tool_call_loop};
use crate::approval::ApprovalManager;
use crate::federation::remote_subagent::{
    FederationChatContext, FederationChatEvent, with_chat_context,
};
use crate::memory::MemoryCategory;
use crate::providers::ChatMessage;
use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, header},
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt, stream::SplitSink};
use parking_lot::Mutex;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const EMPTY_WS_RESPONSE_FALLBACK: &str = "Tool execution completed, but the model returned no final text response. Please ask me to summarize the result.";
const WS_AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;
const WS_CHAT_SUBPROTOCOL: &str = "llamafarm.v1";
const WS_CHAT_STORE_REL_PATH: &str = "state/web-chat-sessions.json";
const WS_PERSISTED_MAX_MESSAGES: usize = 800;
const WS_PERSISTED_MAX_SESSIONS: usize = 64;
const WS_RESTORED_CONTEXT_PREFIX: &str = "[Saved chat context restored]";
const WS_CANCELLED_MESSAGE: &str =
    "Stopped by user. Completed tool results remain in this chat; no final response was produced.";

/// The write half of an upgraded chat socket. Splitting the WebSocket lets the
/// active agent turn continue emitting deltas while the read half receives a
/// `cancel` control frame from the browser.
type WsSink = SplitSink<WebSocket, Message>;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct WsChatSession {
    history: Vec<ChatMessage>,
    temporary: bool,
}

static WS_CHAT_SESSIONS: LazyLock<Mutex<HashMap<String, WsChatSession>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedWsChatSessions {
    #[serde(default)]
    sessions: HashMap<String, PersistedWsChatSession>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct PersistedWsChatSession {
    #[serde(default)]
    history: Vec<ChatMessage>,
    #[serde(default)]
    updated_at_unix: u64,
}

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
        output: String,
    },
}

fn normalize_ws_session_id(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

/// Control frames that may arrive while an agent turn owns the write half of
/// the socket. Keep parsing small and explicit so a malformed frame cannot
/// accidentally be treated as a Stop request.
#[derive(Debug, PartialEq, Eq)]
enum InFlightWsControl {
    Cancel { session_id: String },
    SessionDelete { session_id: String },
    Message { session_id: String },
    Other,
    InvalidJson,
}

fn parse_inflight_ws_control(text: &str) -> InFlightWsControl {
    let parsed = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(value) => value,
        Err(_) => return InFlightWsControl::InvalidJson,
    };
    let session_id =
        normalize_ws_session_id(parsed.get("session_id").and_then(serde_json::Value::as_str))
            .unwrap_or_else(|| "default".to_string());

    match parsed
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
    {
        "cancel" => InFlightWsControl::Cancel { session_id },
        "session_delete" => InFlightWsControl::SessionDelete { session_id },
        "message" => InFlightWsControl::Message { session_id },
        _ => InFlightWsControl::Other,
    }
}

fn parse_seed_history(value: Option<&serde_json::Value>) -> Vec<ChatMessage> {
    let Some(items) = value.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| {
            let role = item.get("role").and_then(serde_json::Value::as_str)?.trim();
            let content = item
                .get("content")
                .and_then(serde_json::Value::as_str)?
                .trim()
                .to_string();
            if content.is_empty() {
                return None;
            }

            let normalized_role = match role {
                "assistant" | "agent" => "assistant",
                "user" => "user",
                "tool" => "tool",
                _ => return None,
            };

            Some(ChatMessage {
                role: normalized_role.to_string(),
                content,
            })
        })
        .collect()
}

fn parse_selected_federation_peer_ids(value: Option<&serde_json::Value>) -> Vec<String> {
    let Some(items) = value.and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };

    let mut peers = items
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    peers.sort();
    peers.dedup();
    peers
}

fn resolve_ws_chat_store_path(config: &crate::config::Config) -> PathBuf {
    config
        .config_path
        .parent()
        .map(PathBuf::from)
        .unwrap_or_else(|| config.workspace_dir.clone())
        .join(WS_CHAT_STORE_REL_PATH)
}

fn current_unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn normalize_ws_history_for_storage(history: &[ChatMessage]) -> Vec<ChatMessage> {
    let filtered: Vec<ChatMessage> = history
        .iter()
        .filter(|message| message.role != "system")
        .filter_map(|message| {
            if message.content.trim().is_empty() {
                None
            } else if (message.role == "user"
                && is_internal_tool_loop_user_message(&message.content))
                || (message.role == "assistant"
                    && (is_restored_context_message(&message.content)
                        || is_internal_tool_loop_assistant_message(&message.content)))
            {
                None
            } else {
                Some(message.clone())
            }
        })
        .collect();

    let mut normalized = Vec::with_capacity(filtered.len());
    let mut pending_tool_trace = Vec::new();

    for message in filtered {
        let is_raw_tool_trace = message.role == "tool"
            || (message.role == "assistant" && is_assistant_tool_call_payload(&message.content));

        if is_raw_tool_trace {
            pending_tool_trace.push(message);
            continue;
        }

        if message.role == "assistant" {
            // When a turn already has a natural-language assistant reply, keep that
            // summary and drop the preceding raw tool-call/tool-result protocol trace.
            pending_tool_trace.clear();
            normalized.push(message);
            continue;
        }

        if !pending_tool_trace.is_empty() {
            normalized.append(&mut pending_tool_trace);
        }
        normalized.push(message);
    }

    if !pending_tool_trace.is_empty() {
        normalized.append(&mut pending_tool_trace);
    }

    if normalized.len() > WS_PERSISTED_MAX_MESSAGES {
        let keep_from = normalized.len() - WS_PERSISTED_MAX_MESSAGES;
        normalized.drain(..keep_from);
    }

    normalized
}

fn select_resume_history(
    history_seed: &[ChatMessage],
    persisted_history: &[ChatMessage],
) -> Vec<ChatMessage> {
    let seeded = normalize_ws_history_for_storage(history_seed);
    let persisted = normalize_ws_history_for_storage(persisted_history);

    match seeded.len().cmp(&persisted.len()) {
        std::cmp::Ordering::Greater => seeded,
        std::cmp::Ordering::Less => persisted,
        std::cmp::Ordering::Equal if !seeded.is_empty() => seeded,
        std::cmp::Ordering::Equal => persisted,
    }
}

async fn read_persisted_ws_chat_sessions(path: &Path) -> PersistedWsChatSessions {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return PersistedWsChatSessions::default();
    };
    if bytes.is_empty() {
        return PersistedWsChatSessions::default();
    }

    match serde_json::from_slice::<PersistedWsChatSessions>(&bytes) {
        Ok(parsed) => parsed,
        Err(error) => {
            tracing::warn!(path = %path.display(), error = %error, "failed to parse persisted ws chat sessions");
            PersistedWsChatSessions::default()
        }
    }
}

async fn write_persisted_ws_chat_sessions(
    path: &Path,
    sessions: &PersistedWsChatSessions,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(sessions)?;
    tokio::fs::write(&tmp, data).await?;
    tokio::fs::rename(&tmp, path).await?;
    Ok(())
}

async fn persist_ws_chat_session(store_path: &Path, session_id: &str, history: &[ChatMessage]) {
    let mut persisted = read_persisted_ws_chat_sessions(store_path).await;
    persisted.sessions.insert(
        session_id.to_string(),
        PersistedWsChatSession {
            history: normalize_ws_history_for_storage(history),
            updated_at_unix: current_unix_timestamp_secs(),
        },
    );

    if persisted.sessions.len() > WS_PERSISTED_MAX_SESSIONS {
        let mut ordered: Vec<(String, u64)> = persisted
            .sessions
            .iter()
            .map(|(session_id, session)| (session_id.clone(), session.updated_at_unix))
            .collect();
        ordered.sort_by_key(|(_, updated_at_unix)| *updated_at_unix);

        let remove_count = persisted.sessions.len() - WS_PERSISTED_MAX_SESSIONS;
        for (session_id, _) in ordered.into_iter().take(remove_count) {
            persisted.sessions.remove(&session_id);
        }
    }

    if let Err(error) = write_persisted_ws_chat_sessions(store_path, &persisted).await {
        tracing::warn!(path = %store_path.display(), error = %error, "failed to persist ws chat session");
    }
}

async fn delete_persisted_ws_chat_session(store_path: &Path, session_id: &str) {
    let mut persisted = read_persisted_ws_chat_sessions(store_path).await;
    if persisted.sessions.remove(session_id).is_none() {
        return;
    }

    if let Err(error) = write_persisted_ws_chat_sessions(store_path, &persisted).await {
        tracing::warn!(path = %store_path.display(), error = %error, "failed to delete persisted ws chat session");
    }
}

async fn load_ws_chat_history(
    session_id: &str,
    temporary: bool,
    history_seed: &[ChatMessage],
    store_path: &Path,
) -> Vec<ChatMessage> {
    let seeded_history = normalize_ws_history_for_storage(history_seed);
    let existing = {
        let mut sessions = WS_CHAT_SESSIONS.lock();
        if let Some(entry) = sessions.get_mut(session_id) {
            entry.temporary = temporary;
            let merged = select_resume_history(&seeded_history, &entry.history);
            let changed = merged != entry.history;
            if changed {
                entry.history = merged.clone();
            }
            Some((entry.history.clone(), changed && !temporary))
        } else {
            None
        }
    };

    if let Some((history, should_persist)) = existing {
        if should_persist {
            persist_ws_chat_session(store_path, session_id, &history).await;
        }
        return history;
    }

    let persisted_history = if temporary {
        Vec::new()
    } else {
        let mut persisted = read_persisted_ws_chat_sessions(store_path).await;
        persisted
            .sessions
            .remove(session_id)
            .map(|session| session.history)
            .unwrap_or_default()
    };
    let initial_history = if seeded_history.is_empty() && !persisted_history.is_empty() {
        build_restored_ws_chat_history(&persisted_history)
    } else {
        select_resume_history(&seeded_history, &persisted_history)
    };

    let mut sessions = WS_CHAT_SESSIONS.lock();
    let entry = sessions
        .entry(session_id.to_string())
        .or_insert_with(|| WsChatSession {
            history: initial_history.clone(),
            temporary,
        });

    if entry.history.is_empty() && !initial_history.is_empty() {
        entry.history = initial_history;
    }
    entry.temporary = temporary;
    entry.history.clone()
}

async fn store_ws_chat_history(
    session_id: &str,
    history: &[ChatMessage],
    temporary: bool,
    store_path: &Path,
) {
    let normalized = normalize_ws_history_for_storage(history);
    {
        let mut sessions = WS_CHAT_SESSIONS.lock();
        sessions.insert(
            session_id.to_string(),
            WsChatSession {
                history: normalized.clone(),
                temporary,
            },
        );
    }

    if !temporary {
        persist_ws_chat_session(store_path, session_id, &normalized).await;
    }
}

async fn delete_ws_chat_history(session_id: &str, store_path: &Path) {
    {
        let mut sessions = WS_CHAT_SESSIONS.lock();
        sessions.remove(session_id);
    }

    delete_persisted_ws_chat_session(store_path, session_id).await;
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

fn is_restored_context_message(content: &str) -> bool {
    content.trim_start().starts_with(WS_RESTORED_CONTEXT_PREFIX)
}

fn is_internal_tool_loop_user_message(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("[Tool results]")
        || trimmed.starts_with("Internal correction:")
        || trimmed.starts_with("Internal continuation:")
        || trimmed.starts_with("Internal working state:")
}

fn is_internal_tool_loop_assistant_message(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("Internal correction:")
        || trimmed.starts_with("Internal continuation:")
        || trimmed.starts_with("Internal working state:")
}

fn is_assistant_tool_call_payload(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| value.get("tool_calls").cloned())
        .is_some()
}

fn extract_latest_assistant_reply(history: &[ChatMessage]) -> Option<String> {
    history.iter().rev().find_map(|message| {
        if message.role != "assistant"
            || is_assistant_tool_call_payload(&message.content)
            || is_restored_context_message(&message.content)
            || is_internal_tool_loop_assistant_message(&message.content)
        {
            return None;
        }

        let trimmed = message.content.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn build_ws_resume_context(history: &[ChatMessage]) -> Option<String> {
    let latest_user = history.iter().rev().find_map(|message| {
        if message.role != "user" || is_internal_tool_loop_user_message(&message.content) {
            return None;
        }

        let trimmed = message.content.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(crate::util::truncate_with_ellipsis(trimmed, 500))
        }
    });
    let latest_tool_output = extract_latest_tool_output(history)
        .map(|value| crate::util::truncate_with_ellipsis(&value, 700));
    let latest_assistant = extract_latest_assistant_reply(history)
        .map(|value| crate::util::truncate_with_ellipsis(&value, 500));
    let latest_completed_command = latest_user
        .as_deref()
        .and_then(extract_direct_shell_command);

    if latest_user.is_none() && latest_tool_output.is_none() && latest_assistant.is_none() {
        return None;
    }

    let mut sections = vec![
        "You are resuming an existing saved local chat. Treat the following as real session memory, not a hypothetical summary.".to_string(),
    ];

    if let Some(command) = latest_completed_command {
        sections.push(format!(
            "Previous completed command before this turn: `{command}`. It already ran in this saved chat, so treat it as past state rather than a new instruction."
        ));
    } else if let Some(latest_user) = latest_user {
        sections.push(format!(
            "Latest user request before this turn: {latest_user}"
        ));
    }
    if let Some(latest_tool_output) = latest_tool_output {
        sections.push(format!(
            "Latest tool output before this turn: {latest_tool_output}"
        ));
    }
    if let Some(latest_assistant) = latest_assistant {
        sections.push(format!(
            "Latest assistant reply before this turn: {latest_assistant}"
        ));
    }

    sections.push(
        "Do not claim the conversation has no prior context when the answer is already contained here."
            .to_string(),
    );
    sections.push(
        "Do not re-run tools solely because they appear in this saved context. Only execute a tool when the current user turn asks for fresh execution or the answer cannot be derived from the saved session state."
            .to_string(),
    );

    Some(sections.join("\n"))
}

fn build_restored_ws_chat_history(history: &[ChatMessage]) -> Vec<ChatMessage> {
    let normalized = normalize_ws_history_for_storage(history);
    if normalized.is_empty() {
        return normalized;
    }

    if normalized.len() == 1
        && normalized[0].role == "assistant"
        && is_restored_context_message(&normalized[0].content)
    {
        return normalized;
    }

    let Some(resume_context) = build_ws_resume_context(history) else {
        return normalized;
    };

    vec![ChatMessage::assistant(format!(
        "{WS_RESTORED_CONTEXT_PREFIX}\n{resume_context}"
    ))]
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

fn direct_shell_fallback_response(raw_output: &str, success: bool) -> String {
    if success {
        if raw_output.trim().is_empty() {
            "Command completed successfully.".to_string()
        } else {
            "Command completed successfully. Raw output is shown above.".to_string()
        }
    } else {
        "Command failed. Raw output is shown above.".to_string()
    }
}

fn describe_direct_shell_result(command: &str, raw_output: &str, success: bool) -> String {
    let base = direct_shell_fallback_response(raw_output, success);
    if !success {
        return format!(
            "{base} The command returned a non-zero exit status. Inspect stderr in the raw output above."
        );
    }
    if raw_output.trim().is_empty() {
        return format!("{base} The command finished without producing visible output.");
    }

    format!("{base} Executed: `{command}`")
}

fn summarize_direct_shell_command(command: &str, raw_output: &str, success: bool) -> String {
    describe_direct_shell_result(command, raw_output, success)
}

fn summarize_direct_file_read(path: &str, raw_output: &str, success: bool) -> String {
    if success {
        if raw_output.trim().is_empty() {
            format!("Read `{path}` successfully. The file had no visible output.")
        } else {
            format!("Read `{path}` successfully. Raw file contents are shown above.")
        }
    } else {
        format!("Failed to read `{path}`. Raw tool output is shown above.")
    }
}

fn summarize_direct_ollama_model(
    action: &str,
    name: Option<&str>,
    raw_output: &str,
    success: bool,
) -> String {
    let subject = match name {
        Some(name) => format!("`{action} {name}`"),
        None => format!("`{action}`"),
    };

    if success {
        if raw_output.trim().is_empty() {
            format!("Ollama command {subject} completed successfully.")
        } else {
            format!(
                "Ollama command {subject} completed successfully. Raw tool output is shown above."
            )
        }
    } else {
        format!("Ollama command {subject} failed. Raw tool output is shown above.")
    }
}

fn websocket_memory_key() -> String {
    format!("webchat_msg_{}", Uuid::new_v4())
}

fn looks_like_direct_shell_command(candidate: &str) -> bool {
    const DIRECT_COMMAND_PREFIXES: &[&str] = &[
        "bash ",
        "cat ",
        "curl ",
        "date",
        "df ",
        "docker ",
        "docker-compose ",
        "du ",
        "echo ",
        "env",
        "find ",
        "git ",
        "grep ",
        "jq ",
        "ls ",
        "lsblk",
        "lspci",
        "lsusb",
        "node ",
        "npm ",
        "pwd",
        "python ",
        "python3 ",
        "rg ",
        "sh ",
        "sqlite3 ",
        "ss ",
        "stat ",
        "tail ",
        "uname",
        "whoami",
    ];

    let trimmed = candidate.trim();
    if trimmed.is_empty() || trimmed.lines().count() != 1 {
        return false;
    }

    let lowered = trimmed.to_ascii_lowercase();
    DIRECT_COMMAND_PREFIXES
        .iter()
        .any(|prefix| lowered == *prefix || lowered.starts_with(prefix))
}

fn has_explanatory_suffix(command: &str) -> bool {
    static EXPLANATION_SUFFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(?:and|then)\s+(?:explain|describe|summari[sz]e|interpret|analy[sz]e)\b")
            .unwrap()
    });
    EXPLANATION_SUFFIX_RE.is_match(command)
}

fn extract_direct_shell_command(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if looks_like_direct_shell_command(trimmed) && !has_explanatory_suffix(trimmed) {
        return Some(trimmed.to_string());
    }

    for prefix in [
        "run ",
        "execute ",
        "run this exact command:",
        "run this exact command",
        "execute this exact command:",
        "execute this exact command",
        "run this command:",
        "execute this command:",
    ] {
        if let Some(rest) = trimmed
            .to_ascii_lowercase()
            .strip_prefix(prefix)
            .map(|_| trimmed[prefix.len()..].trim().trim_matches('`'))
        {
            if looks_like_direct_shell_command(rest) && !has_explanatory_suffix(rest) {
                return Some(rest.to_string());
            }
        }
    }

    None
}

fn shell_quote_single(token: &str) -> String {
    format!("'{}'", token.replace('\'', "'\"'\"'"))
}

fn normalize_direct_directory_target(raw: &str) -> Option<String> {
    let trimmed = raw
        .trim()
        .trim_end_matches(['?', '.', ':'])
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_article = trimmed.strip_prefix("the ").unwrap_or(trimmed).trim();
    let trimmed = without_article
        .strip_suffix(" directory")
        .or_else(|| without_article.strip_suffix(" folder"))
        .unwrap_or(without_article)
        .trim();

    if trimmed.is_empty() {
        return None;
    }

    if matches!(
        trimmed.to_ascii_lowercase().as_str(),
        "workspace" | "current workspace"
    ) {
        Some(".".to_string())
    } else {
        Some(trimmed.to_string())
    }
}

fn extract_direct_directory_listing_command(message: &str) -> Option<String> {
    static DIRECTORY_LIST_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)^(?:ls|list|show|what)\s+(?:all\s+)?(?:files|contents?)?(?:\s+are)?\s*(?:in|of)?\s+(.+)$",
        )
        .unwrap()
    });

    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lowered = trimmed.to_ascii_lowercase();
    if matches!(
        lowered.as_str(),
        "what files are in the workspace"
            | "what files are in workspace"
            | "list workspace files"
            | "show workspace files"
    ) {
        return Some("ls -la".to_string());
    }

    let captures = DIRECTORY_LIST_RE.captures(trimmed)?;
    let path = normalize_direct_directory_target(captures.get(1)?.as_str())?;
    if path == "." {
        Some("ls -la".to_string())
    } else {
        Some(format!("ls -la {}", shell_quote_single(&path)))
    }
}

fn looks_like_direct_file_path(candidate: &str) -> bool {
    let trimmed = candidate.trim();
    if trimmed.is_empty() {
        return false;
    }

    trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.contains('/')
        || trimmed
            .rsplit_once('.')
            .is_some_and(|(stem, ext)| !stem.is_empty() && !ext.is_empty())
}

fn extract_direct_file_read_path(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }

    let lowered = trimmed.to_ascii_lowercase();
    for prefix in ["read ", "show ", "open "] {
        if !lowered.starts_with(prefix) {
            continue;
        }

        let mut remainder = trimmed[prefix.len()..].trim();
        for suffix in [
            " and show its contents",
            " and show the contents",
            " and show contents",
            " and print its contents",
            " and print the contents",
            " and print contents",
            " and show it",
            " and print it",
            " and review it",
            " contents",
            " content",
        ] {
            if remainder.to_ascii_lowercase().ends_with(suffix) {
                let new_len = remainder.len().saturating_sub(suffix.len());
                remainder = remainder[..new_len].trim();
                break;
            }
        }

        let path = remainder
            .trim_end_matches(['?', '.', ':'])
            .trim_matches('`')
            .trim_matches('"')
            .trim_matches('\'')
            .trim();

        if looks_like_direct_file_path(path) {
            return Some(path.to_string());
        }
    }

    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectOllamaModelRequest {
    action: String,
    name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectFileWriteRequest {
    path: String,
    instruction: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectExecutionIntent {
    FileRead(String),
    WorkspaceDelete(String),
    WorkspaceCreateDirectory(String),
    OllamaModel(DirectOllamaModelRequest),
    Shell(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectForcedToolIntent {
    FileWrite(DirectFileWriteRequest),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DirectIntent {
    ExecuteNow(DirectExecutionIntent),
    ForceTool(DirectForcedToolIntent),
}

fn normalize_direct_ollama_model_action(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "list" | "ls" => Some("list"),
        "running" | "ps" => Some("running"),
        "pull" => Some("pull"),
        "show" => Some("show"),
        "delete" | "remove" | "rm" => Some("delete"),
        _ => None,
    }
}

fn looks_like_direct_ollama_model_name(candidate: &str) -> bool {
    let trimmed = candidate.trim();
    !trimmed.is_empty()
        && trimmed.lines().count() == 1
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | ':' | '-' | '_' | '/'))
        && trimmed.chars().any(|ch| ch.is_ascii_alphabetic())
}

fn looks_like_probable_workspace_path(candidate: &str) -> bool {
    let trimmed = candidate.trim();
    if trimmed.is_empty() || trimmed.lines().count() != 1 {
        return false;
    }

    if trimmed.starts_with("./")
        || trimmed.starts_with("../")
        || trimmed.starts_with('/')
        || trimmed.starts_with("~/")
        || trimmed.contains('/')
    {
        return true;
    }

    let token = trimmed
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return false;
    }

    if token.starts_with('-') {
        return false;
    }

    let lower = token.to_ascii_lowercase();
    if lower.ends_with(".md")
        || lower.ends_with(".txt")
        || lower.ends_with(".py")
        || lower.ends_with(".rs")
        || lower.ends_with(".json")
        || lower.ends_with(".toml")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".sh")
    {
        return true;
    }

    token
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-'))
}

fn parse_direct_ollama_model_request(raw: &str) -> Option<DirectOllamaModelRequest> {
    let trimmed = raw.trim().trim_matches('`');
    if trimmed.is_empty() {
        return None;
    }

    let has_ollama_prefix = trimmed.starts_with("ollama ") || trimmed.starts_with("Ollama ");
    let without_ollama = trimmed
        .strip_prefix("ollama ")
        .or_else(|| trimmed.strip_prefix("Ollama "))
        .unwrap_or(trimmed)
        .trim();
    let (raw_action, remainder) = without_ollama
        .split_once(char::is_whitespace)
        .map_or((without_ollama, ""), |(action, rest)| (action, rest.trim()));
    let action = normalize_direct_ollama_model_action(raw_action)?;

    if !has_ollama_prefix
        && (!matches!(action, "pull" | "show")
            || !looks_like_direct_ollama_model_name(remainder)
            || looks_like_probable_workspace_path(remainder))
    {
        return None;
    }

    let name = if remainder.is_empty() {
        None
    } else if looks_like_direct_ollama_model_name(remainder) {
        Some(remainder.to_string())
    } else {
        return None;
    };

    Some(DirectOllamaModelRequest {
        action: action.to_string(),
        name,
    })
}

fn normalize_direct_workspace_target(raw: &str) -> Option<String> {
    let trimmed = raw
        .trim()
        .trim_end_matches(['?', '.', ':'])
        .trim_matches('`')
        .trim_matches('"')
        .trim_matches('\'')
        .trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_article = trimmed.strip_prefix("the ").unwrap_or(trimmed).trim();
    let without_kind = without_article
        .strip_prefix("file ")
        .or_else(|| without_article.strip_prefix("folder "))
        .or_else(|| without_article.strip_prefix("directory "))
        .unwrap_or(without_article)
        .trim();
    if without_kind.is_empty() || without_kind.contains('\n') {
        return None;
    }

    Some(without_kind.to_string())
}

fn extract_direct_workspace_delete_path(message: &str) -> Option<String> {
    let trimmed = message.trim();
    let lowered = trimmed.to_ascii_lowercase();
    for prefix in ["rm ", "delete ", "remove "] {
        if lowered.starts_with(prefix) {
            let path = normalize_direct_workspace_target(&trimmed[prefix.len()..])?;
            if looks_like_probable_workspace_path(&path) {
                return Some(path);
            }
        }
    }

    None
}

fn extract_direct_workspace_directory_create_path(message: &str) -> Option<String> {
    let trimmed = message.trim();
    let lowered = trimmed.to_ascii_lowercase();

    if lowered.starts_with("mkdir ") {
        let path = normalize_direct_workspace_target(&trimmed["mkdir ".len()..])?;
        if looks_like_probable_workspace_path(&path) {
            return Some(path);
        }
    }

    for prefix in [
        "create folder ",
        "create directory ",
        "make folder ",
        "make directory ",
    ] {
        if lowered.starts_with(prefix) {
            let path = normalize_direct_workspace_target(&trimmed[prefix.len()..])?;
            if looks_like_probable_workspace_path(&path) {
                return Some(path);
            }
        }
    }

    None
}

fn extract_direct_file_write_request(message: &str) -> Option<DirectFileWriteRequest> {
    static DIRECT_FILE_WRITE_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r#"(?is)^(?:write_?file|create_?file|save_?file|write file|create file|save file)\s+(?:called\s+|named\s+)?(?:`([^`]+)`|'([^']+)'|"([^"]+)"|([^\s]+))(?:\s+(?:to|with|containing)\s+(.+))?$"#,
        )
        .unwrap()
    });

    let captures = DIRECT_FILE_WRITE_RE.captures(message.trim())?;
    let path = captures
        .get(1)
        .or_else(|| captures.get(2))
        .or_else(|| captures.get(3))
        .or_else(|| captures.get(4))
        .map(|m| m.as_str().trim().to_string())
        .filter(|value| !value.is_empty())?;
    let instruction = captures
        .get(5)
        .map(|m| m.as_str().trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| message.trim().to_string());

    Some(DirectFileWriteRequest { path, instruction })
}

fn classify_direct_intent(message: &str) -> Option<DirectIntent> {
    if let Some(path) = extract_direct_file_read_path(message) {
        return Some(DirectIntent::ExecuteNow(DirectExecutionIntent::FileRead(
            path,
        )));
    }

    if let Some(request) = extract_direct_file_write_request(message) {
        return Some(DirectIntent::ForceTool(DirectForcedToolIntent::FileWrite(
            request,
        )));
    }

    if let Some(path) = extract_direct_workspace_delete_path(message) {
        return Some(DirectIntent::ExecuteNow(
            DirectExecutionIntent::WorkspaceDelete(path),
        ));
    }

    if let Some(path) = extract_direct_workspace_directory_create_path(message) {
        return Some(DirectIntent::ExecuteNow(
            DirectExecutionIntent::WorkspaceCreateDirectory(path),
        ));
    }

    if let Some(request) = extract_direct_ollama_model_request(message) {
        return Some(DirectIntent::ExecuteNow(
            DirectExecutionIntent::OllamaModel(request),
        ));
    }

    if let Some(command) = extract_direct_directory_listing_command(message)
        .or_else(|| extract_direct_shell_command(message))
    {
        return Some(DirectIntent::ExecuteNow(DirectExecutionIntent::Shell(
            command,
        )));
    }

    None
}

fn build_forced_file_write_prompt(base: &str, request: &DirectFileWriteRequest) -> String {
    let mut prompt = base.to_string();
    prompt.push_str(
        "\n## Forced File Write Intent\n\n\
         The current user message is an explicit file-creation request.\n\
         You must create the requested file with a real `file_write` tool call.\n\
         Allowed tools for this turn are `file_write` and `task_plan` only.\n\
         Use `task_plan` only if the request is clearly multi-step; otherwise call `file_write` immediately.\n\
         Do not answer with prose until a real `file_write` tool call succeeds or the runtime returns a blocking error.\n",
    );
    prompt.push_str(&format!(
        "- Required target path: `{}`\n- Content goal: {}\n",
        request.path, request.instruction
    ));
    prompt
}

fn extract_direct_ollama_model_request(message: &str) -> Option<DirectOllamaModelRequest> {
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Some(parsed) = parse_direct_ollama_model_request(trimmed) {
        return Some(parsed);
    }

    for prefix in [
        "run ",
        "execute ",
        "run this exact command:",
        "run this exact command",
        "execute this exact command:",
        "execute this exact command",
        "run this command:",
        "execute this command:",
    ] {
        if let Some(rest) = trimmed
            .to_ascii_lowercase()
            .strip_prefix(prefix)
            .map(|_| trimmed[prefix.len()..].trim())
        {
            if let Some(parsed) = parse_direct_ollama_model_request(rest) {
                return Some(parsed);
            }
        }
    }

    None
}

fn split_tool_progress_payload(raw: &str) -> (&str, Option<&str>) {
    let trimmed = raw.trim_end();
    match trimmed.split_once('\n') {
        Some((header, output)) => (header.trim(), Some(output)),
        None => (trimmed.trim(), None),
    }
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
            let (header, output) = split_tool_progress_payload(rest);
            if let Some((name, duration_secs)) = parse_tool_completion_payload(header) {
                return Some(WsDeltaEvent::ToolResult {
                    name,
                    success: true,
                    duration_secs,
                    output: output.unwrap_or("(no output)").to_string(),
                });
            }
        }

        if let Some(rest) = progress.strip_prefix("❌ ") {
            let (header, output) = split_tool_progress_payload(rest);
            if let Some((name, duration_secs)) = parse_tool_completion_payload(header) {
                return Some(WsDeltaEvent::ToolResult {
                    name,
                    success: false,
                    duration_secs,
                    output: output.unwrap_or("(no output)").to_string(),
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

async fn emit_ws_delta_event(socket: &mut WsSink, session_id: &str, event: WsDeltaEvent) {
    let payload = match event {
        WsDeltaEvent::ContentChunk(content) => json!({
            "type": "chunk",
            "session_id": session_id,
            "content": content,
        }),
        WsDeltaEvent::ToolCall { name, hint } => json!({
            "type": "tool_call",
            "session_id": session_id,
            "name": name,
            "args": {
                "hint": hint,
            },
        }),
        WsDeltaEvent::ToolResult {
            name,
            success,
            duration_secs,
            output,
        } => json!({
            "type": "tool_result",
            "session_id": session_id,
            "name": name,
            "success": success,
            "duration_secs": duration_secs,
            "output": output,
        }),
    };

    let _ = socket.send(Message::Text(payload.to_string().into())).await;
}

async fn emit_ws_federation_event(socket: &mut WsSink, event: FederationChatEvent) {
    let payload = json!({
        "type": event.event_type,
        "session_id": event.session_id,
        "peer_id": event.peer_id,
        "peer_name": event.peer_name,
        "delegate_agent": event.delegate_agent,
        "task_id": event.task_id,
        "content": event.content,
        "name": event.name,
        "args": event.args,
        "success": event.success,
        "duration_secs": event.duration_secs,
        "output": event.output,
        "message": event.message,
    });

    let _ = socket.send(Message::Text(payload.to_string().into())).await;
}

async fn execute_direct_shell_command(
    socket: &mut WsSink,
    session_id: &str,
    runtime: &GatewayRuntimeSnapshot,
    history: &mut Vec<ChatMessage>,
    command: &str,
) -> anyhow::Result<String> {
    let Some(shell_tool) = runtime
        .tools_registry_exec
        .iter()
        .find(|tool| tool.name() == "shell")
    else {
        anyhow::bail!("shell tool is not available in this runtime");
    };

    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolCall {
            name: "shell".to_string(),
            hint: Some(command.to_string()),
        },
    )
    .await;

    let started_at = Instant::now();
    let tool_result = shell_tool
        .execute(json!({ "command": command }))
        .await
        .map_err(|error| anyhow::anyhow!("shell execution failed: {error}"))?;

    let raw_output = if tool_result.output.trim().is_empty() {
        tool_result.error.clone().unwrap_or_default()
    } else {
        tool_result.output.clone()
    };

    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolResult {
            name: "shell".to_string(),
            success: tool_result.success,
            duration_secs: Some(started_at.elapsed().as_secs()),
            output: if raw_output.trim().is_empty() {
                "(no output)".to_string()
            } else {
                raw_output.clone()
            },
        },
    )
    .await;

    let tool_call_id = format!("ws_shell_{}", Uuid::new_v4());
    let assistant_tool_call = json!({
        "content": serde_json::Value::Null,
        "tool_calls": [{
            "id": tool_call_id,
            "name": "shell",
            "arguments": serde_json::to_string(&json!({ "command": command }))
                .unwrap_or_else(|_| "{}".to_string()),
        }],
    });
    history.push(ChatMessage::assistant(assistant_tool_call.to_string()));
    history.push(ChatMessage::tool(
        json!({
            "tool_call_id": tool_call_id,
            "tool_name": "shell",
            "content": if raw_output.trim().is_empty() {
                "(no output)".to_string()
            } else {
                raw_output.clone()
            },
        })
        .to_string(),
    ));

    let final_response = summarize_direct_shell_command(command, &raw_output, tool_result.success);
    history.push(ChatMessage::assistant(&final_response));
    Ok(final_response)
}

fn push_direct_tool_history(
    history: &mut Vec<ChatMessage>,
    tool_name: &str,
    arguments: serde_json::Value,
    raw_output: &str,
) {
    let tool_call_id = format!("ws_{}_{}", tool_name, Uuid::new_v4());
    let assistant_tool_call = json!({
        "content": serde_json::Value::Null,
        "tool_calls": [{
            "id": tool_call_id,
            "name": tool_name,
            "arguments": serde_json::to_string(&arguments).unwrap_or_else(|_| "{}".to_string()),
        }],
    });
    history.push(ChatMessage::assistant(assistant_tool_call.to_string()));
    history.push(ChatMessage::tool(
        json!({
            "tool_call_id": tool_call_id,
            "tool_name": tool_name,
            "content": if raw_output.trim().is_empty() {
                "(no output)".to_string()
            } else {
                raw_output.to_string()
            },
        })
        .to_string(),
    ));
}

async fn execute_direct_workspace_delete(
    socket: &mut WsSink,
    session_id: &str,
    config: &crate::config::Config,
    history: &mut Vec<ChatMessage>,
    path: &str,
) -> anyhow::Result<String> {
    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolCall {
            name: "workspace_delete".to_string(),
            hint: Some(path.to_string()),
        },
    )
    .await;

    let started_at = Instant::now();
    let result = super::api::delete_workspace_path(config, Some(path)).await;
    let (success, raw_output) = match result {
        Ok(payload) => (
            true,
            format!("Deleted workspace {} `{}`.", payload.kind, payload.path),
        ),
        Err(error) => (false, error),
    };

    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolResult {
            name: "workspace_delete".to_string(),
            success,
            duration_secs: Some(started_at.elapsed().as_secs()),
            output: raw_output.clone(),
        },
    )
    .await;

    push_direct_tool_history(
        history,
        "workspace_delete",
        json!({ "path": path }),
        &raw_output,
    );

    let final_response = if success {
        format!("Deleted `{path}` from the workspace.")
    } else {
        format!("Failed to delete `{path}` from the workspace. Raw tool output is shown above.")
    };
    history.push(ChatMessage::assistant(&final_response));
    Ok(final_response)
}

async fn execute_direct_workspace_directory_create(
    socket: &mut WsSink,
    session_id: &str,
    config: &crate::config::Config,
    history: &mut Vec<ChatMessage>,
    path: &str,
) -> anyhow::Result<String> {
    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolCall {
            name: "workspace_mkdir".to_string(),
            hint: Some(path.to_string()),
        },
    )
    .await;

    let started_at = Instant::now();
    let result = super::api::create_workspace_directory(config, Some(path)).await;
    let (success, raw_output) = match result {
        Ok(payload) => (
            true,
            format!("Created workspace directory `{}`.", payload.path),
        ),
        Err(error) => (false, error),
    };

    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolResult {
            name: "workspace_mkdir".to_string(),
            success,
            duration_secs: Some(started_at.elapsed().as_secs()),
            output: raw_output.clone(),
        },
    )
    .await;

    push_direct_tool_history(
        history,
        "workspace_mkdir",
        json!({ "path": path }),
        &raw_output,
    );

    let final_response = if success {
        format!("Created workspace directory `{path}`.")
    } else {
        format!("Failed to create workspace directory `{path}`. Raw tool output is shown above.")
    };
    history.push(ChatMessage::assistant(&final_response));
    Ok(final_response)
}

async fn execute_direct_file_read(
    socket: &mut WsSink,
    session_id: &str,
    runtime: &GatewayRuntimeSnapshot,
    history: &mut Vec<ChatMessage>,
    path: &str,
) -> anyhow::Result<String> {
    let Some(file_read_tool) = runtime
        .tools_registry_exec
        .iter()
        .find(|tool| tool.name() == "file_read")
    else {
        anyhow::bail!("file_read tool is not available in this runtime");
    };

    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolCall {
            name: "file_read".to_string(),
            hint: Some(path.to_string()),
        },
    )
    .await;

    let started_at = Instant::now();
    let tool_result = file_read_tool
        .execute(json!({ "path": path }))
        .await
        .map_err(|error| anyhow::anyhow!("file_read execution failed: {error}"))?;

    let raw_output = if tool_result.output.trim().is_empty() {
        tool_result.error.clone().unwrap_or_default()
    } else {
        tool_result.output.clone()
    };

    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolResult {
            name: "file_read".to_string(),
            success: tool_result.success,
            duration_secs: Some(started_at.elapsed().as_secs()),
            output: if raw_output.trim().is_empty() {
                "(no output)".to_string()
            } else {
                raw_output.clone()
            },
        },
    )
    .await;

    let tool_call_id = format!("ws_file_read_{}", Uuid::new_v4());
    let assistant_tool_call = json!({
        "content": serde_json::Value::Null,
        "tool_calls": [{
            "id": tool_call_id,
            "name": "file_read",
            "arguments": serde_json::to_string(&json!({ "path": path }))
                .unwrap_or_else(|_| "{}".to_string()),
        }],
    });
    history.push(ChatMessage::assistant(assistant_tool_call.to_string()));
    history.push(ChatMessage::tool(
        json!({
            "tool_call_id": tool_call_id,
            "tool_name": "file_read",
            "content": if raw_output.trim().is_empty() {
                "(no output)".to_string()
            } else {
                raw_output.clone()
            },
        })
        .to_string(),
    ));

    let final_response = summarize_direct_file_read(path, &raw_output, tool_result.success);
    history.push(ChatMessage::assistant(&final_response));
    Ok(final_response)
}

async fn execute_direct_ollama_model(
    socket: &mut WsSink,
    session_id: &str,
    runtime: &GatewayRuntimeSnapshot,
    history: &mut Vec<ChatMessage>,
    request: &DirectOllamaModelRequest,
) -> anyhow::Result<String> {
    let Some(ollama_tool) = runtime
        .tools_registry_exec
        .iter()
        .find(|tool| tool.name() == "ollama_model")
    else {
        anyhow::bail!("ollama_model tool is not available in this runtime");
    };

    let hint = match request.name.as_deref() {
        Some(name) => format!("{} {}", request.action, name),
        None => request.action.clone(),
    };
    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolCall {
            name: "ollama_model".to_string(),
            hint: Some(hint),
        },
    )
    .await;

    let started_at = Instant::now();
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        "action".to_string(),
        serde_json::Value::String(request.action.clone()),
    );
    if let Some(name) = request.name.as_deref() {
        arguments.insert(
            "name".to_string(),
            serde_json::Value::String(name.to_string()),
        );
    }
    let arguments_value = serde_json::Value::Object(arguments);

    let tool_result = ollama_tool
        .execute(arguments_value.clone())
        .await
        .map_err(|error| anyhow::anyhow!("ollama_model execution failed: {error}"))?;

    let raw_output = if tool_result.output.trim().is_empty() {
        tool_result.error.clone().unwrap_or_default()
    } else {
        tool_result.output.clone()
    };

    emit_ws_delta_event(
        socket,
        session_id,
        WsDeltaEvent::ToolResult {
            name: "ollama_model".to_string(),
            success: tool_result.success,
            duration_secs: Some(started_at.elapsed().as_secs()),
            output: if raw_output.trim().is_empty() {
                "(no output)".to_string()
            } else {
                raw_output.clone()
            },
        },
    )
    .await;

    let tool_call_id = format!("ws_ollama_model_{}", Uuid::new_v4());
    let assistant_tool_call = json!({
        "content": serde_json::Value::Null,
        "tool_calls": [{
            "id": tool_call_id,
            "name": "ollama_model",
            "arguments": serde_json::to_string(&arguments_value)
                .unwrap_or_else(|_| "{}".to_string()),
        }],
    });
    history.push(ChatMessage::assistant(assistant_tool_call.to_string()));
    history.push(ChatMessage::tool(
        json!({
            "tool_call_id": tool_call_id,
            "tool_name": "ollama_model",
            "content": if raw_output.trim().is_empty() {
                "(no output)".to_string()
            } else {
                raw_output.clone()
            },
        })
        .to_string(),
    ));

    let final_response = summarize_direct_ollama_model(
        &request.action,
        request.name.as_deref(),
        &raw_output,
        tool_result.success,
    );
    history.push(ChatMessage::assistant(&final_response));
    Ok(final_response)
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
                "Unauthorized — provide Authorization: Bearer <token> or Sec-WebSocket-Protocol: llamafarm.v1, bearer.<token>",
            )
                .into_response();
        }
    }

    ws.protocols([WS_CHAT_SUBPROTOCOL])
        .on_upgrade(move |socket| handle_socket(socket, state))
        .into_response()
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    // Keep the halves separate so an in-flight agent turn can be interrupted
    // without waiting for a model stream or tool call to return first.
    let (mut socket, mut socket_rx) = socket.split();
    let mut queued_inbound: Option<String> = None;

    loop {
        let msg = if let Some(queued) = queued_inbound.take() {
            queued
        } else {
            match socket_rx.next().await {
                Some(Ok(Message::Text(text))) => text.to_string(),
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                _ => continue,
            }
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

        let runtime = state.runtime_snapshot();
        let msg_type = parsed["type"].as_str().unwrap_or("").to_string();
        let session_id =
            normalize_ws_session_id(parsed.get("session_id").and_then(serde_json::Value::as_str))
                .unwrap_or_else(|| "default".to_string());
        let selected_federation_peer_ids =
            parse_selected_federation_peer_ids(parsed.get("federation_peer_ids"));
        let (federation_event_tx, mut federation_event_rx) =
            tokio::sync::mpsc::unbounded_channel::<FederationChatEvent>();
        let turn_cancellation = CancellationToken::new();
        let federation_context = state.federation.as_ref().map(|_| FederationChatContext {
            session_id: session_id.clone(),
            selected_peer_ids: selected_federation_peer_ids,
            event_tx: Some(federation_event_tx),
            cancellation: Some(turn_cancellation.clone()),
        });
        let mut followup_message: Option<String> = None;

        with_chat_context(federation_context, async {
            // Warm per-model capability cache before taking the config lock so the
            // MutexGuard isn't held across an await point (parking_lot guards are !Send).
            runtime
                .provider
                .prefetch_model_capabilities(&runtime.model)
                .await;

            let (
                provider_label,
                parallel_tools,
                native_tools,
                approval_manager,
                system_prompt,
                max_history_messages,
                ws_chat_store_path,
            ) = {
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
                let skills = crate::skills::load_skills_with_config(
                    &config_guard.workspace_dir,
                    &config_guard,
                );
                let bootstrap_max_chars = if config_guard.agent.compact_context {
                    Some(6000)
                } else {
                    None
                };
                let native_tools = crate::agent::loop_::configured_native_tools_enabled(
                    &config_guard.agent.tool_dispatcher,
                    &provider_label,
                    &runtime.model,
                    runtime.provider.supports_native_tools(),
                ) || runtime
                    .provider
                    .cached_model_tool_support(&runtime.model)
                    .unwrap_or(false);
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
                system_prompt.push_str(&crate::agent::loop_::build_auto_plan_execute_instructions());

                if let Some(federation) = &state.federation {
                    let remote_agents = federation.remote_adapter().available_remote_agents_info();
                    if !remote_agents.is_empty() {
                        system_prompt.push_str(
                            &crate::agent::loop_::build_federation_delegation_instructions(
                                &remote_agents,
                            ),
                        );
                    }
                }

                (
                    provider_label,
                    config_guard.agent.parallel_tools,
                    native_tools,
                    ApprovalManager::from_config(&config_guard.autonomy),
                    system_prompt,
                    config_guard.agent.max_history_messages,
                    resolve_ws_chat_store_path(&config_guard),
                )
            };

            if msg_type == "session_delete" {
                delete_ws_chat_history(&session_id, &ws_chat_store_path).await;
                return;
            }

            if msg_type == "cancel" {
                // A cancellation frame is normally consumed by the in-flight
                // turn's select loop. If it reached this idle path there is no
                // run left to interrupt, but still acknowledge it explicitly.
                let cancelled = json!({
                    "type": "cancelled",
                    "session_id": session_id,
                    "message": "No active run to stop.",
                });
                let _ = socket.send(Message::Text(cancelled.to_string().into())).await;
                return;
            }

            if msg_type != "message" {
                return;
            }

            let content = parsed["content"].as_str().unwrap_or("").to_string();
            if content.is_empty() {
                return;
            }

            let direct_intent = classify_direct_intent(&content);
            let temporary = parsed["temporary"].as_bool().unwrap_or(false);
            let history_seed = parse_seed_history(parsed.get("history_seed"));
            let mut history = load_ws_chat_history(
                &session_id,
                temporary,
                &history_seed,
                &ws_chat_store_path,
            )
            .await;
            let mut effective_system_prompt = match direct_intent.as_ref() {
                Some(DirectIntent::ForceTool(DirectForcedToolIntent::FileWrite(request))) => {
                    build_forced_file_write_prompt(&system_prompt, request)
                }
                _ => system_prompt.clone(),
            };

            // Cross-session recall: retrieve memories relevant to this message
            // from ALL prior sessions (semantic when the Qdrant backend is
            // configured) and inject them as cited context for this turn.
            if direct_intent.is_none() {
                let min_relevance = { state.config.lock().memory.min_relevance_score };
                let memory_context = crate::channels::build_memory_context(
                    runtime.mem.as_ref(),
                    &content,
                    min_relevance,
                )
                .await;
                if !memory_context.is_empty() {
                    effective_system_prompt.push_str(
                        "\n\n## Recalled memory (prior sessions)\n\
                         Relevant records from earlier conversations on this node. \
                         Treat them as real history, cite them when used, and prefer \
                         current-turn tool evidence when they conflict:\n",
                    );
                    effective_system_prompt.push_str(&memory_context);
                }
            }

            if let Some(first) = history.first_mut() {
                if first.role == "system" {
                    *first = ChatMessage::system(&effective_system_prompt);
                } else {
                    history.insert(0, ChatMessage::system(&effective_system_prompt));
                }
            } else {
                history.push(ChatMessage::system(&effective_system_prompt));
            }

            let history_before_turn = history.clone();

            if state.auto_save && content.chars().count() >= WS_AUTOSAVE_MIN_MESSAGE_CHARS {
                let key = websocket_memory_key();
                let _ = runtime
                    .mem
                    .store(
                        &key,
                        &content,
                        MemoryCategory::Conversation,
                        Some(session_id.as_str()),
                    )
                    .await;
            }

            let already_present = history
                .iter()
                .rev()
                .find(|m| m.role != "system")
                .is_some_and(|m| m.role == "user" && m.content == content);
            if !already_present {
                history.push(ChatMessage::user(&content));
            }

            let _ = state.event_tx.send(serde_json::json!({
                "type": "agent_start",
                "provider": provider_label,
                "model": runtime.model,
            }));

            if let Some(DirectIntent::ExecuteNow(intent)) = direct_intent.as_ref() {
                let result = match intent {
                    DirectExecutionIntent::FileRead(path) => {
                        execute_direct_file_read(
                            &mut socket,
                            &session_id,
                            &runtime,
                            &mut history,
                            path,
                        )
                        .await
                    }
                    DirectExecutionIntent::WorkspaceDelete(path) => {
                        let config = state.config.lock().clone();
                        execute_direct_workspace_delete(
                            &mut socket,
                            &session_id,
                            &config,
                            &mut history,
                            path,
                        )
                        .await
                    }
                    DirectExecutionIntent::WorkspaceCreateDirectory(path) => {
                        let config = state.config.lock().clone();
                        execute_direct_workspace_directory_create(
                            &mut socket,
                            &session_id,
                            &config,
                            &mut history,
                            path,
                        )
                        .await
                    }
                    DirectExecutionIntent::OllamaModel(request) => {
                        execute_direct_ollama_model(
                            &mut socket,
                            &session_id,
                            &runtime,
                            &mut history,
                            request,
                        )
                        .await
                    }
                    DirectExecutionIntent::Shell(command) => {
                        execute_direct_shell_command(
                            &mut socket,
                            &session_id,
                            &runtime,
                            &mut history,
                            command,
                        )
                        .await
                    }
                };

                match result {
                    Ok(response) => {
                        store_ws_chat_history(
                            &session_id,
                            &history,
                            temporary,
                            &ws_chat_store_path,
                        )
                        .await;
                        let done = serde_json::json!({
                            "type": "done",
                            "session_id": session_id,
                            "full_response": response,
                        });
                        let _ = socket.send(Message::Text(done.to_string().into())).await;

                        let _ = state.event_tx.send(serde_json::json!({
                            "type": "agent_end",
                            "provider": provider_label,
                            "model": runtime.model,
                        }));
                    }
                    Err(error) => {
                        store_ws_chat_history(
                            &session_id,
                            &history_before_turn,
                            temporary,
                            &ws_chat_store_path,
                        )
                        .await;
                        let sanitized = crate::providers::sanitize_api_error(&error.to_string());
                        let err = serde_json::json!({
                            "type": "error",
                            "session_id": session_id,
                            "message": sanitized,
                        });
                        let _ = socket.send(Message::Text(err.to_string().into())).await;

                        let _ = state.event_tx.send(serde_json::json!({
                            "type": "error",
                            "component": "ws_chat",
                            "message": sanitized,
                        }));
                    }
                }

                return;
            }

            let excluded_tools: Vec<String> = match direct_intent {
                Some(DirectIntent::ForceTool(DirectForcedToolIntent::FileWrite(_))) => runtime
                    .tools_registry_exec
                    .iter()
                    .map(|tool| tool.name().to_string())
                    .filter(|name| !matches!(name.as_str(), "file_write" | "task_plan"))
                    .collect(),
                _ => Vec::new(),
            };

            // Durable run ledger for the inspector: one ledger per chat
            // session, appended across turns, keyed by the session id.
            let turn_run_ledger = {
                let workspace_dir = state.config.lock().workspace_dir.clone();
                crate::agent::run_ledger::RunLedger::open_or_create(
                    &workspace_dir,
                    &format!("session-{session_id}"),
                    Some(&session_id),
                    "webchat",
                    &provider_label,
                    &runtime.model,
                    "chat",
                )
                .map_err(|e| tracing::warn!("Could not open session run ledger: {e}"))
                .ok()
            };

            let mut delete_session_after_cancel = false;
            let result = crate::agent::loop_::with_tool_loop_settings(
                parallel_tools,
                native_tools,
                crate::agent::loop_::with_tool_loop_history_limit(
                    max_history_messages,
                    async {
                        // A fresh token is scoped to this browser turn. The read
                        // half of the WebSocket remains live below, so a `cancel`
                        // frame can interrupt provider streaming, tool execution,
                        // and the next tool-loop iteration immediately.
                        let cancellation_token = turn_cancellation.clone();
                        let mut cancellation_requested = false;
                        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(128);
                        let mut loop_future =
                            std::pin::pin!(crate::agent::run_ledger::RUN_LEDGER.scope(
                                turn_run_ledger.clone(),
                                run_tool_call_loop(
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
                                    Some(cancellation_token.clone()),
                                    Some(delta_tx),
                                    None,
                                    &excluded_tools,
                                )
                            ));

                        loop {
                            tokio::select! {
                                federation_event = federation_event_rx.recv() => {
                                    if let Some(federation_event) = federation_event {
                                        emit_ws_federation_event(&mut socket, federation_event).await;
                                    }
                                }
                                inbound = socket_rx.next() => {
                                    match inbound {
                                        Some(Ok(Message::Text(text))) => {
                                            match parse_inflight_ws_control(&text) {
                                                InFlightWsControl::Cancel { session_id: control_session_id }
                                                    if control_session_id == session_id => {
                                                        if !cancellation_requested {
                                                            cancellation_requested = true;
                                                            cancellation_token.cancel();
                                                            let payload = json!({
                                                                "type": "cancelling",
                                                                "session_id": session_id,
                                                                "message": "Stopping the active run…",
                                                            });
                                                            let _ = socket.send(Message::Text(payload.to_string().into())).await;
                                                        }
                                                    }
                                                InFlightWsControl::SessionDelete { session_id: control_session_id }
                                                    if control_session_id == session_id => {
                                                        // Deleting an active chat is also a stop request. The
                                                        // deletion is completed after the cancelled turn reaches a
                                                        // clean terminal state, so it cannot be re-persisted below.
                                                        delete_session_after_cancel = true;
                                                        if !cancellation_requested {
                                                            cancellation_requested = true;
                                                            cancellation_token.cancel();
                                                        }
                                                    }
                                                InFlightWsControl::Message { session_id: control_session_id }
                                                    if control_session_id == session_id => {
                                                    // A mid-run message is an intentional follow-up. Queue the
                                                    // exact frame, checkpoint completed evidence through normal
                                                    // cancellation, then immediately start it as the next turn.
                                                    // This lets operators redirect long autonomous work without
                                                    // losing verified tool results or waiting for a generation
                                                    // segment to finish.
                                                    followup_message = Some(text.to_string());
                                                    if !cancellation_requested {
                                                        cancellation_requested = true;
                                                        cancellation_token.cancel();
                                                    }
                                                    let payload = json!({
                                                        "type": "followup_queued",
                                                        "session_id": control_session_id,
                                                        "message": "Follow-up queued; checkpointing the active run first.",
                                                    });
                                                    let _ = socket.send(Message::Text(payload.to_string().into())).await;
                                                }
                                                InFlightWsControl::InvalidJson => {
                                                    let payload = json!({"type": "error", "message": "Invalid JSON"});
                                                    let _ = socket.send(Message::Text(payload.to_string().into())).await;
                                                }
                                                InFlightWsControl::Cancel { .. }
                                                | InFlightWsControl::SessionDelete { .. }
                                                | InFlightWsControl::Message { .. }
                                                | InFlightWsControl::Other => {}
                                            }
                                        }
                                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                                            // The browser disconnected; dropping the active future would
                                            // stop it eventually, but signal it first so model/tool futures
                                            // take their own cancellation-aware paths.
                                            cancellation_token.cancel();
                                            break loop_future.await;
                                        }
                                        _ => {}
                                    }
                                }
                                maybe_delta = delta_rx.recv() => {
                                    if let Some(delta) = maybe_delta {
                                        if let Some(event) = parse_ws_delta_event(&delta) {
                                            emit_ws_delta_event(&mut socket, &session_id, event).await;
                                        }
                                    } else {
                                        break loop_future.await;
                                    }
                                }
                                response = &mut loop_future => {
                                    while let Ok(federation_event) = federation_event_rx.try_recv() {
                                        emit_ws_federation_event(&mut socket, federation_event).await;
                                    }
                                    while let Ok(delta) = delta_rx.try_recv() {
                                        if let Some(event) = parse_ws_delta_event(&delta) {
                                            emit_ws_delta_event(&mut socket, &session_id, event).await;
                                        }
                                    }
                                    break response;
                                }
                            }
                        }
                    },
                ),
            )
            .await;

            while let Ok(federation_event) = federation_event_rx.try_recv() {
                emit_ws_federation_event(&mut socket, federation_event).await;
            }

            if let Some(ref ledger) = turn_run_ledger {
                let status = match &result {
                    Ok(_) => crate::agent::run_ledger::RunStatus::Completed,
                    Err(e) if crate::agent::loop_::is_tool_loop_cancelled(e) => {
                        crate::agent::run_ledger::RunStatus::Cancelled
                    }
                    Err(_) => crate::agent::run_ledger::RunStatus::Failed,
                };
                ledger.finalize(status);
            }

            match result {
                Ok(response) => {
                    let safe_response = finalize_ws_response(
                        &response,
                        &history,
                        runtime.tools_registry_exec.as_ref(),
                    );
                    history.push(ChatMessage::assistant(&safe_response));

                    // Persist a compact exchange record so future sessions can
                    // recall what was asked and answered, not just the prompt.
                    if state.auto_save
                        && !temporary
                        && content.chars().count() >= WS_AUTOSAVE_MIN_MESSAGE_CHARS
                    {
                        let exchange = format!(
                            "Q: {}\nA: {}",
                            crate::util::truncate_with_ellipsis(&content, 300),
                            crate::util::truncate_with_ellipsis(&safe_response, 600),
                        );
                        let _ = runtime
                            .mem
                            .store(
                                &format!("{}-exchange", websocket_memory_key()),
                                &exchange,
                                MemoryCategory::Conversation,
                                Some(session_id.as_str()),
                            )
                            .await;
                    }
                    store_ws_chat_history(
                        &session_id,
                        &history,
                        temporary,
                        &ws_chat_store_path,
                    )
                    .await;

                    let done = serde_json::json!({
                        "type": "done",
                        "session_id": session_id,
                        "full_response": safe_response,
                    });
                    let _ = socket.send(Message::Text(done.to_string().into())).await;

                    let _ = state.event_tx.send(serde_json::json!({
                        "type": "agent_end",
                        "provider": provider_label,
                        "model": runtime.model,
                    }));
                }
                Err(error) if crate::agent::loop_::is_tool_loop_cancelled(&error) => {
                    // Keep the user's prompt plus any *completed* tool traces so a
                    // follow-up can build on verified work. Do not manufacture an
                    // assistant final response for a turn that was intentionally
                    // stopped halfway through.
                    if delete_session_after_cancel {
                        delete_ws_chat_history(&session_id, &ws_chat_store_path).await;
                    } else {
                        store_ws_chat_history(
                            &session_id,
                            &history,
                            temporary,
                            &ws_chat_store_path,
                        )
                        .await;
                    }

                    let cancelled = serde_json::json!({
                        "type": "cancelled",
                        "session_id": session_id,
                        "message": WS_CANCELLED_MESSAGE,
                    });
                    let _ = socket.send(Message::Text(cancelled.to_string().into())).await;

                    let _ = state.event_tx.send(serde_json::json!({
                        "type": "agent_cancelled",
                        "provider": provider_label,
                        "model": runtime.model,
                        "session_id": session_id,
                    }));
                }
                Err(error) => {
                    store_ws_chat_history(
                        &session_id,
                        &history_before_turn,
                        temporary,
                        &ws_chat_store_path,
                    )
                    .await;
                    let sanitized = crate::providers::sanitize_api_error(&error.to_string());
                    let err = serde_json::json!({
                        "type": "error",
                        "session_id": session_id,
                        "message": sanitized,
                    });
                    let _ = socket.send(Message::Text(err.to_string().into())).await;

                    let _ = state.event_tx.send(serde_json::json!({
                        "type": "error",
                        "component": "ws_chat",
                        "message": sanitized,
                    }));
                }
            }
        })
        .await;
        if let Some(followup) = followup_message.take() {
            queued_inbound = Some(followup);
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
    use tempfile::tempdir;

    #[test]
    fn extract_ws_bearer_token_prefers_authorization_header() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer from-auth-header"),
        );
        headers.insert(
            header::SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("llamafarm.v1, bearer.from-protocol"),
        );

        assert_eq!(
            extract_ws_bearer_token(&headers).as_deref(),
            Some("from-auth-header")
        );
    }

    #[test]
    fn parse_inflight_ws_control_recognizes_scoped_stop_frames() {
        assert_eq!(
            parse_inflight_ws_control(r#"{"type":"cancel","session_id":"chat-a"}"#),
            InFlightWsControl::Cancel {
                session_id: "chat-a".to_string(),
            }
        );
        assert_eq!(
            parse_inflight_ws_control(r#"{"type":"cancel","session_id":"   "}"#),
            InFlightWsControl::Cancel {
                session_id: "default".to_string(),
            }
        );
    }

    #[test]
    fn parse_inflight_ws_control_does_not_turn_other_frames_into_stops() {
        assert_eq!(
            parse_inflight_ws_control(r#"{"type":"message","session_id":"chat-a"}"#),
            InFlightWsControl::Message {
                session_id: "chat-a".to_string(),
            }
        );
        assert_eq!(
            parse_inflight_ws_control(r#"{"type":"session_delete","session_id":"chat-a"}"#),
            InFlightWsControl::SessionDelete {
                session_id: "chat-a".to_string(),
            }
        );
        assert_eq!(
            parse_inflight_ws_control(r#"{"type":"ping"}"#),
            InFlightWsControl::Other
        );
        assert_eq!(
            parse_inflight_ws_control("not-json"),
            InFlightWsControl::InvalidJson
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
        let delta = format!("{DRAFT_PROGRESS_SENTINEL}✅ shell (2s)\nfile_a\nfile_b\n");
        assert_eq!(
            parse_ws_delta_event(&delta),
            Some(WsDeltaEvent::ToolResult {
                name: "shell".to_string(),
                success: true,
                duration_secs: Some(2),
                output: "file_a\nfile_b".to_string(),
            })
        );
    }

    #[test]
    fn parse_ws_delta_event_maps_tool_failure_without_output_to_placeholder() {
        let delta = format!("{DRAFT_PROGRESS_SENTINEL}❌ shell (0s)\n");
        assert_eq!(
            parse_ws_delta_event(&delta),
            Some(WsDeltaEvent::ToolResult {
                name: "shell".to_string(),
                success: false,
                duration_secs: Some(0),
                output: "(no output)".to_string(),
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
            HeaderValue::from_static("llamafarm.v1, bearer.protocol-token"),
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
            HeaderValue::from_static("llamafarm.v1"),
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
            HeaderValue::from_static("llamafarm.v1, bearer."),
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

    #[test]
    fn build_ws_resume_context_surfaces_latest_tool_output() {
        let history = vec![
            ChatMessage::user("lsusb"),
            ChatMessage::assistant(
                r#"{"content":null,"tool_calls":[{"id":"call_1","name":"shell","arguments":"{\"command\":\"lsusb\"}"}]}"#,
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"call_1","tool_name":"shell","content":"Bus 001 Device 001"}"#,
            ),
            ChatMessage::assistant("Command completed successfully."),
        ];

        let resume = build_ws_resume_context(&history).unwrap();
        assert!(resume.contains("Previous completed command before this turn: `lsusb`"));
        assert!(resume.contains("Latest tool output before this turn: Bus 001 Device 001"));
        assert!(resume.contains("Do not claim the conversation has no prior context"));
        assert!(
            resume.contains("Do not re-run tools solely because they appear in this saved context")
        );
    }

    #[test]
    fn parse_seed_history_accepts_tool_messages() {
        let seed = serde_json::json!([
            { "role": "user", "content": "write and run add.py" },
            {
                "role": "assistant",
                "content": "{\"content\":null,\"tool_calls\":[{\"id\":\"call_1\",\"name\":\"shell\",\"arguments\":\"{\\\"command\\\":\\\"python add.py\\\"}\"}]}"
            },
            {
                "role": "tool",
                "content": "{\"tool_call_id\":\"call_1\",\"tool_name\":\"shell\",\"content\":\"2 + 2 = 4\"}"
            }
        ]);

        let parsed = parse_seed_history(Some(&seed));
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].role, "user");
        assert_eq!(parsed[1].role, "assistant");
        assert_eq!(parsed[2].role, "tool");
        assert!(parsed[2].content.contains("\"tool_name\":\"shell\""));
    }

    #[test]
    fn normalize_ws_history_for_storage_drops_system_messages() {
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("write add.py"),
            ChatMessage::assistant("done"),
            ChatMessage::tool(r#"{"tool_call_id":"call_1","content":"2 + 2 = 4"}"#),
        ];

        let normalized = normalize_ws_history_for_storage(&history);
        assert_eq!(normalized.len(), 3);
        assert!(normalized.iter().all(|message| message.role != "system"));
    }

    #[test]
    fn normalize_ws_history_for_storage_collapses_completed_tool_trace_to_summary() {
        let history = vec![
            ChatMessage::user("run lsusb"),
            ChatMessage::assistant(
                r#"{"content":null,"tool_calls":[{"id":"call_1","name":"shell","arguments":"{\"command\":\"lsusb\"}"}]}"#,
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"call_1","tool_name":"shell","content":"Bus 001 Device 001"}"#,
            ),
            ChatMessage::assistant("Command completed successfully."),
            ChatMessage::user("write a python file to add two numbers"),
        ];

        let normalized = normalize_ws_history_for_storage(&history);
        assert_eq!(normalized.len(), 3);
        assert_eq!(normalized[0].role, "user");
        assert_eq!(normalized[0].content, "run lsusb");
        assert_eq!(normalized[1].role, "assistant");
        assert_eq!(normalized[1].content, "Command completed successfully.");
        assert_eq!(normalized[2].role, "user");
        assert_eq!(
            normalized[2].content,
            "write a python file to add two numbers"
        );
    }

    #[test]
    fn normalize_ws_history_for_storage_drops_internal_resume_scaffolding() {
        let history = vec![
            ChatMessage::user("build the app"),
            ChatMessage::user(
                "Internal correction: that search or action was already completed earlier in this turn.",
            ),
            ChatMessage::assistant(
                "[Saved chat context restored]\nYou are resuming an existing saved local chat.",
            ),
            ChatMessage::assistant(
                "Internal continuation: task plan created (4 steps). Begin execution NOW.",
            ),
            ChatMessage::assistant("Working on it."),
        ];

        let normalized = normalize_ws_history_for_storage(&history);
        assert_eq!(normalized.len(), 2);
        assert_eq!(normalized[0].role, "user");
        assert_eq!(normalized[0].content, "build the app");
        assert_eq!(normalized[1].role, "assistant");
        assert_eq!(normalized[1].content, "Working on it.");
    }

    #[test]
    fn select_resume_history_prefers_seed_when_lengths_match() {
        let seed = vec![ChatMessage::user("latest seed prompt")];
        let persisted = vec![ChatMessage::user("older persisted prompt")];

        let selected = select_resume_history(&seed, &persisted);
        assert_eq!(selected, seed);
    }

    #[tokio::test]
    async fn persisted_ws_chat_sessions_round_trip_without_system_prompt() {
        let dir = tempdir().unwrap();
        let store_path = dir.path().join("state").join("web-chat-sessions.json");
        let history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("write and run add.py"),
            ChatMessage::assistant(
                r#"{"content":null,"tool_calls":[{"id":"call_1","name":"shell","arguments":"{\"command\":\"python add.py\"}"}]}"#,
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"call_1","tool_name":"shell","content":"2 + 2 = 4"}"#,
            ),
            ChatMessage::assistant("2 + 2 = 4"),
        ];

        persist_ws_chat_session(&store_path, "session-a", &history).await;

        let persisted = read_persisted_ws_chat_sessions(&store_path).await;
        let session = persisted.sessions.get("session-a").unwrap();
        assert_eq!(session.history.len(), 2);
        assert!(
            session
                .history
                .iter()
                .all(|message| message.role != "system")
        );
        assert_eq!(session.history[0].content, "write and run add.py");
        assert_eq!(session.history.last().unwrap().content, "2 + 2 = 4");

        delete_persisted_ws_chat_session(&store_path, "session-a").await;
        let after_delete = read_persisted_ws_chat_sessions(&store_path).await;
        assert!(!after_delete.sessions.contains_key("session-a"));
    }

    #[test]
    fn build_restored_ws_chat_history_collapses_raw_tool_trace_into_summary() {
        let history = vec![
            ChatMessage::user("lsusb"),
            ChatMessage::assistant(
                r#"{"content":null,"tool_calls":[{"id":"call_1","name":"shell","arguments":"{\"command\":\"lsusb\"}"}]}"#,
            ),
            ChatMessage::tool(
                r#"{"tool_call_id":"call_1","tool_name":"shell","content":"Bus 001 Device 001"}"#,
            ),
            ChatMessage::assistant("Command completed successfully."),
        ];

        let restored = build_restored_ws_chat_history(&history);
        assert_eq!(restored.len(), 1);
        assert_eq!(restored[0].role, "assistant");
        assert!(restored[0].content.starts_with(WS_RESTORED_CONTEXT_PREFIX));
        assert!(
            restored[0]
                .content
                .contains("Previous completed command before this turn: `lsusb`")
        );
        assert!(restored[0].content.contains("Bus 001 Device 001"));
    }

    #[test]
    fn build_restored_ws_chat_history_ignores_internal_user_followups() {
        let history = vec![
            ChatMessage::user("continue the physics engine work"),
            ChatMessage::user(
                "Internal correction: that search or action was already completed earlier in this turn.",
            ),
            ChatMessage::assistant("Good progress! Let me continue building."),
        ];

        let restored = build_restored_ws_chat_history(&history);
        assert_eq!(restored.len(), 1);
        assert!(
            restored[0]
                .content
                .contains("Latest user request before this turn: continue the physics engine work")
        );
        assert!(!restored[0].content.contains("Internal correction:"));
    }

    #[test]
    fn extract_direct_shell_command_accepts_plain_command_input() {
        assert_eq!(
            extract_direct_shell_command("curl -s https://example.com"),
            Some("curl -s https://example.com".to_string())
        );
        assert_eq!(
            extract_direct_shell_command("lsusb"),
            Some("lsusb".to_string())
        );
    }

    #[test]
    fn extract_direct_shell_command_accepts_prefixed_command_input() {
        assert_eq!(
            extract_direct_shell_command("Run this exact command: `lsblk`"),
            Some("lsblk".to_string())
        );
        assert_eq!(
            extract_direct_shell_command("run lsusb"),
            Some("lsusb".to_string())
        );
        assert_eq!(
            extract_direct_shell_command("execute lspci"),
            Some("lspci".to_string())
        );
    }

    #[test]
    fn extract_direct_shell_command_rejects_command_plus_explanation_suffix() {
        assert_eq!(
            extract_direct_shell_command("run lsusb and explain all the results"),
            None
        );
        assert_eq!(extract_direct_shell_command("lsblk then summarize"), None);
    }

    #[test]
    fn extract_direct_shell_command_rejects_normal_chat_text() {
        assert_eq!(extract_direct_shell_command("How are you today?"), None);
        assert_eq!(
            extract_direct_shell_command("Please explain what curl does."),
            None
        );
    }

    #[test]
    fn describe_direct_shell_result_failure_is_not_misleading() {
        let summary = describe_direct_shell_result(
            "lsusb and explain all the results",
            "Usage: lsusb [options]...",
            false,
        );
        assert!(summary.contains("Command failed"));
        assert!(summary.contains("non-zero exit status"));
    }

    #[test]
    fn describe_direct_shell_result_is_generic_and_non_hardcoded() {
        let output = "Bus 001 Device 001: ID 1d6b:0002 Linux Foundation 2.0 root hub\n\
Bus 003 Device 003: ID 04f2:b809 Chicony Electronics Co., Ltd HP True Vision FHD Camera\n\
Bus 003 Device 004: ID 8087:0033 Intel Corp.";
        let summary = describe_direct_shell_result("lsusb", output, true);
        assert!(summary.contains("Command completed successfully"));
        assert!(summary.contains("Executed: `lsusb`"));
        assert!(!summary.contains("This lists USB devices"));
    }

    #[test]
    fn extract_direct_directory_listing_command_recovers_workspace_and_paths() {
        assert_eq!(
            extract_direct_directory_listing_command("what files are in the workspace"),
            Some("ls -la".to_string())
        );
        assert_eq!(
            extract_direct_directory_listing_command("ls files in rust_kernel/src"),
            Some("ls -la 'rust_kernel/src'".to_string())
        );
        assert_eq!(
            extract_direct_directory_listing_command("ls all files in the rust_kernel directory"),
            Some("ls -la 'rust_kernel'".to_string())
        );
    }

    #[test]
    fn extract_direct_file_read_path_recovers_explicit_file_requests() {
        assert_eq!(
            extract_direct_file_read_path("read rust_kernel/src/boot/boot.S and show its contents"),
            Some("rust_kernel/src/boot/boot.S".to_string())
        );
        assert_eq!(
            extract_direct_file_read_path("show `AGENTS.md`"),
            Some("AGENTS.md".to_string())
        );
        assert_eq!(
            extract_direct_file_read_path("read all files in rust_kernel directory"),
            None
        );
    }

    #[test]
    fn extract_direct_ollama_model_request_recovers_explicit_commands() {
        assert_eq!(
            extract_direct_ollama_model_request("ollama pull nemotron-3-super"),
            Some(DirectOllamaModelRequest {
                action: "pull".to_string(),
                name: Some("nemotron-3-super".to_string()),
            })
        );
        assert_eq!(
            extract_direct_ollama_model_request("run ollama ps"),
            Some(DirectOllamaModelRequest {
                action: "running".to_string(),
                name: None,
            })
        );
        assert_eq!(
            extract_direct_ollama_model_request("pull qwen2.5-coder:14b"),
            Some(DirectOllamaModelRequest {
                action: "pull".to_string(),
                name: Some("qwen2.5-coder:14b".to_string()),
            })
        );
        assert_eq!(
            extract_direct_ollama_model_request("ollama rm gemma3:1b"),
            Some(DirectOllamaModelRequest {
                action: "delete".to_string(),
                name: Some("gemma3:1b".to_string()),
            })
        );
    }

    #[test]
    fn extract_direct_ollama_model_request_rejects_normal_chat() {
        assert_eq!(
            extract_direct_ollama_model_request("can you pull the latest changes?"),
            None
        );
        assert_eq!(
            extract_direct_ollama_model_request("tell me about ollama pull"),
            None
        );
        assert_eq!(extract_direct_ollama_model_request("delete add.py"), None);
        assert_eq!(extract_direct_ollama_model_request("rm add.py"), None);
    }

    #[test]
    fn classify_direct_intent_routes_workspace_mutations_and_file_write() {
        assert_eq!(
            classify_direct_intent("rm add.py"),
            Some(DirectIntent::ExecuteNow(
                DirectExecutionIntent::WorkspaceDelete("add.py".to_string())
            ))
        );
        assert_eq!(
            classify_direct_intent("create folder demo/subdir"),
            Some(DirectIntent::ExecuteNow(
                DirectExecutionIntent::WorkspaceCreateDirectory("demo/subdir".to_string())
            ))
        );
        assert_eq!(
            classify_direct_intent("write_file add.py to add two numbers"),
            Some(DirectIntent::ForceTool(DirectForcedToolIntent::FileWrite(
                DirectFileWriteRequest {
                    path: "add.py".to_string(),
                    instruction: "add two numbers".to_string(),
                }
            )))
        );
    }

    #[test]
    fn classify_direct_intent_preserves_shell_and_ollama_exact_paths() {
        assert_eq!(
            classify_direct_intent("run this exact command: `python -V`"),
            Some(DirectIntent::ExecuteNow(DirectExecutionIntent::Shell(
                "python -V".to_string()
            )))
        );
        assert_eq!(
            classify_direct_intent("ollama delete gemma3:1b"),
            Some(DirectIntent::ExecuteNow(
                DirectExecutionIntent::OllamaModel(DirectOllamaModelRequest {
                    action: "delete".to_string(),
                    name: Some("gemma3:1b".to_string()),
                })
            ))
        );
        assert_eq!(
            classify_direct_intent("run lsusb and explain all the results"),
            None
        );
    }
}
