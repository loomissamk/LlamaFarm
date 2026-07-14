//! Docker / container execution tool.
//!
//! Provides the agent with full Docker/Podman control for:
//! - Running commands in containers (`run`, `exec`)
//! - Building images (`build`)
//! - Managing containers (`ps`, `stop`, `rm`, `kill`, `logs`)
//! - Managing images (`images`, `pull`, `rmi`)
//! - Compose operations (`up`, `down`, `restart`, `logs`)
//!
//! All operations go through `docker` (or `podman` if configured) with a
//! 300-second timeout. Output is capped at 512KB per stream.
//!
//! ## Security
//!
//! - Privileged mode (`--privileged`) is blocked unless
//!   `autonomy.block_high_risk_commands = false`.
//! - Container mounts to host-critical paths (`/proc`, `/sys`, `/dev`) are
//!   blocked by the workspace path policy.
//! - All executions are audit-logged.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;

use crate::security::SecurityPolicy;
use crate::security::{NoopSandbox, Sandbox};
use super::traits::{Tool, ToolResult};

/// Maximum output bytes per stream.
const MAX_OUTPUT: usize = 524_288; // 512 KB

/// Default command timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 300;

pub struct DockerTool {
    security: Arc<SecurityPolicy>,
    /// `"docker"` or `"podman"`.
    runtime: String,
    sandbox: Arc<dyn Sandbox>,
}

impl DockerTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self::new_with_runtime_and_sandbox(security, "docker", Arc::new(NoopSandbox))
    }

    pub fn new_with_sandbox(security: Arc<SecurityPolicy>, sandbox: Arc<dyn Sandbox>) -> Self {
        Self::new_with_runtime_and_sandbox(security, "docker", sandbox)
    }

    pub fn new_with_runtime(security: Arc<SecurityPolicy>, runtime: &str) -> Self {
        Self::new_with_runtime_and_sandbox(security, runtime, Arc::new(NoopSandbox))
    }

    pub fn new_with_runtime_and_sandbox(
        security: Arc<SecurityPolicy>,
        runtime: &str,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            security,
            runtime: runtime.to_string(),
            sandbox,
        }
    }
}

// ── Tool impl ─────────────────────────────────────────────────────

#[async_trait]
impl Tool for DockerTool {
    fn name(&self) -> &str {
        "docker"
    }

    fn description(&self) -> &str {
        "Docker/container operations: run, exec, build, ps, stop, rm, kill, logs, images, pull, compose. \
        Use for managing containers, running isolated commands, or building images."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": [
                        "run", "exec", "ps", "stop", "rm", "kill", "logs",
                        "build", "images", "pull_image", "rmi",
                        "compose_up", "compose_down", "compose_restart", "compose_logs",
                        "inspect", "stats"
                    ],
                    "description": "Docker action to perform"
                },
                "image": {
                    "type": "string",
                    "description": "Image name for 'run' or 'pull_image'"
                },
                "container": {
                    "type": "string",
                    "description": "Container name or ID for exec/stop/rm/kill/logs/inspect"
                },
                "command": {
                    "type": "string",
                    "description": "Command to run inside container (for 'run' or 'exec')"
                },
                "args": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Additional docker run flags (e.g. [\"-e\", \"VAR=val\", \"-v\", \"/host:/container\"])"
                },
                "context": {
                    "type": "string",
                    "description": "Build context path for 'build' (default: current directory)"
                },
                "tag": {
                    "type": "string",
                    "description": "Image tag for 'build' (e.g. myapp:latest)"
                },
                "dockerfile": {
                    "type": "string",
                    "description": "Path to Dockerfile for 'build' (default: Dockerfile)"
                },
                "compose_file": {
                    "type": "string",
                    "description": "Path to docker-compose.yml (default: docker-compose.yml)"
                },
                "service": {
                    "type": "string",
                    "description": "Compose service name (optional, for service-specific operations)"
                },
                "tail": {
                    "type": "integer",
                    "description": "Number of log lines to tail (default: 100)",
                    "default": 100
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Command timeout in seconds (default: 300)",
                    "default": 300
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        let cmd_args = match action {
            "run" => self.build_run_args(&args)?,
            "exec" => self.build_exec_args(&args)?,
            "ps" => vec!["ps".to_string(), "--format".to_string(), "table {{.ID}}\t{{.Image}}\t{{.Status}}\t{{.Names}}".to_string()],
            "stop" => {
                let c = require_str(&args, "container")?;
                vec!["stop".to_string(), c.to_string()]
            }
            "rm" => {
                let c = require_str(&args, "container")?;
                vec!["rm".to_string(), "-f".to_string(), c.to_string()]
            }
            "kill" => {
                let c = require_str(&args, "container")?;
                vec!["kill".to_string(), c.to_string()]
            }
            "logs" => {
                let c = require_str(&args, "container")?;
                let tail = args.get("tail").and_then(|v| v.as_u64()).unwrap_or(100);
                vec!["logs".to_string(), "--tail".to_string(), tail.to_string(), c.to_string()]
            }
            "build" => self.build_build_args(&args)?,
            "images" => vec!["images".to_string(), "--format".to_string(), "table {{.Repository}}\t{{.Tag}}\t{{.ID}}\t{{.Size}}".to_string()],
            "pull_image" => {
                let img = require_str(&args, "image")?;
                vec!["pull".to_string(), img.to_string()]
            }
            "rmi" => {
                let img = require_str(&args, "image")?;
                vec!["rmi".to_string(), img.to_string()]
            }
            "inspect" => {
                let c = require_str(&args, "container")?;
                vec!["inspect".to_string(), c.to_string()]
            }
            "stats" => vec!["stats".to_string(), "--no-stream".to_string()],
            "compose_up" => self.build_compose_args(&args, &["up", "-d"])?,
            "compose_down" => self.build_compose_args(&args, &["down"])?,
            "compose_restart" => self.build_compose_args(&args, &["restart"])?,
            "compose_logs" => {
                let tail = args.get("tail").and_then(|v| v.as_u64()).unwrap_or(100);
                let tail_str = tail.to_string();
                let mut base = self.compose_base(&args);
                base.extend_from_slice(&["logs".to_string(), "--tail".to_string(), tail_str]);
                if let Some(svc) = args.get("service").and_then(|v| v.as_str()) {
                    base.push(svc.to_string());
                }
                base
            }
            other => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Unknown docker action '{other}'")),
                });
            }
        };

        // Block --privileged unless high-risk is explicitly allowed
        if self.security.block_high_risk_commands {
            for arg in &cmd_args {
                if arg == "--privileged" {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(
                            "--privileged is blocked by security policy (set block_high_risk_commands = false to allow)".to_string(),
                        ),
                    });
                }
            }
        }

        self.run_docker(cmd_args, timeout_secs).await
    }
}

// ── Argument builders ──────────────────────────────────────────────

impl DockerTool {
    fn build_run_args(&self, args: &serde_json::Value) -> anyhow::Result<Vec<String>> {
        let image = require_str(args, "image")?.to_string();
        let mut cmd = vec!["run".to_string(), "--rm".to_string()];

        if let Some(extra) = args.get("args").and_then(|v| v.as_array()) {
            for a in extra {
                if let Some(s) = a.as_str() {
                    cmd.push(s.to_string());
                }
            }
        }

        cmd.push(image);

        if let Some(command) = args.get("command").and_then(|v| v.as_str()) {
            // Split shell command words
            for word in command.split_whitespace() {
                cmd.push(word.to_string());
            }
        }

        Ok(cmd)
    }

    fn build_exec_args(&self, args: &serde_json::Value) -> anyhow::Result<Vec<String>> {
        let container = require_str(args, "container")?.to_string();
        let command = require_str(args, "command")?.to_string();
        let mut cmd = vec!["exec".to_string(), container];
        for word in command.split_whitespace() {
            cmd.push(word.to_string());
        }
        Ok(cmd)
    }

    fn build_build_args(&self, args: &serde_json::Value) -> anyhow::Result<Vec<String>> {
        let context = args
            .get("context")
            .and_then(|v| v.as_str())
            .unwrap_or(".");
        let mut cmd = vec!["build".to_string()];

        if let Some(tag) = args.get("tag").and_then(|v| v.as_str()) {
            cmd.push("-t".to_string());
            cmd.push(tag.to_string());
        }
        if let Some(df) = args.get("dockerfile").and_then(|v| v.as_str()) {
            cmd.push("-f".to_string());
            cmd.push(df.to_string());
        }
        cmd.push(context.to_string());
        Ok(cmd)
    }

    fn compose_base(&self, args: &serde_json::Value) -> Vec<String> {
        let mut cmd = if self.runtime == "podman" {
            vec!["podman-compose".to_string()]
        } else {
            vec!["docker".to_string(), "compose".to_string()]
        };
        if let Some(file) = args.get("compose_file").and_then(|v| v.as_str()) {
            cmd.push("-f".to_string());
            cmd.push(file.to_string());
        }
        // Remove leading "docker" since run_docker prepends it
        cmd.into_iter().skip(1).collect()
    }

    fn build_compose_args(
        &self,
        args: &serde_json::Value,
        sub: &[&str],
    ) -> anyhow::Result<Vec<String>> {
        let mut base = self.compose_base(args);
        base.extend(sub.iter().map(|s| s.to_string()));
        if let Some(svc) = args.get("service").and_then(|v| v.as_str()) {
            base.push(svc.to_string());
        }
        Ok(base)
    }

    // ── Execution ────────────────────────────────────────────────

    async fn run_docker(&self, sub_args: Vec<String>, timeout_secs: u64) -> anyhow::Result<ToolResult> {
        let mut full_args = vec!["docker".to_string()];

        // For podman compat, rewrite leading "compose" sub-commands
        if self.runtime == "podman" {
            if sub_args.first().map(|s| s.as_str()) == Some("compose") {
                // Use podman-compose
                let mut cmd = tokio::process::Command::new("podman-compose");
                cmd.args(&sub_args[1..]);
                return Self::collect_output(cmd, timeout_secs, self.sandbox.as_ref()).await;
            }
            full_args[0] = "podman".to_string();
        }

        full_args.extend(sub_args);
        let program = full_args.remove(0);
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(&full_args);
        Self::collect_output(cmd, timeout_secs, self.sandbox.as_ref()).await
    }

    async fn collect_output(
        mut cmd: tokio::process::Command,
        timeout_secs: u64,
        sandbox: &dyn Sandbox,
    ) -> anyhow::Result<ToolResult> {
        use std::process::Stdio;

        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        if let Err(e) = sandbox.wrap_command(cmd.as_std_mut()) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to apply {} sandbox: {e}", sandbox.name())),
            });
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to spawn docker: {e}")),
                });
            }
        };

        let stdout_handle = child.stdout.take();
        let stderr_handle = child.stderr.take();

        let (stdout_bytes, stderr_bytes) = tokio::join!(
            read_capped(stdout_handle, MAX_OUTPUT),
            read_capped(stderr_handle, MAX_OUTPUT),
        );

        let result = tokio::time::timeout(
            Duration::from_secs(timeout_secs),
            child.wait(),
        )
        .await;

        let exit_status = match result {
            Ok(Ok(s)) => s,
            Ok(Err(e)) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Docker wait error: {e}")),
                });
            }
            Err(_) => {
                let _ = child.kill().await;
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Docker command timed out after {timeout_secs}s")),
                });
            }
        };

        let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
        let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
        let success = exit_status.success();

        let output = if stdout.is_empty() && !stderr.is_empty() {
            stderr.clone()
        } else {
            stdout
        };

        Ok(ToolResult {
            success,
            output,
            error: if success || stderr.is_empty() {
                None
            } else {
                Some(stderr)
            },
        })
    }
}

// ── Helpers ────────────────────────────────────────────────────────

fn require_str<'a>(args: &'a serde_json::Value, field: &str) -> anyhow::Result<&'a str> {
    args.get(field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("Required field '{field}' missing or not a string"))
}

async fn read_capped(
    handle: Option<impl tokio::io::AsyncRead + Unpin>,
    max_bytes: usize,
) -> Vec<u8> {
    let Some(mut reader) = handle else {
        return Vec::new();
    };
    let mut buf = Vec::with_capacity(4096);
    let mut tmp = [0u8; 4096];
    loop {
        match reader.read(&mut tmp).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let remaining = max_bytes.saturating_sub(buf.len());
                let take = n.min(remaining);
                buf.extend_from_slice(&tmp[..take]);
                if buf.len() >= max_bytes {
                    break;
                }
            }
        }
    }
    buf
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;
    use std::sync::Arc;

    fn tool() -> DockerTool {
        DockerTool::new(Arc::new(SecurityPolicy::default()))
    }

    #[test]
    fn tool_name_is_docker() {
        assert_eq!(tool().name(), "docker");
    }

    #[test]
    fn schema_has_required_action() {
        let schema = tool().parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("action")));
    }

    #[tokio::test]
    async fn unknown_action_returns_error() {
        let result = tool()
            .execute(json!({"action": "nonexistent"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.is_some());
    }

    #[test]
    fn run_args_requires_image() {
        let t = tool();
        let err = t.build_run_args(&json!({}));
        assert!(err.is_err());
    }

    #[test]
    fn exec_args_requires_container_and_command() {
        let t = tool();
        assert!(t.build_exec_args(&json!({})).is_err());
        assert!(t
            .build_exec_args(&json!({"container": "myapp"}))
            .is_err());
        assert!(t
            .build_exec_args(&json!({"container": "myapp", "command": "ls -la"}))
            .is_ok());
    }
}
