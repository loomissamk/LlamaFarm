//! Repo patch / build / test / retry workflow agent.
//!
//! Drives the autonomous loop through a structured coding cycle:
//!
//! ```text
//! 1. Explore — read relevant files, understand the codebase
//! 2. Plan    — produce a structured list of file edits
//! 3. Patch   — apply edits via file_write / file_edit tools
//! 4. Build   — run the configured build/test command via shell
//! 5. Verify  — inspect build output; declare done or diagnose failures
//! 6. Retry   — revise patches and re-run until complete or an explicit
//!              positive `retry_budget` is exhausted
//! ```
//!
//! ## Usage
//!
//! ```rust,ignore
//! let agent = RepoWorkflowAgent::new(
//!     "Add error handling to the HTTP client",
//!     "/workspace/myrepo",
//!     "cargo test --lib 2>&1",
//!     provider,
//!     tools,
//!     observer,
//! );
//! let outcome = agent.run("ollama", "qwen3-coder:30b", 0.2).await?;
//! println!("{}", outcome.summary());
//! ```

use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::config::{AgentExecutionMode, MultimodalConfig};
use crate::observability::Observer;
use crate::providers::{ChatMessage, Provider};
use crate::tools::Tool;

use super::autonomous::{AutonomousLoop, LoopOutcome};

/// Default retry budget. Zero means unlimited.
const DEFAULT_RETRY_BUDGET: u32 = 0;

/// Default wall-clock cap for a repo workflow run.
///
/// Zero means unlimited; callers can still opt into a finite cap explicitly.
const DEFAULT_WALL_CLOCK_CAP_SECS: u64 = 0;

/// Default max tool iterations per attempt. Zero follows the main runtime
/// contract and runs until completion, a real stall/error, or cancellation.
const DEFAULT_MAX_TOOL_ITERATIONS: usize = 0;

/// Result of a [`RepoWorkflowAgent`] run.
#[derive(Debug)]
pub struct RepoWorkflowOutcome {
    /// Underlying loop outcome (Completed / RetryBudgetExhausted / …).
    pub loop_outcome: LoopOutcome,
    /// Number of patch→build cycles executed.
    pub attempts: u32,
    /// Final answer / status message from the model.
    pub final_message: String,
}

impl RepoWorkflowOutcome {
    /// True if the build/test cycle completed successfully.
    pub fn is_success(&self) -> bool {
        self.loop_outcome.is_success()
    }

    /// Human-readable one-liner summary.
    pub fn summary(&self) -> String {
        if self.is_success() {
            format!(
                "Repo workflow succeeded after {} attempt(s): {}",
                self.attempts,
                truncate(&self.final_message, 120)
            )
        } else {
            format!(
                "Repo workflow failed after {} attempt(s): {}",
                self.attempts,
                truncate(&self.final_message, 120)
            )
        }
    }
}

fn truncate(s: &str, max: usize) -> &str {
    let end = s.char_indices().nth(max).map(|(i, _)| i).unwrap_or(s.len());
    &s[..end]
}

/// High-level repo patch/test/retry agent.
///
/// Wraps [`AutonomousLoop`] with a specialised system prompt that drives the
/// model through the explore → plan → patch → build → verify cycle.
pub struct RepoWorkflowAgent<'a> {
    task: String,
    workspace: PathBuf,
    build_cmd: String,
    retry_budget: u32,
    wall_clock_cap_secs: u64,
    max_tool_iterations: usize,
    provider: &'a dyn Provider,
    tools_registry: &'a [Box<dyn Tool>],
    observer: &'a dyn Observer,
    multimodal_config: &'a MultimodalConfig,
    channel_name: &'a str,
    excluded_tools: &'a [String],
}

impl<'a> RepoWorkflowAgent<'a> {
    /// Create a workflow agent.
    ///
    /// - `task`: natural-language description of what should be changed / fixed.
    /// - `workspace`: the repo root (used for scoping and per-run traces).
    /// - `build_cmd`: shell command run after each patch round (e.g. `"cargo test 2>&1"`).
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        task: impl Into<String>,
        workspace: &Path,
        build_cmd: impl Into<String>,
        provider: &'a dyn Provider,
        tools_registry: &'a [Box<dyn Tool>],
        observer: &'a dyn Observer,
        multimodal_config: &'a MultimodalConfig,
    ) -> Self {
        Self {
            task: task.into(),
            workspace: workspace.to_path_buf(),
            build_cmd: build_cmd.into(),
            retry_budget: DEFAULT_RETRY_BUDGET,
            wall_clock_cap_secs: DEFAULT_WALL_CLOCK_CAP_SECS,
            max_tool_iterations: DEFAULT_MAX_TOOL_ITERATIONS,
            provider,
            tools_registry,
            observer,
            multimodal_config,
            channel_name: "repo_workflow",
            excluded_tools: &[],
        }
    }

    /// Override the retry budget (`0` means unlimited).
    pub fn with_retry_budget(mut self, budget: u32) -> Self {
        self.retry_budget = budget;
        self
    }

    /// Override the wall-clock cap in seconds (`0` means unlimited).
    pub fn with_wall_clock_cap(mut self, secs: u64) -> Self {
        self.wall_clock_cap_secs = secs;
        self
    }

    /// Override max tool iterations per attempt (`0` means unlimited).
    pub fn with_max_tool_iterations(mut self, n: usize) -> Self {
        self.max_tool_iterations = n;
        self
    }

    /// Run the workflow.
    ///
    /// Returns [`RepoWorkflowOutcome`] when the loop terminates (success,
    /// budget exhausted, or wall-clock cap).
    pub async fn run(
        &self,
        provider_name: &str,
        model: &str,
        temperature: f64,
    ) -> Result<RepoWorkflowOutcome> {
        let static_mc = MultimodalConfig::default();
        let mc = if std::any::TypeId::of::<MultimodalConfig>()
            == std::any::TypeId::of::<MultimodalConfig>()
        {
            self.multimodal_config
        } else {
            &static_mc
        };

        let empty_excluded: Vec<String> = Vec::new();
        let excluded = if self.excluded_tools.is_empty() {
            &empty_excluded
        } else {
            &empty_excluded // use provided list via type coercion below
        };
        let _ = excluded;

        let agent = AutonomousLoop::new(
            AgentExecutionMode::Operator,
            self.retry_budget,
            self.wall_clock_cap_secs,
            self.provider,
            self.tools_registry,
            self.observer,
            provider_name,
            model,
            temperature,
            self.channel_name,
            mc,
            self.max_tool_iterations,
            None,
            self.excluded_tools,
        )
        .with_workspace(&self.workspace);

        let system_prompt = self.build_system_prompt();
        let task_message = self.build_task_message();

        let mut history = vec![
            ChatMessage::system(system_prompt),
            ChatMessage::user(task_message),
        ];

        let loop_outcome = agent.run(&mut history).await?;

        let final_message = loop_outcome.last_answer().to_string();
        let attempts = match &loop_outcome {
            LoopOutcome::Completed { attempts, .. } => *attempts,
            LoopOutcome::RetryBudgetExhausted { attempts, .. } => *attempts,
            LoopOutcome::WallClockCapReached { .. } => self.retry_budget,
            LoopOutcome::Cancelled => 0,
        };

        Ok(RepoWorkflowOutcome {
            loop_outcome,
            attempts,
            final_message,
        })
    }

    fn build_system_prompt(&self) -> String {
        format!(
            r#"You are a precise code-patch agent operating on a local repository.
Your workflow is strictly:

1. **Explore** — read relevant source files and understand the codebase.
2. **Plan** — list every file that needs to change and what must change in each.
3. **Patch** — apply all edits using file_write or file_edit tools.
4. **Build** — run the build/test command: `{build_cmd}`
5. **Verify** — read the build output:
   - If it PASSES: declare success with a concise summary of what you changed.
   - If it FAILS: diagnose the error, revise your patches, and repeat from step 3.

Rules:
- Never skip the build step — always run `{build_cmd}` after patching.
- If a build fails, include the exact compiler/test error in your reasoning.
- Do not ask for clarification — make a decision and proceed.
- Workspace root: `{workspace}`
- When done, output one line starting with "DONE:" followed by a brief summary."#,
            build_cmd = self.build_cmd,
            workspace = self.workspace.display(),
        )
    }

    fn build_task_message(&self) -> String {
        format!(
            "Task: {}\n\nBegin by exploring the repository structure, then plan and apply your patches.",
            self.task
        )
    }
}
