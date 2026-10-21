use super::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_CAPTURED_BYTES: usize = 100_000;

/// Delegates a natural-language request to a locally-trained TerminAI planner
/// model (native C++ runtime, `.tmai` weights — not an Ollama/GGUF model) and
/// returns its structured `--decision-json` plan: target capability, risk
/// classification, and the compiled shell command it would run.
///
/// This never executes anything itself (no `--run` is passed). It is a
/// second opinion / specialist planner for terminal-command intent — review
/// the plan and, if it looks right, run the compiled command yourself via
/// the `shell` tool.
pub struct TerminAiPlanTool {
    binary_path: PathBuf,
    model_path: PathBuf,
    timeout: Duration,
}

impl TerminAiPlanTool {
    pub fn new(binary_path: PathBuf, model_path: PathBuf) -> Self {
        Self {
            binary_path,
            model_path,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
        }
    }

    /// Build from env vars with the paths verified working on this host as
    /// defaults, so swapping in a newer trained checkpoint later (a new
    /// `runs/<name>/selected/*.tmai`) needs only an env change, not a
    /// rebuild of LlamaFarm itself.
    pub fn from_env() -> Self {
        let binary_path = std::env::var("TERMINAI_BINARY_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/home/bat/terminai-ml/bin/terminai"));
        let model_path = std::env::var("TERMINAI_MODEL_PATH").map(PathBuf::from).unwrap_or_else(|_| {
            PathBuf::from(
                "/home/bat/terminai-ml/runs/model-only-90m-v5/selected/selected.q8-row.tmai",
            )
        });
        Self::new(binary_path, model_path)
    }

    /// True if both the compiled planner binary and a trained checkpoint are present —
    /// used to register this tool only when the local TerminAI setup actually exists.
    pub fn is_available(&self) -> bool {
        self.binary_path.is_file() && self.model_path.is_file()
    }

    fn truncate(bytes: &[u8]) -> String {
        let text = String::from_utf8_lossy(bytes);
        if text.len() <= MAX_CAPTURED_BYTES {
            text.into_owned()
        } else {
            format!("{}…[truncated]", &text[..MAX_CAPTURED_BYTES])
        }
    }
}

#[async_trait]
impl Tool for TerminAiPlanTool {
    fn name(&self) -> &str {
        "terminai_plan"
    }

    fn description(&self) -> &str {
        "Ask the locally-trained TerminAI planner model to translate a natural-language \
         request into a terminal command plan. Returns structured JSON: the target \
         capability, a risk classification (read_only / mutating / destructive), whether \
         confirmation is required, and the compiled shell command — it does NOT execute \
         anything. Useful as a specialist second opinion for 'how would I do X from the \
         terminal' questions; review the plan, then run the compiled command yourself via \
         the shell tool if it looks right."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "request": {
                    "type": "string",
                    "description": "Natural-language description of the terminal task, e.g. 'find the 10 largest files under /var/log'"
                }
            },
            "required": ["request"],
            "additionalProperties": false
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let request = match args.get("request").and_then(|v| v.as_str()) {
            Some(r) if !r.trim().is_empty() => r,
            _ => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing or empty 'request' parameter".into()),
                });
            }
        };

        if !self.binary_path.is_file() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "TerminAI binary not found at {} — set TERMINAI_BINARY_PATH or build it from the terminai-ml source tree",
                    self.binary_path.display()
                )),
            });
        }
        if !self.model_path.is_file() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "TerminAI model not found at {} — set TERMINAI_MODEL_PATH to a trained .tmai checkpoint",
                    self.model_path.display()
                )),
            });
        }

        let mut cmd = tokio::process::Command::new(&self.binary_path);
        cmd.arg("--model")
            .arg(&self.model_path)
            .arg("--decision-json")
            .arg(request)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let child = cmd.spawn()?;
        let output = match tokio::time::timeout(self.timeout, child.wait_with_output()).await {
            Ok(result) => result?,
            Err(_) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("terminai timed out after {}s", self.timeout.as_secs())),
                });
            }
        };

        let stdout = Self::truncate(&output.stdout);
        let stderr = Self::truncate(&output.stderr);

        if !output.status.success() {
            return Ok(ToolResult {
                success: false,
                output: stdout,
                error: Some(if stderr.is_empty() {
                    format!("terminai exited with status {:?}", output.status.code())
                } else {
                    stderr
                }),
            });
        }

        // The planner prints one-line native-backend timing probes to stdout before the
        // JSON payload on first load; keep only the JSON line for a clean tool result.
        let plan_line = stdout
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('{'))
            .unwrap_or(stdout.trim());

        let parsed = serde_json::from_str::<serde_json::Value>(plan_line).ok();
        let pretty = parsed
            .as_ref()
            .and_then(|v| serde_json::to_string_pretty(v).ok())
            .unwrap_or_else(|| plan_line.to_string());

        // This 90M-parameter model is reliable at risk/intent classification but weak at
        // composing correct arguments for anything beyond a single simple criterion —
        // confirmed empirically: it has produced `rm -rf --` with the path silently
        // dropped, and bare `grep` with no pattern, both syntactically valid JSON that
        // *looks* complete. Catch the unambiguous case (an empty-string argv element,
        // meaning a parameter the model knew it needed but never filled in) so the
        // caller doesn't mistake an incomplete compiled command for a validated one.
        let output = if parsed.as_ref().is_some_and(plan_has_empty_argv) {
            format!(
                "⚠ This compiled command has a missing argument (an empty string in argv) \
                 — it is very likely incomplete or wrong. Do not run it as-is; re-derive the \
                 command yourself from plan.request and plan.steps, or ask the user for the \
                 missing detail.\n\n{pretty}"
            )
        } else {
            pretty
        };

        Ok(ToolResult {
            success: true,
            output,
            error: None,
        })
    }
}

/// True if any compiled pipeline step has an empty-string element in its argv — the
/// clearest, lowest-false-positive signal that this planner silently dropped a parameter
/// it knew it needed (e.g. `rm -rf --` with the target path missing).
fn plan_has_empty_argv(value: &serde_json::Value) -> bool {
    let Some(pipelines) = value["compiled"]["pipelines"].as_array() else {
        return false;
    };
    pipelines.iter().any(|pipeline| {
        pipeline.as_array().is_some_and(|steps| {
            steps.iter().any(|step| {
                step["argv"]
                    .as_array()
                    .is_some_and(|argv| argv.iter().any(|a| a.as_str() == Some("")))
            })
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_empty_argv_element() {
        // The real, observed case: "delete files in /tmp older than 7 days" compiled to
        // `rm -rf --` with the path silently dropped.
        let broken = json!({
            "compiled": {"pipelines": [[{"executable": "rm", "argv": ["-rf", "--", ""]}]]}
        });
        assert!(plan_has_empty_argv(&broken));
    }

    #[test]
    fn complete_plan_is_not_flagged() {
        let complete = json!({
            "compiled": {"pipelines": [[{"executable": "find", "argv": [".", "-type", "f"]}]]}
        });
        assert!(!plan_has_empty_argv(&complete));
    }

    #[test]
    fn empty_pipelines_is_not_flagged() {
        // meta.clarify legitimately has no pipeline at all — that's a correct "ask the
        // user" outcome, not an incomplete command, so it must not trip this check.
        let clarify = json!({"compiled": {"pipelines": [[]]}});
        assert!(!plan_has_empty_argv(&clarify));
    }

    #[test]
    fn tool_name_and_schema() {
        let tool = TerminAiPlanTool::new(PathBuf::from("/bin/true"), PathBuf::from("/dev/null"));
        assert_eq!(tool.name(), "terminai_plan");
        assert!(tool.is_read_only());
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["request"].is_object());
        let required = schema["required"].as_array().unwrap();
        assert!(required.contains(&serde_json::Value::String("request".into())));
    }

    #[tokio::test]
    async fn missing_request_returns_error() {
        let tool = TerminAiPlanTool::new(PathBuf::from("/bin/true"), PathBuf::from("/dev/null"));
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("request"));
    }

    #[tokio::test]
    async fn missing_binary_reports_clear_error() {
        let tool = TerminAiPlanTool::new(
            PathBuf::from("/nonexistent/terminai"),
            PathBuf::from("/nonexistent/model.tmai"),
        );
        let result = tool
            .execute(json!({"request": "list files"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("binary not found"));
    }

    #[tokio::test]
    async fn missing_model_reports_clear_error() {
        let tool = TerminAiPlanTool::new(PathBuf::from("/bin/true"), PathBuf::from("/nonexistent/model.tmai"));
        let result = tool
            .execute(json!({"request": "list files"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.unwrap().contains("model not found"));
    }
}
