//! Disposable git worktree tool.
//!
//! Enables the isolated, adopt-or-discard coding workflow: the agent creates
//! a scratch worktree on a fresh branch under `<workspace>/.worktrees/`,
//! makes and tests changes there without touching the operator's checkout,
//! and then the changes are either adopted (branch merged) or discarded
//! (worktree removed) atomically. Pairs with the run ledger so a coding run's
//! artifacts live in one throwaway location.

use super::traits::{Tool, ToolResult};
use crate::security::{SecurityPolicy, policy::ToolOperation};
use async_trait::async_trait;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

pub struct GitWorktreeTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: PathBuf,
    /// Node config dir holding the brokered GitHub token, if connected.
    config_dir: Option<PathBuf>,
}

impl GitWorktreeTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
            config_dir: None,
        }
    }

    /// Provide the config dir so an adopted worktree's push to github.com can
    /// use the token connected on the Settings page.
    pub fn with_config_dir(mut self, config_dir: PathBuf) -> Self {
        self.config_dir = Some(config_dir);
        self
    }

    fn worktrees_root(&self) -> PathBuf {
        self.workspace_dir.join(".worktrees")
    }

    async fn run_git(&self, args: &[&str]) -> Result<String, String> {
        // Apply the brokered GitHub token via url.insteadOf so any github.com
        // remote operation (e.g. a push after adopt) authenticates. The token
        // is scrubbed from error text so it never surfaces in tool output.
        let token = self
            .config_dir
            .as_deref()
            .and_then(crate::auth::github_device::brokered_token);
        let mut full: Vec<String> = Vec::new();
        if let Some(ref t) = token {
            full.push("-c".into());
            full.push(format!(
                "url.https://x-access-token:{t}@github.com/.insteadOf=https://github.com/"
            ));
        }
        full.extend(args.iter().map(|s| s.to_string()));

        let output = tokio::process::Command::new("git")
            .args(&full)
            .current_dir(&self.workspace_dir)
            .output()
            .await
            .map_err(|e| format!("failed to spawn git: {e}"))?;
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            let mut stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if let Some(ref t) = token {
                stderr = stderr.replace(t.as_str(), "***");
            }
            Err(stderr)
        }
    }

    /// Only allow simple, filesystem-safe worktree/branch names.
    fn sanitize_name(raw: &str) -> Option<String> {
        let name = raw.trim();
        if name.is_empty() || name.len() > 64 {
            return None;
        }
        if name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '/')
            && !name.starts_with('/')
            && !name.contains("..")
        {
            Some(name.to_string())
        } else {
            None
        }
    }
}

#[async_trait]
impl Tool for GitWorktreeTool {
    fn name(&self) -> &str {
        "git_worktree"
    }

    fn description(&self) -> &str {
        "Manage disposable git worktrees for isolated coding runs under \
         <workspace>/.worktrees/. Actions: 'create' {name, base?} makes a new \
         worktree on a fresh branch (adopt-or-discard scratch space); 'list' \
         shows worktrees; 'adopt' {name} merges the worktree's branch back \
         into the current branch and removes the worktree; 'discard' {name} \
         removes the worktree and deletes its branch without merging. Use for \
         risky refactors so the operator's checkout is never left broken."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "list", "adopt", "discard"],
                    "description": "Worktree operation"
                },
                "name": {
                    "type": "string",
                    "description": "Worktree/branch name (create, adopt, discard)"
                },
                "base": {
                    "type": "string",
                    "description": "Base ref for create (default: current HEAD)"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("list");

        // Mutating actions require Act permission; list is read-only.
        if action != "list" {
            if let Err(msg) = self
                .security
                .enforce_tool_operation(ToolOperation::Act, "git_worktree")
            {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(msg),
                });
            }
        }

        // Must be inside a git repo.
        if self.run_git(&["rev-parse", "--git-dir"]).await.is_err() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("workspace is not a git repository".into()),
            });
        }

        match action {
            "list" => match self.run_git(&["worktree", "list", "--porcelain"]).await {
                Ok(out) => Ok(ToolResult {
                    success: true,
                    output: if out.trim().is_empty() {
                        "No worktrees.".into()
                    } else {
                        out
                    },
                    error: None,
                }),
                Err(e) => Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(e),
                }),
            },
            "create" => {
                let Some(name) = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .and_then(Self::sanitize_name)
                else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("valid 'name' is required (alphanumeric/-/_/)".into()),
                    });
                };
                let base = args
                    .get("base")
                    .and_then(|v| v.as_str())
                    .and_then(Self::sanitize_name)
                    .unwrap_or_else(|| "HEAD".to_string());
                let path = self.worktrees_root().join(&name);
                if let Err(e) = std::fs::create_dir_all(self.worktrees_root()) {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("cannot create worktrees dir: {e}")),
                    });
                }
                let branch = format!("worktree/{name}");
                let path_str = path.to_string_lossy().to_string();
                match self
                    .run_git(&["worktree", "add", "-b", &branch, &path_str, &base])
                    .await
                {
                    Ok(_) => Ok(ToolResult {
                        success: true,
                        output: format!(
                            "Created worktree '{name}' on branch '{branch}' at {path_str} \
                             (base {base}). Make changes there, then adopt or discard."
                        ),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e),
                    }),
                }
            }
            "adopt" => {
                let Some(name) = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .and_then(Self::sanitize_name)
                else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("valid 'name' is required".into()),
                    });
                };
                let branch = format!("worktree/{name}");
                let path = self.worktrees_root().join(&name);
                let path_str = path.to_string_lossy().to_string();
                // Merge the branch into the current checkout, then clean up.
                if let Err(e) = self.run_git(&["merge", "--no-edit", &branch]).await {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!("merge failed (worktree kept for inspection): {e}")),
                    });
                }
                let _ = self.run_git(&["worktree", "remove", "--force", &path_str]).await;
                let _ = self.run_git(&["branch", "-D", &branch]).await;
                Ok(ToolResult {
                    success: true,
                    output: format!("Adopted '{name}': merged '{branch}' and removed the worktree."),
                    error: None,
                })
            }
            "discard" => {
                let Some(name) = args
                    .get("name")
                    .and_then(|v| v.as_str())
                    .and_then(Self::sanitize_name)
                else {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("valid 'name' is required".into()),
                    });
                };
                let branch = format!("worktree/{name}");
                let path = self.worktrees_root().join(&name);
                let path_str = path.to_string_lossy().to_string();
                let removed = self
                    .run_git(&["worktree", "remove", "--force", &path_str])
                    .await;
                let _ = self.run_git(&["branch", "-D", &branch]).await;
                match removed {
                    Ok(_) => Ok(ToolResult {
                        success: true,
                        output: format!("Discarded '{name}': removed worktree and branch."),
                        error: None,
                    }),
                    Err(e) => Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e),
                    }),
                }
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Valid: create, list, adopt, discard"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;
    use tempfile::TempDir;

    async fn init_repo() -> (GitWorktreeTool, TempDir) {
        let tmp = TempDir::new().unwrap();
        let dir = tmp.path();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@t.co"],
            vec!["config", "user.name", "t"],
        ] {
            tokio::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .await
                .unwrap();
        }
        std::fs::write(dir.join("README.md"), "base\n").unwrap();
        for args in [vec!["add", "."], vec!["commit", "-qm", "init"]] {
            tokio::process::Command::new("git")
                .args(&args)
                .current_dir(dir)
                .output()
                .await
                .unwrap();
        }
        let mut policy = SecurityPolicy::default();
        policy.autonomy = AutonomyLevel::Full;
        (
            GitWorktreeTool::new(Arc::new(policy), dir.to_path_buf()),
            tmp,
        )
    }

    #[tokio::test]
    async fn create_change_and_discard_leaves_base_untouched() {
        if which::which("git").is_err() {
            return;
        }
        let (tool, tmp) = init_repo().await;
        let created = tool
            .execute(json!({"action": "create", "name": "feature-x"}))
            .await
            .unwrap();
        assert!(created.success, "{:?}", created.error);

        let wt = tmp.path().join(".worktrees").join("feature-x");
        assert!(wt.join("README.md").exists());
        std::fs::write(wt.join("new.txt"), "scratch work").unwrap();

        let discarded = tool
            .execute(json!({"action": "discard", "name": "feature-x"}))
            .await
            .unwrap();
        assert!(discarded.success, "{:?}", discarded.error);
        assert!(!wt.exists(), "worktree dir removed");
        // Base checkout never saw the scratch file.
        assert!(!tmp.path().join("new.txt").exists());
    }

    #[tokio::test]
    async fn adopt_merges_worktree_changes_back() {
        if which::which("git").is_err() {
            return;
        }
        let (tool, tmp) = init_repo().await;
        tool.execute(json!({"action": "create", "name": "feat"}))
            .await
            .unwrap();
        let wt = tmp.path().join(".worktrees").join("feat");
        std::fs::write(wt.join("added.txt"), "adopted content").unwrap();
        for args in [vec!["add", "."], vec!["commit", "-qm", "work"]] {
            tokio::process::Command::new("git")
                .args(&args)
                .current_dir(&wt)
                .output()
                .await
                .unwrap();
        }

        let adopted = tool
            .execute(json!({"action": "adopt", "name": "feat"}))
            .await
            .unwrap();
        assert!(adopted.success, "{:?}", adopted.error);
        // The change is now in the base checkout.
        assert!(tmp.path().join("added.txt").exists());
        assert!(!wt.exists());
    }

    #[tokio::test]
    async fn rejects_unsafe_names() {
        let (tool, _tmp) = init_repo().await;
        let r = tool
            .execute(json!({"action": "create", "name": "../escape"}))
            .await
            .unwrap();
        assert!(!r.success);
    }
}
