use crate::config::ObservabilityConfig;
use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, RwLock};
use uuid::Uuid;

const DEFAULT_TRACE_REL_PATH: &str = "state/runtime-trace.jsonl";

/// Runtime trace storage policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeTraceStorageMode {
    None,
    Rolling,
    Full,
}

impl RuntimeTraceStorageMode {
    fn from_raw(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "rolling" => Self::Rolling,
            "full" => Self::Full,
            _ => Self::None,
        }
    }
}

/// Structured runtime trace event for tool-call and model-reply diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeTraceEvent {
    pub id: String,
    pub timestamp: String,
    pub event_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub success: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

struct RuntimeTraceLogger {
    mode: RuntimeTraceStorageMode,
    max_entries: usize,
    path: PathBuf,
    write_lock: std::sync::Mutex<()>,
}

impl RuntimeTraceLogger {
    fn new(mode: RuntimeTraceStorageMode, max_entries: usize, path: PathBuf) -> Self {
        Self {
            mode,
            max_entries: max_entries.max(1),
            path,
            write_lock: std::sync::Mutex::new(()),
        }
    }

    fn append(&self, event: &RuntimeTraceEvent) -> Result<()> {
        if self.mode == RuntimeTraceStorageMode::None {
            return Ok(());
        }

        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }

        let line = serde_json::to_string(event)?;
        let mut options = OpenOptions::new();
        options.create(true).append(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }

        let mut file = options.open(&self.path)?;
        writeln!(file, "{line}")?;
        file.sync_data()?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600));
        }

        if self.mode == RuntimeTraceStorageMode::Rolling {
            self.trim_to_last_entries()?;
        }

        Ok(())
    }

    fn trim_to_last_entries(&self) -> Result<()> {
        let raw = fs::read_to_string(&self.path).unwrap_or_default();
        let lines: Vec<&str> = raw
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect();

        if lines.len() <= self.max_entries {
            return Ok(());
        }

        let keep_from = lines.len().saturating_sub(self.max_entries);
        let kept = &lines[keep_from..];
        let mut rewritten = kept.join("\n");
        rewritten.push('\n');

        let tmp = self.path.with_extension(format!(
            "tmp.{}.{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::write(&tmp, rewritten)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }

        fs::rename(tmp, &self.path)?;
        Ok(())
    }
}

static TRACE_LOGGER: LazyLock<RwLock<Option<Arc<RuntimeTraceLogger>>>> =
    LazyLock::new(|| RwLock::new(None));

/// Resolve runtime trace storage mode from config.
pub fn storage_mode_from_config(config: &ObservabilityConfig) -> RuntimeTraceStorageMode {
    let mode = RuntimeTraceStorageMode::from_raw(&config.runtime_trace_mode);
    if mode == RuntimeTraceStorageMode::None
        && !config.runtime_trace_mode.trim().is_empty()
        && !config.runtime_trace_mode.eq_ignore_ascii_case("none")
    {
        tracing::warn!(
            mode = %config.runtime_trace_mode,
            "Unknown observability.runtime_trace_mode; falling back to none"
        );
    }
    mode
}

/// Resolve runtime trace path from config.
pub fn resolve_trace_path(config: &ObservabilityConfig, workspace_dir: &Path) -> PathBuf {
    let raw = config.runtime_trace_path.trim();
    let fallback = workspace_dir.join(DEFAULT_TRACE_REL_PATH);
    if raw.is_empty() {
        return fallback;
    }

    let configured = PathBuf::from(raw);
    if configured.is_absolute() {
        configured
    } else {
        workspace_dir.join(configured)
    }
}

/// Initialize (or disable) runtime trace logging.
pub fn init_from_config(config: &ObservabilityConfig, workspace_dir: &Path) {
    let mode = storage_mode_from_config(config);
    let logger = if mode == RuntimeTraceStorageMode::None {
        None
    } else {
        Some(Arc::new(RuntimeTraceLogger::new(
            mode,
            config.runtime_trace_max_entries.max(1),
            resolve_trace_path(config, workspace_dir),
        )))
    };

    let mut guard = TRACE_LOGGER.write().unwrap_or_else(|e| e.into_inner());
    *guard = logger;
}

/// Record a runtime trace event.
pub fn record_event(
    event_type: &str,
    channel: Option<&str>,
    provider: Option<&str>,
    model: Option<&str>,
    turn_id: Option<&str>,
    success: Option<bool>,
    message: Option<&str>,
    payload: Value,
) {
    let logger = TRACE_LOGGER
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(logger) = logger else {
        return;
    };

    let event = RuntimeTraceEvent {
        id: Uuid::new_v4().to_string(),
        timestamp: Utc::now().to_rfc3339(),
        event_type: event_type.to_string(),
        channel: channel.map(str::to_string),
        provider: provider.map(str::to_string),
        model: model.map(str::to_string),
        turn_id: turn_id.map(str::to_string),
        success,
        message: message.map(str::to_string),
        payload,
    };

    if let Err(err) = logger.append(&event) {
        tracing::warn!("Failed to write runtime trace event: {err}");
    }
}

/// Load recent runtime trace events from storage.
pub fn load_events(
    path: &Path,
    limit: usize,
    event_filter: Option<&str>,
    contains: Option<&str>,
) -> Result<Vec<RuntimeTraceEvent>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let raw = fs::read_to_string(path)?;
    let mut events = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        match serde_json::from_str::<RuntimeTraceEvent>(trimmed) {
            Ok(event) => events.push(event),
            Err(err) => tracing::warn!("Skipping malformed runtime trace line: {err}"),
        }
    }

    if let Some(filter) = event_filter.map(str::trim).filter(|f| !f.is_empty()) {
        let normalized = filter.to_ascii_lowercase();
        events.retain(|event| event.event_type.to_ascii_lowercase() == normalized);
    }

    if let Some(needle) = contains.map(str::trim).filter(|s| !s.is_empty()) {
        let needle = needle.to_ascii_lowercase();
        events.retain(|event| {
            let mut haystack = format!(
                "{} {} {}",
                event.event_type,
                event.message.as_deref().unwrap_or_default(),
                event.payload
            );
            if let Some(channel) = &event.channel {
                haystack.push_str(channel);
            }
            if let Some(provider) = &event.provider {
                haystack.push_str(provider);
            }
            if let Some(model) = &event.model {
                haystack.push_str(model);
            }
            haystack.to_ascii_lowercase().contains(&needle)
        });
    }

    if events.len() > limit {
        let keep_from = events.len() - limit;
        events = events.split_off(keep_from);
    }

    events.reverse();
    Ok(events)
}

// ── Per-run trace ─────────────────────────────────────────────────

/// Writer for a single autonomous run's trace file.
///
/// Each run gets its own JSONL file at `<workspace>/state/runs/<run_id>.jsonl`.
/// Append events with [`RunTracer::record`]; call [`RunTracer::close`] to write
/// the final summary event.  The file is usable even if `close` is not called
/// (e.g. on panic or cancellation).
pub struct RunTracer {
    run_id: String,
    path: PathBuf,
    started_at: String,
    write_lock: std::sync::Mutex<()>,
}

impl RunTracer {
    /// Open (create) a run trace file for `run_id` under `<workspace>/state/runs/`.
    pub fn open(workspace_dir: &Path, run_id: &str) -> Result<Self> {
        let dir = workspace_dir.join("state").join("runs");
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{run_id}.jsonl"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            OpenOptions::new()
                .create(true)
                .append(true)
                .mode(0o600)
                .open(&path)?;
        }
        #[cfg(not(unix))]
        {
            OpenOptions::new().create(true).append(true).open(&path)?;
        }

        Ok(Self {
            run_id: run_id.to_string(),
            path,
            started_at: Utc::now().to_rfc3339(),
            write_lock: std::sync::Mutex::new(()),
        })
    }

    /// Record an event into this run's trace file.
    pub fn record(
        &self,
        event_type: &str,
        success: Option<bool>,
        message: Option<&str>,
        payload: serde_json::Value,
    ) {
        let event = RuntimeTraceEvent {
            id: Uuid::new_v4().to_string(),
            timestamp: Utc::now().to_rfc3339(),
            event_type: event_type.to_string(),
            channel: None,
            provider: None,
            model: None,
            turn_id: Some(self.run_id.clone()),
            success,
            message: message.map(str::to_string),
            payload,
        };
        let _guard = self.write_lock.lock().unwrap_or_else(|e| e.into_inner());
        if let Ok(line) = serde_json::to_string(&event) {
            if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(&self.path) {
                let _ = writeln!(f, "{line}");
            }
        }
    }

    /// Write a final `run_summary` event and return the run ID.
    pub fn close(
        &self,
        success: bool,
        attempts: u32,
        elapsed_secs: u64,
        outcome: &str,
        final_answer_snippet: &str,
    ) {
        self.record(
            "run_summary",
            Some(success),
            Some(outcome),
            serde_json::json!({
                "run_id": self.run_id,
                "started_at": self.started_at,
                "ended_at": Utc::now().to_rfc3339(),
                "attempts": attempts,
                "elapsed_secs": elapsed_secs,
                "outcome": outcome,
                "final_answer_snippet": &final_answer_snippet[..final_answer_snippet.len().min(400)],
            }),
        );
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Format a run trace file as human-readable text for `llamafarm trace replay <run-id>`.
pub fn format_run_trace(path: &Path) -> Result<String> {
    if !path.exists() {
        anyhow::bail!("Run trace file not found: {}", path.display());
    }

    let raw = fs::read_to_string(path)?;
    let mut out = String::new();
    let mut event_count = 0usize;

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(event) = serde_json::from_str::<RuntimeTraceEvent>(trimmed) else {
            continue;
        };

        event_count += 1;
        let status = match event.success {
            Some(true) => "✓",
            Some(false) => "✗",
            None => "·",
        };
        let msg = event.message.as_deref().unwrap_or("");
        out.push_str(&format!(
            "[{}] {} {} {}\n",
            &event.timestamp[..19],
            status,
            event.event_type,
            msg,
        ));

        // For summary event, print the payload highlights.
        if event.event_type == "run_summary" {
            if let Some(obj) = event.payload.as_object() {
                for key in &["outcome", "attempts", "elapsed_secs", "final_answer_snippet"] {
                    if let Some(val) = obj.get(*key) {
                        out.push_str(&format!("    {key}: {val}\n"));
                    }
                }
            }
        }
    }

    if event_count == 0 {
        out.push_str("(empty run trace)");
    }
    Ok(out)
}

/// List all run trace files in `<workspace>/state/runs/`, most-recent first.
pub fn list_run_traces(workspace_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let dir = workspace_dir.join("state").join("runs");
    if !dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries: Vec<(std::time::SystemTime, String, PathBuf)> = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let run_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        entries.push((mtime, run_id, path));
    }

    entries.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(entries.into_iter().map(|(_, id, p)| (id, p)).collect())
}

/// Find a runtime trace event by id.
pub fn find_event_by_id(path: &Path, id: &str) -> Result<Option<RuntimeTraceEvent>> {
    if !path.exists() {
        return Ok(None);
    }

    let raw = fs::read_to_string(path)?;
    for line in raw.lines().rev() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(event) = serde_json::from_str::<RuntimeTraceEvent>(trimmed) {
            if event.id == id {
                return Ok(Some(event));
            }
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_observability_config() -> ObservabilityConfig {
        ObservabilityConfig {
            backend: "none".to_string(),
            otel_endpoint: None,
            otel_service_name: None,
            runtime_trace_mode: "rolling".to_string(),
            runtime_trace_path: "state/runtime-trace.jsonl".to_string(),
            runtime_trace_max_entries: 3,
        }
    }

    #[test]
    fn resolve_trace_path_relative_joins_workspace() {
        let cfg = test_observability_config();
        let workspace = tempfile::tempdir().unwrap();
        let path = resolve_trace_path(&cfg, workspace.path());
        assert_eq!(path, workspace.path().join("state/runtime-trace.jsonl"));
    }

    #[test]
    fn storage_mode_parses_known_values() {
        let mut cfg = test_observability_config();
        cfg.runtime_trace_mode = "none".into();
        assert_eq!(
            storage_mode_from_config(&cfg),
            RuntimeTraceStorageMode::None
        );

        cfg.runtime_trace_mode = "rolling".into();
        assert_eq!(
            storage_mode_from_config(&cfg),
            RuntimeTraceStorageMode::Rolling
        );

        cfg.runtime_trace_mode = "full".into();
        assert_eq!(
            storage_mode_from_config(&cfg),
            RuntimeTraceStorageMode::Full
        );
    }

    #[test]
    fn rolling_mode_keeps_latest_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let logger = RuntimeTraceLogger::new(RuntimeTraceStorageMode::Rolling, 2, path.clone());

        for i in 0..5 {
            let event = RuntimeTraceEvent {
                id: format!("id-{i}"),
                timestamp: Utc::now().to_rfc3339(),
                event_type: "test".into(),
                channel: None,
                provider: None,
                model: None,
                turn_id: None,
                success: None,
                message: Some(format!("event-{i}")),
                payload: serde_json::json!({ "i": i }),
            };
            logger.append(&event).unwrap();
        }

        let events = load_events(&path, 10, None, None).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].message.as_deref(), Some("event-4"));
        assert_eq!(events[1].message.as_deref(), Some("event-3"));
    }

    #[test]
    fn find_event_by_id_returns_match() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trace.jsonl");
        let logger = RuntimeTraceLogger::new(RuntimeTraceStorageMode::Full, 100, path.clone());

        let target_id = "target-event";
        let event = RuntimeTraceEvent {
            id: target_id.into(),
            timestamp: Utc::now().to_rfc3339(),
            event_type: "tool_call_result".into(),
            channel: Some("telegram".into()),
            provider: Some("openrouter".into()),
            model: Some("x".into()),
            turn_id: Some("turn-1".into()),
            success: Some(false),
            message: Some("boom".into()),
            payload: serde_json::json!({ "error": "boom" }),
        };
        logger.append(&event).unwrap();

        let found = find_event_by_id(&path, target_id).unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().id, target_id);
    }
}
