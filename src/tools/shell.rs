use super::traits::{Tool, ToolResult};
use crate::runtime::RuntimeAdapter;
use crate::security::SyscallAnomalyDetector;
use crate::security::{NoopSandbox, Sandbox, SecurityPolicy};
use async_trait::async_trait;
use serde_json::json;
use std::collections::HashSet;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;

use super::process_group::{self, ProcessGroupGuard};

/// Maximum shell command execution time before kill.
const SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;
/// Environment variables safe to pass to shell commands.
/// Only functional variables are included — never API keys or secrets.
const SAFE_ENV_VARS: &[&str] = &[
    "PATH", "HOME", "TERM", "LANG", "LC_ALL", "LC_CTYPE", "USER", "SHELL", "TMPDIR",
];

/// Shell command execution tool with sandboxing
pub struct ShellTool {
    security: Arc<SecurityPolicy>,
    runtime: Arc<dyn RuntimeAdapter>,
    syscall_detector: Option<Arc<SyscallAnomalyDetector>>,
    sandbox: Arc<dyn Sandbox>,
}

impl ShellTool {
    pub fn new(security: Arc<SecurityPolicy>, runtime: Arc<dyn RuntimeAdapter>) -> Self {
        Self::new_with_syscall_detector_and_sandbox(security, runtime, None, Arc::new(NoopSandbox))
    }

    pub fn new_with_sandbox(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self::new_with_syscall_detector_and_sandbox(security, runtime, None, sandbox)
    }

    pub fn new_with_syscall_detector(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        syscall_detector: Option<Arc<SyscallAnomalyDetector>>,
    ) -> Self {
        Self::new_with_syscall_detector_and_sandbox(
            security,
            runtime,
            syscall_detector,
            Arc::new(NoopSandbox),
        )
    }

    pub fn new_with_syscall_detector_and_sandbox(
        security: Arc<SecurityPolicy>,
        runtime: Arc<dyn RuntimeAdapter>,
        syscall_detector: Option<Arc<SyscallAnomalyDetector>>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            security,
            runtime,
            syscall_detector,
            sandbox,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ShellExecutionPlan {
    pub command: String,
    pub cwd: PathBuf,
}

fn is_valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Make the workspace `.venv` the default Python environment for every shell
/// command: prepend its `bin` dir to PATH and set VIRTUAL_ENV, so bare
/// `python`/`pip` invocations resolve to the venv without prompting the model.
pub(crate) fn apply_workspace_venv_env(cmd: &mut tokio::process::Command, workspace_dir: &Path) {
    let venv_dir = workspace_dir.join(".venv");
    let venv_bin = venv_dir.join("bin");
    if !venv_bin.is_dir() {
        return;
    }

    let base_path = std::env::var("PATH").unwrap_or_default();
    let new_path = if base_path.is_empty() {
        venv_bin.to_string_lossy().to_string()
    } else {
        format!("{}:{}", venv_bin.to_string_lossy(), base_path)
    };
    cmd.env("PATH", new_path);
    cmd.env("VIRTUAL_ENV", venv_dir.as_os_str());
}

pub(super) fn collect_allowed_shell_env_vars(security: &SecurityPolicy) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for key in SAFE_ENV_VARS
        .iter()
        .copied()
        .chain(security.shell_env_passthrough.iter().map(|s| s.as_str()))
    {
        let candidate = key.trim();
        if candidate.is_empty() || !is_valid_env_var_name(candidate) {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            out.push(candidate.to_string());
        }
    }
    out
}

fn extract_command_argument(args: &serde_json::Value) -> Option<String> {
    fn command_from_value(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::String(command) => {
                let trimmed = command.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_string())
            }
            serde_json::Value::Array(items) => command_from_argv_array(items),
            _ => None,
        }
    }

    if let Some(command) = args.get("command").and_then(command_from_value) {
        return Some(command);
    }

    for alias in [
        "hint",
        "cmd",
        "script",
        "shell_command",
        "command_line",
        "bash",
        "sh",
        "input",
    ] {
        if let Some(command) = args.get(alias).and_then(command_from_value) {
            return Some(command);
        }
    }

    args.as_str()
        .map(str::trim)
        .filter(|cmd| !cmd.is_empty())
        .map(ToString::to_string)
}

fn command_has_shell_operators(command: &str) -> bool {
    command.chars().any(|ch| {
        matches!(
            ch,
            ' ' | '\t' | '\n' | ';' | '|' | '&' | '<' | '>' | '(' | ')' | '$' | '`'
        )
    })
}

fn resolve_script_path(command: &str, workspace_dir: &Path) -> PathBuf {
    if command == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(command));
    }

    if let Some(stripped) = command.strip_prefix("~/") {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(stripped))
            .unwrap_or_else(|| PathBuf::from(command));
    }

    let path = PathBuf::from(command);
    if path.is_absolute() {
        path
    } else {
        workspace_dir.join(path)
    }
}

fn infer_script_interpreter(path: &Path) -> Option<&'static str> {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .as_deref()
    {
        Some("sh" | "bash" | "zsh") => return Some("bash"),
        Some("py") => return Some("python3"),
        Some("js" | "mjs" | "cjs") => return Some("node"),
        _ => {}
    }

    let mut file = std::fs::File::open(path).ok()?;
    let mut prefix = [0_u8; 256];
    let bytes_read = file.read(&mut prefix).ok()?;
    let shebang = String::from_utf8_lossy(&prefix[..bytes_read]).to_ascii_lowercase();
    if !shebang.starts_with("#!") {
        return None;
    }

    if shebang.contains("python") {
        Some("python3")
    } else if shebang.contains("node") {
        Some("node")
    } else if shebang.contains("bash") || shebang.contains("zsh") {
        Some("bash")
    } else if shebang.contains("sh") {
        Some("sh")
    } else {
        None
    }
}

fn shell_quote_single(token: &str) -> String {
    format!("'{}'", token.replace('\'', "'\"'\"'"))
}

fn render_shell_argv_token(token: &str) -> String {
    if token.chars().all(|ch| {
        ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-' | ':' | '=' | '+')
    }) {
        token.to_string()
    } else {
        shell_quote_single(token)
    }
}

fn command_from_argv_array(items: &[serde_json::Value]) -> Option<String> {
    let mut rendered = Vec::with_capacity(items.len());

    for item in items {
        let token = match item {
            serde_json::Value::String(value) => {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    return None;
                }
                trimmed.to_string()
            }
            serde_json::Value::Number(value) => value.to_string(),
            serde_json::Value::Bool(value) => value.to_string(),
            _ => return None,
        };
        rendered.push(render_shell_argv_token(&token));
    }

    (!rendered.is_empty()).then(|| rendered.join(" "))
}

fn resolve_working_dir(path: &str, base_dir: &Path) -> PathBuf {
    if path == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dir.join(path));
    }

    if let Some(stripped) = path.strip_prefix("~/") {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join(stripped))
            .unwrap_or_else(|| base_dir.join(path));
    }

    let candidate = PathBuf::from(path);
    if candidate.is_absolute() {
        candidate
    } else {
        base_dir.join(candidate)
    }
}

fn fallback_shell_env_value() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "cmd.exe"
    }

    #[cfg(not(target_os = "windows"))]
    {
        "/bin/sh"
    }
}

fn parse_quoted_token(input: &str) -> Option<(String, usize)> {
    let trimmed = input.trim_start();
    let leading_ws = input.len() - trimmed.len();
    let first = trimmed.chars().next()?;

    if matches!(first, '\'' | '"') {
        let mut token = String::new();
        let mut consumed = leading_ws + first.len_utf8();
        let mut closed = false;

        for ch in trimmed[first.len_utf8()..].chars() {
            consumed += ch.len_utf8();
            if ch == first {
                closed = true;
                break;
            }
            token.push(ch);
        }

        if !closed {
            return None;
        }

        return Some((token, consumed));
    }

    let end = trimmed
        .find(|ch: char| ch.is_whitespace() || ch == ';' || ch == '&')
        .unwrap_or(trimmed.len());
    if end == 0 {
        return None;
    }

    Some((trimmed[..end].to_string(), leading_ws + end))
}

fn extract_leading_cd_prefix(command: &str) -> Option<(String, String)> {
    let trimmed = command.trim();
    let rest = trimmed.strip_prefix("cd")?;
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }

    let (path, consumed) = parse_quoted_token(rest)?;
    let remainder = rest[consumed..].trim_start();
    let nested_command = if let Some(rest) = remainder.strip_prefix("&&") {
        rest
    } else if let Some(rest) = remainder.strip_prefix(';') {
        rest
    } else {
        return None;
    };

    let nested_command = nested_command.trim();
    if nested_command.is_empty() {
        return None;
    }

    Some((path, nested_command.to_string()))
}

pub(crate) fn normalize_shell_command_input(command: &str, workspace_dir: &Path) -> String {
    let trimmed = command.trim();
    if trimmed.is_empty() || command_has_shell_operators(trimmed) {
        return trimmed.to_string();
    }

    let resolved_path = resolve_script_path(trimmed, workspace_dir);
    if !resolved_path.is_file() {
        return trimmed.to_string();
    }

    let Some(interpreter) = infer_script_interpreter(&resolved_path) else {
        return trimmed.to_string();
    };

    let rendered_path = if trimmed == "~" || trimmed.starts_with("~/") {
        resolved_path.to_string_lossy().into_owned()
    } else {
        trimmed.to_string()
    };

    format!("{interpreter} {}", shell_quote_single(&rendered_path))
}

pub(crate) fn build_shell_execution_plan(
    command: &str,
    workspace_dir: &Path,
) -> ShellExecutionPlan {
    if let Some((raw_cwd, nested_command)) = extract_leading_cd_prefix(command) {
        let cwd = resolve_working_dir(raw_cwd.trim(), workspace_dir);
        return ShellExecutionPlan {
            command: normalize_shell_command_input(&nested_command, &cwd),
            cwd,
        };
    }

    ShellExecutionPlan {
        command: normalize_shell_command_input(command, workspace_dir),
        cwd: workspace_dir.to_path_buf(),
    }
}

struct ShellCommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

enum ShellCommandExecution {
    Completed(ShellCommandOutput),
    TimedOut { termination_detail: String },
}

/// Run a finite command while retaining ownership of its child so timeout and
/// cancellation paths can terminate the entire process group.
async fn run_command_with_timeout(
    mut cmd: tokio::process::Command,
    timeout: Duration,
) -> io::Result<ShellCommandExecution> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    process_group::configure(&mut cmd);

    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| io::Error::other("spawned command did not expose a process ID"))?;
    let mut process_group = ProcessGroupGuard::new(pid);
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let execution = async {
        let (status, stdout, stderr) = tokio::join!(
            child.wait(),
            read_capped_output(stdout, MAX_OUTPUT_BYTES),
            read_capped_output(stderr, MAX_OUTPUT_BYTES),
        );
        Ok::<ShellCommandOutput, io::Error>(ShellCommandOutput {
            status: status?,
            stdout: stdout?,
            stderr: stderr?,
        })
    };
    let mut execution = Box::pin(execution);

    match tokio::time::timeout(timeout, execution.as_mut()).await {
        Ok(result) => {
            drop(execution);
            match result {
                Ok(output) => {
                    process_group.disarm();
                    Ok(ShellCommandExecution::Completed(output))
                }
                Err(error) => {
                    let _ = process_group.terminate();
                    let _ = child.start_kill();
                    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
                    Err(error)
                }
            }
        }
        Err(_) => {
            drop(execution);
            let process_group_error = process_group.terminate().err();
            // This is redundant on Unix when group signaling succeeds, but is
            // the required fallback on platforms without process groups.
            let direct_child_error = child.start_kill().err();
            let wait_result =
                tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
            let direct_child_exited = matches!(&wait_result, Ok(Ok(_)));

            let termination_detail = if process_group::is_supported() {
                match process_group_error {
                    None => "SIGKILL was sent to its process group".to_string(),
                    Some(error) => {
                        let fallback = if direct_child_exited {
                            "the direct child exited"
                        } else if let Some(child_error) = direct_child_error {
                            return Ok(ShellCommandExecution::TimedOut {
                                termination_detail: format!(
                                    "process-group termination failed ({error}) and direct-child termination failed ({child_error})"
                                ),
                            });
                        } else {
                            "direct-child termination was attempted"
                        };
                        format!("process-group termination failed ({error}); {fallback}")
                    }
                }
            } else if direct_child_exited {
                "the direct child was killed (process groups are unavailable on this platform)"
                    .to_string()
            } else if let Some(error) = direct_child_error {
                format!("direct-child termination failed ({error})")
            } else {
                "direct-child termination was attempted but exit was not confirmed".to_string()
            };

            Ok(ShellCommandExecution::TimedOut { termination_detail })
        }
    }
}

/// Drain a child pipe fully so it cannot block, while retaining at most one
/// byte beyond the output limit so the caller can preserve truncation notices.
async fn read_capped_output<R>(handle: Option<R>, max_bytes: usize) -> io::Result<Vec<u8>>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let Some(mut reader) = handle else {
        return Ok(Vec::new());
    };
    let retained_limit = max_bytes.saturating_add(1);
    let mut output = Vec::with_capacity(retained_limit.min(8192));
    let mut chunk = [0_u8; 8192];

    loop {
        let count = reader.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        let remaining = retained_limit.saturating_sub(output.len());
        let retained = count.min(remaining);
        output.extend_from_slice(&chunk[..retained]);
    }

    Ok(output)
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a finite-duration shell command in the workspace directory (60-second limit). Bare local script paths are accepted and normalized to an explicit interpreter when possible. Leading forms like `cd /path && ./script.sh` are supported. Use the process tool with action='spawn' for web apps, development servers, daemons, and other long-running commands."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "A finite shell command to execute (60-second limit). Bare local script paths like ./test.sh or script.py are supported, as are leading forms like `cd /path && ./script.sh`. Use process.spawn for long-running services."
                },
                "approved": {
                    "type": "boolean",
                    "description": "Set true to explicitly approve medium/high-risk commands in supervised mode",
                    "default": false
                }
            },
            "required": ["command"]
        })
    }

    #[allow(clippy::incompatible_msrv)]
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = extract_command_argument(&args)
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;
        let ShellExecutionPlan {
            command: normalized_command,
            cwd: working_dir,
        } = build_shell_execution_plan(&command, &self.security.workspace_dir);
        let effective_command = self
            .security
            .apply_shell_redirect_policy(&normalized_command);
        let policy_command = match self
            .security
            .command_for_policy_validation(&effective_command)
        {
            Ok(command) => command,
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(reason),
                });
            }
        };
        let approved = args
            .get("approved")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if self.security.is_rate_limited() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: too many actions in the last hour".into()),
            });
        }

        match self
            .security
            .validate_command_execution(&normalized_command, approved)
        {
            Ok(_) => {}
            Err(reason) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(reason),
                });
            }
        }

        let working_dir_str = working_dir.to_string_lossy().to_string();
        let working_dir_allowed = if working_dir.is_absolute() {
            let resolved = working_dir
                .canonicalize()
                .unwrap_or_else(|_| working_dir.clone());
            self.security.is_resolved_path_allowed(&resolved)
        } else {
            self.security.is_path_allowed(&working_dir_str)
        };
        if !working_dir_allowed {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Working directory blocked by security policy: {}",
                    working_dir.display()
                )),
            });
        }

        if let Some(path) = self.security.forbidden_path_argument(&policy_command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Path blocked by security policy: {path}")),
            });
        }

        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Rate limit exceeded: action budget exhausted".into()),
            });
        }

        // Execute with timeout to prevent hanging commands.
        // Clear the environment to prevent leaking API keys and other secrets
        // (CWE-200), then re-add only safe, functional variables.
        let mut cmd = match self
            .runtime
            .build_shell_command(&effective_command, &working_dir)
        {
            Ok(cmd) => cmd,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to build runtime command: {e}")),
                });
            }
        };
        cmd.env_clear();

        for var in collect_allowed_shell_env_vars(&self.security) {
            if let Ok(val) = std::env::var(&var) {
                cmd.env(&var, val);
            }
        }
        if std::env::var("SHELL")
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
        {
            cmd.env("SHELL", fallback_shell_env_value());
        }
        apply_workspace_venv_env(&mut cmd, &self.security.workspace_dir);
        if let Err(e) = self.sandbox.wrap_command(cmd.as_std_mut()) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Failed to apply {} sandbox: {e}",
                    self.sandbox.name()
                )),
            });
        }

        let result = run_command_with_timeout(cmd, Duration::from_secs(SHELL_TIMEOUT_SECS)).await;

        match result {
            Ok(ShellCommandExecution::Completed(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Truncate output to prevent OOM
                if stdout.len() > MAX_OUTPUT_BYTES {
                    stdout.truncate(crate::util::floor_utf8_char_boundary(
                        &stdout,
                        MAX_OUTPUT_BYTES,
                    ));
                    stdout.push_str("\n... [output truncated at 1MB]");
                }
                if stderr.len() > MAX_OUTPUT_BYTES {
                    stderr.truncate(crate::util::floor_utf8_char_boundary(
                        &stderr,
                        MAX_OUTPUT_BYTES,
                    ));
                    stderr.push_str("\n... [stderr truncated at 1MB]");
                }

                if let Some(detector) = &self.syscall_detector {
                    let _ = detector.inspect_command_output(
                        &effective_command,
                        &stdout,
                        &stderr,
                        output.status.code(),
                    );
                }

                Ok(ToolResult {
                    success: output.status.success(),
                    output: stdout,
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute command: {e}")),
            }),
            Ok(ShellCommandExecution::TimedOut { termination_detail }) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Command timed out after {SHELL_TIMEOUT_SECS}s; {termination_detail}"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AuditConfig, SyscallAnomalyConfig};
    use crate::runtime::{NativeRuntime, RuntimeAdapter};
    use crate::security::{
        AutonomyLevel, SecurityPolicy, ShellRedirectPolicy, SyscallAnomalyDetector,
    };
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_redirect_policy(
        autonomy: AutonomyLevel,
        shell_redirect_policy: ShellRedirectPolicy,
    ) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            shell_redirect_policy,
            ..SecurityPolicy::default()
        })
    }

    fn test_runtime() -> Arc<dyn RuntimeAdapter> {
        Arc::new(NativeRuntime::new())
    }

    fn test_syscall_detector(tmp: &TempDir) -> Arc<SyscallAnomalyDetector> {
        let log_path = tmp.path().join("shell-syscall-anomalies.log");
        let cfg = SyscallAnomalyConfig {
            baseline_syscalls: vec!["read".into(), "write".into()],
            log_path: log_path.to_string_lossy().to_string(),
            alert_cooldown_secs: 1,
            max_alerts_per_minute: 50,
            ..SyscallAnomalyConfig::default()
        };
        let audit = AuditConfig {
            enabled: false,
            ..AuditConfig::default()
        };
        Arc::new(SyscallAnomalyDetector::new(cfg, tmp.path(), audit))
    }

    #[test]
    fn shell_tool_name() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn shell_tool_description() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(
            schema["required"]
                .as_array()
                .expect("schema required field should be an array")
                .contains(&json!("command"))
        );
        assert!(schema["properties"]["approved"].is_object());
    }

    #[test]
    fn extract_command_argument_supports_aliases() {
        assert_eq!(
            extract_command_argument(&json!({"hint": "echo from-hint"})).as_deref(),
            Some("echo from-hint")
        );
        assert_eq!(
            extract_command_argument(&json!({"cmd": "echo from-cmd"})).as_deref(),
            Some("echo from-cmd")
        );
        assert_eq!(
            extract_command_argument(&json!({"script": "echo from-script"})).as_deref(),
            Some("echo from-script")
        );
        assert_eq!(
            extract_command_argument(&json!("echo from-string")).as_deref(),
            Some("echo from-string")
        );
        assert_eq!(
            extract_command_argument(&json!({"cmd": ["bash", "-lc", "ls -R"]})).as_deref(),
            Some("bash -lc 'ls -R'")
        );
    }

    #[test]
    fn normalize_shell_command_input_rewrites_shell_script_path() {
        let tmp = TempDir::new().expect("temp dir");
        let script = tmp.path().join("test.sh");
        std::fs::write(&script, "#!/usr/bin/env bash\necho ok\n").expect("write script");

        let normalized = normalize_shell_command_input("./test.sh", tmp.path());
        assert_eq!(normalized, "bash './test.sh'");
    }

    #[test]
    fn normalize_shell_command_input_keeps_plain_commands() {
        let tmp = TempDir::new().expect("temp dir");
        assert_eq!(normalize_shell_command_input("ls", tmp.path()), "ls");
        assert_eq!(
            normalize_shell_command_input("echo hello", tmp.path()),
            "echo hello"
        );
    }

    #[test]
    fn build_shell_execution_plan_extracts_leading_cd_prefix() {
        let tmp = TempDir::new().expect("temp dir");
        let cwd = tmp.path().join("nested");
        std::fs::create_dir_all(&cwd).expect("create nested dir");
        std::fs::write(cwd.join("clean.sh"), "#!/usr/bin/env bash\necho clean\n")
            .expect("write script");

        let plan =
            build_shell_execution_plan(&format!("cd {} && ./clean.sh", cwd.display()), tmp.path());
        assert_eq!(plan.command, "bash './clean.sh'");
        assert_eq!(plan.cwd, cwd);
    }

    #[test]
    fn build_shell_execution_plan_keeps_default_workspace_without_cd_prefix() {
        let tmp = TempDir::new().expect("temp dir");
        let plan = build_shell_execution_plan("python3 bench.py", tmp.path());
        assert_eq!(plan.command, "python3 bench.py");
        assert_eq!(plan.cwd, tmp.path());
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .expect("echo command execution should succeed");
        assert!(result.success);
        assert!(result.output.trim().contains("hello"));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn shell_executes_command_from_cmd_alias() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"cmd": "echo alias"}))
            .await
            .expect("cmd alias execution should succeed");
        assert!(result.success);
        assert!(result.output.trim().contains("alias"));
    }

    #[tokio::test]
    async fn shell_blocks_disallowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "rm -rf /"}))
            .await
            .expect("disallowed command execution should return a result");
        assert!(!result.success);
        let error = result.error.as_deref().unwrap_or("");
        assert!(error.contains("not allowed") || error.contains("high-risk"));
    }

    #[tokio::test]
    async fn shell_blocks_readonly() {
        let tool = ShellTool::new(test_security(AutonomyLevel::ReadOnly), test_runtime());
        let result = tool
            .execute(json!({"command": "ls"}))
            .await
            .expect("readonly command execution should return a result");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_ref()
                .expect("error field should be present for blocked command")
                .contains("not allowed")
        );
    }

    #[tokio::test]
    async fn shell_missing_command_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[tokio::test]
    async fn shell_wrong_type_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool.execute(json!({"command": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_captures_exit_code() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "ls /nonexistent_dir_xyz"}))
            .await
            .expect("command with nonexistent path should return a result");
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_blocks_absolute_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat /etc/passwd"}))
            .await
            .expect("absolute path argument should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_option_assignment_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "grep --file=/etc/passwd root ./src"}))
            .await
            .expect("option-assigned forbidden path should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_short_option_attached_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "grep -f/etc/passwd root ./src"}))
            .await
            .expect("short option attached forbidden path should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_tilde_user_path_argument() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat ~root/.ssh/id_rsa"}))
            .await
            .expect("tilde-user path should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Path blocked")
        );
    }

    #[tokio::test]
    async fn shell_blocks_input_redirection_path_bypass() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised), test_runtime());
        let result = tool
            .execute(json!({"command": "cat </etc/passwd"}))
            .await
            .expect("input redirection bypass should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not allowed")
        );
    }

    #[tokio::test]
    async fn shell_strip_policy_allows_common_stderr_redirects() {
        let tool = ShellTool::new(
            test_security_with_redirect_policy(
                AutonomyLevel::Supervised,
                ShellRedirectPolicy::Strip,
            ),
            test_runtime(),
        );

        let merged = tool
            .execute(json!({"command": "echo redirect-ok 2>&1"}))
            .await
            .expect("2>&1 should be normalized under strip policy");
        assert!(merged.success);
        assert!(merged.output.contains("redirect-ok"));

        let devnull = tool
            .execute(json!({"command": "ls definitely_missing_shell_redirect 2>/dev/null"}))
            .await
            .expect("2>/dev/null should be normalized under strip policy");
        assert!(!devnull.success);
        assert!(
            devnull
                .error
                .as_deref()
                .unwrap_or("")
                .contains("definitely_missing_shell_redirect")
        );
    }

    #[tokio::test]
    async fn shell_strip_policy_still_blocks_unsupported_redirects() {
        let tool = ShellTool::new(
            test_security_with_redirect_policy(
                AutonomyLevel::Supervised,
                ShellRedirectPolicy::Strip,
            ),
            test_runtime(),
        );
        let result = tool
            .execute(json!({"command": "echo blocked > out.txt"}))
            .await
            .expect("unsupported redirect should still be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not allowed")
        );
    }

    #[tokio::test]
    async fn shell_allow_policy_supports_quoted_heredoc_file_creation() {
        let tmp = TempDir::new().expect("temp dir");
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: tmp.path().to_path_buf(),
            shell_redirect_policy: ShellRedirectPolicy::Allow,
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());

        let result = tool
            .execute(json!({"command": "cat > hello.txt << 'EOF'\nhello\nEOF"}))
            .await
            .expect("quoted heredoc should execute");
        assert!(result.success, "{result:?}");
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("hello.txt")).expect("read output file"),
            "hello\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_allows_compound_direct_workspace_script_execution() {
        let tmp = TempDir::new().expect("temp dir");
        let script = tmp.path().join("check.sh");
        std::fs::write(&script, "#!/usr/bin/env bash\necho script-ok\n").expect("write script");
        let mut perms = std::fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).expect("set executable bit");

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: tmp.path().to_path_buf(),
            allowed_commands: vec!["echo".into(), "bash".into(), "sh".into()],
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());

        let result = tool
            .execute(json!({"command": "echo preparing && ./check.sh"}))
            .await
            .expect("compound script command should execute");
        assert!(result.success);
        assert!(result.output.contains("preparing"));
        assert!(result.output.contains("script-ok"));
    }

    fn test_security_with_env_cmd() -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["env".into(), "echo".into()],
            ..SecurityPolicy::default()
        })
    }

    fn test_security_with_env_passthrough(vars: &[&str]) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            workspace_dir: std::env::temp_dir(),
            allowed_commands: vec!["env".into()],
            shell_env_passthrough: vars.iter().map(|v| (*v).to_string()).collect(),
            ..SecurityPolicy::default()
        })
    }

    /// RAII guard that restores an environment variable to its original state on drop,
    /// ensuring cleanup even if the test panics.
    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.original {
                Some(val) => std::env::set_var(self.key, val),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_does_not_leak_api_key() {
        let _g1 = EnvGuard::set("API_KEY", "sk-test-secret-12345");
        let _g2 = EnvGuard::set("LLAMAFARM_API_KEY", "sk-test-secret-67890");

        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command execution should succeed");
        assert!(result.success);
        assert!(
            !result.output.contains("sk-test-secret-12345"),
            "API_KEY leaked to shell command output"
        );
        assert!(
            !result.output.contains("sk-test-secret-67890"),
            "LLAMAFARM_API_KEY leaked to shell command output"
        );
    }

    #[tokio::test]
    async fn shell_preserves_path_and_home_for_env_command() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());

        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command should succeed");
        assert!(result.success);
        assert!(
            result.output.contains("HOME="),
            "HOME should be available in shell environment"
        );
        assert!(
            result.output.contains("PATH="),
            "PATH should be available in shell environment"
        );
    }

    #[tokio::test]
    async fn shell_blocks_plain_variable_expansion() {
        let tool = ShellTool::new(test_security_with_env_cmd(), test_runtime());
        let result = tool
            .execute(json!({"command": "echo $HOME"}))
            .await
            .expect("plain variable expansion should be blocked");
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("not allowed")
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shell_allows_configured_env_passthrough() {
        let _guard = EnvGuard::set("LLAMAFARM_TEST_PASSTHROUGH", "db://unit-test");
        let tool = ShellTool::new(
            test_security_with_env_passthrough(&["LLAMAFARM_TEST_PASSTHROUGH"]),
            test_runtime(),
        );

        let result = tool
            .execute(json!({"command": "env"}))
            .await
            .expect("env command execution should succeed");
        assert!(result.success);
        assert!(
            result
                .output
                .contains("LLAMAFARM_TEST_PASSTHROUGH=db://unit-test")
        );
    }

    #[test]
    fn invalid_shell_env_passthrough_names_are_filtered() {
        let security = SecurityPolicy {
            shell_env_passthrough: vec![
                "VALID_NAME".into(),
                "BAD-NAME".into(),
                "1NOPE".into(),
                "ALSO_VALID".into(),
            ],
            ..SecurityPolicy::default()
        };
        let vars = collect_allowed_shell_env_vars(&security);
        assert!(vars.contains(&"VALID_NAME".to_string()));
        assert!(vars.contains(&"ALSO_VALID".to_string()));
        assert!(!vars.contains(&"BAD-NAME".to_string()));
        assert!(!vars.contains(&"1NOPE".to_string()));
    }

    #[tokio::test]
    async fn shell_requires_approval_for_medium_risk_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            allowed_commands: vec!["touch".into()],
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });

        let tool = ShellTool::new(security.clone(), test_runtime());
        let denied = tool
            .execute(json!({"command": "touch llamafarm_shell_approval_test"}))
            .await
            .expect("unapproved command should return a result");
        assert!(!denied.success);
        assert!(
            denied
                .error
                .as_deref()
                .unwrap_or("")
                .contains("explicit approval")
        );

        let allowed = tool
            .execute(json!({
                "command": "touch llamafarm_shell_approval_test",
                "approved": true
            }))
            .await
            .expect("approved command execution should succeed");
        assert!(allowed.success);

        let _ = tokio::fs::remove_file(std::env::temp_dir().join("llamafarm_shell_approval_test"))
            .await;
    }

    // ── §5.2 Shell timeout enforcement tests ─────────────────

    #[test]
    fn shell_timeout_constant_is_reasonable() {
        assert_eq!(SHELL_TIMEOUT_SECS, 60, "shell timeout must be 60 seconds");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shell_timeout_kills_descendant_process_group() {
        let temp = tempfile::tempdir().expect("temp directory should be created");
        let started = temp.path().join("started");
        let leaked = temp.path().join("descendant-survived");
        let command = format!(
            "touch {}; (sleep 1; touch {}) & wait",
            shell_quote_single(&started.to_string_lossy()),
            shell_quote_single(&leaked.to_string_lossy())
        );
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(command).current_dir(temp.path());

        let result = run_command_with_timeout(cmd, Duration::from_millis(500))
            .await
            .expect("timed command should return an execution outcome");

        assert!(
            matches!(result, ShellCommandExecution::TimedOut { .. }),
            "command should time out"
        );
        assert!(started.exists(), "fixture command should have started");
        tokio::time::sleep(Duration::from_millis(1_100)).await;
        assert!(
            !leaked.exists(),
            "a descendant must not survive the shell timeout"
        );
    }

    #[test]
    fn shell_output_limit_is_1mb() {
        assert_eq!(
            MAX_OUTPUT_BYTES, 1_048_576,
            "max output must be 1 MB to prevent OOM"
        );
    }

    // ── §5.3 Non-UTF8 binary output tests ────────────────────

    #[test]
    fn shell_safe_env_vars_excludes_secrets() {
        for var in SAFE_ENV_VARS {
            let lower = var.to_lowercase();
            assert!(
                !lower.contains("key") && !lower.contains("secret") && !lower.contains("token"),
                "SAFE_ENV_VARS must not include sensitive variable: {var}"
            );
        }
    }

    #[test]
    fn shell_safe_env_vars_includes_essentials() {
        assert!(
            SAFE_ENV_VARS.contains(&"PATH"),
            "PATH must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"HOME"),
            "HOME must be in safe env vars"
        );
        assert!(
            SAFE_ENV_VARS.contains(&"TERM"),
            "TERM must be in safe env vars"
        );
    }

    #[tokio::test]
    async fn shell_blocks_rate_limited() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            max_actions_per_hour: 0,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let result = tool
            .execute(json!({"command": "echo test"}))
            .await
            .expect("rate-limited command should return a result");
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("Rate limit"));
    }

    #[tokio::test]
    async fn shell_handles_nonexistent_command() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());
        let result = tool
            .execute(json!({"command": "nonexistent_binary_xyz_12345"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn shell_captures_stderr_output() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Full), test_runtime());
        let result = tool
            .execute(json!({"command": "echo error_msg >&2"}))
            .await
            .unwrap();
        assert!(result.error.as_deref().unwrap_or("").contains("error_msg"));
    }

    #[tokio::test]
    async fn shell_record_action_budget_exhaustion() {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Full,
            max_actions_per_hour: 1,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        });
        let tool = ShellTool::new(security, test_runtime());

        let r1 = tool
            .execute(json!({"command": "echo first"}))
            .await
            .unwrap();
        assert!(r1.success);

        let r2 = tool
            .execute(json!({"command": "echo second"}))
            .await
            .unwrap();
        assert!(!r2.success);
        assert!(
            r2.error.as_deref().unwrap_or("").contains("Rate limit")
                || r2.error.as_deref().unwrap_or("").contains("budget")
        );
    }

    #[tokio::test]
    async fn shell_syscall_detector_writes_anomaly_log() {
        let tmp = tempfile::tempdir().expect("temp dir should be created");
        let log_path = tmp.path().join("shell-syscall-anomalies.log");
        let detector = test_syscall_detector(&tmp);
        let tool = ShellTool::new_with_syscall_detector(
            test_security(AutonomyLevel::Full),
            test_runtime(),
            Some(detector),
        );

        let result = tool
            .execute(json!({"command": "echo seccomp denied syscall=openat"}))
            .await
            .expect("command execution should return result");
        assert!(result.success);
        assert!(result.output.contains("openat"));

        let log = tokio::fs::read_to_string(&log_path)
            .await
            .expect("syscall anomaly log should be written");
        assert!(log.contains("\"kind\":\"unknown_syscall\""));
        assert!(log.contains("\"syscall\":\"openat\""));
    }
}
