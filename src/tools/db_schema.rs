use super::traits::{Tool, ToolResult};
use crate::config::DbConnectionConfig;
use crate::db::build_adapter;
use async_trait::async_trait;
use serde_json::json;

/// Retrieve the schema (tables/collections and their columns) for a configured database.
pub struct DbSchemaTool {
    connections: Vec<DbConnectionConfig>,
}

impl DbSchemaTool {
    pub fn new(connections: Vec<DbConnectionConfig>) -> Self {
        Self { connections }
    }
}

#[async_trait]
impl Tool for DbSchemaTool {
    fn name(&self) -> &str {
        "db_schema"
    }

    fn description(&self) -> &str {
        "Retrieve the schema (tables/collections and columns) for configured database connections. \
         Only use this when you need to discover an unknown database structure. \
         Skip this tool when the db_query description already tells you the collection name — \
         calling db_schema before every db_query wastes a round trip."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let names: Vec<String> = self.connections.iter().map(|c| c.name.clone()).collect();
        let descs: Vec<String> = self
            .connections
            .iter()
            .map(|c| {
                format!(
                    "{} ({:?}{})",
                    c.name,
                    c.driver,
                    c.database.as_deref().map(|d| format!(" / {d}")).unwrap_or_default()
                )
            })
            .collect();
        json!({
            "type": "object",
            "properties": {
                "connection": {
                    "type": "string",
                    "description": format!(
                        "Name of a specific connection to inspect. Omit to get all schemas at once. Available: {}",
                        descs.join(", ")
                    ),
                    "enum": names
                }
            }
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let conn_name = args["connection"].as_str().filter(|n| !n.is_empty());

        // No connection specified — return schemas for all connections at once.
        if conn_name.is_none() {
            if self.connections.is_empty() {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some("No database connections configured.".to_string()),
                });
            }
            let mut all_out = String::new();
            let mut any_error = false;
            for conn_cfg in &self.connections {
                all_out.push_str(&format!(
                    "=== {} ({}{}) ===\n",
                    conn_cfg.name,
                    conn_cfg.label.as_deref().unwrap_or(conn_cfg.name.as_str()),
                    conn_cfg.database.as_deref().map(|d| format!(" / db:{d}")).unwrap_or_default(),
                ));
                match build_adapter(conn_cfg) {
                    Err(e) => {
                        all_out.push_str(&format!("  [connect error: {e}]\n\n"));
                        any_error = true;
                    }
                    Ok(adapter) => match adapter.schema().await {
                        Err(e) => {
                            all_out.push_str(&format!("  [schema error: {e}]\n\n"));
                            any_error = true;
                        }
                        Ok(schema) => {
                            if schema.tables.is_empty() {
                                all_out.push_str("  No tables or collections found.\n\n");
                            } else {
                                for table in &schema.tables {
                                    all_out.push_str(&format!("  {} ({}):\n", table.name, table.kind));
                                    for col in &table.columns {
                                        all_out.push_str(&format!("    {} {}\n", col.name, col.data_type));
                                    }
                                }
                                all_out.push('\n');
                            }
                        }
                    },
                }
            }
            return Ok(ToolResult {
                success: !any_error,
                output: all_out,
                error: None,
            });
        }

        let conn_name = conn_name.unwrap();
        let conn_cfg = match self.connections.iter().find(|c| c.name == conn_name) {
            Some(c) => c,
            None => {
                let available: Vec<&str> = self.connections.iter().map(|c| c.name.as_str()).collect();
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!(
                        "Unknown connection '{}'. Available: {}",
                        conn_name,
                        available.join(", ")
                    )),
                });
            }
        };

        let adapter: Box<dyn crate::db::DbAdapter> = match build_adapter(conn_cfg) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to connect to '{}': {e}", conn_cfg.name)),
                })
            }
        };

        match adapter.schema().await {
            Ok(schema) => {
                let mut out = format!(
                    "Schema for '{}' ({}){}:\n\n",
                    conn_name,
                    schema.driver,
                    schema.database.as_deref().map(|d| format!(" / {d}")).unwrap_or_default()
                );

                if schema.tables.is_empty() {
                    out.push_str("No tables or collections found.\n");
                } else {
                    for table in &schema.tables {
                        out.push_str(&format!("{} ({}):\n", table.name, table.kind));
                        for col in &table.columns {
                            out.push_str(&format!("  {} {}\n", col.name, col.data_type));
                        }
                        out.push('\n');
                    }
                }

                Ok(ToolResult {
                    success: true,
                    output: out,
                    error: None,
                })
            }
            Err(e) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Schema fetch failed for '{}': {e}", conn_name)),
            }),
        }
    }
}
