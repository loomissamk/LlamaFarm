//! Opt-in host-side command runner.
//!
//! The Docker bundle intentionally keeps normal tools inside the container.
//! Operators who need host lifecycle control can install this user service and
//! expose its owner-only Unix socket through the existing host-home bind mount.
//! Requests are line-delimited JSON and every accepted operation is written to
//! an append-only JSONL audit log without recording command text or output.

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;
use uuid::Uuid;

#[cfg(unix)]
use std::os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt};
#[cfg(unix)]
use std::process::Stdio;
#[cfg(unix)]
use std::sync::Arc;
#[cfg(unix)]
use tokio::fs::{self, OpenOptions};
#[cfg(unix)]
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
#[cfg(unix)]
use tokio::net::{UnixListener, UnixStream};
#[cfg(unix)]
use tokio::process::Command;
#[cfg(unix)]
use tokio::sync::Mutex;
#[cfg(unix)]
use tokio::time::timeout;

pub const HOST_RUNNER_PROTOCOL_VERSION: u8 = 1;
pub const DEFAULT_EXEC_TIMEOUT_SECS: u64 = 60;
pub const DEFAULT_MAX_EXEC_TIMEOUT_SECS: u64 = 300;
const MAX_REQUEST_BYTES: usize = 64 * 1024;
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_COMMAND_BYTES: usize = 32 * 1024;
const MAX_CAPTURE_BYTES: usize = 256 * 1024;
const MAX_JOB_TAIL_BYTES: usize = 64 * 1024;

/// One request sent over the host-runner Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostRunnerRequest {
    pub protocol_version: u8,
    pub request_id: String,
    #[serde(flatten)]
    pub operation: HostRunnerOperation,
}

impl HostRunnerRequest {
    pub fn new(operation: HostRunnerOperation) -> Self {
        Self {
            protocol_version: HOST_RUNNER_PROTOCOL_VERSION,
            request_id: Uuid::new_v4().to_string(),
            operation,
        }
    }
}

/// Operations supported by the host runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum HostRunnerOperation {
    /// Check whether the user service is reachable.
    Health,
    /// Run a command and wait for its bounded output.
    Exec {
        command: String,
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        timeout_secs: Option<u64>,
    },
    /// Start a durable background command and return a job id.
    Spawn {
        command: String,
        #[serde(default)]
        cwd: Option<PathBuf>,
    },
    /// Read the persisted state and output tail for a background job.
    Status { job_id: String },
    /// Start the configured repository's health-gated Docker bundle redeploy.
    Redeploy,
}

impl HostRunnerOperation {
    fn audit_name(&self) -> &'static str {
        match self {
            Self::Health => "health",
            Self::Exec { .. } => "exec",
            Self::Spawn { .. } => "spawn",
            Self::Status { .. } => "status",
            Self::Redeploy => "redeploy",
        }
    }

    fn command_digest(&self) -> Option<String> {
        match self {
            Self::Exec { command, .. } | Self::Spawn { command, .. } => {
                Some(sha256_hex(command.as_bytes()))
            }
            Self::Redeploy => Some(sha256_hex(
                b"bash ./scripts/docker/up-bundle.sh up -d --build",
            )),
            Self::Health | Self::Status { .. } => None,
        }
    }
}

/// One response returned by the host runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostRunnerResponse {
    pub protocol_version: u8,
    pub request_id: String,
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<HostRunnerResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl HostRunnerResponse {
    fn success(request_id: impl Into<String>, result: HostRunnerResult) -> Self {
        Self {
            protocol_version: HOST_RUNNER_PROTOCOL_VERSION,
            request_id: request_id.into(),
            success: true,
            result: Some(result),
            error: None,
        }
    }

    fn failure(request_id: impl Into<String>, error: impl Into<String>) -> Self {
        Self {
            protocol_version: HOST_RUNNER_PROTOCOL_VERSION,
            request_id: request_id.into(),
            success: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

/// Typed operation result.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostRunnerResult {
    Health {
        service: String,
        pid: u32,
        arbitrary_exec_enabled: bool,
        redeploy_enabled: bool,
    },
    Exec {
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
        timed_out: bool,
    },
    Job {
        job: HostJobStatus,
    },
}

/// Durable background-job state returned to clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HostJobStatus {
    pub job_id: String,
    pub job_type: String,
    pub state: HostJobState,
    pub created_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    pub stdout_tail: String,
    pub stderr_tail: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HostJobState {
    Running,
    Succeeded,
    Failed,
    Lost,
}

/// Configuration for the host-side server process.
#[derive(Debug, Clone)]
pub struct HostRunnerServerConfig {
    pub home_dir: PathBuf,
    pub socket_path: PathBuf,
    pub state_dir: PathBuf,
    pub repo_dir: Option<PathBuf>,
    pub allow_exec: bool,
    pub max_exec_timeout_secs: u64,
}

impl HostRunnerServerConfig {
    /// Resolve service paths from explicit environment variables, then user-home
    /// defaults. No config file is loaded so the runner can recover a broken
    /// container deployment.
    pub fn from_env(allow_exec: bool) -> Result<Self> {
        let home = host_home_dir()?;
        let socket_path = nonempty_env_path("LLAMAFARM_HOST_RUNNER_SOCKET")
            .unwrap_or_else(|| home.join(".llamafarm/run/host-runner.sock"));
        let state_dir = nonempty_env_path("LLAMAFARM_HOST_RUNNER_STATE_DIR")
            .unwrap_or_else(|| home.join(".local/state/llamafarm/host-runner"));
        let repo_dir = nonempty_env_path("LLAMAFARM_HOST_RUNNER_REPO");
        let max_exec_timeout_secs = std::env::var("LLAMAFARM_HOST_RUNNER_MAX_TIMEOUT_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(DEFAULT_MAX_EXEC_TIMEOUT_SECS);

        if max_exec_timeout_secs == 0 {
            bail!("LLAMAFARM_HOST_RUNNER_MAX_TIMEOUT_SECS must be greater than zero");
        }

        Ok(Self {
            home_dir: home,
            socket_path,
            state_dir,
            repo_dir,
            allow_exec,
            max_exec_timeout_secs,
        })
    }
}

fn nonempty_env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn host_home_dir() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .context("HOME is required to resolve host-runner paths")
}

fn sha256_hex(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

fn validate_request(request: &HostRunnerRequest, max_timeout_secs: u64) -> Result<()> {
    if request.protocol_version != HOST_RUNNER_PROTOCOL_VERSION {
        bail!(
            "unsupported protocol version {}; expected {}",
            request.protocol_version,
            HOST_RUNNER_PROTOCOL_VERSION
        );
    }
    validate_identifier("request_id", &request.request_id)?;

    match &request.operation {
        HostRunnerOperation::Exec {
            command,
            cwd,
            timeout_secs,
        } => {
            validate_command(command)?;
            validate_cwd(cwd.as_deref())?;
            if let Some(seconds) = timeout_secs {
                if *seconds == 0 || *seconds > max_timeout_secs {
                    bail!("timeout_secs must be between 1 and {max_timeout_secs}");
                }
            }
        }
        HostRunnerOperation::Spawn { command, cwd } => {
            validate_command(command)?;
            validate_cwd(cwd.as_deref())?;
        }
        HostRunnerOperation::Status { job_id } => validate_identifier("job_id", job_id)?,
        HostRunnerOperation::Health | HostRunnerOperation::Redeploy => {}
    }

    Ok(())
}

fn validate_identifier(name: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        bail!("{name} must contain only ASCII letters, digits, '-' or '_'");
    }
    Ok(())
}

fn validate_command(command: &str) -> Result<()> {
    if command.trim().is_empty() {
        bail!("command cannot be empty");
    }
    if command.len() > MAX_COMMAND_BYTES {
        bail!("command exceeds {MAX_COMMAND_BYTES} bytes");
    }
    if command.contains('\0') {
        bail!("command cannot contain a NUL byte");
    }
    Ok(())
}

fn validate_cwd(cwd: Option<&Path>) -> Result<()> {
    let Some(cwd) = cwd else {
        return Ok(());
    };
    if !cwd.is_absolute() {
        bail!("cwd must be an absolute host path");
    }
    let metadata =
        std::fs::metadata(cwd).with_context(|| format!("cwd does not exist: {}", cwd.display()))?;
    if !metadata.is_dir() {
        bail!("cwd is not a directory: {}", cwd.display());
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct JobMetadata {
    job_id: String,
    job_type: String,
    command_sha256: String,
    cwd: PathBuf,
    created_at: String,
    pid: Option<u32>,
}

#[derive(Debug, Serialize)]
struct AuditEvent<'a> {
    timestamp: String,
    request_id: &'a str,
    operation: &'a str,
    outcome: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    job_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command_sha256: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<&'a str>,
}

#[cfg(unix)]
#[derive(Clone)]
struct AuditWriter {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

#[cfg(unix)]
impl AuditWriter {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }

    async fn append(&self, event: &AuditEvent<'_>) -> Result<()> {
        let _guard = self.lock.lock().await;
        let mut line = serde_json::to_vec(event).context("serialize host-runner audit event")?;
        line.push(b'\n');

        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(&self.path)
            .await
            .with_context(|| format!("open audit log {}", self.path.display()))?;
        file.write_all(&line)
            .await
            .context("append host-runner audit event")?;
        file.flush()
            .await
            .context("flush host-runner audit event")?;
        fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))
            .await
            .context("set host-runner audit permissions")?;
        Ok(())
    }
}

#[cfg(unix)]
#[derive(Clone)]
struct ServerContext {
    config: Arc<HostRunnerServerConfig>,
    audit: AuditWriter,
}

/// Run the owner-only Unix-socket service until the process is stopped.
#[cfg(unix)]
pub async fn serve(config: HostRunnerServerConfig) -> Result<()> {
    validate_server_paths(&config)?;
    prepare_private_dir(
        config
            .socket_path
            .parent()
            .context("host-runner socket path needs a parent directory")?,
    )
    .await?;
    prepare_private_dir(&config.state_dir).await?;
    prepare_private_dir(&config.state_dir.join("jobs")).await?;
    prepare_socket_path(&config.socket_path).await?;

    let listener = UnixListener::bind(&config.socket_path)
        .with_context(|| format!("bind host-runner socket {}", config.socket_path.display()))?;
    fs::set_permissions(&config.socket_path, std::fs::Permissions::from_mode(0o600))
        .await
        .context("set host-runner socket permissions")?;

    let context = ServerContext {
        audit: AuditWriter::new(config.state_dir.join("audit.jsonl")),
        config: Arc::new(config),
    };

    tracing::info!(
        socket = %context.config.socket_path.display(),
        allow_exec = context.config.allow_exec,
        redeploy_enabled = context.config.repo_dir.is_some(),
        "host runner listening"
    );

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("accept host-runner client")?;
        let request_context = context.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, request_context).await {
                tracing::warn!(%error, "host-runner request failed");
            }
        });
    }
}

#[cfg(unix)]
fn validate_server_paths(config: &HostRunnerServerConfig) -> Result<()> {
    let home = &config.home_dir;
    if !home.is_absolute() {
        bail!("host-runner HOME path must be absolute");
    }
    for (name, path) in [
        ("socket", config.socket_path.as_path()),
        ("state", config.state_dir.as_path()),
    ] {
        if !path.is_absolute() {
            bail!("host-runner {name} path must be absolute");
        }
        if path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            bail!("host-runner {name} path cannot contain '..'");
        }
        if !path.starts_with(home) {
            bail!(
                "host-runner {name} path must stay under HOME ({}): {}",
                home.display(),
                path.display()
            );
        }
    }
    if config.state_dir.as_path() == home.as_path() {
        bail!("host-runner state path must be a directory below HOME");
    }
    Ok(())
}

#[cfg(not(unix))]
pub async fn serve(_config: HostRunnerServerConfig) -> Result<()> {
    bail!("the host runner requires Unix-domain socket support")
}

#[cfg(unix)]
async fn prepare_private_dir(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path).await {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                bail!(
                    "private host-runner path must be a non-symlink directory: {}",
                    path.display()
                );
            }
            if metadata.permissions().mode() & 0o077 != 0 {
                bail!(
                    "private host-runner directory must be mode 0700: {}",
                    path.display()
                );
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir_all(path)
                .await
                .with_context(|| format!("create private directory {}", path.display()))?;
            fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .await
                .with_context(|| format!("set private directory permissions {}", path.display()))
        }
        Err(error) => {
            Err(error).with_context(|| format!("inspect private directory {}", path.display()))
        }
    }
}

#[cfg(unix)]
async fn prepare_socket_path(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error)
                .with_context(|| format!("inspect host-runner socket {}", path.display()));
        }
    };

    if !metadata.file_type().is_socket() {
        bail!("refusing to replace non-socket path at {}", path.display());
    }
    if UnixStream::connect(path).await.is_ok() {
        bail!("a host runner is already listening at {}", path.display());
    }

    fs::remove_file(path)
        .await
        .with_context(|| format!("remove stale host-runner socket {}", path.display()))
}

#[cfg(unix)]
async fn handle_connection(stream: UnixStream, context: ServerContext) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half.take((MAX_REQUEST_BYTES + 1) as u64));
    let mut wire = Vec::new();
    let read_result = timeout(Duration::from_secs(5), reader.read_until(b'\n', &mut wire))
        .await
        .context("timed out reading host-runner request")??;

    if read_result == 0 {
        bail!("empty host-runner request");
    }
    if wire.len() > MAX_REQUEST_BYTES {
        bail!("host-runner request exceeds {MAX_REQUEST_BYTES} bytes");
    }

    let parsed = serde_json::from_slice::<HostRunnerRequest>(&wire);
    let response = match parsed {
        Ok(request) => dispatch_request(request, &context).await,
        Err(error) => {
            HostRunnerResponse::failure("invalid", format!("invalid request JSON: {error}"))
        }
    };
    let mut encoded = serde_json::to_vec(&response).context("serialize host-runner response")?;
    encoded.push(b'\n');
    write_half
        .write_all(&encoded)
        .await
        .context("write host-runner response")?;
    write_half
        .shutdown()
        .await
        .context("close host-runner response")?;
    Ok(())
}

#[cfg(unix)]
async fn dispatch_request(
    request: HostRunnerRequest,
    context: &ServerContext,
) -> HostRunnerResponse {
    let request_id = request.request_id.clone();
    if let Err(error) = validate_request(&request, context.config.max_exec_timeout_secs) {
        let detail = error.to_string();
        let command_sha256 = request.operation.command_digest();
        let _ = context
            .audit
            .append(&AuditEvent {
                timestamp: Utc::now().to_rfc3339(),
                request_id: &request_id,
                operation: request.operation.audit_name(),
                outcome: "rejected",
                job_id: None,
                command_sha256: command_sha256.as_deref(),
                detail: Some(&detail),
            })
            .await;
        return HostRunnerResponse::failure(request_id, detail);
    }

    let command_sha256 = request.operation.command_digest();
    let operation_name = request.operation.audit_name();
    if let Err(error) = context
        .audit
        .append(&AuditEvent {
            timestamp: Utc::now().to_rfc3339(),
            request_id: &request_id,
            operation: operation_name,
            outcome: "accepted",
            job_id: match &request.operation {
                HostRunnerOperation::Status { job_id } => Some(job_id),
                _ => None,
            },
            command_sha256: command_sha256.as_deref(),
            detail: None,
        })
        .await
    {
        return HostRunnerResponse::failure(
            request_id,
            format!("audit write failed; operation was not executed: {error}"),
        );
    }

    let result = execute_operation(request.operation, context).await;

    match result {
        Ok(result) => {
            let job_id = match &result {
                HostRunnerResult::Job { job } => Some(job.job_id.as_str()),
                HostRunnerResult::Health { .. } | HostRunnerResult::Exec { .. } => None,
            };
            let audit_detail = match &result {
                HostRunnerResult::Exec {
                    exit_code,
                    timed_out,
                    ..
                } => Some(format!(
                    "exit_code={} timed_out={timed_out}",
                    exit_code.map_or_else(|| "signal".to_string(), |code| code.to_string())
                )),
                HostRunnerResult::Job { job } => {
                    Some(format!("state={:?}", job.state).to_ascii_lowercase())
                }
                HostRunnerResult::Health { .. } => None,
            };
            let _ = context
                .audit
                .append(&AuditEvent {
                    timestamp: Utc::now().to_rfc3339(),
                    request_id: &request_id,
                    operation: operation_name,
                    outcome: "completed",
                    job_id,
                    command_sha256: command_sha256.as_deref(),
                    detail: audit_detail.as_deref(),
                })
                .await;
            HostRunnerResponse::success(request_id, result)
        }
        Err(error) => {
            let detail = error.to_string();
            let _ = context
                .audit
                .append(&AuditEvent {
                    timestamp: Utc::now().to_rfc3339(),
                    request_id: &request_id,
                    operation: operation_name,
                    outcome: "failed",
                    job_id: None,
                    command_sha256: command_sha256.as_deref(),
                    detail: Some(&detail),
                })
                .await;
            HostRunnerResponse::failure(request_id, detail)
        }
    }
}

#[cfg(unix)]
async fn execute_operation(
    operation: HostRunnerOperation,
    context: &ServerContext,
) -> Result<HostRunnerResult> {
    match operation {
        HostRunnerOperation::Health => Ok(HostRunnerResult::Health {
            service: "llamafarm-host-runner".to_string(),
            pid: std::process::id(),
            arbitrary_exec_enabled: context.config.allow_exec,
            redeploy_enabled: context.config.repo_dir.is_some(),
        }),
        HostRunnerOperation::Exec {
            command,
            cwd,
            timeout_secs,
        } => {
            if !context.config.allow_exec {
                Err(anyhow::anyhow!(
                    "arbitrary host execution is disabled; restart the runner with --allow-exec"
                ))
            } else {
                let cwd = resolve_cwd(cwd, context.config.repo_dir.as_deref())?;
                run_exec(
                    &command,
                    &cwd,
                    timeout_secs.unwrap_or(DEFAULT_EXEC_TIMEOUT_SECS),
                )
                .await
            }
        }
        HostRunnerOperation::Spawn { command, cwd } => {
            if !context.config.allow_exec {
                Err(anyhow::anyhow!(
                    "arbitrary host execution is disabled; restart the runner with --allow-exec"
                ))
            } else {
                let cwd = resolve_cwd(cwd, context.config.repo_dir.as_deref())?;
                spawn_job(context, "command", &command, &cwd).await
            }
        }
        HostRunnerOperation::Status { job_id } => {
            read_job_status(&context.config.state_dir, &job_id)
                .await
                .map(|job| HostRunnerResult::Job { job })
        }
        HostRunnerOperation::Redeploy => {
            let repo_dir = context
                .config
                .repo_dir
                .as_deref()
                .context("redeploy is disabled because no repository is configured")?
                .canonicalize()
                .context("resolve configured redeploy repository")?;
            let deploy_script = repo_dir.join("scripts/docker/up-bundle.sh");
            let metadata = std::fs::symlink_metadata(&deploy_script)
                .with_context(|| format!("missing redeploy script {}", deploy_script.display()))?;
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!(
                    "redeploy script must be a regular non-symlink file: {}",
                    deploy_script.display()
                );
            }
            spawn_job(
                context,
                "redeploy",
                "bash ./scripts/docker/up-bundle.sh up -d --build",
                &repo_dir,
            )
            .await
        }
    }
}

#[cfg(unix)]
fn resolve_cwd(requested: Option<PathBuf>, repo_dir: Option<&Path>) -> Result<PathBuf> {
    let cwd = match requested {
        Some(path) => path,
        None => repo_dir.map(Path::to_path_buf).unwrap_or(host_home_dir()?),
    };
    validate_cwd(Some(&cwd))?;
    cwd.canonicalize()
        .with_context(|| format!("resolve cwd {}", cwd.display()))
}

#[cfg(unix)]
async fn run_exec(command: &str, cwd: &Path, timeout_secs: u64) -> Result<HostRunnerResult> {
    let mut process = Command::new("/bin/bash");
    process
        .arg("-lc")
        .arg(command)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = process.spawn().context("spawn host command")?;
    let stdout = child.stdout.take().context("capture host command stdout")?;
    let stderr = child.stderr.take().context("capture host command stderr")?;
    let stdout_task = tokio::spawn(capture_output(stdout, MAX_CAPTURE_BYTES));
    let stderr_task = tokio::spawn(capture_output(stderr, MAX_CAPTURE_BYTES));

    let (status, timed_out) = match timeout(Duration::from_secs(timeout_secs), child.wait()).await {
        Ok(waited) => (Some(waited.context("wait for host command")?), false),
        Err(_) => {
            child.kill().await.context("kill timed-out host command")?;
            let waited = child.wait().await.context("reap timed-out host command")?;
            (Some(waited), true)
        }
    };
    let stdout = join_capture(stdout_task).await;
    let stderr = join_capture(stderr_task).await;

    Ok(HostRunnerResult::Exec {
        exit_code: status.and_then(|value| value.code()),
        stdout,
        stderr,
        timed_out,
    })
}

#[cfg(unix)]
async fn capture_output<R>(mut reader: R, limit: usize) -> String
where
    R: AsyncRead + Unpin,
{
    let mut captured = Vec::new();
    let mut buffer = [0_u8; 8192];
    let mut truncated = false;
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(count) => {
                let remaining = limit.saturating_sub(captured.len());
                if remaining > 0 {
                    captured.extend_from_slice(&buffer[..count.min(remaining)]);
                }
                if count > remaining {
                    truncated = true;
                }
            }
        }
    }
    let mut text = String::from_utf8_lossy(&captured).into_owned();
    if truncated {
        text.push_str("\n[output truncated]");
    }
    text
}

#[cfg(unix)]
async fn join_capture(task: tokio::task::JoinHandle<String>) -> String {
    let mut task = task;
    match timeout(Duration::from_secs(2), &mut task).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) => format!("[output capture task failed: {error}]"),
        Err(_) => {
            task.abort();
            "[output capture timed out]".to_string()
        }
    }
}

#[cfg(unix)]
async fn spawn_job(
    context: &ServerContext,
    job_type: &str,
    command: &str,
    cwd: &Path,
) -> Result<HostRunnerResult> {
    let job_id = Uuid::new_v4().to_string();
    let job_dir = context.config.state_dir.join("jobs").join(&job_id);
    prepare_private_dir(&job_dir).await?;

    let stdout_path = job_dir.join("stdout.log");
    let stderr_path = job_dir.join("stderr.log");
    let exit_path = job_dir.join("exit_code");
    let stdout_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&stdout_path)
        .context("create host job stdout log")?;
    let stderr_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&stderr_path)
        .context("create host job stderr log")?;

    let mut metadata = JobMetadata {
        job_id: job_id.clone(),
        job_type: job_type.to_string(),
        command_sha256: sha256_hex(command.as_bytes()),
        cwd: cwd.to_path_buf(),
        created_at: Utc::now().to_rfc3339(),
        pid: None,
    };
    write_job_metadata(&job_dir, &metadata).await?;

    // The wrapper owns completion bookkeeping, so a Docker container
    // replacement (and even a host-runner process restart under KillMode=process)
    // cannot orphan the job's result.
    const WRAPPER: &str = r#"set +e
command_text=$1
exit_file=$2
/bin/bash -lc "$command_text"
status=$?
umask 077
tmp_file="${exit_file}.tmp.$$"
printf '%s\n' "$status" > "$tmp_file"
mv -f -- "$tmp_file" "$exit_file"
exit "$status"
"#;
    let mut process = Command::new("/bin/bash");
    process
        .arg("-c")
        .arg(WRAPPER)
        .arg("llamafarm-host-job")
        .arg(command)
        .arg(&exit_path)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(stderr_file))
        .kill_on_drop(false);
    let child = process.spawn().context("spawn durable host job")?;
    metadata.pid = child.id();
    write_job_metadata(&job_dir, &metadata).await?;
    drop(child);

    let job = read_job_status(&context.config.state_dir, &job_id).await?;
    Ok(HostRunnerResult::Job { job })
}

#[cfg(unix)]
async fn write_job_metadata(job_dir: &Path, metadata: &JobMetadata) -> Result<()> {
    let path = job_dir.join("job.json");
    let temp_path = job_dir.join("job.json.tmp");
    let encoded = serde_json::to_vec_pretty(metadata).context("serialize host job metadata")?;
    fs::write(&temp_path, encoded)
        .await
        .context("write host job metadata")?;
    fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o600))
        .await
        .context("set host job metadata permissions")?;
    fs::rename(&temp_path, &path)
        .await
        .context("publish host job metadata")
}

#[cfg(unix)]
async fn read_job_status(state_dir: &Path, job_id: &str) -> Result<HostJobStatus> {
    validate_identifier("job_id", job_id)?;
    let job_dir = state_dir.join("jobs").join(job_id);
    let encoded = fs::read(job_dir.join("job.json"))
        .await
        .with_context(|| format!("unknown host job {job_id}"))?;
    let metadata: JobMetadata =
        serde_json::from_slice(&encoded).context("parse host job metadata")?;
    if metadata.job_id != job_id {
        bail!("host job metadata id mismatch");
    }

    let exit_path = job_dir.join("exit_code");
    let (state, exit_code, completed_at) = match fs::read_to_string(&exit_path).await {
        Ok(value) => {
            let code = value
                .trim()
                .parse::<i32>()
                .context("parse host job exit code")?;
            let timestamp = fs::metadata(&exit_path)
                .await
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .map(DateTime::<Utc>::from)
                .map(|value| value.to_rfc3339());
            (
                if code == 0 {
                    HostJobState::Succeeded
                } else {
                    HostJobState::Failed
                },
                Some(code),
                timestamp,
            )
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let running = metadata
                .pid
                .is_some_and(|pid| PathBuf::from(format!("/proc/{pid}")).exists());
            (
                if running {
                    HostJobState::Running
                } else {
                    HostJobState::Lost
                },
                None,
                None,
            )
        }
        Err(error) => return Err(error).context("read host job exit code"),
    };

    Ok(HostJobStatus {
        job_id: metadata.job_id,
        job_type: metadata.job_type,
        state,
        created_at: metadata.created_at,
        completed_at,
        exit_code,
        pid: metadata.pid,
        stdout_tail: read_file_tail(&job_dir.join("stdout.log"), MAX_JOB_TAIL_BYTES).await?,
        stderr_tail: read_file_tail(&job_dir.join("stderr.log"), MAX_JOB_TAIL_BYTES).await?,
    })
}

#[cfg(unix)]
async fn read_file_tail(path: &Path, limit: usize) -> Result<String> {
    let mut file = fs::File::open(path)
        .await
        .with_context(|| format!("open host job output {}", path.display()))?;
    let length = file
        .metadata()
        .await
        .context("read host job output metadata")?
        .len();
    let start = length.saturating_sub(limit as u64);
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start))
            .await
            .context("seek host job output")?;
    }
    let mut output = Vec::with_capacity((length - start).min(limit as u64) as usize);
    file.read_to_end(&mut output)
        .await
        .context("read host job output")?;
    let mut text = String::from_utf8_lossy(&output).into_owned();
    if start > 0 {
        text.insert_str(0, "[earlier output truncated]\n");
    }
    Ok(text)
}

/// Send one protocol request and validate its correlated response.
#[cfg(unix)]
pub async fn send_request(
    socket_path: &Path,
    request: &HostRunnerRequest,
    request_timeout: Duration,
) -> Result<HostRunnerResponse> {
    if !socket_path.is_absolute() {
        bail!("host-runner socket path must be absolute");
    }
    let mut stream = timeout(request_timeout, UnixStream::connect(socket_path))
        .await
        .context("timed out connecting to host runner")?
        .with_context(|| format!("connect to host runner at {}", socket_path.display()))?;
    let mut encoded = serde_json::to_vec(request).context("serialize host-runner request")?;
    if encoded.len() + 1 > MAX_REQUEST_BYTES {
        bail!("host-runner request exceeds {MAX_REQUEST_BYTES} bytes");
    }
    encoded.push(b'\n');
    stream
        .write_all(&encoded)
        .await
        .context("write host-runner request")?;
    stream
        .shutdown()
        .await
        .context("finish host-runner request")?;

    let mut reader = BufReader::new(stream.take((MAX_RESPONSE_BYTES + 1) as u64));
    let mut wire = Vec::new();
    let read_result = timeout(request_timeout, reader.read_until(b'\n', &mut wire))
        .await
        .context("timed out waiting for host-runner response")??;
    if read_result == 0 {
        bail!("host runner closed the socket without a response");
    }
    if wire.len() > MAX_RESPONSE_BYTES {
        bail!("host-runner response exceeds {MAX_RESPONSE_BYTES} bytes");
    }
    let response: HostRunnerResponse =
        serde_json::from_slice(&wire).context("parse host-runner response")?;
    if response.protocol_version != HOST_RUNNER_PROTOCOL_VERSION {
        bail!(
            "host-runner response protocol mismatch: {}",
            response.protocol_version
        );
    }
    if response.request_id != request.request_id {
        bail!("host-runner response request_id mismatch");
    }
    Ok(response)
}

#[cfg(not(unix))]
pub async fn send_request(
    _socket_path: &Path,
    _request: &HostRunnerRequest,
    _request_timeout: Duration,
) -> Result<HostRunnerResponse> {
    bail!("the host runner requires Unix-domain socket support")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn protocol_round_trip_preserves_spawn_fields() {
        let request = HostRunnerRequest {
            protocol_version: HOST_RUNNER_PROTOCOL_VERSION,
            request_id: "request-123".to_string(),
            operation: HostRunnerOperation::Spawn {
                command: "docker compose ps".to_string(),
                cwd: Some(std::env::temp_dir()),
            },
        };

        let encoded = serde_json::to_vec(&request).unwrap();
        let decoded: HostRunnerRequest = serde_json::from_slice(&encoded).unwrap();

        assert_eq!(decoded, request);
        assert!(validate_request(&decoded, 300).is_ok());
    }

    #[test]
    fn validation_rejects_relative_cwd_before_io() {
        let request = HostRunnerRequest {
            protocol_version: HOST_RUNNER_PROTOCOL_VERSION,
            request_id: "request-123".to_string(),
            operation: HostRunnerOperation::Exec {
                command: "pwd".to_string(),
                cwd: Some(PathBuf::from("relative/path")),
                timeout_secs: Some(10),
            },
        };

        let error = validate_request(&request, 300).unwrap_err().to_string();
        assert!(error.contains("absolute host path"));
    }

    #[test]
    fn validation_rejects_oversized_timeout() {
        let request = HostRunnerRequest {
            protocol_version: HOST_RUNNER_PROTOCOL_VERSION,
            request_id: "request-123".to_string(),
            operation: HostRunnerOperation::Exec {
                command: "true".to_string(),
                cwd: None,
                timeout_secs: Some(301),
            },
        };

        let error = validate_request(&request, 300).unwrap_err().to_string();
        assert!(error.contains("between 1 and 300"));
    }

    #[test]
    fn validation_rejects_path_like_job_id() {
        let request = HostRunnerRequest {
            protocol_version: HOST_RUNNER_PROTOCOL_VERSION,
            request_id: "request-123".to_string(),
            operation: HostRunnerOperation::Status {
                job_id: "../../other-job".to_string(),
            },
        };

        assert!(validate_request(&request, 300).is_err());
    }

    #[test]
    fn command_audit_uses_digest_not_plaintext() {
        let operation = HostRunnerOperation::Exec {
            command: "echo sensitive-value".to_string(),
            cwd: None,
            timeout_secs: None,
        };

        let digest = operation.command_digest().unwrap();
        assert_eq!(digest.len(), 64);
        assert!(!digest.contains("sensitive-value"));
    }

    #[cfg(unix)]
    #[test]
    fn server_rejects_socket_outside_configured_home() {
        let config = HostRunnerServerConfig {
            home_dir: PathBuf::from("/home/operator"),
            socket_path: PathBuf::from("/tmp/host-runner.sock"),
            state_dir: PathBuf::from("/home/operator/.local/state/llamafarm/host-runner"),
            repo_dir: None,
            allow_exec: false,
            max_exec_timeout_secs: 300,
        };

        let error = validate_server_paths(&config).unwrap_err().to_string();
        assert!(error.contains("must stay under HOME"));
    }

    #[cfg(unix)]
    async fn wait_for_service(socket_path: &Path) {
        for _ in 0..100 {
            if UnixStream::connect(socket_path).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("host-runner test service did not start");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_protocol_health_and_durable_job_round_trip() {
        let temp = tempfile::tempdir().unwrap();
        let socket_path = temp.path().join("run/host-runner.sock");
        let state_dir = temp.path().join("state");
        let server = tokio::spawn(serve(HostRunnerServerConfig {
            home_dir: temp.path().to_path_buf(),
            socket_path: socket_path.clone(),
            state_dir: state_dir.clone(),
            repo_dir: None,
            allow_exec: true,
            max_exec_timeout_secs: 30,
        }));
        wait_for_service(&socket_path).await;

        let socket_mode = std::fs::metadata(&socket_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(socket_mode, 0o600);

        let health_request = HostRunnerRequest::new(HostRunnerOperation::Health);
        let health = send_request(&socket_path, &health_request, Duration::from_secs(2))
            .await
            .unwrap();
        assert!(health.success);
        assert!(matches!(
            health.result,
            Some(HostRunnerResult::Health {
                arbitrary_exec_enabled: true,
                ..
            })
        ));

        let spawn_request = HostRunnerRequest::new(HostRunnerOperation::Spawn {
            command: "printf 'durable-output\\n'".to_string(),
            cwd: Some(temp.path().to_path_buf()),
        });
        let spawned = send_request(&socket_path, &spawn_request, Duration::from_secs(2))
            .await
            .unwrap();
        let job_id = match spawned.result {
            Some(HostRunnerResult::Job { job }) => job.job_id,
            other => panic!("expected job result, got {other:?}"),
        };

        let mut completed = None;
        for _ in 0..100 {
            let status_request = HostRunnerRequest::new(HostRunnerOperation::Status {
                job_id: job_id.clone(),
            });
            let response = send_request(&socket_path, &status_request, Duration::from_secs(2))
                .await
                .unwrap();
            if let Some(HostRunnerResult::Job { job }) = response.result {
                if job.state != HostJobState::Running {
                    completed = Some(job);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let completed = completed.expect("durable job should finish");
        assert_eq!(completed.state, HostJobState::Succeeded);
        assert_eq!(completed.exit_code, Some(0));
        assert!(completed.stdout_tail.contains("durable-output"));

        let audit = fs::read_to_string(state_dir.join("audit.jsonl"))
            .await
            .unwrap();
        assert!(audit.contains("\"operation\":\"health\""));
        assert!(audit.contains("\"operation\":\"spawn\""));
        assert!(!audit.contains("durable-output"));

        server.abort();
    }
}
