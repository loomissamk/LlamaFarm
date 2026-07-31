//! Package manager tool.
//!
//! Provides the agent with control over system and language package managers:
//! - apt / apt-get (Debian / Ubuntu)
//! - dnf / yum (Fedora / RHEL)
//! - pip / uv (Python)
//! - cargo (Rust workspace)
//!
//! ## Security
//!
//! Install, remove, and upgrade operations are **blocked** unless
//! `autonomy.block_high_risk_commands = false` (i.e. chaos_lab or full-autonomy
//! mode). List and search are always permitted.
//!
//! All executions are audit-logged.

use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

use super::command_runner::{run_capped_command, CappedStream, CommandExecution};
use super::shell::apply_workspace_venv_env;
use super::traits::{Tool, ToolResult};
use crate::security::{NoopSandbox, Sandbox, SecurityPolicy};

/// Maximum output bytes per stream.
const MAX_OUTPUT: usize = 524_288; // 512 KB
/// Default command timeout. Zero means unlimited.
const DEFAULT_TIMEOUT_SECS: u64 = 0;

/// Operations that mutate the system — blocked outside chaos modes.
const MUTATING_OPS: &[&str] = &["install", "remove", "upgrade", "hold"];

pub struct PackageManagerTool {
    security: Arc<SecurityPolicy>,
    sandbox: Arc<dyn Sandbox>,
}

impl PackageManagerTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self::new_with_sandbox(security, Arc::new(NoopSandbox))
    }

    pub fn new_with_sandbox(security: Arc<SecurityPolicy>, sandbox: Arc<dyn Sandbox>) -> Self {
        Self { security, sandbox }
    }
}

fn available_package_managers() -> Vec<&'static str> {
    ["apt", "apt-get", "dnf", "yum", "pip", "uv", "cargo"]
        .into_iter()
        .filter(|manager| which::which(manager).is_ok())
        .collect()
}

#[async_trait]
impl Tool for PackageManagerTool {
    fn name(&self) -> &str {
        "package_manager"
    }

    fn description(&self) -> &str {
        "Install, remove, upgrade, list, or search packages via apt/dnf/pip/cargo. \
        Mutating operations (install/remove/upgrade/hold) require chaos_lab or full-autonomy mode. \
        Use 'manager' to select: apt, dnf, pip, uv, cargo."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let managers = available_package_managers();
        json!({
            "type": "object",
            "properties": {
                "manager": {
                    "type": "string",
                    "enum": managers,
                    "description": "Package manager available in this runtime"
                },
                "operation": {
                    "type": "string",
                    "enum": ["install", "remove", "upgrade", "list", "search", "hold"],
                    "description": "Operation to perform"
                },
                "packages": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Package names (required for install/remove/hold/search)"
                },
                "yes": {
                    "type": "boolean",
                    "description": "Pass -y / --yes to suppress prompts (default: true)",
                    "default": true
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "Optional command deadline in seconds; 0 means unlimited",
                    "default": DEFAULT_TIMEOUT_SECS
                }
            },
            "required": ["manager", "operation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let manager = args.get("manager").and_then(|v| v.as_str()).unwrap_or("");
        let operation = args.get("operation").and_then(|v| v.as_str()).unwrap_or("");
        let yes = args.get("yes").and_then(|v| v.as_bool()).unwrap_or(true);
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);

        if !available_package_managers().contains(&manager) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Package manager '{manager}' is not installed in this runtime"
                )),
            });
        }

        let packages: Vec<String> = args
            .get("packages")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();

        // Gate mutating operations.
        if MUTATING_OPS.contains(&operation) && self.security.block_high_risk_commands {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "package_manager '{operation}' is blocked by security policy. \
                    Set block_high_risk_commands = false (chaos_lab mode) to allow."
                )),
            });
        }

        let argv = build_argv(manager, operation, &packages, yes)?;
        if argv.is_empty() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unsupported manager/operation combination: {manager} {operation}"
                )),
            });
        }

        run_command(
            &argv[0],
            &argv[1..],
            timeout_secs,
            self.sandbox.as_ref(),
            &self.security.workspace_dir,
        )
        .await
    }
}

// ── Command builders ───────────────────────────────────────────────

fn build_argv(
    manager: &str,
    operation: &str,
    packages: &[String],
    yes: bool,
) -> anyhow::Result<Vec<String>> {
    match (manager, operation) {
        // ── apt / apt-get ──────────────────────────────────────────
        ("apt" | "apt-get", "install") => {
            let mut v = vec![manager.to_string(), "install".to_string()];
            if yes {
                v.push("-y".to_string());
            }
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("apt" | "apt-get", "remove") => {
            let mut v = vec![manager.to_string(), "remove".to_string()];
            if yes {
                v.push("-y".to_string());
            }
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("apt" | "apt-get", "upgrade") => {
            let mut v = if packages.is_empty() {
                vec![manager.to_string(), "upgrade".to_string()]
            } else {
                vec![
                    manager.to_string(),
                    "install".to_string(),
                    "--only-upgrade".to_string(),
                ]
            };
            if yes {
                v.push("-y".to_string());
            }
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("apt" | "apt-get", "list") => Ok(vec![
            "apt".to_string(),
            "list".to_string(),
            "--installed".to_string(),
        ]),
        ("apt" | "apt-get", "search") => {
            let mut v = vec!["apt-cache".to_string(), "search".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("apt" | "apt-get", "hold") => {
            // apt-mark hold pkg1 pkg2 ...
            let mut v = vec!["apt-mark".to_string(), "hold".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }

        // ── dnf / yum ─────────────────────────────────────────────
        ("dnf" | "yum", "install") => {
            let mut v = vec![manager.to_string(), "install".to_string()];
            if yes {
                v.push("-y".to_string());
            }
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("dnf" | "yum", "remove") => {
            let mut v = vec![manager.to_string(), "remove".to_string()];
            if yes {
                v.push("-y".to_string());
            }
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("dnf" | "yum", "upgrade") => {
            let mut v = vec![manager.to_string(), "upgrade".to_string()];
            if yes {
                v.push("-y".to_string());
            }
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("dnf" | "yum", "list") => Ok(vec![
            manager.to_string(),
            "list".to_string(),
            "installed".to_string(),
        ]),
        ("dnf" | "yum", "search") => {
            let mut v = vec![manager.to_string(), "search".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }

        // ── pip ───────────────────────────────────────────────────
        ("pip", "install") => {
            let mut v = vec!["pip".to_string(), "install".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("pip", "remove") => {
            let mut v = vec!["pip".to_string(), "uninstall".to_string()];
            if yes {
                v.push("-y".to_string());
            }
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("pip", "upgrade") => {
            let mut v = vec![
                "pip".to_string(),
                "install".to_string(),
                "--upgrade".to_string(),
            ];
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("pip", "list") => Ok(vec!["pip".to_string(), "list".to_string()]),
        ("pip", "search") => {
            // pip search is deprecated; fall back to pip index versions
            let pkg = packages.first().map(String::as_str).unwrap_or("");
            Ok(vec![
                "pip".to_string(),
                "index".to_string(),
                "versions".to_string(),
                pkg.to_string(),
            ])
        }

        // ── uv ────────────────────────────────────────────────────
        ("uv", "install") => {
            let mut v = vec!["uv".to_string(), "pip".to_string(), "install".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("uv", "remove") => {
            let mut v = vec!["uv".to_string(), "pip".to_string(), "uninstall".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("uv", "upgrade") => {
            let mut v = vec![
                "uv".to_string(),
                "pip".to_string(),
                "install".to_string(),
                "--upgrade".to_string(),
            ];
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("uv", "list") => Ok(vec![
            "uv".to_string(),
            "pip".to_string(),
            "list".to_string(),
        ]),

        // ── cargo ─────────────────────────────────────────────────
        ("cargo", "install") => {
            let mut v = vec!["cargo".to_string(), "install".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("cargo", "remove") => {
            let mut v = vec!["cargo".to_string(), "uninstall".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("cargo", "upgrade") => {
            // cargo-upgrade is from cargo-edit crate
            let mut v = vec!["cargo".to_string(), "upgrade".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }
        ("cargo", "list") => Ok(vec![
            "cargo".to_string(),
            "install".to_string(),
            "--list".to_string(),
        ]),
        ("cargo", "search") => {
            let mut v = vec!["cargo".to_string(), "search".to_string()];
            v.extend_from_slice(packages);
            Ok(v)
        }

        _ => Ok(vec![]),
    }
}

// ── Execution ──────────────────────────────────────────────────────

async fn run_command(
    program: &str,
    argv: &[String],
    timeout_secs: u64,
    sandbox: &dyn Sandbox,
    workspace_dir: &std::path::Path,
) -> anyhow::Result<ToolResult> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.args(argv).env("DEBIAN_FRONTEND", "noninteractive");
    apply_workspace_venv_env(&mut cmd, workspace_dir);
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

    let output = match execution {
        CommandExecution::Completed(output) => output,
        CommandExecution::TimedOut => {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Timed out after {timeout_secs}s")),
            });
        }
    };

    let stdout = render_stream(output.stdout, "stdout");
    let stderr = render_stream(output.stderr, "stderr");
    let success = output.status.success();

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
    use crate::security::SecurityPolicy;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

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
            PackageManagerTool::new(permissive()).name(),
            "package_manager"
        );
    }

    #[test]
    fn timeout_defaults_to_unlimited() {
        let schema = PackageManagerTool::new(permissive()).parameters_schema();
        assert_eq!(schema["properties"]["timeout_secs"]["default"], 0);
        for manager in schema["properties"]["manager"]["enum"]
            .as_array()
            .unwrap()
        {
            assert!(which::which(manager.as_str().unwrap()).is_ok());
        }
    }

    #[test]
    fn apt_install_argv() {
        let argv = build_argv("apt", "install", &["curl".to_string()], true).unwrap();
        assert_eq!(argv, vec!["apt", "install", "-y", "curl"]);
    }

    #[test]
    fn apt_search_uses_apt_cache() {
        let argv = build_argv("apt", "search", &["nginx".to_string()], true).unwrap();
        assert_eq!(argv[0], "apt-cache");
    }

    #[test]
    fn dnf_remove_argv() {
        let argv = build_argv("dnf", "remove", &["nginx".to_string()], true).unwrap();
        assert_eq!(argv, vec!["dnf", "remove", "-y", "nginx"]);
    }

    #[test]
    fn pip_uninstall_passes_y() {
        let argv = build_argv("pip", "remove", &["requests".to_string()], true).unwrap();
        assert!(argv.contains(&"-y".to_string()));
    }

    #[test]
    fn cargo_install_argv() {
        let argv = build_argv("cargo", "install", &["ripgrep".to_string()], true).unwrap();
        assert_eq!(argv, vec!["cargo", "install", "ripgrep"]);
    }

    #[test]
    fn unknown_combo_returns_empty() {
        let argv = build_argv("apt", "hold_forever", &[], true).unwrap();
        assert!(argv.is_empty());
    }

    #[tokio::test]
    async fn mutating_op_blocked_by_default_policy() {
        let tool = PackageManagerTool::new(restrictive());
        let result = tool
            .execute(json!({"manager": "apt", "operation": "install", "packages": ["curl"]}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("blocked"));
    }

    #[tokio::test]
    async fn unknown_combo_in_execute() {
        let tool = PackageManagerTool::new(permissive());
        let result = tool
            .execute(json!({"manager": "apt", "operation": "hold_forever"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pip_uses_workspace_virtual_environment_when_present() {
        let temp = tempfile::tempdir().expect("temp directory should be created");
        let venv_bin = temp.path().join(".venv/bin");
        std::fs::create_dir_all(&venv_bin).expect("venv bin directory should be created");
        let fake_pip = venv_bin.join("pip");
        std::fs::write(
            &fake_pip,
            "#!/bin/sh\nprintf 'LLAMAFARM_VENV_PIP=%s\\n' \"$VIRTUAL_ENV\"\n",
        )
        .expect("fake pip should be written");
        let mut permissions = std::fs::metadata(&fake_pip)
            .expect("fake pip metadata should exist")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_pip, permissions).expect("fake pip should be executable");

        let security = Arc::new(SecurityPolicy {
            block_high_risk_commands: false,
            workspace_dir: temp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let tool = PackageManagerTool::new(security);
        let result = tool
            .execute(json!({"manager": "pip", "operation": "list"}))
            .await
            .expect("pip list should return a result");

        assert!(
            result.success,
            "fake workspace pip should run: {:?}",
            result.error
        );
        assert!(
            result.output.contains("LLAMAFARM_VENV_PIP="),
            "workspace pip marker missing: {}",
            result.output
        );
        assert!(
            result
                .output
                .contains(&temp.path().join(".venv").to_string_lossy().to_string()),
            "VIRTUAL_ENV should point at the workspace .venv: {}",
            result.output
        );
    }
}
