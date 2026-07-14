//! REST API handlers for the web dashboard.
//!
//! All `/api/*` routes require bearer token authentication (PairingGuard).

use super::{
    build_gateway_runtime_snapshot_with_federation, client_key_from_request, AppState,
};
use crate::config::FederationRole;
use crate::federation::peer_registry::{
    FederationCapabilities, FederationLocalNodeSummary, FederationPeersResponse,
};
use crate::federation::remote_subagent::{
    FederationTaskAccepted, FederationTaskEvent, FederationTaskManager, FederationTaskRequest,
};
use axum::{
    body::{Body, Bytes},
    extract::{ConnectInfo, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Json, Response,
    },
};
use chrono::{DateTime, Utc};
use futures_util::stream;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    convert::Infallible,
    net::SocketAddr,
    path::{Component, Path as FsPath, PathBuf},
    sync::Arc,
};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MASKED_SECRET: &str = "***MASKED***";
const WORKSPACE_EDITOR_FILES: &[&str] = &["AGENTS.md", "SOUL.md"];
const GOD_CONFIG_PRESET_FILE: &str = "config.template.toml";
const SAFE_CONFIG_PRESET_FILE: &str = "config.preset.safe.toml";
const GOD_WORKSPACE_AGENTS_PRESET_FILE: &str = "workspace.preset.god.AGENTS.md";
const GOD_WORKSPACE_SOUL_PRESET_FILE: &str = "workspace.preset.god.SOUL.md";
const SAFE_WORKSPACE_AGENTS_PRESET_FILE: &str = "workspace.preset.safe.AGENTS.md";
const SAFE_WORKSPACE_SOUL_PRESET_FILE: &str = "workspace.preset.safe.SOUL.md";

// ── Bearer token auth extractor ─────────────────────────────────

/// Extract and validate bearer token from Authorization header.
fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
}

/// Verify bearer token against PairingGuard. Returns error response if unauthorized.
fn require_auth(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !state.pairing.require_pairing() {
        return Ok(());
    }

    let token = extract_bearer_token(headers).unwrap_or("");
    if state.pairing.is_authenticated(token) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
            })),
        ))
    }
}

fn federation_disabled_response() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "Federation is disabled on this node"
        })),
    )
        .into_response()
}

fn require_federation_peer_auth(
    state: &AppState,
    headers: &HeaderMap,
    peer_addr: SocketAddr,
) -> Result<(), Response> {
    if state.federation.is_none() {
        return Err(federation_disabled_response());
    }

    let client = client_key_from_request(
        Some(peer_addr),
        headers,
        state.trust_forwarded_headers,
    );
    if crate::tools::url_validation::is_private_or_local_host(&client) {
        Ok(())
    } else {
        Err((
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "Federation peer API only accepts trusted local/LAN callers"
            })),
        )
            .into_response())
    }
}

fn build_federation_peers_response(state: &AppState) -> FederationPeersResponse {
    let config = state.config.lock().clone();
    if let Some(federation) = &state.federation {
        let mut response = federation.peers_response();
        response.local_node.gateway_host = config.gateway.host;
        response
    } else {
        FederationPeersResponse {
            enabled: false,
            local_node: FederationLocalNodeSummary {
                node_id: config.federation.node_name.clone(),
                display_name: config.federation.node_name,
                api_port: config.federation.api_port.unwrap_or(config.gateway.port),
                role: config.federation.default_role,
                allow_remote_subagents: config.federation.allow_remote_subagents,
                discovery_mode: config.federation.discovery_mode,
                service_name: config.federation.service_name,
                gateway_host: config.gateway.host,
            },
            peers: Vec::new(),
        }
    }
}

fn parse_memory_category(raw: &str) -> crate::memory::MemoryCategory {
    match raw {
        "core" => crate::memory::MemoryCategory::Core,
        "daily" => crate::memory::MemoryCategory::Daily,
        "conversation" => crate::memory::MemoryCategory::Conversation,
        other => crate::memory::MemoryCategory::Custom(other.to_string()),
    }
}

fn parse_cron_schedule(
    schedule_kind: Option<&str>,
    schedule: Option<&str>,
    run_at: Option<&str>,
    every_ms: Option<u64>,
) -> Result<crate::cron::Schedule, String> {
    match schedule_kind
        .unwrap_or("cron")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "cron" => {
            let expr = schedule
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "schedule is required for cron jobs".to_string())?;
            Ok(crate::cron::Schedule::Cron {
                expr: expr.to_string(),
                tz: None,
            })
        }
        "at" => {
            let raw = run_at
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| "run_at is required for one-time jobs".to_string())?;
            let at = chrono::DateTime::parse_from_rfc3339(raw)
                .map_err(|error| format!("run_at must be RFC3339: {error}"))?
                .with_timezone(&Utc);
            Ok(crate::cron::Schedule::At { at })
        }
        "every" => {
            let every_ms =
                every_ms.ok_or_else(|| "every_ms is required for interval jobs".to_string())?;
            if every_ms == 0 {
                return Err("every_ms must be greater than 0".to_string());
            }
            Ok(crate::cron::Schedule::Every { every_ms })
        }
        other => Err(format!(
            "unsupported schedule_kind '{other}' (expected cron, at, or every)"
        )),
    }
}

fn cron_job_json(job: &crate::cron::CronJob) -> serde_json::Value {
    serde_json::json!({
        "id": job.id,
        "name": job.name,
        "command": job.command,
        "expression": job.expression,
        "schedule": job.schedule,
        "next_run": job.next_run.to_rfc3339(),
        "last_run": job.last_run.map(|t| t.to_rfc3339()),
        "last_status": job.last_status,
        "last_output": job.last_output,
        "enabled": job.enabled,
    })
}

fn federation_event_is_terminal(event: &FederationTaskEvent) -> bool {
    matches!(event.event_type.as_str(), "done" | "error")
}

fn serialize_federation_sse_event(
    event: &FederationTaskEvent,
) -> Result<Event, Infallible> {
    Ok(Event::default().data(serde_json::to_string(event).unwrap_or_else(
        |_| {
            serde_json::json!({
                "type": "error",
                "task_id": event.task_id,
                "timestamp": Utc::now().to_rfc3339(),
                "message": "Failed to serialize federation event",
            })
            .to_string()
        },
    )))
}

fn build_federation_user_prompt(request: &FederationTaskRequest) -> String {
    let prompt = request.prompt.trim();
    let context = request
        .context
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let requester = request
        .requester_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| {
            request
                .requester_node_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or("federation-peer");

    match context {
        Some(context) => format!(
            "[Requester]\n{requester}\n\n[Context]\n{context}\n\n[Task]\n{prompt}"
        ),
        None => format!("[Requester]\n{requester}\n\n[Task]\n{prompt}"),
    }
}

fn split_federation_tool_progress_payload(raw: &str) -> (&str, Option<&str>) {
    let trimmed = raw.trim_end();
    match trimmed.split_once('\n') {
        Some((header, output)) => (header.trim(), Some(output)),
        None => (trimmed.trim(), None),
    }
}

fn parse_federation_tool_completion_payload(raw: &str) -> Option<(String, Option<u64>)> {
    let trimmed = raw.trim();
    let (name_part, duration_part) = trimmed.rsplit_once(" (")?;
    let duration_part = duration_part.strip_suffix(')')?;
    let secs = duration_part.strip_suffix('s')?.parse::<u64>().ok();
    Some((name_part.trim().to_string(), secs))
}

fn federation_event_from_delta(task_id: &str, delta: &str) -> Option<FederationTaskEvent> {
    if delta == crate::agent::loop_::DRAFT_CLEAR_SENTINEL {
        return None;
    }

    if let Some(progress) = delta.strip_prefix(crate::agent::loop_::DRAFT_PROGRESS_SENTINEL) {
        let progress = progress.trim();
        if let Some(rest) = progress.strip_prefix("⏳ ") {
            let rest = rest.trim();
            if rest.is_empty() {
                return None;
            }
            let (name, hint) = match rest.split_once(": ") {
                Some((name, hint)) => (
                    name.trim().to_string(),
                    (!hint.trim().is_empty()).then(|| hint.trim().to_string()),
                ),
                None => (rest.to_string(), None),
            };
            return Some(FederationTaskEvent {
                event_type: "tool_call".to_string(),
                task_id: task_id.to_string(),
                timestamp: Utc::now().to_rfc3339(),
                content: None,
                full_response: None,
                name: Some(name),
                args: Some(serde_json::json!({ "hint": hint })),
                success: None,
                duration_secs: None,
                output: None,
                message: None,
            });
        }

        if let Some(rest) = progress.strip_prefix("✅ ") {
            let (header, output) = split_federation_tool_progress_payload(rest);
            if let Some((name, duration_secs)) =
                parse_federation_tool_completion_payload(header)
            {
                return Some(FederationTaskEvent {
                    event_type: "tool_result".to_string(),
                    task_id: task_id.to_string(),
                    timestamp: Utc::now().to_rfc3339(),
                    content: None,
                    full_response: None,
                    name: Some(name),
                    args: None,
                    success: Some(true),
                    duration_secs,
                    output: Some(output.unwrap_or("(no output)").to_string()),
                    message: None,
                });
            }
        }

        if let Some(rest) = progress.strip_prefix("❌ ") {
            let (header, output) = split_federation_tool_progress_payload(rest);
            if let Some((name, duration_secs)) =
                parse_federation_tool_completion_payload(header)
            {
                return Some(FederationTaskEvent {
                    event_type: "tool_result".to_string(),
                    task_id: task_id.to_string(),
                    timestamp: Utc::now().to_rfc3339(),
                    content: None,
                    full_response: None,
                    name: Some(name),
                    args: None,
                    success: Some(false),
                    duration_secs,
                    output: Some(output.unwrap_or("(no output)").to_string()),
                    message: None,
                });
            }
        }

        return None;
    }

    (!delta.is_empty()).then(|| FederationTaskEvent {
        event_type: "chunk".to_string(),
        task_id: task_id.to_string(),
        timestamp: Utc::now().to_rfc3339(),
        content: Some(delta.to_string()),
        full_response: None,
        name: None,
        args: None,
        success: None,
        duration_secs: None,
        output: None,
        message: None,
    })
}

async fn build_federation_capabilities(
    state: &AppState,
) -> anyhow::Result<FederationCapabilities> {
    let federation = state
        .federation
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("Federation is disabled"))?;
    let local = federation.local_node_summary();
    let runtime = state.runtime_snapshot();
    let config = state.config.lock().clone();
    let ollama = fetch_ollama_dashboard_info(&config).await;
    let mut installed_models = ollama.installed_models;
    if installed_models.is_empty() {
        installed_models.push(runtime.model.clone());
    }
    installed_models.sort();
    installed_models.dedup();

    Ok(FederationCapabilities {
        node_id: local.node_id,
        display_name: local.display_name,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        provider: config.default_provider.clone(),
        model: runtime.model.clone(),
        installed_models,
        tools: crate::federation::tools_to_capabilities(runtime.tools_registry.as_ref()),
        role_support: local.role,
        allow_remote_subagents: local.allow_remote_subagents,
        health: "online".to_string(),
        api_port: local.api_port,
        last_seen: Utc::now().to_rfc3339(),
    })
}

async fn execute_federation_task(
    state: AppState,
    task_manager: Arc<FederationTaskManager>,
    task_id: String,
    request: FederationTaskRequest,
    cancellation_token: CancellationToken,
) {
    task_manager.publish(
        &task_id,
        FederationTaskEvent::status(&task_id, "Task accepted by remote worker"),
    );

    let runtime = state.runtime_snapshot();
    // Warm per-model capability cache before taking the config lock (guard is !Send).
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
        max_tool_iterations,
        max_history_messages,
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

        (
            provider_label,
            config_guard.agent.parallel_tools,
            native_tools,
            crate::approval::ApprovalManager::from_config(&config_guard.autonomy),
            system_prompt,
            config_guard.agent.max_tool_iterations,
            config_guard.agent.max_history_messages,
        )
    };

    let user_prompt = build_federation_user_prompt(&request);
    let mut history = vec![
        crate::providers::ChatMessage::system(&system_prompt),
        crate::providers::ChatMessage::user(&user_prompt),
    ];

    let loop_result = crate::agent::loop_::with_tool_loop_settings(
        parallel_tools,
        native_tools,
        crate::agent::loop_::with_tool_loop_history_limit(
            max_history_messages,
            async {
                let (delta_tx, mut delta_rx) = tokio::sync::mpsc::channel::<String>(128);
                let mut loop_future = std::pin::pin!(crate::agent::loop_::run_tool_call_loop(
                    runtime.provider.as_ref(),
                    &mut history,
                    runtime.tools_registry_exec.as_ref(),
                    state.observer.as_ref(),
                    &provider_label,
                    &runtime.model,
                    runtime.temperature,
                    true,
                    Some(&approval_manager),
                    "federation",
                    &state.multimodal,
                    request.max_iterations.max(max_tool_iterations),
                    Some(cancellation_token.clone()),
                    Some(delta_tx),
                    None,
                    &[],
                ));

                loop {
                    tokio::select! {
                        _ = cancellation_token.cancelled() => {
                            anyhow::bail!("Federation task cancelled");
                        }
                        maybe_delta = delta_rx.recv() => {
                            if let Some(delta) = maybe_delta {
                                if let Some(event) = federation_event_from_delta(&task_id, &delta) {
                                    task_manager.publish(&task_id, event);
                                }
                            } else {
                                break loop_future.await;
                            }
                        }
                        response = &mut loop_future => {
                            while let Ok(delta) = delta_rx.try_recv() {
                                if let Some(event) = federation_event_from_delta(&task_id, &delta) {
                                    task_manager.publish(&task_id, event);
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

    match loop_result {
        Ok(response) => {
            let rendered =
                super::sanitize_gateway_response(&response, runtime.tools_registry_exec.as_ref());
            let final_response = if rendered.trim().is_empty() {
                "Tool execution completed, but no final response text was returned.".to_string()
            } else {
                rendered
            };
            task_manager.publish(&task_id, FederationTaskEvent::done(&task_id, final_response));
        }
        Err(error) => {
            task_manager.publish(
                &task_id,
                FederationTaskEvent::error(
                    &task_id,
                    crate::providers::sanitize_api_error(&error.to_string()),
                ),
            );
        }
    }
}

// ── Query parameters ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct MemoryQuery {
    pub query: Option<String>,
    pub category: Option<String>,
}

#[derive(Deserialize)]
pub struct MemoryStoreBody {
    pub key: String,
    pub content: String,
    pub category: Option<String>,
}

#[derive(Deserialize)]
pub struct CronAddBody {
    pub name: Option<String>,
    pub schedule_kind: Option<String>,
    pub schedule: Option<String>,
    pub run_at: Option<String>,
    pub every_ms: Option<u64>,
    pub command: String,
    pub enabled: Option<bool>,
}

#[derive(Deserialize)]
pub struct CronUpdateBody {
    pub name: Option<String>,
    pub schedule_kind: Option<String>,
    pub schedule: Option<String>,
    pub run_at: Option<String>,
    pub every_ms: Option<u64>,
    pub command: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Deserialize)]
pub struct MemoryClearBody {
    pub scope: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct FederationPeerRoleUpdateBody {
    pub role: FederationRole,
}

#[derive(Debug, Deserialize)]
pub struct FederationPeerHintsBody {
    pub specialization: String,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Deserialize)]
pub struct FederationAddManualPeerBody {
    pub endpoint: String,
}

#[derive(Deserialize)]
pub struct IntegrationCredentialsUpdateBody {
    pub revision: Option<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
struct IntegrationCredentialsField {
    key: String,
    label: String,
    required: bool,
    has_value: bool,
    input_type: &'static str,
    #[serde(default)]
    options: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    current_value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    masked_value: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct IntegrationSettingsEntry {
    id: String,
    name: String,
    description: String,
    category: crate::integrations::IntegrationCategory,
    status: crate::integrations::IntegrationStatus,
    configured: bool,
    activates_default_provider: bool,
    fields: Vec<IntegrationCredentialsField>,
}

#[derive(Debug, Clone, Serialize)]
struct IntegrationSettingsPayload {
    revision: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    active_default_provider_integration_id: Option<String>,
    integrations: Vec<IntegrationSettingsEntry>,
}

const OLLAMA_INTEGRATION_ID: &str = "ollama";
const OLLAMA_INTEGRATION_NAME: &str = "Ollama";
const OLLAMA_FALLBACK_MODELS: &[&str] = &["qwen3.5:9b", "devstral-small-2:latest", "llama3.2"];

#[derive(Debug, Default, Clone)]
struct OllamaDashboardInfo {
    endpoint: String,
    reachable: bool,
    installed_models: Vec<String>,
    loaded_models: Vec<String>,
    active_model_loaded: bool,
}

#[derive(Debug, Deserialize)]
struct OllamaModelListResponse {
    #[serde(default)]
    models: Vec<OllamaModelListEntry>,
}

#[derive(Debug, Deserialize)]
struct OllamaModelListEntry {
    name: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkspaceFileUpdateBody {
    pub content: String,
}

#[derive(Debug, Deserialize)]
pub struct WorkspacePathQuery {
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigPresetEntry {
    id: &'static str,
    label: &'static str,
    summary: &'static str,
    highlights: Vec<&'static str>,
    content: String,
    workspace_files: Vec<ConfigPresetWorkspaceFile>,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigPresetsPayload {
    safe: ConfigPresetEntry,
    god: ConfigPresetEntry,
}

#[derive(Debug, Clone, Serialize)]
struct ConfigPresetWorkspaceFile {
    name: &'static str,
    content: String,
}

#[derive(Debug, Serialize)]
struct WorkspaceFilePayload {
    name: String,
    content: String,
    exists: bool,
}

#[derive(Debug, Clone, Serialize)]
struct WorkspaceBrowserEntry {
    name: String,
    path: String,
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    size_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkspaceBrowserPayload {
    root_path: String,
    current_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_path: Option<String>,
    entries: Vec<WorkspaceBrowserEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct WorkspaceBlobWritePayload {
    status: &'static str,
    path: String,
    size_bytes: u64,
}

#[derive(Debug, Clone, Serialize)]
pub(super) struct WorkspacePathMutationPayload {
    pub(super) status: &'static str,
    pub(super) path: String,
    pub(super) kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
struct RuntimeShellInfo {
    path: String,
    name: String,
    available: bool,
}

#[derive(Debug, Default, Clone, Serialize)]
struct OllamaUnloadReport {
    endpoint: String,
    reachable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    keep_model: Option<String>,
    attempted: Vec<String>,
    unloaded: Vec<String>,
    failed: Vec<String>,
}

fn has_non_empty(value: Option<&str>) -> bool {
    value.is_some_and(|candidate| !candidate.trim().is_empty())
}

fn config_preset_path_candidates(file_name: &str) -> [String; 2] {
    [
        format!("/usr/share/llamafarm/{file_name}"),
        format!("{}/dev/{file_name}", env!("CARGO_MANIFEST_DIR")),
    ]
}

fn load_preset_file(file_name: &str) -> Result<String, String> {
    let mut last_error = None;

    for candidate in config_preset_path_candidates(file_name) {
        match std::fs::read_to_string(&candidate) {
            Ok(content) => return Ok(content),
            Err(error) => last_error = Some(format!("{candidate}: {error}")),
        }
    }

    Err(last_error.unwrap_or_else(|| format!("missing preset file {file_name}")))
}

fn config_revision(config: &crate::config::Config) -> String {
    let serialized = toml::to_string(config).unwrap_or_default();
    let digest = Sha256::digest(serialized.as_bytes());
    format!("{digest:x}")
}

fn normalize_ollama_base_url(config: &crate::config::Config) -> String {
    let raw = config
        .api_url
        .as_deref()
        .unwrap_or("http://localhost:11434");
    let trimmed = raw.trim().trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/api").unwrap_or(trimmed);
    if trimmed.is_empty() {
        "http://localhost:11434".to_string()
    } else {
        trimmed.to_string()
    }
}

fn ollama_auth_token(config: &crate::config::Config) -> Option<String> {
    config
        .api_key
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn default_runtime_shell() -> Option<(String, String)> {
    #[cfg(target_os = "windows")]
    let absolute_candidates = ["C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"];
    #[cfg(not(target_os = "windows"))]
    let absolute_candidates = ["/bin/bash", "/usr/bin/bash", "/bin/sh", "/usr/bin/sh"];

    for candidate in absolute_candidates {
        let path = FsPath::new(candidate);
        if path.is_file() {
            let name = path
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or(candidate)
                .to_string();
            return Some((candidate.to_string(), name));
        }
    }

    #[cfg(target_os = "windows")]
    let cli_candidates = ["pwsh", "powershell", "cmd"];
    #[cfg(not(target_os = "windows"))]
    let cli_candidates = ["bash", "sh"];

    for candidate in cli_candidates {
        if let Some(cli) = crate::tools::cli_discovery::probe_cli_command(candidate) {
            return Some((cli.path.to_string_lossy().into_owned(), cli.name));
        }
    }

    None
}

fn build_runtime_shell_info() -> RuntimeShellInfo {
    let env_shell = std::env::var("SHELL")
        .ok()
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let (path, name) = if env_shell.is_empty() {
        default_runtime_shell().unwrap_or_default()
    } else {
        let name = FsPath::new(&env_shell)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or(env_shell.as_str())
            .to_string();
        (env_shell, name)
    };
    let available = if path.is_empty() {
        false
    } else if FsPath::new(&path).is_absolute() {
        FsPath::new(&path).is_file()
    } else {
        crate::tools::cli_discovery::probe_cli_command(&name).is_some()
    };

    RuntimeShellInfo {
        path,
        name,
        available,
    }
}

fn normalize_workspace_editor_name(name: &str) -> Option<&'static str> {
    match name.trim() {
        "AGENTS.md" => Some("AGENTS.md"),
        "SOUL.md" => Some("SOUL.md"),
        _ => None,
    }
}

fn workspace_editor_path(
    config: &crate::config::Config,
    name: &str,
) -> Result<std::path::PathBuf, String> {
    let normalized = normalize_workspace_editor_name(name)
        .ok_or_else(|| format!("Unsupported workspace file: {name}"))?;
    Ok(config.workspace_dir.join(normalized))
}

fn normalize_workspace_relative_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed == "." {
        return Ok(String::new());
    }

    let candidate = PathBuf::from(trimmed);
    if candidate.is_absolute() {
        return Err("Workspace paths must be relative to the workspace root".to_string());
    }

    let mut normalized = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                return Err("Workspace paths may not contain '..'".to_string());
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err("Workspace paths must be relative to the workspace root".to_string());
            }
        }
    }

    Ok(normalized
        .to_string_lossy()
        .replace('\\', "/")
        .trim_matches('/')
        .to_string())
}

fn resolve_workspace_path(
    config: &crate::config::Config,
    raw: Option<&str>,
) -> Result<(PathBuf, String), String> {
    let relative = normalize_workspace_relative_path(raw.unwrap_or_default())?;
    let path = if relative.is_empty() {
        config.workspace_dir.clone()
    } else {
        config.workspace_dir.join(&relative)
    };
    Ok((path, relative))
}

fn workspace_parent_path(relative: &str) -> Option<String> {
    if relative.is_empty() {
        return None;
    }

    let parent = FsPath::new(relative).parent()?;
    let parent = parent.to_string_lossy().replace('\\', "/");
    if parent.is_empty() || parent == "." {
        Some(String::new())
    } else {
        Some(parent)
    }
}

fn workspace_entry_kind(is_dir: bool) -> &'static str {
    if is_dir {
        "directory"
    } else {
        "file"
    }
}

pub(super) async fn create_workspace_directory(
    config: &crate::config::Config,
    raw_path: Option<&str>,
) -> Result<WorkspacePathMutationPayload, String> {
    let (target_path, relative_path) = resolve_workspace_path(config, raw_path)?;

    if relative_path.is_empty() {
        return Err("Directory path must include a folder name".to_string());
    }

    if let Ok(metadata) = tokio::fs::metadata(&target_path).await {
        return Err(if metadata.is_dir() {
            "Workspace directory already exists".to_string()
        } else {
            "Workspace path already exists as a file".to_string()
        });
    }

    tokio::fs::create_dir_all(&target_path)
        .await
        .map_err(|error| format!("Failed to create workspace directory: {error}"))?;

    Ok(WorkspacePathMutationPayload {
        status: "ok",
        path: relative_path,
        kind: "directory",
    })
}

pub(super) async fn delete_workspace_path(
    config: &crate::config::Config,
    raw_path: Option<&str>,
) -> Result<WorkspacePathMutationPayload, String> {
    let (target_path, relative_path) = resolve_workspace_path(config, raw_path)?;

    if relative_path.is_empty() {
        return Err("Refusing to delete the workspace root".to_string());
    }

    let metadata = tokio::fs::symlink_metadata(&target_path)
        .await
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                "Workspace path not found".to_string()
            } else {
                format!("Failed to inspect workspace path: {error}")
            }
        })?;

    let file_type = metadata.file_type();
    let is_directory = file_type.is_dir();
    if is_directory {
        tokio::fs::remove_dir_all(&target_path)
            .await
            .map_err(|error| format!("Failed to delete workspace path: {error}"))?;
    } else {
        tokio::fs::remove_file(&target_path)
            .await
            .map_err(|error| format!("Failed to delete workspace path: {error}"))?;
    }

    Ok(WorkspacePathMutationPayload {
        status: "ok",
        path: relative_path,
        kind: workspace_entry_kind(is_directory),
    })
}

fn workspace_download_name(relative: &str, is_dir: bool) -> String {
    let base = if relative.is_empty() {
        "workspace".to_string()
    } else {
        FsPath::new(relative)
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("workspace")
            .to_string()
    };
    let sanitized: String = base
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if is_dir {
        format!("{sanitized}.tar.gz")
    } else {
        sanitized
    }
}

fn download_content_disposition(filename: &str) -> String {
    format!(
        "attachment; filename=\"{}\"",
        filename
            .replace('"', "_")
            .replace('\r', "_")
            .replace('\n', "_")
    )
}

async fn fetch_ollama_model_names(
    client: &reqwest::Client,
    endpoint: &str,
    auth_token: Option<&str>,
    suffix: &str,
) -> Option<Vec<String>> {
    let url = format!("{endpoint}{suffix}");
    let mut request = client.get(url);
    if let Some(token) = auth_token {
        request = request.bearer_auth(token);
    }

    let Ok(response) = request.send().await else {
        return None;
    };
    if !response.status().is_success() {
        return None;
    }
    let Ok(payload) = response.json::<OllamaModelListResponse>().await else {
        return None;
    };

    let mut names: Vec<String> = payload
        .models
        .into_iter()
        .map(|model| model.name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect();
    names.sort();
    names.dedup();
    Some(names)
}

async fn unload_ollama_model(
    client: &reqwest::Client,
    endpoint: &str,
    auth_token: Option<&str>,
    model: &str,
) -> Result<(), String> {
    let url = format!("{endpoint}/api/generate");
    let mut request = client.post(url).json(&serde_json::json!({
        "model": model,
        "prompt": "",
        "stream": false,
        "keep_alive": 0,
    }));
    if let Some(token) = auth_token {
        request = request.bearer_auth(token);
    }

    let response = request
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;
    if response.status().is_success() {
        return Ok(());
    }

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    let body = body.trim();
    if body.is_empty() {
        Err(format!("HTTP {status}"))
    } else {
        Err(format!("HTTP {status}: {body}"))
    }
}

async fn unload_ollama_models_except(
    config: &crate::config::Config,
    keep_model: Option<&str>,
) -> OllamaUnloadReport {
    let endpoint = normalize_ollama_base_url(config);
    let client = crate::config::build_runtime_proxy_client_with_timeouts("provider.ollama", 30, 5);
    let auth_token = ollama_auth_token(config);
    let keep_model = keep_model
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(std::string::ToString::to_string);
    let mut report = OllamaUnloadReport {
        endpoint: endpoint.clone(),
        keep_model: keep_model.clone(),
        ..OllamaUnloadReport::default()
    };

    let Some(loaded_models) =
        fetch_ollama_model_names(&client, &endpoint, auth_token.as_deref(), "/api/ps").await
    else {
        return report;
    };

    report.reachable = true;
    for model in loaded_models {
        if keep_model.as_deref().is_some_and(|keep| keep == model) {
            continue;
        }

        report.attempted.push(model.clone());
        match unload_ollama_model(&client, &endpoint, auth_token.as_deref(), &model).await {
            Ok(()) => report.unloaded.push(model),
            Err(error) => report.failed.push(format!("{model}: {error}")),
        }
    }

    report
}

fn should_rebalance_ollama_models(
    previous: &crate::config::Config,
    updated: &crate::config::Config,
) -> bool {
    normalize_ollama_base_url(previous) != normalize_ollama_base_url(updated)
        || previous
            .default_model
            .as_deref()
            .map(str::trim)
            .unwrap_or_default()
            != updated
                .default_model
                .as_deref()
                .map(str::trim)
                .unwrap_or_default()
}

async fn rebalance_ollama_models_for_live_switch(
    previous: &crate::config::Config,
    updated: &crate::config::Config,
) -> Vec<OllamaUnloadReport> {
    if !should_rebalance_ollama_models(previous, updated) {
        return Vec::new();
    }

    let previous_endpoint = normalize_ollama_base_url(previous);
    let updated_endpoint = normalize_ollama_base_url(updated);
    let mut reports = Vec::new();

    if previous_endpoint != updated_endpoint {
        let report = unload_ollama_models_except(previous, None).await;
        if report.reachable || !report.attempted.is_empty() || !report.failed.is_empty() {
            reports.push(report);
        }
    }

    let report = unload_ollama_models_except(updated, updated.default_model.as_deref()).await;
    if report.reachable || !report.attempted.is_empty() || !report.failed.is_empty() {
        reports.push(report);
    }

    reports
}

async fn fetch_ollama_dashboard_info(config: &crate::config::Config) -> OllamaDashboardInfo {
    let endpoint = normalize_ollama_base_url(config);
    let client = crate::config::build_runtime_proxy_client_with_timeouts("provider.ollama", 30, 5);
    let auth_token = ollama_auth_token(config);
    let installed_models =
        fetch_ollama_model_names(&client, &endpoint, auth_token.as_deref(), "/api/tags").await;
    let loaded_models =
        fetch_ollama_model_names(&client, &endpoint, auth_token.as_deref(), "/api/ps").await;
    let reachable = installed_models.is_some() || loaded_models.is_some();
    let installed_models = installed_models.unwrap_or_default();
    let loaded_models = loaded_models.unwrap_or_default();
    let active_model = config.default_model.as_deref().unwrap_or_default().trim();

    OllamaDashboardInfo {
        endpoint,
        reachable,
        active_model_loaded: !active_model.is_empty()
            && loaded_models.iter().any(|model| model == active_model),
        installed_models,
        loaded_models,
    }
}

fn active_dashboard_provider_id(config: &crate::config::Config) -> Option<String> {
    config
        .default_provider
        .as_deref()
        .is_some_and(|provider| provider.trim().eq_ignore_ascii_case(OLLAMA_INTEGRATION_ID))
        .then(|| OLLAMA_INTEGRATION_ID.to_string())
}

fn build_integration_settings_payload(
    config: &crate::config::Config,
    ollama: &OllamaDashboardInfo,
) -> IntegrationSettingsPayload {
    let all_integrations = crate::integrations::registry::all_integrations();
    let registry_entry = all_integrations
        .iter()
        .find(|entry| entry.name == OLLAMA_INTEGRATION_NAME);
    let is_active_provider = config
        .default_provider
        .as_deref()
        .is_some_and(|provider| provider.trim().eq_ignore_ascii_case(OLLAMA_INTEGRATION_ID));
    let has_key = has_non_empty(config.api_key.as_deref());
    let has_model = has_non_empty(config.default_model.as_deref());
    let has_api_url = has_non_empty(config.api_url.as_deref());
    let model_options = if ollama.installed_models.is_empty() {
        OLLAMA_FALLBACK_MODELS
            .iter()
            .map(|value| (*value).to_string())
            .collect()
    } else {
        ollama.installed_models.clone()
    };
    let fields = vec![
        IntegrationCredentialsField {
            key: "default_model".to_string(),
            label: "Installed Model".to_string(),
            required: false,
            has_value: has_model,
            input_type: "select",
            options: model_options,
            current_value: config
                .default_model
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(std::string::ToString::to_string),
            masked_value: None,
        },
        IntegrationCredentialsField {
            key: "default_temperature".to_string(),
            label: "Temperature".to_string(),
            required: false,
            has_value: true,
            input_type: "text",
            options: Vec::new(),
            current_value: Some(config.default_temperature.to_string()),
            masked_value: None,
        },
        IntegrationCredentialsField {
            key: "api_url".to_string(),
            label: "Ollama Endpoint".to_string(),
            required: false,
            has_value: has_api_url,
            input_type: "text",
            options: Vec::new(),
            current_value: config
                .api_url
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .map(std::string::ToString::to_string),
            masked_value: None,
        },
        IntegrationCredentialsField {
            key: "api_key".to_string(),
            label: "API Key".to_string(),
            required: false,
            has_value: has_key,
            input_type: "secret",
            options: Vec::new(),
            current_value: None,
            masked_value: has_key.then(|| "••••••••".to_string()),
        },
    ];
    let entry = IntegrationSettingsEntry {
        id: OLLAMA_INTEGRATION_ID.to_string(),
        name: registry_entry
            .map(|entry| entry.name.to_string())
            .unwrap_or_else(|| OLLAMA_INTEGRATION_NAME.to_string()),
        description: registry_entry
            .map(|entry| entry.description.to_string())
            .unwrap_or_else(|| "Local Ollama runtime and model selection".to_string()),
        category: registry_entry
            .map(|entry| entry.category)
            .unwrap_or(crate::integrations::IntegrationCategory::AiModel),
        status: if is_active_provider {
            crate::integrations::IntegrationStatus::Active
        } else {
            crate::integrations::IntegrationStatus::Available
        },
        configured: is_active_provider,
        activates_default_provider: true,
        fields,
    };

    IntegrationSettingsPayload {
        revision: config_revision(config),
        active_default_provider_integration_id: active_dashboard_provider_id(config),
        integrations: vec![entry],
    }
}

fn apply_integration_credentials_update(
    config: &crate::config::Config,
    integration_id: &str,
    fields: &BTreeMap<String, String>,
) -> Result<crate::config::Config, String> {
    if !integration_id.eq_ignore_ascii_case(OLLAMA_INTEGRATION_ID) {
        return Err(format!("Unknown integration id: {integration_id}"));
    }

    let mut updated = config.clone();
    let switching_to_ollama = !config
        .default_provider
        .as_deref()
        .is_some_and(|provider| provider.eq_ignore_ascii_case(OLLAMA_INTEGRATION_ID));

    for (key, value) in fields {
        let trimmed = value.trim();
        match key.as_str() {
            "api_key" => {
                updated.api_key = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
            }
            "default_model" => {
                updated.default_model = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
            }
            "default_temperature" => {
                updated.default_temperature = if trimmed.is_empty() {
                    crate::config::Config::default().default_temperature
                } else {
                    trimmed.parse::<f64>().map_err(|_| {
                        "Invalid integration config update: default_temperature must be a number between 0.0 and 2.0".to_string()
                    })?
                };
            }
            "api_url" => {
                updated.api_url = if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_string())
                };
            }
            _ => {
                return Err(format!(
                    "Unsupported field '{key}' for integration '{integration_id}'"
                ));
            }
        }
    }

    updated.default_provider = Some(OLLAMA_INTEGRATION_ID.to_string());
    if !fields.contains_key("api_url") {
        updated.api_url = None;
    }
    if (!fields.contains_key("default_model") && switching_to_ollama)
        || !has_non_empty(updated.default_model.as_deref())
    {
        updated.default_model = Some(OLLAMA_FALLBACK_MODELS[0].to_string());
    }

    updated
        .validate()
        .map_err(|err| format!("Invalid integration config update: {err}"))?;
    Ok(updated)
}

// ── Handlers ────────────────────────────────────────────────────

/// GET /api/status — system status overview
pub async fn handle_api_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let runtime = state.runtime_snapshot();
    let config = state.config.lock().clone();
    let health = crate::health::snapshot();
    let ollama = fetch_ollama_dashboard_info(&config).await;
    let shell = build_runtime_shell_info();

    let mut channels = serde_json::Map::new();

    for (channel, present) in config.channels_config.channels() {
        channels.insert(channel.name().to_string(), serde_json::Value::Bool(present));
    }

    let body = serde_json::json!({
        "provider": "ollama",
        "model": runtime.model,
        "temperature": runtime.temperature,
        "uptime_seconds": health.uptime_seconds,
        "gateway_port": config.gateway.port,
        "locale": "en",
        "memory_backend": runtime.mem.name(),
        "shell": shell,
        "channels": channels,
        "health": health,
        "ollama": {
            "endpoint": ollama.endpoint,
            "reachable": ollama.reachable,
            "installed_models": ollama.installed_models,
            "loaded_models": ollama.loaded_models,
            "active_model_loaded": ollama.active_model_loaded,
        },
    });

    Json(body).into_response()
}

/// GET /api/config — current config (api_key masked)
pub async fn handle_api_config_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();

    // Serialize to TOML after masking sensitive fields.
    let masked_config = mask_sensitive_fields(&config);
    let toml_str = match toml::to_string_pretty(&masked_config) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to serialize config: {e}")})),
            )
                .into_response();
        }
    };

    Json(serde_json::json!({
        "format": "toml",
        "content": toml_str,
    }))
    .into_response()
}

/// GET /api/config/presets — bundled Safe/God dashboard presets
pub async fn handle_api_config_presets(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let safe_content = match load_preset_file(SAFE_CONFIG_PRESET_FILE) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to load Safe config preset: {error}")
                })),
            )
                .into_response();
        }
    };

    let god_content = match load_preset_file(GOD_CONFIG_PRESET_FILE) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to load God config preset: {error}")
                })),
            )
                .into_response();
        }
    };

    let safe_workspace_agents = match load_preset_file(SAFE_WORKSPACE_AGENTS_PRESET_FILE) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to load Safe AGENTS preset: {error}")
                })),
            )
                .into_response();
        }
    };

    let safe_workspace_soul = match load_preset_file(SAFE_WORKSPACE_SOUL_PRESET_FILE) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to load Safe SOUL preset: {error}")
                })),
            )
                .into_response();
        }
    };

    let god_workspace_agents = match load_preset_file(GOD_WORKSPACE_AGENTS_PRESET_FILE) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to load God AGENTS preset: {error}")
                })),
            )
                .into_response();
        }
    };

    let god_workspace_soul = match load_preset_file(GOD_WORKSPACE_SOUL_PRESET_FILE) {
        Ok(content) => content,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to load God SOUL preset: {error}")
                })),
            )
                .into_response();
        }
    };

    Json(ConfigPresetsPayload {
        safe: ConfigPresetEntry {
            id: "safe",
            label: "Safe",
            summary: "Mostly autonomous local Ollama profile with tighter filesystem reach, lower budgets, and calmer runtime persona files.",
            highlights: vec![
                "default model: qwen3.5:9b",
                "autonomous inside guardrails",
                "workspace-only boundaries",
                "lower budgets than god",
                "safer AGENTS/SOUL bundle",
            ],
            content: safe_content,
            workspace_files: vec![
                ConfigPresetWorkspaceFile {
                    name: "AGENTS.md",
                    content: safe_workspace_agents,
                },
                ConfigPresetWorkspaceFile {
                    name: "SOUL.md",
                    content: safe_workspace_soul,
                },
            ],
        },
        god: ConfigPresetEntry {
            id: "god",
            label: "God",
            summary: "Aggressive local Ollama profile with very large budgets, wider system reach, and a harder-edged persona bundle.",
            highlights: vec![
                "default model: qwen3.5:9b",
                "bigger iteration budgets",
                "broader build + low-level commands",
                "redirect + quoted-heredoc shell writes enabled",
                "aggressive AGENTS/SOUL bundle",
            ],
            content: god_content,
            workspace_files: vec![
                ConfigPresetWorkspaceFile {
                    name: "AGENTS.md",
                    content: god_workspace_agents,
                },
                ConfigPresetWorkspaceFile {
                    name: "SOUL.md",
                    content: god_workspace_soul,
                },
            ],
        },
    })
    .into_response()
}

/// GET /api/workspace-files/:name — load AGENTS.md or SOUL.md from the live workspace
pub async fn handle_api_workspace_file_get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let file_path = match workspace_editor_path(&config, &name) {
        Ok(path) => path,
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(
                    serde_json::json!({ "error": error, "allowed_files": WORKSPACE_EDITOR_FILES }),
                ),
            )
                .into_response();
        }
    };
    let normalized = normalize_workspace_editor_name(&name).unwrap_or(name.as_str());

    match tokio::fs::read_to_string(&file_path).await {
        Ok(content) => Json(WorkspaceFilePayload {
            name: normalized.to_string(),
            content,
            exists: true,
        })
        .into_response(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Json(WorkspaceFilePayload {
            name: normalized.to_string(),
            content: String::new(),
            exists: false,
        })
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to read workspace file: {error}")
            })),
        )
            .into_response(),
    }
}

/// PUT /api/workspace-files/:name — save AGENTS.md or SOUL.md into the live workspace
pub async fn handle_api_workspace_file_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(name): Path<String>,
    Json(body): Json<WorkspaceFileUpdateBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let file_path = match workspace_editor_path(&config, &name) {
        Ok(path) => path,
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(
                    serde_json::json!({ "error": error, "allowed_files": WORKSPACE_EDITOR_FILES }),
                ),
            )
                .into_response();
        }
    };
    let normalized = normalize_workspace_editor_name(&name).unwrap_or(name.as_str());

    if let Err(error) = tokio::fs::create_dir_all(&config.workspace_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to prepare workspace directory: {error}")
            })),
        )
            .into_response();
    }

    if let Err(error) = tokio::fs::write(&file_path, &body.content).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to save workspace file: {error}")
            })),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "status": "ok",
        "file": WorkspaceFilePayload {
            name: normalized.to_string(),
            content: body.content,
            exists: true,
        }
    }))
    .into_response()
}

/// GET /api/workspace/browser — browse the live workspace directory tree
pub async fn handle_api_workspace_browser(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WorkspacePathQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let (dir_path, current_path) = match resolve_workspace_path(&config, query.path.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };

    let metadata = match tokio::fs::metadata(&dir_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Workspace path not found" })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to inspect workspace path: {error}")
                })),
            )
                .into_response();
        }
    };

    if !metadata.is_dir() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Workspace browser path must be a directory" })),
        )
            .into_response();
    }

    let mut read_dir = match tokio::fs::read_dir(&dir_path).await {
        Ok(read_dir) => read_dir,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to read workspace directory: {error}")
                })),
            )
                .into_response();
        }
    };

    let mut entries = Vec::new();
    loop {
        let next = match read_dir.next_entry().await {
            Ok(next) => next,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to enumerate workspace directory: {error}")
                    })),
                )
                    .into_response();
            }
        };
        let Some(entry) = next else {
            break;
        };

        let name = entry.file_name().to_string_lossy().to_string();
        let entry_path = if current_path.is_empty() {
            name.clone()
        } else {
            format!("{current_path}/{name}")
        };

        let metadata = match entry.metadata().await {
            Ok(metadata) => metadata,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to inspect workspace entry: {error}")
                    })),
                )
                    .into_response();
            }
        };

        let is_dir = metadata.is_dir();
        let modified_at = metadata
            .modified()
            .ok()
            .map(|timestamp| DateTime::<Utc>::from(timestamp).to_rfc3339());

        entries.push(WorkspaceBrowserEntry {
            name,
            path: entry_path,
            kind: workspace_entry_kind(is_dir),
            size_bytes: (!is_dir).then_some(metadata.len()),
            modified_at,
        });
    }

    entries.sort_by(|left, right| {
        let left_dir = left.kind == "directory";
        let right_dir = right.kind == "directory";
        right_dir.cmp(&left_dir).then_with(|| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
        })
    });

    Json(WorkspaceBrowserPayload {
        root_path: config.workspace_dir.display().to_string(),
        current_path: current_path.clone(),
        parent_path: workspace_parent_path(&current_path),
        entries,
    })
    .into_response()
}

/// PUT /api/workspace/blob?path=... — upload raw file bytes into the live workspace
pub async fn handle_api_workspace_blob_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WorkspacePathQuery>,
    body: Bytes,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let (file_path, relative_path) = match resolve_workspace_path(&config, query.path.as_deref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };

    if relative_path.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Upload path must include a file name" })),
        )
            .into_response();
    }

    if let Ok(metadata) = tokio::fs::metadata(&file_path).await {
        if metadata.is_dir() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "Upload target points to a directory" })),
            )
                .into_response();
        }
    }

    let Some(parent) = file_path.parent() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Upload target has no parent directory" })),
        )
            .into_response();
    };

    if let Err(error) = tokio::fs::create_dir_all(parent).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to prepare workspace directory: {error}")
            })),
        )
            .into_response();
    }

    if let Err(error) = tokio::fs::write(&file_path, &body).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to write workspace file: {error}")
            })),
        )
            .into_response();
    }

    Json(WorkspaceBlobWritePayload {
        status: "ok",
        path: relative_path,
        size_bytes: body.len() as u64,
    })
    .into_response()
}

/// GET /api/workspace/download?path=... — download a file or directory from the live workspace
pub async fn handle_api_workspace_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WorkspacePathQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let (target_path, relative_path) = match resolve_workspace_path(&config, query.path.as_deref())
    {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };

    let metadata = match tokio::fs::metadata(&target_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": "Workspace path not found" })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!("Failed to inspect workspace path: {error}")
                })),
            )
                .into_response();
        }
    };

    let is_dir = metadata.is_dir();
    let download_name = workspace_download_name(&relative_path, is_dir);
    let content_disposition =
        match HeaderValue::from_str(&download_content_disposition(&download_name)) {
            Ok(value) => value,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to build download headers: {error}")
                    })),
                )
                    .into_response();
            }
        };

    let (content_type, body_bytes) = if is_dir {
        let archive_target = if relative_path.is_empty() {
            ".".to_string()
        } else {
            relative_path.clone()
        };
        let output = match tokio::process::Command::new("tar")
            .arg("-czf")
            .arg("-")
            .arg("-C")
            .arg(&config.workspace_dir)
            .arg(&archive_target)
            .output()
            .await
        {
            Ok(output) => output,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to create directory archive: {error}")
                    })),
                )
                    .into_response();
            }
        };

        if !output.status.success() {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({
                    "error": format!(
                        "Directory archive command failed: {}",
                        String::from_utf8_lossy(&output.stderr).trim()
                    )
                })),
            )
                .into_response();
        }

        ("application/gzip".to_string(), output.stdout)
    } else {
        let bytes = match tokio::fs::read(&target_path).await {
            Ok(bytes) => bytes,
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({
                        "error": format!("Failed to read workspace file: {error}")
                    })),
                )
                    .into_response();
            }
        };

        let mime = mime_guess::from_path(&target_path)
            .first_or_octet_stream()
            .essence_str()
            .to_string();
        (mime, bytes)
    };

    let mut response = Response::new(Body::from(body_bytes));
    *response.status_mut() = StatusCode::OK;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(&content_type)
            .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream")),
    );
    response
        .headers_mut()
        .insert(header::CONTENT_DISPOSITION, content_disposition);
    response.into_response()
}

/// PUT /api/workspace/directory?path=... — create a directory in the live workspace
pub async fn handle_api_workspace_directory_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WorkspacePathQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    match create_workspace_directory(&config, query.path.as_deref()).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) if error == "Directory path must include a folder name" => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
        Err(error)
            if error == "Workspace directory already exists"
                || error == "Workspace path already exists as a file" =>
        {
            (
                StatusCode::CONFLICT,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response()
        }
        Err(error)
            if error.starts_with("Workspace paths") || error.starts_with("Workspace path may") =>
        {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
    }
}

/// DELETE /api/workspace/path?path=... — delete a file or directory from the live workspace
pub async fn handle_api_workspace_path_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WorkspacePathQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    match delete_workspace_path(&config, query.path.as_deref()).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error)
            if error == "Refusing to delete the workspace root"
                || error.starts_with("Workspace paths")
                || error.starts_with("Workspace path may") =>
        {
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response()
        }
        Err(error) if error == "Workspace path not found" => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": error })),
        )
            .into_response(),
    }
}

const WORKSPACE_EXEC_MAX_OUTPUT_BYTES: usize = 256 * 1024;

fn truncate_exec_output(mut text: String) -> String {
    if text.len() > WORKSPACE_EXEC_MAX_OUTPUT_BYTES {
        text.truncate(crate::util::floor_utf8_char_boundary(
            &text,
            WORKSPACE_EXEC_MAX_OUTPUT_BYTES,
        ));
        text.push_str("\n... [output truncated]");
    }
    text
}

/// POST /api/workspace/exec — run a shell command in the workspace for the
/// dashboard IDE terminal. Runs under the workspace `.venv` when present so
/// `python`/`pip` resolve to the project environment.
///
/// Deliberately has no timeout (and is registered outside the gateway's
/// global TimeoutLayer): commands like `ollama pull` legitimately run for
/// many minutes and the operator decides when to give up.
pub async fn handle_api_workspace_exec(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let Some(command) = body
        .get("command")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|c| !c.is_empty())
    else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "Missing 'command'" })),
        )
            .into_response();
    };

    let config = state.config.lock().clone();
    let workspace_dir = config.workspace_dir.clone();

    let started = std::time::Instant::now();
    // Plain -c (not -lc): a login shell would source /etc/profile and reset
    // PATH, clobbering the venv prepend below.
    let mut cmd = tokio::process::Command::new("bash");
    cmd.arg("-c")
        .arg(command)
        .current_dir(&workspace_dir)
        .stdin(std::process::Stdio::null());
    crate::tools::shell::apply_workspace_venv_env(&mut cmd, &workspace_dir);

    match cmd.output().await {
        Ok(output) => {
            let stdout = truncate_exec_output(String::from_utf8_lossy(&output.stdout).to_string());
            let stderr = truncate_exec_output(String::from_utf8_lossy(&output.stderr).to_string());
            Json(serde_json::json!({
                "exit_code": output.status.code(),
                "stdout": stdout,
                "stderr": stderr,
                "duration_secs": started.elapsed().as_secs_f64(),
            }))
            .into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to run command: {error}") })),
        )
            .into_response(),
    }
}

/// PUT /api/config — update config from TOML body
pub async fn handle_api_config_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Parse the incoming TOML and normalize known dashboard-masked edge cases.
    let mut incoming_toml: toml::Value = match toml::from_str(&body) {
        Ok(v) => v,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Invalid TOML: {e}")})),
            )
                .into_response();
        }
    };
    normalize_dashboard_config_toml(&mut incoming_toml);
    let incoming: crate::config::Config = match incoming_toml.try_into() {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Invalid TOML: {e}")})),
            )
                .into_response();
        }
    };

    let current_config = state.config.lock().clone();
    let new_config = hydrate_config_for_save(incoming, &current_config);

    if let Err(e) = new_config.validate() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": format!("Invalid config: {e}")})),
        )
            .into_response();
    }

    let runtime_snapshot = match build_gateway_runtime_snapshot_with_federation(
        &new_config,
        state.federation.as_ref().map(|federation| federation.remote_adapter()),
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(
                    serde_json::json!({"error": format!("Config cannot be applied live: {error}")}),
                ),
            )
                .into_response();
        }
    };

    // Save to disk
    if let Err(e) = new_config.save().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save config: {e}")})),
        )
            .into_response();
    }

    // Update in-memory config
    *state.config.lock() = new_config;
    state.replace_runtime_snapshot(runtime_snapshot);

    Json(serde_json::json!({"status": "ok"})).into_response()
}

/// GET /api/tools — list registered tool specs
pub async fn handle_api_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let runtime = state.runtime_snapshot();
    let tools: Vec<serde_json::Value> = runtime
        .tools_registry
        .iter()
        .map(|spec| {
            serde_json::json!({
                "name": spec.name,
                "description": spec.description,
                "parameters": spec.parameters,
            })
        })
        .collect();

    Json(serde_json::json!({"tools": tools})).into_response()
}

/// GET /api/federation/peers — dashboard peer registry and local federation status.
pub async fn handle_api_federation_peers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(error) = require_auth(&state, &headers) {
        return error.into_response();
    }

    Json(build_federation_peers_response(&state)).into_response()
}

/// PUT /api/federation/peers/:peer_id/role — update an operator-assigned peer role.
pub async fn handle_api_federation_peer_role_put(
    State(state): State<AppState>,
    Path(peer_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<FederationPeerRoleUpdateBody>,
) -> impl IntoResponse {
    if let Err(error) = require_auth(&state, &headers) {
        return error.into_response();
    }

    let Some(federation) = &state.federation else {
        return federation_disabled_response();
    };

    match federation.set_assigned_role(&peer_id, body.role) {
        Some(peer) => Json(serde_json::json!({
            "status": "ok",
            "peer": peer,
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Unknown federation peer '{peer_id}'")
            })),
        )
            .into_response(),
    }
}

/// PUT /api/federation/peers/:peer_id/hints — set specialization and priority for a peer.
pub async fn handle_api_federation_peer_hints_put(
    State(state): State<AppState>,
    Path(peer_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<FederationPeerHintsBody>,
) -> impl IntoResponse {
    if let Err(error) = require_auth(&state, &headers) {
        return error.into_response();
    }

    let Some(federation) = &state.federation else {
        return federation_disabled_response();
    };

    match federation.set_peer_hints(&peer_id, body.specialization, body.priority) {
        Some(peer) => Json(serde_json::json!({
            "status": "ok",
            "peer": peer,
        }))
        .into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Unknown federation peer '{peer_id}'")
            })),
        )
            .into_response(),
    }
}

/// POST /api/federation/peers — add a manual peer by endpoint URL.
pub async fn handle_api_federation_peer_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<FederationAddManualPeerBody>,
) -> impl IntoResponse {
    if let Err(error) = require_auth(&state, &headers) {
        return error.into_response();
    }

    let Some(federation) = &state.federation else {
        return federation_disabled_response();
    };

    match crate::federation::normalize_peer_endpoint(&body.endpoint) {
        None => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Invalid endpoint — expected http://host:port or host:port"
            })),
        )
            .into_response(),
        Some((base_url, host, port)) => {
            federation
                .registry()
                .seed_manual_peer(base_url.clone(), base_url.clone(), host, port);
            Json(serde_json::json!({
                "status": "ok",
                "base_url": base_url,
            }))
            .into_response()
        }
    }
}

/// GET /api/federation/delegation — read delegation-enabled flag
pub async fn handle_api_federation_delegation_get(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let enabled = state.config.lock().federation.enable_delegation;
    Json(serde_json::json!({ "enabled": enabled })).into_response()
}

/// PUT /api/federation/delegation — toggle delegation tools on/off
pub async fn handle_api_federation_delegation_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let Some(enabled) = body.get("enabled").and_then(|v| v.as_bool()) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "expected {\"enabled\": bool}"})),
        )
            .into_response();
    };

    let new_config = {
        let mut cfg = state.config.lock().clone();
        cfg.federation.enable_delegation = enabled;
        cfg
    };

    let runtime_snapshot = match super::build_gateway_runtime_snapshot_with_federation(
        &new_config,
        state.federation.as_ref().map(|f| f.remote_adapter()),
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("{error}")})),
            )
                .into_response();
        }
    };

    if let Err(e) = new_config.save().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save config: {e}")})),
        )
            .into_response();
    }

    *state.config.lock() = new_config;
    state.replace_runtime_snapshot(runtime_snapshot);

    Json(serde_json::json!({ "enabled": enabled })).into_response()
}

/// GET /federation/health — private/LAN-only worker health probe.
pub async fn handle_federation_health(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(response) = require_federation_peer_auth(&state, &headers, peer_addr) {
        return response;
    }

    let Some(federation) = &state.federation else {
        return federation_disabled_response();
    };
    let local = federation.local_node_summary();
    Json(serde_json::json!({
        "status": "ok",
        "node_id": local.node_id,
        "display_name": local.display_name,
        "role": local.role,
        "allow_remote_subagents": local.allow_remote_subagents,
        "api_port": local.api_port,
        "app_version": env!("CARGO_PKG_VERSION"),
        "last_seen": Utc::now().to_rfc3339(),
    }))
    .into_response()
}

/// GET /federation/capabilities — private/LAN-only worker capabilities.
pub async fn handle_federation_capabilities(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(response) = require_federation_peer_auth(&state, &headers, peer_addr) {
        return response;
    }

    match build_federation_capabilities(&state).await {
        Ok(capabilities) => Json::<FederationCapabilities>(capabilities).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to build federation capabilities: {error}")
            })),
        )
            .into_response(),
    }
}

/// GET /federation/models — private/LAN-only installed model snapshot.
pub async fn handle_federation_models(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(response) = require_federation_peer_auth(&state, &headers, peer_addr) {
        return response;
    }

    match build_federation_capabilities(&state).await {
        Ok(capabilities) => Json(serde_json::json!({
            "node_id": capabilities.node_id,
            "display_name": capabilities.display_name,
            "model": capabilities.model,
            "installed_models": capabilities.installed_models,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to build federation model list: {error}")
            })),
        )
            .into_response(),
    }
}

/// GET /federation/tools — private/LAN-only tool capability snapshot.
pub async fn handle_federation_tools(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(response) = require_federation_peer_auth(&state, &headers, peer_addr) {
        return response;
    }

    match build_federation_capabilities(&state).await {
        Ok(capabilities) => Json(serde_json::json!({
            "node_id": capabilities.node_id,
            "display_name": capabilities.display_name,
            "tools": capabilities.tools,
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": format!("Failed to build federation tool list: {error}")
            })),
        )
            .into_response(),
    }
}

/// POST /federation/tasks — private/LAN-only remote subagent task ingress.
pub async fn handle_federation_task_create(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(request): Json<FederationTaskRequest>,
) -> impl IntoResponse {
    if let Err(response) = require_federation_peer_auth(&state, &headers, peer_addr) {
        return response;
    }

    let Some(federation) = &state.federation else {
        return federation_disabled_response();
    };
    let local = federation.local_node_summary();
    if !local.role.allows_worker() || !local.allow_remote_subagents {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "This node is not accepting remote worker tasks"
            })),
        )
            .into_response();
    }

    let Some(task_manager) = state.federation_tasks.clone() else {
        return federation_disabled_response();
    };

    let task_id = Uuid::new_v4().to_string();
    task_manager.create_task(&task_id);

    let cancellation = CancellationToken::new();
    let task_state = state.clone();
    let task_request = request.clone();
    let task_manager_clone = task_manager.clone();
    let task_id_clone = task_id.clone();
    let cancellation_for_task = cancellation.clone();
    let handle = tokio::spawn(async move {
        execute_federation_task(
            task_state,
            task_manager_clone,
            task_id_clone,
            task_request,
            cancellation_for_task,
        )
        .await;
    });
    task_manager.set_running_handle(&task_id, handle, cancellation);

    Json(FederationTaskAccepted {
        task_id,
        status: "accepted".to_string(),
    })
    .into_response()
}

/// GET /federation/tasks/:task_id/stream — private/LAN-only SSE task stream.
pub async fn handle_federation_task_stream(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = require_federation_peer_auth(&state, &headers, peer_addr) {
        return response;
    }

    let Some(task_manager) = &state.federation_tasks else {
        return federation_disabled_response();
    };
    let Some((history, receiver)) = task_manager.stream(&task_id) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Unknown federation task '{task_id}'")
            })),
        )
            .into_response();
    };

    let event_stream = stream::unfold(
        (history.into_iter(), receiver, false),
        |(mut history, mut receiver, done)| async move {
            if let Some(event) = history.next() {
                let terminal = done || federation_event_is_terminal(&event);
                return Some((
                    serialize_federation_sse_event(&event),
                    (history, receiver, terminal),
                ));
            }
            if done {
                return None;
            }

            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        let terminal = federation_event_is_terminal(&event);
                        return Some((
                            serialize_federation_sse_event(&event),
                            (history, receiver, terminal),
                        ));
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return None,
                }
            }
        },
    );

    Sse::new(event_stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

/// POST /federation/tasks/:task_id/cancel — private/LAN-only task cancellation.
pub async fn handle_federation_task_cancel(
    State(state): State<AppState>,
    ConnectInfo(peer_addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(task_id): Path<String>,
) -> impl IntoResponse {
    if let Err(response) = require_federation_peer_auth(&state, &headers, peer_addr) {
        return response;
    }

    let Some(task_manager) = &state.federation_tasks else {
        return federation_disabled_response();
    };

    if task_manager.cancel(&task_id) {
        Json(serde_json::json!({
            "status": "cancelled",
            "task_id": task_id,
        }))
        .into_response()
    } else {
        (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": format!("Unknown or completed federation task '{task_id}'")
            })),
        )
            .into_response()
    }
}

/// GET /api/cron — list cron jobs
pub async fn handle_api_cron_list(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    match crate::cron::list_jobs(&config) {
        Ok(jobs) => {
            let jobs_json: Vec<serde_json::Value> = jobs.iter().map(cron_job_json).collect();
            Json(serde_json::json!({"jobs": jobs_json})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to list cron jobs: {e}")})),
        )
            .into_response(),
    }
}

/// POST /api/cron — add a new cron job
pub async fn handle_api_cron_add(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CronAddBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let schedule = match parse_cron_schedule(
        body.schedule_kind.as_deref(),
        body.schedule.as_deref(),
        body.run_at.as_deref(),
        body.every_ms,
    ) {
        Ok(schedule) => schedule,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
    };

    match crate::cron::add_shell_job(&config, body.name, schedule, &body.command) {
        Ok(job) => {
            let job = if body.enabled == Some(false) {
                match crate::cron::update_job(
                    &config,
                    &job.id,
                    crate::cron::CronJobPatch {
                        enabled: Some(false),
                        ..crate::cron::CronJobPatch::default()
                    },
                ) {
                    Ok(updated) => updated,
                    Err(error) => {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            Json(serde_json::json!({"error": format!("Failed to disable new cron job: {error}")})),
                        )
                            .into_response();
                    }
                }
            } else {
                job
            };

            Json(serde_json::json!({
                "status": "ok",
                "job": cron_job_json(&job),
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to add cron job: {e}")})),
        )
            .into_response(),
    }
}

/// DELETE /api/cron/:id — remove a cron job
pub async fn handle_api_cron_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    match crate::cron::remove_job(&config, &id) {
        Ok(()) => Json(serde_json::json!({"status": "ok"})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to remove cron job: {e}")})),
        )
            .into_response(),
    }
}

/// PUT /api/cron/:id — update an existing cron job
pub async fn handle_api_cron_update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<CronUpdateBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let schedule = if body.schedule_kind.is_some()
        || body.schedule.is_some()
        || body.run_at.is_some()
        || body.every_ms.is_some()
    {
        match parse_cron_schedule(
            body.schedule_kind.as_deref(),
            body.schedule.as_deref(),
            body.run_at.as_deref(),
            body.every_ms,
        ) {
            Ok(schedule) => Some(schedule),
            Err(error) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": error})),
                )
                    .into_response();
            }
        }
    } else {
        None
    };

    let config = state.config.lock().clone();
    match crate::cron::update_job(
        &config,
        &id,
        crate::cron::CronJobPatch {
            schedule,
            command: body.command,
            name: body.name,
            enabled: body.enabled,
            ..crate::cron::CronJobPatch::default()
        },
    ) {
        Ok(job) => Json(serde_json::json!({
            "status": "ok",
            "job": cron_job_json(&job),
        }))
        .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to update cron job: {error}")})),
        )
            .into_response(),
    }
}

/// POST /api/cron/:id/run — execute a cron job immediately
pub async fn handle_api_cron_run(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let job = match crate::cron::get_job(&config, &id) {
        Ok(job) => job,
        Err(error) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": format!("Failed to load cron job: {error}")})),
            )
                .into_response();
        }
    };

    let started_at = Utc::now();
    let (success, output) = crate::cron::scheduler::execute_job_now(&config, &job).await;
    let finished_at = Utc::now();
    let duration_ms = (finished_at - started_at).num_milliseconds();
    let _ = crate::cron::record_run(
        &config,
        &job.id,
        started_at,
        finished_at,
        if success { "ok" } else { "error" },
        Some(&output),
        duration_ms,
    );

    if job.delete_after_run && matches!(job.schedule, crate::cron::Schedule::At { .. }) {
        if success {
            let _ = crate::cron::remove_job(&config, &job.id);
        } else {
            let _ = crate::cron::record_last_run(&config, &job.id, finished_at, false, &output);
            let _ = crate::cron::update_job(
                &config,
                &job.id,
                crate::cron::CronJobPatch {
                    enabled: Some(false),
                    ..crate::cron::CronJobPatch::default()
                },
            );
        }
    } else {
        let _ = crate::cron::reschedule_after_run(&config, &job, success, &output);
    }

    let refreshed = crate::cron::get_job(&config, &id).ok();
    Json(serde_json::json!({
        "status": if success { "ok" } else { "error" },
        "output": output,
        "job": refreshed.as_ref().map(cron_job_json),
    }))
    .into_response()
}

/// GET /api/integrations — list all integrations with status
pub async fn handle_api_integrations(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let ollama_status = if config
        .default_provider
        .as_deref()
        .is_some_and(|provider| provider.trim().eq_ignore_ascii_case(OLLAMA_INTEGRATION_ID))
    {
        crate::integrations::IntegrationStatus::Active
    } else {
        crate::integrations::IntegrationStatus::Available
    };

    let integrations = vec![serde_json::json!({
        "name": OLLAMA_INTEGRATION_NAME,
        "description": "Local Ollama runtime and model selection",
        "category": crate::integrations::IntegrationCategory::AiModel,
        "status": ollama_status,
    })];

    Json(serde_json::json!({"integrations": integrations})).into_response()
}

/// GET /api/integrations/settings — dashboard credential schema + masked state
pub async fn handle_api_integrations_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let ollama = fetch_ollama_dashboard_info(&config).await;
    let payload = build_integration_settings_payload(&config, &ollama);
    Json(payload).into_response()
}

/// PUT /api/integrations/:id/credentials — update integration credentials/config
pub async fn handle_api_integration_credentials_put(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(body): Json<IntegrationCredentialsUpdateBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let current = state.config.lock().clone();
    let current_revision = config_revision(&current);
    if let Some(revision) = body.revision.as_deref() {
        if revision != current_revision {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "Integration settings are out of date. Refresh and retry.",
                    "revision": current_revision,
                })),
            )
                .into_response();
        }
    }

    let updated = match apply_integration_credentials_update(&current, &id, &body.fields) {
        Ok(config) => config,
        Err(error) if error.starts_with("Unknown integration id:") => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
        Err(error) if error.starts_with("Unsupported field") => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
        Err(error) if error.starts_with("Invalid integration config update:") => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "error": error })),
            )
                .into_response();
        }
    };

    let updated_revision = config_revision(&updated);
    if updated_revision == current_revision {
        return Json(serde_json::json!({
            "status": "ok",
            "revision": updated_revision,
            "unchanged": true,
        }))
        .into_response();
    }

    let runtime_snapshot = match build_gateway_runtime_snapshot_with_federation(
        &updated,
        state.federation.as_ref().map(|federation| federation.remote_adapter()),
    ) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Integration config cannot be applied live: {error}")})),
            )
                .into_response();
        }
    };

    let unload_requested = should_rebalance_ollama_models(&current, &updated);

    if let Err(error) = updated.save().await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to save config: {error}")})),
        )
            .into_response();
    }

    let unload_reports = rebalance_ollama_models_for_live_switch(&current, &updated).await;
    *state.config.lock() = updated;
    state.replace_runtime_snapshot(runtime_snapshot);
    Json(serde_json::json!({
        "status": "ok",
        "revision": updated_revision,
        "ollama_unload": {
            "requested": unload_requested,
            "reports": unload_reports,
        }
    }))
    .into_response()
}

/// POST /api/doctor — run diagnostics
pub async fn handle_api_doctor(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let results = crate::doctor::diagnose_gateway(&config);

    let ok_count = results
        .iter()
        .filter(|r| r.severity == crate::doctor::Severity::Ok)
        .count();
    let warn_count = results
        .iter()
        .filter(|r| r.severity == crate::doctor::Severity::Warn)
        .count();
    let error_count = results
        .iter()
        .filter(|r| r.severity == crate::doctor::Severity::Error)
        .count();

    Json(serde_json::json!({
        "results": results,
        "summary": {
            "ok": ok_count,
            "warnings": warn_count,
            "errors": error_count,
        }
    }))
    .into_response()
}

/// GET /api/memory — list or search memory entries
pub async fn handle_api_memory_list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<MemoryQuery>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let runtime = state.runtime_snapshot();
    if let Some(ref query) = params.query {
        // Search mode
        match runtime.mem.recall(query, 50, None).await {
            Ok(entries) => Json(serde_json::json!({"entries": entries})).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Memory recall failed: {e}")})),
            )
                .into_response(),
        }
    } else {
        // List mode
        let category = params.category.as_deref().map(parse_memory_category);

        match runtime.mem.list(category.as_ref(), None).await {
            Ok(entries) => Json(serde_json::json!({"entries": entries})).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Memory list failed: {e}")})),
            )
                .into_response(),
        }
    }
}

/// POST /api/memory — store a memory entry
pub async fn handle_api_memory_store(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MemoryStoreBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let runtime = state.runtime_snapshot();
    let category = body
        .category
        .as_deref()
        .map(parse_memory_category)
        .unwrap_or(crate::memory::MemoryCategory::Core);

    match runtime
        .mem
        .store(&body.key, &body.content, category, None)
        .await
    {
        Ok(()) => Json(serde_json::json!({"status": "ok"})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Memory store failed: {e}")})),
        )
            .into_response(),
    }
}

/// DELETE /api/memory/:key — delete a memory entry
pub async fn handle_api_memory_delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let runtime = state.runtime_snapshot();
    match runtime.mem.forget(&key).await {
        Ok(deleted) => {
            Json(serde_json::json!({"status": "ok", "deleted": deleted})).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Memory forget failed: {e}")})),
        )
            .into_response(),
    }
}

/// POST /api/memory/clear — clear conversation or all memory entries
pub async fn handle_api_memory_clear(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<MemoryClearBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let runtime = state.runtime_snapshot();
    let scope = body.scope.unwrap_or_else(|| "conversation".to_string());
    let normalized_scope = scope.trim().to_ascii_lowercase();
    let category = match normalized_scope.as_str() {
        "all" => None,
        "conversation" => Some(crate::memory::MemoryCategory::Conversation),
        "core" => Some(crate::memory::MemoryCategory::Core),
        "daily" => Some(crate::memory::MemoryCategory::Daily),
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Unsupported memory clear scope: {other}")})),
            )
                .into_response();
        }
    };

    let entries = match runtime.mem.list(category.as_ref(), None).await {
        Ok(entries) => entries,
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Memory list failed: {error}")})),
            )
                .into_response();
        }
    };

    let mut deleted = 0usize;
    for entry in entries {
        match runtime.mem.forget(&entry.key).await {
            Ok(true) => deleted += 1,
            Ok(false) => {}
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": format!("Memory clear failed: {error}")})),
                )
                    .into_response();
            }
        }
    }

    Json(serde_json::json!({
        "status": "ok",
        "scope": normalized_scope,
        "deleted": deleted,
    }))
    .into_response()
}

/// GET /api/cost — cost summary
pub async fn handle_api_cost(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    if let Some(ref tracker) = state.cost_tracker {
        match tracker.get_summary() {
            Ok(summary) => Json(serde_json::json!({"cost": summary})).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Cost summary failed: {e}")})),
            )
                .into_response(),
        }
    } else {
        Json(serde_json::json!({
            "cost": {
                "session_cost_usd": 0.0,
                "daily_cost_usd": 0.0,
                "monthly_cost_usd": 0.0,
                "total_tokens": 0,
                "request_count": 0,
                "by_model": {},
            }
        }))
        .into_response()
    }
}

/// GET /api/cli-tools — discovered CLI tools
pub async fn handle_api_cli_tools(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let tools = crate::tools::cli_discovery::discover_cli_tools(&[], &[]);

    Json(serde_json::json!({"cli_tools": tools})).into_response()
}

/// GET /api/health — component health snapshot
pub async fn handle_api_health(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let snapshot = crate::health::snapshot();
    Json(serde_json::json!({"health": snapshot})).into_response()
}

// ── Helpers ─────────────────────────────────────────────────────

fn normalize_dashboard_config_toml(root: &mut toml::Value) {
    // Dashboard editors may round-trip masked reliability api_keys as a single
    // string. Accept that shape by normalizing it back to a string array.
    let Some(root_table) = root.as_table_mut() else {
        return;
    };
    let Some(reliability) = root_table
        .get_mut("reliability")
        .and_then(toml::Value::as_table_mut)
    else {
        return;
    };
    let Some(api_keys) = reliability.get_mut("api_keys") else {
        return;
    };
    if let Some(single) = api_keys.as_str() {
        *api_keys = toml::Value::Array(vec![toml::Value::String(single.to_string())]);
    }
}

fn is_masked_secret(value: &str) -> bool {
    value == MASKED_SECRET
}

fn mask_optional_secret(value: &mut Option<String>) {
    if value.is_some() {
        *value = Some(MASKED_SECRET.to_string());
    }
}

fn mask_required_secret(value: &mut String) {
    if !value.is_empty() {
        *value = MASKED_SECRET.to_string();
    }
}

fn mask_vec_secrets(values: &mut [String]) {
    for value in values.iter_mut() {
        if !value.is_empty() {
            *value = MASKED_SECRET.to_string();
        }
    }
}

#[allow(clippy::ref_option)]
fn restore_optional_secret(value: &mut Option<String>, current: &Option<String>) {
    if value.as_deref().is_some_and(is_masked_secret) {
        *value = current.clone();
    }
}

fn restore_required_secret(value: &mut String, current: &str) {
    if is_masked_secret(value) {
        *value = current.to_string();
    }
}

fn restore_vec_secrets(values: &mut [String], current: &[String]) {
    for (idx, value) in values.iter_mut().enumerate() {
        if is_masked_secret(value) {
            if let Some(existing) = current.get(idx) {
                *value = existing.clone();
            }
        }
    }
}

fn mask_sensitive_fields(config: &crate::config::Config) -> crate::config::Config {
    let mut masked = config.clone();

    mask_optional_secret(&mut masked.api_key);
    mask_vec_secrets(&mut masked.reliability.api_keys);
    mask_optional_secret(&mut masked.composio.api_key);
    mask_optional_secret(&mut masked.proxy.http_proxy);
    mask_optional_secret(&mut masked.proxy.https_proxy);
    mask_optional_secret(&mut masked.proxy.all_proxy);
    mask_optional_secret(&mut masked.browser.computer_use.api_key);
    mask_optional_secret(&mut masked.web_fetch.api_key);
    mask_optional_secret(&mut masked.web_search.api_key);
    mask_optional_secret(&mut masked.web_search.brave_api_key);
    mask_optional_secret(&mut masked.storage.provider.config.db_url);
    if let Some(cloudflare) = masked.tunnel.cloudflare.as_mut() {
        mask_required_secret(&mut cloudflare.token);
    }
    if let Some(ngrok) = masked.tunnel.ngrok.as_mut() {
        mask_required_secret(&mut ngrok.auth_token);
    }

    for agent in masked.agents.values_mut() {
        mask_optional_secret(&mut agent.api_key);
    }

    if let Some(telegram) = masked.channels_config.telegram.as_mut() {
        mask_required_secret(&mut telegram.bot_token);
    }
    if let Some(discord) = masked.channels_config.discord.as_mut() {
        mask_required_secret(&mut discord.bot_token);
    }
    if let Some(slack) = masked.channels_config.slack.as_mut() {
        mask_required_secret(&mut slack.bot_token);
        mask_optional_secret(&mut slack.app_token);
    }
    if let Some(mattermost) = masked.channels_config.mattermost.as_mut() {
        mask_required_secret(&mut mattermost.bot_token);
    }
    if let Some(webhook) = masked.channels_config.webhook.as_mut() {
        mask_optional_secret(&mut webhook.secret);
    }
    if let Some(matrix) = masked.channels_config.matrix.as_mut() {
        mask_required_secret(&mut matrix.access_token);
    }
    if let Some(whatsapp) = masked.channels_config.whatsapp.as_mut() {
        mask_optional_secret(&mut whatsapp.access_token);
        mask_optional_secret(&mut whatsapp.app_secret);
        mask_optional_secret(&mut whatsapp.verify_token);
    }
    if let Some(linq) = masked.channels_config.linq.as_mut() {
        mask_required_secret(&mut linq.api_token);
        mask_optional_secret(&mut linq.signing_secret);
    }
    if let Some(wati) = masked.channels_config.wati.as_mut() {
        mask_required_secret(&mut wati.api_token);
    }
    if let Some(nextcloud) = masked.channels_config.nextcloud_talk.as_mut() {
        mask_required_secret(&mut nextcloud.app_token);
        mask_optional_secret(&mut nextcloud.webhook_secret);
    }
    if let Some(email) = masked.channels_config.email.as_mut() {
        mask_required_secret(&mut email.password);
    }
    if let Some(irc) = masked.channels_config.irc.as_mut() {
        mask_optional_secret(&mut irc.server_password);
        mask_optional_secret(&mut irc.nickserv_password);
        mask_optional_secret(&mut irc.sasl_password);
    }
    if let Some(lark) = masked.channels_config.lark.as_mut() {
        mask_required_secret(&mut lark.app_secret);
        mask_optional_secret(&mut lark.encrypt_key);
        mask_optional_secret(&mut lark.verification_token);
    }
    if let Some(feishu) = masked.channels_config.feishu.as_mut() {
        mask_required_secret(&mut feishu.app_secret);
        mask_optional_secret(&mut feishu.encrypt_key);
        mask_optional_secret(&mut feishu.verification_token);
    }
    if let Some(dingtalk) = masked.channels_config.dingtalk.as_mut() {
        mask_required_secret(&mut dingtalk.client_secret);
    }
    if let Some(qq) = masked.channels_config.qq.as_mut() {
        mask_required_secret(&mut qq.app_secret);
    }
    if let Some(nostr) = masked.channels_config.nostr.as_mut() {
        mask_required_secret(&mut nostr.private_key);
    }
    if let Some(clawdtalk) = masked.channels_config.clawdtalk.as_mut() {
        mask_required_secret(&mut clawdtalk.api_key);
        mask_optional_secret(&mut clawdtalk.webhook_secret);
    }
    masked
}

fn restore_masked_sensitive_fields(
    incoming: &mut crate::config::Config,
    current: &crate::config::Config,
) {
    restore_optional_secret(&mut incoming.api_key, &current.api_key);
    restore_vec_secrets(
        &mut incoming.reliability.api_keys,
        &current.reliability.api_keys,
    );
    restore_optional_secret(&mut incoming.composio.api_key, &current.composio.api_key);
    restore_optional_secret(&mut incoming.proxy.http_proxy, &current.proxy.http_proxy);
    restore_optional_secret(&mut incoming.proxy.https_proxy, &current.proxy.https_proxy);
    restore_optional_secret(&mut incoming.proxy.all_proxy, &current.proxy.all_proxy);
    restore_optional_secret(
        &mut incoming.browser.computer_use.api_key,
        &current.browser.computer_use.api_key,
    );
    restore_optional_secret(&mut incoming.web_fetch.api_key, &current.web_fetch.api_key);
    restore_optional_secret(
        &mut incoming.web_search.api_key,
        &current.web_search.api_key,
    );
    restore_optional_secret(
        &mut incoming.web_search.brave_api_key,
        &current.web_search.brave_api_key,
    );
    restore_optional_secret(
        &mut incoming.storage.provider.config.db_url,
        &current.storage.provider.config.db_url,
    );
    if let (Some(incoming_tunnel), Some(current_tunnel)) = (
        incoming.tunnel.cloudflare.as_mut(),
        current.tunnel.cloudflare.as_ref(),
    ) {
        restore_required_secret(&mut incoming_tunnel.token, &current_tunnel.token);
    }
    if let (Some(incoming_tunnel), Some(current_tunnel)) = (
        incoming.tunnel.ngrok.as_mut(),
        current.tunnel.ngrok.as_ref(),
    ) {
        restore_required_secret(&mut incoming_tunnel.auth_token, &current_tunnel.auth_token);
    }

    for (name, agent) in &mut incoming.agents {
        if let Some(current_agent) = current.agents.get(name) {
            restore_optional_secret(&mut agent.api_key, &current_agent.api_key);
        }
    }

    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.telegram.as_mut(),
        current.channels_config.telegram.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.bot_token, &current_ch.bot_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.discord.as_mut(),
        current.channels_config.discord.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.bot_token, &current_ch.bot_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.slack.as_mut(),
        current.channels_config.slack.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.bot_token, &current_ch.bot_token);
        restore_optional_secret(&mut incoming_ch.app_token, &current_ch.app_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.mattermost.as_mut(),
        current.channels_config.mattermost.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.bot_token, &current_ch.bot_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.webhook.as_mut(),
        current.channels_config.webhook.as_ref(),
    ) {
        restore_optional_secret(&mut incoming_ch.secret, &current_ch.secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.matrix.as_mut(),
        current.channels_config.matrix.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.access_token, &current_ch.access_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.whatsapp.as_mut(),
        current.channels_config.whatsapp.as_ref(),
    ) {
        restore_optional_secret(&mut incoming_ch.access_token, &current_ch.access_token);
        restore_optional_secret(&mut incoming_ch.app_secret, &current_ch.app_secret);
        restore_optional_secret(&mut incoming_ch.verify_token, &current_ch.verify_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.linq.as_mut(),
        current.channels_config.linq.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.api_token, &current_ch.api_token);
        restore_optional_secret(&mut incoming_ch.signing_secret, &current_ch.signing_secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.wati.as_mut(),
        current.channels_config.wati.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.api_token, &current_ch.api_token);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.nextcloud_talk.as_mut(),
        current.channels_config.nextcloud_talk.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.app_token, &current_ch.app_token);
        restore_optional_secret(&mut incoming_ch.webhook_secret, &current_ch.webhook_secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.email.as_mut(),
        current.channels_config.email.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.password, &current_ch.password);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.irc.as_mut(),
        current.channels_config.irc.as_ref(),
    ) {
        restore_optional_secret(
            &mut incoming_ch.server_password,
            &current_ch.server_password,
        );
        restore_optional_secret(
            &mut incoming_ch.nickserv_password,
            &current_ch.nickserv_password,
        );
        restore_optional_secret(&mut incoming_ch.sasl_password, &current_ch.sasl_password);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.lark.as_mut(),
        current.channels_config.lark.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.app_secret, &current_ch.app_secret);
        restore_optional_secret(&mut incoming_ch.encrypt_key, &current_ch.encrypt_key);
        restore_optional_secret(
            &mut incoming_ch.verification_token,
            &current_ch.verification_token,
        );
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.feishu.as_mut(),
        current.channels_config.feishu.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.app_secret, &current_ch.app_secret);
        restore_optional_secret(&mut incoming_ch.encrypt_key, &current_ch.encrypt_key);
        restore_optional_secret(
            &mut incoming_ch.verification_token,
            &current_ch.verification_token,
        );
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.dingtalk.as_mut(),
        current.channels_config.dingtalk.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.client_secret, &current_ch.client_secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.qq.as_mut(),
        current.channels_config.qq.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.app_secret, &current_ch.app_secret);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.nostr.as_mut(),
        current.channels_config.nostr.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.private_key, &current_ch.private_key);
    }
    if let (Some(incoming_ch), Some(current_ch)) = (
        incoming.channels_config.clawdtalk.as_mut(),
        current.channels_config.clawdtalk.as_ref(),
    ) {
        restore_required_secret(&mut incoming_ch.api_key, &current_ch.api_key);
        restore_optional_secret(&mut incoming_ch.webhook_secret, &current_ch.webhook_secret);
    }
}

fn hydrate_config_for_save(
    mut incoming: crate::config::Config,
    current: &crate::config::Config,
) -> crate::config::Config {
    restore_masked_sensitive_fields(&mut incoming, current);
    // These are runtime-computed fields skipped from TOML serialization.
    incoming.config_path = current.config_path.clone();
    incoming.workspace_dir = current.workspace_dir.clone();
    incoming
}

// ── Database explorer API ─────────────────────────────────────────────────────

/// GET /api/db/connections — list configured DB connections (no URIs exposed).
pub async fn handle_api_db_connections(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let connections: Vec<serde_json::Value> = config
        .db_connections
        .iter()
        .map(|c| {
            serde_json::json!({
                "name": c.name,
                "driver": format!("{:?}", c.driver).to_lowercase(),
                "uri": c.uri,
                "database": c.database,
                "read_only": c.read_only,
                "max_rows": c.max_rows,
                "label": c.label.as_deref().unwrap_or(&c.name),
            })
        })
        .collect();

    Json(serde_json::json!({ "connections": connections })).into_response()
}

/// POST /api/db/connections — add a new DB connection and persist to config.toml.
#[derive(serde::Deserialize)]
pub struct DbAddConnectionBody {
    pub name: String,
    pub driver: String,
    pub uri: String,
    pub database: Option<String>,
    pub label: Option<String>,
    pub read_only: Option<bool>,
    pub max_rows: Option<usize>,
}

pub async fn handle_api_db_add_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DbAddConnectionBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let driver = match body.driver.as_str() {
        "sqlite" => crate::config::DbDriver::Sqlite,
        "postgres" => crate::config::DbDriver::Postgres,
        "mongodb" => crate::config::DbDriver::Mongodb,
        "mysql" => crate::config::DbDriver::Mysql,
        other => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("Unknown driver '{other}'") })),
            ).into_response();
        }
    };

    let new_conn = crate::config::DbConnectionConfig {
        name: body.name.clone(),
        driver,
        uri: body.uri,
        database: body.database,
        read_only: body.read_only.unwrap_or(true),
        max_rows: body.max_rows.unwrap_or(500),
        label: body.label,
    };

    // Validate name is unique and non-empty
    if body.name.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "name is required" }))).into_response();
    }

    let mut config = state.config.lock().clone();
    if config.db_connections.iter().any(|c| c.name == body.name) {
        return (StatusCode::CONFLICT, Json(serde_json::json!({ "error": format!("Connection '{}' already exists", body.name) }))).into_response();
    }

    config.db_connections.push(new_conn);

    if let Err(e) = config.save().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("Failed to persist: {e}") }))).into_response();
    }
    *state.config.lock() = config.clone();
    if let Ok(snapshot) = build_gateway_runtime_snapshot_with_federation(
        &config,
        state.federation.as_ref().map(|f| f.remote_adapter()),
    ) {
        state.replace_runtime_snapshot(snapshot);
    }

    Json(serde_json::json!({ "status": "ok" })).into_response()
}

/// PUT /api/db/connections/{name} — update an existing DB connection.
pub async fn handle_api_db_update_connection(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DbAddConnectionBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let driver = match body.driver.as_str() {
        "sqlite"   => crate::config::DbDriver::Sqlite,
        "postgres" => crate::config::DbDriver::Postgres,
        "mongodb"  => crate::config::DbDriver::Mongodb,
        "mysql"    => crate::config::DbDriver::Mysql,
        other => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": format!("Unknown driver '{other}'") }))).into_response(),
    };

    let mut config = state.config.lock().clone();
    let Some(conn) = config.db_connections.iter_mut().find(|c| c.name == name) else {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": format!("Connection '{name}' not found") }))).into_response();
    };

    conn.driver   = driver;
    conn.uri      = body.uri;
    conn.database = body.database;
    conn.read_only = body.read_only.unwrap_or(conn.read_only);
    conn.max_rows  = body.max_rows.unwrap_or(conn.max_rows);
    conn.label     = body.label;
    // Allow rename: if body.name differs and is not taken, rename
    if body.name != name && !body.name.trim().is_empty() {
        if config.db_connections.iter().any(|c| c.name == body.name) {
            return (StatusCode::CONFLICT, Json(serde_json::json!({ "error": format!("Name '{}' already taken", body.name) }))).into_response();
        }
        config.db_connections.iter_mut().find(|c| c.name == name).unwrap().name = body.name;
    }

    if let Err(e) = config.save().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("Failed to persist: {e}") }))).into_response();
    }
    *state.config.lock() = config.clone();
    if let Ok(snapshot) = build_gateway_runtime_snapshot_with_federation(
        &config,
        state.federation.as_ref().map(|f| f.remote_adapter()),
    ) {
        state.replace_runtime_snapshot(snapshot);
    }

    Json(serde_json::json!({ "status": "ok" })).into_response()
}

/// DELETE /api/db/connections/{name} — remove a DB connection and persist to config.toml.
pub async fn handle_api_db_remove_connection(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut config = state.config.lock().clone();
    let before = config.db_connections.len();
    config.db_connections.retain(|c| c.name != name);

    if config.db_connections.len() == before {
        return (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": format!("Connection '{name}' not found") }))).into_response();
    }

    if let Err(e) = config.save().await {
        return (StatusCode::INTERNAL_SERVER_ERROR, Json(serde_json::json!({ "error": format!("Failed to persist: {e}") }))).into_response();
    }
    *state.config.lock() = config.clone();
    if let Ok(snapshot) = build_gateway_runtime_snapshot_with_federation(
        &config,
        state.federation.as_ref().map(|f| f.remote_adapter()),
    ) {
        state.replace_runtime_snapshot(snapshot);
    }

    Json(serde_json::json!({ "status": "ok" })).into_response()
}

/// POST /api/db/connections/test — test a connection without saving it.
pub async fn handle_api_db_test_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<DbAddConnectionBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let driver = match body.driver.as_str() {
        "sqlite"   => crate::config::DbDriver::Sqlite,
        "postgres" => crate::config::DbDriver::Postgres,
        "mongodb"  => crate::config::DbDriver::Mongodb,
        "mysql"    => crate::config::DbDriver::Mysql,
        other => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": format!("Unknown driver '{other}'") }))).into_response(),
    };

    let conn_cfg = crate::config::DbConnectionConfig {
        name: body.name.clone(),
        driver,
        uri: body.uri,
        database: body.database,
        read_only: true,
        max_rows: 1,
        label: body.label,
    };

    match crate::db::build_adapter(&conn_cfg) {
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "ok": false, "error": format!("Connect failed: {e}") })),
        ).into_response(),
        Ok(adapter) => match adapter.schema().await {
            Ok(schema) => Json(serde_json::json!({
                "ok": true,
                "tables": schema.tables.len(),
                "driver": schema.driver,
                "database": schema.database,
            })).into_response(),
            Err(e) => (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "ok": false, "error": format!("Connected but schema failed: {e}") })),
            ).into_response(),
        },
    }
}

/// GET /api/db/{name}/schema — fetch schema for a named connection.
pub async fn handle_api_db_schema(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let config = state.config.lock().clone();
    let conn_cfg = match config.db_connections.iter().find(|c| c.name == name) {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("Unknown connection '{name}'") })),
            )
                .into_response()
        }
    };

    let adapter: Box<dyn crate::db::DbAdapter> = match crate::db::build_adapter(&conn_cfg) {
        Ok(a) => a,
        Err(e) => return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("Connection failed: {e}") })),
        )
            .into_response(),
    };
    match adapter.schema().await {
        Ok(schema) => Json(schema).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Schema fetch failed: {e}") })),
        )
            .into_response(),
    }
}

#[derive(serde::Deserialize)]
pub struct DbQueryBody {
    pub query: String,
    pub max_rows: Option<usize>,
}

/// POST /api/db/{name}/query — execute a query on a named connection.
pub async fn handle_api_db_query(
    State(state): State<AppState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<DbQueryBody>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    if body.query.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "query must not be empty" })),
        )
            .into_response();
    }

    let config = state.config.lock().clone();
    let conn_cfg = match config.db_connections.iter().find(|c| c.name == name) {
        Some(c) => c.clone(),
        None => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({ "error": format!("Unknown connection '{name}'") })),
            )
                .into_response()
        }
    };

    let max_rows = body.max_rows.unwrap_or(conn_cfg.max_rows).min(conn_cfg.max_rows);

    let adapter: Box<dyn crate::db::DbAdapter> = match crate::db::build_adapter(&conn_cfg) {
        Ok(a) => a,
        Err(e) => return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": format!("Connection failed: {e}") })),
        )
            .into_response(),
    };
    match adapter.query(&body.query, max_rows).await {
        Ok(result) => Json(result).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Query failed: {e}") })),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        CloudflareTunnelConfig, LarkReceiveMode, NgrokTunnelConfig, WatiConfig,
    };
    use std::collections::BTreeMap;

    #[test]
    fn masking_keeps_toml_valid_and_preserves_api_keys_type() {
        let mut cfg = crate::config::Config::default();
        cfg.api_key = Some("sk-live-123".to_string());
        cfg.reliability.api_keys = vec!["rk-1".to_string(), "rk-2".to_string()];

        let masked = mask_sensitive_fields(&cfg);
        let toml = toml::to_string_pretty(&masked).expect("masked config should serialize");
        let parsed: crate::config::Config =
            toml::from_str(&toml).expect("masked config should remain valid TOML for Config");

        assert_eq!(parsed.api_key.as_deref(), Some(MASKED_SECRET));
        assert_eq!(
            parsed.reliability.api_keys,
            vec![MASKED_SECRET.to_string(), MASKED_SECRET.to_string()]
        );
    }

    #[test]
    fn hydrate_config_for_save_restores_masked_secrets_and_paths() {
        let mut current = crate::config::Config::default();
        current.config_path = std::path::PathBuf::from("/tmp/current/config.toml");
        current.workspace_dir = std::path::PathBuf::from("/tmp/current/workspace");
        current.api_key = Some("real-key".to_string());
        current.reliability.api_keys = vec!["r1".to_string(), "r2".to_string()];

        let mut incoming = mask_sensitive_fields(&current);
        incoming.default_model = Some("gpt-4.1-mini".to_string());
        // Simulate UI changing only one key and keeping the first masked.
        incoming.reliability.api_keys = vec![MASKED_SECRET.to_string(), "r2-new".to_string()];

        let hydrated = hydrate_config_for_save(incoming, &current);

        assert_eq!(hydrated.config_path, current.config_path);
        assert_eq!(hydrated.workspace_dir, current.workspace_dir);
        assert_eq!(hydrated.api_key, current.api_key);
        assert_eq!(hydrated.default_model.as_deref(), Some("gpt-4.1-mini"));
        assert_eq!(
            hydrated.reliability.api_keys,
            vec!["r1".to_string(), "r2-new".to_string()]
        );
    }

    #[test]
    fn normalize_dashboard_config_toml_promotes_single_api_key_string_to_array() {
        let mut cfg = crate::config::Config::default();
        cfg.reliability.api_keys = vec!["rk-live".to_string()];
        let raw_toml = toml::to_string_pretty(&cfg).expect("config should serialize");
        let mut raw =
            toml::from_str::<toml::Value>(&raw_toml).expect("serialized config should parse");
        raw.as_table_mut()
            .and_then(|root| root.get_mut("reliability"))
            .and_then(toml::Value::as_table_mut)
            .and_then(|reliability| reliability.get_mut("api_keys"))
            .map(|api_keys| *api_keys = toml::Value::String(MASKED_SECRET.to_string()))
            .expect("reliability.api_keys should exist");

        normalize_dashboard_config_toml(&mut raw);

        let parsed: crate::config::Config = raw
            .try_into()
            .expect("normalized toml should parse as Config");
        assert_eq!(parsed.reliability.api_keys, vec![MASKED_SECRET.to_string()]);
    }

    #[test]
    fn mask_sensitive_fields_covers_wati_email_and_feishu_secrets() {
        let mut cfg = crate::config::Config::default();
        cfg.proxy.http_proxy = Some("http://user:pass@proxy.internal:8080".to_string());
        cfg.proxy.https_proxy = Some("https://user:pass@proxy.internal:8443".to_string());
        cfg.proxy.all_proxy = Some("socks5://user:pass@proxy.internal:1080".to_string());
        cfg.tunnel.cloudflare = Some(CloudflareTunnelConfig {
            token: "cloudflare-real-token".to_string(),
        });
        cfg.tunnel.ngrok = Some(NgrokTunnelConfig {
            auth_token: "ngrok-real-token".to_string(),
            domain: Some("ollama.ngrok.app".to_string()),
        });
        cfg.channels_config.wati = Some(WatiConfig {
            api_token: "wati-real-token".to_string(),
            api_url: "https://live-mt-server.wati.io".to_string(),
            tenant_id: Some("tenant-1".to_string()),
            allowed_numbers: vec!["*".to_string()],
        });
        let mut email = crate::channels::email_channel::EmailConfig::default();
        email.password = "email-real-password".to_string();
        cfg.channels_config.email = Some(email);
        cfg.channels_config.feishu = Some(crate::config::FeishuConfig {
            app_id: "cli_app_id".to_string(),
            app_secret: "feishu-real-secret".to_string(),
            encrypt_key: Some("feishu-encrypt-key".to_string()),
            verification_token: Some("feishu-verify-token".to_string()),
            allowed_users: vec!["*".to_string()],
            group_reply: None,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(42617),
            draft_update_interval_ms: crate::config::schema::default_lark_draft_update_interval_ms(
            ),
            max_draft_edits: crate::config::schema::default_lark_max_draft_edits(),
        });

        let masked = mask_sensitive_fields(&cfg);
        assert_eq!(masked.proxy.http_proxy.as_deref(), Some(MASKED_SECRET));
        assert_eq!(masked.proxy.https_proxy.as_deref(), Some(MASKED_SECRET));
        assert_eq!(masked.proxy.all_proxy.as_deref(), Some(MASKED_SECRET));
        assert_eq!(
            masked
                .tunnel
                .cloudflare
                .as_ref()
                .map(|value| value.token.as_str()),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked
                .tunnel
                .ngrok
                .as_ref()
                .map(|value| value.auth_token.as_str()),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked
                .channels_config
                .wati
                .as_ref()
                .map(|value| value.api_token.as_str()),
            Some(MASKED_SECRET)
        );
        assert_eq!(
            masked
                .channels_config
                .email
                .as_ref()
                .map(|value| value.password.as_str()),
            Some(MASKED_SECRET)
        );
        let masked_feishu = masked
            .channels_config
            .feishu
            .as_ref()
            .expect("feishu config should exist");
        assert_eq!(masked_feishu.app_secret, MASKED_SECRET);
        assert_eq!(masked_feishu.encrypt_key.as_deref(), Some(MASKED_SECRET));
        assert_eq!(
            masked_feishu.verification_token.as_deref(),
            Some(MASKED_SECRET)
        );
    }

    #[test]
    fn hydrate_config_for_save_restores_wati_email_and_feishu_secrets() {
        let mut current = crate::config::Config::default();
        current.proxy.http_proxy = Some("http://user:pass@proxy.internal:8080".to_string());
        current.proxy.https_proxy = Some("https://user:pass@proxy.internal:8443".to_string());
        current.proxy.all_proxy = Some("socks5://user:pass@proxy.internal:1080".to_string());
        current.tunnel.cloudflare = Some(CloudflareTunnelConfig {
            token: "cloudflare-real-token".to_string(),
        });
        current.tunnel.ngrok = Some(NgrokTunnelConfig {
            auth_token: "ngrok-real-token".to_string(),
            domain: Some("ollama.ngrok.app".to_string()),
        });
        current.channels_config.wati = Some(WatiConfig {
            api_token: "wati-real-token".to_string(),
            api_url: "https://live-mt-server.wati.io".to_string(),
            tenant_id: Some("tenant-1".to_string()),
            allowed_numbers: vec!["*".to_string()],
        });
        let mut email = crate::channels::email_channel::EmailConfig::default();
        email.password = "email-real-password".to_string();
        current.channels_config.email = Some(email);
        current.channels_config.feishu = Some(crate::config::FeishuConfig {
            app_id: "cli_app_id".to_string(),
            app_secret: "feishu-real-secret".to_string(),
            encrypt_key: Some("feishu-encrypt-key".to_string()),
            verification_token: Some("feishu-verify-token".to_string()),
            allowed_users: vec!["*".to_string()],
            group_reply: None,
            receive_mode: LarkReceiveMode::Webhook,
            port: Some(42617),
            draft_update_interval_ms: crate::config::schema::default_lark_draft_update_interval_ms(
            ),
            max_draft_edits: crate::config::schema::default_lark_max_draft_edits(),
        });

        let incoming = mask_sensitive_fields(&current);
        let restored = hydrate_config_for_save(incoming, &current);

        assert_eq!(
            restored.proxy.http_proxy.as_deref(),
            Some("http://user:pass@proxy.internal:8080")
        );
        assert_eq!(
            restored.proxy.https_proxy.as_deref(),
            Some("https://user:pass@proxy.internal:8443")
        );
        assert_eq!(
            restored.proxy.all_proxy.as_deref(),
            Some("socks5://user:pass@proxy.internal:1080")
        );
        assert_eq!(
            restored
                .tunnel
                .cloudflare
                .as_ref()
                .map(|value| value.token.as_str()),
            Some("cloudflare-real-token")
        );
        assert_eq!(
            restored
                .tunnel
                .ngrok
                .as_ref()
                .map(|value| value.auth_token.as_str()),
            Some("ngrok-real-token")
        );
        assert_eq!(
            restored
                .channels_config
                .wati
                .as_ref()
                .map(|value| value.api_token.as_str()),
            Some("wati-real-token")
        );
        assert_eq!(
            restored
                .channels_config
                .email
                .as_ref()
                .map(|value| value.password.as_str()),
            Some("email-real-password")
        );
        let restored_feishu = restored
            .channels_config
            .feishu
            .as_ref()
            .expect("feishu config should exist");
        assert_eq!(restored_feishu.app_secret, "feishu-real-secret");
        assert_eq!(
            restored_feishu.encrypt_key.as_deref(),
            Some("feishu-encrypt-key")
        );
        assert_eq!(
            restored_feishu.verification_token.as_deref(),
            Some("feishu-verify-token")
        );
    }

    #[test]
    fn integration_settings_payload_includes_ollama_fields_and_revision() {
        let config = crate::config::Config::default();
        let payload = build_integration_settings_payload(&config, &OllamaDashboardInfo::default());

        assert!(
            !payload.revision.is_empty(),
            "settings payload should include deterministic revision"
        );
        assert!(
            payload
                .integrations
                .iter()
                .any(|entry| entry.id == "ollama" && entry.name == "Ollama"),
            "dashboard settings payload should expose Ollama editor metadata"
        );
        let ollama = payload
            .integrations
            .iter()
            .find(|entry| entry.id == "ollama")
            .expect("ollama settings entry should exist");
        assert!(
            ollama
                .fields
                .iter()
                .any(|field| field.key == "default_temperature"),
            "ollama settings payload should expose default_temperature"
        );
    }

    #[test]
    fn apply_integration_credentials_update_switches_provider_with_fallback_model() {
        let mut config = crate::config::Config::default();
        config.default_provider = Some("openrouter".to_string());
        config.default_model = Some("anthropic/claude-sonnet-4-6".to_string());
        config.api_url = Some("https://old.example.com".to_string());

        let updated = apply_integration_credentials_update(&config, "ollama", &BTreeMap::new())
            .expect("ollama update should succeed");

        assert_eq!(updated.default_provider.as_deref(), Some("ollama"));
        assert_eq!(updated.default_model.as_deref(), Some("qwen3.5:9b"));
        assert!(
            updated.api_url.is_none(),
            "switching providers without api_url field should reset stale api_url"
        );
    }

    #[test]
    fn apply_integration_credentials_update_rejects_unknown_fields() {
        let config = crate::config::Config::default();
        let mut fields = BTreeMap::new();
        fields.insert("unknown".to_string(), "value".to_string());

        let err = apply_integration_credentials_update(&config, "ollama", &fields)
            .expect_err("unknown fields should fail validation");
        assert!(err.contains("Unsupported field 'unknown'"));
    }

    #[test]
    fn apply_integration_credentials_update_accepts_temperature() {
        let config = crate::config::Config::default();
        let mut fields = BTreeMap::new();
        fields.insert("default_temperature".to_string(), "0.25".to_string());

        let updated = apply_integration_credentials_update(&config, "ollama", &fields)
            .expect("temperature update should succeed");

        assert!((updated.default_temperature - 0.25).abs() < f64::EPSILON);
    }

    #[test]
    fn apply_integration_credentials_update_rejects_invalid_temperature() {
        let config = crate::config::Config::default();
        let mut fields = BTreeMap::new();
        fields.insert("default_temperature".to_string(), "hot".to_string());

        let err = apply_integration_credentials_update(&config, "ollama", &fields)
            .expect_err("invalid temperature should fail validation");
        assert!(err.contains("default_temperature must be a number"));
    }

    #[test]
    fn normalize_ollama_base_url_strips_api_suffix() {
        let config = crate::config::Config {
            api_url: Some("http://localhost:11434/api/".to_string()),
            ..crate::config::Config::default()
        };

        assert_eq!(normalize_ollama_base_url(&config), "http://localhost:11434");
    }

    #[test]
    fn should_rebalance_ollama_models_detects_model_change() {
        let previous = crate::config::Config {
            default_model: Some("qwen3.5:9b".to_string()),
            ..crate::config::Config::default()
        };
        let updated = crate::config::Config {
            default_model: Some("devstral-small-2:latest".to_string()),
            ..previous.clone()
        };

        assert!(should_rebalance_ollama_models(&previous, &updated));
    }

    #[test]
    fn should_rebalance_ollama_models_ignores_unchanged_selection() {
        let config = crate::config::Config {
            default_model: Some("qwen3.5:9b".to_string()),
            api_url: Some("http://localhost:11434".to_string()),
            ..crate::config::Config::default()
        };

        assert!(!should_rebalance_ollama_models(&config, &config));
    }

    #[test]
    fn config_revision_changes_when_config_changes() {
        let mut config = crate::config::Config::default();
        let initial = config_revision(&config);
        config.default_model = Some("gpt-5.2".to_string());
        let changed = config_revision(&config);
        assert_ne!(initial, changed);
    }
}
