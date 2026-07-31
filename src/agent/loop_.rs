use crate::approval::{ApprovalManager, ApprovalRequest, ApprovalResponse};
use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory};
use crate::multimodal;
use crate::observability::{self, runtime_trace, Observer, ObserverEvent};
use crate::providers::{
    self, ChatMessage, ChatRequest, Provider, ProviderCapabilityError, ToolCall,
};
use crate::runtime;
use crate::security::SecurityPolicy;
use crate::tools::{self, Tool};
use crate::util::{output_shows_uncaught_exception, truncate_with_ellipsis};
use anyhow::Result;
use futures_util::StreamExt;
use regex::{Regex, RegexSet};
use rustyline::error::ReadlineError;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::Write;
use std::io::Write as _;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

mod context;
mod execution;
mod history;
pub(crate) mod parsing;

use context::{build_context, build_hardware_context};
use execution::{
    execute_tools_parallel, execute_tools_sequential, should_execute_tools_in_parallel,
    ToolExecutionOutcome,
};
#[cfg(test)]
use history::{apply_compaction_summary, build_compaction_transcript};
use history::{
    auto_compact_history, auto_compact_history_focused, compaction_range,
    deterministic_compact_history, trim_history,
};

/// Compact conversation history with an optional objective focus.
///
/// Intended for use by [`AutonomousLoop`] between retry attempts so that
/// compacted context stays relevant to the current task.
pub(crate) async fn compact_history_with_focus(
    history: &mut Vec<ChatMessage>,
    provider: &dyn Provider,
    model: &str,
    max_history: usize,
    focus: Option<&str>,
) -> Result<bool> {
    auto_compact_history_focused(history, provider, model, max_history, None, focus).await
}
#[allow(unused_imports)]
use parsing::{
    default_param_for_tool, detect_tool_call_parse_issue, extract_json_values, map_tool_name_alias,
    parse_arguments_value, parse_glm_shortened_body, parse_glm_style_tool_calls,
    parse_perl_style_tool_calls, parse_structured_tool_calls, parse_tool_call_value,
    parse_tool_calls, parse_tool_calls_from_json_value, tool_call_signature, ParsedToolCall,
};

/// Minimum characters per chunk when relaying LLM text to a streaming draft.
const STREAM_CHUNK_MIN_CHARS: usize = 80;
/// Rolling window size for detecting streamed tool-call payload markers.
const STREAM_TOOL_MARKER_WINDOW_CHARS: usize = 512;
/// Keep an operator-visible heartbeat flowing while a local non-streaming
/// inference segment is running. Native-tool Ollama calls intentionally use a
/// non-streaming response so structured tool calls remain reliable.
const MODEL_PROGRESS_HEARTBEAT_SECS: u64 = 10;

/// Minimum user-message length (in chars) for auto-save to memory.
/// Matches the channel-side constant in `channels/mod.rs`.
const AUTOSAVE_MIN_MESSAGE_CHARS: usize = 20;

static SENSITIVE_KEY_PATTERNS: LazyLock<RegexSet> = LazyLock::new(|| {
    RegexSet::new([
        r"(?i)token",
        r"(?i)api[_-]?key",
        r"(?i)password",
        r"(?i)secret",
        r"(?i)user[_-]?key",
        r"(?i)bearer",
        r"(?i)credential",
    ])
    .unwrap()
});

static SENSITIVE_KV_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)(token|api[_-]?key|password|secret|user[_-]?key|bearer|credential)(["']?\s*[:=]\s*)(?:"([^"]{8,})"|'([^']{8,})'|([a-zA-Z0-9_\-\.]{8,}))"#).unwrap()
});

/// Scrub credentials from tool output to prevent accidental exfiltration.
/// Replaces known credential patterns with a redacted placeholder while preserving
/// a small prefix for context.
pub(crate) fn scrub_credentials(input: &str) -> String {
    SENSITIVE_KV_REGEX
        .replace_all(input, |caps: &regex::Captures| {
            let key = &caps[1];
            let delimiter = caps.get(2).map(|m| m.as_str()).unwrap_or(": ");
            let val = caps
                .get(3)
                .or(caps.get(4))
                .or(caps.get(5))
                .map(|m| m.as_str())
                .unwrap_or("");
            let quote = if caps.get(3).is_some() {
                "\""
            } else if caps.get(4).is_some() {
                "'"
            } else {
                ""
            };

            // Preserve first 4 chars for context, then redact.
            let prefix = if val.len() > 4 { &val[..4] } else { "" };

            format!("{key}{delimiter}{quote}{prefix}*[REDACTED]{quote}")
        })
        .to_string()
}

pub(crate) fn configured_native_tools_enabled(
    tool_dispatcher: &str,
    provider_name: &str,
    model: &str,
    provider_supports_native_tools: bool,
) -> bool {
    // Models that reliably use Ollama's native function-calling protocol bypass the
    // global tool_dispatcher setting so they always get structured tool JSON, not the
    // prompt-injected [TOOL_CALLS] XML format they were never trained on.
    let model_wants_native = model_prefers_native_tools(model);
    (model_wants_native || !tool_dispatcher.trim().eq_ignore_ascii_case("xml"))
        && provider_supports_native_tools
        && native_tool_transport_supported(provider_name, model)
}

/// Models that should always use native tool calling regardless of `tool_dispatcher` config.
/// These were trained with structured function-call JSON and don't reliably emit the
/// prompt-injected `[TOOL_CALLS]name[ARGS]{...}` XML format.
fn model_prefers_native_tools(model: &str) -> bool {
    let base = model
        .strip_suffix(":cloud")
        .unwrap_or(model)
        .to_ascii_lowercase();
    let name = base.split(':').next().unwrap_or(&base);
    // qwen3 family: qwen3, qwen3.5, qwen3.6, qwen3-coder, etc.
    name.starts_with("qwen3")
}

fn inject_prompt_tool_fallback_instructions(
    history: &mut [ChatMessage],
    tool_specs: &[crate::tools::ToolSpec],
) {
    let Some(system_message) = history.iter_mut().find(|msg| msg.role == "system") else {
        return;
    };

    if system_message.content.contains("## Tool Use Protocol") {
        return;
    }

    system_message.content.push_str(
        "\n\n## Compatibility Fallback\n\n\
         Native tool calling failed for this model or provider. \
         For the rest of this turn, emit real <tool_call>...</tool_call> tags instead of \
         describing commands or returning native function-call payloads. \
         If the runtime says your last tool format was invalid, immediately retry with another \
         real <tool_call> call until you receive tool results or a runtime error blocks it.\n",
    );
    system_message
        .content
        .push_str(&build_tool_instructions_from_specs(tool_specs));
}

pub(crate) async fn with_tool_loop_settings<F>(
    parallel_tools: bool,
    native_tools: bool,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TOOL_LOOP_PARALLEL_TOOLS_ENABLED
        .scope(
            parallel_tools,
            TOOL_LOOP_NATIVE_TOOLS_ENABLED.scope(Some(native_tools), future),
        )
        .await
}

pub(crate) async fn with_tool_loop_history_limit<F>(
    max_history_messages: usize,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TOOL_LOOP_MAX_HISTORY_MESSAGES
        .scope(Some(max_history_messages), future)
        .await
}

/// Default trigger for auto-compaction when non-system message count exceeds this threshold.
/// Prefer passing the config-driven value via `run_tool_call_loop`; this constant is only
/// used when callers omit the parameter.
pub(crate) const DEFAULT_MAX_HISTORY_MESSAGES: usize = 50;

fn plan_boundary_history_budget(history_budget: usize) -> Option<usize> {
    if history_budget == 0 {
        return None;
    }

    // A value above the compact default is an explicit long-context policy.
    // Preserve its raw messages so the provider can select 128K/256K from the
    // actual request instead of discarding them at each completed plan item.
    (history_budget <= DEFAULT_MAX_HISTORY_MESSAGES)
        .then(|| history_budget.min(12).max(6))
}

fn context_pressure_history_budget(history: &[ChatMessage]) -> Option<usize> {
    const MIN_PRESSURE_HISTORY_MESSAGES: usize = 12;

    let has_system = history.first().is_some_and(|message| message.role == "system");
    let non_system_count = history
        .len()
        .saturating_sub(if has_system { 1 } else { 0 });
    if non_system_count <= MIN_PRESSURE_HISTORY_MESSAGES {
        return None;
    }

    Some(
        (non_system_count / 2)
            .max(MIN_PRESSURE_HISTORY_MESSAGES)
            .min(non_system_count - 1),
    )
}

/// Minimum interval between progress sends to avoid flooding the draft channel.
pub(crate) const PROGRESS_MIN_INTERVAL_MS: u64 = 500;

/// Sentinel value sent through on_delta to signal the draft updater to clear accumulated text.
/// Used before streaming the final answer so progress lines are replaced by the clean response.
pub(crate) const DRAFT_CLEAR_SENTINEL: &str = "\x00CLEAR\x00";
/// Sentinel prefix for internal progress deltas (thinking/tool execution trace).
/// Channel layers can suppress these messages by default and only expose them
/// when the user explicitly asks for command/tool execution details.
pub(crate) const DRAFT_PROGRESS_SENTINEL: &str = "\x00PROGRESS\x00";

/// Sentinel prefix for per-segment inference metrics forwarded on the delta
/// channel as JSON (ttft_ms, generation_tps, prefill_tps, total_ms).
pub(crate) const DRAFT_METRICS_SENTINEL: &str = "\x00METRICS\x00";

tokio::task_local! {
    static TOOL_LOOP_REPLY_TARGET: Option<String>;
}

tokio::task_local! {
    static TOOL_LOOP_PARALLEL_TOOLS_ENABLED: bool;
}

tokio::task_local! {
    static TOOL_LOOP_NATIVE_TOOLS_ENABLED: Option<bool>;
}

tokio::task_local! {
    static TOOL_LOOP_MAX_HISTORY_MESSAGES: Option<usize>;
}

tokio::task_local! {
    /// Optional tool-result cache active for the current autonomous run.
    /// Set via [`AutonomousLoop`] before calling `run_tool_call_loop`.
    pub(crate) static TOOL_CACHE: Option<Arc<crate::agent::tool_cache::ToolResultCache>>;
}

const AUTO_CRON_DELIVERY_CHANNELS: &[&str] = &["telegram", "discord", "slack", "mattermost"];

const NON_CLI_APPROVAL_POLL_INTERVAL_MS: u64 = 250;
const REPEATED_FILE_WRITE_STALL_THRESHOLD: usize = 3;
const AUTO_PLAN_RETRY_LIMIT: usize = 4;
// After this many meaningful tool calls without a task_plan, inject a retrospective
// plan prompt so the model tracks remaining work and doesn't lose the thread.
const RETROSPECTIVE_PLAN_THRESHOLD: usize = 3;
const WEB_SEARCH_WITHOUT_FETCH_STREAK_LIMIT: usize = 3;
const DUPLICATE_TOOL_CALL_STREAK_PER_NUDGE: usize = 2;
const DUPLICATE_TOOL_CALL_MAX_NUDGES: usize = 3;
// A thinking model that keeps exhausting its per-segment output budget on
// reasoning tokens alone, never emitting any visible text or tool call, is
// not making bounded progress like a normal multi-segment continuation —
// it's stuck. Hard-exit rather than checkpointing forever.
const MAX_CONSECUTIVE_EMPTY_OUTPUT_BUDGET_CHECKPOINTS: usize = 6;
const COORDINATION_STATUS_POLL_STREAK_LIMIT: usize = 2;
const MISSING_TOOL_CALL_RETRY_PROMPT: &str = "Internal correction: stay on the current user task. Your last reply implied follow-up action, but no valid tool call was emitted. If another tool step is still required, emit that tool call now and nothing else. For shell actions, prefer a single real command or the runtime's canonical shell tool syntax; do not wrap it in markdown, do not describe what you would run, and do not switch topics. If file creation or editing is needed, prefer the dedicated file tools. If shell-level file creation is still required, use a direct command and, for heredocs, use a quoted delimiter like << 'EOF'. If no tool is needed, provide the complete final answer now and do not defer action.";
const DUPLICATE_TOOL_CALL_NUDGE_PROMPTS: &[&str] = &[
    "Internal correction: that search or action was already completed earlier in this turn — the results are already in context above. \
     Do not repeat it. Move to the next step of your task: use the shell, write a file, or respond with your findings.",
    "You are still repeating an action you already took. Look at the tool results already in context and use them. \
     Take a new action now — something you have not done yet in this turn.",
    "Final redirect: you are stuck in a loop. Use only what you already have and complete the task. \
     Take a concrete new action (shell_exec, file_write) or provide your final answer now.",
];
const TOOL_UNAVAILABLE_RETRY_PROMPT_PREFIX: &str = "Internal correction: your prior reply claimed required tools were unavailable. Use only the runtime-allowed tools listed below. If tool use is needed, emit the real tool call now instead of refusing, narrating, or giving an example.";

/// Detect completion claims that imply state-changing work already happened
/// without an accompanying tool call.
static ACTION_COMPLETION_CUE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)\b(done|completed?|finished|successfully|i(?:'ve|\s+have)|we(?:'ve|\s+have)|i(?:'ll|\s+will)|let\s+me)\b",
    )
    .unwrap()
});

/// Verbs that usually imply side effects requiring tool execution.
static SIDE_EFFECT_ACTION_VERB_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)\b(create|created|write|wrote|run|ran|execute|executed|update|updated|delete|deleted|remove|removed|rename|renamed|move|moved|install|installed|save|saved|make|made)\b",
    )
    .unwrap()
});

/// Concrete artifacts often referenced in file/system action completion claims.
static SIDE_EFFECT_ACTION_OBJECT_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)\b(file|files|folder|folders|directory|directories|workspace|cwd|current\s+working\s+directory|command|commands|script|scripts|path|paths|tool|tools|file_read|file_write|file_edit|web_search_tool|shell|task_plan|http_request)\b",
    )
    .unwrap()
});

static DEFERRED_TOOL_ACTION_CUE_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        # Pattern 1: subject + modal + action + tool  (I'll call task_plan, let me use shell, ...)
        \b(i(?:'ll|\s+will)|let\s+me|now\s+(?:executing|running|using)|we\s+(?:need\s+to|should|must|will)|
           (?:need|should|must)\s+(?:to\s+)?(?:now\s+)?(?:output|emit|send|produce|generate))
        \b.{0,120}
        \b(call|use|run|execute|search|read|write|output|emit)\b.{0,120}
        \b(tool|tool\s+call|function\s+call|file_read|file_write|file_edit|web_search_tool|shell|task_plan|http_request|glob_search|content_search)\b
        |
        # Pattern 2: direct reference to a specific tool call being needed/required
        \b(?:output|emit|send|produce|generate)\s+a\s+(?:tool\s+call|function\s+call)\b
        ",
    )
    .unwrap()
});

static URL_IN_TEXT_REGEX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"https?://[^\s<>"'`)]+"#).unwrap());

/// Detect responses that incorrectly claim file tooling is unavailable even
/// when runtime policy allows file tools in this turn.
static TOOL_UNAVAILABLE_CLAIM_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        \b(
            i\s+(?:do\s+not|don't)\s+have\s+access(?:\s+to)?|
            i\s+(?:cannot|can't)(?:\s+\w+){0,3}\s+(?:access|use|perform|create|edit|write|read|run|execute|open|browse)|
            i\s+am\s+unable\s+to|
            no\s+(?:tool|tools|function|functions)\s+(?:available|access)
        )\b",
    )
    .unwrap()
});

static FILE_WRITE_CONTENT_LITERAL_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)content(?:[^a-z0-9\n]{0,20})(?:"([^"\n]{1,200})"|`([^`\n]{1,200})`)"#)
        .unwrap()
});

#[derive(Debug, Clone)]
struct SuccessfulToolRecord {
    name: String,
    arguments: serde_json::Value,
    output: String,
}

#[derive(Debug, Clone)]
struct FailedToolRecord {
    name: String,
    output: String,
}

#[derive(Debug, Clone)]
pub(crate) struct NonCliApprovalPrompt {
    pub request_id: String,
    pub tool_name: String,
    pub arguments: serde_json::Value,
}

#[derive(Debug, Clone)]
pub(crate) struct NonCliApprovalContext {
    pub sender: String,
    pub reply_target: String,
    pub prompt_tx: tokio::sync::mpsc::UnboundedSender<NonCliApprovalPrompt>,
}

tokio::task_local! {
    static TOOL_LOOP_NON_CLI_APPROVAL_CONTEXT: Option<NonCliApprovalContext>;
}

/// Extract a short hint from tool call arguments for progress display.
fn truncate_tool_args_for_progress(name: &str, args: &serde_json::Value, max_len: usize) -> String {
    let hint = match name {
        "shell" => args.get("command").and_then(|v| v.as_str()),
        "file_read" | "file_write" => args.get("path").and_then(|v| v.as_str()),
        _ => args
            .get("action")
            .and_then(|v| v.as_str())
            .or_else(|| args.get("query").and_then(|v| v.as_str())),
    };
    match hint {
        Some(s) => truncate_with_ellipsis(s, max_len),
        None => String::new(),
    }
}

fn maybe_inject_cron_add_delivery(
    tool_name: &str,
    tool_args: &mut serde_json::Value,
    channel_name: &str,
    reply_target: Option<&str>,
) {
    if tool_name != "cron_add"
        || !AUTO_CRON_DELIVERY_CHANNELS
            .iter()
            .any(|supported| supported == &channel_name)
    {
        return;
    }

    let Some(reply_target) = reply_target.map(str::trim).filter(|v| !v.is_empty()) else {
        return;
    };

    let Some(args_obj) = tool_args.as_object_mut() else {
        return;
    };

    let is_agent_job = match args_obj.get("job_type").and_then(serde_json::Value::as_str) {
        Some("agent") => true,
        Some(_) => false,
        None => args_obj.contains_key("prompt"),
    };
    if !is_agent_job {
        return;
    }

    let delivery = args_obj
        .entry("delivery".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(delivery_obj) = delivery.as_object_mut() else {
        return;
    };

    let mode = delivery_obj
        .get("mode")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("none");
    if mode.eq_ignore_ascii_case("none") || mode.trim().is_empty() {
        delivery_obj.insert(
            "mode".to_string(),
            serde_json::Value::String("announce".to_string()),
        );
    } else if !mode.eq_ignore_ascii_case("announce") {
        // Respect explicitly chosen non-announce modes.
        return;
    }

    let needs_channel = delivery_obj
        .get("channel")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|value| value.trim().is_empty());
    if needs_channel {
        delivery_obj.insert(
            "channel".to_string(),
            serde_json::Value::String(channel_name.to_string()),
        );
    }

    let needs_target = delivery_obj
        .get("to")
        .and_then(serde_json::Value::as_str)
        .is_none_or(|value| value.trim().is_empty());
    if needs_target {
        delivery_obj.insert(
            "to".to_string(),
            serde_json::Value::String(reply_target.to_string()),
        );
    }
}

async fn await_non_cli_approval_decision(
    mgr: &ApprovalManager,
    request_id: &str,
    cancellation_token: Option<&CancellationToken>,
) -> ApprovalResponse {
    loop {
        if let Some(decision) = mgr.take_non_cli_pending_resolution(request_id) {
            return decision;
        }

        if !mgr.has_non_cli_pending_request(request_id) {
            // Fail closed when the request disappears without an explicit resolution.
            return ApprovalResponse::No;
        }

        if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
            return ApprovalResponse::No;
        }

        tokio::time::sleep(Duration::from_millis(NON_CLI_APPROVAL_POLL_INTERVAL_MS)).await;
    }
}

/// Convert a tool registry to OpenAI function-calling format for native tool support.
fn tools_to_openai_format(tools_registry: &[Box<dyn Tool>]) -> Vec<serde_json::Value> {
    tools_registry
        .iter()
        .map(|tool| {
            serde_json::json!({
                "type": "function",
                "function": {
                    "name": tool.name(),
                    "description": tool.description(),
                    "parameters": tool.parameters_schema()
                }
            })
        })
        .collect()
}

fn autosave_memory_key(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4())
}

/// Build assistant history entry in JSON format for native tool-call APIs.
/// `convert_messages` in the OpenRouter provider parses this JSON to reconstruct
/// the proper `NativeMessage` with structured `tool_calls`.
fn build_native_assistant_history(
    text: &str,
    tool_calls: &[ToolCall],
    reasoning_content: Option<&str>,
) -> String {
    let calls_json: Vec<serde_json::Value> = tool_calls
        .iter()
        .map(|tc| {
            serde_json::json!({
                "id": tc.id,
                "name": tc.name,
                "arguments": tc.arguments,
            })
        })
        .collect();

    let content = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text.trim().to_string())
    };

    let mut obj = serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    });

    if let Some(rc) = reasoning_content {
        obj.as_object_mut().unwrap().insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc.to_string()),
        );
    }

    obj.to_string()
}

fn build_native_assistant_history_from_parsed_calls(
    text: &str,
    tool_calls: &[ParsedToolCall],
    reasoning_content: Option<&str>,
) -> Option<String> {
    let calls_json = tool_calls
        .iter()
        .map(|tc| {
            Some(serde_json::json!({
                "id": tc.tool_call_id.clone()?,
                "name": tc.name,
                "arguments": serde_json::to_string(&tc.arguments).unwrap_or_else(|_| "{}".to_string()),
            }))
        })
        .collect::<Option<Vec<_>>>()?;

    let content = if text.trim().is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::Value::String(text.trim().to_string())
    };

    let mut obj = serde_json::json!({
        "content": content,
        "tool_calls": calls_json,
    });

    if let Some(rc) = reasoning_content {
        obj.as_object_mut().unwrap().insert(
            "reasoning_content".to_string(),
            serde_json::Value::String(rc.to_string()),
        );
    }

    Some(obj.to_string())
}

fn build_assistant_history_with_tool_calls(text: &str, tool_calls: &[ToolCall]) -> String {
    let mut parts = Vec::new();

    if !text.trim().is_empty() {
        parts.push(text.trim().to_string());
    }

    for call in tool_calls {
        let arguments = serde_json::from_str::<serde_json::Value>(&call.arguments)
            .unwrap_or_else(|_| serde_json::Value::String(call.arguments.clone()));
        let payload = serde_json::json!({
            "id": call.id,
            "name": call.name,
            "arguments": arguments,
        });
        parts.push(format!("<tool_call>\n{payload}\n</tool_call>"));
    }

    parts.join("\n")
}

/// Returns true for models that are too small for reliable native function-calling
/// (≤2B actual parameters, detected from common Ollama tag suffixes like `:1b`, `:2b`).
/// These models use the prompt-based `<tool_call>` path instead, which gives more
/// explicit formatting guidance and avoids native API confusion.
///
/// Note: `:e2b` / `:e4b` tags (Google Gemma edge variants) are NOT tiny — they are
/// ~5B and ~8B actual parameters respectively and should use native tools normally.
fn is_small_model_for_native_tools(model: &str) -> bool {
    // Strip optional `:cloud` suffix before checking the tag.
    let base = model.strip_suffix(":cloud").unwrap_or(model);
    let tag = base
        .rsplit_once(':')
        .map_or("", |(_, t)| t)
        .to_ascii_lowercase();
    // Only match bare :1b / :2b or quantized variants like :1b-q4_0 — not :e2b / :e4b.
    matches!(tag.as_str(), "1b" | "2b") || tag.starts_with("1b-") || tag.starts_with("2b-")
}

/// Returns false for provider/model combinations that currently advertise native
/// tool support but are not reliable enough for LlamaFarm's autonomous loop.
fn native_tool_transport_supported(provider_name: &str, model: &str) -> bool {
    if is_small_model_for_native_tools(model) {
        return false;
    }

    let provider = provider_name.trim().to_ascii_lowercase();
    let normalized_model = model
        .strip_suffix(":cloud")
        .unwrap_or(model)
        .to_ascii_lowercase();

    // Ollama currently rejects some gpt-oss tool calls server-side by trying to
    // parse raw shell text as JSON. Force compatibility tool mode up front so the
    // model can still execute tools reliably on high-end local GPU boxes.
    if provider == "ollama" && normalized_model.contains("gpt-oss") {
        return false;
    }

    true
}

fn looks_like_unverified_action_completion_without_tool_call(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    ACTION_COMPLETION_CUE_REGEX.is_match(trimmed)
        && SIDE_EFFECT_ACTION_VERB_REGEX.is_match(trimmed)
        && SIDE_EFFECT_ACTION_OBJECT_REGEX.is_match(trimmed)
}

fn looks_like_deferred_tool_action_without_call(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }

    DEFERRED_TOOL_ACTION_CUE_REGEX.is_match(trimmed)
}

fn looks_like_tool_unavailability_claim(text: &str, tool_specs: &[crate::tools::ToolSpec]) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() || !TOOL_UNAVAILABLE_CLAIM_REGEX.is_match(trimmed) {
        return false;
    }

    let lowered = trimmed.to_ascii_lowercase();
    let has_file_tool = tool_specs.iter().any(|spec| {
        matches!(
            spec.name.as_str(),
            "file_write" | "file_edit" | "file_read" | "glob_search"
        )
    });
    let has_shell_tool = tool_specs
        .iter()
        .any(|spec| matches!(spec.name.as_str(), "shell" | "process"));
    let has_browser_tool = tool_specs
        .iter()
        .any(|spec| matches!(spec.name.as_str(), "browser" | "browser_open"));
    let claims_file = ["file", "write", "edit", "read"]
        .iter()
        .any(|needle| lowered.contains(needle));
    let claims_shell = ["shell", "command", "commands", "run", "execute", "terminal"]
        .iter()
        .any(|needle| lowered.contains(needle));
    let claims_browser = ["browser", "browse", "open", "page", "website"]
        .iter()
        .any(|needle| lowered.contains(needle));
    let claims_generic_tool_access = ["tool", "tools", "function", "functions"]
        .iter()
        .any(|needle| lowered.contains(needle));

    (claims_file && has_file_tool)
        || (claims_shell && has_shell_tool)
        || (claims_browser && has_browser_tool)
        || (claims_generic_tool_access && !tool_specs.is_empty())
}

fn build_tool_unavailable_retry_prompt(tool_specs: &[crate::tools::ToolSpec]) -> String {
    const MAX_TOOLS_IN_PROMPT: usize = 24;
    let tool_list = tool_specs
        .iter()
        .map(|spec| spec.name.as_str())
        .take(MAX_TOOLS_IN_PROMPT)
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "{TOOL_UNAVAILABLE_RETRY_PROMPT_PREFIX}\nRuntime tools: {tool_list}\nEmit the correct tool call now if tool use is required. Otherwise provide the final answer without claiming missing tools."
    )
}

fn build_tool_result_grounding_retry_prompt(records: &[SuccessfulToolRecord]) -> String {
    let mut prompt = String::from(
        "Internal correction: use the verified tool results below and provide the final answer now. Do not reinterpret tool output as a missing or invalid tool. Do not ask the user to clarify unless the tool results are genuinely insufficient. For `file_read`, answer directly from the file contents in the tool result. For `file_write`, do not invent file contents; only mention content that appeared in the write arguments or a verified read-back.\n\nVerified tool results:\n",
    );

    for record in records.iter().rev().take(4).rev() {
        let args = serde_json::to_string(&record.arguments).unwrap_or_else(|_| "{}".to_string());
        let output = truncate_with_ellipsis(&record.output, 240);
        let _ = writeln!(&mut prompt, "- {} {} => {}", record.name, args, output);
    }

    prompt.push_str("\nProvide the grounded final answer now.");
    prompt
}

fn build_failed_tool_retry_prompt(records: &[FailedToolRecord]) -> String {
    let Some(record) = records.last() else {
        return MISSING_TOOL_CALL_RETRY_PROMPT.to_string();
    };

    let output = truncate_with_ellipsis(record.output.trim(), 240);
    if record.name == "task_plan" && output.contains("execution has already started this turn") {
        return "Internal correction: the task plan already exists and execution is already underway. Do NOT call `task_plan` again right now. Continue directly with the next incomplete step using a real work tool, or provide the final answer if the work is complete.".to_string();
    }

    let mut prompt = format!(
        "Internal correction: your last tool call for `{}` failed with this runtime result:\n{}\n\nEmit a corrected real <tool_call> now and nothing else. Do not ask the user how to proceed, do not explain the schema, and do not switch topics.",
        record.name, output
    );

    if record.name == "task_plan" {
        prompt.push_str(
            "\nFor `task_plan` create requests, derive the plan directly from the user's current request and emit a non-empty `tasks` array with `{ \"title\": \"...\" }` items.",
        );
    } else if record.name == "shell" {
        let lowered = output.to_ascii_lowercase();
        if lowered.contains("can't open file")
            || lowered.contains("no such file or directory")
            || lowered.contains("not found")
        {
            prompt.push_str(
                "\nIf the command failed because a file path does not exist, reuse the exact real path from earlier successful file tools or create the file first with `file_write`, then rerun the shell command. Do not use placeholder paths like `/path/to/script.py` unless that exact file was actually created.",
            );
        }
        if lowered.contains("command not allowed by security policy")
            || lowered.contains("missing 'command' parameter")
        {
            prompt.push_str(
                "\nEmit a real `shell` tool call with a plain command string. Do not send `shell(command=\"...\")` as shell input, and do not paste raw script bodies into `shell` when `file_write` should create the file.",
            );
        }
    }

    prompt
}

fn extract_file_read_content(output: &str) -> Option<String> {
    let mut lines = Vec::new();

    for line in output.lines() {
        let trimmed = line.trim_end();
        if trimmed.starts_with('[') && trimmed.ends_with("lines total]") {
            break;
        }

        if let Some((prefix, rest)) = trimmed.split_once(": ") {
            if prefix.chars().all(|ch| ch.is_ascii_digit()) {
                lines.push(rest.to_string());
                continue;
            }
        }

        if !trimmed.is_empty() {
            lines.push(trimmed.to_string());
        }
    }

    (!lines.is_empty()).then(|| lines.join("\n"))
}

fn extract_preferred_url(output: &str) -> Option<String> {
    let mut urls = URL_IN_TEXT_REGEX
        .find_iter(output)
        .map(|m| {
            m.as_str()
                .trim_end_matches(['.', ',', ';', ':', ')', ']', '}', '>'])
                .to_string()
        })
        .filter(|url| !url.is_empty())
        .collect::<Vec<_>>();
    if urls.is_empty() {
        return None;
    }

    if let Some(preferred) = urls
        .iter()
        .find(|url| url.to_ascii_lowercase().contains("rust-lang.org"))
    {
        return Some(preferred.clone());
    }

    Some(urls.remove(0))
}

fn looks_like_tool_result_misinterpretation(text: &str) -> bool {
    let lowered = text.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return false;
    }

    [
        "doesn't appear to be a valid tool",
        "does not appear to be a valid tool",
        "available toolset",
        "available tools",
        "i have access to tools like",
        "could you clarify what you'd like",
        "what would you like to accomplish",
        "i understand the correction",
        "i'll use the available runtime tools",
        "what would you like me to help you with",
        "what would you like me to do next",
        "what would you like to do next",
        "please let me know how you'd like to proceed",
        "please share more context",
        "to help you better",
        "would you like me to search for pdf files",
        "path to the pdf file",
        "pdf read operation failed",
        "i need to actually create the file using the file_write tool",
        "<tool_result name=",
        "<web_search_tool",
        "i'll execute the `file_read` tool",
        "i will execute the `file_read` tool",
        "i'll execute the file_read tool",
        "i will execute the file_read tool",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn looks_like_irrelevant_code_dump(text: &str) -> bool {
    text.contains("```") && text.lines().count() >= 12
}

fn looks_like_file_read_answer_mismatch(text: &str, records: &[SuccessfulToolRecord]) -> bool {
    let Some(record) = records
        .iter()
        .rev()
        .find(|record| record.name == "file_read")
    else {
        return false;
    };

    let Some(expected_content) = extract_file_read_content(&record.output) else {
        return false;
    };

    if text.contains(&expected_content) {
        return false;
    }

    looks_like_tool_result_misinterpretation(text) || looks_like_irrelevant_code_dump(text)
}

fn looks_like_file_write_content_mismatch(text: &str, records: &[SuccessfulToolRecord]) -> bool {
    let Some(record) = records
        .iter()
        .rev()
        .find(|record| record.name == "file_write")
    else {
        return false;
    };

    let Some(expected_content) = record
        .arguments
        .get("content")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|content| !content.is_empty() && !content.contains('\n'))
    else {
        return false;
    };

    if text.contains(expected_content) {
        return false;
    }

    FILE_WRITE_CONTENT_LITERAL_REGEX
        .captures_iter(text)
        .filter_map(|caps| caps.get(1).or_else(|| caps.get(2)))
        .map(|capture| capture.as_str().trim())
        .any(|captured| !captured.is_empty() && captured != expected_content)
}

fn looks_like_task_plan_followup_question(text: &str, records: &[SuccessfulToolRecord]) -> bool {
    let Some(record) = records
        .iter()
        .rev()
        .find(|record| record.name == "task_plan")
    else {
        return false;
    };

    let action = record
        .arguments
        .get("action")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if action != "create" && action != "list" {
        return false;
    }

    let lowered = text.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return false;
    }

    [
        "the active task plan was created",
        "active task plan was created",
        "i don't have the plan_id",
        "i do not have the plan_id",
        "could you provide the plan_id",
        "could you provide that plan_id",
        "provide the plan_id",
        "provide that plan_id",
        "what is the plan_id",
        "what's the plan_id",
        "plan id",
        "would you like me to",
        "what would you like to do next",
        "what would you like me to do next",
        "could you please provide more details",
        "please let me know how you'd like to proceed",
        "please share more context",
        "to help you better",
        "what were the tasks about",
        "proceed with executing",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

fn is_internal_tool_loop_user_message(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with("[Tool results]")
        || trimmed.starts_with("Internal correction:")
        || trimmed.starts_with("Internal continuation:")
        || trimmed.starts_with("Internal working state:")
}

fn latest_external_user_request(history: &[ChatMessage]) -> Option<&str> {
    history
        .iter()
        .rev()
        .find(|message| {
            message.role == "user" && !is_internal_tool_loop_user_message(&message.content)
        })
        .map(|message| {
            let content = message.content.as_str();
            // User messages are enriched as "{mem_context}[YYYY-MM-DD HH:MM:SS TZ] {request}".
            // Strip the prefix so auto-plan step counting only sees the actual request text,
            // not action words from injected memory entries.
            strip_enriched_user_message_prefix(content).unwrap_or(content)
        })
}

fn strip_enriched_user_message_prefix(content: &str) -> Option<&str> {
    // Format: "{optional_context}[YYYY-MM-DD HH:MM:SS TZ] {actual_request}"
    // Find the last "[YYYY-" style timestamp bracket and return everything after "] ".
    let mut search = content;
    let mut last_match: Option<usize> = None;
    while let Some(pos) = search.find('[') {
        let abs = content.len() - search.len() + pos;
        let after_bracket = &content[abs + 1..];
        // Timestamp starts with a 4-digit year (digit chars followed by '-')
        let is_timestamp = after_bracket.len() >= 5
            && after_bracket[..4].chars().all(|c| c.is_ascii_digit())
            && after_bracket.as_bytes().get(4) == Some(&b'-');
        if is_timestamp {
            last_match = Some(abs);
        }
        search = &search[pos + 1..];
    }
    let ts_start = last_match?;
    let after_ts = &content[ts_start..];
    let bracket_end = after_ts.find(']')?;
    let rest = &after_ts[bracket_end + 1..];
    Some(rest.trim_start())
}

fn build_missing_tool_call_retry_prompt(history: &[ChatMessage]) -> String {
    let mut prompt = MISSING_TOOL_CALL_RETRY_PROMPT.to_string();

    if let Some(request) = latest_external_user_request(history) {
        prompt.push_str("\n\nCurrent user task:\n");
        prompt.push_str(request.trim());
        prompt.push_str(
            "\n\nStay on that exact task. Emit the next real tool call now if action is still required.",
        );
    }

    prompt
}

fn actionable_request_step_count(text: &str) -> usize {
    static ACTION_STEP_REGEX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?i)\b(write_?file|create_?file|save_?file|write|create|save|read|show|open|run|execute|print|delete|remove|rm|mkdir|make|list|pull|build|compile|install|fix|update|add|implement|test|verify|check|explore|find|search|start|continue|deploy|generate|parse|edit|modify|refactor|debug|launch|init|initialize|setup|configure)\b",
        )
        .unwrap()
    });

    let normalized = text
        .replace(" and then ", "\n")
        .replace(" then ", "\n")
        .replace(" after that ", "\n")
        .replace(" after ", "\n")
        .replace(',', "\n")
        .replace(';', "\n");

    let clause_count = normalized
        .lines()
        .filter(|clause| ACTION_STEP_REGEX.is_match(clause))
        .count();
    if clause_count > 0 {
        clause_count
    } else {
        ACTION_STEP_REGEX.find_iter(text).count()
    }
}

fn is_planning_only_request(text: &str) -> bool {
    let lowered = text.to_ascii_lowercase();
    let asks_for_plan = lowered.contains("task plan")
        || lowered.contains("implementation plan")
        || lowered.contains("create a plan")
        || lowered.contains("create the plan")
        || lowered.contains("create plan")
        || lowered.contains("make a plan")
        || lowered.contains("make the plan")
        || lowered.contains("give me a plan");
    let asks_for_execution = lowered.contains("execute")
        || lowered.contains("run it")
        || lowered.contains("do it")
        || lowered.contains("carry out")
        || lowered.contains("implement")
        || lowered.contains("complete the plan")
        || lowered.contains("complete plan")
        || lowered.contains("complete it")
        || lowered.contains("then complete")
        || lowered.contains("then continue")
        || lowered.contains("continue after planning")
        || lowered.contains("after planning")
        || lowered.contains("end-to-end")
        || lowered.contains("using real tools")
        || lowered.contains("do not stop after planning")
        || lowered.contains("don't stop after planning");

    lowered.contains("only create the plan")
        || lowered.contains("only make a plan")
        || lowered.contains("plan only")
        || lowered.contains("before any tool actions")
        || lowered.contains("what is next")
        || lowered.contains("next task")
        || lowered.contains("implementation plan before any tool actions")
        || (asks_for_plan && !asks_for_execution)
}

/// Returns true when the user is asking for information about the agent rather
/// than asking it to change the environment. These answers naturally mention
/// available tools and often end with phrases such as "what do you need done?";
/// they must not be mistaken for an unverified action-completion claim.
fn is_informational_agent_request(text: &str) -> bool {
    let lowered = text.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return false;
    }

    let asks_about_capabilities = [
        "what capabilities",
        "your capabilities",
        "what can you do",
        "what are you able to do",
        "what tools do you have",
        "which tools do you have",
        "list your tools",
        "what is available",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));
    let asks_for_status = [
        "are you working",
        "are you online",
        "are you functional",
        "what is your status",
        "are you ready",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));
    let explicitly_requests_execution = [
        "test ",
        "verify ",
        "execute ",
        "run ",
        "create ",
        "write ",
        "install ",
        "configure ",
        "deploy ",
        "fix ",
        "update ",
    ]
    .iter()
    .any(|needle| lowered.contains(needle));

    (asks_about_capabilities || asks_for_status) && !explicitly_requests_execution
}

fn has_high_value_task_plan_signal(text: &str) -> bool {
    static TASK_PLAN_SIGNAL_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?ix)
            \b(
                test\s+all
                |all\s+tool(?:\s+calls?)?
                |every\s+tool
                |each\s+tool
                |full\s+(?:workflow|sweep|validation|verification)
                |exhaustive
                |batch
                |delegate
                |delegation
                |parallel
                |federat(?:e|ed|ion)
                |local\s+and\s+remote
                |both\s+(?:hosts|boxes|machines|nodes)
                |multi[-\s]?host
                |multi[-\s]?machine
                |track\s+progress
                |checklist
                |task\s+plan
                |task_plan
                |create\s+a\s+plan
                |make\s+a\s+plan
            )\b
            ",
        )
        .unwrap()
    });

    TASK_PLAN_SIGNAL_RE.is_match(text)
}

fn should_auto_plan_current_request(history: &[ChatMessage]) -> bool {
    let Some(request) = latest_external_user_request(history) else {
        return false;
    };

    if is_planning_only_request(request) {
        return false;
    }

    let step_count = actionable_request_step_count(request);
    step_count >= 4 || has_high_value_task_plan_signal(request)
}

fn task_plan_call_is_create(arguments: &serde_json::Value) -> bool {
    arguments
        .get("action")
        .and_then(|value| value.as_str())
        .is_some_and(|action| action == "create")
        || arguments
            .get("tasks")
            .and_then(|value| value.as_array())
            .is_some_and(|tasks| !tasks.is_empty())
        || arguments
            .get("hint")
            .and_then(|value| value.as_str())
            .is_some_and(|hint| hint == "create")
}

/// Returns true if a completed tool record represents a successful task_plan create,
/// detected via arguments (any supported schema) OR the tool output text.
fn task_plan_record_is_create(record: &SuccessfulToolRecord) -> bool {
    if record.name != "task_plan" {
        return false;
    }
    if task_plan_call_is_create(&record.arguments) {
        return true;
    }
    // Fallback: tool output says "Created N task(s)" regardless of argument form
    let out = record.output.to_ascii_lowercase();
    out.contains("created") && (out.contains("task") || out.contains("step"))
}

fn should_require_task_plan_before_execution(
    history: &[ChatMessage],
    tool_calls: &[ParsedToolCall],
    records: &[SuccessfulToolRecord],
) -> bool {
    if !should_auto_plan_current_request(history) {
        return false;
    }

    if records
        .iter()
        .any(|record| record.name == "task_plan" && task_plan_call_is_create(&record.arguments))
    {
        return false;
    }

    let has_non_plan_call = tool_calls.iter().any(|call| call.name != "task_plan");
    let has_task_plan_create_call = tool_calls
        .iter()
        .any(|call| call.name == "task_plan" && task_plan_call_is_create(&call.arguments));

    has_non_plan_call && !has_task_plan_create_call
}

fn build_auto_plan_retry_prompt() -> String {
    "Internal correction: this request contains multiple actionable steps. First emit a real `task_plan` create call. Give each step a concise title, compact task-local context, and the expected tool names, then continue executing with real tool calls. Do not ask the user what to do next unless a tool returns a blocking error.".to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TaskPlanProgress {
    total: usize,
    completed: usize,
    resolved: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskPlanItemSnapshot {
    id: usize,
    title: String,
    status: String,
    context: Option<String>,
    tools: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TaskPlanSnapshot {
    items: Vec<TaskPlanItemSnapshot>,
}

fn task_plan_status_is_terminal(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "blocked" | "skipped")
}

fn task_plan_record_resolves_item(record: &SuccessfulToolRecord) -> bool {
    record.name == "task_plan"
        && record
            .arguments
            .get("action")
            .and_then(|value| value.as_str())
            == Some("update")
        && record
            .arguments
            .get("status")
            .and_then(|value| value.as_str())
            .is_some_and(task_plan_status_is_terminal)
}

fn task_plan_items_from_create_arguments(
    arguments: &serde_json::Value,
) -> Vec<TaskPlanItemSnapshot> {
    arguments
        .get("tasks")
        .or_else(|| arguments.get("steps"))
        .and_then(|value| value.as_array())
        .map(|tasks| {
            tasks
                .iter()
                .enumerate()
                .filter_map(|(idx, task)| {
                    let title = [
                        "title",
                        "description",
                        "name",
                        "task_name",
                        "step",
                        "command",
                    ]
                    .iter()
                    .find_map(|&key| {
                        task.get(key)
                            .and_then(|value| value.as_str())
                            .map(str::trim)
                            .filter(|title| !title.is_empty())
                    })?;
                    let status = task
                        .get("status")
                        .and_then(|value| value.as_str())
                        .unwrap_or("pending");
                    let context = task
                        .get("context")
                        .or_else(|| task.get("sub_context"))
                        .and_then(|value| value.as_str())
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(str::to_string);
                    let mut tools = Vec::new();
                    if let Some(items) = task
                        .get("tools")
                        .or_else(|| task.get("allowed_tools"))
                        .and_then(|value| value.as_array())
                    {
                        for tool in items.iter().filter_map(|value| value.as_str()) {
                            let tool = tool.trim();
                            if !tool.is_empty() && !tools.iter().any(|value| value == tool) {
                                tools.push(tool.to_string());
                            }
                        }
                    }

                    Some(TaskPlanItemSnapshot {
                        id: idx + 1,
                        title: title.to_string(),
                        status: status.to_string(),
                        context,
                        tools,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_task_plan_items_from_output(output: &str) -> Vec<TaskPlanItemSnapshot> {
    static TASK_PLAN_OUTPUT_ITEM_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^\s*-\s*\[(\d+)\]\s*\[([a-z_]+)\]\s+(.+?)\s*$").unwrap());

    let mut items = Vec::new();
    for line in output.lines() {
        if let Some(captures) = TASK_PLAN_OUTPUT_ITEM_RE.captures(line) {
            let Some(id) = captures
                .get(1)
                .and_then(|value| value.as_str().parse::<usize>().ok())
            else {
                continue;
            };
            let Some(status) = captures.get(2).map(|value| value.as_str().trim()) else {
                continue;
            };
            let Some(title) = captures.get(3).map(|value| value.as_str().trim()) else {
                continue;
            };
            if status.is_empty() || title.is_empty() {
                continue;
            }

            items.push(TaskPlanItemSnapshot {
                id,
                title: title.to_string(),
                status: status.to_string(),
                context: None,
                tools: Vec::new(),
            });
            continue;
        }

        let detail = line.trim();
        let Some(item) = items.last_mut() else {
            continue;
        };
        if let Some(context) = detail.strip_prefix("↳ context:") {
            let context = context.trim();
            if !context.is_empty() {
                item.context = Some(context.to_string());
            }
        } else if let Some(tools) = detail.strip_prefix("↳ tools:") {
            item.tools = tools
                .split(',')
                .map(str::trim)
                .filter(|tool| !tool.is_empty())
                .map(str::to_string)
                .collect();
        }
    }
    items
}

fn task_plan_snapshot(records: &[SuccessfulToolRecord]) -> Option<TaskPlanSnapshot> {
    let mut items: BTreeMap<usize, TaskPlanItemSnapshot> = BTreeMap::new();

    for record in records {
        if record.name != "task_plan" {
            continue;
        }

        let action = record
            .arguments
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or_default();

        if task_plan_call_is_create(&record.arguments) {
            items.clear();
            let parsed_items = task_plan_items_from_create_arguments(&record.arguments);
            let parsed_items = if parsed_items.is_empty() {
                parse_task_plan_items_from_output(&record.output)
            } else {
                parsed_items
            };
            for item in parsed_items {
                items.insert(item.id, item);
            }
            continue;
        }

        match action {
            "list" => {
                let parsed_items = parse_task_plan_items_from_output(&record.output);
                if !parsed_items.is_empty() {
                    items.clear();
                    for item in parsed_items {
                        items.insert(item.id, item);
                    }
                }
            }
            "add" => {
                let Some(title) = record
                    .arguments
                    .get("title")
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|title| !title.is_empty())
                else {
                    continue;
                };
                let id = items.keys().next_back().copied().unwrap_or(0) + 1;
                let context = record
                    .arguments
                    .get("context")
                    .or_else(|| record.arguments.get("sub_context"))
                    .and_then(|value| value.as_str())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string);
                let tools = record
                    .arguments
                    .get("tools")
                    .or_else(|| record.arguments.get("allowed_tools"))
                    .and_then(|value| value.as_array())
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|value| value.as_str())
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                items.insert(
                    id,
                    TaskPlanItemSnapshot {
                        id,
                        title: title.to_string(),
                        status: "pending".to_string(),
                        context,
                        tools,
                    },
                );
            }
            "update" => {
                let Some(id) = record
                    .arguments
                    .get("id")
                    .and_then(|value| value.as_u64())
                    .map(|value| value as usize)
                else {
                    continue;
                };
                if let Some(existing) = items.get_mut(&id) {
                    if let Some(status) = record
                        .arguments
                        .get("status")
                        .and_then(|value| value.as_str())
                    {
                        existing.status = status.to_string();
                    }
                    if record.arguments.get("context").is_some()
                        || record.arguments.get("sub_context").is_some()
                    {
                        existing.context = record
                            .arguments
                            .get("context")
                            .or_else(|| record.arguments.get("sub_context"))
                            .and_then(|value| value.as_str())
                            .map(str::trim)
                            .filter(|value| !value.is_empty())
                            .map(str::to_string);
                    }
                    if record.arguments.get("tools").is_some()
                        || record.arguments.get("allowed_tools").is_some()
                    {
                        existing.tools = record
                            .arguments
                            .get("tools")
                            .or_else(|| record.arguments.get("allowed_tools"))
                            .and_then(|value| value.as_array())
                            .map(|values| {
                                values
                                    .iter()
                                    .filter_map(|value| value.as_str())
                                    .map(str::trim)
                                    .filter(|value| !value.is_empty())
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default();
                    }
                }
            }
            "delete" => items.clear(),
            _ => {}
        }
    }

    (!items.is_empty()).then(|| TaskPlanSnapshot {
        items: items.into_values().collect(),
    })
}

fn task_plan_progress_snapshot(records: &[SuccessfulToolRecord]) -> Option<TaskPlanProgress> {
    let snapshot = task_plan_snapshot(records)?;
    Some(TaskPlanProgress {
        total: snapshot.items.len(),
        completed: snapshot
            .items
            .iter()
            .filter(|item| item.status == "completed")
            .count(),
        resolved: snapshot
            .items
            .iter()
            .filter(|item| task_plan_status_is_terminal(&item.status))
            .count(),
    })
}

/// Injected immediately after a `task_plan create` turn where no real execution tool ran yet.
/// Drives the model to start executing step 1 without waiting for user input.
/// After web search, nudge the model to read result pages with `web_fetch`
/// instead of stopping at search snippets.
fn build_post_web_search_fetch_prompt(
    records: &[SuccessfulToolRecord],
    web_fetch_available: bool,
) -> Option<String> {
    if !web_fetch_available {
        return None;
    }
    let (search_idx, search_record) = records
        .iter()
        .enumerate()
        .rev()
        .find(|(_, record)| record.name == "web_search_tool")?;

    let urls = extract_candidate_urls_from_search_output(&search_record.output, 5);
    if urls.is_empty() {
        return None;
    }

    let fetched_after_search: HashSet<String> = records[search_idx + 1..]
        .iter()
        .filter(|record| record.name == "web_fetch")
        .filter_map(|record| {
            record
                .arguments
                .get("url")
                .and_then(|value| value.as_str())
                .map(normalize_url_for_tracking)
        })
        .collect();

    let pending_urls = urls
        .into_iter()
        .filter(|url| !fetched_after_search.contains(url))
        .take(3)
        .collect::<Vec<_>>();
    if pending_urls.is_empty() {
        return None;
    }

    Some(format!(
        "Internal continuation: web search returned results. \
         Now use web_fetch to read the full content of the most relevant URLs: {}. \
         Fetch them one at a time and synthesize the content into a complete answer. \
         Do not summarize only the search snippets — read the actual pages.",
        pending_urls.join(", ")
    ))
}

fn web_search_needs_fetch_continuation(
    records: &[SuccessfulToolRecord],
    web_fetch_available: bool,
) -> bool {
    web_fetch_available
        && records
            .iter()
            .any(|record| record.name == "web_search_tool")
        && !records.iter().any(|record| record.name == "web_fetch")
}

fn normalize_url_for_tracking(url: &str) -> String {
    url.trim()
        .trim_end_matches(['.', ',', ';', ':', ')', ']', '}', '>'])
        .to_string()
}

fn extract_candidate_urls_from_search_output(output: &str, limit: usize) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut urls = Vec::new();

    for found in URL_IN_TEXT_REGEX.find_iter(output) {
        let normalized = normalize_url_for_tracking(found.as_str());
        if normalized.is_empty() {
            continue;
        }
        if seen.insert(normalized.clone()) {
            urls.push(normalized);
            if urls.len() >= limit {
                break;
            }
        }
    }

    urls
}

fn is_deep_web_research_request(text: &str) -> bool {
    static DEEP_RESEARCH_HINT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(
            r"(?ix)\b(in[-\s]?depth|deep(?:er)?|comprehensive|thorough|agentic(?:ally)?|research|analyze|analysis|compare|cross[-\s]?check|multiple\s+sources|source\s+validation)\b",
        )
        .unwrap()
    });
    DEEP_RESEARCH_HINT_RE.is_match(text)
}

fn build_agentic_web_research_followup_prompt(
    history: &[ChatMessage],
    records: &[SuccessfulToolRecord],
    web_fetch_available: bool,
) -> Option<String> {
    if !web_fetch_available {
        return None;
    }
    let (search_idx, search_record) = records
        .iter()
        .enumerate()
        .rev()
        .find(|(_, record)| record.name == "web_search_tool")?;

    let candidate_urls = extract_candidate_urls_from_search_output(&search_record.output, 6);
    if candidate_urls.is_empty() {
        return None;
    }

    let fetched_after_search = records[search_idx + 1..]
        .iter()
        .filter(|record| record.name == "web_fetch")
        .filter_map(|record| {
            record
                .arguments
                .get("url")
                .and_then(|value| value.as_str())
                .map(normalize_url_for_tracking)
        })
        .collect::<Vec<_>>();
    let fetched_set = fetched_after_search.iter().cloned().collect::<HashSet<_>>();
    let fetched_count = fetched_after_search.len();

    let request = latest_external_user_request(history).unwrap_or_default();
    let target_fetches = if is_deep_web_research_request(request) {
        3
    } else {
        1
    };
    if fetched_count >= target_fetches {
        return None;
    }

    let pending = candidate_urls
        .into_iter()
        .filter(|url| !fetched_set.contains(url))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return None;
    }

    let needed = target_fetches.saturating_sub(fetched_count).max(1);
    let next_urls = pending.into_iter().take(needed.min(2)).collect::<Vec<_>>();
    if next_urls.is_empty() {
        return None;
    }

    let mode_text = if target_fetches > 1 {
        format!(
            "This is a deeper online-research task (fetched {fetched_count}/{target_fetches} pages so far)."
        )
    } else {
        "Fetch at least one primary source page before finalizing.".to_string()
    };

    Some(format!(
        "Internal continuation: {mode_text} \
         Next, call `web_fetch` on the most relevant pending URL(s): {}. \
         Fetch one URL at a time. After each fetch, decide whether more evidence is needed; \
         if yes, continue fetching, otherwise provide a grounded final answer with source URLs.",
        next_urls.join(", ")
    ))
}

fn build_post_plan_create_start_prompt(records: &[SuccessfulToolRecord]) -> Option<String> {
    let snapshot = task_plan_snapshot(records)?;
    let next = snapshot
        .items
        .iter()
        .find(|item| !task_plan_status_is_terminal(&item.status))?;

    let mut prompt = format!(
        "Internal continuation: task plan created ({total} steps). \
         Begin execution NOW — do not summarize, do not describe what you will do, just call tools. \
         do not ask the user what to do next.\n\
         Full plan:\n",
        total = snapshot.items.len(),
    );
    for item in snapshot.items.iter().take(20) {
        prompt.push_str(&format!(
            "  [{}] [{}] {}\n",
            item.id, item.status, item.title
        ));
    }
    if snapshot.items.len() > 20 {
        prompt.push_str(&format!(
            "  ... ({} more steps)\n",
            snapshot.items.len() - 20
        ));
    }
    if let Some(context) = next.context.as_deref() {
        prompt.push_str(&format!("Step context: {context}\n"));
    }
    if !next.tools.is_empty() {
        prompt.push_str(&format!("Expected tools: {}\n", next.tools.join(", ")));
    }
    prompt.push_str(&format!(
        "Execute step [{id}]: {title} — call the appropriate tool right now.",
        id = next.id,
        title = next.title,
    ));

    Some(prompt)
}

fn task_plan_execution_started(records: &[SuccessfulToolRecord]) -> bool {
    records.iter().any(|record| {
        !matches!(
            record.name.as_str(),
            "task_plan" | "memory_store" | "memory_recall"
        )
    })
}

fn build_task_plan_execution_followup_prompt(records: &[SuccessfulToolRecord]) -> Option<String> {
    let snapshot = task_plan_snapshot(records)?;
    let progress = task_plan_progress_snapshot(records)?;
    if progress.resolved >= progress.total {
        return None;
    }

    let mut prompt = format!(
        "Internal continuation: a task plan is active ({}/{} completed; {}/{} resolved).",
        progress.completed, progress.total, progress.resolved, progress.total
    );

    prompt.push_str("\nActive task plan:");
    for item in snapshot.items.iter().take(8) {
        prompt.push_str(&format!(
            "\n- [{}] [{}] {}",
            item.id, item.status, item.title
        ));
    }

    if let Some(next_step) = snapshot
        .items
        .iter()
        .find(|item| !task_plan_status_is_terminal(&item.status))
    {
        prompt.push_str(&format!(
            "\nNext incomplete step: [{}] {}",
            next_step.id, next_step.title
        ));
        if let Some(context) = next_step.context.as_deref() {
            prompt.push_str(&format!("\nStep context: {context}"));
        }
        if !next_step.tools.is_empty() {
            prompt.push_str(&format!("\nExpected tools: {}", next_step.tools.join(", ")));
        }
    }

    if task_plan_execution_started(records) {
        prompt.push_str(
            "\nExecution has already started in this turn. Do NOT emit `task_plan` create/update calls now, and do NOT recreate the plan. Continue directly with the next incomplete step using real tools. Do not ask the user what to do next unless blocked.",
        );
    } else {
        prompt.push_str(
            "\nIf the last action finished a planned step, emit a `task_plan` update for that step, then continue with the next incomplete step using real tools. Do not ask the user what to do next unless blocked.",
        );
    }

    Some(prompt)
}

fn build_file_write_continuation_prompt(
    records: &[SuccessfulToolRecord],
    history: &[ChatMessage],
) -> Option<String> {
    // Only fire when no task_plan is active (task_plan handles its own continuation)
    if task_plan_snapshot(records).is_some() {
        return None;
    }
    let last = records.last()?;
    if last.name != "file_write" {
        return None;
    }
    let path = last
        .arguments
        .get("path")
        .or_else(|| last.arguments.get("file_path"))
        .and_then(|v| v.as_str())
        .unwrap_or("the file");
    let user_task = latest_external_user_request(history)
        .map(|t| format!("\nOriginal task: {}", truncate_with_ellipsis(t.trim(), 200)))
        .unwrap_or_default();
    Some(format!(
        "Written: `{path}`.{user_task}\n\
         If there are remaining files or steps in the task, continue now — \
         write the next file or execute the next action immediately. \
         Do NOT wait for user confirmation. Only stop when all steps are done."
    ))
}

/// When a multi-step task has accumulated enough tool calls without a task_plan,
/// prompt the model to create one retrospectively — marking completed steps and
/// listing what remains. This hands off to the plan-execution machinery so the
/// rest of the task is tracked and nudged reliably.
fn build_retrospective_task_plan_prompt(
    records: &[SuccessfulToolRecord],
    history: &[ChatMessage],
) -> Option<String> {
    // Bail if a plan already exists
    if task_plan_snapshot(records).is_some() {
        return None;
    }

    // Only fire for meaningful action tools, not metadata lookups
    const MEANINGFUL: &[&str] = &["file_write", "shell", "web_fetch", "db_query", "file_read"];
    let meaningful: Vec<&SuccessfulToolRecord> = records
        .iter()
        .filter(|r| MEANINGFUL.contains(&r.name.as_str()))
        .collect();

    if meaningful.len() < RETROSPECTIVE_PLAN_THRESHOLD {
        return None;
    }

    let completed_summary = meaningful
        .iter()
        .map(|r| match r.name.as_str() {
            "file_write" => {
                let path = r
                    .arguments
                    .get("path")
                    .or_else(|| r.arguments.get("file_path"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("file");
                format!("Write {}", path.rsplit('/').next().unwrap_or(path))
            }
            "shell" => {
                let cmd = r
                    .arguments
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("command");
                format!("Run: {}", truncate_with_ellipsis(cmd, 50))
            }
            "web_fetch" => {
                let url = r
                    .arguments
                    .get("url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("url");
                format!("Fetch: {}", truncate_with_ellipsis(url, 50))
            }
            "db_query" => {
                let conn = r
                    .arguments
                    .get("connection")
                    .and_then(|v| v.as_str())
                    .unwrap_or("db");
                format!("Query {conn}")
            }
            _ => format!("Use: {}", r.name),
        })
        .collect::<Vec<_>>()
        .join("; ");

    let user_task = latest_external_user_request(history)
        .map(|t| format!("\nOriginal task: {}", truncate_with_ellipsis(t.trim(), 300)))
        .unwrap_or_default();

    Some(format!(
        "Internal: you have completed {n} steps without a task plan.{user_task}\n\
         Completed so far: {completed_summary}\n\n\
         Call task_plan(action:create) NOW with ALL steps — mark the completed ones \
         as 'completed' and list every remaining step as 'pending'. \
         After creating the plan, immediately continue executing the next pending step \
         with a real tool call. Do not stop or ask the user.",
        n = meaningful.len(),
    ))
}

/// Build a compaction summary for the active tool loop without calling the
/// model. Reuses the same plan snapshot and recent-results state that
/// [`build_working_state_prompt`] injects on every iteration, so the raw
/// messages being folded away are replaced by exactly the checklist + latest
/// results the model already sees fresh each turn — not the entire chat
/// history a model-based summarizer would otherwise have to re-read.
///
/// `to_compact` must be the specific message slice being folded away (see
/// [`compaction_range`]), not the full history — the fallback path below
/// transcribes it verbatim, and using the full history there would echo
/// "old" content back into the surviving summary message.
fn deterministic_compaction_summary(
    to_compact: &[ChatMessage],
    successful_records: &[SuccessfulToolRecord],
    failed_records: &[FailedToolRecord],
) -> String {
    let mut lines = Vec::new();

    if let Some(snapshot) = task_plan_snapshot(successful_records) {
        lines.push("Active task plan:".to_string());
        for item in snapshot.items.iter().take(8) {
            lines.push(format!("- [{}] [{}] {}", item.id, item.status, item.title));
        }
    }

    let recent_results = successful_records.iter().rev().take(5).collect::<Vec<_>>();
    if !recent_results.is_empty() {
        lines.push("Recent verified tool results:".to_string());
        for record in recent_results.into_iter().rev() {
            let output = truncate_with_ellipsis(&scrub_credentials(record.output.trim()), 180);
            lines.push(format!("- {} => {}", record.name, output));
        }
    }

    if let Some(record) = failed_records.last() {
        lines.push(format!(
            "Last tool error: {} => {}",
            record.name,
            truncate_with_ellipsis(&scrub_credentials(record.output.trim()), 180)
        ));
    }

    if lines.is_empty() {
        // No plan/tool-record state yet (e.g. a chain that hasn't run
        // task_plan or any tool successfully). There is nothing worth
        // preserving verbatim without a model to paraphrase it, and echoing
        // the raw old messages back in would defeat the point of compacting
        // them out — so collapse to a short deterministic placeholder
        // instead.
        return format!(
            "{} earlier tool-chain message(s) omitted (no plan or tool results yet to preserve).",
            to_compact.len()
        );
    }

    lines.join("\n")
}

fn build_working_state_prompt(
    history: &[ChatMessage],
    successful_records: &[SuccessfulToolRecord],
    failed_records: &[FailedToolRecord],
) -> Option<String> {
    if successful_records.is_empty() && failed_records.is_empty() {
        return None;
    }

    let mut lines = vec!["Internal working state:".to_string()];

    if let Some(request) = latest_external_user_request(history) {
        lines.push(format!(
            "- Current user task: {}",
            truncate_with_ellipsis(request.trim(), 240)
        ));
    }

    if let Some(snapshot) = task_plan_snapshot(successful_records) {
        lines.push("- Active task plan:".to_string());
        for item in snapshot.items.iter().take(8) {
            lines.push(format!("- [{}] [{}] {}", item.id, item.status, item.title));
        }
        if let Some(next_step) = snapshot
            .items
            .iter()
            .find(|item| !task_plan_status_is_terminal(&item.status))
        {
            lines.push(format!(
                "- Next incomplete step: [{}] {}",
                next_step.id, next_step.title
            ));
        }
    }

    let recent_results = successful_records.iter().rev().take(3).collect::<Vec<_>>();
    if !recent_results.is_empty() {
        lines.push("- Recent verified tool results:".to_string());
        for record in recent_results.into_iter().rev() {
            let output = truncate_with_ellipsis(&scrub_credentials(record.output.trim()), 180);
            lines.push(format!("- {} => {}", record.name, output));
        }
    }

    if let Some(record) = failed_records.last() {
        lines.push(format!(
            "- Last tool error: {} => {}",
            record.name,
            truncate_with_ellipsis(&scrub_credentials(record.output.trim()), 180)
        ));
    }

    let last_db_query_with_rows = successful_records
        .iter()
        .rev()
        .find(|r| r.name == "db_query")
        .is_some_and(|r| {
            r.output.starts_with("Query returned ")
                && !r.output.starts_with("Query returned no rows")
        });
    if last_db_query_with_rows {
        lines.push(
            "- Database query complete. Present the results above to the user. Do NOT call db_query again.".to_string(),
        );
    } else {
        lines.push(
            "- Use this only as grounding for the current task. Continue with real tool calls when action is still required.".to_string(),
        );
    }

    Some(lines.join("\n"))
}

fn looks_like_failed_tool_followthrough(text: &str, records: &[FailedToolRecord]) -> bool {
    let Some(record) = records.last() else {
        return false;
    };

    let lowered = text.trim().to_ascii_lowercase();
    if lowered.is_empty() {
        return false;
    }

    match record.name.as_str() {
        "task_plan" => [
            "task_plan tool requires",
            "parameter 'tasks'",
            "non-empty array of task objects",
            "let me create a simple task plan",
            "use this example task plan",
            "specific project or task you have in mind",
            "would you like me to",
        ]
        .iter()
        .any(|needle| lowered.contains(needle)),
        "shell" => [
            "i need a command to execute",
            "need a command to execute",
            "i need a command",
            "need a command",
            "please provide a command",
            "provide a command to execute",
            "would you like me to create it first",
            "would you like me to write it first",
            "please provide the correct path",
            "please share the correct path",
            "the file path doesn't exist",
            "the file path does not exist",
            "the file doesn't exist",
            "the file does not exist",
            "create it first",
            "/path/to/script.py",
        ]
        .iter()
        .any(|needle| lowered.contains(needle)),
        _ => false,
    }
}

fn should_retry_with_tool_result_grounding(text: &str, records: &[SuccessfulToolRecord]) -> bool {
    looks_like_tool_result_misinterpretation(text)
        || looks_like_file_read_answer_mismatch(text, records)
        || looks_like_file_write_content_mismatch(text, records)
        || looks_like_task_plan_followup_question(text, records)
}

fn record_argument_string<'a>(record: &'a SuccessfulToolRecord, key: &str) -> Option<&'a str> {
    record.arguments.get(key).and_then(|value| value.as_str())
}

fn command_mentions_path(command: &str, path: &str) -> bool {
    if command.contains(path) {
        return true;
    }

    path.rsplit('/')
        .next()
        .is_some_and(|name| !name.is_empty() && command.contains(name))
}

fn shell_command_runs_python_script(command: &str, path: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    lowered.contains("python") && command_mentions_path(command, path)
}

fn shell_output_confirms_cleanup(output: &str) -> bool {
    let lowered = output.to_ascii_lowercase();
    lowered.contains("deleted successfully")
        || lowered.contains("file deleted")
        || lowered.contains("removed successfully")
}

fn shell_command_removes_path(command: &str, path: &str) -> bool {
    command.contains("rm ") && command_mentions_path(command, path)
}

fn detect_repeated_file_write_stall(records: &[SuccessfulToolRecord]) -> Option<(String, usize)> {
    let mut target_path: Option<String> = None;
    let mut write_count = 0usize;

    for record in records.iter().rev() {
        match record.name.as_str() {
            "file_write" => {
                let Some(path) = record_argument_string(record, "path")
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                else {
                    break;
                };

                match target_path.as_deref() {
                    None => {
                        target_path = Some(path.to_string());
                        write_count = 1;
                    }
                    Some(existing) if existing == path => {
                        write_count = write_count.saturating_add(1);
                    }
                    Some(_) => break,
                }
            }
            "file_read" => {
                let Some(existing) = target_path.as_deref() else {
                    break;
                };
                if record_argument_string(record, "path")
                    .map(str::trim)
                    .is_some_and(|path| path == existing)
                {
                    continue;
                }
                break;
            }
            "task_plan" => {
                if target_path.is_none() {
                    break;
                }
            }
            "shell" => {
                let Some(existing) = target_path.as_deref() else {
                    break;
                };
                if record_argument_string(record, "command")
                    .is_some_and(|command| command_mentions_path(command, existing))
                {
                    break;
                }
                break;
            }
            _ => break,
        }
    }

    (write_count >= REPEATED_FILE_WRITE_STALL_THRESHOLD)
        .then(|| target_path.map(|path| (path, write_count)))
        .flatten()
}

fn synthesize_python_execution_answer(records: &[SuccessfulToolRecord]) -> Option<String> {
    for (idx, run_record) in records.iter().enumerate().rev() {
        if run_record.name != "shell" {
            continue;
        }

        let Some(command) = record_argument_string(run_record, "command") else {
            continue;
        };
        let Some(file_write_record) = records[..=idx].iter().rev().find(|record| {
            record.name == "file_write"
                && record_argument_string(record, "path").is_some_and(|path| {
                    path.ends_with(".py") && shell_command_runs_python_script(command, path)
                })
        }) else {
            continue;
        };

        let Some(path) = record_argument_string(file_write_record, "path") else {
            continue;
        };
        let output = run_record.output.trim();
        if output.is_empty() {
            continue;
        }

        let contents = records
            .iter()
            .rev()
            .find(|record| {
                record.name == "file_read"
                    && record_argument_string(record, "path")
                        .is_some_and(|candidate| candidate == path)
            })
            .and_then(|record| extract_file_read_content(&record.output))
            .or_else(|| {
                record_argument_string(file_write_record, "content")
                    .map(str::trim)
                    .filter(|content| !content.is_empty())
                    .map(ToString::to_string)
            });

        let cleanup_verified = shell_output_confirms_cleanup(output)
            || records.iter().any(|record| {
                record.name == "shell"
                    && record_argument_string(record, "command")
                        .is_some_and(|candidate| shell_command_removes_path(candidate, path))
            });

        let mut answer = if output_shows_uncaught_exception(output) {
            format!("The script `{path}` was created, but it failed when executed.")
        } else {
            format!("The script `{path}` was created and executed successfully.")
        };
        answer.push_str("\n\nOutput:\n\n```text\n");
        answer.push_str(output);
        answer.push_str("\n```");

        if let Some(contents) = contents {
            answer.push_str("\n\nScript contents:\n\n```python\n");
            answer.push_str(&contents);
            answer.push_str("\n```");
        }

        if cleanup_verified {
            answer.push_str("\n\nThe file was deleted after execution.");
        }

        return Some(answer);
    }

    None
}

fn should_short_circuit_after_tool_execution(
    history: &[ChatMessage],
    records: &[SuccessfulToolRecord],
) -> bool {
    // Short-circuit immediately after a successful task_plan create so local models
    // cannot embellish the plan in prose before execution has started.
    let has_task_plan_create = records.iter().any(|record| {
        record.name == "task_plan"
            && record
                .arguments
                .get("action")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                == "create"
    });
    if has_task_plan_create {
        // Only short-circuit if this is the last (most recent) significant tool, i.e. no
        // non-task_plan execution tools followed it — we don't want to suppress the
        // final answer after a real execution turn that happened to start with a plan.
        let last_non_plan = records
            .iter()
            .rev()
            .find(|r| r.name != "task_plan")
            .map(|r| r.name.as_str());
        let last_is_plan = records.last().is_some_and(|r| r.name == "task_plan");
        if (last_is_plan || last_non_plan.is_none()) && !should_auto_plan_current_request(history) {
            return true;
        }
    }

    // Short-circuit after file_write + shell (python execution) with non-empty output.
    // We stop as soon as the script has run — cleanup is optional, the result is already
    // available. Without this, local models spin through additional iterations and time out.
    for (idx, run_record) in records.iter().enumerate().rev() {
        if run_record.name != "shell" {
            continue;
        }

        let command = match record_argument_string(run_record, "command") {
            Some(command) => command,
            None => continue,
        };
        let _path = match records[..=idx].iter().rev().find_map(|record| {
            (record.name == "file_write")
                .then(|| record_argument_string(record, "path"))
                .flatten()
                .filter(|path| {
                    path.ends_with(".py") && shell_command_runs_python_script(command, path)
                })
        }) {
            Some(path) => path,
            None => continue,
        };

        let output = run_record.output.trim();
        if output.is_empty() {
            continue;
        }

        if output_shows_uncaught_exception(output) {
            // The script crashed — let the loop continue so the model can see the
            // failure and react (fix the bug, retry, explain) instead of being cut
            // off right as the error becomes visible.
            continue;
        }

        // Non-empty, non-failing shell output after a matching file_write — we're done.
        return true;
    }

    false
}

fn synthesize_grounded_final_answer(
    records: &[SuccessfulToolRecord],
    history: &[ChatMessage],
) -> Option<String> {
    let last_task_plan = records
        .iter()
        .enumerate()
        .rev()
        .find(|(_, record)| record.name == "task_plan");
    if let Some((idx, record)) = last_task_plan {
        let has_later_execution = records[idx + 1..]
            .iter()
            .any(|later| later.name != "task_plan");
        let is_create = task_plan_record_is_create(record);
        let action = record
            .arguments
            .get("action")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if is_create && !has_later_execution {
            // For action-oriented requests, do not return a terminal "plan created" answer —
            // the orchestration loop must continue into execution, not stop here.
            // Use a broader check than should_auto_plan_current_request (which requires 4+
            // steps): any history with a non-planning-only request suppresses the summary.
            let has_action_history = latest_external_user_request(history)
                .is_some_and(|req| !is_planning_only_request(req));
            if should_auto_plan_current_request(history) || has_action_history {
                return None;
            }
            if let Some(snapshot) = task_plan_snapshot(&records[..=idx]) {
                let mut answer = format!("Task plan created with {} steps:", snapshot.items.len());
                for item in snapshot.items {
                    answer.push_str(&format!("\n{}. {}", item.id, item.title));
                }
                return Some(answer);
            }
        }

        if action == "list" && !has_later_execution && !record.output.trim().is_empty() {
            return Some(record.output.trim().to_string());
        }
    }

    // Research/inspection tools (web_search, db_query, file_read) only get to
    // supply the final answer if nothing more substantive happened since —
    // otherwise a stale early-research-phase call (e.g. the web search that
    // kicked off a coding task) outranks the actual outcome (a server that
    // was started, files that were written) just because it's checked first
    // in this fixed tool-type order below.
    let is_superseded = |idx: usize| -> bool {
        records[idx + 1..].iter().any(|later| {
            matches!(
                later.name.as_str(),
                "file_write" | "file_edit" | "apply_patch" | "shell" | "code_run"
            )
        })
    };

    let last_web_search = records
        .iter()
        .enumerate()
        .rev()
        .find(|(_, record)| record.name == "web_search_tool");
    if let Some((idx, record)) = last_web_search {
        if !is_superseded(idx) {
            if let Some(url) = extract_preferred_url(&record.output) {
                return Some(format!("The main URL is {url}"));
            }
            if !record.output.trim().is_empty() {
                return Some(record.output.trim().to_string());
            }
        }
    }

    let last_db_query = records
        .iter()
        .enumerate()
        .rev()
        .find(|(_, r)| r.name == "db_query");
    if let Some((idx, record)) = last_db_query {
        if !is_superseded(idx)
            && record.output.starts_with("Query returned ")
            && !record.output.starts_with("Query returned no rows")
        {
            return Some(record.output.trim().to_string());
        }
    }

    let last_file_read = records
        .iter()
        .enumerate()
        .rev()
        .find(|(_, record)| record.name == "file_read");
    if let Some((idx, record)) = last_file_read {
        let path = record
            .arguments
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("the file");
        if !is_superseded(idx) {
            if let Some(content) = extract_file_read_content(&record.output) {
                return Some(format!(
                    "The file `{path}` contains:\n\n```\n{content}\n```"
                ));
            }
        }
    }

    if let Some(answer) = synthesize_python_execution_answer(records) {
        return Some(answer);
    }

    let last_file_write = records
        .iter()
        .rev()
        .find(|record| record.name == "file_write");
    if let Some(record) = last_file_write {
        let path = record
            .arguments
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("the file");
        if let Some(content) = record
            .arguments
            .get("content")
            .and_then(|value| value.as_str())
            .map(str::trim)
            .filter(|content| !content.is_empty() && !content.contains('\n'))
        {
            return Some(format!(
                "The file `{path}` was written successfully with content:\n\n```\n{content}\n```"
            ));
        }

        return Some(format!("The file `{path}` was written successfully."));
    }

    None
}

async fn return_final_response(
    history: &mut Vec<ChatMessage>,
    final_text: String,
    on_delta: Option<&tokio::sync::mpsc::Sender<String>>,
    cancellation_token: Option<&CancellationToken>,
    response_text: Option<&str>,
    response_streamed_live: bool,
) -> Result<String> {
    if let Some(tx) = on_delta {
        let should_emit_post_hoc_chunks =
            !response_streamed_live || response_text.is_none_or(|text| final_text != text);
        if !should_emit_post_hoc_chunks {
            history.push(ChatMessage::assistant(final_text.clone()));
            return Ok(final_text);
        }

        let _ = tx.send(DRAFT_CLEAR_SENTINEL.to_string()).await;
        let mut chunk = String::new();
        for word in final_text.split_inclusive(char::is_whitespace) {
            if cancellation_token.is_some_and(CancellationToken::is_cancelled) {
                return Err(ToolLoopCancelled.into());
            }
            chunk.push_str(word);
            if chunk.len() >= STREAM_CHUNK_MIN_CHARS
                && tx.send(std::mem::take(&mut chunk)).await.is_err()
            {
                break;
            }
        }
        if !chunk.is_empty() {
            let _ = tx.send(chunk).await;
        }
    }

    history.push(ChatMessage::assistant(final_text.clone()));
    Ok(final_text)
}

#[derive(Debug)]
pub(crate) struct ToolLoopCancelled;

impl std::fmt::Display for ToolLoopCancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("tool loop cancelled")
    }
}

impl std::error::Error for ToolLoopCancelled {}

pub(crate) fn is_tool_loop_cancelled(err: &anyhow::Error) -> bool {
    err.chain().any(|source| source.is::<ToolLoopCancelled>())
}

/// Carries the model's last no-tool-call text alongside the "repeated intent"
/// bail so callers that can usefully consume ungrounded text (e.g. a
/// federation worker reporting back to its controller) don't lose it. Display
/// matches the original bail message exactly, so any caller that only reads
/// `.to_string()` sees no change in behavior.
pub(crate) struct UngroundedFinalText {
    pub text: String,
    pub retry_count: usize,
}

impl std::fmt::Display for UngroundedFinalText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Model repeated intent text without a tool call after {} retries",
            self.retry_count
        )
    }
}

impl std::fmt::Debug for UngroundedFinalText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UngroundedFinalText(retry_count={})", self.retry_count)
    }
}

impl std::error::Error for UngroundedFinalText {}

/// If `err` is (or wraps) an [`UngroundedFinalText`], return the model's last
/// raw text and the retry count that triggered the fast-exit.
pub(crate) fn ungrounded_final_text(err: &anyhow::Error) -> Option<(&str, usize)> {
    err.chain()
        .find_map(|source| source.downcast_ref::<UngroundedFinalText>())
        .map(|marker| (marker.text.as_str(), marker.retry_count))
}

pub(crate) fn is_tool_iteration_limit_error(err: &anyhow::Error) -> bool {
    err.chain().any(|source| {
        source
            .to_string()
            .contains("Agent exceeded maximum tool iterations")
    })
}

#[derive(Debug, Default)]
struct StreamedChatOutcome {
    response_text: String,
    forwarded_live_deltas: bool,
}

fn looks_like_streamed_tool_payload(window: &str) -> bool {
    let lowered = window.to_ascii_lowercase();
    lowered.contains("<tool_call")
        || lowered.contains("<toolcall")
        || lowered.contains("\"tool_calls\"")
        || lowered.contains("\"tool\":")
        || lowered.contains("json{")
        || lowered.contains("shell(")
        || lowered.contains("file_read(")
        || lowered.contains("file_write(")
        || lowered.contains("'''bash")
        || lowered.contains("'''sh")
        || lowered.contains("'''shell")
        || lowered.contains("```bash")
        || lowered.contains("```sh")
        || lowered.contains("```shell")
}

async fn call_provider_chat(
    provider: &dyn Provider,
    messages: &[ChatMessage],
    request_tools: Option<&[crate::tools::ToolSpec]>,
    model: &str,
    temperature: f64,
    cancellation_token: Option<&CancellationToken>,
    on_delta: Option<&tokio::sync::mpsc::Sender<String>>,
) -> Result<crate::providers::ChatResponse> {
    let chat_future = provider.chat(
        ChatRequest {
            messages,
            tools: request_tools,
        },
        model,
        temperature,
    );
    tokio::pin!(chat_future);

    let started_at = Instant::now();
    let mut heartbeat = tokio::time::interval(Duration::from_secs(MODEL_PROGRESS_HEARTBEAT_SECS));
    // Skip interval's immediate first tick; the caller already emitted
    // "Thinking..." before entering this function.
    heartbeat.tick().await;

    loop {
        if let Some(token) = cancellation_token {
            tokio::select! {
                () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                result = &mut chat_future => return result,
                _ = heartbeat.tick(), if on_delta.is_some() => {
                    if let Some(tx) = on_delta {
                        let elapsed = started_at.elapsed().as_secs();
                        let _ = tx.send(format!(
                            "{DRAFT_PROGRESS_SENTINEL}🧠 Still reasoning… {elapsed}s (this is a resumable local inference segment)\n"
                        )).await;
                    }
                }
            }
        } else {
            tokio::select! {
                result = &mut chat_future => return result,
                _ = heartbeat.tick(), if on_delta.is_some() => {
                    if let Some(tx) = on_delta {
                        let elapsed = started_at.elapsed().as_secs();
                        let _ = tx.send(format!(
                            "{DRAFT_PROGRESS_SENTINEL}🧠 Still reasoning… {elapsed}s (this is a resumable local inference segment)\n"
                        )).await;
                    }
                }
            }
        }
    }
}

/// A provider reaching its output cap is a normal checkpoint boundary for a
/// long local run, not a failed tool-followthrough retry. Keep this prompt
/// short: the assistant's partial response is already in the history.
fn build_output_budget_continuation_prompt(thinking_only: bool) -> String {
    let boundary = if thinking_only {
        "The prior segment used its reasoning budget before producing visible text."
    } else {
        "The prior segment used its output/reasoning budget and its partial response is already in the conversation."
    };

    format!(
        "Internal continuation: {boundary} This is a normal checkpoint, not a failure. \
         Continue the same task now. Do not repeat completed reasoning or already-emitted text. \
         If work remains, emit the next real tool call or the remaining answer; if the task is genuinely complete, give a concise final answer."
    )
}

async fn consume_provider_streaming_response(
    provider: &dyn Provider,
    messages: &[ChatMessage],
    model: &str,
    temperature: f64,
    cancellation_token: Option<&CancellationToken>,
    on_delta: Option<&tokio::sync::mpsc::Sender<String>>,
) -> Result<StreamedChatOutcome> {
    let mut provider_stream = provider.stream_chat_with_history(
        messages,
        model,
        temperature,
        crate::providers::traits::StreamOptions::new(true),
    );
    let mut outcome = StreamedChatOutcome::default();
    let mut delta_sender = on_delta;
    let mut suppress_forwarding = false;
    let mut marker_window = String::new();

    loop {
        let next_chunk = if let Some(token) = cancellation_token {
            tokio::select! {
                () = token.cancelled() => return Err(ToolLoopCancelled.into()),
                chunk = provider_stream.next() => chunk,
            }
        } else {
            provider_stream.next().await
        };

        let Some(chunk_result) = next_chunk else {
            break;
        };

        let chunk = chunk_result.map_err(|err| anyhow::anyhow!("provider stream error: {err}"))?;
        if chunk.is_final {
            break;
        }
        if chunk.delta.is_empty() {
            continue;
        }

        outcome.response_text.push_str(&chunk.delta);
        marker_window.push_str(&chunk.delta);

        if marker_window.len() > STREAM_TOOL_MARKER_WINDOW_CHARS {
            let keep_from = marker_window.len() - STREAM_TOOL_MARKER_WINDOW_CHARS;
            let boundary = marker_window
                .char_indices()
                .find(|(idx, _)| *idx >= keep_from)
                .map_or(0, |(idx, _)| idx);
            marker_window.drain(..boundary);
        }

        if !suppress_forwarding && looks_like_streamed_tool_payload(&marker_window) {
            suppress_forwarding = true;
            if outcome.forwarded_live_deltas {
                if let Some(tx) = delta_sender {
                    let _ = tx.send(DRAFT_CLEAR_SENTINEL.to_string()).await;
                }
                outcome.forwarded_live_deltas = false;
            }
        }

        if suppress_forwarding {
            continue;
        }

        if let Some(tx) = delta_sender {
            if !outcome.forwarded_live_deltas {
                let _ = tx.send(DRAFT_CLEAR_SENTINEL.to_string()).await;
                outcome.forwarded_live_deltas = true;
            }
            if tx.send(chunk.delta).await.is_err() {
                delta_sender = None;
            }
        }
    }

    Ok(outcome)
}

/// Execute a single turn of the agent loop: send messages, parse tool calls,
/// execute tools, and loop until the LLM produces a final text response.
/// When `silent` is true, suppresses stdout (for channel use).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn agent_turn(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
) -> Result<String> {
    run_tool_call_loop(
        provider,
        history,
        tools_registry,
        observer,
        provider_name,
        model,
        temperature,
        silent,
        None,
        "channel",
        multimodal_config,
        max_tool_iterations,
        None,
        None,
        None,
        &[],
    )
    .await
}

/// Run the tool loop with channel reply_target context, used by channel runtimes
/// to auto-populate delivery routing for scheduled reminders.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tool_call_loop_with_reply_target(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    approval: Option<&ApprovalManager>,
    channel_name: &str,
    reply_target: Option<&str>,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<String>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
) -> Result<String> {
    TOOL_LOOP_REPLY_TARGET
        .scope(
            reply_target.map(str::to_string),
            run_tool_call_loop(
                provider,
                history,
                tools_registry,
                observer,
                provider_name,
                model,
                temperature,
                silent,
                approval,
                channel_name,
                multimodal_config,
                max_tool_iterations,
                cancellation_token,
                on_delta,
                hooks,
                excluded_tools,
            ),
        )
        .await
}

/// Run the tool loop with optional non-CLI approval context scoped to this task.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tool_call_loop_with_non_cli_approval_context(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    approval: Option<&ApprovalManager>,
    channel_name: &str,
    non_cli_approval_context: Option<NonCliApprovalContext>,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<String>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
) -> Result<String> {
    let reply_target = non_cli_approval_context
        .as_ref()
        .map(|ctx| ctx.reply_target.clone());

    TOOL_LOOP_NON_CLI_APPROVAL_CONTEXT
        .scope(
            non_cli_approval_context,
            TOOL_LOOP_REPLY_TARGET.scope(
                reply_target,
                run_tool_call_loop(
                    provider,
                    history,
                    tools_registry,
                    observer,
                    provider_name,
                    model,
                    temperature,
                    silent,
                    approval,
                    channel_name,
                    multimodal_config,
                    max_tool_iterations,
                    cancellation_token,
                    on_delta,
                    hooks,
                    excluded_tools,
                ),
            ),
        )
        .await
}

// ── Agent Tool-Call Loop ──────────────────────────────────────────────────
// Core agentic iteration: send conversation to the LLM, parse any tool
// calls from the response, execute them, append results to history, and
// repeat until the LLM produces a final text-only answer.
//
// Loop invariant: at the start of each iteration, `history` contains the
// full conversation so far (system prompt + user messages + prior tool
// results). The loop exits when:
//   • the LLM returns no tool calls (final answer), or
//   • an explicitly configured positive max_iterations is reached, or
//   • the cancellation token fires (external abort).
//
// A zero max_iterations value is deliberately unlimited. Real non-progress is
// handled by the loop's duplicate-call, repeated-failure, and empty-output
// stall detectors rather than by an arbitrary count of productive tool calls.

fn tool_loop_has_next_iteration(iteration: usize, limit: Option<usize>) -> bool {
    limit.is_none_or(|limit| iteration.saturating_add(1) < limit)
}

/// Execute a single turn of the agent loop: send messages, parse tool calls,
/// execute tools, and loop until the LLM produces a final text response.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_tool_call_loop(
    provider: &dyn Provider,
    history: &mut Vec<ChatMessage>,
    tools_registry: &[Box<dyn Tool>],
    observer: &dyn Observer,
    provider_name: &str,
    model: &str,
    temperature: f64,
    silent: bool,
    approval: Option<&ApprovalManager>,
    channel_name: &str,
    multimodal_config: &crate::config::MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    on_delta: Option<tokio::sync::mpsc::Sender<String>>,
    hooks: Option<&crate::hooks::HookRunner>,
    excluded_tools: &[String],
) -> Result<String> {
    let non_cli_approval_context = TOOL_LOOP_NON_CLI_APPROVAL_CONTEXT
        .try_with(Clone::clone)
        .ok()
        .flatten();
    let channel_reply_target = TOOL_LOOP_REPLY_TARGET
        .try_with(Clone::clone)
        .ok()
        .flatten()
        .or_else(|| {
            non_cli_approval_context
                .as_ref()
                .map(|ctx| ctx.reply_target.clone())
        });

    let configured_iteration_limit = (max_tool_iterations > 0).then_some(max_tool_iterations);
    let parallel_tools_enabled = TOOL_LOOP_PARALLEL_TOOLS_ENABLED
        .try_with(|enabled| *enabled)
        .unwrap_or(true);

    let tool_specs: Vec<crate::tools::ToolSpec> = tools_registry
        .iter()
        .filter(|tool| !excluded_tools.iter().any(|ex| ex == tool.name()))
        .map(|tool| tool.spec())
        .collect();
    let web_fetch_available = tool_specs.iter().any(|spec| spec.name == "web_fetch");
    let mut use_native_tools = TOOL_LOOP_NATIVE_TOOLS_ENABLED
        .try_with(|enabled| *enabled)
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            configured_native_tools_enabled(
                "auto",
                provider_name,
                model,
                provider.supports_native_tools(),
            )
        })
        && !tool_specs.is_empty();
    let turn_id = Uuid::new_v4().to_string();
    let mut seen_tool_signatures: HashSet<(String, String)> = HashSet::new();
    let mut missing_tool_call_retry_used = false;
    let mut duplicate_nudge_count: usize = 0;
    let mut tool_result_grounding_retry_used = false;
    let mut missing_tool_call_retry_prompt: Option<String> = None;
    let mut post_tool_execution_prompt: Option<String> = None;
    let mut retrospective_plan_injected = false;
    // True for the iteration immediately following a plan-create continuation injection.
    // Lets us detect when the model responds with a plan-summary text instead of calling tools.
    let mut successful_tool_execution_seen = false;
    // Rolling window of previous no-tool-call response texts for detecting repeated intent.
    let mut prior_no_tool_response_texts: Vec<String> = Vec::new();
    // Cumulative retry counter for trace events.
    let mut retry_count: usize = 0;
    // Number of normal checkpoint/continuations caused by a provider-side
    // per-segment output limit. This is intentionally separate from retries:
    // reaching a reasoning budget is progress, not an error.
    let mut output_budget_continuation_count: usize = 0;
    // Consecutive output-budget checkpoints that produced NO visible text at
    // all (pure reasoning, nothing else). Reset the instant a checkpoint (or
    // any other iteration) produces visible content or a tool call — only a
    // run of total silence indicates a stall.
    let mut consecutive_empty_output_budget_checkpoints: usize = 0;
    // Visible text produced before a length stop. We join it with the eventual
    // final segment so a normal continuation never silently drops the first
    // half of a long answer.
    let mut checkpointed_output_segments: Vec<String> = Vec::new();
    // How many times we've asked the model to create a task_plan before executing.
    // After a few failed attempts, stop blocking and let the model execute directly.
    let mut auto_plan_retry_count: usize = 0;
    let mut recent_successful_tool_records: Vec<SuccessfulToolRecord> = Vec::new();
    let mut recent_failed_tool_records: Vec<FailedToolRecord> = Vec::new();
    // Completing a plan item creates a semantic context boundary. On the next
    // provider request, compact older messages into a focused checkpoint so
    // the next subtask gets a fresh capsule instead of inheriting every raw
    // tool log from the previous item.
    let mut forced_history_budget: Option<usize> = None;
    let mut final_plan_verification_requested = false;
    // Counts consecutive iterations where web_search ran but web_fetch did not.
    // Resets to 0 as soon as any web_fetch succeeds in an iteration.
    let mut consecutive_web_searches_without_fetch: usize = 0;
    let mut consecutive_coordination_status_only_iterations: usize = 0;
    // Tracks (tool_name, normalized_error_prefix) for the last failed iteration to
    // detect when the same tool keeps failing with the same error class.
    let mut last_failure_signature: Option<(String, String)> = None;
    let mut consecutive_same_failure_count: usize = 0;
    let mut prompt_tool_fallback_used = false;
    // Counts consecutive iterations where ALL tool calls were duplicates (nothing new ran).
    // After the retry prompt fails to unstick the model several times, hard-exit to
    // avoid burning the entire context window on the same tool call forever.
    let mut consecutive_all_duplicate_iterations: usize = 0;
    let mut early_exit_reason: Option<(&'static str, String)> = None;
    let history_budget = TOOL_LOOP_MAX_HISTORY_MESSAGES
        .try_with(|max_history| *max_history)
        .ok()
        .flatten()
        .unwrap_or(DEFAULT_MAX_HISTORY_MESSAGES);
    let bypass_non_cli_approval_for_turn =
        approval.is_some_and(|mgr| channel_name != "cli" && mgr.consume_non_cli_allow_all_once());
    if bypass_non_cli_approval_for_turn {
        runtime_trace::record_event(
            "approval_bypass_one_time_all_tools_consumed",
            Some(channel_name),
            Some(provider_name),
            Some(model),
            Some(&turn_id),
            Some(true),
            Some("consumed one-time non-cli allow-all approval token"),
            serde_json::json!({}),
        );
    }

    // With the default zero value there is no fixed step count: completion, a
    // real stall/error, or explicit cancellation is the termination control.
    // A positive operator value remains a real hard cap.
    let mut effective_limit = configured_iteration_limit;
    let mut next_iteration = 0usize;

    loop {
        if effective_limit.is_some_and(|limit| next_iteration >= limit) {
            break;
        }
        let iteration = next_iteration;
        next_iteration = next_iteration.saturating_add(1);
        if cancellation_token
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(ToolLoopCancelled.into());
        }

        if let Some(retry_prompt) = missing_tool_call_retry_prompt.take() {
            history.push(ChatMessage::user(retry_prompt));
        }
        // Scoped per iteration: it is set from this turn's continuation prompt
        // and read later in the same iteration, never across iterations.
        let mut pending_post_plan_create_retry = false;
        if let Some(continuation_prompt) = post_tool_execution_prompt.take() {
            // Track whether this is a plan-create start prompt so we can detect
            // when the model responds with plan-summary text instead of tool calls.
            pending_post_plan_create_retry = continuation_prompt.contains("Begin execution NOW");
            history.push(ChatMessage::user(continuation_prompt));
        }

        let image_marker_count = multimodal::count_image_markers(history);
        if image_marker_count > 0 && !provider.supports_vision() {
            return Err(ProviderCapabilityError {
                provider: provider_name.to_string(),
                capability: "vision".to_string(),
                message: format!(
                    "received {image_marker_count} image marker(s), but this provider does not support vision input"
                ),
            }
            .into());
        }

        // Deterministic, local compaction: this loop already rebuilds a fresh
        // checklist + recent-results snapshot every iteration (see
        // `build_working_state_prompt` below), so folding older raw messages
        // through an extra model call here was pure overhead — most visibly
        // right after a plan item resolves, when `forced_history_budget`
        // shrinks and used to trigger a full provider round trip (tens of
        // seconds) just to re-derive what the checklist already captured.
        // No provider call, no Result to fail on.
        let history_len_before_compaction = history.len();
        let request_history_budget = forced_history_budget.take().unwrap_or(history_budget);
        let compacted =
            if let Some((start, end)) = compaction_range(history, request_history_budget) {
                let compaction_summary = deterministic_compaction_summary(
                    &history[start..end],
                    &recent_successful_tool_records,
                    &recent_failed_tool_records,
                );
                deterministic_compact_history(
                    history,
                    request_history_budget,
                    &compaction_summary,
                    model,
                )
            } else {
                false
            };
        if compacted {
            runtime_trace::record_event(
                "tool_loop_history_auto_compacted",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                Some(true),
                Some("compacted history locally before provider request (no model call)"),
                serde_json::json!({
                    "iteration": iteration + 1,
                    "history_budget": request_history_budget,
                    "messages_before": history_len_before_compaction,
                    "messages_after": history.len(),
                }),
            );
        }

        let mut request_history = history.clone();
        if let Some(working_state_prompt) = build_working_state_prompt(
            history,
            &recent_successful_tool_records,
            &recent_failed_tool_records,
        ) {
            request_history.push(ChatMessage::user(working_state_prompt));
        }

        let prepared_messages =
            multimodal::prepare_messages_for_provider(&request_history, multimodal_config).await?;

        // ── Progress: LLM thinking ────────────────────────────
        if let Some(ref tx) = on_delta {
            let phase = if iteration == 0 {
                "\u{1f914} Thinking...\n".to_string()
            } else {
                format!("\u{1f914} Thinking (round {})...\n", iteration + 1)
            };
            let _ = tx.send(format!("{DRAFT_PROGRESS_SENTINEL}{phase}")).await;
        }

        observer.record_event(&ObserverEvent::LlmRequest {
            provider: provider_name.to_string(),
            model: model.to_string(),
            messages_count: request_history.len(),
        });
        runtime_trace::record_event(
            "llm_request",
            Some(channel_name),
            Some(provider_name),
            Some(model),
            Some(&turn_id),
            None,
            None,
            serde_json::json!({
                "iteration": iteration + 1,
                "messages_count": request_history.len(),
            }),
        );

        let llm_started_at = Instant::now();

        // Fire void hook before LLM call
        if let Some(hooks) = hooks {
            hooks.fire_llm_input(&request_history, model).await;
        }

        // Unified path via Provider::chat so provider-specific native tool logic
        // (OpenAI/Anthropic/OpenRouter/compatible adapters) is honored.
        let request_tools = if use_native_tools {
            Some(tool_specs.as_slice())
        } else {
            None
        };
        let should_consume_provider_stream =
            on_delta.is_some() && provider.supports_streaming() && request_tools.is_none();
        let mut streamed_live_deltas = false;

        let chat_result = if should_consume_provider_stream {
            match consume_provider_streaming_response(
                provider,
                &prepared_messages.messages,
                model,
                temperature,
                cancellation_token.as_ref(),
                on_delta.as_ref(),
            )
            .await
            {
                Ok(streamed) => {
                    streamed_live_deltas = streamed.forwarded_live_deltas;
                    Ok(crate::providers::ChatResponse {
                        text: Some(streamed.response_text),
                        tool_calls: Vec::new(),
                        usage: None,
                        metrics: None,
                        reasoning_content: None,
                    })
                }
                Err(stream_err) => {
                    tracing::warn!(
                        provider = provider_name,
                        model = model,
                        iteration = iteration + 1,
                        "provider streaming failed, falling back to non-streaming chat: {stream_err}"
                    );
                    runtime_trace::record_event(
                        "llm_stream_fallback",
                        Some(channel_name),
                        Some(provider_name),
                        Some(model),
                        Some(&turn_id),
                        Some(false),
                        Some("provider stream failed; fallback to non-streaming chat"),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "error": scrub_credentials(&stream_err.to_string()),
                        }),
                    );
                    if let Some(ref tx) = on_delta {
                        let _ = tx.send(DRAFT_CLEAR_SENTINEL.to_string()).await;
                    }
                    call_provider_chat(
                        provider,
                        &prepared_messages.messages,
                        request_tools,
                        model,
                        temperature,
                        cancellation_token.as_ref(),
                        on_delta.as_ref(),
                    )
                    .await
                }
            }
        } else {
            call_provider_chat(
                provider,
                &prepared_messages.messages,
                request_tools,
                model,
                temperature,
                cancellation_token.as_ref(),
                on_delta.as_ref(),
            )
            .await
        };

        let (
            response_text,
            parsed_text,
            tool_calls,
            assistant_history_content,
            native_tool_calls,
            parse_issue_detected,
            response_streamed_live,
            response_was_empty,
            response_output_budget_exhausted,
        ) = match chat_result {
            Ok(resp) => {
                let (resp_input_tokens, resp_output_tokens) = resp
                    .usage
                    .as_ref()
                    .map(|u| (u.input_tokens, u.output_tokens))
                    .unwrap_or((None, None));

                // Forward real inference timing (Ollama nanosecond fields)
                // to the UI so its throughput display reflects decode TPS
                // and time-to-first-token instead of wall-clock estimates.
                if let (Some(metrics), Some(tx)) = (resp.metrics.as_ref(), on_delta.as_ref()) {
                    let payload = serde_json::json!({
                        "ttft_ms": metrics.ttft_ms,
                        "generation_tps": metrics.generation_tps,
                        "prefill_tps": metrics.prefill_tps,
                        "total_ms": metrics.total_ms,
                        // Real prompt token count from the provider — the true
                        // "how big is my context" number (system prompt + tools
                        // + memory + history), so the UI budget reflects reality.
                        "prompt_tokens": resp.usage.as_ref().and_then(|u| u.input_tokens),
                    });
                    let _ = tx.send(format!("{DRAFT_METRICS_SENTINEL}{payload}")).await;
                }

                observer.record_event(&ObserverEvent::LlmResponse {
                    provider: provider_name.to_string(),
                    model: model.to_string(),
                    duration: llm_started_at.elapsed(),
                    success: true,
                    error_message: None,
                    input_tokens: resp_input_tokens,
                    output_tokens: resp_output_tokens,
                });

                let response_text = resp.text_or_empty().to_string();
                let response_output_budget_exhausted = resp
                    .usage
                    .as_ref()
                    .is_some_and(|usage| usage.output_truncated);
                // True whenever the provider emitted neither visible text nor
                // a tool call. Some Ollama models expose hidden work in
                // `thinking`; others return only an empty natural-stop segment.
                // Neither case is a completed turn.
                let response_was_empty = resp.text.is_none();
                // First try native structured tool calls (OpenAI-format).
                // Fall back to text-based parsing (XML tags, markdown blocks,
                // GLM format) only if the provider returned no native calls —
                // this ensures we support both native and prompt-guided models.
                let mut calls = parse_structured_tool_calls(&resp.tool_calls);
                let mut parsed_text = String::new();

                if calls.is_empty() {
                    let (fallback_text, fallback_calls) = parse_tool_calls(&response_text);
                    if !fallback_text.is_empty() {
                        parsed_text = fallback_text;
                    }
                    calls = fallback_calls;
                }

                let parse_issue = detect_tool_call_parse_issue(&response_text, &calls);
                if let Some(parse_issue) = parse_issue.as_ref() {
                    runtime_trace::record_event(
                        "tool_call_parse_issue",
                        Some(channel_name),
                        Some(provider_name),
                        Some(model),
                        Some(&turn_id),
                        Some(false),
                        Some(&parse_issue),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "response_excerpt": truncate_with_ellipsis(
                                &scrub_credentials(&response_text),
                                600
                            ),
                        }),
                    );
                }

                runtime_trace::record_event(
                    "llm_response",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(true),
                    None,
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "duration_ms": llm_started_at.elapsed().as_millis(),
                        "input_tokens": resp_input_tokens,
                        "output_tokens": resp_output_tokens,
                        "raw_response": scrub_credentials(&response_text),
                        "native_tool_calls": resp.tool_calls.len(),
                        "parsed_tool_calls": calls.len(),
                    }),
                );

                // Preserve native tool call IDs in assistant history so role=tool
                // follow-up messages can reference the exact call id.
                let reasoning_content = resp.reasoning_content.clone();
                let assistant_history_content = if resp.tool_calls.is_empty() {
                    if use_native_tools {
                        build_native_assistant_history_from_parsed_calls(
                            &response_text,
                            &calls,
                            reasoning_content.as_deref(),
                        )
                        .unwrap_or_else(|| response_text.clone())
                    } else {
                        response_text.clone()
                    }
                } else {
                    build_native_assistant_history(
                        &response_text,
                        &resp.tool_calls,
                        reasoning_content.as_deref(),
                    )
                };

                let native_calls = resp.tool_calls;
                (
                    response_text,
                    parsed_text,
                    calls,
                    assistant_history_content,
                    native_calls,
                    parse_issue.is_some(),
                    streamed_live_deltas,
                    response_was_empty,
                    response_output_budget_exhausted,
                )
            }
            Err(e) => {
                let safe_error = crate::providers::sanitize_api_error(&e.to_string());
                observer.record_event(&ObserverEvent::LlmResponse {
                    provider: provider_name.to_string(),
                    model: model.to_string(),
                    duration: llm_started_at.elapsed(),
                    success: false,
                    error_message: Some(safe_error.clone()),
                    input_tokens: None,
                    output_tokens: None,
                });
                runtime_trace::record_event(
                    "llm_response",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some(&safe_error),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "duration_ms": llm_started_at.elapsed().as_millis(),
                    }),
                );

                if crate::providers::reliable::is_context_window_exceeded(&e) {
                    if let Some(next_history_budget) = context_pressure_history_budget(history) {
                        forced_history_budget = Some(next_history_budget);
                        runtime_trace::record_event(
                            "tool_loop_context_pressure_compacted",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(true),
                            Some(
                                "provider reached its context ceiling; compacting old history and retrying",
                            ),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "next_history_budget": next_history_budget,
                                "native_tools_preserved": use_native_tools,
                                "error": safe_error,
                            }),
                        );
                        if let Some(ref tx) = on_delta {
                            let _ = tx
                                .send(format!(
                                    "{DRAFT_PROGRESS_SENTINEL}\u{21bb} Context is full; compacting older history and retrying\n"
                                ))
                                .await;
                        }
                        continue;
                    }

                    return Err(e);
                }

                let should_retry_with_prompt_tools = use_native_tools
                    && !prompt_tool_fallback_used
                    && !tool_specs.is_empty()
                    && !successful_tool_execution_seen;
                if should_retry_with_prompt_tools {
                    prompt_tool_fallback_used = true;
                    use_native_tools = false;
                    inject_prompt_tool_fallback_instructions(history, &tool_specs);
                    runtime_trace::record_event(
                        "llm_native_tool_fallback",
                        Some(channel_name),
                        Some(provider_name),
                        Some(model),
                        Some(&turn_id),
                        Some(false),
                        Some("native tool path failed; retrying with prompt tool mode"),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "error": safe_error,
                        }),
                    );
                    if let Some(ref tx) = on_delta {
                        let _ = tx
                            .send(format!(
                                "{DRAFT_PROGRESS_SENTINEL}\u{21bb} Retrying with compatibility tool mode after native tool-call failure\n"
                            ))
                            .await;
                    }
                    continue;
                }
                if successful_tool_execution_seen {
                    if let Some(fallback) =
                        synthesize_grounded_final_answer(&recent_successful_tool_records, history)
                    {
                        runtime_trace::record_event(
                            "llm_error_grounded_fallback",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(true),
                            Some("using grounded fallback after post-tool llm error"),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "error": safe_error,
                                "text": scrub_credentials(&fallback),
                            }),
                        );
                        (
                            fallback.clone(),
                            fallback.clone(),
                            Vec::new(),
                            fallback,
                            Vec::new(),
                            false,
                            false,
                            false,
                            false,
                        )
                    } else {
                        return Err(e);
                    }
                } else {
                    return Err(e);
                }
            }
        };

        let display_text = if parsed_text.is_empty() {
            response_text.clone()
        } else {
            parsed_text
        };

        // ── Progress: LLM responded ─────────────────────────────
        if let Some(ref tx) = on_delta {
            let llm_secs = llm_started_at.elapsed().as_secs();
            if !tool_calls.is_empty() {
                let _ = tx
                    .send(format!(
                        "{DRAFT_PROGRESS_SENTINEL}\u{1f4ac} Got {} tool call(s) ({llm_secs}s)\n",
                        tool_calls.len()
                    ))
                    .await;
            }
        }

        if auto_plan_retry_count < AUTO_PLAN_RETRY_LIMIT
            && should_require_task_plan_before_execution(
                history,
                &tool_calls,
                &recent_successful_tool_records,
            )
        {
            retry_count += 1;
            auto_plan_retry_count += 1;
            missing_tool_call_retry_prompt = Some(build_auto_plan_retry_prompt());
            runtime_trace::record_event(
                "auto_plan_retry_required",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                Some(false),
                Some("multi-step request must create task_plan before execution"),
                serde_json::json!({
                    "iteration": iteration + 1,
                    "tool_calls": tool_calls.iter().map(|call| call.name.clone()).collect::<Vec<_>>(),
                }),
            );
            continue;
        }

        if tool_calls.is_empty() {
            // `num_predict` ended this model segment. Preserve its partial
            // assistant state, visibly report the checkpoint, then continue in
            // a fresh request. Do not route this through the missing-tool-call
            // retry machinery: a thinking model hitting its per-turn budget is
            // making bounded progress, not refusing the task.
            if response_output_budget_exhausted {
                output_budget_continuation_count =
                    output_budget_continuation_count.saturating_add(1);

                let checkpoint_text = display_text.trim();
                if !checkpoint_text.is_empty() && !parse_issue_detected {
                    checkpointed_output_segments.push(checkpoint_text.to_string());
                }
                let checkpoint_was_totally_empty = response_text.trim().is_empty();
                if checkpoint_was_totally_empty {
                    consecutive_empty_output_budget_checkpoints =
                        consecutive_empty_output_budget_checkpoints.saturating_add(1);
                    history.push(ChatMessage::assistant(
                        "[Local inference checkpoint: reasoning segment reached its output budget before visible text.]",
                    ));
                } else {
                    consecutive_empty_output_budget_checkpoints = 0;
                    // Keep raw provider text in history so the next model call
                    // can continue from the exact partial answer or incomplete
                    // tool-formulation boundary.
                    history.push(ChatMessage::assistant(response_text.clone()));
                }

                // A model that spends every single segment on reasoning tokens
                // alone, with nothing visible ever coming out, isn't making
                // bounded progress the way a normal long-answer continuation
                // does — it's stuck. Without this, the loop above (which has no
                // overall iteration cap by design) would checkpoint forever.
                if consecutive_empty_output_budget_checkpoints
                    >= MAX_CONSECUTIVE_EMPTY_OUTPUT_BUDGET_CHECKPOINTS
                {
                    runtime_trace::record_event(
                        "llm_output_budget_stall_hard_exit",
                        Some(channel_name),
                        Some(provider_name),
                        Some(model),
                        Some(&turn_id),
                        Some(false),
                        Some("model produced no visible output across consecutive reasoning segments; hard-exiting turn"),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "consecutive_empty_output_budget_checkpoints": consecutive_empty_output_budget_checkpoints,
                        }),
                    );
                    early_exit_reason = Some((
                        "output_budget_stall",
                        format!(
                            "Agent exited after {consecutive_empty_output_budget_checkpoints} consecutive reasoning segments produced no visible output"
                        ),
                    ));
                    break;
                }

                missing_tool_call_retry_prompt = Some(build_output_budget_continuation_prompt(
                    response_was_empty,
                ));
                // A per-segment continuation must not consume an explicitly
                // configured finite tool-iteration budget. Unlimited runs need
                // no adjustment.
                if let Some(limit) = effective_limit.as_mut() {
                    *limit = limit.saturating_add(1);
                }

                runtime_trace::record_event(
                    "llm_output_budget_checkpoint",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(true),
                    Some("provider output budget reached; checkpointed and continuing"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "continuation_count": output_budget_continuation_count,
                        "checkpointed_visible_segments": checkpointed_output_segments.len(),
                        "empty_response": response_was_empty,
                    }),
                );
                if let Some(ref tx) = on_delta {
                    let _ = tx
                        .send(format!(
                            "{DRAFT_PROGRESS_SENTINEL}↪ Reasoning segment checkpointed — continuing automatically (segment {output_budget_continuation_count})\n"
                        ))
                        .await;
                }
                continue;
            }

            // Ollama can report a natural stop after generating hidden or
            // otherwise unparseable tokens while returning neither content nor
            // a tool call. Preserve native tool mode and continue the same turn
            // instead of surfacing an "incomplete response" placeholder.
            if response_was_empty {
                retry_count = retry_count.saturating_add(1);
                missing_tool_call_retry_prompt = Some(
                    "Internal continuation: the prior inference segment ended without visible text or a valid tool call. This is not task completion. Continue the current user task now using the native runtime tools. Emit the next real tool call, or provide the complete final answer only if no work remains."
                        .to_string(),
                );
                // An empty provider segment is not a real tool-loop step, so it
                // must not consume an explicitly configured finite iteration.
                if let Some(limit) = effective_limit.as_mut() {
                    *limit = limit.saturating_add(1);
                }
                runtime_trace::record_event(
                    "llm_empty_response_continuation",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(true),
                    Some("empty provider segment continued without changing tool protocol"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "retry_count": retry_count,
                        "native_tools_preserved": use_native_tools,
                    }),
                );
                if let Some(ref tx) = on_delta {
                    let _ = tx
                        .send(format!(
                            "{DRAFT_PROGRESS_SENTINEL}↪ Empty inference segment — continuing automatically\n"
                        ))
                        .await;
                }
                continue;
            }

            // ── Repeated-intent guard ──────────────────────────────────────────────────
            // If the model keeps emitting the same no-tool-call text after a retry, stop
            // spending tokens: return a grounded answer (when available) or bail fast.
            // Match on the first 160 chars of trimmed text to catch paraphrased repeats.
            let text_key = display_text
                .trim()
                .chars()
                .take(160)
                .collect::<String>()
                .to_ascii_lowercase();
            let is_repeated_intent = !text_key.is_empty()
                && retry_count > 0
                && prior_no_tool_response_texts
                    .iter()
                    .any(|prev| prev == &text_key);
            prior_no_tool_response_texts.push(text_key);
            if prior_no_tool_response_texts.len() > 4 {
                let drain_to = prior_no_tool_response_texts.len() - 4;
                prior_no_tool_response_texts.drain(..drain_to);
            }
            if is_repeated_intent {
                runtime_trace::record_event(
                    "repeated_intent_fast_exit",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some("model repeated intent text without a tool call; fast-exiting"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "retry_count": retry_count,
                        "response_excerpt": truncate_with_ellipsis(
                            &scrub_credentials(&display_text),
                            240
                        ),
                    }),
                );
                if let Some(final_text) =
                    synthesize_grounded_final_answer(&recent_successful_tool_records, history)
                {
                    return return_final_response(
                        history,
                        final_text,
                        on_delta.as_ref(),
                        cancellation_token.as_ref(),
                        None,
                        false,
                    )
                    .await;
                }
                return Err(UngroundedFinalText {
                    text: display_text.clone(),
                    retry_count,
                }
                .into());
            }

            let action_oriented_request =
                latest_external_user_request(history).is_some_and(|request| {
                    !is_planning_only_request(request) && !is_informational_agent_request(request)
                });
            // Only enforce a missing tool call when the user actually asked for
            // environment-changing work. Capability/status questions commonly
            // contain words such as "execute" and "done" in an otherwise valid
            // text answer, which previously caused a false-positive retry loop.
            let completion_claim_signal = action_oriented_request
                && looks_like_unverified_action_completion_without_tool_call(&display_text);
            let deferred_tool_action_signal = action_oriented_request
                && looks_like_deferred_tool_action_without_call(&display_text);
            let tool_unavailable_signal = action_oriented_request
                && looks_like_tool_unavailability_claim(&display_text, &tool_specs);
            let task_plan_followup_requires_execution = successful_tool_execution_seen
                && action_oriented_request
                && looks_like_task_plan_followup_question(
                    &display_text,
                    &recent_successful_tool_records,
                );
            let tool_result_grounding_retry_needed = successful_tool_execution_seen
                && !recent_successful_tool_records.is_empty()
                && should_retry_with_tool_result_grounding(
                    &display_text,
                    &recent_successful_tool_records,
                );
            // Model sometimes emits a bare tool name (e.g. "db_query") as text instead
            // of a proper [TOOL_CALLS] block. Detect and retry rather than returning it.
            let bare_tool_name_response = {
                let bare = display_text.trim();
                bare.len() < 50
                    && bare.split_whitespace().count() <= 2
                    && tool_specs.iter().any(|spec| {
                        spec.name.eq_ignore_ascii_case(bare)
                            || bare.to_ascii_lowercase().contains(&spec.name)
                    })
            };
            let missing_tool_call_signal = parse_issue_detected
                || completion_claim_signal
                || deferred_tool_action_signal
                || tool_unavailable_signal
                || task_plan_followup_requires_execution
                || bare_tool_name_response
                // Model finished thinking but emitted nothing — force a retry so
                // mid-sequence thinking-only responses don't silently end the turn.
                || response_was_empty
                || (!successful_tool_execution_seen
                    && looks_like_failed_tool_followthrough(
                        &display_text,
                        &recent_failed_tool_records,
                    ));
            let missing_tool_call_followthrough = !missing_tool_call_retry_used
                && tool_loop_has_next_iteration(iteration, effective_limit)
                && !tool_specs.is_empty()
                && missing_tool_call_signal;

            if missing_tool_call_followthrough {
                // If tools already succeeded and we can synthesize a grounded answer, return
                // it immediately instead of queuing a retry that risks a downstream 502.
                // This handles the case where the model emits JSON-ish success prose after a
                // successful tool call — the turn is done; don't spend another LLM round.
                // Exception: when the model emitted an empty segment,
                // skip the grounded-answer shortcut and force a real retry so it can continue.
                if successful_tool_execution_seen
                    && !recent_successful_tool_records.is_empty()
                    && !response_was_empty
                {
                    if let Some(final_text) =
                        synthesize_grounded_final_answer(&recent_successful_tool_records, history)
                    {
                        runtime_trace::record_event(
                            "tool_call_grounded_early_exit",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(true),
                            Some(
                                "tools succeeded; skipping followthrough retry, returning grounded answer",
                            ),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "retry_count": retry_count,
                                "text": scrub_credentials(&final_text),
                            }),
                        );
                        return return_final_response(
                            history,
                            final_text,
                            on_delta.as_ref(),
                            cancellation_token.as_ref(),
                            None,
                            false,
                        )
                        .await;
                    }
                    if completion_claim_signal && !tool_result_grounding_retry_needed {
                        runtime_trace::record_event(
                            "tool_call_completion_claim_accepted",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(true),
                            Some(
                                "tool succeeded; accepting completion claim text without another retry",
                            ),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "retry_count": retry_count,
                                "text": scrub_credentials(&display_text),
                            }),
                        );
                        return return_final_response(
                            history,
                            display_text.clone(),
                            on_delta.as_ref(),
                            cancellation_token.as_ref(),
                            None,
                            false,
                        )
                        .await;
                    }
                }

                let failed_tool_followthrough = !successful_tool_execution_seen
                    && looks_like_failed_tool_followthrough(
                        &display_text,
                        &recent_failed_tool_records,
                    );
                let switched_to_prompt_tool_mode =
                    use_native_tools && !prompt_tool_fallback_used && !tool_specs.is_empty();
                if switched_to_prompt_tool_mode {
                    prompt_tool_fallback_used = true;
                    use_native_tools = false;
                    inject_prompt_tool_fallback_instructions(history, &tool_specs);
                }
                missing_tool_call_retry_used = true;
                retry_count += 1;
                missing_tool_call_retry_prompt = Some(
                    if (bare_tool_name_response || parse_issue_detected) && !use_native_tools {
                        // Model emitted a bare/malformed tool call instead of [TOOL_CALLS] format.
                        // Remind it of the exact XML format expected. Do NOT include a live example
                        // that would itself be parsed as a tool call — use a placeholder notation.
                        let tname = display_text
                            .trim()
                            .split('[')
                            .next()
                            .unwrap_or("tool_name")
                            .trim();
                        format!(
                            "Internal correction: the tool call format was wrong. \
                         Required format: [TOOL_CALLS]TOOL_NAME[ARGS]JSON_OBJECT — brackets and [ARGS] keyword are required. \
                         Do NOT use SQL for MongoDB connections — use JSON filter syntax. \
                         For db_query on arxiv: connection=arxiv, collection=Papers, \
                         filter={{\"categories\":{{\"$regex\":\"cs.AI\"}}}}, projection={{\"title\":1,\"_id\":0}}, limit=3. \
                         Emit the [TOOL_CALLS]{tname}[ARGS]{{...JSON...}} block now."
                        )
                    } else if failed_tool_followthrough {
                        build_failed_tool_retry_prompt(&recent_failed_tool_records)
                    } else if tool_unavailable_signal {
                        build_tool_unavailable_retry_prompt(&tool_specs)
                    } else if task_plan_followup_requires_execution {
                        build_post_plan_create_start_prompt(&recent_successful_tool_records)
                            .or_else(|| {
                                build_task_plan_execution_followup_prompt(
                                    &recent_successful_tool_records,
                                )
                            })
                            .unwrap_or_else(|| build_missing_tool_call_retry_prompt(history))
                    } else {
                        build_missing_tool_call_retry_prompt(history)
                    },
                );
                let retry_reason = if parse_issue_detected {
                    "parse_issue_detected"
                } else if deferred_tool_action_signal {
                    "deferred_tool_action_detected"
                } else if failed_tool_followthrough {
                    "tool_error_followthrough_detected"
                } else if tool_unavailable_signal {
                    "tool_unavailable_claim_detected"
                } else if task_plan_followup_requires_execution {
                    "task_plan_followup_requires_execution"
                } else {
                    "completion_claim_text_detected"
                };
                runtime_trace::record_event(
                    "tool_call_followthrough_retry",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some(retry_reason),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "retry_count": retry_count,
                        "switched_to_prompt_tool_mode": switched_to_prompt_tool_mode,
                        "response_excerpt": truncate_with_ellipsis(
                            &scrub_credentials(&display_text),
                            240
                        ),
                    }),
                );
                if let Some(ref tx) = on_delta {
                    let _ = tx
                        .send(format!(
                            "{DRAFT_PROGRESS_SENTINEL}\u{21bb} Retrying: response implied action without a verifiable tool call\n"
                        ))
                        .await;
                }
                continue;
            }

            let grounding_issue = tool_result_grounding_retry_needed;
            let can_retry_grounding = grounding_issue
                && !tool_result_grounding_retry_used
                && tool_loop_has_next_iteration(iteration, effective_limit);
            if can_retry_grounding {
                tool_result_grounding_retry_used = true;
                retry_count += 1;
                missing_tool_call_retry_prompt = Some(build_tool_result_grounding_retry_prompt(
                    &recent_successful_tool_records,
                ));
                runtime_trace::record_event(
                    "tool_result_grounding_retry",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some("tool_result_grounding_retry"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "retry_count": retry_count,
                        "response_excerpt": truncate_with_ellipsis(
                            &scrub_credentials(&display_text),
                            240
                        ),
                    }),
                );
                if let Some(ref tx) = on_delta {
                    let _ = tx
                        .send(format!(
                            "{DRAFT_PROGRESS_SENTINEL}\u{21bb} Retrying: grounding final answer in verified tool results\n"
                        ))
                        .await;
                }
                continue;
            }

            // If the model responded with text instead of tool calls right after a
            // plan-create start prompt, force one retry with a strong execute directive.
            if pending_post_plan_create_retry
                && !missing_tool_call_retry_used
                && tool_loop_has_next_iteration(iteration, effective_limit)
            {
                missing_tool_call_retry_used = true;
                retry_count += 1;
                missing_tool_call_retry_prompt =
                    Some(build_missing_tool_call_retry_prompt(history));
                runtime_trace::record_event(
                    "post_plan_create_no_tool_call_retry",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some(
                        "model described plan instead of executing; retrying with execute directive",
                    ),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "response_excerpt": truncate_with_ellipsis(
                            &scrub_credentials(&display_text),
                            240
                        ),
                    }),
                );
                if let Some(ref tx) = on_delta {
                    let _ = tx
                        .send(format!(
                            "{DRAFT_PROGRESS_SENTINEL}\u{21bb} Retrying: plan created, now executing\n"
                        ))
                        .await;
                }
                continue;
            }

            let mut final_text = display_text.clone();

            if missing_tool_call_signal && missing_tool_call_retry_used {
                runtime_trace::record_event(
                    "tool_call_followthrough_failed",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some("model repeated deferred action without tool call"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "response_excerpt": truncate_with_ellipsis(
                            &scrub_credentials(&display_text),
                            240
                        ),
                    }),
                );
                if successful_tool_execution_seen {
                    if let Some(fallback) =
                        synthesize_grounded_final_answer(&recent_successful_tool_records, history)
                    {
                        final_text = fallback;
                        runtime_trace::record_event(
                            "tool_call_followthrough_grounded_fallback",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(true),
                            Some("using grounded fallback after repeated followthrough failure"),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "text": scrub_credentials(&final_text),
                            }),
                        );
                    } else if tool_unavailable_signal
                        && !parse_issue_detected
                        && !completion_claim_signal
                    {
                        tracing::warn!(
                            "Model still claims missing tools after corrective retry; returning text response."
                        );
                    } else {
                        anyhow::bail!(
                            "Model repeatedly deferred action without emitting a tool call"
                        );
                    }
                } else if tool_unavailable_signal
                    && !parse_issue_detected
                    && !completion_claim_signal
                {
                    tracing::warn!(
                        "Model still claims missing tools after corrective retry; returning text response."
                    );
                } else {
                    anyhow::bail!("Model repeatedly deferred action without emitting a tool call");
                }
            }

            if grounding_issue && !can_retry_grounding {
                if let Some(fallback) =
                    synthesize_grounded_final_answer(&recent_successful_tool_records, history)
                {
                    final_text = fallback;
                    runtime_trace::record_event(
                        "tool_result_grounding_fallback",
                        Some(channel_name),
                        Some(provider_name),
                        Some(model),
                        Some(&turn_id),
                        Some(true),
                        Some("using grounded fallback after ungrounded final answer"),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "text": scrub_credentials(&final_text),
                        }),
                    );
                }
            }

            runtime_trace::record_event(
                "turn_final_response",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                Some(true),
                None,
                serde_json::json!({
                    "iteration": iteration + 1,
                    "retry_count": retry_count,
                    "stop_reason": if retry_count == 0 { "clean" } else { "after_retries" },
                    "text": scrub_credentials(&final_text),
                }),
            );
            // No tool calls — this is the final response.
            // If a streaming sender is provided, relay the text in small chunks
            // so the channel can progressively update the draft message.
            return return_final_response(
                history,
                final_text,
                on_delta.as_ref(),
                cancellation_token.as_ref(),
                Some(&response_text),
                response_streamed_live,
            )
            .await;
        }

        // A real tool call is genuine progress, not a stall — reset the
        // empty-reasoning-checkpoint streak.
        consecutive_empty_output_budget_checkpoints = 0;

        // Print any text the LLM produced alongside tool calls (unless silent)
        if !silent && !display_text.is_empty() {
            print!("{display_text}");
            let _ = std::io::stdout().flush();
        }

        // Execute tool calls and build results. `individual_results` tracks per-call output so
        // native-mode history can emit one role=tool message per tool call with the correct ID.
        //
        // When multiple tool calls are present and interactive CLI approval is not needed, run
        // tool executions concurrently for lower wall-clock latency.
        let mut tool_results = String::new();
        let mut individual_results: Vec<(Option<String>, String)> = Vec::new();
        let mut ordered_results: Vec<Option<(String, Option<String>, ToolExecutionOutcome)>> =
            (0..tool_calls.len()).map(|_| None).collect();
        let allow_parallel_execution = parallel_tools_enabled
            && should_execute_tools_in_parallel(&tool_calls, tools_registry, approval);
        let mut executable_indices: Vec<usize> = Vec::new();
        let mut executable_calls: Vec<ParsedToolCall> = Vec::new();
        let mut duplicate_tool_call_count = 0usize;

        for (idx, call) in tool_calls.iter().enumerate() {
            // ── Hook: before_tool_call (modifying) ──────────
            let mut tool_name = call.name.clone();
            let mut tool_args = call.arguments.clone();
            if let Some(hooks) = hooks {
                match hooks
                    .run_before_tool_call(tool_name.clone(), tool_args.clone())
                    .await
                {
                    crate::hooks::HookResult::Cancel(reason) => {
                        tracing::info!(tool = %call.name, %reason, "tool call cancelled by hook");
                        let cancelled = format!("Cancelled by hook: {reason}");
                        runtime_trace::record_event(
                            "tool_call_result",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(false),
                            Some(&cancelled),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": call.name,
                                "arguments": scrub_credentials(&tool_args.to_string()),
                            }),
                        );
                        ordered_results[idx] = Some((
                            call.name.clone(),
                            call.tool_call_id.clone(),
                            ToolExecutionOutcome {
                                output: cancelled,
                                success: false,
                                error_reason: Some(scrub_credentials(&reason)),
                                duration: Duration::ZERO,
                            },
                        ));
                        continue;
                    }
                    crate::hooks::HookResult::Continue((name, args)) => {
                        tool_name = name;
                        tool_args = args;
                    }
                }
            }

            maybe_inject_cron_add_delivery(
                &tool_name,
                &mut tool_args,
                channel_name,
                channel_reply_target.as_deref(),
            );

            if excluded_tools.iter().any(|ex| ex == &tool_name) {
                let blocked = format!("Tool '{tool_name}' is not available for this turn.");
                runtime_trace::record_event(
                    "tool_call_result",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some(&blocked),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "tool": tool_name.clone(),
                        "arguments": scrub_credentials(&tool_args.to_string()),
                        "blocked_by_tool_selection": true,
                    }),
                );
                ordered_results[idx] = Some((
                    tool_name.clone(),
                    call.tool_call_id.clone(),
                    ToolExecutionOutcome {
                        output: blocked.clone(),
                        success: false,
                        error_reason: Some(blocked),
                        duration: Duration::ZERO,
                    },
                ));
                continue;
            }

            // ── Approval hook ────────────────────────────────
            if let Some(mgr) = approval {
                if bypass_non_cli_approval_for_turn {
                    mgr.record_decision(
                        &tool_name,
                        &tool_args,
                        ApprovalResponse::Yes,
                        channel_name,
                    );
                } else if mgr.needs_approval(&tool_name) {
                    let request = ApprovalRequest {
                        tool_name: tool_name.clone(),
                        arguments: tool_args.clone(),
                    };

                    let decision = if channel_name == "cli" {
                        mgr.prompt_cli(&request)
                    } else if let Some(ctx) = non_cli_approval_context.as_ref() {
                        let pending = mgr.create_non_cli_pending_request(
                            &tool_name,
                            &ctx.sender,
                            channel_name,
                            &ctx.reply_target,
                            Some(
                                "interactive approval required for supervised non-cli tool execution"
                                    .to_string(),
                            ),
                        );

                        let _ = ctx.prompt_tx.send(NonCliApprovalPrompt {
                            request_id: pending.request_id.clone(),
                            tool_name: tool_name.clone(),
                            arguments: tool_args.clone(),
                        });

                        await_non_cli_approval_decision(
                            mgr,
                            &pending.request_id,
                            cancellation_token.as_ref(),
                        )
                        .await
                    } else {
                        ApprovalResponse::No
                    };

                    mgr.record_decision(&tool_name, &tool_args, decision, channel_name);

                    if decision == ApprovalResponse::No {
                        let denied = "Denied by user.".to_string();
                        runtime_trace::record_event(
                            "tool_call_result",
                            Some(channel_name),
                            Some(provider_name),
                            Some(model),
                            Some(&turn_id),
                            Some(false),
                            Some(&denied),
                            serde_json::json!({
                                "iteration": iteration + 1,
                                "tool": tool_name.clone(),
                                "arguments": scrub_credentials(&tool_args.to_string()),
                            }),
                        );
                        ordered_results[idx] = Some((
                            tool_name.clone(),
                            call.tool_call_id.clone(),
                            ToolExecutionOutcome {
                                output: denied.clone(),
                                success: false,
                                error_reason: Some(denied),
                                duration: Duration::ZERO,
                            },
                        ));
                        continue;
                    }
                }
            }

            // ── Planner-churn suppression (TODO #9) ─────────────────────────────────
            // Suppress opportunistic task_plan calls that arrive after real execution
            // tools have already run. A direct "write, run, delete" prompt should not
            // reopen planning once execution has started.
            if tool_name == "task_plan" && successful_tool_execution_seen {
                let execution_started = recent_successful_tool_records.iter().any(|r| {
                    !matches!(
                        r.name.as_str(),
                        "task_plan" | "memory_store" | "memory_recall"
                    )
                });
                if execution_started {
                    let suppressed = "Skipped task_plan call: execution has already started this turn. Use the existing plan and continue executing.".to_string();
                    runtime_trace::record_event(
                        "planner_churn_suppressed",
                        Some(channel_name),
                        Some(provider_name),
                        Some(model),
                        Some(&turn_id),
                        Some(false),
                        Some("task_plan suppressed after execution started"),
                        serde_json::json!({
                            "iteration": iteration + 1,
                            "arguments": scrub_credentials(&tool_args.to_string()),
                        }),
                    );
                    ordered_results[idx] = Some((
                        tool_name.clone(),
                        call.tool_call_id.clone(),
                        ToolExecutionOutcome {
                            output: suppressed.clone(),
                            success: false,
                            error_reason: Some(suppressed),
                            duration: Duration::ZERO,
                        },
                    ));
                    continue;
                }
            }

            let signature = tool_call_signature(&tool_name, &tool_args);
            if !seen_tool_signatures.insert(signature) {
                duplicate_tool_call_count += 1;
                let duplicate = format!(
                    "Skipped duplicate tool call '{tool_name}' with identical arguments in this turn."
                );
                runtime_trace::record_event(
                    "tool_call_result",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(false),
                    Some(&duplicate),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "tool": tool_name.clone(),
                        "arguments": scrub_credentials(&tool_args.to_string()),
                        "deduplicated": true,
                    }),
                );
                ordered_results[idx] = Some((
                    tool_name.clone(),
                    call.tool_call_id.clone(),
                    ToolExecutionOutcome {
                        output: duplicate.clone(),
                        success: false,
                        error_reason: Some(duplicate),
                        duration: Duration::ZERO,
                    },
                ));
                continue;
            }

            runtime_trace::record_event(
                "tool_call_start",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                None,
                None,
                serde_json::json!({
                    "iteration": iteration + 1,
                    "tool": tool_name.clone(),
                    "arguments": scrub_credentials(&tool_args.to_string()),
                }),
            );

            // ── Progress: tool start ────────────────────────────
            if let Some(ref tx) = on_delta {
                let hint = truncate_tool_args_for_progress(&tool_name, &tool_args, 60);
                let progress = if hint.is_empty() {
                    format!("\u{23f3} {}\n", tool_name)
                } else {
                    format!("\u{23f3} {}: {hint}\n", tool_name)
                };
                tracing::debug!(tool = %tool_name, "Sending progress start to draft");
                let _ = tx
                    .send(format!("{DRAFT_PROGRESS_SENTINEL}{progress}"))
                    .await;
            }

            executable_indices.push(idx);
            executable_calls.push(ParsedToolCall {
                name: tool_name,
                arguments: tool_args,
                tool_call_id: call.tool_call_id.clone(),
            });
        }

        let executed_outcomes = if allow_parallel_execution && executable_calls.len() > 1 {
            execute_tools_parallel(
                &executable_calls,
                tools_registry,
                observer,
                cancellation_token.as_ref(),
            )
            .await?
        } else {
            execute_tools_sequential(
                &executable_calls,
                tools_registry,
                observer,
                cancellation_token.as_ref(),
            )
            .await?
        };
        let mut current_successful_tool_records = Vec::new();
        let mut current_failed_tool_records = Vec::new();

        for ((idx, call), outcome) in executable_indices
            .iter()
            .zip(executable_calls.iter())
            .zip(executed_outcomes.into_iter())
        {
            if outcome.success {
                successful_tool_execution_seen = true;
                current_successful_tool_records.push(SuccessfulToolRecord {
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                    output: outcome.output.clone(),
                });
            } else {
                current_failed_tool_records.push(FailedToolRecord {
                    name: call.name.clone(),
                    output: outcome.output.clone(),
                });
            }
            if let Some(ledger) = crate::agent::run_ledger::current() {
                ledger.record_tool_event(
                    &call.name,
                    &call.arguments,
                    outcome.success,
                    outcome.duration.as_millis() as u64,
                    &outcome.output,
                );
            }
            runtime_trace::record_event(
                "tool_call_result",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                Some(outcome.success),
                outcome.error_reason.as_deref(),
                serde_json::json!({
                    "iteration": iteration + 1,
                    "tool": call.name.clone(),
                    "duration_ms": outcome.duration.as_millis(),
                    "output": scrub_credentials(&outcome.output),
                }),
            );

            // ── Hook: after_tool_call (void) ─────────────────
            if let Some(hooks) = hooks {
                let tool_result_obj = crate::tools::ToolResult {
                    success: outcome.success,
                    output: outcome.output.clone(),
                    error: None,
                };
                hooks
                    .fire_after_tool_call(&call.name, &tool_result_obj, outcome.duration)
                    .await;
            }

            // ── Progress: tool completion ───────────────────────
            if let Some(ref tx) = on_delta {
                let secs = outcome.duration.as_secs();
                let icon = if outcome.success {
                    "\u{2705}"
                } else {
                    "\u{274c}"
                };
                let output = truncate_with_ellipsis(&scrub_credentials(&outcome.output), 12_000);
                let progress = if output.trim().is_empty() {
                    format!("{DRAFT_PROGRESS_SENTINEL}{icon} {} ({secs}s)\n", call.name)
                } else {
                    format!(
                        "{DRAFT_PROGRESS_SENTINEL}{icon} {} ({secs}s)\n{}\n",
                        call.name, output
                    )
                };
                tracing::debug!(tool = %call.name, secs, "Sending progress complete to draft");
                let _ = tx.send(progress).await;
            }

            ordered_results[*idx] = Some((call.name.clone(), call.tool_call_id.clone(), outcome));
        }

        let iteration_had_failed_tools = !current_failed_tool_records.is_empty();
        let iteration_resolved_plan_item = current_successful_tool_records
            .iter()
            .any(task_plan_record_resolves_item);
        let iteration_executed_non_plan_tool =
            current_successful_tool_records.iter().any(|record| {
                !matches!(
                    record.name.as_str(),
                    "task_plan" | "memory_store" | "memory_recall"
                )
            });
        let iteration_had_only_task_plan_create = !iteration_executed_non_plan_tool
            && current_successful_tool_records
                .iter()
                .any(task_plan_record_is_create);
        // Track web_search_tool without web_fetch so we can prompt the model to
        // read the actual pages instead of just citing search snippets.
        let iteration_had_web_search_without_fetch = web_search_needs_fetch_continuation(
            &current_successful_tool_records,
            web_fetch_available,
        );
        let iteration_had_fetch = current_successful_tool_records
            .iter()
            .any(|r| r.name == "web_fetch");
        if iteration_had_web_search_without_fetch {
            consecutive_web_searches_without_fetch =
                consecutive_web_searches_without_fetch.saturating_add(1);
        } else if iteration_had_fetch {
            consecutive_web_searches_without_fetch = 0;
        }

        // Detect model polling delegate_coordination_status in a loop with no other action.
        // This happens when the model tries to find delegate workers that don't exist.
        let iteration_was_coordination_status_only = !current_successful_tool_records.is_empty()
            && current_successful_tool_records
                .iter()
                .all(|r| r.name == "delegate_coordination_status");
        if iteration_was_coordination_status_only {
            consecutive_coordination_status_only_iterations =
                consecutive_coordination_status_only_iterations.saturating_add(1);
        } else {
            consecutive_coordination_status_only_iterations = 0;
        }

        if !current_successful_tool_records.is_empty() {
            recent_successful_tool_records.extend(current_successful_tool_records);
            // Plan records are the durable execution contract for this turn.
            // Retain every update since the most recent create while bounding
            // ordinary action records to the latest 12. This prevents large
            // autonomous plans from forgetting their own checklist halfway
            // through execution.
            if let Some(latest_create) = recent_successful_tool_records.iter().rposition(|record| {
                record.name == "task_plan" && task_plan_call_is_create(&record.arguments)
            }) {
                let mut non_plan_after_create = 0usize;
                recent_successful_tool_records = recent_successful_tool_records
                    .drain(..)
                    .enumerate()
                    .rev()
                    .filter(|(index, record)| {
                        if *index < latest_create {
                            return false;
                        }
                        if record.name == "task_plan" {
                            return true;
                        }
                        non_plan_after_create += 1;
                        non_plan_after_create <= 12
                    })
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .map(|(_, record)| record)
                    .collect();
            } else if recent_successful_tool_records.len() > 12 {
                let drain_to = recent_successful_tool_records.len() - 12;
                recent_successful_tool_records.drain(..drain_to);
            }
        }

        if iteration_resolved_plan_item {
            forced_history_budget = plan_boundary_history_budget(history_budget);
        }

        if !current_failed_tool_records.is_empty() {
            // Detect when the same tool keeps failing with the same error class across iterations.
            if current_failed_tool_records.len() == 1 {
                let rec = &current_failed_tool_records[0];
                let error_prefix = rec.output.split('\n').next().unwrap_or("").trim();
                let sig = (rec.name.clone(), error_prefix.to_ascii_lowercase());
                if last_failure_signature.as_ref() == Some(&sig) {
                    consecutive_same_failure_count =
                        consecutive_same_failure_count.saturating_add(1);
                } else {
                    last_failure_signature = Some(sig);
                    consecutive_same_failure_count = 1;
                }
            } else {
                last_failure_signature = None;
                consecutive_same_failure_count = 0;
            }
            recent_failed_tool_records.extend(current_failed_tool_records);
            let recent_len = recent_failed_tool_records.len();
            if recent_len > 8 {
                recent_failed_tool_records.drain(..recent_len - 8);
            }
        } else {
            last_failure_signature = None;
            consecutive_same_failure_count = 0;
        }

        for (tool_name, tool_call_id, outcome) in ordered_results.into_iter().flatten() {
            individual_results.push((tool_call_id, outcome.output.clone()));
            let _ = writeln!(
                tool_results,
                "<tool_result name=\"{}\">\n{}\n</tool_result>",
                tool_name, outcome.output
            );
        }

        // Add assistant message with tool calls + tool results to history.
        // Native mode: use JSON-structured messages so convert_messages() can
        // reconstruct proper OpenAI-format tool_calls and tool result messages.
        // Prompt mode: use XML-based text format as before.
        history.push(ChatMessage::assistant(assistant_history_content));
        if native_tool_calls.is_empty() {
            let all_results_have_ids = use_native_tools
                && !individual_results.is_empty()
                && individual_results
                    .iter()
                    .all(|(tool_call_id, _)| tool_call_id.is_some());
            if all_results_have_ids {
                for (tool_call_id, result) in &individual_results {
                    let tool_msg = serde_json::json!({
                        "tool_call_id": tool_call_id,
                        "content": result,
                    });
                    history.push(ChatMessage::tool(tool_msg.to_string()));
                }
            } else {
                history.push(ChatMessage::user(format!("[Tool results]\n{tool_results}")));
            }
        } else {
            for (native_call, (_, result)) in
                native_tool_calls.iter().zip(individual_results.iter())
            {
                let tool_msg = serde_json::json!({
                    "tool_call_id": native_call.id,
                    "content": result,
                });
                history.push(ChatMessage::tool(tool_msg.to_string()));
            }
        }

        if !iteration_had_failed_tools
            && !task_plan_progress_snapshot(&recent_successful_tool_records)
                .is_some_and(|progress| progress.total > 0 && progress.resolved == progress.total)
            && should_short_circuit_after_tool_execution(history, &recent_successful_tool_records)
        {
            if let Some(final_text) =
                synthesize_grounded_final_answer(&recent_successful_tool_records, history)
            {
                runtime_trace::record_event(
                    "tool_execution_grounded_fast_exit",
                    Some(channel_name),
                    Some(provider_name),
                    Some(model),
                    Some(&turn_id),
                    Some(true),
                    Some("returning grounded final answer after verified tool completion"),
                    serde_json::json!({
                        "iteration": iteration + 1,
                        "text": scrub_credentials(&final_text),
                    }),
                );
                return return_final_response(
                    history,
                    final_text,
                    on_delta.as_ref(),
                    cancellation_token.as_ref(),
                    None,
                    false,
                )
                .await;
            }
        }

        post_tool_execution_prompt = if !iteration_had_failed_tools {
            if let Some((path, write_count)) =
                detect_repeated_file_write_stall(&recent_successful_tool_records)
            {
                Some(format!(
                    "STOP. The file `{path}` has already been written {write_count} times without advancing the task. \
                     Do NOT call `file_write` again for the same path right now. \
                     Use the verified file state and move forward with a different tool: \
                     `file_read` if you need to inspect the current contents, `shell` if you need to run, test, or build it, \
                     or `task_plan` update if the write step is already complete."
                ))
            } else
            // Prefer web-research continuation whenever the latest search still
            // has relevant, unfetched URLs for the current request.
            if iteration_had_web_search_without_fetch {
                // After a few consecutive searches without any fetch, emit a hard override
                // prompt so the model cannot search again before fetching at least one URL.
                if consecutive_web_searches_without_fetch >= WEB_SEARCH_WITHOUT_FETCH_STREAK_LIMIT {
                    let force_urls = extract_candidate_urls_from_search_output(
                        recent_successful_tool_records
                            .iter()
                            .rev()
                            .find(|r| r.name == "web_search_tool")
                            .map(|r| r.output.as_str())
                            .unwrap_or(""),
                        3,
                    );
                    let url_list = if force_urls.is_empty() {
                        "the URLs returned by the most recent search".to_string()
                    } else {
                        force_urls.join(", ")
                    };
                    Some(format!(
                        "STOP. You have called web_search_tool {consecutive_web_searches_without_fetch} times in a row without reading any page. \
                         DO NOT call web_search_tool again. \
                         You MUST call web_fetch NOW on: {url_list}. \
                         Fetch one URL, read the content, then provide your answer."
                    ))
                } else {
                    build_agentic_web_research_followup_prompt(
                        history,
                        &recent_successful_tool_records,
                        web_fetch_available,
                    )
                    .or_else(|| {
                        build_post_web_search_fetch_prompt(
                            &recent_successful_tool_records,
                            web_fetch_available,
                        )
                    })
                    .or_else(|| {
                        build_task_plan_execution_followup_prompt(&recent_successful_tool_records)
                    })
                }
            } else if consecutive_coordination_status_only_iterations
                >= COORDINATION_STATUS_POLL_STREAK_LIMIT
            {
                // Model is polling delegate_coordination_status in a loop with no other work.
                // This typically means it is waiting for delegate workers that don't exist.
                // Force it to proceed with direct execution.
                Some(format!(
                    "STOP. You have called delegate_coordination_status {} times in a row without \
                     taking any other action. No delegate workers are waiting for tasks. \
                     Do NOT call delegate_coordination_status again. \
                     Complete the task yourself using your available tools (shell, file_write, task_plan, etc.) \
                     — do not wait for or attempt to delegate to other agents.",
                    consecutive_coordination_status_only_iterations
                ))
            } else if iteration_executed_non_plan_tool {
                let last_was_db_query_with_rows =
                    recent_successful_tool_records.last().is_some_and(|r| {
                        r.name == "db_query"
                            && r.output.starts_with("Query returned ")
                            && !r.output.starts_with("Query returned no rows")
                    });
                // Special case: db_schema ran and no task_plan is active.
                // Inject a short prompt so the model calls db_query next instead of
                // continuing its previous text and emitting a bare tool name.
                let last_was_db_schema = recent_successful_tool_records
                    .last()
                    .is_some_and(|r| r.name == "db_schema")
                    && !recent_successful_tool_records
                        .iter()
                        .any(|r| r.name == "task_plan");
                if last_was_db_query_with_rows {
                    // Data retrieved — no continuation prompt; let the model present results.
                    None
                } else if last_was_db_schema {
                    Some("Schema retrieved. Now call db_query with the correct connection, collection, filter, and projection for the user's request.".to_string())
                } else {
                    build_agentic_web_research_followup_prompt(
                        history,
                        &recent_successful_tool_records,
                        web_fetch_available,
                    )
                    .or_else(|| {
                        build_task_plan_execution_followup_prompt(&recent_successful_tool_records)
                    })
                }
            } else if iteration_had_only_task_plan_create {
                build_agentic_web_research_followup_prompt(
                    history,
                    &recent_successful_tool_records,
                    web_fetch_available,
                )
                    .or_else(|| build_post_plan_create_start_prompt(&recent_successful_tool_records))
                    // Fallback: snapshot unavailable (args had no tasks array), but we know a plan
                    // was created — always force execution so the model doesn't just summarise.
                    .or_else(|| Some("Internal continuation: task plan created. Begin execution NOW — use task_plan(action:list) to retrieve the steps, then immediately call the tool for step 1. Do not describe or summarise the plan, just execute.".to_string()))
            } else {
                build_agentic_web_research_followup_prompt(
                    history,
                    &recent_successful_tool_records,
                    web_fetch_available,
                )
            }
        } else if consecutive_same_failure_count >= 3 {
            // Same tool keeps failing with the same error 3+ times — break the stall by
            // telling the model explicitly that the approach is not working.
            if let Some((ref failing_tool, ref error_hint)) = last_failure_signature {
                let short_error = truncate_with_ellipsis(error_hint, 120);
                Some(format!(
                    "STOP. The `{failing_tool}` tool has failed {consecutive_same_failure_count} consecutive times with the same error: \"{short_error}\". \
                     This approach is not working. Do NOT call `{failing_tool}` with the same parameters again. \
                     Try a completely different approach or tool to accomplish the goal, \
                     or if no alternative exists, explain why the task cannot be completed."
                ))
            } else {
                None
            }
        } else {
            None
        };

        if !iteration_had_failed_tools
            && post_tool_execution_prompt.is_none()
            && iteration_executed_non_plan_tool
        {
            post_tool_execution_prompt =
                build_task_plan_execution_followup_prompt(&recent_successful_tool_records)
                    .or_else(|| {
                        build_post_plan_create_start_prompt(&recent_successful_tool_records)
                    })
                    .or_else(|| {
                        // Retrospective plan: after enough unplanned tool calls, ask the model
                        // to create a task_plan capturing what's done and what remains.
                        if !retrospective_plan_injected {
                            if let Some(prompt) = build_retrospective_task_plan_prompt(
                                &recent_successful_tool_records,
                                history,
                            ) {
                                retrospective_plan_injected = true;
                                return Some(prompt);
                            }
                        }
                        None
                    })
                    .or_else(|| {
                        build_file_write_continuation_prompt(
                            &recent_successful_tool_records,
                            history,
                        )
                    });
        }

        if !final_plan_verification_requested
            && task_plan_progress_snapshot(&recent_successful_tool_records)
                .is_some_and(|progress| progress.total > 0 && progress.resolved == progress.total)
        {
            final_plan_verification_requested = true;
            post_tool_execution_prompt = Some(
                "Internal final acceptance pass: every task-plan item is now resolved. Re-read the original user request and the verified evidence in the current working state. Check that each acceptance criterion is actually supported by tool results. If a material criterion is still unverified, call the required verification tool now and update the plan truthfully. Otherwise return the final concise pass/fail/blocked summary with concrete evidence; do not merely say the plan is complete."
                    .to_string(),
            );
        }

        let all_tool_calls_were_duplicates = successful_tool_execution_seen
            && duplicate_tool_call_count > 0
            && duplicate_tool_call_count == tool_calls.len();

        if all_tool_calls_were_duplicates {
            consecutive_all_duplicate_iterations =
                consecutive_all_duplicate_iterations.saturating_add(1);
        } else {
            consecutive_all_duplicate_iterations = 0;
        }

        // After exhausting all nudges, hard-exit if still stuck on duplicates.
        if duplicate_nudge_count >= DUPLICATE_TOOL_CALL_MAX_NUDGES
            && consecutive_all_duplicate_iterations >= DUPLICATE_TOOL_CALL_STREAK_PER_NUDGE
        {
            runtime_trace::record_event(
                "tool_call_duplicate_hard_exit",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                Some(false),
                Some("model stuck in duplicate tool-call loop; hard-exiting turn"),
                serde_json::json!({
                    "iteration": iteration + 1,
                    "duplicate_nudge_count": duplicate_nudge_count,
                    "consecutive_all_duplicate_iterations": consecutive_all_duplicate_iterations,
                }),
            );
            early_exit_reason = Some((
                "duplicate_tool_call_loop",
                format!(
                    "Agent exited after {} nudges with no progress",
                    duplicate_nudge_count
                ),
            ));
            break;
        }

        // Send an escalating nudge whenever the model has repeated itself enough times
        // without making progress, up to MAX_NUDGES times before hard-exiting.
        let should_nudge = all_tool_calls_were_duplicates
            && duplicate_nudge_count < DUPLICATE_TOOL_CALL_MAX_NUDGES
            && consecutive_all_duplicate_iterations >= DUPLICATE_TOOL_CALL_STREAK_PER_NUDGE
            && tool_loop_has_next_iteration(iteration, effective_limit);
        if should_nudge {
            let prompt = DUPLICATE_TOOL_CALL_NUDGE_PROMPTS
                [duplicate_nudge_count.min(DUPLICATE_TOOL_CALL_NUDGE_PROMPTS.len() - 1)];
            missing_tool_call_retry_prompt = Some(prompt.to_string());
            duplicate_nudge_count += 1;
            consecutive_all_duplicate_iterations = 0;
            runtime_trace::record_event(
                "tool_call_duplicate_followthrough_retry",
                Some(channel_name),
                Some(provider_name),
                Some(model),
                Some(&turn_id),
                Some(false),
                Some("model repeated an already-completed tool call; nudging"),
                serde_json::json!({
                    "iteration": iteration + 1,
                    "nudge_number": duplicate_nudge_count,
                    "duplicate_tool_call_count": duplicate_tool_call_count,
                }),
            );
            if let Some(ref tx) = on_delta {
                let _ = tx
                    .send(format!(
                        "{DRAFT_PROGRESS_SENTINEL}\u{21bb} Redirecting: model repeated a tool call (nudge {duplicate_nudge_count}/{})\n",
                        DUPLICATE_TOOL_CALL_MAX_NUDGES
                    ))
                    .await;
            }
            continue;
        }
    }

    let (stop_reason, error_message) = if let Some((reason, message)) = early_exit_reason {
        (reason, message)
    } else {
        (
            "max_iterations_exhausted",
            format!("Agent exceeded maximum tool iterations ({max_tool_iterations})"),
        )
    };

    runtime_trace::record_event(
        "tool_loop_exhausted",
        Some(channel_name),
        Some(provider_name),
        Some(model),
        Some(&turn_id),
        Some(false),
        Some(&error_message),
        serde_json::json!({
            "max_iterations": max_tool_iterations,
            "retry_count": retry_count,
            "stop_reason": stop_reason,
        }),
    );
    anyhow::bail!(error_message)
}

/// Build the tool instruction block for the system prompt from concrete tool
/// specs so the LLM knows how to invoke tools.
pub(crate) fn build_tool_instructions(tools_registry: &[Box<dyn Tool>]) -> String {
    let specs: Vec<crate::tools::ToolSpec> =
        tools_registry.iter().map(|tool| tool.spec()).collect();
    build_tool_instructions_from_specs(&specs)
}

/// Build the tool instruction block for the system prompt from concrete tool
/// specs so the LLM knows how to invoke tools.
pub(crate) fn build_tool_instructions_from_specs(tool_specs: &[crate::tools::ToolSpec]) -> String {
    let mut instructions = String::new();
    let available = tool_specs
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<HashSet<_>>();
    instructions.push_str("\n## Tool Use Protocol\n\n");
    instructions.push_str("To use a tool, wrap a JSON object in <tool_call></tool_call> tags:\n\n");
    instructions.push_str("```\n<tool_call>\n{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}\n</tool_call>\n```\n\n");
    instructions.push_str(
        "CRITICAL: Output actual <tool_call> tags—never describe steps or give examples.\n\n",
    );
    instructions.push_str(
        "When a tool is needed, emit a real call (not prose), for example:\n\
<tool_call>\n\
{\"name\":\"tool_name\",\"arguments\":{}}\n\
</tool_call>\n\n",
    );
    instructions.push_str(
        "If the runtime says your previous tool format was invalid, immediately emit another real <tool_call> in the exact format above. Do not apologize, do not narrate the command, and do not stop at one failed attempt.\n\n",
    );
    instructions.push_str(
        "Do not wrap tool calls in ```json fences or return bare JSON without <tool_call> tags.\n\n",
    );
    let mut guardrails = Vec::new();
    if available.contains("shell") {
        guardrails.push(
            "- Use `shell` for immediate local command execution (for example: lsusb, lsblk, lspci, pwd, git status, rg, cat).".to_string(),
        );
    }
    let scheduling_tools = ["cron_add", "schedule"]
        .into_iter()
        .filter(|name| available.contains(name))
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>();
    if !scheduling_tools.is_empty() {
        guardrails.push(format!(
            "- Use {} only when the user explicitly wants delayed, scheduled, or recurring execution. Never use {} for an immediate one-off command.",
            scheduling_tools.join(" or "),
            if scheduling_tools.len() == 1 {
                "it"
            } else {
                "them"
            }
        ));
    }
    if available.contains("file_read") {
        guardrails.push("- Use `file_read` to inspect files.".to_string());
    }
    let file_mutation_tools = ["file_write", "file_edit"]
        .into_iter()
        .filter(|name| available.contains(name))
        .map(|name| format!("`{name}`"))
        .collect::<Vec<_>>();
    if !file_mutation_tools.is_empty() {
        guardrails.push(format!(
            "- Use {} to change files instead of shelling out when the dedicated file tool fits.",
            file_mutation_tools.join(" or ")
        ));
    }
    if !guardrails.is_empty() {
        instructions.push_str("Tool selection guardrails:\n");
        instructions.push_str(&guardrails.join("\n"));
        instructions.push_str("\n\n");
    }
    instructions.push_str("You may use multiple tool calls in a single response. ");
    instructions.push_str("After tool execution, results appear in <tool_result> tags. ");
    instructions.push_str(
        "Continue using tools with the results until the task is actually complete, then give the final answer.\n\n",
    );
    instructions.push_str(
        "Ground the final answer in the actual tool results. Do not reinterpret tool output as a tool name or availability error.",
    );
    if available.contains("file_read") {
        instructions.push_str(" For `file_read`, answer from the returned contents.");
    }
    if available.contains("file_write") {
        instructions.push_str(
            " For `file_write`, do not invent contents that were not in the write arguments or a verified read-back.",
        );
    }
    instructions.push_str("\n\n");
    instructions.push_str("### Available Tools\n\n");

    for tool in tool_specs {
        let _ = writeln!(
            instructions,
            "**{}**: {}\nParameters: `{}`\n",
            tool.name, tool.description, tool.parameters
        );
    }

    instructions
}

/// Build shell-policy instructions for the system prompt so the model is aware
/// of command-level execution constraints before it emits tool calls.
pub(crate) fn build_shell_policy_instructions(autonomy: &crate::config::AutonomyConfig) -> String {
    let mut instructions = String::new();
    instructions.push_str("\n## Shell Policy\n\n");
    instructions
        .push_str("When using the `shell` tool, follow these runtime constraints exactly.\n\n");
    instructions.push_str(
        "- If the user asks you to run a local command (for example `lsusb`, `git status`, `cargo test`, or `./script.sh`), emit a `shell` tool call instead of explaining how to run it.\n",
    );

    let autonomy_label = match autonomy.level {
        crate::security::AutonomyLevel::ReadOnly => "read_only",
        crate::security::AutonomyLevel::Supervised => "supervised",
        crate::security::AutonomyLevel::Full => "full",
    };
    let _ = writeln!(instructions, "- Autonomy level: `{autonomy_label}`");

    if autonomy.level == crate::security::AutonomyLevel::ReadOnly {
        instructions.push_str(
            "- Shell execution is disabled in `read_only` mode. Do not emit shell tool calls.\n",
        );
        return instructions;
    }

    let normalized: BTreeSet<String> = autonomy
        .allowed_commands
        .iter()
        .map(|entry| entry.trim())
        .filter(|entry| !entry.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    if normalized.contains("*") {
        instructions.push_str(
            "- Allowed commands: wildcard `*` is configured (any command name/path may be allowlisted).\n",
        );
    } else if normalized.is_empty() {
        instructions
            .push_str("- Allowed commands: none configured. Any shell command will be rejected.\n");
    } else {
        const MAX_DISPLAY_COMMANDS: usize = 64;
        let shown: Vec<String> = normalized
            .iter()
            .take(MAX_DISPLAY_COMMANDS)
            .map(|cmd| format!("`{cmd}`"))
            .collect();
        let hidden = normalized.len().saturating_sub(MAX_DISPLAY_COMMANDS);
        let _ = write!(instructions, "- Allowed commands: {}", shown.join(", "));
        if hidden > 0 {
            let _ = write!(instructions, " (+{hidden} more)");
        }
        instructions.push('\n');
    }

    if autonomy.level == crate::security::AutonomyLevel::Supervised
        && autonomy.require_approval_for_medium_risk
    {
        instructions.push_str(
            "- Medium-risk shell commands require explicit approval in `supervised` mode.\n",
        );
    }
    if autonomy.block_high_risk_commands {
        instructions.push_str(
            "- High-risk shell commands are blocked even when command names are allowed.\n",
        );
    }
    instructions.push_str(
        "- If a requested command is outside policy, choose allowed alternatives and explain the limitation.\n",
    );

    instructions
}

pub(crate) fn build_runtime_tool_availability_notice(tools_registry: &[Box<dyn Tool>]) -> String {
    let specs: Vec<crate::tools::ToolSpec> =
        tools_registry.iter().map(|tool| tool.spec()).collect();
    build_runtime_tool_availability_notice_from_specs(&specs)
}

pub(crate) fn build_runtime_tool_availability_notice_from_specs(
    tool_specs: &[crate::tools::ToolSpec],
) -> String {
    let names = tool_specs
        .iter()
        .map(|tool| tool.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    format!(
        "\n## Runtime Tool Availability (Authoritative)\n\n\
         Use only these runtime-available tools for this turn.\n\
         Tools: {names}\n\
         A tool being listed means it is registered for this runtime; it does not prove its host binary, credentials, remote service, or a particular operation has been verified.\n\
         Do not claim a listed tool is unavailable without a concrete runtime error, and do not claim it is working without a concrete successful tool result from this run.\n\
         If the user asked for an action, keep using these tools until the action is complete or the runtime returns a blocking error.\n"
    )
}

pub(crate) fn build_managed_app_runtime_notice() -> String {
    let managed_ports = std::env::var("LLAMAFARM_MANAGED_APP_PORTS").unwrap_or_default();
    if managed_ports.trim().is_empty() {
        return String::new();
    }
    let reserved_ports =
        std::env::var("LLAMAFARM_RESERVED_APP_PORTS").unwrap_or_else(|_| "5000".to_string());
    let public_hosts = std::env::var("LLAMAFARM_PUBLIC_APP_HOSTS").unwrap_or_default();
    let host_docker = std::env::var("LLAMAFARM_HOST_DOCKER")
        .map(|value| value.eq_ignore_ascii_case("true") || value == "1")
        .unwrap_or(false);

    build_managed_app_runtime_notice_from_values(
        reserved_ports.trim(),
        managed_ports.trim(),
        public_hosts.trim(),
        host_docker,
    )
}

fn build_managed_app_runtime_notice_from_values(
    reserved_ports: &str,
    managed_ports: &str,
    public_hosts: &str,
    host_docker: bool,
) -> String {
    use std::fmt::Write as _;

    let mut notice = format!(
        "\n## Managed App Runtime (Authoritative)\n\n\
         - Your shell and background processes run inside the LlamaFarm bundle container.\n\
         - Ports {reserved_ports} are reserved for operator services; do not use them for generated apps.\n\
         - For generated web apps, choose the first free port in {managed_ports}, bind to `0.0.0.0:<port>`, and use the `process` tool for the long-running server.\n\
         - Verify syntax/imports, then verify the app's real HTTP health endpoint. A listening process or static shell HTML alone is not proof that the app works.\n"
    );
    if !public_hosts.is_empty() {
        let urls = public_hosts
            .split(',')
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(|host| format!("`http://{host}:<port>`"))
            .collect::<Vec<_>>()
            .join(", ");
        if !urls.is_empty() {
            let _ = writeln!(
                notice,
                "- After an external HTTP check succeeds, report the verified operator URLs: {urls}."
            );
        }
    }
    if host_docker {
        notice.push_str(
            "- The mounted Docker socket controls the host Docker daemon. The `docker` tool manages sibling/outside host containers; do not describe it as an isolated Docker-in-Docker daemon.\n\
             - Replacing the currently running LlamaFarm container requires a verified external updater or helper that survives this container stopping. Do not claim a self-update succeeded from an in-container command alone.\n",
        );
    }
    notice
}

pub(crate) fn build_ipc_state_usage_instructions(tools_registry: &[Box<dyn Tool>]) -> String {
    let has_state_tools = tools_registry.iter().any(|tool| {
        matches!(
            tool.name(),
            "state_get" | "state_set" | "agents_list" | "agents_send"
        )
    });
    if !has_state_tools {
        return String::new();
    }

    "\n## IPC State Usage\n\n\
     - `state_get` and `state_set` are shared inter-agent state tools, not your default local scratchpad.\n\
     - Only call `state_get` when the user explicitly asks for shared state or a prior tool result established the key.\n\
     - Do not probe guessed keys like `task` or `current_task` to recover the local task.\n\
     - Use the current turn, `task_plan`, and verified tool results for local task tracking.\n"
        .to_string()
}

pub(crate) fn build_auto_plan_execute_instructions() -> String {
    "\n## Auto Plan & Execute\n\n\
     Use `task_plan` only when it materially helps: long or branching work, batch/exhaustive checks, multi-host or delegated work, or anything that needs progress tracking across many tool calls.\n\
     For short direct tasks, execute immediately without planning.\n\n\
     When a request clearly needs a task plan, follow this exact pattern without stopping:\n\
     1. Call `task_plan` with action=create and a `tasks` array listing every step. Include compact `context` and expected `tools` per step when they improve precision; these fields guide execution but never grant permissions.\n\
     2. Immediately — in the same turn — start executing step 1 using real tools.\n\
     3. After each step has concrete verification evidence, call `task_plan` with action=update to mark it completed, then start the next step. If recovery is exhausted, mark the item failed or blocked rather than falsely completing it, then continue independent work.\n\
     4. Never stop after creating the plan to summarize or wait for user input. Execute immediately.\n\
     5. Never stop mid-execution to describe what you are about to do. Use the tool and show the result.\n\
     6. Only pause to ask the user if a tool returns a hard blocking error or a required input is genuinely unknown.\n\
     7. When all steps are completed, provide a single grounded final answer backed by the actual tool results.\n\n\
     ## Capability Audit Semantics\n\n\
     When the user asks to test or audit all available tools, treat that as a real executable integration audit, not a capability claim or a reason to stop after creating a plan.\n\
     - Create a finite task plan using only tools listed in the runtime availability section, and run one bounded probe per applicable tool.\n\
     - Track every tool as exactly one of: verified (a successful result in this run), failed (a concrete error), blocked (missing configuration, credential, host dependency, or policy), or skipped (outside the requested scope).\n\
     - Never describe an untested, blocked, or merely registered tool as functional. Do not retry the same blocked probe unless you made a specific repair that could change its outcome.\n\
     - Prefer status, list, read-only, reversible, or isolated-workspace probes for side-effecting tools. If a meaningful probe would create lasting external state, say so and use the smallest permitted probe instead of inventing a pass.\n\
     - When a tool has a harmless, missing fixture prerequisite (for example, a Git repository), create that fixture in an isolated workspace before probing it. A missing fixture is not evidence that the tool itself failed.\n\
     - Finish the audit with a compact evidence matrix and the next repair action for each failed or blocked capability.\n\n\
     CRITICAL: When you do create a plan, creating the plan is step zero. The first real work tool call must happen in the same LLM turn as the plan creation.\n"
        .to_string()
}

pub(crate) fn build_federation_delegation_instructions(
    remote_agents: &[crate::federation::peer_registry::RemoteAgentInfo],
    delegate_available: bool,
    subagent_spawn_available: bool,
) -> String {
    if !delegate_available && !subagent_spawn_available {
        return String::new();
    }
    let agent_lines: String = remote_agents
        .iter()
        .map(|info| {
            if info.specialization.is_empty() {
                format!("- `{}`\n", info.agent_name)
            } else {
                format!("- `{}` — {}\n", info.agent_name, info.specialization)
            }
        })
        .collect();
    let first_agent = remote_agents
        .first()
        .map(|info| info.agent_name.as_str())
        .unwrap_or("the worker");
    let mut instructions = format!(
        "\n## Remote Worker Delegation\n\n\
         You have {n} remote worker node(s) available:\n{agent_lines}\n\
         Route tasks based on the worker descriptions above. When a worker has a specialization, \
         prefer it for matching task types.\n\n\
         Good candidates to send to a remote worker:\n\
         - Tasks that can run in parallel with local work\n\
         - Compute-heavy inference where the remote has a better/different model\n\
         - Operations that need to run on the remote machine's filesystem or services\n\
         - Subtasks that are fully self-contained and don't need local context\n",
        n = remote_agents.len(),
        agent_lines = agent_lines,
    );
    if delegate_available {
        instructions.push_str(
            "\nUse `delegate` with agentic=true when the remote worker needs its own tool loop (file operations, shell commands, or multi-step research). Use `delegate` without agentic for single-shot inference where all context is supplied. Include `delegate` calls in a task plan when planning is useful.\n",
        );
        let _ = writeln!(
            instructions,
            "Example: delegate to {first_agent} with agentic=true: [subtask]"
        );
    }
    if subagent_spawn_available {
        instructions.push_str(
            "\nUse `subagent_spawn` for independent fire-and-forget work that can run in parallel.\n",
        );
    }
    instructions
}

// ── CLI Entrypoint ───────────────────────────────────────────────────────
// Wires up all subsystems (observer, runtime, security, memory, tools,
// provider, hardware RAG, peripherals) and enters either single-shot or
// interactive REPL mode. The interactive loop manages history compaction
// and hard trimming to keep the context window bounded.

#[allow(clippy::too_many_lines)]
pub async fn run(
    config: Config,
    message: Option<String>,
    provider_override: Option<String>,
    model_override: Option<String>,
    temperature: f64,
    peripheral_overrides: Vec<String>,
    interactive: bool,
) -> Result<String> {
    // ── Wire up agnostic subsystems ──────────────────────────────
    let base_observer = observability::create_observer(&config.observability);
    let observer: Arc<dyn Observer> = Arc::from(base_observer);
    let runtime: Arc<dyn runtime::RuntimeAdapter> =
        Arc::from(runtime::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));

    // ── Memory (the brain) ────────────────────────────────────────
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory_with_storage(
        &config.memory,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);
    tracing::info!(backend = mem.name(), "Memory initialized");

    // ── Peripherals (merge peripheral tools into registry) ─
    if !peripheral_overrides.is_empty() {
        tracing::info!(
            peripherals = ?peripheral_overrides,
            "Peripheral overrides from CLI (config boards take precedence)"
        );
    }

    // ── Tools (including memory tools and peripherals) ────────────
    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };
    let mut tools_registry = tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        mem.clone(),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.workspace_dir,
        &config.agents,
        config.api_key.as_deref(),
        &config,
    );

    let peripheral_tools: Vec<Box<dyn Tool>> =
        crate::peripherals::create_peripheral_tools(&config.peripherals).await?;
    if !peripheral_tools.is_empty() {
        tracing::info!(count = peripheral_tools.len(), "Peripheral tools added");
        tools_registry.extend(peripheral_tools);
    }

    // ── Resolve provider ─────────────────────────────────────────
    let provider_name = provider_override
        .as_deref()
        .or(config.default_provider.as_deref())
        .unwrap_or("openrouter");

    let model_name = model_override
        .as_deref()
        .or(config.default_model.as_deref())
        .unwrap_or("anthropic/claude-sonnet-4");

    let provider_runtime_options = providers::ProviderRuntimeOptions::from_config(&config);

    let provider: Box<dyn Provider> = providers::create_routed_provider_with_options(
        provider_name,
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &config.model_routes,
        model_name,
        &provider_runtime_options,
    )?;

    observer.record_event(&ObserverEvent::AgentStart {
        provider: provider_name.to_string(),
        model: model_name.to_string(),
    });

    // ── Hardware RAG (datasheet retrieval when peripherals + datasheet_dir) ──
    let hardware_rag: Option<crate::rag::HardwareRag> = config
        .peripherals
        .datasheet_dir
        .as_ref()
        .filter(|d| !d.trim().is_empty())
        .map(|dir| crate::rag::HardwareRag::load(&config.workspace_dir, dir.trim()))
        .and_then(Result::ok)
        .filter(|r: &crate::rag::HardwareRag| !r.is_empty());
    if let Some(ref rag) = hardware_rag {
        tracing::info!(chunks = rag.len(), "Hardware RAG loaded");
    }

    let board_names: Vec<String> = config
        .peripherals
        .boards
        .iter()
        .map(|b| b.board.clone())
        .collect();

    // ── Build system prompt from workspace MD files (OpenClaw framework) ──
    let skills = crate::skills::load_skills_with_config(&config.workspace_dir, &config);
    let mut tool_descs: Vec<(&str, &str)> = vec![
        (
            "shell",
            "Execute terminal commands. Use when: running local checks, build/test commands, diagnostics. Don't use when: a safer dedicated tool exists, or command is destructive without approval.",
        ),
        (
            "file_read",
            "Read file contents. Use when: inspecting project files, configs, logs. Don't use when: a targeted search is enough.",
        ),
        (
            "file_write",
            "Write file contents. Use when: applying focused edits, scaffolding files, updating docs/code. Don't use when: side effects are unclear or file ownership is uncertain.",
        ),
        (
            "memory_store",
            "Save to memory. Use when: preserving durable preferences, decisions, key context. Don't use when: information is transient/noisy/sensitive without need.",
        ),
        (
            "memory_recall",
            "Search memory. Use when: retrieving prior decisions, user preferences, historical context. Don't use when: answer is already in current context.",
        ),
        (
            "memory_forget",
            "Delete a memory entry. Use when: memory is incorrect/stale or explicitly requested for removal. Don't use when: impact is uncertain.",
        ),
    ];
    tool_descs.push((
        "cron_add",
        "Create a cron job. Supports schedule kinds: cron, at, every; and job types: shell or agent.",
    ));
    tool_descs.push((
        "cron_list",
        "List all cron jobs with schedule, status, and metadata.",
    ));
    tool_descs.push(("cron_remove", "Remove a cron job by job_id."));
    tool_descs.push((
        "cron_update",
        "Patch a cron job (schedule, enabled, command/prompt, model, delivery, session_target).",
    ));
    tool_descs.push((
        "cron_run",
        "Force-run a cron job immediately and record a run history entry.",
    ));
    tool_descs.push(("cron_runs", "Show recent run history for a cron job."));
    tool_descs.push((
        "screenshot",
        "Capture a screenshot of the current screen. Returns file path and base64-encoded PNG. Use when: visual verification, UI inspection, debugging displays.",
    ));
    tool_descs.push((
        "image_info",
        "Read image file metadata (format, dimensions, size) and optionally base64-encode it. Use when: inspecting images, preparing visual data for analysis.",
    ));
    if config.browser.enabled {
        tool_descs.push((
            "browser_open",
            "Open approved HTTPS URLs in system browser (allowlist-only, no scraping)",
        ));
    }
    if config.composio.enabled {
        tool_descs.push((
            "composio",
            "Execute actions on 1000+ apps via Composio (Gmail, Notion, GitHub, Slack, etc.). Use action='list' to discover, 'execute' to run (optionally with connected_account_id), 'connect' to OAuth.",
        ));
    }
    tool_descs.push((
        "schedule",
        "Manage scheduled tasks (create/list/get/cancel/pause/resume). Supports recurring cron and one-shot delays.",
    ));
    tool_descs.push((
        "model_routing_config",
        "Configure default model, scenario routing, and delegate agents. Use for natural-language requests like: 'set conversation to kimi and coding to gpt-5.3-codex'.",
    ));
    if !config.agents.is_empty() {
        tool_descs.push((
            "delegate",
            "Delegate a sub-task to a specialized agent. Use when: task needs different model/capability, or to parallelize work.",
        ));
    }
    if config.peripherals.enabled && !config.peripherals.boards.is_empty() {
        tool_descs.push((
            "gpio_read",
            "Read GPIO pin value (0 or 1) on connected hardware (STM32, Arduino). Use when: checking sensor/button state, LED status.",
        ));
        tool_descs.push((
            "gpio_write",
            "Set GPIO pin high (1) or low (0) on connected hardware. Use when: turning LED on/off, controlling actuators.",
        ));
        tool_descs.push((
            "arduino_upload",
            "Upload agent-generated Arduino sketch. Use when: user asks for 'make a heart', 'blink pattern', or custom LED behavior on Arduino. You write the full .ino code; Ollama compiles and uploads it. Pin 13 = built-in LED on Uno.",
        ));
        tool_descs.push((
            "hardware_memory_map",
            "Return flash and RAM address ranges for connected hardware. Use when: user asks for 'upper and lower memory addresses', 'memory map', or 'readable addresses'.",
        ));
        tool_descs.push((
            "hardware_board_info",
            "Return full board info (chip, architecture, memory map) for connected hardware. Use when: user asks for 'board info', 'what board do I have', 'connected hardware', 'chip info', or 'what hardware'.",
        ));
        tool_descs.push((
            "hardware_memory_read",
            "Read actual memory/register values from Nucleo via USB. Use when: user asks to 'read register values', 'read memory', 'dump lower memory 0-126', 'give address and value'. Params: address (hex, default 0x20000000), length (bytes, default 128).",
        ));
        tool_descs.push((
            "hardware_capabilities",
            "Query connected hardware for reported GPIO pins and LED pin. Use when: user asks what pins are available.",
        ));
    }
    let bootstrap_max_chars = Some(if config.agent.compact_context {
        6000
    } else {
        usize::MAX
    });
    // Dynamic: use Ollama's cached `/api/show` capability report when available.
    // Falls back to hardcoded model heuristics + config tool_dispatcher.
    let native_tools = match provider.cached_model_tool_support(model_name) {
        Some(ollama_says_tools) => {
            ollama_says_tools && native_tool_transport_supported(provider_name, model_name)
        }
        None => configured_native_tools_enabled(
            &config.agent.tool_dispatcher,
            provider_name,
            model_name,
            provider.supports_native_tools(),
        ),
    };
    let mut system_prompt = crate::channels::build_system_prompt_with_mode(
        &config.workspace_dir,
        model_name,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        native_tools,
        config.skills.prompt_injection_mode,
    );

    // Append structured tool-use instructions with schemas (only for non-native providers)
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(&tools_registry));
    }
    system_prompt.push_str(&build_shell_policy_instructions(&config.autonomy));
    system_prompt.push_str(&build_runtime_tool_availability_notice(&tools_registry));
    system_prompt.push_str(&build_managed_app_runtime_notice());
    system_prompt.push_str(&build_ipc_state_usage_instructions(&tools_registry));
    system_prompt.push_str(&build_auto_plan_execute_instructions());

    // ── Approval manager (supervised mode) ───────────────────────
    let approval_manager = if interactive {
        Some(ApprovalManager::from_config(&config.autonomy))
    } else {
        None
    };
    let channel_name = if interactive { "cli" } else { "daemon" };

    // ── Execute ──────────────────────────────────────────────────
    let start = Instant::now();

    let mut final_output = String::new();

    if let Some(msg) = message {
        // Auto-save user message to memory (skip short/trivial messages)
        if config.memory.auto_save && msg.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS {
            let user_key = autosave_memory_key("user_msg");
            let _ = mem
                .store(&user_key, &msg, MemoryCategory::Conversation, None)
                .await;
        }

        // Inject memory + hardware RAG context into user message
        let mem_context =
            build_context(mem.as_ref(), &msg, config.memory.min_relevance_score).await;
        let rag_limit = if config.agent.compact_context { 2 } else { 5 };
        let hw_context = hardware_rag
            .as_ref()
            .map(|r| build_hardware_context(r, &msg, &board_names, rag_limit))
            .unwrap_or_default();
        let context = format!("{mem_context}{hw_context}");
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
        let enriched = if context.is_empty() {
            format!("[{now}] {msg}")
        } else {
            format!("{context}[{now}] {msg}")
        };

        let mut history = vec![
            ChatMessage::system(&system_prompt),
            ChatMessage::user(&enriched),
        ];

        let response = with_tool_loop_settings(
            config.agent.parallel_tools,
            native_tools,
            with_tool_loop_history_limit(
                config.agent.max_history_messages,
                run_tool_call_loop(
                    provider.as_ref(),
                    &mut history,
                    &tools_registry,
                    observer.as_ref(),
                    provider_name,
                    model_name,
                    temperature,
                    false,
                    approval_manager.as_ref(),
                    channel_name,
                    &config.multimodal,
                    config.agent.max_tool_iterations,
                    None,
                    None,
                    None,
                    &[],
                ),
            ),
        )
        .await?;
        final_output = response.clone();
        println!("{response}");
        observer.record_event(&ObserverEvent::TurnComplete);
    } else {
        println!("🦀 LlamaFarm Interactive Mode");
        println!("Type /help for commands.\n");
        let cli = crate::channels::CliChannel::new();

        // Persistent conversation history across turns
        let mut history = vec![ChatMessage::system(&system_prompt)];
        // Reusable readline editor for UTF-8 input support
        let mut rl = rustyline::DefaultEditor::new()?;

        loop {
            let input = match rl.readline("> ") {
                Ok(line) => line,
                Err(ReadlineError::Interrupted | ReadlineError::Eof) => {
                    break;
                }
                Err(e) => {
                    eprintln!("\nError reading input: {e}\n");
                    break;
                }
            };

            let user_input = input.trim().to_string();
            if user_input.is_empty() {
                continue;
            }
            rl.add_history_entry(&input)?;
            match user_input.as_str() {
                "/quit" | "/exit" => break,
                "/help" => {
                    println!("Available commands:");
                    println!("  /help        Show this help message");
                    println!("  /clear /new  Clear conversation history");
                    println!("  /quit /exit  Exit interactive mode\n");
                    continue;
                }
                "/clear" | "/new" => {
                    println!(
                        "This will clear the current conversation and delete all session memory."
                    );
                    println!("Core memories (long-term facts/preferences) will be preserved.");
                    let confirm = rl.readline("Continue? [y/N] ").unwrap_or_default();

                    if !matches!(confirm.trim().to_lowercase().as_str(), "y" | "yes") {
                        println!("Cancelled.\n");
                        continue;
                    }

                    // Ensure prior prompts are not navigable after reset.
                    rl.clear_history()?;
                    history.clear();
                    history.push(ChatMessage::system(&system_prompt));
                    // Clear conversation and daily memory
                    let mut cleared = 0;
                    for category in [MemoryCategory::Conversation, MemoryCategory::Daily] {
                        let entries = mem.list(Some(&category), None).await.unwrap_or_default();
                        for entry in entries {
                            if mem.forget(&entry.key).await.unwrap_or(false) {
                                cleared += 1;
                            }
                        }
                    }
                    if cleared > 0 {
                        println!("Conversation cleared ({cleared} memory entries removed).\n");
                    } else {
                        println!("Conversation cleared.\n");
                    }
                    continue;
                }
                _ => {}
            }

            // Auto-save conversation turns (skip short/trivial messages)
            if config.memory.auto_save && user_input.chars().count() >= AUTOSAVE_MIN_MESSAGE_CHARS {
                let user_key = autosave_memory_key("user_msg");
                let _ = mem
                    .store(&user_key, &user_input, MemoryCategory::Conversation, None)
                    .await;
            }

            // Inject memory + hardware RAG context into user message
            let mem_context =
                build_context(mem.as_ref(), &user_input, config.memory.min_relevance_score).await;
            let rag_limit = if config.agent.compact_context { 2 } else { 5 };
            let hw_context = hardware_rag
                .as_ref()
                .map(|r| build_hardware_context(r, &user_input, &board_names, rag_limit))
                .unwrap_or_default();
            let context = format!("{mem_context}{hw_context}");
            let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
            let enriched = if context.is_empty() {
                format!("[{now}] {user_input}")
            } else {
                format!("{context}[{now}] {user_input}")
            };

            history.push(ChatMessage::user(&enriched));

            let response = match with_tool_loop_settings(
                config.agent.parallel_tools,
                native_tools,
                with_tool_loop_history_limit(
                    config.agent.max_history_messages,
                    run_tool_call_loop(
                        provider.as_ref(),
                        &mut history,
                        &tools_registry,
                        observer.as_ref(),
                        provider_name,
                        model_name,
                        temperature,
                        false,
                        approval_manager.as_ref(),
                        channel_name,
                        &config.multimodal,
                        config.agent.max_tool_iterations,
                        None,
                        None,
                        None,
                        &[],
                    ),
                ),
            )
            .await
            {
                Ok(resp) => resp,
                Err(e) => {
                    if is_tool_iteration_limit_error(&e) {
                        let pause_notice = format!(
                            "⚠️ Reached tool-iteration limit ({}). Context and progress are preserved. \
                            Reply \"continue\" to resume, or increase `agent.max_tool_iterations` in config.",
                            config.agent.max_tool_iterations
                        );
                        history.push(ChatMessage::assistant(&pause_notice));
                        eprintln!("\n{pause_notice}\n");
                        continue;
                    }
                    eprintln!("\nError: {e}\n");
                    continue;
                }
            };
            final_output = response.clone();
            if let Err(e) = crate::channels::Channel::send(
                &cli,
                &crate::channels::traits::SendMessage::new(format!("\n{response}\n"), "user"),
            )
            .await
            {
                eprintln!("\nError sending CLI response: {e}\n");
            }
            observer.record_event(&ObserverEvent::TurnComplete);

            // Auto-compaction before hard trimming to preserve long-context signal.
            if let Ok(compacted) = auto_compact_history(
                &mut history,
                provider.as_ref(),
                model_name,
                config.agent.max_history_messages,
                Some(mem.as_ref()),
            )
            .await
            {
                if compacted {
                    println!("🧹 Auto-compaction complete");
                }
            }

            // Hard cap as a safety net.
            trim_history(&mut history, config.agent.max_history_messages);
        }
    }

    let duration = start.elapsed();
    observer.record_event(&ObserverEvent::AgentEnd {
        provider: provider_name.to_string(),
        model: model_name.to_string(),
        duration,
        tokens_used: None,
        cost_usd: None,
    });

    Ok(final_output)
}

/// Process a single message through the full agent (with tools, peripherals, memory).
/// Used by channels (Telegram, Discord, etc.) to enable hardware and tool use.
pub async fn process_message(config: Config, message: &str) -> Result<String> {
    let observer: Arc<dyn Observer> =
        Arc::from(observability::create_observer(&config.observability));
    let runtime: Arc<dyn runtime::RuntimeAdapter> =
        Arc::from(runtime::create_runtime(&config.runtime)?);
    let security = Arc::new(SecurityPolicy::from_config(
        &config.autonomy,
        &config.workspace_dir,
    ));
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory_with_storage(
        &config.memory,
        Some(&config.storage.provider.config),
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);

    let (composio_key, composio_entity_id) = if config.composio.enabled {
        (
            config.composio.api_key.as_deref(),
            Some(config.composio.entity_id.as_str()),
        )
    } else {
        (None, None)
    };
    let mut tools_registry = tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        runtime,
        mem.clone(),
        composio_key,
        composio_entity_id,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &config.workspace_dir,
        &config.agents,
        config.api_key.as_deref(),
        &config,
    );
    let peripheral_tools: Vec<Box<dyn Tool>> =
        crate::peripherals::create_peripheral_tools(&config.peripherals).await?;
    tools_registry.extend(peripheral_tools);

    let provider_name = config.default_provider.as_deref().unwrap_or("openrouter");
    let model_name = config
        .default_model
        .clone()
        .unwrap_or_else(|| "anthropic/claude-sonnet-4-20250514".into());
    let provider_runtime_options = providers::ProviderRuntimeOptions::from_config(&config);
    let provider: Box<dyn Provider> = providers::create_routed_provider_with_options(
        provider_name,
        config.api_key.as_deref(),
        config.api_url.as_deref(),
        &config.reliability,
        &config.model_routes,
        &model_name,
        &provider_runtime_options,
    )?;

    let hardware_rag: Option<crate::rag::HardwareRag> = config
        .peripherals
        .datasheet_dir
        .as_ref()
        .filter(|d| !d.trim().is_empty())
        .map(|dir| crate::rag::HardwareRag::load(&config.workspace_dir, dir.trim()))
        .and_then(Result::ok)
        .filter(|r: &crate::rag::HardwareRag| !r.is_empty());
    let board_names: Vec<String> = config
        .peripherals
        .boards
        .iter()
        .map(|b| b.board.clone())
        .collect();

    let skills = crate::skills::load_skills_with_config(&config.workspace_dir, &config);
    let mut tool_descs: Vec<(&str, &str)> = vec![
        ("shell", "Execute terminal commands."),
        ("file_read", "Read file contents."),
        ("file_write", "Write file contents."),
        ("memory_store", "Save to memory."),
        ("memory_recall", "Search memory."),
        ("memory_forget", "Delete a memory entry."),
        (
            "model_routing_config",
            "Configure default model, scenario routing, and delegate agents.",
        ),
        ("screenshot", "Capture a screenshot."),
        ("image_info", "Read image metadata."),
    ];
    if config.browser.enabled {
        tool_descs.push(("browser_open", "Open approved URLs in browser."));
    }
    if config.composio.enabled {
        tool_descs.push(("composio", "Execute actions on 1000+ apps via Composio."));
    }
    if config.peripherals.enabled && !config.peripherals.boards.is_empty() {
        tool_descs.push(("gpio_read", "Read GPIO pin value on connected hardware."));
        tool_descs.push((
            "gpio_write",
            "Set GPIO pin high or low on connected hardware.",
        ));
        tool_descs.push((
            "arduino_upload",
            "Upload Arduino sketch. Use for 'make a heart', custom patterns. You write full .ino code; Ollama uploads it.",
        ));
        tool_descs.push((
            "hardware_memory_map",
            "Return flash and RAM address ranges. Use when user asks for memory addresses or memory map.",
        ));
        tool_descs.push((
            "hardware_board_info",
            "Return full board info (chip, architecture, memory map). Use when user asks for board info, what board, connected hardware, or chip info.",
        ));
        tool_descs.push((
            "hardware_memory_read",
            "Read actual memory/register values from Nucleo. Use when user asks to read registers, read memory, dump lower memory 0-126, or give address and value.",
        ));
        tool_descs.push((
            "hardware_capabilities",
            "Query connected hardware for reported GPIO pins and LED pin. Use when user asks what pins are available.",
        ));
    }
    let bootstrap_max_chars = Some(if config.agent.compact_context {
        6000
    } else {
        usize::MAX
    });
    let native_tools = match provider.cached_model_tool_support(&model_name) {
        Some(ollama_says_tools) => {
            ollama_says_tools && native_tool_transport_supported(provider_name, &model_name)
        }
        None => configured_native_tools_enabled(
            &config.agent.tool_dispatcher,
            provider_name,
            &model_name,
            provider.supports_native_tools(),
        ),
    };
    let mut system_prompt = crate::channels::build_system_prompt_with_mode(
        &config.workspace_dir,
        &model_name,
        &tool_descs,
        &skills,
        Some(&config.identity),
        bootstrap_max_chars,
        native_tools,
        config.skills.prompt_injection_mode,
    );
    if !native_tools {
        system_prompt.push_str(&build_tool_instructions(&tools_registry));
    }
    system_prompt.push_str(&build_shell_policy_instructions(&config.autonomy));
    system_prompt.push_str(&build_runtime_tool_availability_notice(&tools_registry));
    system_prompt.push_str(&build_managed_app_runtime_notice());
    system_prompt.push_str(&build_ipc_state_usage_instructions(&tools_registry));
    system_prompt.push_str(&build_auto_plan_execute_instructions());

    let mem_context = build_context(mem.as_ref(), message, config.memory.min_relevance_score).await;
    let rag_limit = if config.agent.compact_context { 2 } else { 5 };
    let hw_context = hardware_rag
        .as_ref()
        .map(|r| build_hardware_context(r, message, &board_names, rag_limit))
        .unwrap_or_default();
    let context = format!("{mem_context}{hw_context}");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S %Z");
    let enriched = if context.is_empty() {
        format!("[{now}] {message}")
    } else {
        format!("{context}[{now}] {message}")
    };

    let mut history = vec![
        ChatMessage::system(&system_prompt),
        ChatMessage::user(&enriched),
    ];

    with_tool_loop_settings(
        config.agent.parallel_tools,
        native_tools,
        with_tool_loop_history_limit(
            config.agent.max_history_messages,
            agent_turn(
                provider.as_ref(),
                &mut history,
                &tools_registry,
                observer.as_ref(),
                provider_name,
                &model_name,
                config.default_temperature,
                true,
                &config.multimodal,
                config.agent.max_tool_iterations,
            ),
        ),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn test_scrub_credentials() {
        let input = "API_KEY=sk-1234567890abcdef; token: 1234567890; password=\"secret123456\"";
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("API_KEY=sk-1*[REDACTED]"));
        assert!(scrubbed.contains("token: 1234*[REDACTED]"));
        assert!(scrubbed.contains("password=\"secr*[REDACTED]\""));
        assert!(!scrubbed.contains("abcdef"));
        assert!(!scrubbed.contains("secret123456"));
    }

    #[test]
    fn configured_native_tools_enabled_honors_xml_override_for_non_preferred_models() {
        // xml override works for models without a native preference
        assert!(!configured_native_tools_enabled(
            "xml",
            "ollama",
            "devstral-small-2:latest",
            true
        ));
    }

    #[test]
    fn configured_native_tools_enabled_qwen3_ignores_xml_override() {
        // qwen3.x always uses native tools regardless of tool_dispatcher
        assert!(configured_native_tools_enabled(
            "xml",
            "ollama",
            "qwen3-coder:30b",
            true
        ));
        assert!(configured_native_tools_enabled(
            "xml",
            "ollama",
            "qwen3.6:35b",
            true
        ));
    }

    #[test]
    fn configured_native_tools_enabled_uses_provider_capability_outside_xml_mode() {
        assert!(configured_native_tools_enabled(
            "auto",
            "ollama",
            "qwen3-coder:30b",
            true
        ));
        assert!(!configured_native_tools_enabled(
            "native",
            "ollama",
            "qwen3-coder:30b",
            false
        ));
    }

    #[test]
    fn configured_native_tools_enabled_disables_ollama_gpt_oss_models() {
        assert!(!configured_native_tools_enabled(
            "auto",
            "ollama",
            "gpt-oss:120b",
            true
        ));
        assert!(!configured_native_tools_enabled(
            "auto",
            "ollama",
            "gpt-oss:20b:cloud",
            true
        ));
    }

    #[test]
    fn configured_native_tools_enabled_preserves_non_ollama_gpt_oss_models() {
        assert!(configured_native_tools_enabled(
            "auto",
            "openai",
            "openai/gpt-oss-120b",
            true
        ));
    }

    #[test]
    fn inject_prompt_tool_fallback_instructions_appends_tool_protocol_once() {
        let mut history = vec![ChatMessage::system("Base prompt")];
        let security = Arc::new(SecurityPolicy::from_config(
            &crate::config::AutonomyConfig::default(),
            std::path::Path::new("."),
        ));
        let tools_registry = tools::default_tools(security);
        let tool_specs: Vec<crate::tools::ToolSpec> =
            tools_registry.iter().map(|tool| tool.spec()).collect();

        inject_prompt_tool_fallback_instructions(&mut history, &tool_specs);
        inject_prompt_tool_fallback_instructions(&mut history, &tool_specs);

        let system_prompt = &history[0].content;
        assert!(system_prompt.contains("## Compatibility Fallback"));
        assert!(system_prompt.contains("## Tool Use Protocol"));
        assert_eq!(
            system_prompt.matches("## Compatibility Fallback").count(),
            1
        );
    }

    #[test]
    fn inject_prompt_tool_fallback_instructions_uses_only_selected_specs() {
        let mut history = vec![ChatMessage::system("Base prompt")];
        let all_specs = vec![
            crate::tools::ToolSpec {
                name: "selected_tool".to_string(),
                description: "Selected tool description".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "selected_field": { "type": "string" }
                    }
                }),
            },
            crate::tools::ToolSpec {
                name: "excluded_tool".to_string(),
                description: "Excluded tool description".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "excluded_field": { "type": "string" }
                    }
                }),
            },
        ];
        let selected_specs: Vec<_> = all_specs
            .into_iter()
            .filter(|spec| spec.name == "selected_tool")
            .collect();

        inject_prompt_tool_fallback_instructions(&mut history, &selected_specs);

        let system_prompt = &history[0].content;
        assert!(system_prompt.contains("**selected_tool**"));
        assert!(system_prompt.contains("selected_field"));
        assert!(!system_prompt.contains("excluded_tool"));
        assert!(!system_prompt.contains("excluded_field"));
        assert!(!system_prompt.contains("`shell`"));
        assert!(!system_prompt.contains("`cron_add`"));
        assert!(!system_prompt.contains("`file_read`"));
        assert!(!system_prompt.contains("`file_write`"));
        assert!(!system_prompt.contains("`file_edit`"));
    }

    #[test]
    fn test_scrub_credentials_json() {
        let input = r#"{"api_key": "sk-1234567890", "other": "public"}"#;
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains("\"api_key\": \"sk-1*[REDACTED]\""));
        assert!(scrubbed.contains("public"));
    }

    #[test]
    fn test_scrub_credentials_toml_value_with_colon_preserves_equals_delimiter() {
        let input = r#"api_key = "enc2:QmFzZTY0VG9rZW4=""#;
        let scrubbed = scrub_credentials(input);
        assert!(scrubbed.contains(r#"api_key = "enc2*[REDACTED]""#));
        assert!(!scrubbed.contains(r#""api_key":"#));
    }

    #[test]
    fn maybe_inject_cron_add_delivery_populates_agent_delivery_from_channel_context() {
        let mut args = serde_json::json!({
            "job_type": "agent",
            "prompt": "remind me later"
        });

        maybe_inject_cron_add_delivery("cron_add", &mut args, "telegram", Some("-10012345"));

        assert_eq!(args["delivery"]["mode"], "announce");
        assert_eq!(args["delivery"]["channel"], "telegram");
        assert_eq!(args["delivery"]["to"], "-10012345");
    }

    #[test]
    fn maybe_inject_cron_add_delivery_does_not_override_explicit_target() {
        let mut args = serde_json::json!({
            "job_type": "agent",
            "prompt": "remind me later",
            "delivery": {
                "mode": "announce",
                "channel": "discord",
                "to": "C123"
            }
        });

        maybe_inject_cron_add_delivery("cron_add", &mut args, "telegram", Some("-10012345"));

        assert_eq!(args["delivery"]["channel"], "discord");
        assert_eq!(args["delivery"]["to"], "C123");
    }

    #[test]
    fn maybe_inject_cron_add_delivery_skips_shell_jobs() {
        let mut args = serde_json::json!({
            "job_type": "shell",
            "command": "echo hello"
        });

        maybe_inject_cron_add_delivery("cron_add", &mut args, "telegram", Some("-10012345"));

        assert!(args.get("delivery").is_none());
    }

    use crate::memory::{Memory, MemoryCategory, SqliteMemory};
    use crate::observability::NoopObserver;
    use crate::providers::traits::{ProviderCapabilities, StreamChunk, StreamOptions};
    use crate::providers::ChatResponse;
    use crate::runtime::NativeRuntime;
    use crate::security::{AutonomyLevel, SecurityPolicy, ShellRedirectPolicy};
    use tempfile::TempDir;

    struct NonVisionProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for NonVisionProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }
    }

    struct VisionProvider {
        calls: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl Provider for VisionProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities {
                native_tool_calling: false,
                vision: true,
            }
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok("ok".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let marker_count = crate::multimodal::count_image_markers(request.messages);
            if marker_count == 0 {
                anyhow::bail!("expected image markers in request messages");
            }

            if request.tools.is_some() {
                anyhow::bail!("no tools should be attached for this test");
            }

            Ok(ChatResponse {
                text: Some("vision-ok".to_string()),
                tool_calls: Vec::new(),
                usage: None,
                metrics: None,
                reasoning_content: None,
            })
        }
    }

    struct ScriptedProvider {
        responses: Arc<Mutex<VecDeque<ChatResponse>>>,
        capabilities: ProviderCapabilities,
    }

    impl ScriptedProvider {
        fn from_text_responses(responses: Vec<&str>) -> Self {
            let scripted = responses
                .into_iter()
                .map(|text| ChatResponse {
                    text: Some(text.to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                })
                .collect();
            Self {
                responses: Arc::new(Mutex::new(scripted)),
                capabilities: ProviderCapabilities::default(),
            }
        }

        fn with_native_tool_support(mut self) -> Self {
            self.capabilities.native_tool_calling = true;
            self
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in scripted provider tests");
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            let mut responses = self
                .responses
                .lock()
                .expect("responses lock should be valid");
            responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted provider exhausted responses"))
        }
    }

    struct RecordingScriptedProvider {
        responses: Arc<Mutex<VecDeque<ChatResponse>>>,
        recorded_requests: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
        capabilities: ProviderCapabilities,
    }

    impl RecordingScriptedProvider {
        fn from_text_responses(responses: Vec<&str>) -> Self {
            let scripted = responses
                .into_iter()
                .map(|text| ChatResponse {
                    text: Some(text.to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                })
                .collect();
            Self {
                responses: Arc::new(Mutex::new(scripted)),
                recorded_requests: Arc::new(Mutex::new(Vec::new())),
                capabilities: ProviderCapabilities::default(),
            }
        }

        fn recorded_requests(&self) -> Vec<Vec<ChatMessage>> {
            self.recorded_requests
                .lock()
                .expect("recorded request lock should be valid")
                .clone()
        }
    }

    #[async_trait]
    impl Provider for RecordingScriptedProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!(
                "chat_with_system should not be used in recording scripted provider tests"
            );
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.recorded_requests
                .lock()
                .expect("recorded request lock should be valid")
                .push(request.messages.to_vec());
            let mut responses = self
                .responses
                .lock()
                .expect("responses lock should be valid");
            responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted provider exhausted responses"))
        }
    }

    struct RecordingSummarizingProvider {
        responses: Arc<Mutex<VecDeque<ChatResponse>>>,
        recorded_requests: Arc<Mutex<Vec<Vec<ChatMessage>>>>,
        summary_calls: Arc<AtomicUsize>,
    }

    impl RecordingSummarizingProvider {
        fn from_text_responses(responses: Vec<&str>) -> Self {
            let scripted = responses
                .into_iter()
                .map(|text| ChatResponse {
                    text: Some(text.to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                })
                .collect();
            Self {
                responses: Arc::new(Mutex::new(scripted)),
                recorded_requests: Arc::new(Mutex::new(Vec::new())),
                summary_calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn recorded_requests(&self) -> Vec<Vec<ChatMessage>> {
            self.recorded_requests
                .lock()
                .expect("recorded request lock should be valid")
                .clone()
        }

        fn summary_calls(&self) -> usize {
            self.summary_calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Provider for RecordingSummarizingProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            self.summary_calls.fetch_add(1, Ordering::SeqCst);
            Ok("- preserved context summary".to_string())
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.recorded_requests
                .lock()
                .expect("recorded request lock should be valid")
                .push(request.messages.to_vec());
            let mut responses = self
                .responses
                .lock()
                .expect("responses lock should be valid");
            responses
                .pop_front()
                .ok_or_else(|| anyhow::anyhow!("scripted provider exhausted responses"))
        }
    }

    struct SummarizingProvider;

    #[async_trait]
    impl Provider for SummarizingProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok("- preserved context summary".to_string())
        }
    }

    struct StreamingScriptedProvider {
        responses: Arc<Mutex<VecDeque<String>>>,
        stream_calls: Arc<AtomicUsize>,
        chat_calls: Arc<AtomicUsize>,
    }

    impl StreamingScriptedProvider {
        fn from_text_responses(responses: Vec<&str>) -> Self {
            Self {
                responses: Arc::new(Mutex::new(
                    responses.into_iter().map(ToString::to_string).collect(),
                )),
                stream_calls: Arc::new(AtomicUsize::new(0)),
                chat_calls: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    #[async_trait]
    impl Provider for StreamingScriptedProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!(
                "chat_with_system should not be used in streaming scripted provider tests"
            );
        }

        async fn chat(
            &self,
            _request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.chat_calls.fetch_add(1, Ordering::SeqCst);
            anyhow::bail!("chat should not be called when streaming succeeds")
        }

        fn supports_streaming(&self) -> bool {
            true
        }

        fn stream_chat_with_history(
            &self,
            _messages: &[ChatMessage],
            _model: &str,
            _temperature: f64,
            options: StreamOptions,
        ) -> futures_util::stream::BoxStream<
            'static,
            crate::providers::traits::StreamResult<StreamChunk>,
        > {
            self.stream_calls.fetch_add(1, Ordering::SeqCst);
            if !options.enabled {
                return Box::pin(futures_util::stream::empty());
            }

            let response = self
                .responses
                .lock()
                .expect("responses lock should be valid")
                .pop_front()
                .unwrap_or_default();

            Box::pin(futures_util::stream::iter(vec![
                Ok(StreamChunk::delta(response)),
                Ok(StreamChunk::final_chunk()),
            ]))
        }
    }

    struct CountingTool {
        name: String,
        invocations: Arc<AtomicUsize>,
    }

    impl CountingTool {
        fn new(name: &str, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                invocations,
            }
        }
    }

    #[async_trait]
    impl Tool for CountingTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Counts executions for loop-stability tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(crate::tools::ToolResult {
                success: true,
                output: format!("counted:{value}"),
                error: None,
            })
        }
    }

    struct CommandEchoTool {
        name: String,
        invocations: Arc<AtomicUsize>,
    }

    impl CommandEchoTool {
        fn new(name: &str, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                invocations,
            }
        }
    }

    #[async_trait]
    impl Tool for CommandEchoTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Echoes shell-style command arguments for parser and retry tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string" }
                }
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            let command = args
                .get("command")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Ok(crate::tools::ToolResult {
                success: true,
                output: format!("ran:{command}"),
                error: None,
            })
        }
    }

    struct StaticOutputTool {
        name: String,
        output: String,
        success: bool,
        invocations: Arc<AtomicUsize>,
    }

    impl StaticOutputTool {
        fn new(name: &str, output: &str, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                output: output.to_string(),
                success: true,
                invocations,
            }
        }

        fn failing(name: &str, error: &str, invocations: Arc<AtomicUsize>) -> Self {
            Self {
                name: name.to_string(),
                output: error.to_string(),
                success: false,
                invocations,
            }
        }
    }

    #[async_trait]
    impl Tool for StaticOutputTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Returns a fixed tool output for grounding tests"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" }
                }
            })
        }

        async fn execute(
            &self,
            _args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            self.invocations.fetch_add(1, Ordering::SeqCst);
            Ok(crate::tools::ToolResult {
                success: self.success,
                output: if self.success {
                    self.output.clone()
                } else {
                    String::new()
                },
                error: if self.success {
                    None
                } else {
                    Some(self.output.clone())
                },
            })
        }
    }

    struct ResultScriptedProvider {
        responses: Arc<Mutex<VecDeque<anyhow::Result<ChatResponse>>>>,
        capabilities: ProviderCapabilities,
        calls: Arc<AtomicUsize>,
        native_tool_requests: Arc<Mutex<Vec<bool>>>,
    }

    impl ResultScriptedProvider {
        fn from_results(
            responses: Vec<anyhow::Result<ChatResponse>>,
            calls: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                responses: Arc::new(Mutex::new(responses.into())),
                capabilities: ProviderCapabilities::default(),
                calls,
                native_tool_requests: Arc::new(Mutex::new(Vec::new())),
            }
        }

        fn with_native_tool_support(mut self) -> Self {
            self.capabilities.native_tool_calling = true;
            self
        }

        fn native_tool_requests(&self) -> Vec<bool> {
            self.native_tool_requests
                .lock()
                .expect("native tool request lock should be valid")
                .clone()
        }
    }

    #[async_trait]
    impl Provider for ResultScriptedProvider {
        fn capabilities(&self) -> ProviderCapabilities {
            self.capabilities.clone()
        }

        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            anyhow::bail!("chat_with_system should not be used in result scripted provider tests");
        }

        async fn chat(
            &self,
            request: ChatRequest<'_>,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<ChatResponse> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.native_tool_requests
                .lock()
                .expect("native tool request lock should be valid")
                .push(request.tools.is_some());
            let mut responses = self
                .responses
                .lock()
                .expect("responses lock should be valid");
            responses.pop_front().unwrap_or_else(|| {
                Err(anyhow::anyhow!(
                    "result scripted provider exhausted responses"
                ))
            })
        }
    }

    struct DelayTool {
        name: String,
        delay_ms: u64,
        active: Arc<AtomicUsize>,
        max_active: Arc<AtomicUsize>,
    }

    impl DelayTool {
        fn new(
            name: &str,
            delay_ms: u64,
            active: Arc<AtomicUsize>,
            max_active: Arc<AtomicUsize>,
        ) -> Self {
            Self {
                name: name.to_string(),
                delay_ms,
                active,
                max_active,
            }
        }
    }

    #[async_trait]
    impl Tool for DelayTool {
        fn name(&self) -> &str {
            &self.name
        }

        fn description(&self) -> &str {
            "Delay tool for testing parallel tool execution"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                },
                "required": ["value"]
            })
        }

        async fn execute(
            &self,
            args: serde_json::Value,
        ) -> anyhow::Result<crate::tools::ToolResult> {
            let now_active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now_active, Ordering::SeqCst);

            tokio::time::sleep(Duration::from_millis(self.delay_ms)).await;

            self.active.fetch_sub(1, Ordering::SeqCst);

            let value = args
                .get("value")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();

            Ok(crate::tools::ToolResult {
                success: true,
                output: format!("ok:{value}"),
                error: None,
            })
        }
    }

    #[tokio::test]
    async fn run_tool_call_loop_returns_structured_error_for_non_vision_provider() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = NonVisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "please inspect [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect_err("provider without vision support should fail");

        assert!(err.to_string().contains("provider_capability_error"));
        assert!(err.to_string().contains("capability=vision"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_rejects_oversized_image_payload() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = VisionProvider {
            calls: Arc::clone(&calls),
        };

        let oversized_payload = STANDARD.encode(vec![0_u8; (1024 * 1024) + 1]);
        let mut history = vec![ChatMessage::user(format!(
            "[IMAGE:data:image/png;base64,{oversized_payload}]"
        ))];

        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;
        let multimodal = crate::config::MultimodalConfig {
            max_images: 4,
            max_image_size_mb: 1,
            allow_remote_fetch: false,
        };

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &multimodal,
            3,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect_err("oversized payload must fail");

        assert!(err
            .to_string()
            .contains("multimodal image size limit exceeded"));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_accepts_valid_multimodal_request_flow() {
        let calls = Arc::new(AtomicUsize::new(0));
        let provider = VisionProvider {
            calls: Arc::clone(&calls),
        };

        let mut history = vec![ChatMessage::user(
            "Analyze this [IMAGE:data:image/png;base64,iVBORw0KGgo=]".to_string(),
        )];
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            3,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("valid multimodal payload should pass");

        assert_eq!(result, "vision-ok");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_false_for_single_call() {
        let calls = vec![ParsedToolCall {
            name: "file_read".to_string(),
            arguments: serde_json::json!({"path": "a.txt"}),
            tool_call_id: None,
        }];

        assert!(!should_execute_tools_in_parallel(&calls, &[], None));
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_false_when_approval_is_required() {
        let calls = vec![
            ParsedToolCall {
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "pwd"}),
                tool_call_id: None,
            },
            ParsedToolCall {
                name: "http_request".to_string(),
                arguments: serde_json::json!({"url": "https://example.com"}),
                tool_call_id: None,
            },
        ];
        let approval_cfg = crate::config::AutonomyConfig::default();
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        assert!(!should_execute_tools_in_parallel(
            &calls,
            &[],
            Some(&approval_mgr)
        ));
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_false_for_write_tools() {
        // Write tools (shell, http_request) must never run in parallel even without approval.
        let calls = vec![
            ParsedToolCall {
                name: "shell".to_string(),
                arguments: serde_json::json!({"command": "pwd"}),
                tool_call_id: None,
            },
            ParsedToolCall {
                name: "http_request".to_string(),
                arguments: serde_json::json!({"url": "https://example.com"}),
                tool_call_id: None,
            },
        ];
        let approval_cfg = crate::config::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..crate::config::AutonomyConfig::default()
        };
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        // No tools in registry → both unknown → not concurrency-safe → false.
        assert!(!should_execute_tools_in_parallel(
            &calls,
            &[],
            Some(&approval_mgr)
        ));
    }

    #[test]
    fn should_execute_tools_in_parallel_returns_true_for_read_only_tools() {
        use crate::security::SecurityPolicy;
        use crate::tools::{FileReadTool, GlobSearchTool};
        use std::sync::Arc;
        let sec = Arc::new(SecurityPolicy::default());
        let registry: Vec<Box<dyn crate::tools::Tool>> = vec![
            Box::new(FileReadTool::new(sec.clone())),
            Box::new(GlobSearchTool::new(sec.clone())),
        ];
        let calls = vec![
            ParsedToolCall {
                name: "file_read".to_string(),
                arguments: serde_json::json!({"path": "a.txt"}),
                tool_call_id: None,
            },
            ParsedToolCall {
                name: "glob_search".to_string(),
                arguments: serde_json::json!({"pattern": "**/*.rs"}),
                tool_call_id: None,
            },
        ];
        let approval_cfg = crate::config::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..crate::config::AutonomyConfig::default()
        };
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        assert!(should_execute_tools_in_parallel(
            &calls,
            &registry,
            Some(&approval_mgr)
        ));
    }

    #[tokio::test]
    async fn run_tool_call_loop_executes_multiple_tools_with_ordered_results() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"delay_a","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"delay_b","arguments":{"value":"B"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(DelayTool::new(
                "delay_a",
                200,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
            Box::new(DelayTool::new(
                "delay_b",
                200,
                Arc::clone(&active),
                Arc::clone(&max_active),
            )),
        ];

        let approval_cfg = crate::config::AutonomyConfig {
            level: crate::security::AutonomyLevel::Full,
            ..crate::config::AutonomyConfig::default()
        };
        let approval_mgr = ApprovalManager::from_config(&approval_cfg);

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("parallel execution should complete");

        assert_eq!(result, "done");
        assert!(
            max_active.load(Ordering::SeqCst) >= 1,
            "tools should execute successfully"
        );

        let tool_results_message = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("tool results message should be present");
        let idx_a = tool_results_message
            .content
            .find("name=\"delay_a\"")
            .expect("delay_a result should be present");
        let idx_b = tool_results_message
            .content
            .find("name=\"delay_b\"")
            .expect("delay_b result should be present");
        assert!(
            idx_a < idx_b,
            "tool results should preserve input order for tool call mapping"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_denies_supervised_tools_on_non_cli_channels() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hi"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(DelayTool::new(
            "shell",
            50,
            Arc::clone(&active),
            Arc::clone(&max_active),
        ))];

        let approval_mgr = ApprovalManager::from_config(&crate::config::AutonomyConfig::default());

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("tool loop should complete with denied tool execution");

        assert_eq!(result, "done");
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            0,
            "shell tool must not execute when approval is unavailable on non-CLI channels"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_waits_for_non_cli_approval_resolution() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hi"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(DelayTool::new(
            "shell",
            50,
            Arc::clone(&active),
            Arc::clone(&max_active),
        ))];

        let approval_mgr = Arc::new(ApprovalManager::from_config(
            &crate::config::AutonomyConfig::default(),
        ));
        let (prompt_tx, mut prompt_rx) =
            tokio::sync::mpsc::unbounded_channel::<NonCliApprovalPrompt>();
        let approval_mgr_for_task = Arc::clone(&approval_mgr);
        let approval_task = tokio::spawn(async move {
            let prompt = prompt_rx
                .recv()
                .await
                .expect("approval prompt should arrive");
            approval_mgr_for_task
                .confirm_non_cli_pending_request(
                    &prompt.request_id,
                    "alice",
                    "telegram",
                    "chat-approval",
                )
                .expect("pending approval should confirm");
            approval_mgr_for_task
                .record_non_cli_pending_resolution(&prompt.request_id, ApprovalResponse::Yes);
        });

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop_with_non_cli_approval_context(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(approval_mgr.as_ref()),
            "telegram",
            Some(NonCliApprovalContext {
                sender: "alice".to_string(),
                reply_target: "chat-approval".to_string(),
                prompt_tx,
            }),
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("tool loop should continue after non-cli approval");

        approval_task.await.expect("approval task should complete");
        assert_eq!(result, "done");
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "shell tool should execute after non-cli approval is resolved"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_consumes_one_time_non_cli_allow_all_token() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hi"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(DelayTool::new(
            "shell",
            50,
            Arc::clone(&active),
            Arc::clone(&max_active),
        ))];

        let approval_mgr = ApprovalManager::from_config(&crate::config::AutonomyConfig::default());
        approval_mgr.grant_non_cli_allow_all_once();
        assert_eq!(approval_mgr.non_cli_allow_all_once_remaining(), 1);

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell once"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            Some(&approval_mgr),
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("tool loop should consume one-time allow-all token");

        assert_eq!(result, "done");
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            1,
            "shell tool should execute after consuming one-time allow-all token"
        );
        assert_eq!(approval_mgr.non_cli_allow_all_once_remaining(), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_blocks_tools_excluded_for_channel() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo hi"}}
</tool_call>"#,
            "done",
        ]);

        let active = Arc::new(AtomicUsize::new(0));
        let max_active = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(DelayTool::new(
            "shell",
            50,
            Arc::clone(&active),
            Arc::clone(&max_active),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run shell"),
        ];
        let observer = NoopObserver;
        let excluded_tools = vec!["shell".to_string()];

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &excluded_tools,
        )
        .await
        .expect("tool loop should complete with blocked tool execution");

        assert_eq!(result, "done");
        assert_eq!(
            max_active.load(Ordering::SeqCst),
            0,
            "excluded tool must not execute even if the model requests it"
        );

        let tool_results_message = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("tool results message should be present");
        assert!(
            tool_results_message
                .content
                .contains("not available for this turn"),
            "blocked reason should be visible to the model"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_deduplicates_repeated_tool_calls() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>
<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>"#,
            "done",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("loop should finish after deduplicating repeated calls");

        assert_eq!(result, "done");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "duplicate tool call with same args should not execute twice"
        );

        let tool_results = history
            .iter()
            .find(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .expect("prompt-mode tool result payload should be present");
        assert!(tool_results.content.contains("counted:A"));
        assert!(tool_results.content.contains("Skipped duplicate tool call"));
    }

    #[tokio::test]
    async fn run_tool_call_loop_retries_after_semantic_duplicate_browser_open_call() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"{"content":"opening browser","tool_calls":[{"id":"call_1","name":"browser","arguments":"{\"action\":\"open\",\"url\":\"https://example.com\"}"}]}"#,
            r#"{"content":"opening browser again","tool_calls":[{"id":"call_2","name":"browser","arguments":"{\"url\":\"https://example.com\",\"backend\":\"rust_native\",\"command\":\"curl -s 'https://example.com'\"}"}]}"#,
            // A second consecutive duplicate-only round is required to reach
            // DUPLICATE_TOOL_CALL_STREAK_PER_NUDGE before a nudge is injected.
            r#"{"content":"opening browser yet again","tool_calls":[{"id":"call_3","name":"browser","arguments":"{\"url\":\"https://example.com\",\"backend\":\"rust_native\",\"command\":\"curl -s 'https://example.com'\"}"}]}"#,
            "done after prior browser result",
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "browser",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("open example.com"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("loop should recover from semantically duplicate browser-open calls");

        assert_eq!(result, "done after prior browser result");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "browser open should execute once when the second call is semantically identical"
        );
        assert!(
            history.iter().any(|msg| {
                msg.role == "user"
                    && DUPLICATE_TOOL_CALL_NUDGE_PROMPTS
                        .iter()
                        .any(|p| msg.content.contains(*p))
            }),
            "loop should inject a corrective nudge prompt after duplicate-only follow-up rounds"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_switches_native_mode_to_prompt_fallback_after_parse_issue() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"{"tool":"shell","command":"lsusb""#,
            r#"<tool_call>
{"name":"shell","arguments":{"command":"lsusb"}}
</tool_call>"#,
            "done after compatibility retry",
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CommandEchoTool::new(
            "shell",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run lsusb"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("loop should recover by switching to prompt tool fallback");

        assert_eq!(result, "done after compatibility retry");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            history[0].content.contains("## Compatibility Fallback"),
            "system prompt should switch into prompt tool fallback mode after parse failure"
        );
        assert!(
            history.iter().any(|msg| {
                msg.role == "user"
                    && (msg.content.contains("the tool call format was wrong")
                        || msg.content.starts_with(MISSING_TOOL_CALL_RETRY_PROMPT))
            }),
            "loop should inject corrective retry guidance after malformed tool payloads"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_shell_strip_policy_handles_repeated_redirect_calls() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo redirect-loop-ok 2>&1"}}
</tool_call>"#,
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo redirect-loop-ok 2>&1"}}
</tool_call>"#,
            r#"<tool_call>
{"name":"shell","arguments":{"command":"echo redirect-loop-ok 2>&1"}}
</tool_call>"#,
            "done after shell redirect retries",
        ]);

        let workspace = TempDir::new().expect("temp workspace");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: workspace.path().to_path_buf(),
            shell_redirect_policy: ShellRedirectPolicy::Strip,
            ..SecurityPolicy::default()
        });
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(crate::tools::ShellTool::new(
            Arc::clone(&security),
            Arc::new(NativeRuntime::new()),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run repeated shell redirects"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            6,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("loop should complete when strip policy normalizes redirects");

        assert_eq!(result, "done after shell redirect retries");

        let tool_result_messages: Vec<_> = history
            .iter()
            .filter(|msg| msg.role == "user" && msg.content.starts_with("[Tool results]"))
            .collect();
        assert_eq!(
            tool_result_messages.len(),
            3,
            "expected one tool result payload per scripted shell call"
        );
        for message in tool_result_messages {
            assert!(
                message.content.contains("<tool_result name=\"shell\">"),
                "tool results should include shell execution payloads"
            );
            assert!(
                !message
                    .content
                    .contains("Command not allowed by security policy"),
                "strip policy should avoid redirect-policy rejections"
            );
        }
    }

    #[tokio::test]
    async fn run_tool_call_loop_retries_when_response_claims_completion_without_tool_call() {
        let provider = ScriptedProvider::from_text_responses(vec![
            "Done — I've created the `names` folder in the current working directory.",
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"mkdir names"}}
</tool_call>"#,
            "done after verified tool execution",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("please create the names folder"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("completion claim without tool call should trigger a recovery retry");

        assert_eq!(result, "done after verified tool execution");
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            1,
            "recovery retry should enforce one real tool execution"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_errors_when_completion_claim_repeats_without_tool_call() {
        let provider = ScriptedProvider::from_text_responses(vec![
            "Done — I've created the `names` folder in the current working directory.",
            "Finished successfully. The folder and file are now created in workspace.",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("please create the names folder"),
        ];
        let observer = NoopObserver;

        let err = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect_err("repeated completion claims without tool call should hard-fail");

        let err_text = err.to_string();
        assert!(
            err_text.contains("deferred action without emitting a tool call"),
            "unexpected error text: {err_text}"
        );
        assert_eq!(
            invocations.load(Ordering::SeqCst),
            0,
            "tool should not execute when provider never emits a real tool call"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_allows_capability_answer_without_forcing_a_tool_call() {
        let answer = "Here are my capabilities: I can run shell commands, read and write files, search the web, query local RAG, and delegate work. I don't just describe capabilities — I execute them. What do you need done?";
        let provider = ScriptedProvider::from_text_responses(vec![answer]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("what capabilities do you have in this environment?"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("capability question should accept a truthful text answer");

        assert_eq!(result, answer);
        assert_eq!(invocations.load(Ordering::SeqCst), 0);
        assert!(
            !history.iter().any(|message| {
                message.role == "user" && message.content.starts_with("Internal correction:")
            }),
            "capability answer should not trigger a tool-followthrough retry"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_retries_when_model_claims_missing_file_tools() {
        let provider = ScriptedProvider::from_text_responses(vec![
            "I don't have access to a file creation tool in my current set of available functions.",
            r#"<tool_call>
{"name":"file_write","arguments":{"value":"retry"}}
</tool_call>"#,
            "done after file tool",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "file_write",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("create a test file"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("loop should retry once when model wrongly claims file tools are unavailable");

        assert_eq!(result, "done after file tool");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_accepts_completion_claim_after_real_tool_execution() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"delete"}}
</tool_call>"#,
            "Done. The file has been deleted from the workspace.",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("delete the file"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("completion text after a successful tool run should be accepted");

        assert!(result.contains("deleted from the workspace"));
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_accepts_json_wrapper_final_text_after_file_write() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"{"content":"writing file","tool_calls":[{"id":"call_1","name":"file_write","arguments":"{\"path\":\"/tmp/smoke.txt\",\"content\":\"tool smoke cloud\"}"}]}"#,
            r#"{"content":"File written successfully.","tool_calls":[]}"#,
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "file_write",
            "Written 16 bytes to /tmp/smoke.txt",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("write the file"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("json wrapper final text after real file_write should be accepted");

        assert_eq!(result, "File written successfully.");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            history
                .iter()
                .any(|msg| msg.role == "assistant" && msg.content == "File written successfully."),
            "assistant history should keep the unwrapped final text"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_falls_back_to_grounded_file_read_answer() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"file_read","arguments":{"path":"rust_kernel/src/main.rs"}}
</tool_call>"#,
            "The string `tool smoke qwen` doesn't appear to be a valid tool in my available toolset.",
            "The string `tool smoke qwen` doesn't appear to be a valid tool in my available toolset.",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "file_read",
            "1: tool smoke qwen",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("read the file"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("ungrounded post-file_read answer should fall back to a grounded answer");

        assert_eq!(
            result,
            "The file `rust_kernel/src/main.rs` contains:\n\n```\ntool smoke qwen\n```"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            history.iter().any(|msg| {
                msg.role == "user"
                    && msg
                        .content
                        .starts_with("Internal correction: use the verified tool results below")
            }),
            "loop should inject a grounding retry prompt before using the fallback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_falls_back_to_grounded_file_write_answer() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"file_write","arguments":{"path":"rust_kernel/src/main.rs","content":"tool smoke qwen"}}
</tool_call>"#,
            r#"Done. I wrote content "Hello" to rust_kernel/src/main.rs."#,
            r#"Done. I wrote content "Hello" to rust_kernel/src/main.rs."#,
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "file_write",
            "Written 15 bytes to rust_kernel/src/main.rs",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("write the file"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("invented file contents after file_write should fall back to grounded answer");

        assert_eq!(
            result,
            "The file `rust_kernel/src/main.rs` was written successfully with content:\n\n```\ntool smoke qwen\n```"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            history.iter().any(|msg| {
                msg.role == "user"
                    && msg
                        .content
                        .starts_with("Internal correction: use the verified tool results below")
            }),
            "loop should inject a grounding retry prompt before using the fallback"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_fast_exits_after_post_tool_followthrough_claim() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"file_write","arguments":{"path":"/llamafarm-data/workspace/tool_smoke_matrix.txt","content":"tool smoke llamafarm"}}
</tool_call>"#,
            "The file write operation has already been completed according to the verified tool results. Let me verify the content was written correctly by reading the file:",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "file_write",
            "Written 20 bytes to /llamafarm-data/workspace/tool_smoke_matrix.txt",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("write the file and confirm success"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("followthrough claim after file_write should short-circuit to grounded answer");

        assert_eq!(
            result,
            "The file `/llamafarm-data/workspace/tool_smoke_matrix.txt` was written successfully with content:\n\n```\ntool smoke llamafarm\n```"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_falls_back_to_grounded_file_read_after_meta_correction_text() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"file_read","arguments":{"path":"rust_kernel/src/main.rs"}}
</tool_call>"#,
            "I understand the correction. I'll use the available runtime tools as needed.\n\nWhat would you like me to help you with?",
            "I understand the correction. I'll use the available runtime tools as needed.\n\nWhat would you like me to help you with?",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "file_read",
            "1: tool smoke qwen",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("read the file"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("meta correction text after file_read should fall back to grounded content");

        assert_eq!(
            result,
            "The file `rust_kernel/src/main.rs` contains:\n\n```\ntool smoke qwen\n```"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_uses_grounded_fallback_after_post_tool_provider_error() {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let provider = ResultScriptedProvider::from_results(
            vec![
                Ok(ChatResponse {
                    text: Some(
                        r#"<tool_call>
{"name":"file_read","arguments":{"path":"rust_kernel/src/main.rs"}}
</tool_call>"#
                            .to_string(),
                    ),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
                Err(anyhow::anyhow!("upstream timeout after tool execution")),
            ],
            Arc::clone(&provider_calls),
        );

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "file_read",
            "1: tool smoke qwen",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("read the file"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("post-tool provider error should fall back to grounded tool output");

        assert_eq!(
            result,
            "The file `rust_kernel/src/main.rs` contains:\n\n```\ntool smoke qwen\n```"
        );
        assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn context_pressure_compacts_and_retries_without_dropping_native_tools_or_plan() {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let provider = ResultScriptedProvider::from_results(
            vec![
                Ok(ChatResponse {
                    text: None,
                    tool_calls: vec![ToolCall {
                        id: "call_before_pressure".to_string(),
                        name: "test_tool".to_string(),
                        arguments: "{}".to_string(),
                    }],
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
                Err(anyhow::anyhow!(
                    "context length exceeded: Ollama prompt reached the 262144-token context ceiling"
                )),
                Ok(ChatResponse {
                    text: Some(
                        "continued after context pressure with the verified result".to_string(),
                    ),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
            ],
            Arc::clone(&provider_calls),
        )
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "test_tool",
            "verified",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![ChatMessage::system("test-system")];
        for index in 0..30 {
            history.push(ChatMessage::user(format!("prior message {index}")));
        }
        history.push(ChatMessage::user("use test_tool and finish"));
        let observer = NoopObserver;

        let result = with_tool_loop_settings(
            false,
            true,
            run_tool_call_loop(
                &provider,
                &mut history,
                &tools_registry,
                &observer,
                "ollama",
                "qwen3.6:35b-a3b-mtp-q4_K_M",
                0.0,
                true,
                None,
                "cli",
                &crate::config::MultimodalConfig::default(),
                5,
                None,
                None,
                None,
                &[],
            ),
        )
        .await
        .expect("context pressure should compact and resume the existing native-tool plan");

        assert_eq!(
            result,
            "continued after context pressure with the verified result"
        );
        assert_eq!(provider_calls.load(Ordering::SeqCst), 3);
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert_eq!(provider.native_tool_requests(), vec![true, true, true]);
        assert!(
            history
                .iter()
                .all(|message| !message.content.contains("## Compatibility Fallback"))
        );
    }

    #[tokio::test]
    async fn zero_iteration_limit_runs_native_tool_loop_until_completion() {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let mut responses = Vec::new();
        for index in 0..4 {
            responses.push(Ok(ChatResponse {
                text: None,
                tool_calls: vec![ToolCall {
                    id: format!("call_{index}"),
                    name: "test_tool".to_string(),
                    arguments: format!(r#"{{"index":{index}}}"#),
                }],
                usage: None,
                metrics: None,
                reasoning_content: None,
            }));
        }
        responses.push(Ok(ChatResponse {
            text: Some("completed all four tool calls".to_string()),
            tool_calls: Vec::new(),
            usage: None,
            metrics: None,
            reasoning_content: None,
        }));
        let provider = ResultScriptedProvider::from_results(responses, Arc::clone(&provider_calls))
            .with_native_tool_support();
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "test_tool",
            "verified",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run every required tool call, then finish"),
        ];
        let observer = NoopObserver;

        let result = with_tool_loop_settings(
            false,
            true,
            run_tool_call_loop(
                &provider,
                &mut history,
                &tools_registry,
                &observer,
                "ollama",
                "test-model",
                0.0,
                true,
                None,
                "cli",
                &crate::config::MultimodalConfig::default(),
                0,
                None,
                None,
                None,
                &[],
            ),
        )
        .await
        .expect("zero must keep the tool loop running until completion");

        assert_eq!(result, "completed all four tool calls");
        assert_eq!(provider_calls.load(Ordering::SeqCst), 5);
        assert_eq!(invocations.load(Ordering::SeqCst), 4);
    }

    #[tokio::test]
    async fn run_tool_call_loop_falls_back_to_grounded_task_plan_summary() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"task_plan","arguments":{"action":"create","tasks":[{"title":"Write a file"},{"title":"Read the file"},{"title":"Delete the file"}]}}
</tool_call>"#,
            "I see that 3 tasks have been created. What would you like to do next?",
            "I see that 3 tasks have been created. What would you like to do next?",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "task_plan",
            "Created 3 task(s).",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("create the plan"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("task_plan follow-up question should fall back to a grounded plan summary");

        assert_eq!(
            result,
            "Task plan created with 3 steps:\n1. Write a file\n2. Read the file\n3. Delete the file"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_falls_back_to_grounded_task_plan_after_tool_result_leak() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"task_plan","arguments":{"action":"create","tasks":[{"title":"Write a file"},{"title":"Read the file"},{"title":"Delete the file"}]}}
</tool_call>"#,
            "I see that 3 tasks have been created. Let me check the details of what was planned:\n\n<tool_result name=\"task_plan\">\n</tool_result>\n\nCould you please provide more details about what you'd like me to plan?",
            "I see that 3 tasks have been created. Let me check the details of what was planned:\n\n<tool_result name=\"task_plan\">\n</tool_result>\n\nCould you please provide more details about what you'd like me to plan?",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "task_plan",
            "Created 3 task(s).",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("create the plan"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("task_plan tool_result leak should fall back to the grounded plan summary");

        assert_eq!(
            result,
            "Task plan created with 3 steps:\n1. Write a file\n2. Read the file\n3. Delete the file"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_retries_task_plan_followup_question_for_action_request() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"task_plan","arguments":{"action":"create","tasks":[{"title":"Execute the requested rust_kernel workflow"}]}}
</tool_call>"#,
            "The active task plan was created, but I don't have the plan_id needed to mark steps complete. Could you provide that plan_id so I can proceed with executing the plan?",
            "The active task plan was created, but I don't have the plan_id needed to mark steps complete. Could you provide that plan_id so I can proceed with executing the plan?",
        ]);

        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(
            crate::tools::task_plan::TaskPlanTool::new(Arc::new(SecurityPolicy::default())),
        )];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user(
                "Use task_plan first because this request is multi-step. Then complete the plan end-to-end using real tools: create a directory named rust_kernel in the workspace, run pwd and ls -d rust_kernel to verify it exists, delete the directory, and answer with the verification output plus whether cleanup succeeded. Do not stop after planning.",
            ),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect_err("task-plan follow-up question should not be accepted as final text");

        let err_text = result.to_string();
        assert!(
            err_text.contains("Model repeated intent text without a tool call after 1 retries"),
            "unexpected error: {err_text}"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_repairs_failed_task_plan_then_falls_back_to_grounded_summary() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"task_plan","arguments":{"action":"create"}}
</tool_call>"#,
            "I see the task_plan tool requires a 'tasks' parameter with a non-empty array of task objects. Let me create a simple task plan for you:\n\nWould you like me to:\n1. Use this example task plan, or\n2. Create a task plan based on a specific project or task you have in mind?",
            r#"<tool_call>
{"name":"task_plan","arguments":{"action":"create","tasks":[{"title":"Write a file"},{"title":"Read the file"},{"title":"Delete the file"}]}}
</tool_call>"#,
            "I see that 3 tasks have been created. What would you like me to do next?",
            "I see that 3 tasks have been created. What would you like me to do next?",
        ]);

        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(
            crate::tools::task_plan::TaskPlanTool::new(Arc::new(SecurityPolicy::default())),
        )];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("create a 3-step task plan: write a file, read it, delete it"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            7,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("failed task_plan should trigger a repair retry and grounded summary");

        assert_eq!(
            result,
            "Task plan created with 3 steps:\n1. Write a file\n2. Read the file\n3. Delete the file"
        );
        assert!(
            history.iter().any(|msg| {
                msg.role == "user"
                    && msg.content.starts_with(
                        "Internal correction: your last tool call for `task_plan` failed",
                    )
            }),
            "loop should inject a failed-tool repair retry prompt"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_falls_back_to_grounded_file_write_after_unrelated_pdf_error() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"file_write","arguments":{"path":"rust_kernel/src/main.rs","content":"tool smoke qwen"}}
</tool_call>"#,
            "The PDF read operation failed because the file path couldn't be resolved. Would you like me to search for PDF files first?",
            "The PDF read operation failed because the file path couldn't be resolved. Would you like me to search for PDF files first?",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "file_write",
            "Written 15 bytes to rust_kernel/src/main.rs",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("write the file"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("unrelated pdf error after file_write should fall back to grounded write result");

        assert_eq!(
            result,
            "The file `rust_kernel/src/main.rs` was written successfully with content:\n\n```\ntool smoke qwen\n```"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_falls_back_to_grounded_file_write_after_markdown_summary_mismatch()
    {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"file_write","arguments":{"path":"/llamafarm-data/workspace/tool_smoke_qwen_postpatch.txt","content":"tool smoke qwen postpatch"}}
</tool_call>"#,
            "The file has been successfully written!\n\n**Summary:**\n- **File:** `/llamafarm-data/workspace/tool_smoke_qwen_postpatch.txt`\n- **Content:** `Hello World`\n- **Size:** 25 bytes",
            "The file has been successfully written!\n\n**Summary:**\n- **File:** `/llamafarm-data/workspace/tool_smoke_qwen_postpatch.txt`\n- **Content:** `Hello World`\n- **Size:** 25 bytes",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(StaticOutputTool::new(
            "file_write",
            "Written 25 bytes to /llamafarm-data/workspace/tool_smoke_qwen_postpatch.txt",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("write the file"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("markdown summary mismatch after file_write should fall back to grounded content");

        assert_eq!(
            result,
            "The file `/llamafarm-data/workspace/tool_smoke_qwen_postpatch.txt` was written successfully with content:\n\n```\ntool smoke qwen postpatch\n```"
        );
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_fast_exits_after_verified_python_execution() {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let provider = ResultScriptedProvider::from_results(
            vec![
                Ok(ChatResponse {
                    text: Some(
                        r##"<tool_call>
{"name":"file_write","arguments":{"path":"/tmp/add_two_plus_three.py","content":"#!/usr/bin/env python3\nimport os\n\nresult = 2 + 3\nprint(f\"2 + 3 = {result}\")\nprint(\"File contents:\")\nprint(\"#!/usr/bin/env python3\")\nos.remove(__file__)\nprint(\"File deleted successfully.\")\n"}}
</tool_call>"##
                            .to_string(),
                    ),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
                Ok(ChatResponse {
                    text: Some(
                        r#"<tool_call>
{"name":"shell","arguments":{"command":"python3 /tmp/add_two_plus_three.py"}}
</tool_call>"#
                            .to_string(),
                    ),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
                Err(anyhow::anyhow!("provider should not be called after grounded fast exit")),
            ],
            Arc::clone(&provider_calls),
        );

        let file_write_invocations = Arc::new(AtomicUsize::new(0));
        let shell_invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(StaticOutputTool::new(
                "file_write",
                "Written 212 bytes to /tmp/add_two_plus_three.py",
                Arc::clone(&file_write_invocations),
            )),
            Box::new(StaticOutputTool::new(
                "shell",
                "2 + 3 = 5\nFile contents:\n#!/usr/bin/env python3\nFile deleted successfully.",
                Arc::clone(&shell_invocations),
            )),
        ];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user(
                "write a python file to add 2 + 3 print its output and the files contents then delete the file after execution",
            ),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("verified python execution should return immediately from tool results");

        assert!(result.contains(
            "The script `/tmp/add_two_plus_three.py` was created and executed successfully."
        ));
        assert!(result.contains("2 + 3 = 5"));
        assert!(result.contains("Script contents:"));
        assert!(result.contains("The file was deleted after execution."));
        assert_eq!(provider_calls.load(Ordering::SeqCst), 2);
        assert_eq!(file_write_invocations.load(Ordering::SeqCst), 1);
        assert_eq!(shell_invocations.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn run_tool_call_loop_repairs_failed_shell_placeholder_path() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"shell","arguments":{"command":"python3 /path/to/script.py"}}
</tool_call>"#,
            "The file path doesn't exist. Would you like me to create it first?",
            r#"<tool_call>
{"name":"file_write","arguments":{"path":"add_two_plus_three.py","content":"print(2 + 3)"}}
</tool_call>"#,
            "done after creating the file",
        ]);

        let shell_invocations = Arc::new(AtomicUsize::new(0));
        let file_write_invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(StaticOutputTool::failing(
                "shell",
                "python3: can't open file '/path/to/script.py': [Errno 2] No such file or directory",
                Arc::clone(&shell_invocations),
            )),
            Box::new(StaticOutputTool::new(
                "file_write",
                "Written 12 bytes to add_two_plus_three.py",
                Arc::clone(&file_write_invocations),
            )),
        ];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run the python script"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            6,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("failed placeholder-path shell call should trigger a repair retry");

        assert_eq!(result, "done after creating the file");
        assert_eq!(shell_invocations.load(Ordering::SeqCst), 1);
        assert_eq!(file_write_invocations.load(Ordering::SeqCst), 1);
        assert!(
            history.iter().any(|msg| {
                msg.role == "user"
                    && msg
                        .content
                        .starts_with("Internal correction: your last tool call for `shell` failed")
                    && msg
                        .content
                        .contains("Do not use placeholder paths like `/path/to/script.py`")
            }),
            "loop should inject shell-specific repair guidance after placeholder-path failures"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_breaks_repeated_file_write_stalls() {
        let provider_calls = Arc::new(AtomicUsize::new(0));
        let provider = ResultScriptedProvider::from_results(
            vec![
                Ok(ChatResponse {
                    text: Some(
                        r#"<tool_call>
{"name":"file_write","arguments":{"path":"fib.py","content":"print(1)"}}
</tool_call>"#
                            .to_string(),
                    ),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
                Ok(ChatResponse {
                    text: Some(
                        r#"<tool_call>
{"name":"file_write","arguments":{"path":"fib.py","content":"print(1)\nprint(1)"}}
</tool_call>"#
                            .to_string(),
                    ),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
                Ok(ChatResponse {
                    text: Some(
                        r#"<tool_call>
{"name":"file_write","arguments":{"path":"fib.py","content":"print(1)\nprint(1)\nprint(2)"}}
</tool_call>"#
                            .to_string(),
                    ),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
                Ok(ChatResponse {
                    text: Some(
                        r#"<tool_call>
{"name":"file_read","arguments":{"path":"fib.py"}}
</tool_call>"#
                            .to_string(),
                    ),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
                Ok(ChatResponse {
                    text: Some("done after inspection".to_string()),
                    tool_calls: Vec::new(),
                    usage: None,
                    metrics: None,
                    reasoning_content: None,
                }),
            ],
            Arc::clone(&provider_calls),
        );

        let file_write_invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(StaticOutputTool::new(
                "file_write",
                "Written bytes to fib.py",
                Arc::clone(&file_write_invocations),
            )),
            Box::new(StaticOutputTool::new(
                "file_read",
                "1: print(1)\n2: print(1)\n3: print(2)",
                Arc::new(AtomicUsize::new(0)),
            )),
        ];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("write a python fibonacci script to fib.py, inspect it, and answer"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            6,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("repeated file_write churn should trigger a stop prompt and advance to file_read");

        assert_eq!(result, "done after inspection");
        assert_eq!(provider_calls.load(Ordering::SeqCst), 5);
        assert_eq!(file_write_invocations.load(Ordering::SeqCst), 3);
        assert!(
            history.iter().any(|msg| {
                msg.role == "user"
                    && msg
                        .content
                        .contains("The file `fib.py` has already been written 3 times")
                    && msg.content.contains("Do NOT call `file_write` again")
            }),
            "loop should inject a repeated file_write stall-breaker prompt"
        );
    }

    #[test]
    fn detect_repeated_file_write_stall_counts_same_path_churn() {
        let records = vec![
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({"path":"fib.py","content":"print(1)"}),
                output: "Written".into(),
            },
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({"path":"fib.py","content":"print(1)\nprint(1)"}),
                output: "Written".into(),
            },
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({"path":"fib.py","content":"print(1)\nprint(1)\nprint(2)"}),
                output: "Written".into(),
            },
        ];

        assert_eq!(
            detect_repeated_file_write_stall(&records),
            Some(("fib.py".to_string(), 3))
        );
    }

    #[test]
    fn detect_repeated_file_write_stall_ignores_executed_or_unrelated_paths() {
        let executed_records = vec![
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({"path":"fib.py","content":"print(1)"}),
                output: "Written".into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({"command":"python3 fib.py"}),
                output: "1".into(),
            },
        ];
        assert_eq!(detect_repeated_file_write_stall(&executed_records), None);

        let mixed_path_records = vec![
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({"path":"fib.py","content":"print(1)"}),
                output: "Written".into(),
            },
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({"path":"other.py","content":"print(2)"}),
                output: "Written".into(),
            },
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({"path":"other.py","content":"print(3)"}),
                output: "Written".into(),
            },
        ];
        assert_eq!(detect_repeated_file_write_stall(&mixed_path_records), None);
    }

    #[test]
    fn failed_shell_followthrough_detects_missing_command_prompt() {
        let records = vec![FailedToolRecord {
            name: "shell".to_string(),
            output: "Error executing shell: Missing 'command' parameter".to_string(),
        }];

        assert!(looks_like_failed_tool_followthrough(
            "I need a command to execute.",
            &records
        ));
    }

    #[test]
    fn failed_task_plan_execution_started_prompt_discourages_replanning() {
        let records = vec![FailedToolRecord {
            name: "task_plan".to_string(),
            output: "Skipped task_plan call: execution has already started this turn. Use the existing plan and continue executing.".to_string(),
        }];

        let prompt = build_failed_tool_retry_prompt(&records);
        assert!(prompt.contains("Do NOT call `task_plan` again right now"));
        assert!(prompt.contains("Continue directly with the next incomplete step"));
    }

    #[tokio::test]
    async fn run_tool_call_loop_allows_text_only_planning_without_tool_call() {
        let provider = ScriptedProvider::from_text_responses(vec![
            "We were previously discussing gmail integration. Goal 1 is done. Our next task is Goal 2 — Gmail API via OAuth. Here is the implementation plan before any tool actions.",
        ]);

        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::new(AtomicUsize::new(0)),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("we finished goal one, what is next"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("planning-only text should be returned without forced tool-call rejection");

        assert!(result.contains("implementation plan"));
    }

    #[tokio::test]
    async fn run_tool_call_loop_auto_compacts_history_before_provider_request() {
        let provider = RecordingSummarizingProvider::from_text_responses(vec!["done"]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("old 1"),
            ChatMessage::assistant("old 2"),
            ChatMessage::user("old 3"),
            ChatMessage::assistant("recent 1"),
            ChatMessage::user("recent 2"),
        ];
        let observer = NoopObserver;

        let result = with_tool_loop_history_limit(
            2,
            run_tool_call_loop(
                &provider,
                &mut history,
                &tools_registry,
                &observer,
                "mock-provider",
                "mock-model",
                0.0,
                true,
                None,
                "webchat",
                &crate::config::MultimodalConfig::default(),
                2,
                None,
                None,
                None,
                &[],
            ),
        )
        .await
        .expect("tool loop should return final response");

        assert_eq!(result, "done");
        // The tool loop compacts history deterministically (see
        // `deterministic_compact_history`), never through a provider call —
        // that model round trip was the source of the multi-second stalls on
        // every plan-item boundary this test now guards against.
        assert_eq!(provider.summary_calls(), 0);

        let recorded_requests = provider.recorded_requests();
        assert_eq!(recorded_requests.len(), 1);
        let first_request = &recorded_requests[0];

        assert!(
            first_request
                .iter()
                .any(|msg| msg.content.contains("[Compaction summary]")),
            "provider request should include compaction summary"
        );
        assert!(
            !first_request
                .iter()
                .any(|msg| msg.content.contains("old 1")),
            "oldest history should be compacted out of the provider request"
        );
        assert!(
            !first_request
                .iter()
                .any(|msg| msg.content.contains("old 2")),
            "oldest history should be compacted out of the provider request"
        );
        assert!(
            !first_request
                .iter()
                .any(|msg| msg.content.contains("old 3")),
            "oldest history should be compacted out of the provider request"
        );
        assert!(
            first_request
                .iter()
                .any(|msg| msg.content.contains("recent 1")),
            "recent context should be preserved"
        );
        assert!(
            first_request
                .iter()
                .any(|msg| msg.content.contains("recent 2")),
            "recent context should be preserved"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_requires_task_plan_before_multi_step_execution_and_continues() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"<tool_call>{"name":"count_tool","arguments":{"value":"premature"}}</tool_call>"#,
            r#"<tool_call>{"name":"task_plan","arguments":{"action":"create","tasks":[{"title":"Write a file"},{"title":"Run it"},{"title":"Delete it"}]}}</tool_call>"#,
            r#"<tool_call>{"name":"count_tool","arguments":{"value":"executed"}}</tool_call>"#,
            "All steps completed.",
        ]);

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(CountingTool::new("count_tool", Arc::clone(&invocations))),
            Box::new(crate::tools::task_plan::TaskPlanTool::new(Arc::new(
                SecurityPolicy::default(),
            ))),
        ];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("write a file, run it, print the result, then delete it"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            8,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("multi-step request should recover through task_plan and continue executing");

        assert_eq!(result, "All steps completed.");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            history.iter().any(|message| {
                message.role == "user"
                    && message.content.starts_with(
                        "Internal correction: this request contains multiple actionable steps.",
                    )
            }),
            "loop should inject an auto-plan retry before executing non-plan tools"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_injects_working_state_after_task_plan_creation() {
        let provider = RecordingScriptedProvider::from_text_responses(vec![
            r#"<tool_call>{"name":"task_plan","arguments":{"action":"create","tasks":[{"title":"Inspect rust_kernel"},{"title":"Create plan"},{"title":"Execute plan"}]}}</tool_call>"#,
            r#"<tool_call>{"name":"count_tool","arguments":{"value":"executed"}}</tool_call>"#,
            "Execution started.",
        ]);

        let tools_registry: Vec<Box<dyn Tool>> = vec![
            Box::new(crate::tools::task_plan::TaskPlanTool::new(Arc::new(
                SecurityPolicy::default(),
            ))),
            Box::new(CountingTool::new(
                "count_tool",
                Arc::new(AtomicUsize::new(0)),
            )),
        ];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("inspect rust_kernel, create a plan, then execute it"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            6,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("working-state injection flow should complete");

        assert_eq!(result, "Execution started.");
        let recorded_requests = provider.recorded_requests();
        assert!(recorded_requests.len() >= 2);
        let second_request = &recorded_requests[1];
        let working_state = second_request
            .iter()
            .find(|message| {
                message.role == "user" && message.content.starts_with("Internal working state:")
            })
            .expect("second request should include an internal working state message");

        assert!(working_state.content.contains("Current user task"));
        assert!(working_state
            .content
            .contains("[1] [pending] Inspect rust_kernel"));
        assert!(working_state
            .content
            .contains("Next incomplete step: [1] Inspect rust_kernel"));
    }

    #[tokio::test]
    async fn run_tool_call_loop_native_mode_preserves_fallback_tool_call_ids() {
        let provider = ScriptedProvider::from_text_responses(vec![
            r#"{"content":"Need to call tool","tool_calls":[{"id":"call_abc","name":"count_tool","arguments":"{\"value\":\"X\"}"}]}"#,
            "done",
        ])
        .with_native_tool_support();

        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];

        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "cli",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            None,
            None,
            &[],
        )
        .await
        .expect("native fallback id flow should complete");

        assert_eq!(result, "done");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert!(
            history.iter().any(|msg| {
                msg.role == "tool" && msg.content.contains("\"tool_call_id\":\"call_abc\"")
            }),
            "tool result should preserve parsed fallback tool_call_id in native mode"
        );
        assert!(
            history
                .iter()
                .all(|msg| !(msg.role == "user" && msg.content.starts_with("[Tool results]"))),
            "native mode should use role=tool history instead of prompt fallback wrapper"
        );
    }

    #[tokio::test]
    async fn run_tool_call_loop_consumes_provider_stream_for_final_response() {
        let provider =
            StreamingScriptedProvider::from_text_responses(vec!["streamed final answer"]);
        let tools_registry: Vec<Box<dyn Tool>> = Vec::new();
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("say hi"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(32);

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            &crate::config::MultimodalConfig::default(),
            4,
            None,
            Some(tx),
            None,
            &[],
        )
        .await
        .expect("streaming provider should complete");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if delta == DRAFT_CLEAR_SENTINEL || delta.starts_with(DRAFT_PROGRESS_SENTINEL) {
                continue;
            }
            visible_deltas.push_str(&delta);
        }

        assert_eq!(result, "streamed final answer");
        assert_eq!(
            visible_deltas, "streamed final answer",
            "draft should receive upstream deltas once without post-hoc duplication"
        );
        assert_eq!(provider.stream_calls.load(Ordering::SeqCst), 1);
        assert_eq!(provider.chat_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn run_tool_call_loop_streaming_path_preserves_tool_loop_semantics() {
        let provider = StreamingScriptedProvider::from_text_responses(vec![
            r#"<tool_call>
{"name":"count_tool","arguments":{"value":"A"}}
</tool_call>"#,
            "done",
        ]);
        let invocations = Arc::new(AtomicUsize::new(0));
        let tools_registry: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "count_tool",
            Arc::clone(&invocations),
        ))];
        let mut history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("run tool calls"),
        ];
        let observer = NoopObserver;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(64);

        let result = run_tool_call_loop(
            &provider,
            &mut history,
            &tools_registry,
            &observer,
            "mock-provider",
            "mock-model",
            0.0,
            true,
            None,
            "telegram",
            &crate::config::MultimodalConfig::default(),
            5,
            None,
            Some(tx),
            None,
            &[],
        )
        .await
        .expect("streaming tool loop should execute tool and finish");

        let mut visible_deltas = String::new();
        while let Some(delta) = rx.recv().await {
            if delta == DRAFT_CLEAR_SENTINEL || delta.starts_with(DRAFT_PROGRESS_SENTINEL) {
                continue;
            }
            visible_deltas.push_str(&delta);
        }

        assert_eq!(result, "done");
        assert_eq!(invocations.load(Ordering::SeqCst), 1);
        assert_eq!(provider.stream_calls.load(Ordering::SeqCst), 2);
        assert_eq!(provider.chat_calls.load(Ordering::SeqCst), 0);
        assert_eq!(visible_deltas, "done");
        assert!(
            !visible_deltas.contains("<tool_call"),
            "draft text should not leak streamed tool payload markers"
        );
    }

    #[test]
    fn looks_like_unverified_action_completion_without_tool_call_detects_claimed_side_effects() {
        assert!(looks_like_unverified_action_completion_without_tool_call(
            "Done — I've created the `names` folder in the current working directory."
        ));
        assert!(looks_like_unverified_action_completion_without_tool_call(
            "Finished successfully: I wrote the file to the workspace path."
        ));
    }

    #[test]
    fn looks_like_unverified_action_completion_without_tool_call_ignores_non_side_effect_text() {
        assert!(!looks_like_unverified_action_completion_without_tool_call(
            "Done. Here is the explanation of why that approach works."
        ));
        assert!(!looks_like_unverified_action_completion_without_tool_call(
            "I have a suggestion for the plan if you want me to proceed."
        ));
        assert!(!looks_like_unverified_action_completion_without_tool_call(
            "We were previously discussing gmail integration. Goal 1 is done. Our next task is Goal 2 — Gmail API via OAuth."
        ));
    }

    #[test]
    fn informational_agent_requests_are_not_treated_as_action_tasks() {
        assert!(is_informational_agent_request(
            "what capabilities do you have in this environment?"
        ));
        assert!(is_informational_agent_request("are you working?"));
        assert!(!is_informational_agent_request(
            "test all your capabilities and write the results to a file"
        ));
        assert!(!is_informational_agent_request(
            "please create a capability report in the workspace"
        ));
    }

    #[test]
    fn runtime_tool_notice_distinguishes_registered_from_verified() {
        let tools: Vec<Box<dyn Tool>> = Vec::new();
        let notice = build_runtime_tool_availability_notice(&tools);

        assert!(notice.contains("registered for this runtime"));
        assert!(notice.contains("does not prove"));
        assert!(notice.contains("concrete successful tool result from this run"));
    }

    #[test]
    fn runtime_tool_notice_lists_the_entire_selected_set() {
        let specs = (0..45)
            .map(|index| crate::tools::ToolSpec {
                name: format!("selected_tool_{index}"),
                description: "Selected fixture".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            })
            .collect::<Vec<_>>();
        let notice = build_runtime_tool_availability_notice_from_specs(&specs);
        assert!(notice.contains("selected_tool_0"));
        assert!(notice.contains("selected_tool_44"));
    }

    #[test]
    fn managed_app_notice_separates_reserved_ports_and_host_docker() {
        let notice = build_managed_app_runtime_notice_from_values(
            "5000",
            "8501-8599",
            "192.168.1.154,100.107.226.49",
            true,
        );

        assert!(notice.contains("Ports 5000 are reserved"));
        assert!(notice.contains("first free port in 8501-8599"));
        assert!(notice.contains("http://192.168.1.154:<port>"));
        assert!(notice.contains("http://100.107.226.49:<port>"));
        assert!(notice.contains("controls the host Docker daemon"));
        assert!(notice.contains("external updater or helper"));
    }

    #[test]
    fn auto_plan_instructions_require_bounded_evidence_for_capability_audits() {
        let instructions = build_auto_plan_execute_instructions();

        assert!(instructions.contains("real executable integration audit"));
        assert!(instructions.contains("one bounded probe per applicable tool"));
        assert!(instructions.contains("verified (a successful result in this run)"));
        assert!(instructions.contains("failed or blocked capability"));
    }

    #[test]
    fn looks_like_tool_unavailability_claim_detects_false_missing_tool_replies() {
        let tools = vec![
            crate::tools::ToolSpec {
                name: "file_write".to_string(),
                description: "Write file".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            },
            crate::tools::ToolSpec {
                name: "file_edit".to_string(),
                description: "Edit file".to_string(),
                parameters: serde_json::json!({"type":"object"}),
            },
        ];

        assert!(looks_like_tool_unavailability_claim(
            "I don't have access to a file creation tool in my current set of available functions.",
            &tools
        ));
        assert!(!looks_like_tool_unavailability_claim(
            "I can create that file now.",
            &tools
        ));
    }

    #[test]
    fn looks_like_tool_unavailability_claim_detects_false_missing_shell_replies() {
        let shell_tools = vec![crate::tools::ToolSpec {
            name: "shell".to_string(),
            description: "Run shell".to_string(),
            parameters: serde_json::json!({"type":"object"}),
        }];
        let browser_tools = vec![crate::tools::ToolSpec {
            name: "browser".to_string(),
            description: "Use browser".to_string(),
            parameters: serde_json::json!({"type":"object"}),
        }];

        assert!(looks_like_tool_unavailability_claim(
            "I can't directly run `lsusb` for you, but I can explain what it does.",
            &shell_tools
        ));
        assert!(looks_like_tool_unavailability_claim(
            "I cannot execute terminal commands from here.",
            &shell_tools
        ));
        assert!(!looks_like_tool_unavailability_claim(
            "I can't directly run `lsusb` for you, but I can explain what it does.",
            &browser_tools
        ));
    }

    #[test]
    fn parse_tool_calls_extracts_single_call() {
        let response = r#"Let me check that.
<tool_call>
{"name": "shell", "arguments": {"command": "ls -la"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Let me check that.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_extracts_multiple_calls() {
        let response = r#"<tool_call>
{"name": "file_read", "arguments": {"path": "a.txt"}}
</tool_call>
<tool_call>
{"name": "file_read", "arguments": {"path": "b.txt"}}
</tool_call>"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[1].name, "file_read");
    }

    #[test]
    fn parse_tool_calls_recovers_top_level_pseudo_tool_json() {
        let response = r#"{"tool":"shell","command":"lsusb"}"#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "lsusb");
    }

    #[test]
    fn parse_tool_calls_recovers_single_key_task_plan_json() {
        let response = r#"{"task_plan":{"steps":[{"description":"Write a file"},{"description":"Read the file"},{"description":"Delete the file"}]}}"#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "task_plan");
        assert_eq!(calls[0].arguments["action"], "create");
        assert_eq!(calls[0].arguments["tasks"][0]["title"], "Write a file");
        assert_eq!(calls[0].arguments["tasks"][1]["title"], "Read the file");
        assert_eq!(calls[0].arguments["tasks"][2]["title"], "Delete the file");
    }

    #[test]
    fn parse_tool_calls_normalizes_task_plan_tasks_with_descriptions() {
        let response = r#"{"task_plan":{"action":"create","tasks":[{"step":1,"action":"file_write","description":"Create Python file that prints 2 + 2","target":"/llamafarm-data/workspace/smoke_test.py"},{"step":2,"action":"shell","description":"Run the Python file with python3","command":"python3 /llamafarm-data/workspace/smoke_test.py"},{"step":3,"action":"shell","description":"Delete the Python file","command":"rm /llamafarm-data/workspace/smoke_test.py"}]}}"#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "task_plan");
        assert_eq!(calls[0].arguments["action"], "create");
        assert_eq!(
            calls[0].arguments["tasks"][0]["title"],
            "Create Python file that prints 2 + 2"
        );
        assert_eq!(
            calls[0].arguments["tasks"][1]["title"],
            "Run the Python file with python3"
        );
        assert_eq!(
            calls[0].arguments["tasks"][2]["title"],
            "Delete the Python file"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_fenced_single_key_task_plan_json() {
        let response = r#"```json
{"task_plan":{"steps":[{"description":"Write a file"},{"description":"Read the file"},{"description":"Delete the file"}]}}
```"#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "task_plan");
        assert_eq!(calls[0].arguments["action"], "create");
        assert_eq!(calls[0].arguments["tasks"][0]["title"], "Write a file");
        assert_eq!(calls[0].arguments["tasks"][1]["title"], "Read the file");
        assert_eq!(calls[0].arguments["tasks"][2]["title"], "Delete the file");
    }

    #[test]
    fn parse_tool_calls_maps_list_dir_alias_to_glob_search() {
        let response = r#"{"tool":"list_dir","path":"src"}"#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "glob_search");
        assert_eq!(calls[0].arguments["pattern"], "src/**/*");
    }

    #[test]
    fn parse_tool_calls_maps_web_search_alias_to_real_tool() {
        let response = r#"{"tool":"web_search","query":"rust traits"}"#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search_tool");
        assert_eq!(calls[0].arguments["query"], "rust traits");
    }

    #[test]
    fn parse_tool_calls_recovers_function_style_shell_call() {
        let response = "I'll run it now.\nshell(\"lsusb\")";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "lsusb");
        assert!(text.contains("I'll run it now."));
    }

    #[test]
    fn parse_tool_calls_recovers_function_style_shell_json_args() {
        let response = r#"I'll run it now.
shell({"hint":"lsblk"})"#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["hint"], "lsblk");
        assert!(text.contains("I'll run it now."));
    }

    #[test]
    fn parse_tool_calls_recovers_function_style_single_quoted_args() {
        let response = "shell(command='lsusb')";
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "lsusb");
    }

    #[test]
    fn parse_tool_calls_recovers_json_wrapped_function_style_shell_call() {
        let response = "json{shell(lsusb)}";
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "lsusb");
    }

    #[test]
    fn parse_tool_calls_recovers_shell_block_after_action_cue() {
        let response = "Running now:\n```bash\nlsusb\n```";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "lsusb");
        assert!(text.contains("Running now:"));
    }

    #[test]
    fn parse_tool_calls_recovers_apostrophe_shell_block_after_action_cue() {
        let response = "Running now:\n'''bash\nlsusb\n'''";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "lsusb");
        assert!(text.contains("Running now:"));
    }

    #[test]
    fn parse_tool_calls_recovers_unlabeled_shell_block_after_action_cue() {
        let response = "Running now:\n```\nlsblk\n```";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "lsblk");
        assert!(text.contains("Running now:"));
    }

    #[test]
    fn parse_tool_calls_recovers_explicit_tool_block() {
        let response = "tool: shell\ncommand: lsusb";
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "lsusb");
    }

    #[test]
    fn parse_tool_calls_recovers_prose_file_write_with_code_block() {
        let response = r#"I'll create a Python file that adds two numbers. Here's the code:

```python
def add_numbers(a, b):
    return a + b
```

I'll write this to a file called `add_numbers.py`:"#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(calls[0].arguments["path"], "add_numbers.py");
        assert_eq!(
            calls[0].arguments["content"],
            "def add_numbers(a, b):\n    return a + b"
        );
        assert!(text.contains("I'll create a Python file"));
    }

    #[test]
    fn parse_tool_calls_recovers_tool_name_plus_json_block() {
        let response = "shell\n{\"hint\":\"lsblk\"}";
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["hint"], "lsblk");
    }

    #[test]
    fn parse_tool_calls_recovers_plain_shell_command() {
        let response = "lsusb";
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "lsusb");
    }

    #[test]
    fn parse_tool_calls_recovers_plain_build_command() {
        let response = "cargo test";
        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "cargo test");
    }

    #[test]
    fn parse_tool_calls_returns_text_only_when_no_calls() {
        let response = "Just a normal response with no tools.";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Just a normal response with no tools.");
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_malformed_json() {
        let response = r#"<tool_call>
not valid json
</tool_call>
Some text after."#;

        let (text, calls) = parse_tool_calls(response);
        assert!(calls.is_empty());
        assert!(text.contains("Some text after."));
    }

    #[test]
    fn parse_tool_calls_text_before_and_after() {
        let response = r#"Before text.
<tool_call>
{"name": "shell", "arguments": {"command": "echo hi"}}
</tool_call>
After text."#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Before text."));
        assert!(text.contains("After text."));
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn parse_tool_calls_handles_openai_format() {
        // OpenAI-style response with tool_calls array
        let response = r#"{"content": "Let me check that for you.", "tool_calls": [{"type": "function", "function": {"name": "shell", "arguments": "{\"command\": \"ls -la\"}"}}]}"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Let me check that for you.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_handles_openai_format_multiple_calls() {
        let response = r#"{"tool_calls": [{"type": "function", "function": {"name": "file_read", "arguments": "{\"path\": \"a.txt\"}"}}, {"type": "function", "function": {"name": "file_read", "arguments": "{\"path\": \"b.txt\"}"}}]}"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[1].name, "file_read");
    }

    #[test]
    fn parse_tool_calls_openai_format_without_content() {
        // Some providers don't include content field with tool_calls
        let response = r#"{"tool_calls": [{"type": "function", "function": {"name": "memory_recall", "arguments": "{}"}}]}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty()); // No content field
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
    }

    #[test]
    fn parse_tool_calls_handles_openai_message_wrapper_with_content() {
        let response = r#"{
            "message": {
                "role": "assistant",
                "content": "<think>plan</think>\nI will call a tool.",
                "tool_calls": [
                    {
                        "id": "chatcmpl-tool-a18c01b8849eb05d",
                        "type": "function",
                        "function": {
                            "name": "shell",
                            "arguments": "{\"command\": \"ls -la\"}"
                        }
                    }
                ]
            },
            "finish_reason": "tool_calls"
        }"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
        assert!(text.contains("I will call a tool."));
    }

    #[test]
    fn parse_tool_calls_handles_openai_choices_message_wrapper() {
        let response = r#"{
            "id": "chatcmpl-123",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "Checking now.",
                        "tool_calls": [
                            {
                                "id": "call_1",
                                "type": "function",
                                "function": {
                                    "name": "shell",
                                    "arguments": "{\"command\":\"pwd\"}"
                                }
                            }
                        ]
                    },
                    "finish_reason": "tool_calls"
                }
            ]
        }"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Checking now.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
        assert_eq!(calls[0].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn parse_tool_calls_preserves_openai_tool_call_ids() {
        let response = r#"{"tool_calls":[{"id":"call_42","function":{"name":"shell","arguments":"{\"command\":\"pwd\"}"}}]}"#;
        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool_call_id.as_deref(), Some("call_42"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_json_inside_tool_call_tag() {
        let response = r#"<tool_call>
```json
{"name": "file_write", "arguments": {"path": "test.py", "content": "print('ok')"}}
```
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "test.py"
        );
    }

    #[test]
    fn parse_tool_calls_handles_noisy_tool_call_tag_body() {
        let response = r#"<tool_call>
I will now call the tool with this payload:
{"name": "shell", "arguments": {"command": "pwd"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_call_inline_attributes_with_send_message_alias() {
        let response = r#"<tool_call>send_message channel="user_channel" message="Hello! How can I assist you today?"</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "message_send");
        assert_eq!(
            calls[0].arguments.get("channel").unwrap().as_str().unwrap(),
            "user_channel"
        );
        assert_eq!(
            calls[0].arguments.get("message").unwrap().as_str().unwrap(),
            "Hello! How can I assist you today?"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_call_function_style_arguments() {
        let response = r#"<tool_call>message_send(channel="general", message="test")</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "message_send");
        assert_eq!(
            calls[0].arguments.get("channel").unwrap().as_str().unwrap(),
            "general"
        );
        assert_eq!(
            calls[0].arguments.get("message").unwrap().as_str().unwrap(),
            "test"
        );
    }

    #[test]
    fn parse_tool_calls_handles_xml_nested_tool_payload() {
        let response = r#"<tool_call>
<memory_recall>
<query>project roadmap</query>
</memory_recall>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
        assert_eq!(
            calls[0].arguments.get("query").unwrap().as_str().unwrap(),
            "project roadmap"
        );
    }

    #[test]
    fn parse_tool_calls_ignores_xml_thinking_wrapper() {
        let response = r#"<tool_call>
<thinking>Need to inspect memory first</thinking>
<memory_recall>
<query>recent deploy notes</query>
</memory_recall>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "memory_recall");
        assert_eq!(
            calls[0].arguments.get("query").unwrap().as_str().unwrap(),
            "recent deploy notes"
        );
    }

    #[test]
    fn parse_tool_calls_handles_xml_with_json_arguments() {
        let response = r#"<tool_call>
<shell>{"command":"pwd"}</shell>
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
    }

    #[test]
    fn parse_tool_calls_handles_direct_file_write_xml_tag_body() {
        let response = r#"I'll write the file now.
<file_write>
path="smoke_js/add_two.mjs"
content="console.log(2 + 3);"
</file_write>"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "I'll write the file now.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "smoke_js/add_two.mjs"
        );
        assert_eq!(
            calls[0].arguments.get("content").unwrap().as_str().unwrap(),
            "console.log(2 + 3);"
        );
    }

    #[test]
    fn parse_tool_calls_handles_direct_file_write_self_closing_xml_tag() {
        let response = r#"I'll use the file_write tool.
<file_write path="/llamafarm-data/workspace/tool_smoke_matrix.txt" content="tool smoke llamafarm"/>"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "I'll use the file_write tool.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "/llamafarm-data/workspace/tool_smoke_matrix.txt"
        );
        assert_eq!(
            calls[0].arguments.get("content").unwrap().as_str().unwrap(),
            "tool smoke llamafarm"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_multiple_shell_blocks_after_create_cue() {
        let response = r#"I'll create a simple Node.js script that adds 2 + 3 and runs it for you.

```bash
mkdir -p smoke_js
cat > smoke_js/add_two.mjs << 'EOF'
console.log(2 + 3);
EOF
```

Now let me run the script:

```bash
node smoke_js/add_two.mjs
```"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I'll create a simple Node.js script"));
        assert!(text.contains("Now let me run the script"));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "shell");
        assert!(calls[0]
            .arguments
            .get("command")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("mkdir -p smoke_js"));
        assert_eq!(calls[1].name, "shell");
        assert_eq!(
            calls[1].arguments.get("command").unwrap().as_str().unwrap(),
            "node smoke_js/add_two.mjs"
        );
    }

    #[test]
    fn parse_tool_calls_preserves_heredoc_body_indentation_in_shell_blocks() {
        let response = r#"I'll create and run the Python file now.

```bash
cat > smoke_py/add_two.py << 'EOF'
def add(a, b):
    return a + b

print(add(2, 3))
EOF
python3 smoke_py/add_two.py
```"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I'll create and run the Python file now."));
        let command = calls
            .iter()
            .find_map(|call| {
                call.arguments
                    .get("command")
                    .and_then(|value| value.as_str())
                    .filter(|command| command.contains("cat > smoke_py/add_two.py << 'EOF'"))
            })
            .expect("expected recovered shell command containing quoted heredoc");
        assert!(command.contains("cat > smoke_py/add_two.py << 'EOF'"));
        assert!(command.contains("def add(a, b):"));
        assert!(
            command.contains("    return a + b"),
            "expected preserved indentation in command: {command}"
        );
        assert!(command.contains("\n\nprint(add(2, 3))"));
        assert!(command.contains("\nEOF\npython3 smoke_py/add_two.py"));
    }

    #[test]
    fn parse_tool_calls_recovers_function_style_write_plus_shell_follow_up() {
        let response = r#"I'll create the Python file for you and run it to show the output.

```python
file_write(path="smoke_py/add_two.py", content='def add(a, b):\n    return a + b\n\nprint(add(2, 3))\n')
```

Now let me run it with python3:

```bash
python3 smoke_py/add_two.py
```"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I'll create the Python file for you"));
        assert!(text.contains("Now let me run it with python3"));
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "smoke_py/add_two.py"
        );
        assert_eq!(calls[1].name, "shell");
        assert_eq!(
            calls[1].arguments.get("command").unwrap().as_str().unwrap(),
            "python3 smoke_py/add_two.py"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_shell_block_after_ready_to_execute_cue() {
        let response = r#"The file is now ready to be executed with Node.js:

```bash
node smoke_js/add_two.mjs
```"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("ready to be executed with Node.js"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "node smoke_js/add_two.mjs"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_shell_block_after_alternative_approach_cue() {
        let response = r#"I'll use an alternative approach with echo to create the file:

```bash
echo 'console.log(2 + 3);' > smoke_js/add_two.mjs
```"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("alternative approach"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "echo 'console.log(2 + 3);' > smoke_js/add_two.mjs"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_shell_block_after_option_cue() {
        let response = r#"Option 1: Run individual commands directly

```bash
echo "=== System Information ===" && uname -a && echo "" && echo "=== USB Devices ===" && lsusb
```"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Option 1"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert!(calls[0]
            .arguments
            .get("command")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("uname -a"));
    }

    #[test]
    fn parse_tool_calls_recovers_shell_block_with_tool_code_hint() {
        let response = r#"I'll create a shell script in the workspace.

```bash
mkdir -p smoke_sys
cat > smoke_sys/check.sh << 'EOF'
#!/bin/bash
uname -a
lsusb
EOF
chmod +x smoke_sys/check.sh
./smoke_sys/check.sh
```

Let me execute this:

<tool_code>shell</tool_code>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I'll create a shell script"));
        assert!(text.contains("<tool_code>shell</tool_code>"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        let command = calls[0].arguments.get("command").unwrap().as_str().unwrap();
        assert!(command.contains("mkdir -p smoke_sys"));
        assert!(command.contains("./smoke_sys/check.sh"));
    }

    #[test]
    fn parse_tool_calls_recovers_explicit_shell_parameter_block() {
        let response = r#"I'll create a Python file that adds 2 + 2 and run it for you.

```python
# smoke_py/add_two.py
def add(a, b):
    return a + b

print(add(2, 2))
```

Let me write this file and execute it:

<tool_code>shell</tool_code><parameter>command="mkdir -p smoke_py && cat > smoke_py/add_two.py << 'EOF'
def add(a, b):
    return a + b

print(add(2, 2))
EOF
" />"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I'll create a Python file"));
        assert!(text.contains("```python"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        let command = calls[0].arguments.get("command").unwrap().as_str().unwrap();
        assert!(command.contains("cat > smoke_py/add_two.py << 'EOF'"));
        assert!(command.contains("    return a + b"));
        assert!(command.contains("print(add(2, 2))"));
    }

    #[test]
    fn parse_tool_calls_recovers_bare_shell_marker_from_python_code_block() {
        let response = r#"I'll create a Python file to add 2 + 2 and run it for you.

```python
# Create a Python file to add 2 + 2
with open('add_two.py', 'w') as f:
    f.write('''#!/usr/bin/env python3
result = 2 + 2
print(f"2 + 2 = {result}")
''')

# Run the script
import subprocess
subprocess.run(['python3', 'add_two.py'])
```

Let me execute this using the shell tool:

<tool_code>shell</tool_code>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I'll create a Python file"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        let command = calls[0].arguments.get("command").unwrap().as_str().unwrap();
        assert!(command.starts_with("python3 - <<'PY'"));
        assert!(command.contains("with open('add_two.py', 'w') as f:"));
        assert!(command.contains("subprocess.run(['python3', 'add_two.py'])"));
    }

    #[test]
    fn parse_tool_calls_recovers_malformed_direct_shell_command_tags() {
        let response = r#"I'll create and run a Python file to add 2 + 2 for you.

```python
# Create the Python file
result = 2 + 2
print(f"2 + 2 = {result}")
```

Now let me run this script:

```bash
python3 /tmp/add_two.py
```

Let me execute this now:

<shell command="cat > /tmp/add_two.py << 'EOF'
# Simple Python script to add 2 + 2
result = 2 + 2
print(f"2 + 2 = {result}")
EOF</shell>

<shell command="python3 /tmp/add_two.py"</shell>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I'll create and run a Python file"));
        assert!(calls.len() >= 2);
        let write_command = calls
            .iter()
            .find_map(|call| {
                (call.name == "shell")
                    .then(|| {
                        call.arguments
                            .get("command")
                            .and_then(|value| value.as_str())
                    })
                    .flatten()
                    .filter(|command| command.contains("cat > /tmp/add_two.py << 'EOF'"))
            })
            .expect("expected recovered shell file-write command");
        let run_command = calls
            .iter()
            .find_map(|call| {
                (call.name == "shell")
                    .then(|| {
                        call.arguments
                            .get("command")
                            .and_then(|value| value.as_str())
                    })
                    .flatten()
                    .filter(|command| *command == "python3 /tmp/add_two.py")
            })
            .expect("expected recovered shell run command");
        assert!(write_command.contains("cat > /tmp/add_two.py << 'EOF'"));
        assert!(write_command.contains(r#"print(f"2 + 2 = {result}")"#));
        assert_eq!(run_command, "python3 /tmp/add_two.py");
    }

    #[test]
    fn parse_tool_calls_decodes_escaped_newlines_in_direct_shell_command_tags() {
        let response =
            r#"<shell command="cat > smoke_js/add_two.mjs << 'EOF'\nconsole.log(2 + 3);\nEOF" />"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        let command = calls[0].arguments.get("command").unwrap().as_str().unwrap();
        assert!(command.contains("cat > smoke_js/add_two.mjs << 'EOF'\nconsole.log(2 + 3);\nEOF"));
        assert!(!command.contains("\\n"));
    }

    #[test]
    fn parse_tool_calls_recovers_wrapper_attribute_parameters_payload() {
        let response = r#"I need to inspect the workspace first.
<tool_call name="shell" parameters='{"command":"ls -la"}' />"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I need to inspect the workspace first."));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_ignores_unknown_function_style_calls_in_code_examples() {
        let response = r#"```python
result = 2 + 2
print(result)
```"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(calls.is_empty());
        assert!(text.contains("print(result)"));
    }

    #[test]
    fn parse_tool_calls_handles_bracketed_tool_call_syntax() {
        let response = r#"I'll run that now.[TOOL_CALLS]shell[ARGS]{"command":"lsusb"}"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "I'll run that now.");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "lsusb"
        );
    }

    #[test]
    fn parse_tool_calls_handles_split_bracket_shell_args() {
        let response = r#"[TOOL_CALLS]shell[ARGS]{}[ARGS]"command" "lsusb""#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "lsusb"
        );
    }

    #[test]
    fn parse_tool_calls_handles_positional_bracket_shell_args() {
        let response = r#"[TOOL_CALLS]shell[ARGS]{}[ARGS]lsusb[TOOL_CALLS]{}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "lsusb"
        );
    }

    #[test]
    fn parse_tool_calls_normalizes_workspace_bootstrap_file_paths() {
        let response = r#"file_read({"path":"agen.md"})"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "AGENTS.md"
        );
    }

    #[test]
    fn parse_tool_calls_keeps_bracket_browser_tool_name() {
        let response = r#"[TOOL_CALLS]browser[ARGS]{"url":"https://example.com"}"#;

        let (_, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "browser");
        assert_eq!(
            calls[0].arguments.get("url").unwrap().as_str().unwrap(),
            "https://example.com"
        );
    }

    #[test]
    fn tool_call_signature_normalizes_browser_open_variants() {
        let explicit = tool_call_signature(
            "browser",
            &serde_json::json!({"action":"open","url":"https://example.com"}),
        );
        let inferred = tool_call_signature(
            "browser",
            &serde_json::json!({
                "url":"https://example.com",
                "backend":"rust_native",
                "command":"curl -s 'https://example.com'"
            }),
        );

        assert_eq!(explicit, inferred);
    }

    #[test]
    fn parse_tool_calls_handles_markdown_tool_call_fence() {
        let response = r#"I'll check that.
```tool_call
{"name": "shell", "arguments": {"command": "pwd"}}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "pwd"
        );
        assert!(text.contains("I'll check that."));
        assert!(text.contains("Done."));
        assert!(!text.contains("```tool_call"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_tool_call_hybrid_close_tag() {
        let response = r#"Preface
```tool-call
{"name": "shell", "arguments": {"command": "date"}}
</tool_call>
Tail"#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert!(text.contains("Preface"));
        assert!(text.contains("Tail"));
        assert!(!text.contains("```tool-call"));
    }

    #[test]
    fn parse_tool_calls_handles_markdown_invoke_fence() {
        let response = r#"Checking.
```invoke
{"name": "shell", "arguments": {"command": "date"}}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
        assert!(text.contains("Checking."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_tool_name_fence_format() {
        // Issue #1420: xAI grok models use ```tool <name> format
        let response = r#"I'll write a test file.
```tool file_write
{"path": "/home/user/test.txt", "content": "Hello world"}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(
            calls[0].arguments.get("path").unwrap().as_str().unwrap(),
            "/home/user/test.txt"
        );
        assert!(text.contains("I'll write a test file."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_tool_name_fence_shell() {
        // Issue #1420: Test shell command in ```tool shell format
        let response = r#"```tool shell
{"command": "ls -la"}
```"#;

        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "ls -la"
        );
    }

    #[test]
    fn parse_tool_calls_handles_multiple_tool_name_fences() {
        // Multiple tool calls in ```tool <name> format
        let response = r#"First, I'll write a file.
```tool file_write
{"path": "/tmp/a.txt", "content": "A"}
```
Then read it.
```tool file_read
{"path": "/tmp/a.txt"}
```
Done."#;

        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "file_write");
        assert_eq!(calls[1].name, "file_read");
        assert!(text.contains("First, I'll write a file."));
        assert!(text.contains("Then read it."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_tool_calls_handles_toolcall_tag_alias() {
        let response = r#"<toolcall>
{"name": "shell", "arguments": {"command": "date"}}
</toolcall>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
    }

    #[test]
    fn parse_tool_calls_handles_tool_dash_call_tag_alias() {
        let response = r#"<tool-call>
{"name": "shell", "arguments": {"command": "whoami"}}
</tool-call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "whoami"
        );
    }

    #[test]
    fn parse_tool_calls_handles_invoke_tag_alias() {
        let response = r#"<invoke>
{"name": "shell", "arguments": {"command": "uptime"}}
</invoke>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime"
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_invoke_parameter_format() {
        let response = r#"<minimax:tool_call>
<invoke name="shell">
<parameter name="command">sqlite3 /tmp/test.db ".tables"</parameter>
</invoke>
</minimax:tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            r#"sqlite3 /tmp/test.db ".tables""#
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_invoke_with_surrounding_text() {
        let response = r#"Preface
<minimax:tool_call>
<invoke name='http_request'>
<parameter name='url'>https://example.com</parameter>
<parameter name='method'>GET</parameter>
</invoke>
</minimax:tool_call>
Tail"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Preface"));
        assert!(text.contains("Tail"));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "http_request");
        assert_eq!(
            calls[0].arguments.get("url").unwrap().as_str().unwrap(),
            "https://example.com"
        );
        assert_eq!(
            calls[0].arguments.get("method").unwrap().as_str().unwrap(),
            "GET"
        );
    }

    #[test]
    fn parse_tool_calls_handles_minimax_toolcall_alias_and_cross_close_tag() {
        let response = r#"<tool_call>
{"name":"shell","arguments":{"command":"date"}}
</minimax:toolcall>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "date"
        );
    }

    #[test]
    fn parse_tool_calls_handles_perl_style_tool_call_blocks() {
        let response = r#"TOOL_CALL
{tool => "shell", args => { --command "uname -a" }}}
/TOOL_CALL"#;

        let calls = parse_perl_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uname -a"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_unclosed_tool_call_with_json() {
        let response = r#"I will call the tool now.
<tool_call>
{"name": "shell", "arguments": {"command": "uptime -p"}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("I will call the tool now."));
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime -p"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_mismatched_close_tag() {
        let response = r#"<tool_call>
{"name": "shell", "arguments": {"command": "uptime"}}
</arg_value>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(
            calls[0].arguments.get("command").unwrap().as_str().unwrap(),
            "uptime"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_cross_alias_closing_tags() {
        let response = r#"<toolcall>
{"name": "shell", "arguments": {"command": "date"}}
</tool_call>"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
    }

    #[test]
    fn parse_tool_calls_rejects_raw_tool_json_without_tags() {
        // SECURITY: Raw JSON without explicit wrappers should NOT be parsed
        // This prevents prompt injection attacks where malicious content
        // could include JSON that mimics a tool call.
        let response = r#"Sure, creating the file now.
{"name": "file_write", "arguments": {"path": "hello.py", "content": "print('hello')"}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Sure, creating the file now."));
        assert_eq!(
            calls.len(),
            0,
            "Raw JSON without wrappers should not be parsed"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_malformed_jsonish_named_tool_payload() {
        let response = r#"{"name": "file_read", "parameters": {"arguments": {"/llamafarm-data/workspace/tool_smoke_matrix.txt"}}}"#;

        let (text, calls) = parse_tool_calls(response);
        assert!(text.is_empty());
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(
            calls[0]
                .arguments
                .get("path")
                .and_then(|value| value.as_str()),
            Some("/llamafarm-data/workspace/tool_smoke_matrix.txt")
        );
    }

    #[test]
    fn build_tool_instructions_includes_all_tools() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_config(
            &crate::config::AutonomyConfig::default(),
            std::path::Path::new("/tmp"),
        ));
        let tools = tools::default_tools(security);
        let instructions = build_tool_instructions(&tools);

        assert!(instructions.contains("## Tool Use Protocol"));
        assert!(instructions.contains("<tool_call>"));
        assert!(instructions.contains("shell"));
        assert!(instructions.contains("file_read"));
        assert!(instructions.contains("file_write"));
    }

    #[test]
    fn build_shell_policy_instructions_lists_allowlist() {
        let mut autonomy = crate::config::AutonomyConfig::default();
        autonomy.level = crate::security::AutonomyLevel::Supervised;
        autonomy.allowed_commands = vec!["grep".into(), "cat".into(), "grep".into()];

        let instructions = build_shell_policy_instructions(&autonomy);

        assert!(instructions.contains("## Shell Policy"));
        assert!(instructions.contains("Autonomy level: `supervised`"));
        assert!(instructions.contains("`cat`"));
        assert!(instructions.contains("`grep`"));
    }

    #[test]
    fn build_shell_policy_instructions_handles_wildcard() {
        let mut autonomy = crate::config::AutonomyConfig::default();
        autonomy.level = crate::security::AutonomyLevel::Full;
        autonomy.allowed_commands = vec!["*".into()];

        let instructions = build_shell_policy_instructions(&autonomy);

        assert!(instructions.contains("Autonomy level: `full`"));
        assert!(instructions.contains("wildcard `*`"));
    }

    #[test]
    fn build_shell_policy_instructions_read_only_disables_shell() {
        let mut autonomy = crate::config::AutonomyConfig::default();
        autonomy.level = crate::security::AutonomyLevel::ReadOnly;

        let instructions = build_shell_policy_instructions(&autonomy);

        assert!(instructions.contains("Autonomy level: `read_only`"));
        assert!(instructions.contains("Shell execution is disabled"));
    }

    #[test]
    fn build_ipc_state_usage_instructions_warns_against_guessed_keys() {
        let tools: Vec<Box<dyn Tool>> = vec![Box::new(CountingTool::new(
            "state_get",
            Arc::new(AtomicUsize::new(0)),
        ))];

        let instructions = build_ipc_state_usage_instructions(&tools);

        assert!(instructions.contains("shared inter-agent state"));
        assert!(instructions.contains("Do not probe guessed keys"));
        assert!(instructions.contains("current_task"));
    }

    #[test]
    fn should_auto_plan_current_request_skips_short_direct_requests() {
        let history = vec![ChatMessage::user("write a file, read it, then delete it")];

        assert!(
            !should_auto_plan_current_request(&history),
            "short direct requests should execute without a forced task_plan"
        );
    }

    #[test]
    fn should_auto_plan_current_request_keeps_exhaustive_tool_sweeps() {
        let history = vec![ChatMessage::user(
            "test all tool calls in the local environment",
        )];

        assert!(
            should_auto_plan_current_request(&history),
            "batch tool sweeps should still trigger task_plan"
        );
    }

    #[test]
    fn should_auto_plan_current_request_keeps_plan_then_execute_requests() {
        let history = vec![ChatMessage::user(
            "Use task_plan first because this request is multi-step. Then complete the plan end-to-end using real tools: create a directory named rust_kernel in the workspace, run pwd and ls -d rust_kernel to verify it exists, delete the directory, and answer with the verification output plus whether cleanup succeeded. Do not stop after planning.",
        )];

        assert!(
            should_auto_plan_current_request(&history),
            "plan-first prompts that explicitly require execution must not be treated as planning-only"
        );
    }

    #[test]
    fn looks_like_task_plan_followup_question_detects_plan_id_requests() {
        let records = vec![SuccessfulToolRecord {
            name: "task_plan".into(),
            arguments: serde_json::json!({
                "action": "create",
                "tasks": [
                    {"title": "Create rust_kernel"},
                    {"title": "Verify it"},
                    {"title": "Delete it"},
                ]
            }),
            output: "Created 3 task(s).".into(),
        }];

        assert!(looks_like_task_plan_followup_question(
            "The active task plan was created, but I don't have the plan_id needed to mark steps complete. Could you provide that plan_id so I can proceed with executing the plan?",
            &records,
        ));
    }

    #[test]
    fn build_task_plan_execution_followup_prompt_includes_next_step_title() {
        let records = vec![
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "create",
                    "tasks": [
                        { "title": "Write a file" },
                        {
                            "title": "Run it",
                            "context": "Start on the host-published app port and require HTTP 200.",
                            "tools": ["shell", "http_request"]
                        },
                        { "title": "Delete it" }
                    ]
                }),
                output: "Created 3 task(s).".into(),
            },
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "update",
                    "id": 1,
                    "status": "completed"
                }),
                output: "Task [1] updated to completed.".into(),
            },
        ];

        let prompt =
            build_task_plan_execution_followup_prompt(&records).expect("task plan should exist");

        assert!(prompt.contains("[1] [completed] Write a file"));
        assert!(prompt.contains("[2] [pending] Run it"));
        assert!(prompt.contains("Next incomplete step: [2] Run it"));
        assert!(prompt
            .contains("Step context: Start on the host-published app port and require HTTP 200."));
        assert!(prompt.contains("Expected tools: shell, http_request"));
    }

    #[test]
    fn task_plan_list_output_recovers_context_and_tools() {
        let records = vec![SuccessfulToolRecord {
            name: "task_plan".into(),
            arguments: serde_json::json!({"action": "list"}),
            output: "Tasks (0/1 completed; 0/1 resolved):\n\
- [1] [pending] Launch app\n\
    ↳ context: Bind to the published development port.\n\
    ↳ tools: shell, http_request"
                .into(),
        }];

        let prompt =
            build_task_plan_execution_followup_prompt(&records).expect("task plan should exist");
        assert!(prompt.contains("Step context: Bind to the published development port."));
        assert!(prompt.contains("Expected tools: shell, http_request"));
    }

    #[test]
    fn terminal_task_plan_statuses_finish_an_audit_without_retrying_blocked_steps() {
        let records = vec![
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "create",
                    "tasks": [
                        { "title": "Verified tool" },
                        { "title": "Missing credential" },
                        { "title": "Unavailable host service" }
                    ]
                }),
                output: "Created 3 task(s).".into(),
            },
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "update", "id": 1, "status": "completed"
                }),
                output: "Task [1] updated to completed.".into(),
            },
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "update", "id": 2, "status": "blocked"
                }),
                output: "Task [2] updated to blocked.".into(),
            },
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "update", "id": 3, "status": "failed"
                }),
                output: "Task [3] updated to failed.".into(),
            },
        ];

        let progress = task_plan_progress_snapshot(&records).expect("plan progress");
        assert_eq!(progress.completed, 1);
        assert_eq!(progress.resolved, 3);
        assert_eq!(progress.total, 3);
        assert!(build_task_plan_execution_followup_prompt(&records).is_none());
        assert!(build_post_plan_create_start_prompt(&records).is_none());
    }

    #[test]
    fn build_task_plan_execution_followup_prompt_forbids_plan_updates_after_execution_starts() {
        let records = vec![
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "create",
                    "tasks": [
                        { "title": "Write a file" },
                        { "title": "Run it" }
                    ]
                }),
                output: "Created 2 task(s).".into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "touch demo.txt"
                }),
                output: "".into(),
            },
        ];

        let prompt =
            build_task_plan_execution_followup_prompt(&records).expect("task plan should exist");

        assert!(prompt.contains("Do NOT emit `task_plan` create/update calls now"));
        assert!(prompt.contains("Continue directly with the next incomplete step"));
    }

    #[test]
    fn build_post_plan_create_start_prompt_targets_first_incomplete_step() {
        let records = vec![
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "create",
                    "tasks": [
                        { "title": "Write a file" },
                        { "title": "Run it" }
                    ]
                }),
                output: "Created 2 task(s).".into(),
            },
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "update",
                    "id": 1,
                    "status": "completed"
                }),
                output: "Task [1] updated to completed.".into(),
            },
        ];

        let prompt = build_post_plan_create_start_prompt(&records)
            .expect("task plan should have an incomplete step");

        assert!(prompt.contains("task plan created (2 steps)"));
        assert!(prompt.contains("Execute step [2]: Run it"));
        assert!(prompt.contains("do not ask the user"));
    }

    #[test]
    fn build_post_plan_create_start_prompt_uses_task_plan_output_when_create_args_are_lossy() {
        let records = vec![SuccessfulToolRecord {
            name: "task_plan".into(),
            arguments: serde_json::json!({
                "hint": "create",
            }),
            output: "Created 2 task(s).\nTasks (0/2 completed):\n- [1] [pending] Write a file\n- [2] [pending] Run it".into(),
        }];

        let prompt = build_post_plan_create_start_prompt(&records)
            .expect("task plan output should be enough to recover the plan");

        assert!(prompt.contains("task plan created (2 steps)"));
        assert!(prompt.contains("Execute step [1]: Write a file"));
    }

    #[test]
    fn build_post_web_search_fetch_prompt_extracts_top_urls() {
        let records = vec![SuccessfulToolRecord {
            name: "web_search_tool".into(),
            arguments: serde_json::json!({ "query": "official Rust language website" }),
            output: "Search results for: official Rust language website (via DuckDuckGo)\n1. Rust Programming Language\n   https://www.rust-lang.org/\n   Empowering everyone to build reliable software.\n2. Rust Book\n   https://doc.rust-lang.org/book/\n   The Rust Programming Language book."
                .into(),
        }];

        let prompt = build_post_web_search_fetch_prompt(&records, true)
            .expect("web search output should yield fetch URLs");

        assert!(prompt.contains("use web_fetch"));
        assert!(prompt.contains("https://www.rust-lang.org/"));
        assert!(prompt.contains("https://doc.rust-lang.org/book/"));
        assert!(prompt.contains("Do not summarize only the search snippets"));
    }

    #[test]
    fn web_search_only_requires_fetch_when_fetch_is_selected() {
        let records = vec![SuccessfulToolRecord {
            name: "web_search_tool".into(),
            arguments: serde_json::json!({ "query": "current news" }),
            output: "Search result snippets".into(),
        }];
        assert!(web_search_needs_fetch_continuation(&records, true));
        assert!(!web_search_needs_fetch_continuation(&records, false));
    }

    #[test]
    fn build_agentic_web_research_followup_prompt_requests_multi_hop_fetches_for_deep_research() {
        let history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user(
                "look online and do an in-depth research comparison across multiple sources",
            ),
        ];
        let records = vec![SuccessfulToolRecord {
            name: "web_search_tool".into(),
            arguments: serde_json::json!({ "query": "rust language official site and docs" }),
            output: "Search results:\n1. Rust\nhttps://www.rust-lang.org/\n2. Rust Book\nhttps://doc.rust-lang.org/book/\n3. Rust std docs\nhttps://doc.rust-lang.org/std/"
                .into(),
        }];

        let prompt = build_agentic_web_research_followup_prompt(&history, &records, true)
            .expect("deep research request should produce a multi-hop follow-up prompt");

        assert!(prompt.contains("deeper online-research task"));
        assert!(prompt.contains("web_fetch"));
        assert!(prompt.contains("https://www.rust-lang.org/"));
        assert!(prompt.contains("https://doc.rust-lang.org/book/"));
    }

    #[test]
    fn build_agentic_web_research_followup_prompt_stops_after_single_fetch_for_basic_lookup() {
        let history = vec![
            ChatMessage::system("test-system"),
            ChatMessage::user("find the official rust language website"),
        ];
        let records = vec![
            SuccessfulToolRecord {
                name: "web_search_tool".into(),
                arguments: serde_json::json!({ "query": "official rust language website" }),
                output: "Search results:\n1. Rust\nhttps://www.rust-lang.org/".into(),
            },
            SuccessfulToolRecord {
                name: "web_fetch".into(),
                arguments: serde_json::json!({ "url": "https://www.rust-lang.org/" }),
                output: "Rust Programming Language".into(),
            },
        ];

        assert!(
            build_agentic_web_research_followup_prompt(&history, &records, true).is_none(),
            "basic lookup should not force extra fetch hops after one page is read"
        );
    }

    #[test]
    fn synthesize_python_execution_answer_includes_output_and_cleanup() {
        let records = vec![
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "/llamafarm-data/workspace/add_two.py",
                    "content": "print(2 + 2)"
                }),
                output: "Written 12 bytes to /llamafarm-data/workspace/add_two.py".into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "python3 /llamafarm-data/workspace/add_two.py"
                }),
                output: "4".into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "rm /llamafarm-data/workspace/add_two.py"
                }),
                output: "deleted successfully".into(),
            },
        ];

        let answer = synthesize_python_execution_answer(&records)
            .expect("python execution should synthesize a grounded answer");

        assert!(answer.contains("created and executed successfully"));
        assert!(answer.contains("```text\n4\n```"));
        assert!(answer.contains("```python\nprint(2 + 2)\n```"));
        assert!(answer.contains("deleted after execution"));
    }

    #[test]
    fn synthesize_python_execution_answer_reports_failure_on_syntax_error() {
        // Regression test: the "Tool follow through" session showed the exact
        // canned "was created and executed successfully" wording sitting right
        // above a visible Python SyntaxError traceback. The wording must flip
        // to a failure message whenever the captured output shows a crash.
        let records = vec![
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "/llamafarm-data/workspace/data_pipeline_fixed.py",
                    "content": "def broken():\n    pass"
                }),
                output: "Written 24 bytes to /llamafarm-data/workspace/data_pipeline_fixed.py"
                    .into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "python3 /llamafarm-data/workspace/data_pipeline_fixed.py"
                }),
                output: "  File \"data_pipeline_fixed.py\", line 180\n    \"current_price\": round(current_price if current_price else (previous_close and float(previous_close) else 0), 2),\nSyntaxError: invalid syntax".into(),
            },
        ];

        let answer = synthesize_python_execution_answer(&records)
            .expect("python execution should synthesize a grounded answer even on failure");

        assert!(
            !answer.contains("executed successfully"),
            "must not claim success when the output shows a SyntaxError: {answer}"
        );
        assert!(answer.contains("failed when executed"));
        assert!(answer.contains("SyntaxError: invalid syntax"));
    }

    #[test]
    fn should_short_circuit_after_tool_execution_stops_on_clean_success() {
        let history = vec![ChatMessage::user("write and run a python script")];
        let records = vec![
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "/llamafarm-data/workspace/add_two.py",
                    "content": "print(2 + 2)"
                }),
                output: "Written 12 bytes to /llamafarm-data/workspace/add_two.py".into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "python3 /llamafarm-data/workspace/add_two.py"
                }),
                output: "4".into(),
            },
        ];

        assert!(should_short_circuit_after_tool_execution(
            &history, &records
        ));
    }

    #[test]
    fn should_short_circuit_after_tool_execution_keeps_going_after_a_crash() {
        // Regression test: forcing an early stop the instant a script produces
        // any non-empty output meant the loop stopped right as a crash became
        // visible, never letting the model see and fix the error in the same
        // turn. A failing script must NOT short-circuit the loop.
        let history = vec![ChatMessage::user("write and run a python script")];
        let records = vec![
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "/llamafarm-data/workspace/data_pipeline_fixed.py",
                    "content": "def broken():\n    pass"
                }),
                output: "Written 24 bytes to /llamafarm-data/workspace/data_pipeline_fixed.py"
                    .into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "python3 /llamafarm-data/workspace/data_pipeline_fixed.py"
                }),
                output: "Traceback (most recent call last):\nSyntaxError: invalid syntax".into(),
            },
        ];

        assert!(!should_short_circuit_after_tool_execution(
            &history, &records
        ));
    }

    #[test]
    fn synthesize_grounded_final_answer_prefers_python_result_after_task_plan_execution() {
        let records = vec![
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "create",
                    "tasks": [
                        {"title": "Write Python file"},
                        {"title": "Run the Python script"},
                        {"title": "Delete the file"},
                    ]
                }),
                output: "Task plan created.".into(),
            },
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "/llamafarm-data/workspace/add_two.py",
                    "content": "print(2 + 2)"
                }),
                output: "Written 12 bytes to /llamafarm-data/workspace/add_two.py".into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "python3 /llamafarm-data/workspace/add_two.py"
                }),
                output: "4".into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "rm /llamafarm-data/workspace/add_two.py"
                }),
                output: "deleted successfully".into(),
            },
        ];

        let answer = synthesize_grounded_final_answer(&records, &[])
            .expect("python execution should outrank a prior task plan summary");

        assert!(answer.contains("created and executed successfully"));
        assert!(answer.contains("```text\n4\n```"));
        assert!(!answer.starts_with("Task plan created with"));
    }

    #[test]
    fn synthesize_grounded_final_answer_does_not_cite_a_stale_web_search_url_after_later_work() {
        // Regression: an early research-phase web_search's URL used to win
        // unconditionally over everything that happened afterward, so a
        // completed coding task (files written, a server started) reported
        // "The main URL is <the tutorial link from step one>" as its final
        // answer instead of anything about the actual outcome.
        let records = vec![
            SuccessfulToolRecord {
                name: "web_search_tool".into(),
                arguments: serde_json::json!({"query": "stock trading platform tutorial"}),
                output:
                    "1. Predicting Stock Prices - Medium\n   https://medium.com/example-tutorial"
                        .into(),
            },
            SuccessfulToolRecord {
                name: "file_write".into(),
                arguments: serde_json::json!({
                    "path": "stock_trading_platform/app.py",
                    "content": "from flask import Flask\napp = Flask(__name__)\n"
                }),
                output: "Written 40 bytes to stock_trading_platform/app.py".into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({
                    "command": "nohup python3 app.py > server.log 2>&1 & echo \"Server PID: $!\""
                }),
                output: "Server PID: 1853".into(),
            },
        ];

        let answer = synthesize_grounded_final_answer(&records, &[]);

        if let Some(answer) = answer {
            assert!(
                !answer.contains("medium.com"),
                "must not fall back to the stale research-phase URL: {answer}"
            );
        }
    }

    #[test]
    fn synthesize_grounded_final_answer_plan_only_returns_plan_summary() {
        // Plan-only requests ("make me a plan") should still get a "Task plan created" answer.
        let records = vec![SuccessfulToolRecord {
            name: "task_plan".into(),
            arguments: serde_json::json!({
                "action": "create",
                "tasks": [
                    {"title": "Step A"},
                    {"title": "Step B"},
                ]
            }),
            output: "Task plan created.".into(),
        }];

        // Empty history → not action-oriented → plan summary should be returned.
        let answer = synthesize_grounded_final_answer(&records, &[])
            .expect("plan-only flow should still synthesize a plan summary");
        assert!(answer.starts_with("Task plan created with 2 steps:"));
        assert!(answer.contains("Step A"));
        assert!(answer.contains("Step B"));
    }

    #[test]
    fn synthesize_grounded_final_answer_plan_only_uses_task_plan_output_when_args_are_lossy() {
        let records = vec![SuccessfulToolRecord {
            name: "task_plan".into(),
            arguments: serde_json::json!({
                "hint": "create",
            }),
            output: "Created 2 task(s).\nTasks (0/2 completed):\n- [1] [pending] Step A\n- [2] [pending] Step B".into(),
        }];

        let answer = synthesize_grounded_final_answer(&records, &[])
            .expect("plan summary should be recovered from task_plan output");
        assert!(answer.starts_with("Task plan created with 2 steps:"));
        assert!(answer.contains("Step A"));
        assert!(answer.contains("Step B"));
    }

    #[test]
    fn synthesize_grounded_final_answer_action_request_suppresses_plan_only_answer() {
        // For action-oriented requests, plan-create must NOT be treated as terminal —
        // the loop should continue into execution.  Verify that synthesize returns None.
        let records = vec![SuccessfulToolRecord {
            name: "task_plan".into(),
            arguments: serde_json::json!({
                "action": "create",
                "tasks": [
                    {"title": "Run shell command"},
                    {"title": "Write a file"},
                    {"title": "Delete the file"},
                ]
            }),
            output: "Task plan created.".into(),
        }];

        // History contains a multi-step action request → should_auto_plan_current_request=true
        let history = vec![ChatMessage::user(
            "run a shell command, write a file, then delete it",
        )];

        let answer = synthesize_grounded_final_answer(&records, &history);
        assert!(
            answer.is_none(),
            "action-oriented plan-create should return None so execution continues, got: {answer:?}"
        );
    }

    #[test]
    fn synthesize_grounded_final_answer_plan_first_then_execute_request_suppresses_plan_only_answer(
    ) {
        let records = vec![SuccessfulToolRecord {
            name: "task_plan".into(),
            arguments: serde_json::json!({
                "action": "create",
                "tasks": [
                    {"title": "Create a directory named rust_kernel in the workspace"},
                    {"title": "Run pwd and ls -d rust_kernel to verify it exists"},
                    {"title": "Delete the rust_kernel directory"},
                    {"title": "Answer with the verification output plus whether cleanup succeeded"},
                ]
            }),
            output: "Task plan created.".into(),
        }];

        let history = vec![ChatMessage::user(
            "Use task_plan first because this request is multi-step. Then complete the plan end-to-end using real tools: create a directory named rust_kernel in the workspace, run pwd and ls -d rust_kernel to verify it exists, delete the directory, and answer with the verification output plus whether cleanup succeeded. Do not stop after planning.",
        )];

        let answer = synthesize_grounded_final_answer(&records, &history);
        assert!(
            answer.is_none(),
            "plan-first execution requests should keep the loop running, got: {answer:?}"
        );
    }

    #[test]
    fn iteration_had_only_task_plan_create_detects_tasks_array_form() {
        // Regression: task_plan called with {"tasks":[...]} (no explicit "action":"create")
        // must still be detected as a create so the post-plan continuation fires.
        let record = SuccessfulToolRecord {
            name: "task_plan".into(),
            arguments: serde_json::json!({
                "tasks": [
                    {"title": "Do thing A"},
                    {"title": "Do thing B"},
                ]
            }),
            output: "Task plan created.".into(),
        };
        assert!(
            task_plan_call_is_create(&record.arguments),
            "tasks-array form must be recognised as a create call"
        );
    }

    #[test]
    fn tools_to_openai_format_produces_valid_schema() {
        use crate::security::SecurityPolicy;
        let security = Arc::new(SecurityPolicy::from_config(
            &crate::config::AutonomyConfig::default(),
            std::path::Path::new("/tmp"),
        ));
        let tools = tools::default_tools(security);
        let formatted = tools_to_openai_format(&tools);

        assert!(!formatted.is_empty());
        for tool_json in &formatted {
            assert_eq!(tool_json["type"], "function");
            assert!(tool_json["function"]["name"].is_string());
            assert!(tool_json["function"]["description"].is_string());
            assert!(!tool_json["function"]["name"].as_str().unwrap().is_empty());
        }
        // Verify known tools are present
        let names: Vec<&str> = formatted
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"shell"));
        assert!(names.contains(&"file_read"));
    }

    #[test]
    fn trim_history_preserves_system_prompt() {
        let mut history = vec![ChatMessage::system("system prompt")];
        for i in 0..DEFAULT_MAX_HISTORY_MESSAGES + 20 {
            history.push(ChatMessage::user(format!("msg {i}")));
        }
        let original_len = history.len();
        assert!(original_len > DEFAULT_MAX_HISTORY_MESSAGES + 1);

        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);

        // System prompt preserved
        assert_eq!(history[0].role, "system");
        assert_eq!(history[0].content, "system prompt");
        // Trimmed to limit
        assert_eq!(history.len(), DEFAULT_MAX_HISTORY_MESSAGES + 1); // +1 for system
                                                                     // Most recent messages preserved
        let last = &history[history.len() - 1];
        assert_eq!(
            last.content,
            format!("msg {}", DEFAULT_MAX_HISTORY_MESSAGES + 19)
        );
    }

    #[test]
    fn trim_history_noop_when_within_limit() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("hello"),
            ChatMessage::assistant("hi"),
        ];
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history.len(), 3);
    }

    #[test]
    fn zero_history_budget_preserves_raw_history_until_context_pressure() {
        let mut history = vec![ChatMessage::system("sys")];
        for index in 0..1_000 {
            history.push(ChatMessage::user(format!("message {index}")));
        }
        let original = history.clone();

        trim_history(&mut history, 0);

        assert_eq!(history, original);
        assert_eq!(compaction_range(&history, 0), None);
        assert_eq!(plan_boundary_history_budget(0), None);
    }

    #[test]
    fn plan_boundaries_preserve_explicit_long_context_history() {
        assert_eq!(plan_boundary_history_budget(48), Some(12));
        assert_eq!(plan_boundary_history_budget(512), None);
    }

    #[test]
    fn build_compaction_transcript_formats_roles() {
        let messages = vec![
            ChatMessage::user("I like dark mode"),
            ChatMessage::assistant("Got it"),
        ];
        let transcript = build_compaction_transcript(&messages);
        assert!(transcript.contains("USER: I like dark mode"));
        assert!(transcript.contains("ASSISTANT: Got it"));
    }

    #[test]
    fn deterministic_compaction_summary_uses_checklist_and_recent_results_not_raw_history() {
        let to_compact = vec![
            ChatMessage::user("old raw message one"),
            ChatMessage::assistant("old raw message two"),
        ];
        let successful_records = vec![
            SuccessfulToolRecord {
                name: "task_plan".into(),
                arguments: serde_json::json!({
                    "action": "create",
                    "tasks": [{"title": "Inspect repo"}, {"title": "Ship fix"}],
                }),
                output: "Plan created".into(),
            },
            SuccessfulToolRecord {
                name: "shell".into(),
                arguments: serde_json::json!({"command": "cargo build"}),
                output: "Compiling... Finished".into(),
            },
        ];
        let failed_records = vec![FailedToolRecord {
            name: "shell".into(),
            output: "permission denied".into(),
        }];

        let summary =
            deterministic_compaction_summary(&to_compact, &successful_records, &failed_records);

        // Checklist + minimal results, not the entire compacted-away chat history.
        assert!(summary.contains("Active task plan"));
        assert!(summary.contains("Inspect repo"));
        assert!(summary.contains("shell => Compiling"));
        assert!(summary.contains("Last tool error"));
        assert!(!summary.contains("old raw message"));
    }

    #[test]
    fn deterministic_compaction_summary_falls_back_to_placeholder_without_records() {
        let to_compact = vec![
            ChatMessage::user("old raw message one"),
            ChatMessage::assistant("old raw message two"),
        ];

        let summary = deterministic_compaction_summary(&to_compact, &[], &[]);

        assert!(!summary.contains("old raw message"));
        assert!(summary.contains("2 earlier tool-chain message"));
    }

    #[test]
    fn apply_compaction_summary_replaces_old_segment() {
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("old 1"),
            ChatMessage::assistant("old 2"),
            ChatMessage::user("recent 1"),
            ChatMessage::assistant("recent 2"),
        ];

        apply_compaction_summary(&mut history, 1, 3, "- user prefers concise replies");

        assert_eq!(history.len(), 4);
        assert!(history[1].content.contains("Compaction summary"));
        assert!(history[2].content.contains("recent 1"));
        assert!(history[3].content.contains("recent 2"));
    }

    #[tokio::test]
    async fn auto_compact_history_persists_summary_to_memory() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        let provider = SummarizingProvider;
        let mut history = vec![
            ChatMessage::system("sys"),
            ChatMessage::user("old 1"),
            ChatMessage::assistant("old 2"),
            ChatMessage::user("old 3"),
            ChatMessage::assistant("recent 1"),
            ChatMessage::user("recent 2"),
        ];

        let compacted = auto_compact_history(&mut history, &provider, "mock-model", 3, Some(&mem))
            .await
            .expect("compaction should succeed");

        assert!(compacted);
        let daily_entries = mem
            .list(Some(&MemoryCategory::Daily), None)
            .await
            .expect("daily memories should list");
        assert!(daily_entries.iter().any(|entry| {
            entry.key.starts_with("conversation_summary_")
                && entry.content.contains("preserved context summary")
        }));
    }

    #[test]
    fn autosave_memory_key_has_prefix_and_uniqueness() {
        let key1 = autosave_memory_key("user_msg");
        let key2 = autosave_memory_key("user_msg");

        assert!(key1.starts_with("user_msg_"));
        assert!(key2.starts_with("user_msg_"));
        assert_ne!(key1, key2);
    }

    #[tokio::test]
    async fn autosave_memory_keys_preserve_multiple_turns() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();

        let key1 = autosave_memory_key("user_msg");
        let key2 = autosave_memory_key("user_msg");

        mem.store(&key1, "I'm Paul", MemoryCategory::Conversation, None)
            .await
            .unwrap();
        mem.store(&key2, "I'm 45", MemoryCategory::Conversation, None)
            .await
            .unwrap();

        assert_eq!(mem.count().await.unwrap(), 2);

        let recalled = mem.recall("45", 5, None).await.unwrap();
        assert!(recalled.iter().any(|entry| entry.content.contains("45")));
    }

    #[tokio::test]
    async fn build_context_ignores_legacy_assistant_autosave_entries() {
        let tmp = TempDir::new().unwrap();
        let mem = SqliteMemory::new(tmp.path()).unwrap();
        mem.store(
            "assistant_resp_poisoned",
            "User suffered a fabricated event",
            MemoryCategory::Daily,
            None,
        )
        .await
        .unwrap();
        mem.store(
            "user_msg_real",
            "User asked for concise status updates",
            MemoryCategory::Conversation,
            None,
        )
        .await
        .unwrap();

        let context = build_context(&mem, "status updates", 0.0).await;
        assert!(context.contains("user_msg_real"));
        assert!(!context.contains("assistant_resp_poisoned"));
        assert!(!context.contains("fabricated event"));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Tool Call Parsing Edge Cases
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_tool_calls_handles_empty_tool_result() {
        // Recovery: Empty tool_result tag should be handled gracefully
        let response = r#"I'll run that command.
<tool_result name="shell">

</tool_result>
Done."#;
        let (text, calls) = parse_tool_calls(response);
        assert!(text.contains("Done."));
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_arguments_value_handles_null() {
        // Recovery: null arguments are returned as-is (Value::Null)
        let value = serde_json::json!(null);
        let result = parse_arguments_value(Some(&value));
        assert!(result.is_null());
    }

    #[test]
    fn parse_tool_calls_handles_empty_tool_calls_array() {
        // Recovery: Empty tool_calls array should unwrap content text and avoid false parse issues.
        let response = r#"{"content": "Hello", "tool_calls": []}"#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(text, "Hello");
        assert!(calls.is_empty());
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_json_wrapper_with_empty_tool_calls() {
        let response = r#"{"content":"File written successfully.","tool_calls":[]}"#;
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(issue.is_none());
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_malformed_payloads() {
        let response =
            "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}</tool_call>";
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(
            issue.is_some(),
            "malformed tool payload should be flagged for diagnostics"
        );
    }

    #[test]
    fn detect_tool_call_parse_issue_flags_jsonish_task_plan_tail() {
        let response = r#"{"task_plan":{"action":"create","tasks":[{"action":"write","file":"/llamafarm-data/workspace/smoke_test.py","content":"print(2 + 2)"}]}"#;
        let issue = detect_tool_call_parse_issue(response, &[]);
        assert!(
            issue.is_some(),
            "truncated json-ish task_plan payload should be flagged for retry"
        );
    }

    #[test]
    fn detect_tool_call_parse_issue_ignores_normal_text() {
        let issue = detect_tool_call_parse_issue("Thanks, done.", &[]);
        assert!(issue.is_none());
    }

    #[test]
    fn build_missing_tool_call_retry_prompt_includes_current_user_task() {
        let history = vec![
            ChatMessage::user("Write a Python file, run it, and delete it."),
            ChatMessage::assistant("I'll do that."),
        ];

        let prompt = build_missing_tool_call_retry_prompt(&history);
        assert!(prompt.starts_with(MISSING_TOOL_CALL_RETRY_PROMPT));
        assert!(prompt.contains("Current user task:"));
        assert!(prompt.contains("Write a Python file, run it, and delete it."));
    }

    #[test]
    fn parse_tool_calls_handles_whitespace_only_name() {
        // Recovery: Whitespace-only tool name should return None
        let value = serde_json::json!({"function": {"name": "   ", "arguments": {}}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_none());
    }

    #[test]
    fn parse_tool_calls_handles_empty_string_arguments() {
        // Recovery: Empty string arguments should be handled
        let value = serde_json::json!({"name": "test", "arguments": ""});
        let result = parse_tool_call_value(&value);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "test");
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - History Management
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn trim_history_with_no_system_prompt() {
        // Recovery: History without system prompt should trim correctly
        let mut history = vec![];
        for i in 0..DEFAULT_MAX_HISTORY_MESSAGES + 20 {
            history.push(ChatMessage::user(format!("msg {i}")));
        }
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history.len(), DEFAULT_MAX_HISTORY_MESSAGES);
    }

    #[test]
    fn trim_history_preserves_role_ordering() {
        // Recovery: After trimming, role ordering should remain consistent
        let mut history = vec![ChatMessage::system("system")];
        for i in 0..DEFAULT_MAX_HISTORY_MESSAGES + 10 {
            history.push(ChatMessage::user(format!("user {i}")));
            history.push(ChatMessage::assistant(format!("assistant {i}")));
        }
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history[0].role, "system");
        assert_eq!(history[history.len() - 1].role, "assistant");
    }

    #[test]
    fn trim_history_with_only_system_prompt() {
        // Recovery: Only system prompt should not be trimmed
        let mut history = vec![ChatMessage::system("system prompt")];
        trim_history(&mut history, DEFAULT_MAX_HISTORY_MESSAGES);
        assert_eq!(history.len(), 1);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Arguments Parsing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_arguments_value_handles_invalid_json_string() {
        // Recovery: Invalid JSON string should return empty object
        let value = serde_json::Value::String("not valid json".to_string());
        let result = parse_arguments_value(Some(&value));
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    #[test]
    fn parse_arguments_value_handles_none() {
        // Recovery: None arguments should return empty object
        let result = parse_arguments_value(None);
        assert!(result.is_object());
        assert!(result.as_object().unwrap().is_empty());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - JSON Extraction
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn extract_json_values_handles_empty_string() {
        // Recovery: Empty input should return empty vec
        let result = extract_json_values("");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_json_values_handles_whitespace_only() {
        // Recovery: Whitespace only should return empty vec
        let result = extract_json_values("   \n\t  ");
        assert!(result.is_empty());
    }

    #[test]
    fn extract_json_values_handles_multiple_objects() {
        // Recovery: Multiple JSON objects should all be extracted
        let input = r#"{"a": 1}{"b": 2}{"c": 3}"#;
        let result = extract_json_values(input);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn extract_json_values_handles_arrays() {
        // Recovery: JSON arrays should be extracted
        let input = r#"[1, 2, 3]{"key": "value"}"#;
        let result = extract_json_values(input);
        assert_eq!(result.len(), 2);
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Constants Validation
    // ═══════════════════════════════════════════════════════════════════════

    const _: () = {
        assert!(DEFAULT_MAX_HISTORY_MESSAGES > 0);
        assert!(DEFAULT_MAX_HISTORY_MESSAGES <= 1000);
    };

    #[test]
    fn constants_bounds_are_compile_time_checked() {
        // Bounds are enforced by the const assertions above.
    }

    #[test]
    fn zero_tool_iteration_limit_always_has_capacity() {
        let limit = None;
        assert!(tool_loop_has_next_iteration(0, limit));
        assert!(tool_loop_has_next_iteration(100_000, limit));
        assert!(tool_loop_has_next_iteration(usize::MAX, limit));
    }

    #[test]
    fn positive_tool_iteration_limit_stays_bounded() {
        let limit = Some(3);
        assert!(tool_loop_has_next_iteration(0, limit));
        assert!(tool_loop_has_next_iteration(1, limit));
        assert!(!tool_loop_has_next_iteration(2, limit));
    }

    // ═══════════════════════════════════════════════════════════════════════
    // Recovery Tests - Tool Call Value Parsing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_tool_call_value_handles_missing_name_field() {
        // Recovery: Missing name field should return None
        let value = serde_json::json!({"function": {"arguments": {}}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_none());
    }

    #[test]
    fn parse_tool_call_value_handles_top_level_name() {
        // Recovery: Tool call with name at top level (non-OpenAI format)
        let value = serde_json::json!({"name": "test_tool", "arguments": {}});
        let result = parse_tool_call_value(&value);
        assert!(result.is_some());
        assert_eq!(result.unwrap().name, "test_tool");
    }

    #[test]
    fn parse_tool_call_value_accepts_top_level_parameters_alias() {
        let value = serde_json::json!({
            "name": "schedule",
            "parameters": {"action": "create", "message": "test"}
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "schedule");
        assert_eq!(
            result.arguments.get("action").and_then(|v| v.as_str()),
            Some("create")
        );
    }

    #[test]
    fn parse_tool_call_value_accepts_function_parameters_alias() {
        let value = serde_json::json!({
            "function": {
                "name": "shell",
                "parameters": {"command": "date"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "shell");
        assert_eq!(
            result.arguments.get("command").and_then(|v| v.as_str()),
            Some("date")
        );
    }

    #[test]
    fn parse_tool_call_value_normalizes_task_plan_steps_without_action() {
        let value = serde_json::json!({
            "tool": "task_plan",
            "parameters": {
                "steps": [
                    {"step": 1, "description": "Write a file"},
                    {"step": 2, "description": "Read the file"},
                    {"step": 3, "description": "Delete the file"}
                ]
            }
        });
        let result = parse_tool_call_value(&value).expect("task plan call should parse");
        assert_eq!(result.name, "task_plan");
        assert_eq!(
            result.arguments.get("action").and_then(|v| v.as_str()),
            Some("create")
        );
        assert_eq!(
            result
                .arguments
                .get("tasks")
                .and_then(|v| v.as_array())
                .map(|tasks| tasks.len()),
            Some(3)
        );
    }

    #[test]
    fn parse_tool_call_value_recovers_shell_command_from_raw_string_arguments() {
        let value = serde_json::json!({
            "name": "shell",
            "arguments": "uname -a"
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "shell");
        assert_eq!(
            result.arguments.get("command").and_then(|v| v.as_str()),
            Some("uname -a")
        );
    }

    #[test]
    fn parse_tool_call_value_recovers_shell_command_from_cmd_alias() {
        let value = serde_json::json!({
            "function": {
                "name": "shell",
                "arguments": {"cmd": "pwd"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.name, "shell");
        assert_eq!(
            result.arguments.get("command").and_then(|v| v.as_str()),
            Some("pwd")
        );
    }

    #[test]
    fn parse_tool_call_value_preserves_tool_call_id_aliases() {
        let value = serde_json::json!({
            "call_id": "legacy_1",
            "function": {
                "name": "shell",
                "arguments": {"command": "date"}
            }
        });
        let result = parse_tool_call_value(&value).expect("tool call should parse");
        assert_eq!(result.tool_call_id.as_deref(), Some("legacy_1"));
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_empty_array() {
        // Recovery: Empty tool_calls array should return empty vec
        let value = serde_json::json!({"tool_calls": []});
        let result = parse_tool_calls_from_json_value(&value);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_missing_tool_calls() {
        // Recovery: Missing tool_calls field should fall through
        let value = serde_json::json!({"name": "test", "arguments": {}});
        let result = parse_tool_calls_from_json_value(&value);
        assert_eq!(result.len(), 1);
    }

    #[test]
    fn parse_tool_calls_from_json_value_handles_top_level_array() {
        // Recovery: Top-level array of tool calls
        let value = serde_json::json!([
            {"name": "tool_a", "arguments": {}},
            {"name": "tool_b", "arguments": {}}
        ]);
        let result = parse_tool_calls_from_json_value(&value);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn parse_structured_tool_calls_recovers_shell_command_from_string_payload() {
        let calls = vec![ToolCall {
            id: "call_1".to_string(),
            name: "shell".to_string(),
            arguments: "ls -la".to_string(),
        }];
        let parsed = parse_structured_tool_calls(&calls);
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].name, "shell");
        assert_eq!(
            parsed[0].arguments.get("command").and_then(|v| v.as_str()),
            Some("ls -la")
        );
    }

    // ═══════════════════════════════════════════════════════════════════════
    // GLM-Style Tool Call Parsing
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn parse_glm_style_browser_open_url() {
        let response = "browser_open/url>https://example.com";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert!(calls[0].1["command"].as_str().unwrap().contains("curl"));
        assert!(calls[0].1["command"]
            .as_str()
            .unwrap()
            .contains("example.com"));
    }

    #[test]
    fn parse_glm_style_shell_command() {
        let response = "shell/command>ls -la";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(calls[0].1["command"], "ls -la");
    }

    #[test]
    fn parse_glm_style_http_request() {
        let response = "http_request/url>https://api.example.com/data";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "http_request");
        assert_eq!(calls[0].1["url"], "https://api.example.com/data");
        assert_eq!(calls[0].1["method"], "GET");
    }

    #[test]
    fn parse_glm_style_plain_url() {
        let response = "https://example.com/api";
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert!(calls[0].1["command"].as_str().unwrap().contains("curl"));
    }

    #[test]
    fn parse_glm_style_json_args() {
        let response = r#"shell/{"command": "echo hello"}"#;
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "shell");
        assert_eq!(calls[0].1["command"], "echo hello");
    }

    #[test]
    fn parse_glm_style_multiple_calls() {
        let response = r#"shell/command>ls
browser_open/url>https://example.com"#;
        let calls = parse_glm_style_tool_calls(response);
        assert_eq!(calls.len(), 2);
    }

    #[test]
    fn parse_glm_style_tool_call_integration() {
        // Integration test: GLM format should be parsed in parse_tool_calls
        let response = "Checking...\nbrowser_open/url>https://example.com\nDone";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert!(text.contains("Checking"));
        assert!(text.contains("Done"));
    }

    #[test]
    fn parse_glm_style_rejects_non_http_url_param() {
        let response = "browser_open/url>javascript:alert(1)";
        let calls = parse_glm_style_tool_calls(response);
        assert!(calls.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_unclosed_tool_call_tag() {
        let response = "<tool_call>{\"name\":\"shell\",\"arguments\":{\"command\":\"pwd\"}}\nDone";
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "pwd");
        assert_eq!(text, "Done");
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG4 (inline): parse_tool_calls robustness — malformed/edge-case inputs
    // Prevents: Pattern 4 issues #746, #418, #777, #848
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn parse_tool_calls_empty_input_returns_empty() {
        let (text, calls) = parse_tool_calls("");
        assert!(calls.is_empty(), "empty input should produce no tool calls");
        assert!(text.is_empty(), "empty input should produce no text");
    }

    #[test]
    fn parse_tool_calls_whitespace_only_returns_empty_calls() {
        let (text, calls) = parse_tool_calls("   \n\t  ");
        assert!(calls.is_empty());
        assert!(text.is_empty() || text.trim().is_empty());
    }

    #[test]
    fn parse_tool_calls_nested_xml_tags_handled() {
        // Double-wrapped tool call should still parse the inner call
        let response = r#"<tool_call><tool_call>{"name":"echo","arguments":{"msg":"hi"}}</tool_call></tool_call>"#;
        let (_text, calls) = parse_tool_calls(response);
        // Should find at least one tool call
        assert!(
            !calls.is_empty(),
            "nested XML tags should still yield at least one tool call"
        );
    }

    #[test]
    fn parse_tool_calls_truncated_json_no_panic() {
        // Incomplete JSON inside tool_call tags
        let response = r#"<tool_call>{"name":"shell","arguments":{"command":"ls"</tool_call>"#;
        let (_text, _calls) = parse_tool_calls(response);
        // Should not panic — graceful handling of truncated JSON
    }

    #[test]
    fn parse_tool_calls_empty_json_object_in_tag() {
        let response = "<tool_call>{}</tool_call>";
        let (_text, calls) = parse_tool_calls(response);
        // Empty JSON object has no name field — should not produce valid tool call
        assert!(
            calls.is_empty(),
            "empty JSON object should not produce a tool call"
        );
    }

    #[test]
    fn parse_tool_calls_closing_tag_only_returns_text() {
        let response = "Some text </tool_call> more text";
        let (text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "closing tag only should not produce calls"
        );
        assert!(
            !text.is_empty(),
            "text around orphaned closing tag should be preserved"
        );
    }

    #[test]
    fn parse_tool_calls_very_large_arguments_no_panic() {
        let large_arg = "x".repeat(100_000);
        let response = format!(
            r#"<tool_call>{{"name":"echo","arguments":{{"message":"{}"}}}}</tool_call>"#,
            large_arg
        );
        let (_text, calls) = parse_tool_calls(&response);
        assert_eq!(calls.len(), 1, "large arguments should still parse");
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn parse_tool_calls_special_characters_in_arguments() {
        let response = r#"<tool_call>{"name":"echo","arguments":{"message":"hello \"world\" <>&'\n\t"}}</tool_call>"#;
        let (_text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "echo");
    }

    #[test]
    fn parse_tool_calls_text_with_embedded_json_not_extracted() {
        // Raw JSON without any tags should NOT be extracted as a tool call
        let response = r#"Here is some data: {"name":"echo","arguments":{"message":"hi"}} end."#;
        let (_text, calls) = parse_tool_calls(response);
        assert!(
            calls.is_empty(),
            "raw JSON in text without tags should not be extracted"
        );
    }

    #[test]
    fn parse_tool_calls_fenced_json_tool_call_with_preamble() {
        let response = r#"I'll search for the top news stories online for you.

```json
{"tool_name": "web_search_tool", "parameters": {"query": "top news stories today"}}
```"#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1, "should extract the fenced JSON tool call");
        assert_eq!(calls[0].name, "web_search_tool");
        assert_eq!(calls[0].arguments["query"], "top news stories today");
        assert!(
            text.contains("search for the top news stories online"),
            "surrounding explanatory text should be preserved"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_trailing_json_tool_call_after_preamble() {
        let response = r#"I'll use the web_search_tool to find the official Rust language website.
</think>

{
  "tool_name": "web_search_tool",
  "arguments": {
    "query": "official Rust language website main URL"
  }
}"#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(calls.len(), 1, "should extract the trailing json tool call");
        assert_eq!(calls[0].name, "web_search_tool");
        assert_eq!(
            calls[0].arguments["query"],
            "official Rust language website main URL"
        );
        assert!(
            !text.contains("</think>"),
            "trailing stray think close tag should be stripped from preserved text"
        );
        assert!(
            text.contains("official Rust language website"),
            "assistant preamble should still be preserved"
        );
    }

    #[test]
    fn parse_tool_calls_recovers_unclosed_attribute_style_web_tool_tag() {
        let response = r#"I'll search for the official Rust site now.
<web_search_tool query="official Rust language website">"#;
        let (text, calls) = parse_tool_calls(response);

        assert_eq!(
            calls.len(),
            1,
            "should recover attribute-style web tool call"
        );
        assert_eq!(calls[0].name, "web_search_tool");
        assert_eq!(
            calls[0].arguments["query"],
            "official Rust language website"
        );
        assert!(text.contains("official Rust site"));
    }

    #[test]
    fn parse_tool_calls_markdown_named_tool_call_with_parameters_line() {
        let response = r#"I'll search for today's top news stories online.

**web_search_tool**: Search for top news stories today
Parameters: `{"properties":{"query":"top news stories today 2026-03-16","search_depth":"medium","search_type":"web_search"},"required":["query"],"type":"object"}`"#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "should extract the markdown-style tool call"
        );
        assert_eq!(calls[0].name, "web_search_tool");
        assert_eq!(
            calls[0].arguments["query"],
            "top news stories today 2026-03-16"
        );
        assert_eq!(calls[0].arguments["search_depth"], "medium");
        assert_eq!(calls[0].arguments["search_type"], "web_search");
        assert!(
            text.contains("I'll search for today's top news stories online."),
            "surrounding explanatory text should be preserved"
        );
    }

    #[test]
    fn parse_tool_calls_meta_wrapped_json_tool_call() {
        let response = r#"I'll search online for information about current Ryzen CPUs.

<tool_code>
{"name": "web_search_tool", "parameters": {"query": "current Ryzen CPUs 2024 2025 latest AMD processors"}}
</tool_code>"#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "should extract the meta-wrapped JSON tool call"
        );
        assert_eq!(calls[0].name, "web_search_tool");
        assert_eq!(
            calls[0].arguments["query"],
            "current Ryzen CPUs 2024 2025 latest AMD processors"
        );
        assert!(
            text.contains("I'll search online for information about current Ryzen CPUs."),
            "surrounding explanatory text should be preserved"
        );
    }

    #[test]
    fn parse_tool_calls_multiple_formats_mixed() {
        // Mix of text and properly tagged tool call
        let response = r#"I'll help you with that.

<tool_call>
{"name":"shell","arguments":{"command":"echo hello"}}
</tool_call>

Let me check the result."#;
        let (text, calls) = parse_tool_calls(response);
        assert_eq!(
            calls.len(),
            1,
            "should extract one tool call from mixed content"
        );
        assert_eq!(calls[0].name, "shell");
        assert!(
            text.contains("help you"),
            "text before tool call should be preserved"
        );
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG4 (inline): scrub_credentials edge cases
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn scrub_credentials_empty_input() {
        let result = scrub_credentials("");
        assert_eq!(result, "");
    }

    #[test]
    fn scrub_credentials_no_sensitive_data() {
        let input = "normal text without any secrets";
        let result = scrub_credentials(input);
        assert_eq!(
            result, input,
            "non-sensitive text should pass through unchanged"
        );
    }

    #[test]
    fn scrub_credentials_short_values_not_redacted() {
        // Values shorter than 8 chars should not be redacted
        let input = r#"api_key="short""#;
        let result = scrub_credentials(input);
        assert_eq!(result, input, "short values should not be redacted");
    }

    // ─────────────────────────────────────────────────────────────────────
    // TG4 (inline): trim_history edge cases
    // ─────────────────────────────────────────────────────────────────────

    #[test]
    fn trim_history_empty_history() {
        let mut history: Vec<crate::providers::ChatMessage> = vec![];
        trim_history(&mut history, 10);
        assert!(history.is_empty());
    }

    #[test]
    fn trim_history_system_only() {
        let mut history = vec![crate::providers::ChatMessage::system("system prompt")];
        trim_history(&mut history, 10);
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, "system");
    }

    #[test]
    fn trim_history_exactly_at_limit() {
        let mut history = vec![
            crate::providers::ChatMessage::system("system"),
            crate::providers::ChatMessage::user("msg 1"),
            crate::providers::ChatMessage::assistant("reply 1"),
        ];
        trim_history(&mut history, 2); // 2 non-system messages = exactly at limit
        assert_eq!(history.len(), 3, "should not trim when exactly at limit");
    }

    #[test]
    fn trim_history_removes_oldest_non_system() {
        let mut history = vec![
            crate::providers::ChatMessage::system("system"),
            crate::providers::ChatMessage::user("old msg"),
            crate::providers::ChatMessage::assistant("old reply"),
            crate::providers::ChatMessage::user("new msg"),
            crate::providers::ChatMessage::assistant("new reply"),
        ];
        trim_history(&mut history, 2);
        assert_eq!(history.len(), 3); // system + 2 kept
        assert_eq!(history[0].role, "system");
        assert_eq!(history[1].content, "new msg");
    }

    /// When `build_system_prompt_with_mode` is called with `native_tools = true`,
    /// the output must contain ZERO XML protocol artifacts. In the native path
    /// `build_tool_instructions` is never called, so the system prompt alone
    /// must be clean of XML tool-call protocol.
    #[test]
    fn native_tools_system_prompt_contains_zero_xml() {
        use crate::channels::build_system_prompt_with_mode;

        let tool_summaries: Vec<(&str, &str)> = vec![
            ("shell", "Execute shell commands"),
            ("file_read", "Read files"),
        ];

        let system_prompt = build_system_prompt_with_mode(
            std::path::Path::new("/tmp"),
            "test-model",
            &tool_summaries,
            &[],  // no skills
            None, // no identity config
            None, // no bootstrap_max_chars
            true, // native_tools
            crate::config::SkillsPromptInjectionMode::Full,
        );

        // Must contain zero XML protocol artifacts
        assert!(
            !system_prompt.contains("<tool_call>"),
            "Native prompt must not contain <tool_call>"
        );
        assert!(
            !system_prompt.contains("</tool_call>"),
            "Native prompt must not contain </tool_call>"
        );
        assert!(
            !system_prompt.contains("<tool_result>"),
            "Native prompt must not contain <tool_result>"
        );
        assert!(
            !system_prompt.contains("</tool_result>"),
            "Native prompt must not contain </tool_result>"
        );
        assert!(
            !system_prompt.contains("## Tool Use Protocol"),
            "Native prompt must not contain XML protocol header"
        );

        // Positive: native prompt should still list tools and contain task instructions
        assert!(
            system_prompt.contains("shell"),
            "Native prompt must list tool names"
        );
        assert!(
            system_prompt.contains("## Your Task"),
            "Native prompt should contain task instructions"
        );
    }

    // ── Cross-Alias & GLM Shortened Body Tests ──────────────────────────

    #[test]
    fn parse_tool_calls_cross_alias_close_tag_with_json() {
        // <tool_call> opened but closed with </invoke> — JSON body
        let input = r#"<tool_call>{"name": "shell", "arguments": {"command": "ls"}}</invoke>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_cross_alias_close_tag_with_glm_shortened() {
        // <tool_call>shell>uname -a</invoke> — GLM shortened inside cross-alias tags
        let input = "<tool_call>shell>uname -a</invoke>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "uname -a");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_glm_shortened_body_in_matched_tags() {
        // <tool_call>shell>pwd</tool_call> — GLM shortened in matched tags
        let input = "<tool_call>shell>pwd</tool_call>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "pwd");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_glm_yaml_style_in_tags() {
        // <tool_call>shell>\ncommand: date\napproved: true</invoke>
        let input = "<tool_call>shell>\ncommand: date\napproved: true</invoke>";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "date");
        assert_eq!(calls[0].arguments["approved"], true);
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_attribute_style_in_tags() {
        // <tool_call>shell command="date" /></tool_call>
        let input = r#"<tool_call>shell command="date" /></tool_call>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "date");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_handles_generic_self_closing_tool_tag() {
        let input = r#"<tool name="file_read" parameters='{"path":"AGENTS.md"}'/>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments["path"], "AGENTS.md");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_file_read_shortened_in_cross_alias() {
        // <tool_call>file_read path=".env" /></invoke>
        let input = r#"<tool_call>file_read path=".env" /></invoke>"#;
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "file_read");
        assert_eq!(calls[0].arguments["path"], ".env");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_unclosed_glm_shortened_no_close_tag() {
        // <tool_call>shell>ls -la (no close tag at all)
        let input = "<tool_call>shell>ls -la";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "ls -la");
        assert!(text.is_empty());
    }

    #[test]
    fn parse_tool_calls_text_before_cross_alias() {
        // Text before and after cross-alias tool call
        let input = "Let me check that.\n<tool_call>shell>uname -a</invoke>\nDone.";
        let (text, calls) = parse_tool_calls(input);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments["command"], "uname -a");
        assert!(text.contains("Let me check that."));
        assert!(text.contains("Done."));
    }

    #[test]
    fn parse_glm_shortened_body_url_to_curl() {
        // URL values for shell should be wrapped in curl
        let call = parse_glm_shortened_body("shell>https://example.com/api").unwrap();
        assert_eq!(call.name, "shell");
        let cmd = call.arguments["command"].as_str().unwrap();
        assert!(cmd.contains("curl"));
        assert!(cmd.contains("example.com"));
    }

    #[test]
    fn parse_glm_shortened_body_browser_open_maps_to_shell_command() {
        // browser_open aliases to shell, and shortened calls must still emit
        // shell's canonical "command" argument.
        let call = parse_glm_shortened_body("browser_open>https://example.com").unwrap();
        assert_eq!(call.name, "shell");
        let cmd = call.arguments["command"].as_str().unwrap();
        assert!(cmd.contains("curl"));
        assert!(cmd.contains("example.com"));
    }

    #[test]
    fn parse_glm_shortened_body_memory_recall() {
        // memory_recall>some query — default param is "query"
        let call = parse_glm_shortened_body("memory_recall>recent meetings").unwrap();
        assert_eq!(call.name, "memory_recall");
        assert_eq!(call.arguments["query"], "recent meetings");
    }

    #[test]
    fn parse_glm_shortened_body_function_style_alias_maps_to_message_send() {
        let call =
            parse_glm_shortened_body(r#"sendmessage(channel="alerts", message="hi")"#).unwrap();
        assert_eq!(call.name, "message_send");
        assert_eq!(call.arguments["channel"], "alerts");
        assert_eq!(call.arguments["message"], "hi");
    }

    #[test]
    fn map_tool_name_alias_direct_coverage() {
        assert_eq!(map_tool_name_alias("bash"), "shell");
        assert_eq!(map_tool_name_alias("filelist"), "glob_search");
        assert_eq!(map_tool_name_alias("edit_file"), "file_edit");
        assert_eq!(map_tool_name_alias("web_search"), "web_search_tool");
        assert_eq!(map_tool_name_alias("memorystore"), "memory_store");
        assert_eq!(map_tool_name_alias("memoryforget"), "memory_forget");
        assert_eq!(map_tool_name_alias("http"), "http_request");
        assert_eq!(
            map_tool_name_alias("totally_unknown_tool"),
            "totally_unknown_tool"
        );
    }

    #[test]
    fn default_param_for_tool_coverage() {
        assert_eq!(default_param_for_tool("shell"), "command");
        assert_eq!(default_param_for_tool("bash"), "command");
        assert_eq!(default_param_for_tool("file_read"), "path");
        assert_eq!(default_param_for_tool("glob_search"), "pattern");
        assert_eq!(default_param_for_tool("memory_recall"), "query");
        assert_eq!(default_param_for_tool("memory_store"), "content");
        assert_eq!(default_param_for_tool("http_request"), "url");
        assert_eq!(default_param_for_tool("browser_open"), "url");
        assert_eq!(default_param_for_tool("web_search_tool"), "query");
        assert_eq!(default_param_for_tool("unknown_tool"), "input");
    }

    #[test]
    fn parse_glm_shortened_body_rejects_empty() {
        assert!(parse_glm_shortened_body("").is_none());
        assert!(parse_glm_shortened_body("   ").is_none());
    }

    #[test]
    fn parse_glm_shortened_body_rejects_invalid_tool_name() {
        // Tool names with special characters should be rejected
        assert!(parse_glm_shortened_body("not-a-tool>value").is_none());
        assert!(parse_glm_shortened_body("tool name>value").is_none());
    }

    // ═══════════════════════════════════════════════════════════════════════
    // reasoning_content pass-through tests for history builders
    // ═══════════════════════════════════════════════════════════════════════

    #[test]
    fn build_native_assistant_history_includes_reasoning_content() {
        let calls = vec![ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        }];
        let result = build_native_assistant_history("answer", &calls, Some("thinking step"));
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert_eq!(parsed["reasoning_content"].as_str(), Some("thinking step"));
        assert!(parsed["tool_calls"].is_array());
    }

    #[test]
    fn build_native_assistant_history_omits_reasoning_content_when_none() {
        let calls = vec![ToolCall {
            id: "call_1".into(),
            name: "shell".into(),
            arguments: "{}".into(),
        }];
        let result = build_native_assistant_history("answer", &calls, None);
        let parsed: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert!(parsed.get("reasoning_content").is_none());
    }

    #[test]
    fn build_native_assistant_history_from_parsed_calls_includes_reasoning_content() {
        let calls = vec![ParsedToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({"command": "pwd"}),
            tool_call_id: Some("call_2".into()),
        }];
        let result = build_native_assistant_history_from_parsed_calls(
            "answer",
            &calls,
            Some("deep thought"),
        );
        assert!(result.is_some());
        let parsed: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert_eq!(parsed["reasoning_content"].as_str(), Some("deep thought"));
        assert!(parsed["tool_calls"].is_array());
    }

    #[test]
    fn build_native_assistant_history_from_parsed_calls_omits_reasoning_content_when_none() {
        let calls = vec![ParsedToolCall {
            name: "shell".into(),
            arguments: serde_json::json!({"command": "pwd"}),
            tool_call_id: Some("call_2".into()),
        }];
        let result = build_native_assistant_history_from_parsed_calls("answer", &calls, None);
        assert!(result.is_some());
        let parsed: serde_json::Value = serde_json::from_str(result.as_deref().unwrap()).unwrap();
        assert_eq!(parsed["content"].as_str(), Some("answer"));
        assert!(parsed.get("reasoning_content").is_none());
    }
}
