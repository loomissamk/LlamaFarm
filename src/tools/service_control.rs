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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::command_runner::{run_capped_command, CappedStream, CommandExecution};
use super::traits::{Tool, ToolResult};
use crate::host_runner::{
    send_request_with_timeouts, HostRunnerOperation, HostRunnerRequest, HostRunnerResult,
};
use crate::security::{NoopSandbox, Sandbox, SecurityPolicy};

const MAX_OUTPUT: usize = 524_288; // 512 KB
/// Default command timeout. Zero means unlimited.
const DEFAULT_TIMEOUT_SECS: u64 = 0;

/// Read-only operations that are always permitted.
const READONLY_OPS: &[&str] = &[
    "status",
    "logs",
    "is-active",
    "is-enabled",
    "is-failed",
    "list-units",
];

pub struct ServiceControlTool {
    security: Arc<SecurityPolicy>,
    sandbox: Arc<dyn Sandbox>,
    host_runner: Option<HostRunnerTarget>,
}

struct HostRunnerTarget {
    socket_path: PathBuf,
    max_exec_timeout_secs: u64,
}

impl ServiceControlTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self::new_with_sandbox(security, Arc::new(NoopSandbox))
    }

    pub fn new_with_sandbox(security: Arc<SecurityPolicy>, sandbox: Arc<dyn Sandbox>) -> Self {
        Self {
            security,
            sandbox,
            host_runner: None,
        }
    }

    /// Route service commands to the host user service instead of executing
    /// them in the current runtime (normally the bundled container).
    pub fn with_host_runner(mut self, socket_path: PathBuf, max_exec_timeout_secs: u64) -> Self {
        self.host_runner = Some(HostRunnerTarget {
            socket_path,
            max_exec_timeout_secs,
        });
        self
    }

    async fn run(&self, argv: &[String], timeout_secs: u64) -> anyhow::Result<ToolResult> {
        if let Some(host_runner) = &self.host_runner {
            return run_host_argv(argv, timeout_secs, host_runner).await;
        }
        run_argv(argv, timeout_secs, self.sandbox.as_ref()).await
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
        permitted. Mutating ops require chaos_lab mode. When the host runner is configured, \
        commands target the host; otherwise they target the current runtime. Set user_scope=true \
        to manage the current user's systemd units without requiring system-level privileges."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let mut timeout_schema = json!({
            "type": "integer",
            "minimum": 0,
            "description": "Optional command deadline in seconds; 0 means unlimited",
            "default": DEFAULT_TIMEOUT_SECS
        });
        if let Some(maximum) = self
            .host_runner
            .as_ref()
            .map(|target| target.max_exec_timeout_secs)
            .filter(|maximum| *maximum > 0)
        {
            timeout_schema["maximum"] = json!(maximum);
        }

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
                "user_scope": {
                    "type": "boolean",
                    "description": "Use the current user's systemd manager (systemctl --user / journalctl --user)",
                    "default": false
                },
                "timeout_secs": timeout_schema
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let operation = args.get("operation").and_then(|v| v.as_str()).unwrap_or("");
        let unit = args.get("unit").and_then(|v| v.as_str());
        let lines = args.get("lines").and_then(|v| v.as_u64()).unwrap_or(50);
        let use_sysv = args
            .get("use_sysv")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let user_scope = args
            .get("user_scope")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if user_scope && use_sysv {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("user_scope is only supported with systemd, not SysV".to_string()),
            });
        }
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        if let Some(maximum) = self
            .host_runner
            .as_ref()
            .map(|target| target.max_exec_timeout_secs)
            .filter(|maximum| *maximum > 0)
        {
            if timeout_secs > maximum {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "timeout_secs must be 0 (unlimited) or at most {maximum}"
                    )),
                });
            }
        }

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
            let mut argv = vec!["journalctl".to_string()];
            if user_scope {
                argv.push("--user".to_string());
            }
            argv.extend([
                "-u".to_string(),
                unit_name.to_string(),
                "-n".to_string(),
                lines.to_string(),
                "--no-pager".to_string(),
            ]);
            return self.run(&argv, timeout_secs).await;
        }

        // Special case: list-units
        if operation == "list-units" {
            let mut argv = vec!["systemctl".to_string()];
            if user_scope {
                argv.push("--user".to_string());
            }
            argv.extend([
                "list-units".to_string(),
                "--no-pager".to_string(),
                "--no-legend".to_string(),
            ]);
            return self.run(&argv, timeout_secs).await;
        }

        // daemon-reload doesn't need a unit name
        if operation == "daemon-reload" {
            let mut argv = vec!["systemctl".to_string()];
            if user_scope {
                argv.push("--user".to_string());
            }
            argv.push("daemon-reload".to_string());
            return self.run(&argv, timeout_secs).await;
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
            let mut v = vec!["systemctl".to_string()];
            if user_scope {
                v.push("--user".to_string());
            }
            v.extend([operation.to_string(), unit_name.to_string()]);
            if operation == "status" {
                v.push("--no-pager".to_string());
            }
            v
        };

        self.run(&argv, timeout_secs).await
    }
}

// ── Execution ──────────────────────────────────────────────────────

async fn run_argv(
    argv: &[String],
    timeout_secs: u64,
    sandbox: &dyn Sandbox,
) -> anyhow::Result<ToolResult> {
    let program = &argv[0];
    let rest = &argv[1..];

    let mut cmd = tokio::process::Command::new(program);
    cmd.args(rest);
    if let Err(e) = sandbox.wrap_command(cmd.as_std_mut()) {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(format!("Failed to apply {} sandbox: {e}", sandbox.name())),
        });
    }

    let timeout = (timeout_secs > 0).then(|| Duration::from_secs(timeout_secs));
    let execution = match run_capped_command(cmd, MAX_OUTPUT, timeout).await {
        Ok(execution) => execution,
        Err(error) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute '{program}': {error}")),
            });
        }
    };

    let command_output = match execution {
        CommandExecution::Completed(output) => output,
        CommandExecution::TimedOut => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Timed out after {timeout_secs}s")),
            });
        }
    };

    let stdout = render_stream(command_output.stdout, "stdout");
    let stderr = render_stream(command_output.stderr, "stderr");
    let success = command_output.status.success();

    // For `status`, a non-zero exit code (unit inactive/failed) is informative,
    // not a tool failure — return the output regardless.
    let output = if stdout.is_empty() && !stderr.is_empty() {
        stderr.clone()
    } else if stdout.is_empty() && stderr.is_empty() {
        format!("exit code: {}", command_output.status.code().unwrap_or(-1))
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

async fn run_host_argv(
    argv: &[String],
    timeout_secs: u64,
    target: &HostRunnerTarget,
) -> anyhow::Result<ToolResult> {
    let request = HostRunnerRequest::new(HostRunnerOperation::Exec {
        command: argv_to_shell_command(argv),
        cwd: None,
        timeout_secs: Some(timeout_secs),
    });
    let response_timeout =
        (timeout_secs > 0).then(|| Duration::from_secs(timeout_secs.saturating_add(5)));
    let response = match send_request_with_timeouts(
        &target.socket_path,
        &request,
        Duration::from_secs(10),
        response_timeout,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Host runner unavailable at {}: {error}",
                    target.socket_path.display()
                )),
            });
        }
    };
    if !response.success {
        return Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some(
                response
                    .error
                    .unwrap_or_else(|| "host runner rejected service command".to_string()),
            ),
        });
    }

    match response.result {
        Some(HostRunnerResult::Exec {
            exit_code,
            stdout,
            stderr,
            timed_out,
        }) => {
            let success = exit_code == Some(0) && !timed_out;
            let output = if stdout.is_empty() && !stderr.is_empty() {
                stderr.clone()
            } else if stdout.is_empty() && stderr.is_empty() {
                format!("exit code: {}", exit_code.unwrap_or(-1))
            } else {
                stdout
            };
            let error = if timed_out {
                Some(format!("Timed out after {timeout_secs}s"))
            } else if success || stderr.is_empty() {
                None
            } else {
                Some(stderr)
            };
            Ok(ToolResult {
                success,
                output,
                error,
            })
        }
        Some(_) => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("host runner returned an unexpected result".to_string()),
        }),
        None => Ok(ToolResult {
            success: false,
            output: String::new(),
            error: Some("host runner returned no result".to_string()),
        }),
    }
}

fn argv_to_shell_command(argv: &[String]) -> String {
    argv.iter()
        .map(|argument| shell_quote(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_quote(argument: &str) -> String {
    if !argument.is_empty()
        && argument.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'@')
        })
    {
        return argument.to_string();
    }
    format!("'{}'", argument.replace('\'', "'\"'\"'"))
}

fn render_stream(stream: CappedStream, label: &str) -> String {
    let mut text = String::from_utf8_lossy(&stream.bytes).into_owned();
    if stream.truncated {
        text.push_str(&format!("\n... [{label} truncated at 512KB]"));
    }
    text
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::host_runner::{HostRunnerResponse, HOST_RUNNER_PROTOCOL_VERSION};
    use crate::security::SecurityPolicy;
    #[cfg(unix)]
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    #[cfg(unix)]
    use tokio::net::UnixListener;

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
        let result = tool.execute(json!({"operation": "logs"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("unit"));
    }

    #[tokio::test]
    async fn unknown_op_without_unit_returns_error() {
        let tool = ServiceControlTool::new(permissive());
        let result = tool.execute(json!({"operation": "start"})).await.unwrap();
        assert!(!result.success);
    }

    #[test]
    fn schema_has_required_operation() {
        let schema = ServiceControlTool::new(permissive()).parameters_schema();
        let required = schema["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("operation")));
        assert_eq!(schema["properties"]["timeout_secs"]["default"], 0);
        assert_eq!(schema["properties"]["user_scope"]["default"], false);
    }

    #[test]
    fn host_runner_timeout_limit_is_advertised() {
        let tool =
            ServiceControlTool::new(permissive()).with_host_runner("/tmp/test.sock".into(), 120);
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"]["timeout_secs"]["maximum"], 120);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn configured_host_runner_receives_service_command() {
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("host-runner.sock");
        let listener = UnixListener::bind(&socket_path).unwrap();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let (read_half, mut write_half) = stream.into_split();
            let mut line = String::new();
            BufReader::new(read_half)
                .read_line(&mut line)
                .await
                .unwrap();
            let request: HostRunnerRequest = serde_json::from_str(&line).unwrap();
            match &request.operation {
                HostRunnerOperation::Exec {
                    command,
                    cwd,
                    timeout_secs,
                } => {
                    assert_eq!(
                        command,
                        "systemctl --user status 'fixture service.service' --no-pager"
                    );
                    assert_eq!(cwd, &None);
                    assert_eq!(timeout_secs, &Some(0));
                }
                operation => panic!("unexpected operation: {operation:?}"),
            }
            let response = HostRunnerResponse {
                protocol_version: HOST_RUNNER_PROTOCOL_VERSION,
                request_id: request.request_id,
                success: true,
                result: Some(HostRunnerResult::Exec {
                    exit_code: Some(0),
                    stdout: "host service active\n".to_string(),
                    stderr: String::new(),
                    timed_out: false,
                }),
                error: None,
            };
            let mut wire = serde_json::to_vec(&response).unwrap();
            wire.push(b'\n');
            write_half.write_all(&wire).await.unwrap();
        });

        let tool = ServiceControlTool::new(permissive()).with_host_runner(socket_path, 0);
        let result = tool
            .execute(json!({
                "operation": "status",
                "unit": "fixture service.service",
                "user_scope": true
            }))
            .await
            .unwrap();

        server.await.unwrap();
        assert!(result.success, "{:?}", result.error);
        assert_eq!(result.output, "host service active\n");
    }
}
