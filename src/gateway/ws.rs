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

use super::{AppState, GatewayRuntimeSnapshot};
use crate::agent::loop_::{
    auto_compact_history, run_tool_call_loop, trim_history, DRAFT_CLEAR_SENTINEL,
    DRAFT_PROGRESS_SENTINEL,
};
use crate::approval::ApprovalManager;
use crate::memory::MemoryCategory;
use crate::providers::{ChatMessage, ChatRequest};
use axum::{
    extract::{
        ws::{Message, WebSocket},
        State, WebSocketUpgrade,
    },
    http::{header, HeaderMap},
    response::IntoResponse,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const EMPTY_WS_RESPONSE_FALLBACK: &str =
    "Tool execution completed, but the model returned no final text response. Please ask me to summarize the result.";
const WS_AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;
const WS_CHAT_SUBPROTOCOL: &str = "llamafarm.v1";
const WS_CHAT_STORE_REL_PATH: &str = "state/web-chat-sessions.json";
const WS_PERSISTED_MAX_MESSAGES: usize = 800;
const WS_PERSISTED_MAX_SESSIONS: usize = 64;
const WS_RESTORED_HISTORY_KEEP_MESSAGES: usize = 12;
const WS_RESTORED_CONTEXT_PREFIX: &str = "[Saved chat context restored]";

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
    let mut normalized: Vec<ChatMessage> = history
        .iter()
        .filter(|message| message.role != "system")
        .filter_map(|message| {
            if message.content.trim().is_empty() {
                None
            } else {
                Some(message.clone())
            }
        })
        .collect();

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

fn is_assistant_tool_call_payload(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|value| value.get("tool_calls").cloned())
        .is_some()
}

fn extract_latest_compaction_summary(history: &[ChatMessage]) -> Option<String> {
    history.iter().rev().find_map(|message| {
        if message.role != "assistant" {
            return None;
        }

        let trimmed = message.content.trim();
        trimmed
            .starts_with("[Compaction summary]")
            .then(|| trimmed.to_string())
    })
}

fn convert_tool_message_to_restore_safe_user_message(message: &ChatMessage) -> Option<ChatMessage> {
    let trimmed = message.content.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        let tool_name = value
            .get("tool_name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let content = value
            .get("content")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?;

        let rendered = match tool_name {
            Some(name) => format!("[Tool results]\n<tool_result name=\"{name}\">\n{content}\n</tool_result>"),
            None => format!("[Tool results]\n<tool_result>\n{content}\n</tool_result>"),
        };
        return Some(ChatMessage::user(rendered));
    }

    Some(ChatMessage::user(format!(
        "[Tool results]\n<tool_result>\n{trimmed}\n</tool_result>"
    )))
}

fn restore_safe_chat_message(message: &ChatMessage) -> Option<ChatMessage> {
    let trimmed = message.content.trim();
    if trimmed.is_empty() {
        return None;
    }

    match message.role.as_str() {
        "user" => Some(ChatMessage::user(trimmed)),
        "assistant" => {
            if is_assistant_tool_call_payload(trimmed) {
                None
            } else {
                Some(ChatMessage::assistant(trimmed))
            }
        }
        "tool" => convert_tool_message_to_restore_safe_user_message(message),
        _ => None,
    }
}

fn extract_latest_assistant_reply(history: &[ChatMessage]) -> Option<String> {
    history.iter().rev().find_map(|message| {
        if message.role != "assistant"
            || is_assistant_tool_call_payload(&message.content)
            || is_restored_context_message(&message.content)
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
        if message.role != "user" {
            return None;
        }

        let trimmed = message.content.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(crate::util::truncate_with_ellipsis(trimmed, 500))
        }
    });
    let latest_tool_output =
        extract_latest_tool_output(history).map(|value| crate::util::truncate_with_ellipsis(&value, 700));
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
        sections.push(format!("Latest user request before this turn: {latest_user}"));
    }
    if let Some(latest_tool_output) = latest_tool_output {
        sections.push(format!("Latest tool output before this turn: {latest_tool_output}"));
    }
    if let Some(latest_assistant) = latest_assistant {
        sections.push(format!("Latest assistant reply before this turn: {latest_assistant}"));
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

    let restore_safe: Vec<ChatMessage> = normalized
        .iter()
        .filter_map(restore_safe_chat_message)
        .collect();
    if restore_safe.is_empty() {
        let Some(resume_context) = build_ws_resume_context(&normalized) else {
            return Vec::new();
        };
        return vec![ChatMessage::assistant(format!(
            "{WS_RESTORED_CONTEXT_PREFIX}\n{resume_context}"
        ))];
    }

    let keep_from = restore_safe
        .len()
        .saturating_sub(WS_RESTORED_HISTORY_KEEP_MESSAGES);
    let mut restored = Vec::new();

    if keep_from > 0 {
        if let Some(summary) = extract_latest_compaction_summary(&restore_safe[..keep_from]) {
            restored.push(ChatMessage::assistant(summary));
        } else if let Some(resume_context) = build_ws_resume_context(&restore_safe[..keep_from]) {
            restored.push(ChatMessage::assistant(format!(
                "{WS_RESTORED_CONTEXT_PREFIX}\n{resume_context}"
            )));
        }
    }

    restored.extend(restore_safe.into_iter().skip(keep_from));
    restored
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
    let command_name = command
        .split_whitespace()
        .next()
        .unwrap_or("command")
        .trim()
        .to_ascii_lowercase();
    let detail = match command_name.as_str() {
        "curl" => {
            "This fetched data from the target endpoint. The raw HTTP or payload response is shown above."
        }
        "lsusb" => {
            "This lists USB devices currently visible to the system, including bus/device IDs and vendor or product labels."
        }
        "lsblk" => "This lists block devices and partitions currently visible to the kernel.",
        "lspci" => "This lists PCI devices detected on the system.",
        "docker" | "docker-compose" => {
            "This ran a Docker command against the local container runtime."
        }
        "git" => "This ran a Git command. The repository result is shown in the raw output above.",
        _ if raw_output.trim().is_empty() => {
            "The command finished without producing any stdout or stderr output."
        }
        _ => "The raw command output is shown above.",
    };

    format!("{base} {detail}")
}

async fn summarize_direct_shell_command(
    runtime: &GatewayRuntimeSnapshot,
    command: &str,
    raw_output: &str,
    success: bool,
) -> String {
    let fallback = describe_direct_shell_result(command, raw_output, success);
    let output_for_prompt = if raw_output.trim().is_empty() {
        "(no output)"
    } else {
        raw_output.trim()
    };
    let truncated_output = crate::util::truncate_with_ellipsis(output_for_prompt, 4_000);
    let status = if success { "success" } else { "failure" };
    let messages = vec![
        ChatMessage::system(
            "You summarize completed local shell commands for a chat UI. The command has already run. No tools are available in this step. Briefly explain what happened based only on the command and its raw output. If the command failed, say so plainly. Keep the reply concise.",
        ),
        ChatMessage::user(format!(
            "Command: {command}\nStatus: {status}\nRaw output:\n{truncated_output}\n\nExplain the result in 1 to 4 sentences."
        )),
    ];

    match tokio::time::timeout(
        Duration::from_secs(12),
        runtime.provider.chat(
            ChatRequest {
                messages: &messages,
                tools: None,
            },
            &runtime.model,
            runtime.temperature,
        ),
    )
    .await
    {
        Ok(Ok(response)) => {
            let text = sanitize_ws_response(
                response.text_or_empty(),
                runtime.tools_registry_exec.as_ref(),
            );
            if text.trim().is_empty() {
                fallback
            } else {
                text
            }
        }
        Ok(Err(error)) => {
            tracing::warn!(
                command = %command,
                model = %runtime.model,
                error = %error,
                "direct shell summary fallback triggered"
            );
            fallback
        }
        Err(_) => {
            tracing::warn!(
                command = %command,
                model = %runtime.model,
                "direct shell summary timed out"
            );
            fallback
        }
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

fn extract_direct_shell_command(message: &str) -> Option<String> {
    let trimmed = message.trim();
    if looks_like_direct_shell_command(trimmed) {
        return Some(trimmed.to_string());
    }

    for prefix in [
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
            if looks_like_direct_shell_command(rest) {
                return Some(rest.to_string());
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

async fn emit_ws_delta_event(socket: &mut WebSocket, session_id: &str, event: WsDeltaEvent) {
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

async fn execute_direct_shell_command(
    socket: &mut WebSocket,
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

    let final_response =
        summarize_direct_shell_command(runtime, command, &raw_output, tool_result.success).await;
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

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    while let Some(msg) = socket.recv().await {
        let runtime = state.runtime_snapshot();
        let (
            provider_label,
            parallel_tools,
            native_tools,
            approval_manager,
            max_history_messages,
            system_prompt,
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
            let skills =
                crate::skills::load_skills_with_config(&config_guard.workspace_dir, &config_guard);
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
                config_guard.agent.max_history_messages,
                system_prompt,
                resolve_ws_chat_store_path(&config_guard),
            )
        };

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
        let session_id =
            normalize_ws_session_id(parsed.get("session_id").and_then(serde_json::Value::as_str))
                .unwrap_or_else(|| "default".to_string());

        if msg_type == "session_delete" {
            delete_ws_chat_history(&session_id, &ws_chat_store_path).await;
            continue;
        }

        if msg_type != "message" {
            continue;
        }

        let content = parsed["content"].as_str().unwrap_or("").to_string();
        if content.is_empty() {
            continue;
        }

        let temporary = parsed["temporary"].as_bool().unwrap_or(false);
        let history_seed = parse_seed_history(parsed.get("history_seed"));
        let mut history =
            load_ws_chat_history(&session_id, temporary, &history_seed, &ws_chat_store_path).await;

        if let Some(first) = history.first_mut() {
            if first.role == "system" {
                *first = ChatMessage::system(&system_prompt);
            } else {
                history.insert(0, ChatMessage::system(&system_prompt));
            }
        } else {
            history.push(ChatMessage::system(&system_prompt));
        }
        if let Some(resume_context) = build_ws_resume_context(&history[1..]) {
            history.insert(1, ChatMessage::system(resume_context));
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

        history.push(ChatMessage::user(&content));

        // Broadcast agent_start event
        let _ = state.event_tx.send(serde_json::json!({
            "type": "agent_start",
            "provider": provider_label,
            "model": runtime.model,
        }));

        if let Some(command) = extract_direct_shell_command(&content) {
            let result = execute_direct_shell_command(
                &mut socket,
                &session_id,
                &runtime,
                &mut history,
                &command,
            )
            .await;

            match result {
                Ok(response) => {
                    let _ = auto_compact_history(
                        &mut history,
                        runtime.provider.as_ref(),
                        &runtime.model,
                        max_history_messages,
                    )
                    .await;
                    trim_history(&mut history, max_history_messages);
                    store_ws_chat_history(&session_id, &history, temporary, &ws_chat_store_path)
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

            continue;
        }

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
                                    emit_ws_delta_event(&mut socket, &session_id, event).await;
                                }
                            } else {
                                break loop_future.await;
                            }
                        }
                        response = &mut loop_future => {
                            while let Ok(delta) = delta_rx.try_recv() {
                                if let Some(event) = parse_ws_delta_event(&delta) {
                                    emit_ws_delta_event(&mut socket, &session_id, event).await;
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
                let _ = auto_compact_history(
                    &mut history,
                    runtime.provider.as_ref(),
                    &runtime.model,
                    max_history_messages,
                )
                .await;
                trim_history(&mut history, max_history_messages);
                store_ws_chat_history(&session_id, &history, temporary, &ws_chat_store_path).await;

                // Send the full response as a done message
                let done = serde_json::json!({
                    "type": "done",
                    "session_id": session_id,
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
                store_ws_chat_history(
                    &session_id,
                    &history_before_turn,
                    temporary,
                    &ws_chat_store_path,
                )
                .await;
                let sanitized = crate::providers::sanitize_api_error(&e.to_string());
                let err = serde_json::json!({
                    "type": "error",
                    "session_id": session_id,
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
        assert!(resume.contains("Do not re-run tools solely because they appear in this saved context"));
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
        assert_eq!(session.history.len(), 4);
        assert!(session
            .history
            .iter()
            .all(|message| message.role != "system"));
        assert_eq!(session.history.last().unwrap().content, "2 + 2 = 4");

        delete_persisted_ws_chat_session(&store_path, "session-a").await;
        let after_delete = read_persisted_ws_chat_sessions(&store_path).await;
        assert!(!after_delete.sessions.contains_key("session-a"));
    }

    #[test]
    fn build_restored_ws_chat_history_keeps_recent_safe_transcript() {
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
        assert_eq!(restored.len(), 3);
        assert_eq!(restored[0].role, "user");
        assert_eq!(restored[0].content, "lsusb");
        assert_eq!(restored[1].role, "user");
        assert!(restored[1].content.contains("[Tool results]"));
        assert!(restored[1].content.contains("Bus 001 Device 001"));
        assert_eq!(restored[2].role, "assistant");
        assert_eq!(restored[2].content, "Command completed successfully.");
    }

    #[test]
    fn build_restored_ws_chat_history_keeps_compaction_summary_when_truncated() {
        let mut history = vec![ChatMessage::assistant(
            "[Compaction summary]\n- user wants concise answers",
        )];
        for i in 0..(WS_RESTORED_HISTORY_KEEP_MESSAGES + 2) {
            history.push(ChatMessage::user(format!("user {i}")));
        }

        let restored = build_restored_ws_chat_history(&history);
        assert_eq!(restored[0].role, "assistant");
        assert!(restored[0].content.contains("[Compaction summary]"));
        assert_eq!(restored.len(), WS_RESTORED_HISTORY_KEEP_MESSAGES + 1);
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
    }

    #[test]
    fn extract_direct_shell_command_rejects_normal_chat_text() {
        assert_eq!(extract_direct_shell_command("How are you today?"), None);
        assert_eq!(
            extract_direct_shell_command("Please explain what curl does."),
            None
        );
    }
}
