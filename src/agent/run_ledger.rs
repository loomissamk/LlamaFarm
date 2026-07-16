//! Durable planner → executor → verifier run ledger.
//!
//! Persists every run's plan steps, tool-call evidence, and deterministic
//! verification state to `<workspace>/state/runs/<run_id>.ledger.json` so that
//! "done" can be gated on recorded evidence instead of model prose, and so the
//! run inspector API/UI can show plan state, the tool timeline, artifacts,
//! attempts, and retry reasons for both live and historical runs.
//!
//! Wiring:
//! - `run_tool_call_loop` records every executed tool call as a [`ToolEvent`]
//!   when a ledger is in task-local scope, and mirrors `task_plan` calls into
//!   durable [`PlanStep`] records.
//! - [`AutonomousLoop`](super::autonomous::AutonomousLoop) scopes a ledger per
//!   run and refuses to report `Completed` while plan steps lack evidence.
//! - The gateway exposes `GET /api/runs` and `GET /api/runs/{id}` over the
//!   in-process registry (live runs) and the on-disk ledger files (history).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::agent::loop_::scrub_credentials;

const ARGS_SUMMARY_MAX: usize = 400;
const OUTPUT_EXCERPT_MAX: usize = 1200;
const MAX_EVENTS: usize = 2000;

// ── Data model ────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Blocked,
    Skipped,
}

impl StepStatus {
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(Self::Pending),
            "in_progress" => Some(Self::InProgress),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "blocked" => Some(Self::Blocked),
            "skipped" => Some(Self::Skipped),
            _ => None,
        }
    }

    /// A step no longer awaiting work (terminal, whether or not it succeeded).
    pub fn is_resolved(self) -> bool {
        !matches!(self, Self::Pending | Self::InProgress)
    }
}

/// One durable plan step with its evidence links and verifier state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanStep {
    pub id: usize,
    pub title: String,
    pub status: StepStatus,
    /// Tools this step is expected to use; empty means unrestricted.
    #[serde(default)]
    pub allowed_tools: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<usize>,
    /// Substring patterns that must appear in linked evidence (tool name,
    /// args summary, or output excerpt) for the step to verify.
    #[serde(default)]
    pub expected_evidence: Vec<String>,
    /// Sequence numbers of [`ToolEvent`]s recorded while this step was in progress.
    #[serde(default)]
    pub evidence: Vec<u64>,
    /// Deterministic verifier outcome — never set from model prose.
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub verifier_note: Option<String>,
}

/// One executed tool call, scrubbed and truncated for durable storage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolEvent {
    pub seq: u64,
    pub ts_ms: u64,
    pub tool: String,
    pub args_summary: String,
    pub success: bool,
    pub duration_ms: u64,
    /// SHA-256 (first 16 hex chars) of the full untruncated output.
    pub output_digest: String,
    pub output_excerpt: String,
    #[serde(default)]
    pub artifacts: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Completed,
    /// The model claimed completion but plan steps lacked verified evidence.
    CompletedUnverified,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunMeta {
    pub run_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    pub channel: String,
    pub provider: String,
    pub model: String,
    pub mode: String,
    pub started_at_ms: u64,
    #[serde(default)]
    pub ended_at_ms: Option<u64>,
    pub status: RunStatus,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub retry_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunLedgerData {
    pub meta: RunMeta,
    #[serde(default)]
    pub plan: Vec<PlanStep>,
    #[serde(default)]
    pub events: Vec<ToolEvent>,
    #[serde(default)]
    next_seq: u64,
}

// ── Ledger ────────────────────────────────────────────────────────

/// Thread-safe, file-backed run ledger. Every mutation persists atomically.
pub struct RunLedger {
    path: PathBuf,
    data: Mutex<RunLedgerData>,
}

impl RunLedger {
    /// Open (appending to an existing ledger file) or create a run ledger at
    /// `<workspace>/state/runs/<run_id>.ledger.json`.
    pub fn open_or_create(
        workspace_dir: &Path,
        run_id: &str,
        session_id: Option<&str>,
        channel: &str,
        provider: &str,
        model: &str,
        mode: &str,
    ) -> Result<Arc<Self>> {
        let dir = runs_dir(workspace_dir);
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating run ledger dir {}", dir.display()))?;
        let path = dir.join(format!("{}.ledger.json", sanitize_run_id(run_id)));

        let data = match std::fs::read_to_string(&path) {
            Ok(raw) => match serde_json::from_str::<RunLedgerData>(&raw) {
                Ok(mut existing) => {
                    existing.meta.status = RunStatus::Running;
                    existing.meta.ended_at_ms = None;
                    existing
                }
                Err(e) => {
                    tracing::warn!("Corrupt run ledger {} — starting fresh: {e}", path.display());
                    fresh_data(run_id, session_id, channel, provider, model, mode)
                }
            },
            Err(_) => fresh_data(run_id, session_id, channel, provider, model, mode),
        };

        let ledger = Arc::new(Self {
            path,
            data: Mutex::new(data),
        });
        ledger.save();
        register(&ledger);
        Ok(ledger)
    }

    pub fn run_id(&self) -> String {
        self.data.lock().unwrap().meta.run_id.clone()
    }

    /// Record one executed tool call and link it as evidence to the current
    /// in-progress plan step (when tool use is consistent with that step).
    pub fn record_tool_event(
        &self,
        tool: &str,
        args: &serde_json::Value,
        success: bool,
        duration_ms: u64,
        output: &str,
    ) {
        if tool == "task_plan" {
            self.mirror_task_plan(args, success);
        }

        let mut data = self.data.lock().unwrap();
        if data.events.len() >= MAX_EVENTS {
            drop(data);
            return;
        }
        let seq = data.next_seq;
        data.next_seq += 1;

        let event = ToolEvent {
            seq,
            ts_ms: now_ms(),
            tool: tool.to_string(),
            args_summary: truncate_chars(&scrub_credentials(&args.to_string()), ARGS_SUMMARY_MAX),
            success,
            duration_ms,
            output_digest: digest16(output),
            output_excerpt: truncate_chars(&scrub_credentials(output), OUTPUT_EXCERPT_MAX),
            artifacts: extract_artifacts(tool, args),
        };

        // Attach as evidence to the active step: prefer an in-progress step
        // whose allowed_tools permit this tool, else any in-progress step.
        if tool != "task_plan" {
            let idx = data
                .plan
                .iter()
                .position(|s| {
                    s.status == StepStatus::InProgress
                        && (s.allowed_tools.is_empty()
                            || s.allowed_tools.iter().any(|t| t == tool))
                })
                .or_else(|| {
                    data.plan
                        .iter()
                        .position(|s| s.status == StepStatus::InProgress)
                });
            if let Some(i) = idx {
                data.plan[i].evidence.push(seq);
            }
        }

        data.events.push(event);
        drop(data);
        self.verify_and_save();
    }

    /// Mirror `task_plan` tool calls into durable plan records.
    fn mirror_task_plan(&self, args: &serde_json::Value, success: bool) {
        if !success {
            return;
        }
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let mut data = self.data.lock().unwrap();
        match action {
            "create" => {
                let tasks = args
                    .get("tasks")
                    .or_else(|| args.get("steps"))
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();
                data.plan = tasks
                    .iter()
                    .enumerate()
                    .filter_map(|(i, entry)| {
                        let title = entry.get("title").and_then(|v| v.as_str())?;
                        let status = entry
                            .get("status")
                            .and_then(|v| v.as_str())
                            .and_then(StepStatus::from_str)
                            .unwrap_or(StepStatus::Pending);
                        Some(PlanStep {
                            id: i + 1,
                            title: title.to_string(),
                            status,
                            allowed_tools: str_list(entry.get("allowed_tools")),
                            depends_on: usize_list(entry.get("depends_on")),
                            expected_evidence: str_list(entry.get("expected_evidence")),
                            evidence: Vec::new(),
                            verified: false,
                            verifier_note: None,
                        })
                    })
                    .collect();
            }
            "add" => {
                if let Some(title) = args.get("title").and_then(|v| v.as_str()) {
                    let id = data.plan.iter().map(|s| s.id).max().unwrap_or(0) + 1;
                    data.plan.push(PlanStep {
                        id,
                        title: title.to_string(),
                        status: StepStatus::Pending,
                        allowed_tools: Vec::new(),
                        depends_on: Vec::new(),
                        expected_evidence: Vec::new(),
                        evidence: Vec::new(),
                        verified: false,
                        verifier_note: None,
                    });
                }
            }
            "update" => {
                let id = args.get("id").and_then(|v| v.as_u64()).map(|v| v as usize);
                let status = args
                    .get("status")
                    .and_then(|v| v.as_str())
                    .and_then(StepStatus::from_str);
                if let (Some(id), Some(status)) = (id, status) {
                    if let Some(step) = data.plan.iter_mut().find(|s| s.id == id) {
                        step.status = status;
                    }
                }
            }
            "delete" => data.plan.clear(),
            _ => {}
        }
        drop(data);
        self.verify_and_save();
    }

    /// Deterministic verifier: a step verifies only when it is marked
    /// completed, has at least one successful evidence event, its dependencies
    /// verified, and every expected-evidence pattern matches linked evidence.
    fn verify_steps(data: &mut RunLedgerData) {
        let events: HashMap<u64, ToolEvent> = data
            .events
            .iter()
            .map(|e| (e.seq, e.clone()))
            .collect();
        let mut verified_ids: Vec<usize> = Vec::new();
        // Steps are ordered; verify in plan order so depends_on can resolve.
        for i in 0..data.plan.len() {
            let step = &data.plan[i];
            let mut note = None;
            let mut ok = step.status == StepStatus::Completed;
            if ok {
                let successful: Vec<&ToolEvent> = step
                    .evidence
                    .iter()
                    .filter_map(|seq| events.get(seq))
                    .filter(|e| e.success)
                    .collect();
                if successful.is_empty() {
                    ok = false;
                    note = Some("completed without any successful tool evidence".to_string());
                } else if let Some(missing) = step.expected_evidence.iter().find(|pat| {
                    !successful.iter().any(|e| {
                        e.tool.contains(pat.as_str())
                            || e.args_summary.contains(pat.as_str())
                            || e.output_excerpt.contains(pat.as_str())
                    })
                }) {
                    ok = false;
                    note = Some(format!("expected evidence not found: {missing}"));
                } else if let Some(dep) = step
                    .depends_on
                    .iter()
                    .find(|dep| !verified_ids.contains(dep))
                {
                    ok = false;
                    note = Some(format!("dependency step {dep} is not verified"));
                }
            }
            if ok {
                verified_ids.push(step.id);
            }
            let step = &mut data.plan[i];
            step.verified = ok;
            step.verifier_note = note;
        }
    }

    /// Human-readable summary of plan steps that block verified completion.
    /// `None` means the plan (if any) is fully resolved with verified evidence.
    pub fn unverified_plan_summary(&self) -> Option<String> {
        let mut data = self.data.lock().unwrap();
        if data.plan.is_empty() {
            return None;
        }
        Self::verify_steps(&mut data);
        let blockers: Vec<String> = data
            .plan
            .iter()
            .filter(|s| match s.status {
                StepStatus::Pending | StepStatus::InProgress => true,
                StepStatus::Completed => !s.verified,
                // Failed/blocked/skipped are honest terminal states, not blockers.
                _ => false,
            })
            .map(|s| {
                let why = match s.status {
                    StepStatus::Pending => "still pending".to_string(),
                    StepStatus::InProgress => "still in progress".to_string(),
                    _ => s
                        .verifier_note
                        .clone()
                        .unwrap_or_else(|| "unverified".to_string()),
                };
                format!("- step {} \"{}\": {}", s.id, s.title, why)
            })
            .collect();
        if blockers.is_empty() {
            None
        } else {
            Some(blockers.join("\n"))
        }
    }

    /// Whether the ledger holds any plan steps at all.
    pub fn has_plan(&self) -> bool {
        !self.data.lock().unwrap().plan.is_empty()
    }

    pub fn set_attempt(&self, attempts: u32, retry_reason: Option<String>) {
        {
            let mut data = self.data.lock().unwrap();
            data.meta.attempts = attempts;
            data.meta.retry_reason = retry_reason;
        }
        self.save();
    }

    /// Close the run with a terminal status and persist final verifier state.
    pub fn finalize(&self, status: RunStatus) {
        {
            let mut data = self.data.lock().unwrap();
            Self::verify_steps(&mut data);
            data.meta.status = status;
            data.meta.ended_at_ms = Some(now_ms());
        }
        self.save();
        unregister(&self.run_id());
    }

    pub fn snapshot(&self) -> RunLedgerData {
        let mut data = self.data.lock().unwrap();
        Self::verify_steps(&mut data);
        data.clone()
    }

    fn verify_and_save(&self) {
        {
            let mut data = self.data.lock().unwrap();
            Self::verify_steps(&mut data);
        }
        self.save();
    }

    fn save(&self) {
        let (json, path) = {
            let data = self.data.lock().unwrap();
            match serde_json::to_string(&*data) {
                Ok(j) => (j, self.path.clone()),
                Err(e) => {
                    tracing::warn!("Could not serialize run ledger: {e}");
                    return;
                }
            }
        };
        let tmp = path.with_extension("json.tmp");
        if let Err(e) =
            std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &path))
        {
            tracing::warn!("Could not persist run ledger {}: {e}", path.display());
        }
    }
}

// ── Task-local scope + live registry ─────────────────────────────

tokio::task_local! {
    /// Ledger for the run executing on this task, mirroring `TOOL_CACHE`.
    pub static RUN_LEDGER: Option<Arc<RunLedger>>;
}

/// The ledger scoped to the current task, if any.
pub fn current() -> Option<Arc<RunLedger>> {
    RUN_LEDGER.try_with(|l| l.clone()).ok().flatten()
}

static ACTIVE_RUNS: LazyLock<RwLock<HashMap<String, Arc<RunLedger>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn register(ledger: &Arc<RunLedger>) {
    ACTIVE_RUNS
        .write()
        .unwrap()
        .insert(ledger.run_id(), ledger.clone());
}

fn unregister(run_id: &str) {
    ACTIVE_RUNS.write().unwrap().remove(run_id);
}

/// Snapshot of a live (still registered) run, if present.
pub fn live_snapshot(run_id: &str) -> Option<RunLedgerData> {
    let ledger = ACTIVE_RUNS.read().unwrap().get(run_id).cloned();
    ledger.map(|l| l.snapshot())
}

/// Run IDs currently registered as live.
pub fn live_run_ids() -> Vec<String> {
    ACTIVE_RUNS.read().unwrap().keys().cloned().collect()
}

// ── Disk enumeration for the inspector API ───────────────────────

pub fn runs_dir(workspace_dir: &Path) -> PathBuf {
    workspace_dir.join("state").join("runs")
}

/// Load one ledger snapshot by run id — live registry first, then disk.
pub fn load_snapshot(workspace_dir: &Path, run_id: &str) -> Option<RunLedgerData> {
    if let Some(live) = live_snapshot(run_id) {
        return Some(live);
    }
    let path = runs_dir(workspace_dir).join(format!("{}.ledger.json", sanitize_run_id(run_id)));
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&raw).ok()
}

/// List ledger metadata for all runs (live + on disk), newest first.
pub fn list_runs(workspace_dir: &Path, limit: usize) -> Vec<RunMeta> {
    let mut metas: HashMap<String, RunMeta> = HashMap::new();
    if let Ok(entries) = std::fs::read_dir(runs_dir(workspace_dir)) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.ends_with(".ledger.json") {
                continue;
            }
            if let Ok(raw) = std::fs::read_to_string(entry.path()) {
                if let Ok(data) = serde_json::from_str::<RunLedgerData>(&raw) {
                    metas.insert(data.meta.run_id.clone(), data.meta);
                }
            }
        }
    }
    // Only merge live runs that belong to this workspace — the registry is
    // process-global and may hold runs from other workspaces.
    let dir = runs_dir(workspace_dir);
    let live: Vec<Arc<RunLedger>> = ACTIVE_RUNS.read().unwrap().values().cloned().collect();
    for ledger in live {
        if ledger.path.starts_with(&dir) {
            let data = ledger.snapshot();
            metas.insert(data.meta.run_id.clone(), data.meta);
        }
    }
    let mut list: Vec<RunMeta> = metas.into_values().collect();
    list.sort_by(|a, b| b.started_at_ms.cmp(&a.started_at_ms));
    list.truncate(limit);
    list
}

// ── Helpers ───────────────────────────────────────────────────────

fn fresh_data(
    run_id: &str,
    session_id: Option<&str>,
    channel: &str,
    provider: &str,
    model: &str,
    mode: &str,
) -> RunLedgerData {
    RunLedgerData {
        meta: RunMeta {
            run_id: run_id.to_string(),
            session_id: session_id.map(str::to_string),
            channel: channel.to_string(),
            provider: provider.to_string(),
            model: model.to_string(),
            mode: mode.to_string(),
            started_at_ms: now_ms(),
            ended_at_ms: None,
            status: RunStatus::Running,
            attempts: 0,
            retry_reason: None,
        },
        plan: Vec::new(),
        events: Vec::new(),
        next_seq: 0,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn digest16(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    let hex = format!("{:x}", hasher.finalize());
    hex[..16].to_string()
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}…")
}

fn sanitize_run_id(run_id: &str) -> String {
    run_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn str_list(v: Option<&serde_json::Value>) -> Vec<String> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

fn usize_list(v: Option<&serde_json::Value>) -> Vec<usize> {
    v.and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| e.as_u64().map(|n| n as usize))
                .collect()
        })
        .unwrap_or_default()
}

/// Best-effort artifact path extraction from mutating tool arguments.
fn extract_artifacts(tool: &str, args: &serde_json::Value) -> Vec<String> {
    match tool {
        "file_write" => args
            .get("path")
            .and_then(|v| v.as_str())
            .map(|p| vec![p.to_string()])
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_ws(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "lf-run-ledger-{name}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[tokio::test]
    async fn plan_mirroring_and_evidence_gating() {
        let ws = temp_ws("gating");
        let ledger =
            RunLedger::open_or_create(&ws, "run-1", None, "test", "ollama", "m", "operator")
                .unwrap();

        // Plan created via task_plan mirror.
        ledger.record_tool_event(
            "task_plan",
            &json!({"action": "create", "tasks": [
                {"title": "write file", "status": "in_progress"},
                {"title": "verify file"}
            ]}),
            true,
            5,
            "Tasks (0/2 completed)",
        );
        assert!(ledger.has_plan());
        // Both steps unresolved → blockers reported.
        assert!(ledger.unverified_plan_summary().is_some());

        // Evidence lands on the in-progress step.
        ledger.record_tool_event(
            "file_write",
            &json!({"path": "/tmp/x.txt", "content": "hi"}),
            true,
            12,
            "wrote 2 bytes",
        );
        ledger.record_tool_event(
            "task_plan",
            &json!({"action": "update", "id": 1, "status": "completed"}),
            true,
            2,
            "updated",
        );
        // Step 2 still pending → not done.
        assert!(ledger.unverified_plan_summary().is_some());

        // Completing step 2 WITHOUT evidence must not verify.
        ledger.record_tool_event(
            "task_plan",
            &json!({"action": "update", "id": 2, "status": "completed"}),
            true,
            2,
            "updated",
        );
        let summary = ledger.unverified_plan_summary().expect("step 2 lacks evidence");
        assert!(summary.contains("step 2"), "summary: {summary}");
        assert!(summary.contains("without any successful tool evidence"));

        // Give step 2 evidence via in_progress + a successful read.
        ledger.record_tool_event(
            "task_plan",
            &json!({"action": "update", "id": 2, "status": "in_progress"}),
            true,
            2,
            "updated",
        );
        ledger.record_tool_event(
            "file_read",
            &json!({"path": "/tmp/x.txt"}),
            true,
            3,
            "hi",
        );
        ledger.record_tool_event(
            "task_plan",
            &json!({"action": "update", "id": 2, "status": "completed"}),
            true,
            2,
            "updated",
        );
        assert!(ledger.unverified_plan_summary().is_none());

        ledger.finalize(RunStatus::Completed);
        let snap = load_snapshot(&ws, "run-1").expect("persisted ledger");
        assert_eq!(snap.meta.status, RunStatus::Completed);
        assert!(snap.plan.iter().all(|s| s.verified));
        assert_eq!(snap.events.iter().filter(|e| e.tool != "task_plan").count(), 2);
        std::fs::remove_dir_all(&ws).ok();
    }

    #[tokio::test]
    async fn expected_evidence_patterns_enforced() {
        let ws = temp_ws("expected");
        let ledger =
            RunLedger::open_or_create(&ws, "run-2", None, "test", "ollama", "m", "operator")
                .unwrap();
        ledger.record_tool_event(
            "task_plan",
            &json!({"action": "create", "tasks": [
                {"title": "run tests", "status": "in_progress",
                 "expected_evidence": ["test result: ok"]}
            ]}),
            true,
            1,
            "created",
        );
        ledger.record_tool_event("shell", &json!({"command": "cargo test"}), true, 900, "1 failed");
        ledger.record_tool_event(
            "task_plan",
            &json!({"action": "update", "id": 1, "status": "completed"}),
            true,
            1,
            "updated",
        );
        let summary = ledger.unverified_plan_summary().expect("pattern unmet");
        assert!(summary.contains("expected evidence not found"));

        ledger.record_tool_event(
            "task_plan",
            &json!({"action": "update", "id": 1, "status": "in_progress"}),
            true,
            1,
            "updated",
        );
        ledger.record_tool_event(
            "shell",
            &json!({"command": "cargo test"}),
            true,
            900,
            "test result: ok. 42 passed",
        );
        ledger.record_tool_event(
            "task_plan",
            &json!({"action": "update", "id": 1, "status": "completed"}),
            true,
            1,
            "updated",
        );
        assert!(ledger.unverified_plan_summary().is_none());
        ledger.finalize(RunStatus::Completed);
        std::fs::remove_dir_all(&ws).ok();
    }

    #[tokio::test]
    async fn list_runs_orders_newest_first() {
        let ws = temp_ws("list");
        let a = RunLedger::open_or_create(&ws, "run-a", None, "t", "o", "m", "chat").unwrap();
        a.finalize(RunStatus::Failed);
        std::thread::sleep(std::time::Duration::from_millis(5));
        let b = RunLedger::open_or_create(&ws, "run-b", Some("sess-1"), "t", "o", "m", "chat")
            .unwrap();
        b.finalize(RunStatus::Completed);
        let runs = list_runs(&ws, 10);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].run_id, "run-b");
        assert_eq!(runs[0].session_id.as_deref(), Some("sess-1"));
        std::fs::remove_dir_all(&ws).ok();
    }
}
