//! REST API handlers for the web dashboard.
//!
//! All `/api/*` routes require bearer token authentication (PairingGuard).

use super::{build_gateway_runtime_snapshot, AppState};
use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json},
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path as FsPath};

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
    match schedule_kind.unwrap_or("cron").trim().to_ascii_lowercase().as_str() {
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
const OLLAMA_FALLBACK_MODELS: &[&str] = &[
    "qwen3.5:9b",
    "devstral-small-2:latest",
    "devstral-2:123b-cloud",
    "llama3.2",
];

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
    let raw = config.api_url.as_deref().unwrap_or("http://localhost:11434");
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
    if !fields.contains_key("default_model") && !has_non_empty(updated.default_model.as_deref()) {
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
                Json(serde_json::json!({ "error": error, "allowed_files": WORKSPACE_EDITOR_FILES })),
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
                Json(serde_json::json!({ "error": error, "allowed_files": WORKSPACE_EDITOR_FILES })),
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

    let runtime_snapshot = match build_gateway_runtime_snapshot(&new_config) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Config cannot be applied live: {error}")})),
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

    let runtime_snapshot = match build_gateway_runtime_snapshot(&updated) {
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
