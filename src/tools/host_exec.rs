use super::traits::{Tool, ToolResult};
use crate::host_runner::{
    send_request, HostRunnerOperation, HostRunnerRequest, HostRunnerResult,
    DEFAULT_EXEC_TIMEOUT_SECS,
};
use crate::security::SecurityPolicy;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Explicit bridge from a containerized agent to the opt-in host user service.
pub struct HostExecTool {
    security: Arc<SecurityPolicy>,
    socket_path: PathBuf,
    max_exec_timeout_secs: u64,
}

impl HostExecTool {
    pub fn new(
        security: Arc<SecurityPolicy>,
        socket_path: PathBuf,
        max_exec_timeout_secs: u64,
    ) -> Self {
        Self {
            security,
            socket_path,
            max_exec_timeout_secs,
        }
    }

    fn denied(reason: impl Into<String>) -> ToolResult {
        ToolResult {
            success: false,
            output: String::new(),
            error: Some(reason.into()),
        }
    }

    fn authorize_command(
        &self,
        command: &str,
        cwd: &Path,
        approved: bool,
    ) -> std::result::Result<String, String> {
        if self.security.is_rate_limited() {
            return Err("Rate limit exceeded: too many actions in the last hour".into());
        }

        self.security
            .validate_command_execution(command, approved)?;
        let effective_command = self.security.apply_shell_redirect_policy(command);
        let policy_command = self
            .security
            .command_for_policy_validation(&effective_command)?;
        if let Some(path) = self.security.forbidden_path_argument(&policy_command) {
            return Err(format!("Path blocked by security policy: {path}"));
        }

        let resolved = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
        if !self.security.is_resolved_path_allowed(&resolved) {
            return Err(format!(
                "Host working directory blocked by security policy: {}",
                cwd.display()
            ));
        }
        if !self.security.record_action() {
            return Err("Rate limit exceeded: action budget exhausted".into());
        }
        Ok(effective_command)
    }

    fn authorize_redeploy(&self, approved: bool) -> std::result::Result<(), String> {
        const REDEPLOY_POLICY_COMMAND: &str = "bash ./scripts/docker/up-bundle.sh up -d --build";
        if self.security.is_rate_limited() {
            return Err("Rate limit exceeded: too many actions in the last hour".into());
        }
        self.security
            .validate_command_execution(REDEPLOY_POLICY_COMMAND, approved)?;
        if !self.security.record_action() {
            return Err("Rate limit exceeded: action budget exhausted".into());
        }
        Ok(())
    }

    fn required_string(args: &serde_json::Value, name: &str) -> Result<String> {
        let value = args
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow::anyhow!("Missing '{name}' parameter"))?;
        Ok(value.to_string())
    }
}

#[async_trait]
impl Tool for HostExecTool {
    fn name(&self) -> &str {
        "host_exec"
    }

    fn description(&self) -> &str {
        "Run policy-checked commands on the Docker host through the explicitly enabled \
         LlamaFarm host-runner user service. Use exec for bounded foreground work, spawn \
         plus status for durable jobs, and redeploy to rebuild/recreate LlamaFarm without \
         losing the job when the current container is replaced. This tool targets the host; \
         use shell for work inside the current runtime."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["health", "exec", "spawn", "status", "redeploy"],
                    "description": "Host-runner operation"
                },
                "command": {
                    "type": "string",
                    "description": "Command for exec or spawn; checked by the active command policy"
                },
                "cwd": {
                    "type": "string",
                    "description": "Required absolute host working directory for exec or spawn"
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": self.max_exec_timeout_secs,
                    "default": DEFAULT_EXEC_TIMEOUT_SECS,
                    "description": "Foreground exec timeout"
                },
                "job_id": {
                    "type": "string",
                    "description": "Job id returned by spawn or redeploy, used with status"
                },
                "approved": {
                    "type": "boolean",
                    "default": false,
                    "description": "Explicit risk approval in supervised mode; full autonomy does not require it"
                }
            },
            "required": ["action"],
            "additionalProperties": false
        })
    }

    async fn execute(&self, args: serde_json::Value) -> Result<ToolResult> {
        let action = Self::required_string(&args, "action")?.to_ascii_lowercase();
        let approved = args
            .get("approved")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let operation = match action.as_str() {
            "health" => HostRunnerOperation::Health,
            "exec" | "spawn" => {
                let command = Self::required_string(&args, "command")?;
                let cwd = PathBuf::from(Self::required_string(&args, "cwd")?);
                if !cwd.is_absolute() {
                    return Ok(Self::denied("cwd must be an absolute host path"));
                }
                let timeout_secs = if action == "exec" {
                    let timeout_secs = args
                        .get("timeout_secs")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(DEFAULT_EXEC_TIMEOUT_SECS);
                    if timeout_secs == 0 || timeout_secs > self.max_exec_timeout_secs {
                        return Ok(Self::denied(format!(
                            "timeout_secs must be between 1 and {}",
                            self.max_exec_timeout_secs
                        )));
                    }
                    Some(timeout_secs)
                } else {
                    None
                };
                let effective_command = match self.authorize_command(&command, &cwd, approved) {
                    Ok(command) => command,
                    Err(reason) => return Ok(Self::denied(reason)),
                };
                if action == "exec" {
                    HostRunnerOperation::Exec {
                        command: effective_command,
                        cwd: Some(cwd),
                        timeout_secs,
                    }
                } else {
                    HostRunnerOperation::Spawn {
                        command: effective_command,
                        cwd: Some(cwd),
                    }
                }
            }
            "status" => HostRunnerOperation::Status {
                job_id: Self::required_string(&args, "job_id")?,
            },
            "redeploy" => {
                if let Err(reason) = self.authorize_redeploy(approved) {
                    return Ok(Self::denied(reason));
                }
                HostRunnerOperation::Redeploy
            }
            _ => {
                return Ok(Self::denied(
                    "action must be one of health, exec, spawn, status, or redeploy",
                ));
            }
        };

        let request = HostRunnerRequest::new(operation);
        let transport_timeout = match &request.operation {
            HostRunnerOperation::Exec { timeout_secs, .. } => Duration::from_secs(
                timeout_secs
                    .unwrap_or(DEFAULT_EXEC_TIMEOUT_SECS)
                    .saturating_add(5),
            ),
            _ => Duration::from_secs(10),
        };
        let response = match send_request(&self.socket_path, &request, transport_timeout).await {
            Ok(response) => response,
            Err(error) => {
                return Ok(Self::denied(format!(
                    "Host runner unavailable at {}: {error}",
                    self.socket_path.display()
                )));
            }
        };
        if !response.success {
            return Ok(Self::denied(response.error.unwrap_or_else(|| {
                "host runner rejected the request".to_string()
            })));
        }

        let Some(result) = response.result else {
            return Ok(Self::denied("host runner returned no result"));
        };
        match result {
            HostRunnerResult::Exec {
                exit_code,
                stdout,
                stderr,
                timed_out,
            } => {
                let success = exit_code == Some(0) && !timed_out;
                let error = if timed_out {
                    Some(format!(
                        "Host command timed out after {} seconds{}",
                        args.get("timeout_secs")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(DEFAULT_EXEC_TIMEOUT_SECS),
                        if stderr.is_empty() {
                            String::new()
                        } else {
                            format!(": {stderr}")
                        }
                    ))
                } else if stderr.is_empty() {
                    None
                } else {
                    Some(stderr)
                };
                Ok(ToolResult {
                    success,
                    output: stdout,
                    error,
                })
            }
            other => Ok(ToolResult {
                success: true,
                output: serde_json::to_string_pretty(&other)
                    .context("serialize host-runner tool result")?,
                error: None,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, ShellRedirectPolicy};

    fn policy(level: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: level,
            workspace_dir: PathBuf::from("/tmp"),
            workspace_only: false,
            allowed_commands: vec!["*".to_string()],
            forbidden_paths: vec![],
            allowed_roots: vec![PathBuf::from("/")],
            max_actions_per_hour: 100,
            max_cost_per_day_cents: 100,
            require_approval_for_medium_risk: true,
            block_high_risk_commands: true,
            shell_redirect_policy: ShellRedirectPolicy::Block,
            shell_env_passthrough: vec![],
            tracker: crate::security::policy::ActionTracker::new(),
        })
    }

    #[test]
    fn supervised_mode_keeps_existing_approval_gate() {
        let tool = HostExecTool::new(
            policy(AutonomyLevel::Supervised),
            PathBuf::from("/tmp/missing.sock"),
            300,
        );

        let error = tool
            .authorize_command("touch test-file", Path::new("/tmp"), false)
            .unwrap_err();

        assert!(error.contains("explicit approval"));
    }

    #[test]
    fn full_mode_does_not_require_interactive_approval() {
        let tool = HostExecTool::new(
            policy(AutonomyLevel::Full),
            PathBuf::from("/tmp/missing.sock"),
            300,
        );

        assert!(tool
            .authorize_command("touch test-file", Path::new("/tmp"), false)
            .is_ok());
    }
}
