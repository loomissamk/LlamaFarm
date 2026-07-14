//! Autonomous multi-turn run loop.
//!
//! Wraps [`run_tool_call_loop`] with an outer retry/recovery cycle that drives
//! the agent through long multi-step tasks without per-step human approval.
//!
//! ## Cycle structure
//!
//! ```text
//! for attempt in 0..retry_budget {
//!     check wall-clock cap
//!     inject execution-mode system context (chaos, autonomous, …)
//!     run_tool_call_loop  → final_answer
//!     if objective_complete(final_answer) → return Completed
//!     inject recovery prompt
//! }
//! return RetryBudgetExhausted
//! ```
//!
//! In `chaos_lab` mode a hypothesis/recovery prompt is injected before each
//! retry so the model actively diagnoses and attempts alternate fixes rather
//! than re-running the same plan.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::agent::tool_cache::ToolResultCache;
use crate::config::{AgentExecutionMode, MultimodalConfig};
use crate::observability::{runtime_trace, Observer};
use crate::observability::runtime_trace::RunTracer;
use crate::providers::{ChatMessage, Provider};
use crate::tools::Tool;

use super::loop_::{compact_history_with_focus, run_tool_call_loop, TOOL_CACHE};

// ── Outcome ───────────────────────────────────────────────────────

/// Result of an [`AutonomousLoop`] run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopOutcome {
    /// Objective completed (model produced a final answer without failure signals).
    Completed { attempts: u32, final_answer: String },
    /// Retry budget exhausted before the objective was confirmed complete.
    RetryBudgetExhausted { attempts: u32, last_answer: String },
    /// Optional wall-clock cap was reached.
    WallClockCapReached { elapsed_secs: u64, last_answer: String },
    /// External cancellation token fired.
    Cancelled,
}

impl LoopOutcome {
    /// The last answer returned by the model, regardless of outcome.
    pub fn last_answer(&self) -> &str {
        match self {
            Self::Completed { final_answer, .. } => final_answer,
            Self::RetryBudgetExhausted { last_answer, .. } => last_answer,
            Self::WallClockCapReached { last_answer, .. } => last_answer,
            Self::Cancelled => "",
        }
    }

    /// Whether the loop considers the objective met.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }
}

// ── Builder ───────────────────────────────────────────────────────

/// Builder / driver for the autonomous multi-turn cycle.
///
/// Construct via [`AutonomousLoop::new`], configure with builder methods,
/// then call [`AutonomousLoop::run`].
pub struct AutonomousLoop<'a> {
    mode: AgentExecutionMode,
    retry_budget: u32,
    wall_clock_cap: Option<Duration>,
    provider: &'a dyn Provider,
    tools_registry: &'a [Box<dyn Tool>],
    observer: &'a dyn Observer,
    provider_name: &'a str,
    model: &'a str,
    temperature: f64,
    channel_name: &'a str,
    multimodal_config: &'a MultimodalConfig,
    max_tool_iterations: usize,
    cancellation_token: Option<CancellationToken>,
    excluded_tools: &'a [String],
    /// Unique run identifier.  Auto-generated as a UUID v4 if not set.
    run_id: String,
    /// Workspace directory for per-run trace files (`state/runs/<run_id>.jsonl`).
    workspace_dir: Option<PathBuf>,
    /// Tool-result cache shared across all retry attempts in this run.
    tool_cache: std::sync::Arc<ToolResultCache>,
}

impl<'a> AutonomousLoop<'a> {
    /// Create a new loop driver.  Call builder methods then [`run`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        mode: AgentExecutionMode,
        retry_budget: u32,
        wall_clock_cap_secs: u64,
        provider: &'a dyn Provider,
        tools_registry: &'a [Box<dyn Tool>],
        observer: &'a dyn Observer,
        provider_name: &'a str,
        model: &'a str,
        temperature: f64,
        channel_name: &'a str,
        multimodal_config: &'a MultimodalConfig,
        max_tool_iterations: usize,
        cancellation_token: Option<CancellationToken>,
        excluded_tools: &'a [String],
    ) -> Self {
        let wall_clock_cap = if wall_clock_cap_secs > 0 {
            Some(Duration::from_secs(wall_clock_cap_secs))
        } else {
            None
        };
        Self {
            mode,
            retry_budget: retry_budget.max(1),
            wall_clock_cap,
            provider,
            tools_registry,
            observer,
            provider_name,
            model,
            temperature,
            channel_name,
            multimodal_config,
            max_tool_iterations,
            cancellation_token,
            excluded_tools,
            run_id: Uuid::new_v4().to_string(),
            workspace_dir: None,
            tool_cache: std::sync::Arc::new(ToolResultCache::default_ttl()),
        }
    }

    /// Set the workspace directory so per-run trace files are written to
    /// `<workspace>/state/runs/<run_id>.jsonl`.
    pub fn with_workspace(mut self, workspace_dir: &Path) -> Self {
        self.workspace_dir = Some(workspace_dir.to_path_buf());
        self
    }

    /// Override the auto-generated run ID (useful for resuming or forking runs).
    pub fn with_run_id(mut self, run_id: impl Into<String>) -> Self {
        self.run_id = run_id.into();
        self
    }

    /// Fork this run configuration with a fresh run ID, keeping all other settings.
    ///
    /// The caller should snapshot `history` at the desired checkpoint and pass
    /// the clone to the forked loop's [`run`].
    pub fn fork(&self) -> AutonomousLoopFork {
        AutonomousLoopFork {
            run_id: Uuid::new_v4().to_string(),
            parent_run_id: self.run_id.clone(),
        }
    }

    /// The run ID for this autonomous run.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    // ── Execution ─────────────────────────────────────────────────

    /// Drive the autonomous cycle until the objective is met, budget is
    /// exhausted, or the wall-clock cap is reached.
    ///
    /// `history` is the live conversation buffer.  The caller is responsible
    /// for injecting the initial user objective message before calling `run`.
    pub async fn run(&self, history: &mut Vec<ChatMessage>) -> Result<LoopOutcome> {
        let start = Instant::now();

        // ── per-run trace ───────────────────────────────────────
        let tracer = self.workspace_dir.as_deref().and_then(|dir| {
            RunTracer::open(dir, &self.run_id)
                .map_err(|e| {
                    tracing::warn!("Could not open per-run trace for {}: {e}", self.run_id);
                    e
                })
                .ok()
        });
        if let Some(ref t) = tracer {
            t.record(
                "run_start",
                Some(true),
                Some("autonomous run started"),
                serde_json::json!({
                    "run_id": self.run_id,
                    "mode": self.mode.to_string(),
                    "model": self.model,
                    "retry_budget": self.retry_budget,
                }),
            );
        }

        // Inject mode-specific system context once per autonomous run.
        self.inject_system_context(history);

        let mut last_answer = String::new();

        for attempt in 0..self.retry_budget {
            // ── wall-clock cap ──────────────────────────────────
            if let Some(cap) = self.wall_clock_cap {
                let elapsed = start.elapsed();
                if elapsed >= cap {
                    runtime_trace::record_event(
                        "autonomous_loop_wall_clock_cap",
                        Some(self.channel_name),
                        Some(self.provider_name),
                        Some(self.model),
                        None,
                        Some(false),
                        Some("wall-clock cap reached"),
                        serde_json::json!({
                            "elapsed_secs": elapsed.as_secs(),
                            "cap_secs": cap.as_secs(),
                            "attempt": attempt,
                        }),
                    );
                    if let Some(ref t) = tracer {
                        t.close(false, attempt, elapsed.as_secs(), "wall_clock_cap", &last_answer);
                    }
                    return Ok(LoopOutcome::WallClockCapReached {
                        elapsed_secs: elapsed.as_secs(),
                        last_answer,
                    });
                }
            }

            // ── cancellation ────────────────────────────────────
            if self
                .cancellation_token
                .as_ref()
                .is_some_and(CancellationToken::is_cancelled)
            {
                return Ok(LoopOutcome::Cancelled);
            }

            runtime_trace::record_event(
                "autonomous_loop_attempt_start",
                Some(self.channel_name),
                Some(self.provider_name),
                Some(self.model),
                None,
                Some(true),
                Some("starting attempt"),
                serde_json::json!({
                    "attempt": attempt,
                    "retry_budget": self.retry_budget,
                    "mode": self.mode.to_string(),
                }),
            );

            let answer = TOOL_CACHE
                .scope(
                    Some(self.tool_cache.clone()),
                    run_tool_call_loop(
                        self.provider,
                        history,
                        self.tools_registry,
                        self.observer,
                        self.provider_name,
                        self.model,
                        self.temperature,
                        /* silent */ false,
                        /* approval */ None,
                        self.channel_name,
                        self.multimodal_config,
                        self.max_tool_iterations,
                        self.cancellation_token.clone(),
                        /* on_delta */ None,
                        /* hooks */ None,
                        self.excluded_tools,
                    ),
                )
                .await?;
            // Invalidate cache after each attempt — mutating tools may have
            // changed on-disk state.  Read-only results within a single attempt
            // are still served from cache via the task-local scope above.
            self.tool_cache.invalidate_all();

            last_answer = answer.clone();

            // ── completion detection ─────────────────────────────
            if !is_failure_response(&answer) {
                runtime_trace::record_event(
                    "autonomous_loop_completed",
                    Some(self.channel_name),
                    Some(self.provider_name),
                    Some(self.model),
                    None,
                    Some(true),
                    Some("objective completed"),
                    serde_json::json!({
                        "attempt": attempt,
                        "elapsed_secs": start.elapsed().as_secs(),
                    }),
                );
                if let Some(ref t) = tracer {
                    t.close(true, attempt + 1, start.elapsed().as_secs(), "completed", &answer);
                }
                return Ok(LoopOutcome::Completed {
                    attempts: attempt + 1,
                    final_answer: answer,
                });
            }

            // ── this attempt failed — compact then inject recovery prompt ──
            if attempt + 1 < self.retry_budget {
                // Compact history with objective focus so long runs don't
                // overflow the context window.  Extract the first user
                // message as the focus hint (owned so the borrow ends before
                // the mutable compact call).
                let objective: Option<String> = history
                    .iter()
                    .find(|m| m.role == "user")
                    .map(|m| m.content.clone());
                let _ = compact_history_with_focus(
                    history,
                    self.provider,
                    self.model,
                    // Keep at most 2× max_tool_iterations messages between compactions.
                    self.max_tool_iterations.saturating_mul(2).max(20),
                    objective.as_deref(),
                )
                .await;

                let recovery_prompt = self.build_recovery_prompt(attempt, &answer);
                runtime_trace::record_event(
                    "autonomous_loop_retry",
                    Some(self.channel_name),
                    Some(self.provider_name),
                    Some(self.model),
                    None,
                    Some(false),
                    Some("injecting recovery prompt"),
                    serde_json::json!({
                        "attempt": attempt,
                        "remaining": self.retry_budget - attempt - 1,
                    }),
                );
                history.push(ChatMessage::user(recovery_prompt));
            }
        }

        runtime_trace::record_event(
            "autonomous_loop_budget_exhausted",
            Some(self.channel_name),
            Some(self.provider_name),
            Some(self.model),
            None,
            Some(false),
            Some("retry budget exhausted"),
            serde_json::json!({
                "attempts": self.retry_budget,
                "elapsed_secs": start.elapsed().as_secs(),
            }),
        );

        if let Some(ref t) = tracer {
            t.close(
                false,
                self.retry_budget,
                start.elapsed().as_secs(),
                "retry_budget_exhausted",
                &last_answer,
            );
        }
        Ok(LoopOutcome::RetryBudgetExhausted {
            attempts: self.retry_budget,
            last_answer,
        })
    }

    // ── Helpers ───────────────────────────────────────────────────

    /// Inject a mode-specific addendum into the system message so the model
    /// understands the execution context from the first turn.
    fn inject_system_context(&self, history: &mut Vec<ChatMessage>) {
        let Some(system_msg) = history.iter_mut().find(|m| m.role == "system") else {
            return;
        };
        let addendum = match self.mode {
            AgentExecutionMode::ChaosLab => CHAOS_LAB_SYSTEM_ADDENDUM,
            AgentExecutionMode::AutonomousOperator | AgentExecutionMode::Operator => {
                AUTONOMOUS_OPERATOR_SYSTEM_ADDENDUM
            }
            AgentExecutionMode::Chat => return,
        };
        if !system_msg.content.contains("## Execution Mode") {
            system_msg.content.push_str(addendum);
        }
    }

    /// Build a recovery / retry prompt appropriate to the current mode.
    fn build_recovery_prompt(&self, attempt: u32, last_answer: &str) -> String {
        match self.mode {
            AgentExecutionMode::ChaosLab => {
                format!(
                    "## Recovery Required (attempt {attempt})\n\n\
                     Your last action produced a failure or left the objective unmet.\n\
                     Last response summary: {snippet}\n\n\
                     You are operating in chaos_lab mode on a disposable target — \
                     it is acceptable to mutate state, restart services, or take \
                     destructive corrective actions.\n\n\
                     Steps:\n\
                     1. Inspect relevant logs and current state.\n\
                     2. Form a new hypothesis about the root cause.\n\
                     3. Apply a different fix than you tried before.\n\
                     4. Verify the outcome.\n\
                     5. If still failing, escalate the intervention.\n\n\
                     Do not repeat the same action that already failed. \
                     Continue without waiting for approval.",
                    attempt = attempt + 1,
                    snippet = truncate(last_answer, 300),
                )
            }
            AgentExecutionMode::AutonomousOperator | AgentExecutionMode::Operator => {
                format!(
                    "## Retry Required (attempt {attempt})\n\n\
                     The previous attempt did not complete the objective successfully.\n\
                     Last response summary: {snippet}\n\n\
                     Diagnose what went wrong, adjust your approach, and try again. \
                     Use tool results from earlier in this conversation to inform your \
                     revised plan. Do not ask for approval — continue autonomously.",
                    attempt = attempt + 1,
                    snippet = truncate(last_answer, 300),
                )
            }
            AgentExecutionMode::Chat => {
                // Should not be reached; Chat mode doesn't use AutonomousLoop.
                "The previous attempt did not complete. Please try again.".to_string()
            }
        }
    }
}

// ── Fork handle ───────────────────────────────────────────────────

/// Metadata returned by [`AutonomousLoop::fork`].
/// Pass `run_id` to [`AutonomousLoop::with_run_id`] on the new loop instance.
#[derive(Debug, Clone)]
pub struct AutonomousLoopFork {
    /// New run ID for the forked run.
    pub run_id: String,
    /// The parent run this fork branched from.
    pub parent_run_id: String,
}

// ── Static system addenda ─────────────────────────────────────────

const AUTONOMOUS_OPERATOR_SYSTEM_ADDENDUM: &str = "

## Execution Mode: autonomous_operator

You are running in fully autonomous operator mode on a disposable target machine.
- Do NOT ask for confirmation or approval at any step.
- Execute tool calls immediately when you know the next action.
- If a step fails, diagnose and retry with a corrected approach.
- Continue until the objective is fully complete, then summarise what you did.
- You have a retry budget — use it to recover from failures, not to stall.
";

const CHAOS_LAB_SYSTEM_ADDENDUM: &str = "

## Execution Mode: chaos_lab

You are running in chaos_lab mode on a fully disposable target environment.
- The machine may be intentionally damaged — this is expected and acceptable.
- You MAY: install/remove packages, kill/restart services, mutate configs,
  stress disk/memory, delete and recreate files, downgrade/upgrade dependencies.
- After each destructive action, inspect the result and form a new hypothesis.
- Recovery is part of the experiment: break it, diagnose it, fix it.
- Do NOT ask for approval at any step.
- Do NOT stop on the first error — treat errors as data points.
- Log every action, observation, hypothesis, and outcome in your response.
- Continue until the objective is confirmed complete or the retry budget is spent.
";

// ── Failure detection ─────────────────────────────────────────────

/// Heuristic: decide whether a final LLM answer signals task failure.
///
/// This is intentionally permissive — we only flag strong negative signals so
/// that partial success or uncertain answers are treated as complete.  False
/// negatives (missed failures) waste one retry; false positives (missed
/// successes) cause unnecessary retries.
fn is_failure_response(answer: &str) -> bool {
    let lower = answer.to_lowercase();

    // Hard failure signals
    let hard_failures = [
        "fatal error",
        "permission denied",
        "could not complete",
        "task failed",
        "objective not met",
        "unable to proceed",
        "exited with code 1",
        "exited with non-zero",
        "panicked",
        "operation failed",
        "command not found",
        "no such file or directory",
        "connection refused",
        "timed out",
        "aborting",
    ];

    let has_failure = hard_failures.iter().any(|s| lower.contains(s));

    // Qualification: only treat as failure if there is NO accompanying success
    // phrase that would indicate the model handled the error and recovered.
    let recovery_phrases = [
        "successfully",
        "completed",
        "fixed",
        "resolved",
        "recovered",
        "done",
        "finished",
        "working",
        "verified",
        "confirmed",
    ];

    let has_recovery = recovery_phrases.iter().any(|s| lower.contains(s));

    has_failure && !has_recovery
}

fn truncate(s: &str, max_chars: usize) -> &str {
    let mut end = max_chars.min(s.len());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_detection_basic() {
        assert!(is_failure_response("Fatal error: could not complete the task."));
        assert!(!is_failure_response("Fatal error occurred but successfully recovered."));
        assert!(!is_failure_response("All done, task completed."));
        assert!(!is_failure_response("The model returned a summary."));
    }

    #[test]
    fn failure_detection_partial_success() {
        // If the model says it recovered, it is not a failure.
        assert!(!is_failure_response(
            "Permission denied on first attempt; retried as root and it worked. Fixed."
        ));
    }

    #[test]
    fn loop_outcome_helpers() {
        let o = LoopOutcome::Completed {
            attempts: 1,
            final_answer: "ok".into(),
        };
        assert!(o.is_success());
        assert_eq!(o.last_answer(), "ok");

        let o2 = LoopOutcome::RetryBudgetExhausted {
            attempts: 5,
            last_answer: "fail".into(),
        };
        assert!(!o2.is_success());
    }

    #[test]
    fn truncate_respects_char_boundary() {
        let s = "hello world";
        assert_eq!(truncate(s, 5), "hello");
        assert_eq!(truncate(s, 100), s);
    }
}
