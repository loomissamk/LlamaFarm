//! Ollama model management tool.
//!
//! Provides `list`, `pull`, `delete`, `show`, and `running` actions against the
//! local Ollama daemon REST API. Uses the base URL from config (defaults to
//! `http://localhost:11434`).

use super::traits::{Tool, ToolResult};
use crate::security::{policy::ToolOperation, SecurityPolicy};
use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "http://localhost:11434";
/// Timeout for read-only API calls (list, show, ps).
const READ_TIMEOUT_SECS: u64 = 30;
/// Connect safeguards remain bounded, but model downloads have no total
/// wall-clock deadline because large models can legitimately take hours.
const CONNECT_TIMEOUT_SECS: u64 = 10;

// ── Response shapes ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct TagsResponse {
    models: Vec<ModelEntry>,
}

#[derive(Deserialize)]
struct ModelEntry {
    name: String,
    size: u64,
    #[serde(default)]
    details: ModelDetails,
}

#[derive(Deserialize, Default)]
struct ModelDetails {
    #[serde(default)]
    parameter_size: String,
    #[serde(default)]
    quantization_level: String,
    #[serde(default)]
    family: String,
}

#[derive(Deserialize)]
struct RunningResponse {
    models: Vec<RunningEntry>,
}

#[derive(Deserialize)]
struct RunningEntry {
    name: String,
    #[serde(default)]
    size_vram: u64,
    #[serde(default)]
    expires_at: String,
}

#[derive(Deserialize)]
struct ShowResponse {
    #[serde(default)]
    modelfile: String,
    #[serde(default)]
    parameters: String,
    #[serde(default)]
    details: ModelDetails,
}

#[derive(Deserialize)]
struct PullResponse {
    status: String,
}

// ── Tool ─────────────────────────────────────────────────────────────────────

pub struct OllamaModelTool {
    security: Arc<SecurityPolicy>,
    base_url: String,
}

impl OllamaModelTool {
    pub fn new(security: Arc<SecurityPolicy>, base_url: Option<String>) -> Self {
        let base_url = base_url
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or(DEFAULT_BASE_URL)
            .trim_end_matches('/')
            .to_string();
        Self { security, base_url }
    }

    fn enforce_mutation(&self) -> Result<(), ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Act, "ollama_model")
            .map_err(|msg| ToolResult {
                success: false,
                output: String::new(),
                error: Some(msg),
            })
    }

    fn client(timeout: u64) -> anyhow::Result<Client> {
        Ok(Client::builder()
            .timeout(Duration::from_secs(timeout))
            .build()?)
    }

    fn pull_client() -> Client {
        crate::config::build_runtime_proxy_client_with_optional_timeouts(
            "tool.ollama_model.pull",
            None,
            Some(CONNECT_TIMEOUT_SECS),
        )
    }

    fn format_bytes(bytes: u64) -> String {
        if bytes >= 1_000_000_000 {
            format!("{:.1} GB", bytes as f64 / 1_000_000_000.0)
        } else if bytes >= 1_000_000 {
            format!("{:.0} MB", bytes as f64 / 1_000_000.0)
        } else {
            format!("{} B", bytes)
        }
    }

    async fn handle_list(&self) -> ToolResult {
        let url = format!("{}/api/tags", self.base_url);
        let client = match Self::client(READ_TIMEOUT_SECS) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build HTTP client: {e}")),
                };
            }
        };

        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Cannot reach Ollama at {}: {e}", self.base_url)),
                };
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Ollama /api/tags returned HTTP {status}")),
            };
        }

        let body: TagsResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to parse /api/tags response: {e}")),
                };
            }
        };

        if body.models.is_empty() {
            return ToolResult {
                success: true,
                output: "No models installed.".to_string(),
                error: None,
            };
        }

        let mut lines = vec![format!("{} model(s) installed:", body.models.len())];
        for m in &body.models {
            let size = Self::format_bytes(m.size);
            let family = if m.details.family.is_empty() {
                String::new()
            } else {
                format!("  family={}", m.details.family)
            };
            let params = if m.details.parameter_size.is_empty() {
                String::new()
            } else {
                format!("  params={}", m.details.parameter_size)
            };
            let quant = if m.details.quantization_level.is_empty() {
                String::new()
            } else {
                format!("  quant={}", m.details.quantization_level)
            };
            lines.push(format!("- {}{family}{params}{quant}  size={size}", m.name));
        }

        ToolResult {
            success: true,
            output: lines.join("\n"),
            error: None,
        }
    }

    async fn handle_running(&self) -> ToolResult {
        let url = format!("{}/api/ps", self.base_url);
        let client = match Self::client(READ_TIMEOUT_SECS) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build HTTP client: {e}")),
                };
            }
        };

        let resp = match client.get(&url).send().await {
            Ok(r) => r,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Cannot reach Ollama at {}: {e}", self.base_url)),
                };
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Ollama /api/ps returned HTTP {status}")),
            };
        }

        let body: RunningResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to parse /api/ps response: {e}")),
                };
            }
        };

        if body.models.is_empty() {
            return ToolResult {
                success: true,
                output: "No models currently loaded.".to_string(),
                error: None,
            };
        }

        let mut lines = vec![format!("{} model(s) loaded in memory:", body.models.len())];
        for m in &body.models {
            let vram = if m.size_vram > 0 {
                format!("  vram={}", Self::format_bytes(m.size_vram))
            } else {
                String::new()
            };
            let expires = if m.expires_at.is_empty() {
                String::new()
            } else {
                format!("  expires_at={}", m.expires_at)
            };
            lines.push(format!("- {}{vram}{expires}", m.name));
        }

        ToolResult {
            success: true,
            output: lines.join("\n"),
            error: None,
        }
    }

    async fn handle_show(&self, name: &str) -> ToolResult {
        if name.is_empty() {
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some("Parameter 'name' is required for action 'show'".into()),
            };
        }

        let url = format!("{}/api/show", self.base_url);
        let client = match Self::client(READ_TIMEOUT_SECS) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build HTTP client: {e}")),
                };
            }
        };

        let resp = match client
            .post(&url)
            .json(&json!({ "name": name }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Cannot reach Ollama at {}: {e}", self.base_url)),
                };
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Ollama /api/show returned HTTP {status}: {body}")),
            };
        }

        let body: ShowResponse = match resp.json().await {
            Ok(b) => b,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to parse /api/show response: {e}")),
                };
            }
        };

        let mut lines = vec![format!("Model: {name}")];
        if !body.details.family.is_empty() {
            lines.push(format!("Family: {}", body.details.family));
        }
        if !body.details.parameter_size.is_empty() {
            lines.push(format!("Parameters: {}", body.details.parameter_size));
        }
        if !body.details.quantization_level.is_empty() {
            lines.push(format!("Quantization: {}", body.details.quantization_level));
        }
        if !body.parameters.trim().is_empty() {
            lines.push(format!("Parameters block:\n{}", body.parameters.trim()));
        }
        if !body.modelfile.trim().is_empty() {
            let excerpt = body
                .modelfile
                .lines()
                .take(10)
                .collect::<Vec<_>>()
                .join("\n");
            lines.push(format!("Modelfile (first 10 lines):\n{excerpt}"));
        }

        ToolResult {
            success: true,
            output: lines.join("\n"),
            error: None,
        }
    }

    async fn handle_pull(&self, name: &str) -> ToolResult {
        if name.is_empty() {
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some("Parameter 'name' is required for action 'pull'".into()),
            };
        }

        let url = format!("{}/api/pull", self.base_url);
        let client = Self::pull_client();

        // Stream progress so long downloads keep producing response traffic;
        // only the final status is retained for the tool result.
        let mut resp = match client
            .post(&url)
            .json(&json!({ "name": name, "stream": true }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Cannot reach Ollama at {}: {e}", self.base_url)),
                };
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Ollama /api/pull returned HTTP {status}: {body}")),
            };
        }

        let mut pending = Vec::new();
        let mut last_status = None;
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => {
                    pending.extend_from_slice(&chunk);
                    while let Some(newline) = pending.iter().position(|byte| *byte == b'\n') {
                        let line = pending.drain(..=newline).collect::<Vec<_>>();
                        update_pull_status(&line, &mut last_status);
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    return ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("Ollama pull stream failed: {error}")),
                    };
                }
            }
        }
        update_pull_status(&pending, &mut last_status);
        let status = last_status.unwrap_or_else(|| "unknown".to_string());

        ToolResult {
            success: status == "success",
            output: format!("Pull {name}: {status}"),
            error: if status == "success" {
                None
            } else {
                Some(format!("Unexpected pull status: {status}"))
            },
        }
    }

    async fn handle_delete(&self, name: &str) -> ToolResult {
        if name.is_empty() {
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some("Parameter 'name' is required for action 'delete'".into()),
            };
        }

        let url = format!("{}/api/delete", self.base_url);
        let client = match Self::client(READ_TIMEOUT_SECS) {
            Ok(c) => c,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build HTTP client: {e}")),
                };
            }
        };

        let resp = match client
            .delete(&url)
            .json(&json!({ "name": name }))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Cannot reach Ollama at {}: {e}", self.base_url)),
                };
            }
        };

        match resp.status().as_u16() {
            200 => ToolResult {
                success: true,
                output: format!("Deleted model '{name}'."),
                error: None,
            },
            404 => ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Model '{name}' not found.")),
            },
            status => {
                let body = resp.text().await.unwrap_or_default();
                ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Ollama /api/delete returned HTTP {status}: {body}")),
                }
            }
        }
    }
}

fn update_pull_status(line: &[u8], last_status: &mut Option<String>) {
    let line = String::from_utf8_lossy(line);
    let line = line.trim();
    if line.is_empty() {
        return;
    }
    if let Ok(status) = serde_json::from_str::<PullResponse>(line) {
        *last_status = Some(status.status);
    }
}

#[async_trait]
impl Tool for OllamaModelTool {
    fn name(&self) -> &str {
        "ollama_model"
    }

    fn description(&self) -> &str {
        "Manage Ollama models on this machine. \
         List installed models, pull new ones, delete unused ones, \
         inspect a model's details, or check which models are loaded in memory."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list", "pull", "delete", "show", "running"],
                    "description": "Operation: list=all installed, pull=download, delete=remove, show=model info, running=loaded in VRAM"
                },
                "name": {
                    "type": "string",
                    "description": "Model name/tag (required for pull, delete, show). E.g. 'qwen3.5:9b', 'llama3.2:3b'."
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("list");

        let name = args
            .get("name")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .unwrap_or_default();

        match action {
            "list" => Ok(self.handle_list().await),
            "running" => Ok(self.handle_running().await),
            "show" => Ok(self.handle_show(name).await),
            "pull" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                Ok(self.handle_pull(name).await)
            }
            "delete" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                Ok(self.handle_delete(name).await)
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Valid: list, pull, delete, show, running"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn default_tool() -> OllamaModelTool {
        OllamaModelTool::new(Arc::new(SecurityPolicy::default()), None)
    }

    fn readonly_tool() -> OllamaModelTool {
        OllamaModelTool::new(
            Arc::new(SecurityPolicy {
                autonomy: AutonomyLevel::ReadOnly,
                ..SecurityPolicy::default()
            }),
            None,
        )
    }

    #[test]
    fn tool_name_and_schema() {
        let tool = default_tool();
        assert_eq!(tool.name(), "ollama_model");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["name"].is_object());
    }

    #[test]
    fn default_base_url() {
        let tool = default_tool();
        assert_eq!(tool.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn custom_base_url_trimmed() {
        let tool = OllamaModelTool::new(
            Arc::new(SecurityPolicy::default()),
            Some("http://10.0.0.1:11434/".to_string()),
        );
        assert_eq!(tool.base_url, "http://10.0.0.1:11434");
    }

    #[test]
    fn empty_base_url_uses_default() {
        let tool = OllamaModelTool::new(Arc::new(SecurityPolicy::default()), Some(String::new()));
        assert_eq!(tool.base_url, DEFAULT_BASE_URL);
    }

    #[test]
    fn pull_status_tracks_final_stream_event() {
        let mut status = None;
        update_pull_status(br#"{"status":"pulling manifest"}"#, &mut status);
        update_pull_status(br#"{"status":"success"}"#, &mut status);
        assert_eq!(status.as_deref(), Some("success"));
    }

    #[tokio::test]
    async fn unknown_action_returns_failure() {
        let tool = default_tool();
        let r = tool.execute(json!({ "action": "nope" })).await.unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn pull_requires_name() {
        let tool = default_tool();
        let r = tool.execute(json!({ "action": "pull" })).await.unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("name"));
    }

    #[tokio::test]
    async fn delete_requires_name() {
        let tool = default_tool();
        let r = tool.execute(json!({ "action": "delete" })).await.unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("name"));
    }

    #[tokio::test]
    async fn show_requires_name() {
        let tool = default_tool();
        let r = tool.execute(json!({ "action": "show" })).await.unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("name"));
    }

    #[tokio::test]
    async fn readonly_blocks_pull_and_delete() {
        let tool = readonly_tool();
        for action in &["pull", "delete"] {
            let r = tool
                .execute(json!({ "action": action, "name": "llama3.2:3b" }))
                .await
                .unwrap();
            assert!(
                !r.success,
                "action '{action}' should be blocked in read-only"
            );
            assert!(r.error.unwrap().contains("read-only"));
        }
    }
}
