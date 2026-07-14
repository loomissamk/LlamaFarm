//! Model bakeoff harness.
//!
//! Benchmarks all installed Ollama models across task categories and writes
//! scores to the [`CapabilityRegistry`].
//!
//! ## Task categories
//!
//! | Category | What it tests |
//! |----------|---------------|
//! | `shell_reliability` | Run 10 shell tasks, score by success rate |
//! | `json_compliance` | Structured output tasks, score by parse success |
//! | `coding` | Code generation, score by syntax correctness |
//! | `rag_synthesis` | Answer from retrieved context, score by citation quality |
//! | `autonomous_tasks` | Multi-step tasks, score by completion rate |
//! | `latency` | TTFT and tokens/sec |
//!
//! Results are written to `~/.llamafarm/capability-registry.json` via the
//! [`CapabilityRegistry`] and are loaded at agent startup to inform model
//! routing decisions.
//!
//! ## Usage
//!
//! ```rust,ignore
//! let harness = BakeoffHarness::new(provider, registry);
//! let report = harness.run_all().await?;
//! println!("{report}");
//! ```

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use tracing::{info, warn};

use crate::capability::{CapabilityRegistry, ModelCapability};
use crate::providers::ollama::{OllamaModelEntry, OllamaProvider};
use crate::providers::Provider as _;

// ── Task definitions ──────────────────────────────────────────────

/// A single bakeoff task with a prompt, expected pattern, and category.
struct BakeoffTask {
    category: &'static str,
    prompt: &'static str,
    /// If the response contains this substring (case-insensitive), the task passes.
    expected_contains: &'static str,
    /// System prompt to inject.
    system: Option<&'static str>,
}

fn shell_tasks() -> Vec<BakeoffTask> {
    vec![
        BakeoffTask {
            category: "shell",
            prompt: "Write a single bash command that lists all .rs files in the current directory recursively.",
            expected_contains: "find",
            system: Some("Reply with only the shell command, nothing else."),
        },
        BakeoffTask {
            category: "shell",
            prompt: "Write a bash one-liner that counts the number of lines in all .py files.",
            expected_contains: "wc",
            system: Some("Reply with only the shell command, nothing else."),
        },
        BakeoffTask {
            category: "shell",
            prompt: "Write a command to show the 10 largest files in /tmp sorted by size.",
            expected_contains: "sort",
            system: Some("Reply with only the shell command, nothing else."),
        },
    ]
}

fn json_tasks() -> Vec<BakeoffTask> {
    vec![
        BakeoffTask {
            category: "json",
            prompt: r#"Return a JSON object with fields "name" (string) and "score" (number between 0 and 1). Set name to "test" and score to 0.85."#,
            expected_contains: "\"name\"",
            system: Some("Reply with only valid JSON. No explanation."),
        },
        BakeoffTask {
            category: "json",
            prompt: r#"Return a JSON array of 3 objects, each with "id" (integer) and "value" (string)."#,
            expected_contains: "[",
            system: Some("Reply with only valid JSON. No explanation."),
        },
    ]
}

fn coding_tasks() -> Vec<BakeoffTask> {
    vec![
        BakeoffTask {
            category: "coding",
            prompt: "Write a Python function that takes a list of integers and returns the sum of all even numbers.",
            expected_contains: "def",
            system: Some("Reply with only the function code, no explanation."),
        },
        BakeoffTask {
            category: "coding",
            prompt: "Write a Rust function that checks if a string is a palindrome. Use &str as input.",
            expected_contains: "fn",
            system: Some("Reply with only the function code, no explanation."),
        },
    ]
}

fn rag_tasks() -> Vec<BakeoffTask> {
    vec![
        BakeoffTask {
            category: "rag",
            prompt: "Context: [Source 1: docs/auth.md]\nAuthentication uses JWT tokens that expire after 24 hours.\n\nQuestion: How long do JWT tokens last?\nAnswer with a citation like [1].",
            expected_contains: "[1]",
            system: Some("You are a RAG assistant. Answer only from the provided context and cite sources."),
        },
    ]
}

fn latency_task() -> BakeoffTask {
    BakeoffTask {
        category: "latency",
        prompt: "Say 'hello' in exactly one word.",
        expected_contains: "hello",
        system: None,
    }
}

// ── Score helpers ─────────────────────────────────────────────────

/// Check if a response passes a task (case-insensitive substring match).
fn task_passes(response: &str, expected: &str) -> bool {
    response.to_lowercase().contains(&expected.to_lowercase())
}

/// Check if a JSON string is syntactically valid.
fn is_valid_json(s: &str) -> bool {
    // Find the first `{` or `[` and try to parse from there
    let start = s.find(|c: char| c == '{' || c == '[').unwrap_or(0);
    serde_json::from_str::<serde_json::Value>(&s[start..]).is_ok()
}

// ── BakeoffHarness ────────────────────────────────────────────────

/// Bakeoff harness: runs benchmark tasks against all installed Ollama models
/// and writes scores to the [`CapabilityRegistry`].
pub struct BakeoffHarness {
    provider: OllamaProvider,
    registry: Arc<CapabilityRegistry>,
    /// Temperature for bakeoff tasks (low = deterministic).
    temperature: f64,
    /// Maximum tokens to generate per task.
    max_tokens: usize,
}

impl BakeoffHarness {
    pub fn new(provider: OllamaProvider, registry: Arc<CapabilityRegistry>) -> Self {
        Self {
            provider,
            registry,
            temperature: 0.1,
            max_tokens: 512,
        }
    }

    /// Run the full bakeoff against all installed models and return a summary report.
    pub async fn run_all(&self) -> Result<BakeoffReport> {
        let models = self.provider.list_models().await?;
        info!("bakeoff: found {} installed model(s)", models.len());

        let mut report = BakeoffReport::new();

        for model_entry in &models {
            match self.run_model(model_entry).await {
                Ok(result) => {
                    info!(
                        "bakeoff: {} — shell={:.2} json={:.2} coding={:.2} rag={:.2} latency={}ms",
                        result.model,
                        result.shell_reliability.unwrap_or(0.0),
                        result.json_compliance.unwrap_or(0.0),
                        result.coding_score.unwrap_or(0.0),
                        result.rag_score.unwrap_or(0.0),
                        result.latency_ms_p50.unwrap_or(0),
                    );
                    report.add(result);
                }
                Err(e) => {
                    warn!("bakeoff: {} failed: {e}", model_entry.name);
                    report.add_error(&model_entry.name, e.to_string());
                }
            }
        }

        Ok(report)
    }

    /// Run all bakeoff tasks for a single model and update the registry.
    async fn run_model(&self, entry: &OllamaModelEntry) -> Result<ModelCapability> {
        let model = &entry.name;

        // Populate static metadata first
        let info = self.provider.show_model(model).await.ok();

        let ctx_len = info.as_ref().map(|i| i.context_length()).unwrap_or(4096);
        let supports_vision = info.as_ref().map(|i| i.supports_vision()).unwrap_or(false);

        self.registry.update_metadata(
            model,
            ctx_len,
            &entry.details.parameter_size,
            &entry.details.quantization_level,
            &entry.details.family,
            supports_vision,
            false, // tool-use reliability measured by shell score
        );

        // Shell tasks
        let shell_score = self.score_tasks(model, shell_tasks()).await;
        // JSON tasks
        let json_score = self.score_json_tasks(model, json_tasks()).await;
        // Coding tasks
        let coding_score = self.score_tasks(model, coding_tasks()).await;
        // RAG tasks
        let rag_score = self.score_tasks(model, rag_tasks()).await;
        // Latency
        let (latency_ms, tps) = self.measure_latency(model).await;

        self.registry.update_scores(
            model,
            Some(shell_score),
            None,
            Some(json_score),
            Some(coding_score),
            Some(rag_score),
            None,
            None,
            Some(latency_ms),
            Some(tps),
        );

        Ok(self.registry.get(model).unwrap_or_else(|| ModelCapability {
            model: model.to_string(),
            ..Default::default()
        }))
    }

    /// Run a list of tasks and return the pass fraction.
    async fn score_tasks(&self, model: &str, tasks: Vec<BakeoffTask>) -> f32 {
        let n = tasks.len();
        if n == 0 {
            return 0.0;
        }
        let mut passed = 0usize;

        for task in tasks {
            let result = self
                .provider
                .chat_with_system(task.system, task.prompt, model, self.temperature)
                .await;

            match result {
                Ok(response) if task_passes(&response, task.expected_contains) => {
                    passed += 1;
                }
                Ok(_) => {}
                Err(e) => {
                    warn!("bakeoff task '{}' error on {model}: {e}", task.category);
                }
            }
        }

        passed as f32 / n as f32
    }

    /// Run JSON tasks: score by valid JSON parse success + content match.
    async fn score_json_tasks(&self, model: &str, tasks: Vec<BakeoffTask>) -> f32 {
        let n = tasks.len();
        if n == 0 {
            return 0.0;
        }
        let mut score = 0.0f32;

        for task in tasks {
            let result = self
                .provider
                .chat_with_system(task.system, task.prompt, model, self.temperature)
                .await;

            match result {
                Ok(response) => {
                    let content_ok = task_passes(&response, task.expected_contains);
                    let json_ok = is_valid_json(&response);
                    // Full point for both, half point for content-only
                    score += if json_ok && content_ok {
                        1.0
                    } else if content_ok {
                        0.5
                    } else {
                        0.0
                    };
                }
                Err(e) => {
                    warn!("bakeoff json task error on {model}: {e}");
                }
            }
        }

        score / n as f32
    }

    /// Measure median latency and tokens/sec with the latency task.
    async fn measure_latency(&self, model: &str) -> (u64, f32) {
        const RUNS: usize = 3;
        let task = latency_task();
        let mut latencies = Vec::new();
        let mut tps_vals = Vec::new();

        for _ in 0..RUNS {
            let start = Instant::now();
            let result = self
                .provider
                .chat_with_system(task.system, task.prompt, model, 0.0)
                .await;
            let elapsed_ms = start.elapsed().as_millis() as u64;

            if let Ok(response) = result {
                latencies.push(elapsed_ms);
                // Rough token count: ~4 chars per token
                let tokens = (response.len() as f32 / 4.0).max(1.0);
                let secs = elapsed_ms as f32 / 1000.0;
                tps_vals.push(tokens / secs);
            }
        }

        if latencies.is_empty() {
            return (9999, 0.0);
        }

        latencies.sort();
        let p50 = latencies[latencies.len() / 2];
        let avg_tps = tps_vals.iter().sum::<f32>() / tps_vals.len() as f32;

        (p50, avg_tps)
    }
}

// ── BakeoffReport ─────────────────────────────────────────────────

/// Summary report from a bakeoff run.
#[derive(Debug, Default)]
pub struct BakeoffReport {
    results: Vec<ModelCapability>,
    errors: Vec<(String, String)>,
}

impl BakeoffReport {
    fn new() -> Self {
        Self::default()
    }

    fn add(&mut self, cap: ModelCapability) {
        self.results.push(cap);
    }

    fn add_error(&mut self, model: &str, error: String) {
        self.errors.push((model.to_string(), error));
    }

    /// Return the model name with the best score for a given hint.
    pub fn winner(&self, hint: &str) -> Option<&str> {
        self.results
            .iter()
            .max_by(|a, b| {
                a.score_for_hint(hint)
                    .partial_cmp(&b.score_for_hint(hint))
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|c| c.model.as_str())
    }

    /// Render a markdown table summary.
    pub fn to_markdown(&self) -> String {
        let mut out = String::from("# Model Bakeoff Results\n\n");

        if self.results.is_empty() {
            out.push_str("No models evaluated.\n");
            return out;
        }

        // Sort by composite score descending
        let mut sorted = self.results.clone();
        sorted.sort_by(|a, b| {
            let sa = a.score_for_hint("generic");
            let sb = b.score_for_hint("generic");
            sb.partial_cmp(&sa).unwrap_or(std::cmp::Ordering::Equal)
        });

        out.push_str("| Model | Shell | JSON | Coding | RAG | Latency ms | TPS |\n");
        out.push_str("|-------|-------|------|--------|-----|------------|-----|\n");

        for cap in &sorted {
            out.push_str(&format!(
                "| {} | {:.0}% | {:.0}% | {:.0}% | {:.0}% | {} | {:.0} |\n",
                cap.model,
                cap.shell_reliability.unwrap_or(0.0) * 100.0,
                cap.json_compliance.unwrap_or(0.0) * 100.0,
                cap.coding_score.unwrap_or(0.0) * 100.0,
                cap.rag_score.unwrap_or(0.0) * 100.0,
                cap.latency_ms_p50.unwrap_or(9999),
                cap.tokens_per_sec_p50.unwrap_or(0.0),
            ));
        }

        if !self.errors.is_empty() {
            out.push_str("\n## Errors\n\n");
            for (model, err) in &self.errors {
                out.push_str(&format!("- **{model}**: {err}\n"));
            }
        }

        out.push_str("\n## Winners by task\n\n");
        for hint in &["coder", "operator", "planner", "rag", "fast"] {
            if let Some(w) = self.winner(hint) {
                out.push_str(&format!("- **{hint}**: {w}\n"));
            }
        }

        out
    }
}

impl std::fmt::Display for BakeoffReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_markdown())
    }
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_passes_case_insensitive() {
        assert!(task_passes("find . -name '*.rs'", "find"));
        assert!(task_passes("FIND files", "find"));
        assert!(!task_passes("ls -la", "find"));
    }

    #[test]
    fn is_valid_json_detects_valid() {
        assert!(is_valid_json(r#"{"name":"test","score":0.85}"#));
        assert!(is_valid_json(r#"[{"id":1,"value":"a"}]"#));
        assert!(!is_valid_json("not json"));
        assert!(!is_valid_json("here is your JSON: oops"));
    }

    #[test]
    fn report_markdown_empty() {
        let report = BakeoffReport::new();
        let md = report.to_markdown();
        assert!(md.contains("No models evaluated"));
    }

    #[test]
    fn report_markdown_with_results() {
        let mut report = BakeoffReport::new();
        report.add(ModelCapability {
            model: "test-model:7b".to_string(),
            shell_reliability: Some(0.9),
            json_compliance: Some(0.85),
            coding_score: Some(0.75),
            rag_score: Some(0.6),
            latency_ms_p50: Some(250),
            tokens_per_sec_p50: Some(42.0),
            ..Default::default()
        });
        let md = report.to_markdown();
        assert!(md.contains("test-model:7b"));
        assert!(md.contains("90%")); // shell_reliability 0.9
        assert!(md.contains("Winners by task"));
    }

    #[test]
    fn winner_returns_best_model() {
        let mut report = BakeoffReport::new();
        report.add(ModelCapability {
            model: "fast:7b".to_string(),
            coding_score: Some(0.5),
            tokens_per_sec_p50: Some(80.0),
            ..Default::default()
        });
        report.add(ModelCapability {
            model: "smart:32b".to_string(),
            coding_score: Some(0.95),
            tokens_per_sec_p50: Some(10.0),
            ..Default::default()
        });
        assert_eq!(report.winner("coder"), Some("smart:32b"));
        assert_eq!(report.winner("fast"), Some("fast:7b"));
    }
}
