//! Polyglot code execution tool.
//!
//! Gives the agent a structured write → compile → run path for short
//! programs across the toolchains shipped in the bundle image (python3,
//! node, gcc/g++, go, rustc, bash) instead of hand-rolling shell pipelines.
//! Each run executes in a disposable directory under
//! `<workspace>/.code_run/` with an optional wall-clock timeout and captured
//! stdout/stderr/exit status.

use super::process_group::{self, ProcessGroupGuard};
use super::traits::{Tool, ToolResult};
use crate::security::{SecurityPolicy, policy::ToolOperation};
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

const DEFAULT_TIMEOUT_SECS: u64 = 0;
const MAX_CAPTURED_BYTES: usize = 64 * 1024;

struct LanguageSpec {
    /// Source file name inside the scratch dir.
    source: &'static str,
    /// Compile command; empty means interpreted.
    compile: &'static [&'static str],
    /// Run command; `{bin}` and `{src}` are substituted.
    run: &'static [&'static str],
    /// Binary that must exist on PATH for this language to be available.
    requires: &'static str,
}

fn language_spec(language: &str) -> Option<LanguageSpec> {
    match language {
        "python" => Some(LanguageSpec {
            source: "main.py",
            compile: &[],
            run: &["python3", "{src}"],
            requires: "python3",
        }),
        "javascript" | "node" => Some(LanguageSpec {
            source: "main.js",
            compile: &[],
            run: &["node", "{src}"],
            requires: "node",
        }),
        "typescript" => Some(LanguageSpec {
            source: "main.ts",
            compile: &[],
            run: &["node", "--experimental-strip-types", "{src}"],
            requires: "node",
        }),
        "c" => Some(LanguageSpec {
            source: "main.c",
            compile: &["gcc", "{src}", "-O2", "-o", "{bin}"],
            run: &["{bin}"],
            requires: "gcc",
        }),
        "cpp" | "c++" => Some(LanguageSpec {
            source: "main.cpp",
            compile: &["g++", "{src}", "-O2", "-std=c++20", "-o", "{bin}"],
            run: &["{bin}"],
            requires: "g++",
        }),
        "go" => Some(LanguageSpec {
            source: "main.go",
            compile: &[],
            run: &["go", "run", "{src}"],
            requires: "go",
        }),
        "rust" => Some(LanguageSpec {
            source: "main.rs",
            compile: &["rustc", "{src}", "-O", "-o", "{bin}"],
            run: &["{bin}"],
            requires: "rustc",
        }),
        "bash" | "sh" => Some(LanguageSpec {
            source: "main.sh",
            compile: &[],
            run: &["bash", "{src}"],
            requires: "bash",
        }),
        _ => None,
    }
}

pub struct CodeRunTool {
    security: Arc<SecurityPolicy>,
    scratch_root: PathBuf,
}

impl CodeRunTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: &Path) -> Self {
        Self {
            security,
            scratch_root: workspace_dir.join(".code_run"),
        }
    }
}

fn substitute(template: &[&str], src: &Path, bin: &Path) -> Vec<String> {
    template
        .iter()
        .map(|part| {
            part.replace("{src}", &src.to_string_lossy())
                .replace("{bin}", &bin.to_string_lossy())
        })
        .collect()
}

fn truncate_bytes(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    crate::util::truncate_with_ellipsis(&text, MAX_CAPTURED_BYTES)
}

async fn run_command(
    argv: &[String],
    cwd: &Path,
    stdin_data: Option<&str>,
    timeout: Option<Duration>,
) -> anyhow::Result<(bool, String, String, Option<i32>)> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    process_group::configure(&mut cmd);
    let mut child = cmd.spawn()?;
    let pid = child
        .id()
        .ok_or_else(|| anyhow::anyhow!("spawned code process did not expose a process ID"))?;
    let mut process_group = ProcessGroupGuard::new(pid);
    if let Some(data) = stdin_data {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(data.as_bytes()).await;
        }
    } else {
        drop(child.stdin.take());
    }
    let mut output = Box::pin(child.wait_with_output());
    let result = match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, output.as_mut()).await {
            Ok(result) => Some(result),
            Err(_) => None,
        },
        None => Some(output.as_mut().await),
    };
    match result {
        Some(output) => {
            let output = output?;
            process_group.disarm();
            Ok((
                output.status.success(),
                truncate_bytes(&output.stdout),
                truncate_bytes(&output.stderr),
                output.status.code(),
            ))
        }
        None => {
            let _ = process_group.terminate();
            Ok((
                false,
                String::new(),
                format!(
                    "timed out after {}s",
                    timeout.map_or(0, |duration| duration.as_secs())
                ),
                None,
            ))
        }
    }
}

#[async_trait]
impl Tool for CodeRunTool {
    fn name(&self) -> &str {
        "code_run"
    }

    fn description(&self) -> &str {
        "Write, compile, and execute a short program in a disposable \
         directory with no wall-clock deadline by default. Languages: python, javascript, \
         typescript, c, cpp, go, rust, bash. Returns stdout, stderr, and \
         exit status. Prefer this over hand-written shell pipelines for \
         running code snippets; use file_write + shell for full projects."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "language": {
                    "type": "string",
                    "enum": ["python", "javascript", "typescript", "c", "cpp", "go", "rust", "bash"],
                    "description": "Language / toolchain to use"
                },
                "code": {
                    "type": "string",
                    "description": "Complete source of the program to run"
                },
                "stdin": {
                    "type": "string",
                    "description": "Optional data piped to the program's stdin"
                },
                "timeout_secs": {
                    "type": "integer",
                    "minimum": 0,
                    "default": DEFAULT_TIMEOUT_SECS,
                    "description": "Optional compile/run deadline in seconds; 0 means unlimited"
                }
            },
            "required": ["language", "code"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        if let Err(msg) = self
            .security
            .enforce_tool_operation(ToolOperation::Act, "code_run")
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(msg),
            });
        }

        let language = args
            .get("language")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let Some(spec) = language_spec(&language) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unsupported language '{language}'. Supported: python, javascript, \
                     typescript, c, cpp, go, rust, bash"
                )),
            });
        };
        // Sibling tool `shell` takes its script under `command`, not `code` —
        // seen in practice: a model calling code_run right after a run of
        // shell calls carries that param name over by habit. Accepting it as
        // a fallback turns a hard, repeated failure into a silent recovery
        // instead of relying on the model to notice and self-correct.
        let Some(code) = args
            .get("code")
            .or_else(|| args.get("command"))
            .and_then(|v| v.as_str())
            .filter(|c| !c.trim().is_empty())
        else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Parameter 'code' is required".into()),
            });
        };
        if which::which(spec.requires).is_err() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Toolchain '{}' is not installed in this runtime",
                    spec.requires
                )),
            });
        }

        let timeout_secs = args
            .get("timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let timeout = (timeout_secs > 0).then(|| Duration::from_secs(timeout_secs));
        let stdin_data = args.get("stdin").and_then(|v| v.as_str());

        let run_dir = self
            .scratch_root
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&run_dir)?;
        let scratch_guard = ScratchDirGuard(run_dir.clone());
        let src = run_dir.join(spec.source);
        std::fs::write(&src, code)?;
        let bin = run_dir.join("program");

        // Compile step (compiled languages only).
        if !spec.compile.is_empty() {
            let argv = substitute(spec.compile, &src, &bin);
            let (ok, out, err, code_num) =
                run_command(&argv, &run_dir, None, timeout).await?;
            if !ok {
                return Ok(ToolResult {
                    success: false,
                    output: out,
                    error: Some(format!(
                        "compilation failed (exit {:?}):\n{err}",
                        code_num
                    )),
                });
            }
        }

        let argv = substitute(spec.run, &src, &bin);
        let (ok, stdout, stderr, exit_code) =
            run_command(&argv, &run_dir, stdin_data, timeout).await?;
        drop(scratch_guard);

        let mut output = format!(
            "exit: {}\n",
            exit_code.map_or_else(|| "killed".to_string(), |c| c.to_string())
        );
        if !stdout.is_empty() {
            output.push_str(&format!("stdout:\n{stdout}\n"));
        }
        if !stderr.is_empty() {
            output.push_str(&format!("stderr:\n{stderr}\n"));
        }
        Ok(ToolResult {
            success: ok,
            output,
            error: if ok { None } else { Some("program exited non-zero".into()) },
        })
    }
}

struct ScratchDirGuard(PathBuf);

impl Drop for ScratchDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tool() -> (CodeRunTool, TempDir) {
        let tmp = TempDir::new().unwrap();
        let mut policy = SecurityPolicy::default();
        policy.autonomy = crate::security::AutonomyLevel::Full;
        (CodeRunTool::new(Arc::new(policy), tmp.path()), tmp)
    }

    #[tokio::test]
    async fn python_snippet_runs_and_cleans_up() {
        if which::which("python3").is_err() {
            return;
        }
        let (tool, tmp) = tool();
        let result = tool
            .execute(json!({"language": "python", "code": "print(6*7)"}))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);
        assert!(result.output.contains("42"));
        let leftovers = std::fs::read_dir(tmp.path().join(".code_run"))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(leftovers, 0, "scratch dir must be cleaned up");
    }

    #[tokio::test]
    async fn zero_timeout_is_unlimited() {
        if which::which("bash").is_err() {
            return;
        }
        let (tool, _tmp) = tool();
        let schema = tool.parameters_schema();
        assert_eq!(schema["properties"]["timeout_secs"]["default"], 0);
        assert!(schema["properties"]["timeout_secs"]
            .get("maximum")
            .is_none());

        let result = tokio::time::timeout(
            Duration::from_secs(2),
            tool.execute(json!({
                "language": "bash",
                "code": "sleep 0.05; printf unlimited-code-run",
                "timeout_secs": 0
            })),
        )
        .await
        .expect("unlimited code run should finish")
        .unwrap();
        assert!(result.success, "{:?}", result.error);
        assert!(result.output.contains("unlimited-code-run"));
    }

    #[tokio::test]
    async fn accepts_command_as_a_fallback_for_the_code_parameter() {
        // Regression: a model that just called `shell` (which takes
        // `command`) sometimes carries that param name over into the next
        // `code_run` call instead of switching to `code`.
        if which::which("bash").is_err() {
            return;
        }
        let (tool, _tmp) = tool();
        let result = tool
            .execute(json!({"language": "bash", "command": "echo command-fallback-ok"}))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);
        assert!(result.output.contains("command-fallback-ok"));
    }

    #[tokio::test]
    async fn missing_both_code_and_command_still_errors() {
        let (tool, _tmp) = tool();
        let result = tool
            .execute(json!({"language": "python"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("Parameter 'code' is required"));
    }

    #[tokio::test]
    async fn c_snippet_compiles_and_runs() {
        if which::which("gcc").is_err() {
            return;
        }
        let (tool, _tmp) = tool();
        let result = tool
            .execute(json!({
                "language": "c",
                "code": "#include <stdio.h>\nint main(){printf(\"c-ok %d\\n\", 7*3);return 0;}"
            }))
            .await
            .unwrap();
        assert!(result.success, "{:?}", result.error);
        assert!(result.output.contains("c-ok 21"));
    }

    #[tokio::test]
    async fn node_snippet_runs_with_stdin() {
        if which::which("node").is_err() {
            return;
        }
        let (tool, _tmp) = tool();
        let result = tool
            .execute(json!({
                "language": "javascript",
                "code": "process.stdin.on('data', d => console.log('got:' + d.toString().trim()));",
                "stdin": "hello",
                "timeout_secs": 10
            }))
            .await
            .unwrap();
        assert!(result.output.contains("got:hello"), "{}", result.output);
    }

    #[tokio::test]
    async fn unsupported_language_is_rejected() {
        let (tool, _tmp) = tool();
        let result = tool
            .execute(json!({"language": "cobol", "code": "x"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn compile_error_is_reported() {
        if which::which("gcc").is_err() {
            return;
        }
        let (tool, _tmp) = tool();
        let result = tool
            .execute(json!({"language": "c", "code": "int main( { broken"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("compilation failed"));
    }
}
