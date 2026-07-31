//! Session-scoped task checklist for tracking multi-step work.
//!
//! Provides a `task_plan` tool that lets the agent break complex work into
//! steps and track progress within a single session. The task list lives in
//! memory (`Arc<RwLock<Vec<TaskItem>>>`) and is discarded when the session
//! ends — it is intentionally not persisted via the Memory trait.

use crate::security::{policy::ToolOperation, SecurityPolicy};
use crate::tools::traits::{Tool, ToolResult};
use async_trait::async_trait;
use serde_json::json;
use std::fmt;
use std::sync::{Arc, RwLock};

// ── Data Structures ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    Blocked,
    Skipped,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "pending"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Completed => write!(f, "completed"),
            TaskStatus::Failed => write!(f, "failed"),
            TaskStatus::Blocked => write!(f, "blocked"),
            TaskStatus::Skipped => write!(f, "skipped"),
        }
    }
}

impl TaskStatus {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(TaskStatus::Pending),
            "in_progress" => Some(TaskStatus::InProgress),
            "completed" => Some(TaskStatus::Completed),
            "failed" => Some(TaskStatus::Failed),
            "blocked" => Some(TaskStatus::Blocked),
            "skipped" => Some(TaskStatus::Skipped),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
struct TaskItem {
    id: usize,
    title: String,
    status: TaskStatus,
    /// Compact task-local intent and acceptance context.
    context: Option<String>,
    /// Expected tools for this step. This guides execution and evidence
    /// attribution; it does not expand any runtime security allowlist.
    tools: Vec<String>,
    /// Earlier step IDs that must resolve before this step can verify.
    depends_on: Vec<usize>,
    /// Evidence patterns used by the durable run-ledger verifier.
    expected_evidence: Vec<String>,
}

fn optional_string(entry: &serde_json::Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| entry.get(*key).and_then(serde_json::Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

fn string_list(entry: &serde_json::Value, keys: &[&str]) -> Vec<String> {
    let mut values = Vec::new();
    for key in keys {
        if let Some(items) = entry.get(*key).and_then(serde_json::Value::as_array) {
            for item in items {
                if let Some(value) = item
                    .as_str()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    if !values.iter().any(|existing| existing == value) {
                        values.push(value.to_string());
                    }
                }
            }
        }
    }
    values
}

fn usize_list(entry: &serde_json::Value, key: &str) -> Vec<usize> {
    let mut values = Vec::new();
    if let Some(items) = entry.get(key).and_then(serde_json::Value::as_array) {
        for item in items {
            if let Some(value) = item.as_u64().and_then(|value| usize::try_from(value).ok()) {
                if value > 0 && !values.contains(&value) {
                    values.push(value);
                }
            }
        }
    }
    values
}

// ── Tool ─────────────────────────────────────────────────────────────────

pub struct TaskPlanTool {
    security: Arc<SecurityPolicy>,
    tasks: Arc<RwLock<Vec<TaskItem>>>,
    next_id: Arc<RwLock<usize>>,
}

impl TaskPlanTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self {
            security,
            tasks: Arc::new(RwLock::new(Vec::new())),
            next_id: Arc::new(RwLock::new(1)),
        }
    }

    /// Enforce mutation permission (autonomy + rate limit).
    fn enforce_mutation(&self) -> Result<(), ToolResult> {
        self.security
            .enforce_tool_operation(ToolOperation::Act, "task_plan")
            .map_err(|msg| ToolResult {
                success: false,
                output: String::new(),
                error: Some(msg),
            })
    }

    fn render_task_list(tasks: &[TaskItem]) -> String {
        let completed = tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Completed)
            .count();
        let resolved = tasks
            .iter()
            .filter(|task| {
                matches!(
                    task.status,
                    TaskStatus::Completed
                        | TaskStatus::Failed
                        | TaskStatus::Blocked
                        | TaskStatus::Skipped
                )
            })
            .count();
        let total = tasks.len();

        let mut lines = vec![format!(
            "Tasks ({completed}/{total} completed; {resolved}/{total} resolved):"
        )];
        for task in tasks {
            lines.push(format!("- [{}] [{}] {}", task.id, task.status, task.title));
            if let Some(context) = task.context.as_deref() {
                lines.push(format!("    ↳ context: {context}"));
            }
            if !task.tools.is_empty() {
                lines.push(format!("    ↳ tools: {}", task.tools.join(", ")));
            }
            if !task.depends_on.is_empty() {
                lines.push(format!(
                    "    ↳ depends_on: {}",
                    task.depends_on
                        .iter()
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            if !task.expected_evidence.is_empty() {
                lines.push(format!(
                    "    ↳ evidence: {}",
                    task.expected_evidence.join(", ")
                ));
            }
        }

        lines.join("\n")
    }

    fn handle_create(&self, tasks_val: &serde_json::Value) -> ToolResult {
        let arr = match tasks_val.as_array() {
            Some(a) if !a.is_empty() => a,
            _ => {
                return ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(
                        "Parameter 'tasks' must be a non-empty array of {title, status?, context?, tools?}".into(),
                    ),
                };
            }
        };

        let mut items = Vec::with_capacity(arr.len());
        let mut id = 1usize;
        for entry in arr {
            let title = match entry.get("title").and_then(|v| v.as_str()) {
                Some(t) if !t.is_empty() => t.to_string(),
                _ => {
                    return ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Each task must have a non-empty 'title' string".into()),
                    };
                }
            };
            let status = entry
                .get("status")
                .and_then(|v| v.as_str())
                .and_then(TaskStatus::from_str)
                .unwrap_or(TaskStatus::Pending);
            items.push(TaskItem {
                id,
                title,
                status,
                context: optional_string(entry, &["context", "sub_context"]),
                tools: string_list(entry, &["tools", "allowed_tools"]),
                depends_on: usize_list(entry, "depends_on"),
                expected_evidence: string_list(entry, &["expected_evidence"]),
            });
            id += 1;
        }

        let count = items.len();
        *self.tasks.write().unwrap() = items;
        *self.next_id.write().unwrap() = id;

        let tasks = self.tasks.read().unwrap();
        let output = format!(
            "Created {count} task(s).\n{}",
            Self::render_task_list(&tasks)
        );

        ToolResult {
            success: true,
            output,
            error: None,
        }
    }

    fn handle_add(&self, args: &serde_json::Value) -> ToolResult {
        let title = args
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .unwrap_or_default();
        if title.is_empty() {
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some("Parameter 'title' must be a non-empty string".into()),
            };
        }

        let mut next_id = self.next_id.write().unwrap();
        let id = *next_id;
        *next_id += 1;

        self.tasks.write().unwrap().push(TaskItem {
            id,
            title: title.to_string(),
            status: TaskStatus::Pending,
            context: optional_string(args, &["context", "sub_context"]),
            tools: string_list(args, &["tools", "allowed_tools"]),
            depends_on: usize_list(args, "depends_on"),
            expected_evidence: string_list(args, &["expected_evidence"]),
        });

        ToolResult {
            success: true,
            output: format!("Added task [{id}] \"{title}\"."),
            error: None,
        }
    }

    fn handle_update(&self, id: usize, args: &serde_json::Value) -> ToolResult {
        let status = match args.get("status").and_then(serde_json::Value::as_str) {
            Some(status_str) => match TaskStatus::from_str(status_str) {
                Some(status) => Some(status),
                None => {
                    return ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Invalid status '{status_str}'. Must be: pending, in_progress, completed, failed, blocked, skipped"
                        )),
                    };
                }
            },
            None => None,
        };
        let updates_metadata = [
            "context",
            "sub_context",
            "tools",
            "allowed_tools",
            "depends_on",
            "expected_evidence",
        ]
        .iter()
        .any(|key| args.get(*key).is_some());
        if status.is_none() && !updates_metadata {
            return ToolResult {
                success: false,
                output: String::new(),
                error: Some(
                    "Update requires at least one of status, context, tools, depends_on, or expected_evidence"
                        .into(),
                ),
            };
        }
        let mut tasks = self.tasks.write().unwrap();
        match tasks.iter_mut().find(|t| t.id == id) {
            Some(task) => {
                if let Some(status) = status {
                    task.status = status;
                }
                if args.get("context").is_some() || args.get("sub_context").is_some() {
                    task.context = optional_string(args, &["context", "sub_context"]);
                }
                if args.get("tools").is_some() || args.get("allowed_tools").is_some() {
                    task.tools = string_list(args, &["tools", "allowed_tools"]);
                }
                if args.get("depends_on").is_some() {
                    task.depends_on = usize_list(args, "depends_on");
                }
                if args.get("expected_evidence").is_some() {
                    task.expected_evidence = string_list(args, &["expected_evidence"]);
                }
                ToolResult {
                    success: true,
                    output: format!("Task [{id}] updated.\n{}", Self::render_task_list(&tasks)),
                    error: None,
                }
            }
            None => ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Task with id {id} not found")),
            },
        }
    }

    fn handle_list(&self) -> ToolResult {
        let tasks = self.tasks.read().unwrap();
        if tasks.is_empty() {
            return ToolResult {
                success: true,
                output: "No tasks.".into(),
                error: None,
            };
        }

        ToolResult {
            success: true,
            output: Self::render_task_list(&tasks),
            error: None,
        }
    }

    fn handle_delete(&self) -> ToolResult {
        self.tasks.write().unwrap().clear();
        *self.next_id.write().unwrap() = 1;

        ToolResult {
            success: true,
            output: "Task list cleared.".into(),
            error: None,
        }
    }

    fn infer_action(args: &serde_json::Value) -> &'static str {
        if args
            .get("tasks")
            .and_then(|v| v.as_array())
            .is_some_and(|tasks| !tasks.is_empty())
            || args
                .get("steps")
                .and_then(|v| v.as_array())
                .is_some_and(|steps| !steps.is_empty())
        {
            "create"
        } else if args.get("title").and_then(|v| v.as_str()).is_some() {
            "add"
        } else if args.get("id").and_then(|v| v.as_u64()).is_some() {
            "update"
        } else {
            "list"
        }
    }

    fn normalize_task_item(
        obj: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<serde_json::Value> {
        // Accept title from any commonly-used field name so models that emit
        // {task_id, description}, {name, ...}, {step, ...}, etc. all work.
        let title = obj
            .get("title")
            .or_else(|| obj.get("description"))
            .or_else(|| obj.get("name"))
            .or_else(|| obj.get("task_name"))
            .or_else(|| obj.get("step"))
            .or_else(|| obj.get("command"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|t| !t.is_empty())?;
        let mut normalized = serde_json::Map::from_iter([(
            "title".to_string(),
            serde_json::Value::String(title.to_string()),
        )]);
        if let Some(status) = obj.get("status").and_then(|value| value.as_str()) {
            normalized.insert(
                "status".to_string(),
                serde_json::Value::String(status.to_string()),
            );
        }
        let entry = serde_json::Value::Object(obj.clone());
        if let Some(context) = optional_string(&entry, &["context", "sub_context"]) {
            normalized.insert("context".to_string(), serde_json::Value::String(context));
        }
        let tools = string_list(&entry, &["tools", "allowed_tools"]);
        if !tools.is_empty() || obj.get("tools").is_some() || obj.get("allowed_tools").is_some() {
            normalized.insert("tools".to_string(), json!(tools));
        }
        for key in ["depends_on", "expected_evidence"] {
            if let Some(value) = obj.get(key) {
                normalized.insert(key.to_string(), value.clone());
            }
        }
        Some(serde_json::Value::Object(normalized))
    }

    fn normalize_create_tasks(args: &serde_json::Value) -> serde_json::Value {
        // Try `tasks` first, then `steps` as a fallback alias.
        let raw_items = args.get("tasks").or_else(|| args.get("steps"));

        // Some native-tool models serialize a large array as a JSON string
        // inside the otherwise-valid arguments object. Accept that equivalent
        // representation so a long plan does not need a corrective model turn.
        let parsed_items = raw_items
            .and_then(serde_json::Value::as_str)
            .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok());
        let items = parsed_items
            .as_ref()
            .or(raw_items)
            .and_then(serde_json::Value::as_array);

        let Some(items) = items else {
            return json!([]);
        };

        let tasks = items
            .iter()
            .filter_map(|item| item.as_object().and_then(Self::normalize_task_item))
            .collect::<Vec<_>>();

        serde_json::Value::Array(tasks)
    }
}

#[async_trait]
impl Tool for TaskPlanTool {
    fn name(&self) -> &str {
        "task_plan"
    }

    fn description(&self) -> &str {
        "Manage a task checklist for the current session. Use to break complex work into steps and track progress.\n\
         Actions: create (batch), add (single), update (change status), list (view all), delete (clear all). \
         Give each step compact context and expected tools when they improve execution fidelity. \
         Statuses: pending, in_progress, completed, failed, blocked, skipped. Mark completed only after a verifier or concrete successful result."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "add", "update", "list", "delete"],
                    "description": "Operation to perform"
                },
                "tasks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "status": {
                                "type": "string",
                                "enum": ["pending", "in_progress", "completed", "failed", "blocked", "skipped"]
                            },
                            "context": {
                                "type": "string",
                                "description": "Compact step-local goal and acceptance context"
                            },
                            "tools": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Expected tools for this step; never expands runtime permissions"
                            },
                            "depends_on": {
                                "type": "array",
                                "items": { "type": "integer", "minimum": 1 }
                            },
                            "expected_evidence": {
                                "type": "array",
                                "items": { "type": "string" }
                            }
                        },
                        "required": ["title"]
                    },
                    "description": "For 'create': list of tasks to create (replaces existing list)"
                },
                "title": {
                    "type": "string",
                    "description": "For 'add': title of the new task"
                },
                "context": {
                    "type": "string",
                    "description": "For 'add'/'update': compact step-local context"
                },
                "tools": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "For 'add'/'update': expected tools; never expands runtime permissions"
                },
                "depends_on": {
                    "type": "array",
                    "items": { "type": "integer", "minimum": 1 }
                },
                "expected_evidence": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "id": {
                    "type": "integer",
                    "description": "For 'update': ID of the task to update"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "in_progress", "completed", "failed", "blocked", "skipped"],
                    "description": "For 'update': new status"
                }
            },
            "required": ["action"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let action = args
            .get("action")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|action| !action.is_empty())
            .unwrap_or_else(|| Self::infer_action(&args));

        match action {
            "create" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                let tasks_val = Self::normalize_create_tasks(&args);
                Ok(self.handle_create(&tasks_val))
            }
            "add" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                Ok(self.handle_add(&args))
            }
            "update" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                #[allow(clippy::cast_possible_truncation)]
                let id = args.get("id").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                if id == 0 {
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some("Parameter 'id' is required for update".into()),
                    });
                }
                Ok(self.handle_update(id, &args))
            }
            "list" => Ok(self.handle_list()),
            "delete" => {
                if let Err(r) = self.enforce_mutation() {
                    return Ok(r);
                }
                Ok(self.handle_delete())
            }
            other => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Unknown action '{other}'. Valid: create, add, update, list, delete"
                )),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::AutonomyLevel;

    fn default_tool() -> TaskPlanTool {
        TaskPlanTool::new(Arc::new(SecurityPolicy::default()))
    }

    fn readonly_tool() -> TaskPlanTool {
        TaskPlanTool::new(Arc::new(SecurityPolicy {
            autonomy: AutonomyLevel::ReadOnly,
            ..SecurityPolicy::default()
        }))
    }

    #[test]
    fn tool_name_and_schema() {
        let tool = default_tool();
        assert_eq!(tool.name(), "task_plan");
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["action"].is_object());
        assert!(schema["properties"]["tasks"].is_object());
        assert!(schema["properties"]["id"].is_object());
        assert!(schema["properties"]["status"].is_object());
        assert!(schema["properties"]["context"].is_object());
        assert!(schema["properties"]["tools"].is_object());
        assert!(schema["properties"]["tasks"]["items"]["properties"]["context"].is_object());
        assert!(schema["properties"]["tasks"]["items"]["properties"]["tools"].is_object());
    }

    #[tokio::test]
    async fn create_and_list() {
        let tool = default_tool();

        let r = tool
            .execute(json!({
                "action": "create",
                "tasks": [
                    { "title": "step one" },
                    { "title": "step two" },
                    { "title": "step three", "status": "completed" }
                ]
            }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("3 task(s)"));
        assert!(r.output.contains("Tasks (1/3 completed; 1/3 resolved):"));
        assert!(r.output.contains("[1] [pending] step one"));
        assert!(r.output.contains("[3] [completed] step three"));

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("1/3 completed"));
        assert!(r.output.contains("[1] [pending] step one"));
        assert!(r.output.contains("[2] [pending] step two"));
        assert!(r.output.contains("[3] [completed] step three"));
    }

    #[tokio::test]
    async fn create_normalizes_task_id_description_schema() {
        // Regression: models sometimes emit {task_id, description} instead of {title}.
        // normalize_task_item must extract the title from description in that case.
        let tool = default_tool();

        let r = tool
            .execute(json!({
                "action": "create",
                "tasks": [
                    { "task_id": "shell_test", "description": "Run a shell command" },
                    { "task_id": "file_test",  "description": "Write a test file" }
                ]
            }))
            .await
            .unwrap();
        assert!(r.success, "failed: {}", r.output);
        assert!(
            r.output.contains("2 task(s)"),
            "unexpected output: {}",
            r.output
        );

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.output.contains("Run a shell command"));
        assert!(r.output.contains("Write a test file"));
    }

    #[tokio::test]
    async fn create_infers_action_and_normalizes_steps_alias() {
        let tool = default_tool();

        let r = tool
            .execute(json!({
                "steps": [
                    { "description": "write a file" },
                    { "description": "read the file" },
                    { "description": "delete the file" }
                ]
            }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("3 task(s)"));

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("[1] [pending] write a file"));
        assert!(r.output.contains("[2] [pending] read the file"));
        assert!(r.output.contains("[3] [pending] delete the file"));
    }

    #[tokio::test]
    async fn create_preserves_step_context_tools_and_verifier_metadata() {
        let tool = default_tool();

        let result = tool
            .execute(json!({
                "action": "create",
                "steps": [{
                    "description": "Launch and verify the app",
                    "status": "in_progress",
                    "sub_context": "Bind the container app to the host-published development port and require HTTP 200.",
                    "allowed_tools": ["shell", "http_request", "shell", ""],
                    "depends_on": [1, 1, 0],
                    "expected_evidence": ["HTTP/1.1 200"]
                }]
            }))
            .await
            .unwrap();

        assert!(result.success);
        assert!(result
            .output
            .contains("[in_progress] Launch and verify the app"));
        assert!(result
            .output
            .contains("context: Bind the container app to the host-published development port"));
        assert!(result.output.contains("tools: shell, http_request"));
        assert!(result.output.contains("depends_on: 1"));
        assert!(result.output.contains("evidence: HTTP/1.1 200"));
    }

    #[tokio::test]
    async fn add_task() {
        let tool = default_tool();

        // Create initial list
        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "first" }]
        }))
        .await
        .unwrap();

        // Add a task — should get id=2
        let r = tool
            .execute(json!({ "action": "add", "title": "second" }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("[2]"));

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.output.contains("[1] [pending] first"));
        assert!(r.output.contains("[2] [pending] second"));
    }

    #[tokio::test]
    async fn add_and_update_preserve_unspecified_metadata_and_allow_clearing_tools() {
        let tool = default_tool();
        let added = tool
            .execute(json!({
                "action": "add",
                "title": "Verify service",
                "context": "Check the published URL from outside the container.",
                "tools": ["shell", "http_request"]
            }))
            .await
            .unwrap();
        assert!(added.success);

        let updated = tool
            .execute(json!({
                "action": "update",
                "id": 1,
                "status": "in_progress"
            }))
            .await
            .unwrap();
        assert!(updated.output.contains("context: Check the published URL"));
        assert!(updated.output.contains("tools: shell, http_request"));

        let cleared = tool
            .execute(json!({
                "action": "update",
                "id": 1,
                "tools": []
            }))
            .await
            .unwrap();
        assert!(cleared.success);
        assert!(!cleared.output.contains("tools:"));
        assert!(cleared.output.contains("context: Check the published URL"));
    }

    #[tokio::test]
    async fn update_status() {
        let tool = default_tool();

        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "do thing" }]
        }))
        .await
        .unwrap();

        let r = tool
            .execute(json!({ "action": "update", "id": 1, "status": "in_progress" }))
            .await
            .unwrap();
        assert!(r.success);
        assert!(r.output.contains("in_progress"));

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.output.contains("[in_progress]"));
    }

    #[tokio::test]
    async fn audit_terminal_statuses_remain_visible() {
        let tool = default_tool();
        tool.execute(json!({
            "action": "create",
            "tasks": [
                { "title": "verified tool" },
                { "title": "missing credential" },
                { "title": "unsupported integration" }
            ]
        }))
        .await
        .unwrap();

        for (id, status) in [(1, "completed"), (2, "blocked"), (3, "failed")] {
            let result = tool
                .execute(json!({ "action": "update", "id": id, "status": status }))
                .await
                .unwrap();
            assert!(result.success, "status {status} should be accepted");
        }

        let result = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(result
            .output
            .contains("Tasks (1/3 completed; 3/3 resolved):"));
        assert!(result.output.contains("[blocked] missing credential"));
        assert!(result.output.contains("[failed] unsupported integration"));
    }

    #[tokio::test]
    async fn update_nonexistent_id() {
        let tool = default_tool();

        let r = tool
            .execute(json!({ "action": "update", "id": 999, "status": "completed" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("not found"));
    }

    #[tokio::test]
    async fn create_replaces_existing() {
        let tool = default_tool();

        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "old task" }]
        }))
        .await
        .unwrap();

        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "new task" }]
        }))
        .await
        .unwrap();

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(!r.output.contains("old task"));
        assert!(r.output.contains("new task"));
        // ID should reset to 1
        assert!(r.output.contains("[1]"));
    }

    #[tokio::test]
    async fn delete_clears_all() {
        let tool = default_tool();

        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "will be deleted" }]
        }))
        .await
        .unwrap();

        let r = tool.execute(json!({ "action": "delete" })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("cleared"));

        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.output.contains("No tasks"));
    }

    #[tokio::test]
    async fn readonly_blocks_mutations() {
        let tool = readonly_tool();

        for action in &["create", "add", "update", "delete"] {
            let mut args = json!({ "action": action });
            if *action == "create" {
                args["tasks"] = json!([{ "title": "t" }]);
            }
            if *action == "add" {
                args["title"] = json!("t");
            }
            if *action == "update" {
                args["id"] = json!(1);
                args["status"] = json!("completed");
            }
            let r = tool.execute(args).await.unwrap();
            assert!(
                !r.success,
                "action '{action}' should be blocked in read-only"
            );
            assert!(r.error.unwrap().contains("read-only"));
        }
    }

    #[tokio::test]
    async fn list_works_in_readonly() {
        let tool = readonly_tool();
        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.success);
    }

    #[tokio::test]
    async fn unknown_action_returns_failure() {
        let tool = default_tool();
        let r = tool.execute(json!({ "action": "nope" })).await.unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("Unknown action"));
    }

    #[tokio::test]
    async fn create_with_empty_tasks_fails() {
        let tool = default_tool();
        let r = tool
            .execute(json!({ "action": "create", "tasks": [] }))
            .await
            .unwrap();
        assert!(!r.success);
    }

    #[tokio::test]
    async fn update_missing_params_fails() {
        let tool = default_tool();

        // Missing id
        let r = tool
            .execute(json!({ "action": "update", "status": "completed" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("id"));

        // Missing status
        let r = tool
            .execute(json!({ "action": "update", "id": 1 }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("status"));
    }

    #[tokio::test]
    async fn invalid_status_value_fails() {
        let tool = default_tool();
        tool.execute(json!({
            "action": "create",
            "tasks": [{ "title": "t" }]
        }))
        .await
        .unwrap();

        let r = tool
            .execute(json!({ "action": "update", "id": 1, "status": "invalid" }))
            .await
            .unwrap();
        assert!(!r.success);
        assert!(r.error.unwrap().contains("Invalid status"));
    }

    #[tokio::test]
    async fn add_empty_title_fails() {
        let tool = default_tool();
        let r = tool
            .execute(json!({ "action": "add", "title": "" }))
            .await
            .unwrap();
        assert!(!r.success);
    }

    #[tokio::test]
    async fn list_empty_shows_no_tasks() {
        let tool = default_tool();
        let r = tool.execute(json!({ "action": "list" })).await.unwrap();
        assert!(r.success);
        assert!(r.output.contains("No tasks"));
    }
}
