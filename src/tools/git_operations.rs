use super::traits::{Tool, ToolResult};
use crate::security::{AutonomyLevel, SecurityPolicy};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;

/// Git operations tool for structured repository management.
/// Provides safe, parsed git operations with JSON output.
pub struct GitOperationsTool {
    security: Arc<SecurityPolicy>,
    workspace_dir: std::path::PathBuf,
    /// Node config dir holding the brokered GitHub token, if connected.
    config_dir: Option<std::path::PathBuf>,
}

impl GitOperationsTool {
    pub fn new(security: Arc<SecurityPolicy>, workspace_dir: std::path::PathBuf) -> Self {
        Self {
            security,
            workspace_dir,
            config_dir: None,
        }
    }

    /// Provide the config dir so github.com operations can use the token that
    /// was connected on the Settings page (device flow).
    pub fn with_config_dir(mut self, config_dir: std::path::PathBuf) -> Self {
        self.config_dir = Some(config_dir);
        self
    }

    /// True when a GitHub token is available for authenticated operations.
    fn github_connected(&self) -> bool {
        self.config_dir
            .as_deref()
            .and_then(crate::auth::github_device::brokered_token)
            .is_some()
    }

    /// Canonical form of the workspace root (symlinks resolved).
    fn canonical_workspace_root(&self) -> anyhow::Result<std::path::PathBuf> {
        std::fs::canonicalize(&self.workspace_dir)
            .map_err(|_| anyhow::anyhow!("could not canonicalize workspace directory"))
    }

    /// True when the workspace (or a parent of it) is a git repository.
    fn workspace_has_git_repo(&self) -> bool {
        if self.workspace_dir.join(".git").exists() {
            return true;
        }
        let mut current_dir = self.workspace_dir.as_path();
        while current_dir.parent().is_some() {
            if current_dir.join(".git").exists() {
                return true;
            }
            current_dir = current_dir.parent().unwrap();
        }
        false
    }

    /// Resolve the target repository for an operation from the optional
    /// `repo_path` argument.
    ///
    /// A relative path resolves against the workspace root; the result is
    /// canonicalized (symlinks and `..` resolved) and must be an existing
    /// directory that belongs to a git repository. The repository's top level
    /// is returned, so every subsequent command operates on exactly that
    /// repository — never silently substituting the workspace repository for
    /// the intended target.
    fn resolve_repo_dir(&self, args: &serde_json::Value) -> anyhow::Result<std::path::PathBuf> {
        let raw = match args.get("repo_path").and_then(|v| v.as_str()) {
            Some(p) if !p.trim().is_empty() => p.trim().to_string(),
            _ => anyhow::bail!("repo_path must not be empty"),
        };

        let candidate = if std::path::Path::new(&raw).is_absolute() {
            std::path::PathBuf::from(&raw)
        } else {
            self.workspace_dir.join(&raw)
        };

        let metadata = std::fs::metadata(&candidate).map_err(|_| {
            anyhow::anyhow!("repo_path '{}' does not exist or is not a directory", raw)
        })?;
        if !metadata.is_dir() {
            anyhow::bail!("repo_path '{}' is not a directory", raw);
        }

        let dir = std::fs::canonicalize(&candidate)
            .map_err(|_| anyhow::anyhow!("could not canonicalize repo_path '{}'", raw))?;

        // The directory must belong to a git repository. This also accepts git
        // worktrees, whose `.git` is a file rather than a directory.
        let output = std::process::Command::new("git")
            .args([
                "-C",
                dir.to_string_lossy().as_ref(),
                "rev-parse",
                "--show-toplevel",
            ])
            .output()
            .map_err(|e| anyhow::anyhow!("failed to inspect repository at '{}': {e}", raw))?;
        if !output.status.success() {
            anyhow::bail!("repo_path '{}' is not inside a git repository", raw);
        }
        let toplevel = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let toplevel_path = std::fs::canonicalize(&toplevel)
            .map_err(|_| anyhow::anyhow!("could not canonicalize repository top level"))?;
        Ok(toplevel_path)
    }

    /// Resolve and validate the clone destination. Clone destinations are
    /// separate from existing repository targets: the destination must stay
    /// inside the workspace root, and if it already exists it must be an
    /// empty directory.
    fn resolve_clone_destination(
        &self,
        args: &serde_json::Value,
    ) -> anyhow::Result<std::path::PathBuf> {
        let raw = args.get("directory").and_then(|v| v.as_str()).unwrap_or("");
        if raw.trim().is_empty() {
            // Default clone target is a fresh subdirectory of the workspace,
            // created by git itself.
            return Ok(self.workspace_dir.clone());
        }
        let raw = raw.trim();
        let candidate = if std::path::Path::new(raw).is_absolute() {
            std::path::PathBuf::from(raw)
        } else {
            self.workspace_dir.join(raw)
        };
        let workspace_root = self.canonical_workspace_root()?;

        if candidate.exists() {
            let dir = std::fs::canonicalize(&candidate)
                .map_err(|_| anyhow::anyhow!("could not canonicalize clone destination '{}'", raw))?;
            if !dir.starts_with(&workspace_root) {
                anyhow::bail!("clone destination '{}' is outside the node workspace", raw);
            }
            let empty = dir
                .read_dir()
                .map(|mut entries| entries.next().is_none())
                .unwrap_or(false);
            if !empty {
                anyhow::bail!("clone destination '{}' exists and is not empty", raw);
            }
            return Ok(dir);
        }

        let parent = candidate.parent().unwrap_or_else(|| std::path::Path::new("/"));
        let parent_canonical = std::fs::canonicalize(parent).map_err(|_| {
            anyhow::anyhow!("parent of clone destination '{}' does not exist", raw)
        })?;
        if !parent_canonical.starts_with(&workspace_root) {
            anyhow::bail!("clone destination '{}' is outside the node workspace", raw);
        }
        Ok(candidate)
    }

    /// Sanitize git arguments to prevent injection attacks
    fn sanitize_git_args(&self, args: &str) -> anyhow::Result<Vec<String>> {
        let mut result = Vec::new();
        for arg in args.split_whitespace() {
            // Block dangerous git options that could lead to command injection
            let arg_lower = arg.to_lowercase();
            if arg_lower.starts_with("--exec=")
                || arg_lower.starts_with("--upload-pack=")
                || arg_lower.starts_with("--receive-pack=")
                || arg_lower.starts_with("--pager=")
                || arg_lower.starts_with("--editor=")
                || arg_lower == "--no-verify"
                || arg_lower.contains("$(")
                || arg_lower.contains('`')
                || arg.contains('|')
                || arg.contains(';')
                || arg.contains('>')
            {
                anyhow::bail!("Blocked potentially dangerous git argument: {arg}");
            }
            // Block `-c` config injection (exact match or `-c=...` prefix).
            // This must not false-positive on `--cached` or `-cached`.
            if arg_lower == "-c" || arg_lower.starts_with("-c=") {
                anyhow::bail!("Blocked potentially dangerous git argument: {arg}");
            }
            result.push(arg.to_string());
        }
        Ok(result)
    }

    /// Check if an operation requires write access
    fn requires_write_access(&self, operation: &str) -> bool {
        matches!(
            operation,
            "commit"
                | "add"
                | "checkout"
                | "stash"
                | "reset"
                | "revert"
                | "clone"
                | "pull"
                | "fetch"
                | "push"
        )
    }

    /// Check if an operation is read-only
    fn is_read_only(&self, operation: &str) -> bool {
        matches!(
            operation,
            "status" | "diff" | "log" | "show" | "branch" | "rev-parse"
        )
    }

    async fn run_git_command(&self, cwd: &std::path::Path, args: &[&str]) -> anyhow::Result<String> {
        let output = tokio::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .await?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("Git command failed: {stderr}");
        }

        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Run a git command in `cwd` with the brokered GitHub token applied via
    /// `url.insteadOf`, which transparently authenticates clone, pull, fetch,
    /// and push against any github.com HTTPS remote. Scrubs the token from any
    /// error text so it never surfaces in tool output.
    async fn run_git_authenticated(
        &self,
        cwd: &std::path::Path,
        args: &[&str],
    ) -> anyhow::Result<String> {
        let token = self
            .config_dir
            .as_deref()
            .and_then(crate::auth::github_device::brokered_token);

        let mut full: Vec<String> = Vec::new();
        if let Some(ref t) = token {
            // Rewrite github.com HTTPS URLs to include the token on the fly.
            full.push("-c".into());
            full.push(format!(
                "url.https://x-access-token:{t}@github.com/.insteadOf=https://github.com/"
            ));
        }
        full.extend(args.iter().map(|s| s.to_string()));

        let output = tokio::process::Command::new("git")
            .args(&full)
            .current_dir(cwd)
            .output()
            .await?;

        if !output.status.success() {
            let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
            if let Some(ref t) = token {
                stderr = stderr.replace(t.as_str(), "***");
            }
            anyhow::bail!("Git command failed: {stderr}");
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// clone handler. `url` is required.
    ///
    /// Clone is a destination operation with its own validation, separate
    /// from existing repository targets.
    async fn git_clone(&self, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let Some(url) = args.get("url").and_then(|v| v.as_str()) else {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("'url' is required for clone".into()),
            });
        };
        // Reject option-injection and non-http(s)/git schemes.
        if url.starts_with('-')
            || !(url.starts_with("https://")
                || url.starts_with("http://")
                || url.starts_with("git@")
                || url.starts_with("git://"))
        {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Refusing to clone unsafe URL: {url}")),
            });
        }
        // Clone destination is validated separately from repository targets
        // and must stay inside the workspace.
        let destination = self.resolve_clone_destination(args)?;
        let mut git_args = vec!["clone".to_string(), url.to_string()];
        if args.get("directory").and_then(|v| v.as_str()).is_some() {
            let raw = args.get("directory").and_then(|v| v.as_str()).unwrap_or("");
            git_args.push(raw.to_string());
        }
        let refs: Vec<&str> = git_args.iter().map(String::as_str).collect();
        let output = self.run_git_authenticated(&destination, &refs).await;
        match output {
            Ok(out) => Ok(ToolResult {
                success: true,
                output: if out.trim().is_empty() {
                    "clone completed".to_string()
                } else {
                    out
                },
                error: None,
            }),
            Err(e) => {
                let hint = if !self.github_connected() && e.to_string().contains("Authentication")
                {
                    " (no GitHub token connected — connect GitHub on the Settings page)"
                } else {
                    ""
                };
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("{e}{hint}")),
                })
            }
        }
    }

    /// pull/fetch/push handler against an already-resolved repository target.
    async fn git_remote_op(
        &self,
        operation: &str,
        target: &std::path::Path,
        args: &serde_json::Value,
    ) -> anyhow::Result<ToolResult> {
        let result = match operation {
            "pull" => self.run_git_authenticated(target, &["pull", "--ff-only"]).await,
            "fetch" => self.run_git_authenticated(target, &["fetch", "--all", "--prune"]).await,
            "push" => {
                let mut git_args = vec!["push"];
                if let Some(branch) = args.get("branch").and_then(|v| v.as_str()) {
                    git_args.push("origin");
                    git_args.push(branch);
                }
                self.run_git_authenticated(target, &git_args).await
            }
            _ => unreachable!(),
        };

        match result {
            Ok(output) => Ok(ToolResult {
                success: true,
                output: if output.trim().is_empty() {
                    format!("{operation} completed")
                } else {
                    output
                },
                error: None,
            }),
            Err(e) => {
                let hint = if !self.github_connected()
                    && e.to_string().contains("Authentication")
                {
                    " (no GitHub token connected — connect GitHub on the Settings page)"
                } else {
                    ""
                };
                Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("{e}{hint}")),
                })
            }
        }
    }

    async fn git_status(&self, cwd: &std::path::Path) -> anyhow::Result<ToolResult> {
        let output = self
            .run_git_command(cwd, &["status", "--porcelain=2", "--branch"])
            .await?;

        // Parse git status output into structured format
        let mut result = serde_json::Map::new();
        let mut branch = String::new();
        let mut staged = Vec::new();
        let mut unstaged = Vec::new();
        let mut untracked = Vec::new();

        for line in output.lines() {
            if line.starts_with("# branch.head ") {
                branch = line.trim_start_matches("# branch.head ").to_string();
            } else if let Some(rest) = line.strip_prefix("1 ") {
                // Ordinary changed entry
                let mut parts = rest.splitn(3, ' ');
                if let (Some(staging), Some(path)) = (parts.next(), parts.next()) {
                    if !staging.is_empty() {
                        let status_char = staging.chars().next().unwrap_or(' ');
                        if status_char != '.' && status_char != ' ' {
                            staged.push(json!({"path": path, "status": status_char}));
                        }
                        let status_char = staging.chars().nth(1).unwrap_or(' ');
                        if status_char != '.' && status_char != ' ' {
                            unstaged.push(json!({"path": path, "status": status_char}));
                        }
                    }
                }
            } else if let Some(rest) = line.strip_prefix("? ") {
                untracked.push(rest.to_string());
            }
        }

        result.insert("branch".to_string(), json!(branch));
        result.insert("staged".to_string(), json!(staged));
        result.insert("unstaged".to_string(), json!(unstaged));
        result.insert("untracked".to_string(), json!(untracked));
        result.insert(
            "clean".to_string(),
            json!(staged.is_empty() && unstaged.is_empty() && untracked.is_empty()),
        );

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result).unwrap_or_default(),
            error: None,
        })
    }

    async fn git_diff(&self, cwd: &std::path::Path, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let files = args.get("files").and_then(|v| v.as_str()).unwrap_or(".");
        let cached = args
            .get("cached")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        // Validate files argument against injection patterns
        self.sanitize_git_args(files)?;

        let mut git_args = vec!["diff", "--unified=3"];
        if cached {
            git_args.push("--cached");
        }
        git_args.push("--");
        git_args.push(files);

        let output = self.run_git_command(cwd, &git_args).await?;

        // Parse diff into structured hunks
        let mut result = serde_json::Map::new();
        let mut hunks = Vec::new();
        let mut current_file = String::new();
        let mut current_hunk = serde_json::Map::new();
        let mut lines = Vec::new();

        for line in output.lines() {
            if line.starts_with("diff --git ") {
                if !lines.is_empty() {
                    current_hunk.insert("lines".to_string(), json!(lines));
                    if !current_hunk.is_empty() {
                        hunks.push(serde_json::Value::Object(current_hunk.clone()));
                    }
                    lines = Vec::new();
                    current_hunk = serde_json::Map::new();
                }
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 4 {
                    current_file = parts[3].trim_start_matches("b/").to_string();
                    current_hunk.insert("file".to_string(), json!(current_file));
                }
            } else if line.starts_with("@@ ") {
                if !lines.is_empty() {
                    current_hunk.insert("lines".to_string(), json!(lines));
                    if !current_hunk.is_empty() {
                        hunks.push(serde_json::Value::Object(current_hunk.clone()));
                    }
                    lines = Vec::new();
                    current_hunk = serde_json::Map::new();
                    current_hunk.insert("file".to_string(), json!(current_file));
                }
                current_hunk.insert("header".to_string(), json!(line));
            } else if !line.is_empty() {
                lines.push(json!({
                    "text": line,
                    "type": if line.starts_with('+') { "add" }
                           else if line.starts_with('-') { "delete" }
                           else { "context" }
                }));
            }
        }

        if !lines.is_empty() {
            current_hunk.insert("lines".to_string(), json!(lines));
            if !current_hunk.is_empty() {
                hunks.push(serde_json::Value::Object(current_hunk));
            }
        }

        result.insert("hunks".to_string(), json!(hunks));
        result.insert("file_count".to_string(), json!(hunks.len()));

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&result).unwrap_or_default(),
            error: None,
        })
    }

    async fn git_log(&self, cwd: &std::path::Path, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let limit_raw = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10);
        let limit = usize::try_from(limit_raw).unwrap_or(usize::MAX).min(1000);
        let limit_str = limit.to_string();

        let output = self
            .run_git_command(
                cwd,
                &[
                    "log",
                    &format!("-{limit_str}"),
                    "--pretty=format:%H|%an|%ae|%ad|%s",
                    "--date=iso",
                ],
            )
            .await?;

        let mut commits = Vec::new();

        for line in output.lines() {
            let parts: Vec<&str> = line.split('|').collect();
            if parts.len() >= 5 {
                commits.push(json!({
                    "hash": parts[0],
                    "author": parts[1],
                    "email": parts[2],
                    "date": parts[3],
                    "message": parts[4]
                }));
            }
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({ "commits": commits }))
                .unwrap_or_default(),
            error: None,
        })
    }

    async fn git_branch(&self, cwd: &std::path::Path) -> anyhow::Result<ToolResult> {
        let output = self
            .run_git_command(cwd, &["branch", "--format=%(refname:short)|%(HEAD)"])
            .await?;

        let mut branches = Vec::new();
        let mut current = String::new();

        for line in output.lines() {
            if let Some((name, head)) = line.split_once('|') {
                let is_current = head == "*";
                if is_current {
                    current = name.to_string();
                }
                branches.push(json!({
                    "name": name,
                    "current": is_current
                }));
            }
        }

        Ok(ToolResult {
            success: true,
            output: serde_json::to_string_pretty(&json!({
                "current": current,
                "branches": branches
            }))
            .unwrap_or_default(),
            error: None,
        })
    }

    fn truncate_commit_message(message: &str) -> String {
        if message.chars().count() > 2000 {
            format!("{}...", message.chars().take(1997).collect::<String>())
        } else {
            message.to_string()
        }
    }

    async fn git_commit(&self, cwd: &std::path::Path, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let message = args
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'message' parameter"))?;

        // Sanitize commit message
        let sanitized = message
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n");

        if sanitized.is_empty() {
            anyhow::bail!("Commit message cannot be empty");
        }

        // Limit message length
        let message = Self::truncate_commit_message(&sanitized);

        let output = self.run_git_command(cwd, &["commit", "-m", &message]).await;

        match output {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Committed: {message}"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Commit failed: {e}")),
            }),
        }
    }

    async fn git_add(&self, cwd: &std::path::Path, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let paths = args
            .get("paths")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'paths' parameter"))?;

        // Validate paths against injection patterns
        self.sanitize_git_args(paths)?;

        let output = self.run_git_command(cwd, &["add", "--", paths]).await;

        match output {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Staged: {paths}"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Add failed: {e}")),
            }),
        }
    }

    async fn git_checkout(&self, cwd: &std::path::Path, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let branch = args
            .get("branch")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'branch' parameter"))?;

        // Sanitize branch name
        let sanitized = self.sanitize_git_args(branch)?;

        if sanitized.is_empty() || sanitized.len() > 1 {
            anyhow::bail!("Invalid branch specification");
        }

        let branch_name = &sanitized[0];

        // Block dangerous branch names
        if branch_name.contains('@') || branch_name.contains('^') || branch_name.contains('~') {
            anyhow::bail!("Branch name contains invalid characters");
        }

        let output = self.run_git_command(cwd, &["checkout", branch_name]).await;

        match output {
            Ok(_) => Ok(ToolResult {
                success: true,
                output: format!("Switched to branch: {branch_name}"),
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Checkout failed: {e}")),
            }),
        }
    }

    async fn git_stash(&self, cwd: &std::path::Path, args: &serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .unwrap_or("push");

        let output = match action {
            "push" | "save" => self.run_git_command(cwd, &["stash", "push", "-m", "auto-stash"]).await,
            "pop" => self.run_git_command(cwd, &["stash", "pop"]).await,
            "list" => self.run_git_command(cwd, &["stash", "list"]).await,
            "drop" => {
                let index_raw = args.get("index").and_then(|v| v.as_u64()).unwrap_or(0);
                let index = i32::try_from(index_raw)
                    .map_err(|_| anyhow::anyhow!("stash index too large: {index_raw}"))?;
                self.run_git_command(cwd, &["stash", "drop", &format!("stash@{{{index}}}")])
                    .await
            }
            _ => anyhow::bail!("Unknown stash action: {action}. Use: push, pop, list, drop"),
        };

        match output {
            Ok(out) => Ok(ToolResult {
                success: true,
                output: out,
                error: None,
            }),
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Stash {action} failed: {e}")),
            }),
        }
    }
}

#[async_trait]
impl Tool for GitOperationsTool {
    fn name(&self) -> &str {
        "git_operations"
    }

    fn description(&self) -> &str {
        "Perform Git operations: status, diff, log, branch, commit, add, checkout, stash, and remote operations clone/pull/fetch/push. Every operation (except clone) targets the repository containing the optional 'repo_path' (absolute, or relative to the node workspace) — pass it when operating on a repository other than the workspace root. 'clone' needs a 'url' (and optional 'directory'); 'push' takes an optional 'branch'. Remote operations against github.com authenticate automatically with the GitHub account connected on the Settings page — use these instead of web_fetch to work with repositories."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "operation": {
                    "type": "string",
                    "enum": ["status", "diff", "log", "branch", "commit", "add", "checkout", "stash", "clone", "pull", "fetch", "push"],
                    "description": "Git operation to perform"
                },
                "message": {
                    "type": "string",
                    "description": "Commit message (for 'commit' operation)"
                },
                "paths": {
                    "type": "string",
                    "description": "File paths to stage (for 'add' operation)"
                },
                "branch": {
                    "type": "string",
                    "description": "Branch name (for 'checkout' and 'push' operations)"
                },
                "repo_path": {
                    "type": "string",
                    "description": "Repository to operate on (all operations except 'clone'). Absolute path, or relative to the node workspace; must be a directory inside a git repository. Commands always run against the repository containing this path, so operations target the intended repo (including a sibling repo) and never silently substitute the workspace repo. Omit to operate on the workspace repository (backward compatible)."
                },
                "url": {
                    "type": "string",
                    "description": "Repository URL (required for 'clone'), e.g. https://github.com/owner/repo"
                },
                "directory": {
                    "type": "string",
                    "description": "Target directory name for 'clone' (optional)"
                },
                "files": {
                    "type": "string",
                    "description": "File or path to diff (for 'diff' operation, default: '.')"
                },
                "cached": {
                    "type": "boolean",
                    "description": "Show staged changes (for 'diff' operation)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of log entries (for 'log' operation, default: 10)"
                },
                "action": {
                    "type": "string",
                    "enum": ["push", "pop", "list", "drop"],
                    "description": "Stash action (for 'stash' operation)"
                },
                "index": {
                    "type": "integer",
                    "description": "Stash index (for 'stash' with 'drop' action)"
                }
            },
            "required": ["operation"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let operation = match args.get("operation").and_then(|v| v.as_str()) {
            Some(op) => op,
            None => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("Missing 'operation' parameter".into()),
                });
            }
        };

        // Clone targets a destination directory, not an existing repository;
        // handle it before any repository resolution.
        if operation == "clone" {
            return self.git_clone(&args).await;
        }

        // Resolve the target repository. An explicit `repo_path` names the
        // repository to operate on; when omitted the workspace repository is
        // used (backward-compatible default).
        let target = if let Some(raw) = args.get("repo_path").and_then(|v| v.as_str()) {
            if raw.trim().is_empty() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("repo_path must not be empty".into()),
                });
            }
            match self.resolve_repo_dir(&args) {
                Ok(dir) => dir,
                Err(e) => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(e.to_string()),
                    });
                }
            }
        } else if !self.workspace_has_git_repo() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Not in a git repository (no repo_path given)".into()),
            });
        } else {
            self.workspace_dir.clone()
        };

        // Check autonomy level for write operations
        if self.requires_write_access(operation) {
            if !self.security.can_act() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "Action blocked: git write operations require higher autonomy level".into(),
                    ),
                });
            }

            match self.security.autonomy {
                AutonomyLevel::ReadOnly => {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Action blocked: read-only mode".into()),
                    });
                }
                AutonomyLevel::Supervised | AutonomyLevel::Full => {}
            }
        }

        // Record action for rate limiting
        if !self.security.record_action() {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some("Action blocked: rate limit exceeded".into()),
            });
        }

        // Execute the requested operation against the resolved target
        let target = target.as_path();
        match operation {
            "status" => self.git_status(target).await,
            "diff" => self.git_diff(target, &args).await,
            "log" => self.git_log(target, &args).await,
            "branch" => self.git_branch(target).await,
            "commit" => self.git_commit(target, &args).await,
            "add" => self.git_add(target, &args).await,
            "checkout" => self.git_checkout(target, &args).await,
            "stash" => self.git_stash(target, &args).await,
            "pull" | "fetch" | "push" => {
                let op = operation.to_string();
                self.git_remote_op(&op, target, &args).await
            }
            _ => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Unknown operation: {operation}")),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::SecurityPolicy;
    use tempfile::TempDir;

    fn test_tool(dir: std::path::PathBuf) -> GitOperationsTool {
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::Supervised,
            ..SecurityPolicy::default()
        });
        GitOperationsTool::new(security, dir)
    }

    #[test]
    fn sanitize_git_blocks_injection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        // Should block dangerous arguments
        assert!(tool.sanitize_git_args("--exec=rm -rf /").is_err());
        assert!(tool.sanitize_git_args("$(echo pwned)").is_err());
        assert!(tool.sanitize_git_args("`malicious`").is_err());
        assert!(tool.sanitize_git_args("arg | cat").is_err());
        assert!(tool.sanitize_git_args("arg; rm file").is_err());
    }

    #[test]
    fn sanitize_git_blocks_pager_editor_injection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        assert!(tool.sanitize_git_args("--pager=less").is_err());
        assert!(tool.sanitize_git_args("--editor=vim").is_err());
    }

    #[test]
    fn sanitize_git_blocks_config_injection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        // Exact `-c` flag (config injection)
        assert!(tool.sanitize_git_args("-c core.sshCommand=evil").is_err());
        assert!(tool.sanitize_git_args("-c=core.pager=less").is_err());
    }

    #[test]
    fn sanitize_git_blocks_no_verify() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        assert!(tool.sanitize_git_args("--no-verify").is_err());
    }

    #[test]
    fn sanitize_git_blocks_redirect_in_args() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        assert!(tool.sanitize_git_args("file.txt > /tmp/out").is_err());
    }

    #[test]
    fn sanitize_git_cached_not_blocked() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        // --cached must NOT be blocked by the `-c` check
        assert!(tool.sanitize_git_args("--cached").is_ok());
        // Other safe flags starting with -c prefix
        assert!(tool.sanitize_git_args("-cached").is_ok());
    }

    #[test]
    fn sanitize_git_allows_safe() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        // Should allow safe arguments
        assert!(tool.sanitize_git_args("main").is_ok());
        assert!(tool.sanitize_git_args("feature/test-branch").is_ok());
        assert!(tool.sanitize_git_args("--cached").is_ok());
        assert!(tool.sanitize_git_args("src/main.rs").is_ok());
        assert!(tool.sanitize_git_args(".").is_ok());
    }

    #[test]
    fn requires_write_detection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        assert!(tool.requires_write_access("commit"));
        assert!(tool.requires_write_access("add"));
        assert!(tool.requires_write_access("checkout"));

        assert!(!tool.requires_write_access("status"));
        assert!(!tool.requires_write_access("diff"));
        assert!(!tool.requires_write_access("log"));
    }

    #[test]
    fn branch_is_not_write_gated() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        // Branch listing is read-only; it must not require write access
        assert!(!tool.requires_write_access("branch"));
        assert!(tool.is_read_only("branch"));
    }

    #[test]
    fn is_read_only_detection() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        assert!(tool.is_read_only("status"));
        assert!(tool.is_read_only("diff"));
        assert!(tool.is_read_only("log"));
        assert!(tool.is_read_only("branch"));

        assert!(!tool.is_read_only("commit"));
        assert!(!tool.is_read_only("add"));
    }

    #[tokio::test]
    async fn blocks_readonly_mode_for_write_ops() {
        let tmp = TempDir::new().unwrap();
        // Initialize a git repository
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, tmp.path().to_path_buf());

        let result = tool
            .execute(json!({"operation": "commit", "message": "test"}))
            .await
            .unwrap();
        assert!(!result.success);
        // can_act() returns false for ReadOnly, so we get the "higher autonomy level" message
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("higher autonomy")
        );
    }

    #[tokio::test]
    async fn allows_branch_listing_in_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        // Initialize a git repository so the command can succeed
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, tmp.path().to_path_buf());

        let result = tool.execute(json!({"operation": "branch"})).await.unwrap();
        // Branch listing must not be blocked by read-only autonomy
        let error_msg = result.error.as_deref().unwrap_or("");
        assert!(
            !error_msg.contains("read-only") && !error_msg.contains("higher autonomy"),
            "branch listing should not be blocked in read-only mode, got: {error_msg}"
        );
    }

    #[tokio::test]
    async fn allows_readonly_ops_in_readonly_mode() {
        let tmp = TempDir::new().unwrap();
        let security = Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        });
        let tool = GitOperationsTool::new(security, tmp.path().to_path_buf());

        // This will fail because there's no git repo, but it shouldn't be blocked by autonomy
        let result = tool.execute(json!({"operation": "status"})).await.unwrap();
        // The error should be about git (not about autonomy/read-only mode)
        assert!(!result.success, "Expected failure due to missing git repo");
        let error_msg = result.error.as_deref().unwrap_or("");
        assert!(
            !error_msg.is_empty(),
            "Expected a git-related error message"
        );
        assert!(
            !error_msg.contains("read-only") && !error_msg.contains("autonomy"),
            "Error should be about git, not about autonomy restrictions: {error_msg}"
        );
    }

    #[tokio::test]
    async fn rejects_missing_operation() {
        let tmp = TempDir::new().unwrap();
        let tool = test_tool(tmp.path().to_path_buf());

        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Missing 'operation'")
        );
    }

    #[tokio::test]
    async fn rejects_unknown_operation() {
        let tmp = TempDir::new().unwrap();
        // Initialize a git repository
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();

        let tool = test_tool(tmp.path().to_path_buf());

        let result = tool
            .execute(json!({"operation": "frobnicate"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("Unknown operation")
        );
    }

    #[tokio::test]
    async fn clone_rejects_option_injection_url() {
        let tmp = TempDir::new().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .output()
            .unwrap();
        let tool = test_tool(tmp.path().to_path_buf());
        let result = tool
            .execute(json!({"operation": "clone", "url": "--upload-pack=touch /tmp/x"}))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error.as_deref().unwrap_or("").contains("unsafe URL"));
    }

    #[test]
    fn truncates_multibyte_commit_message_without_panicking() {
        let long = "🦀".repeat(2500);
        let truncated = GitOperationsTool::truncate_commit_message(&long);

        assert_eq!(truncated.chars().count(), 2000);
    }

    // ---- repo_path target resolution: cross-repository isolation ----
    //
    // These tests reproduce the historical defect where a push intended for a
    // sibling repository (e.g. `St-Clippus`) silently targeted the workspace
    // repository (e.g. `LlamaFarmWorkspace`) because every operation ran with
    // the workspace directory as its cwd.

    fn run_git(dir: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git command should run");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn init_repo(base: &std::path::Path) {
        run_git(base, &["init", "-b", "main"]);
        run_git(base, &["config", "user.email", "llamafarm_user@example.com"]);
        run_git(base, &["config", "user.name", "llamafarm_user"]);
        std::fs::write(base.join("marker.txt"), "marker\n").unwrap();
        run_git(base, &["add", "--", "marker.txt"]);
        run_git(base, &["commit", "-m", "initial commit"]);
    }

    fn head_message(repo: &std::path::Path) -> String {
        let out = std::process::Command::new("git")
            .args(["log", "-1", "--pretty=%s"])
            .current_dir(repo)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    #[tokio::test]
    async fn repo_path_add_and_commit_target_sibling_not_workspace() {
        let base = TempDir::new().unwrap();
        // Two sibling repositories, as in the real incident.
        let workspace_repo = base.path().join("LlamaFarmWorkspace");
        let target_repo = base.path().join("St-Clippus");
        std::fs::create_dir_all(&workspace_repo).unwrap();
        std::fs::create_dir_all(&target_repo).unwrap();
        init_repo(&workspace_repo);
        init_repo(&target_repo);

        // A new file exists only in the target repository.
        std::fs::write(target_repo.join("stclippus.txt"), "sibling content").unwrap();

        let tool = test_tool(workspace_repo.clone());
        let before = head_message(&workspace_repo);

        let add_result = tool
            .execute(json!({
                "operation": "add",
                "repo_path": target_repo.to_str().unwrap(),
                "paths": "stclippus.txt"
            }))
            .await
            .unwrap();
        assert!(add_result.success, "add failed: {:?}", add_result.error);

        let commit_result = tool
            .execute(json!({
                "operation": "commit",
                "repo_path": target_repo.to_str().unwrap(),
                "message": "add stclippus file"
            }))
            .await
            .unwrap();
        assert!(commit_result.success, "commit failed: {:?}", commit_result.error);

        // The workspace repository HEAD must be unchanged.
        let after = head_message(&workspace_repo);
        assert_eq!(
            before, after,
            "workspace repository HEAD changed during a sibling commit"
        );
        // The new file must be committed in the target repository.
        assert!(
            target_repo.join("stclippus.txt").exists(),
            "sibling commit should have created the file in the target repository"
        );
        assert_eq!(
            head_message(&target_repo),
            "add stclippus file",
            "target repository HEAD should be the new commit"
        );
        // The workspace repository index must not contain the sibling's file.
        let ws_status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&workspace_repo)
            .output()
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&ws_status.stdout).contains("stclippus.txt"),
            "sibling file leaked into workspace repository index"
        );
    }

    #[tokio::test]
    async fn repo_path_status_reports_target_repo() {
        let base = TempDir::new().unwrap();
        let workspace_repo = base.path().join("LlamaFarmWorkspace");
        let target_repo = base.path().join("St-Clippus");
        std::fs::create_dir_all(&workspace_repo).unwrap();
        std::fs::create_dir_all(&target_repo).unwrap();
        init_repo(&workspace_repo);
        init_repo(&target_repo);

        // Create a distinctive untracked file in the target repository only.
        std::fs::write(target_repo.join("unique_to_target.txt"), "x").unwrap();

        let tool = test_tool(workspace_repo.clone());

        let result = tool
            .execute(json!({"operation": "status", "repo_path": target_repo.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(result.success, "status failed: {:?}", result.error);
        assert!(
            result.output.contains("unique_to_target.txt"),
            "status did not reflect the target repository: {}",
            result.output
        );

        // The workspace repository must still be clean (no leak of the sibling's file).
        let ws_status = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&workspace_repo)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&ws_status.stdout).trim().is_empty(),
            "workspace repository should be clean"
        );
    }

    #[tokio::test]
    async fn repo_path_branch_targets_target_repo() {
        let base = TempDir::new().unwrap();
        let workspace_repo = base.path().join("LlamaFarmWorkspace");
        let target_repo = base.path().join("St-Clippus");
        std::fs::create_dir_all(&workspace_repo).unwrap();
        std::fs::create_dir_all(&target_repo).unwrap();
        init_repo(&workspace_repo);
        init_repo(&target_repo);

        // Distinct branch names in each repo.
        run_git(&target_repo, &["checkout", "-b", "stclippus-branch"]);
        let tool = test_tool(workspace_repo.clone());

        let result = tool
            .execute(json!({"operation": "branch", "repo_path": target_repo.to_str().unwrap()}))
            .await
            .unwrap();
        assert!(result.success, "branch failed: {:?}", result.error);
        assert!(
            result.output.contains("stclippus-branch"),
            "branch listing did not reflect the target repository: {}",
            result.output
        );
    }

    #[tokio::test]
    async fn repo_path_commit_targets_exact_sibling_repo() {
        // Regression: a commit meant for a sibling repository must land in that
        // repository, not in the workspace repository.
        let base = TempDir::new().unwrap();
        let workspace_repo = base.path().join("LlamaFarmWorkspace");
        let sibling_repo = base.path().join("St-Clippus");
        std::fs::create_dir_all(&workspace_repo).unwrap();
        std::fs::create_dir_all(&sibling_repo).unwrap();
        init_repo(&workspace_repo);
        init_repo(&sibling_repo);

        // Put a file in the workspace repo so we can prove it is NOT the target.
        std::fs::write(workspace_repo.join("ws_marker.txt"), "workspace").unwrap();
        run_git(&workspace_repo, &["add", "--", "ws_marker.txt"]);
        run_git(&workspace_repo, &["commit", "-m", "workspace commit"]);

        let tool = test_tool(workspace_repo.clone());
        let ws_head_before = head_message(&workspace_repo);

        // A new file only in the sibling repo.
        std::fs::write(sibling_repo.join("sibling_new.txt"), "sibling").unwrap();
        let add = tool
            .execute(json!({
                "operation": "add",
                "repo_path": sibling_repo.to_str().unwrap(),
                "paths": "sibling_new.txt"
            }))
            .await
            .unwrap();
        assert!(add.success, "add failed: {:?}", add.error);

        let commit = tool
            .execute(json!({
                "operation": "commit",
                "repo_path": sibling_repo.to_str().unwrap(),
                "message": "sibling commit"
            }))
            .await
            .unwrap();
        assert!(commit.success, "commit failed: {:?}", commit.error);

        // Workspace HEAD is unchanged; sibling HEAD is the new commit.
        assert_eq!(ws_head_before, head_message(&workspace_repo));
        assert_eq!(head_message(&sibling_repo), "sibling commit");
        assert!(sibling_repo.join("sibling_new.txt").exists());
    }

    #[tokio::test]
    async fn repo_path_nonexistent_is_rejected() {
        let base = TempDir::new().unwrap();
        let workspace_repo = base.path().join("LlamaFarmWorkspace");
        std::fs::create_dir_all(&workspace_repo).unwrap();
        init_repo(&workspace_repo);

        let tool = test_tool(workspace_repo.clone());
        let result = tool
            .execute(json!({
                "operation": "status",
                "repo_path": "does-not-exist-xyz"
            }))
            .await
            .unwrap();
        assert!(!result.success);
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("does not exist")
        );
    }
}
