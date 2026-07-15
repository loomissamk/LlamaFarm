use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Result of a tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    pub success: bool,
    pub output: String,
    pub error: Option<String>,
}

/// Description of a tool for the LLM
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Core tool trait — implement for any capability
#[async_trait]
pub trait Tool: Send + Sync {
    /// Tool name (used in LLM function calling)
    fn name(&self) -> &str;

    /// Human-readable description
    fn description(&self) -> &str;

    /// JSON schema for parameters
    fn parameters_schema(&self) -> serde_json::Value;

    /// True if this tool never mutates state.
    /// Enables result caching and concurrent execution with other read-only tools.
    /// Conservative default: false (assume write).
    fn is_read_only(&self) -> bool {
        false
    }

    /// True if this tool can safely run concurrently with other concurrency-safe tools.
    /// Defaults to is_read_only() — write tools always serialize.
    fn is_concurrency_safe(&self) -> bool {
        self.is_read_only()
    }

    /// Output size threshold in bytes. Outputs larger than this are persisted to a
    /// temp file and the model receives a preview + path instead of the raw blob.
    /// Use usize::MAX to disable persistence (e.g. file_read avoids a circular dep).
    fn max_output_bytes(&self) -> usize {
        100 * 1024 // 100 KB
    }

    /// Execute the tool with given arguments
    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult>;

    /// Get the full spec for LLM registration
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: self.name().to_string(),
            description: self.description().to_string(),
            parameters: self.parameters_schema(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy_tool"
        }

        fn description(&self) -> &str {
            "A deterministic test tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "value": { "type": "string" }
                }
            })
        }

        async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: args
                    .get("value")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                error: None,
            })
        }
    }

    struct ReadOnlyTool;

    #[async_trait]
    impl Tool for ReadOnlyTool {
        fn name(&self) -> &str {
            "ro_tool"
        }
        fn description(&self) -> &str {
            "read-only stub"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        fn is_read_only(&self) -> bool {
            true
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    #[test]
    fn spec_uses_tool_metadata_and_schema() {
        let tool = DummyTool;
        let spec = tool.spec();

        assert_eq!(spec.name, "dummy_tool");
        assert_eq!(spec.description, "A deterministic test tool");
        assert_eq!(spec.parameters["type"], "object");
        assert_eq!(spec.parameters["properties"]["value"]["type"], "string");
    }

    #[tokio::test]
    async fn execute_returns_expected_output() {
        let tool = DummyTool;
        let result = tool
            .execute(serde_json::json!({ "value": "hello-tool" }))
            .await
            .unwrap();

        assert!(result.success);
        assert_eq!(result.output, "hello-tool");
        assert!(result.error.is_none());
    }

    #[test]
    fn tool_result_serialization_roundtrip() {
        let result = ToolResult {
            success: false,
            output: String::new(),
            error: Some("boom".into()),
        };

        let json = serde_json::to_string(&result).unwrap();
        let parsed: ToolResult = serde_json::from_str(&json).unwrap();

        assert!(!parsed.success);
        assert_eq!(parsed.error.as_deref(), Some("boom"));
    }

    #[test]
    fn write_tool_defaults_not_read_only_not_concurrency_safe() {
        let tool = DummyTool;
        assert!(!tool.is_read_only());
        assert!(!tool.is_concurrency_safe());
        assert_eq!(tool.max_output_bytes(), 100 * 1024);
    }

    #[test]
    fn read_only_tool_is_also_concurrency_safe() {
        let tool = ReadOnlyTool;
        assert!(tool.is_read_only());
        assert!(tool.is_concurrency_safe());
    }
}
