//! Service control tool.
//!
//! Provides the agent with systemd / SysV service management:
//! - systemctl (start, stop, restart, enable, disable, status, reload, daemon-reload)
//! - service (SysV fallback for systems without systemd)
//! - journalctl (tail recent log lines for a unit)
//!
//! ## Security
//!
//! All operations except `status` and `logs` are **blocked** unless
//! `autonomy.block_high_risk_commands = false` (chaos_lab or full-autonomy mode).
//!
//! All executions are audit-logged.

use async_trait::async_trait;
use serde_json::json;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;

use crate::security::SecurityPolicy;
use super::traits::{Tool, ToolResult};

const MAX_OUTPUT: usize = 524_288; // 512 KB
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Read-only operations that are always permitted.
const READONLY_OPS: &[&str] = &["status", "logs", "is-active", "is-enabled", "is-failed", "list-units"];

pub struct ServiceControlTool {
    security: Arc<SecurityPolicy>,
}

impl ServiceControlTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for ServiceControlTool {
    fn name(&self) -> &str {
        "service_control"
    }

    fn description(&self) -> &str {
        "Control systemd/SysV services: start, stop, restart, enable, disable, status, reload, \
        daemon-reload, logs (journalctl). Read-only ops (status/logs/is-active) are always \
        permitted. Mutating ops require chaos_lab mode."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": [
                        "start", "stop", "restart", "reload", "enable", "disable",
                        "status", "logs", "daemon-reload",
                        "is-active", "is-enabled", "is-failed", "list-units"
                    ],
                    "description": "Service operation to perform"
                },
                "unit": {
                    "type": "string",
                    "description": "Service/unit name (e.g. nginx, sshd, docker.service). \
                        Not required for daemon-reload or list-units."
                },
                "lines": {
                    "type": "integer",
                    "description": "Number of journal log lines to return for 'logs' (default: 50)",
                    "default": 50
                },
                "use_sysv": {
                    "type": "boolean",
                    "description": "Use 'service' command instead of 'systemctl' (SysV fallback)",
                    "default": false
                },
                "timeout_secs": {
                    "type": "integer",
                    "description": "Command timeout in seconds (default: 30)",
                    "default": 30
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let operation = args
            .get("operation")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let unit = args.get("unit").and_then(|v| v.as_str());
        let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(50);
        let use_sysv = args.get("use_sysv").and_then(|v| v.as_bool()).unwrap_or(false);
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        // Gate mutating operations.
        if !READONLY_OPS.contains(&operation) && self.security.block_high_risk_commands {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "service_control '{operation}' is blocked by security policy. \
                    Set block_high_risk_commands = false (chaos_lab mode) to allow."
                )),
            });
        }

        // Special case: journalctl for logs
        if operation == "logs" {
            let unit_name = match unit {
                Some(u) => u,
                None => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("'unit' is required for 'logs' operation".to_string()),
                    });
                }
            };
            let argv = vec![
                "journalctl".to_string(),
                "-u".to_string(),
                unit_name.to_string(),
                "-n".to_string(),
                lines.to_string(),
                "--no-pager".to_string(),
            ];
            return run_argv(&argv, timeout_secs).await;
        }

        // Special case: list-units
        if operation == "list-units" {
            let argv = vec![
                "systemctl".to_string(),
                "list-units".to_string(),
                "--no-pager".to_string(),
                "--no-legend".to_string(),
            ];
            return run_argv(&argv, timeout_secs).await;
        }

        // daemon-reload doesn't need a unit name
        if operation == "daemon-reload" {
            let argv = vec![
                "systemctl".to_string(),
                "daemon-reload".to_string(),
            ];
            return run_argv(&argv, timeout_secs).await;
        }

        // All other operations require a unit name.
        let unit_name = match unit {
            Some(u) => u,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("'unit' is required for '{operation}'")),
                });
            }
        };

        let argv = if use_sysv {
            // SysV: service <unit> <operation>
            vec![
                "service".to_string(),
                unit_name.to_string(),
                operation.to_string(),
            ]
        } else {
            // systemctl: systemctl <operation> <unit> [--no-pager]
            let mut v = vec![
                "systemctl".to_string(),
                operation.to_string(),
                unit_name.to_string(),
            ];
            if operation == "status" {
                v.push("--no-pager".to_string());
            }
            v
        };

        run_argv(&argv, timeout_secs).await
    }
}

// ── Execution ──────────────────────────────────────────────────────

async fn run_argv(argv: &[String], timeout_secs: u64) -> anyhow::Result<ToolResult> {
    let program = &argv[0];
    let rest = &argv[1..];

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(rest)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to spawn '{program}': {e}")),
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
                error: Some(format!("Wait error: {e}")),
            });
        }
        Err(_) => {
            let _ = child.kill().await;
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Timed out after {timeout_secs}s")),
            });
        }
    };

    let stdout = String::from_utf8_lossy(&stdout_bytes).into_owned();
    let stderr = String::from_utf8_lossy(&stderr_bytes).into_owned();
    let success = exit_status.success();

    // For `status`, a non-zero exit code (unit inactive/failed) is informative,
    // not a tool failure — return the output regardless.
    let output = if stdout.is_empty() && !stderr.is_empty() {
        stderr.clone()
    } else if stdout.is_empty() && stderr.is_empty() {
        format!("exit code: {}", exit_status.code().unwrap_or(-1))
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

    fn permissive() -> Arc<SecurityPolicy> {
        let mut p = SecurityPolicy::default();
        p.block_high_risk_commands = false;
        Arc::new(p)
    }

    fn restrictive() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy::default())
    }

    #[test]
    fn tool_name() {
        assert_eq!(
            ServiceControlTool::new(permissive()).name(),
            "service_control"
        );
    }

    #[test]
    fn readonly_ops_list() {
        assert!(READONLY_OPS.contains(&"status"));
        assert!(READONLY_OPS.contains(&"logs"));
        assert!(READONLY_OPS.contains(&"is-active"));
        assert!(!READONLY_OPS.contains(&"start"));
        assert!(!READONLY_OPS.contains(&"stop"));
    }

    #[tokio::test]
    async fn mutating_op_blocked_by_default() {
        let tool = ServiceControlTool::new(restrictive());
        let result = tool
            .execute(json!({"operation": "restart", "unit": "nginx"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("blocked"));
    }

    #[tokio::test]
    async fn logs_requires_unit() {
        let tool = ServiceControlTool::new(permissive());
        let result = tool
            .execute(json!({"operation": "logs"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("unit"));
    }

    #[tokio::test]
    async fn unknown_op_without_unit_returns_error() {
        let tool = ServiceControlTool::new(permissive());
        let result = tool
            .execute(json!({"operation": "start"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[test]
    fn schema_has_required_operation() {
        let schema = ServiceControlTool::new(permissive()).parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("operation")));
    }
}
