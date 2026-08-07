use super::traits::{Tool, ToolResult};
use crate::config::DbConnectionConfig;
use crate::db::{build_adapter, sanitize_connection_error};
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
                    c.database
                        .as_deref()
                        .map(|d| format!(" / {d}"))
                        .unwrap_or_default()
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
            // Probe independently and concurrently. One stale remote connection
            // must not hold the entire catalogue/schema view hostage.
            let probes = futures_util::future::join_all(self.connections.iter().map(
                |conn_cfg| async move {
                    let result = match build_adapter(conn_cfg) {
                        Err(error) => Err(format!(
                            "connect error: {}",
                            sanitize_connection_error(&error, &conn_cfg.uri)
                        )),
                        Ok(adapter) => match tokio::time::timeout(
                            std::time::Duration::from_secs(5),
                            adapter.schema(),
                        )
                        .await
                        {
                            Ok(Ok(schema)) => Ok(schema),
                            Ok(Err(error)) => Err(format!(
                                "schema error: {}",
                                sanitize_connection_error(&error, &conn_cfg.uri)
                            )),
                            Err(_) => Err("schema error: probe timed out after 5s".to_string()),
                        },
                    };
                    (conn_cfg, result)
                },
            ))
            .await;

            let mut all_out = String::new();
            let mut any_success = false;
            for (conn_cfg, result) in probes {
                all_out.push_str(&format!(
                    "=== {} ({}{}) ===\n",
                    conn_cfg.name,
                    conn_cfg.label.as_deref().unwrap_or(conn_cfg.name.as_str()),
                    conn_cfg
                        .database
                        .as_deref()
                        .map(|d| format!(" / db:{d}"))
                        .unwrap_or_default(),
                ));
                match result {
                    Err(error) => all_out.push_str(&format!("  [{error}]\n\n")),
                    Ok(schema) => {
                        any_success = true;
                        if schema.tables.is_empty() {
                            all_out.push_str("  No tables or collections found.\n\n");
                        } else {
                            for table in &schema.tables {
                                all_out.push_str(&format!("  {} ({}):\n", table.name, table.kind));
                                for col in &table.columns {
                                    all_out
                                        .push_str(&format!("    {} {}\n", col.name, col.data_type));
                                }
                            }
                            all_out.push('\n');
                        }
                    }
                }
            }
            return Ok(ToolResult {
                // Aggregate discovery is useful when at least one configured
                // database responds; individual failures remain visible inline.
                success: any_success,
                output: all_out,
                error: None,
            });
        }

        let conn_name = conn_name.unwrap();
        let conn_cfg = match self.connections.iter().find(|c| c.name == conn_name) {
            Some(c) => c,
            None => {
                let available: Vec<&str> =
                    self.connections.iter().map(|c| c.name.as_str()).collect();
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
                    error: Some(format!(
                        "Failed to connect to '{}': {}",
                        conn_cfg.name,
                        sanitize_connection_error(&e, &conn_cfg.uri)
                    )),
                });
            }
        };

        match adapter.schema().await {
            Ok(schema) => {
                let mut out = format!(
                    "Schema for '{}' ({}){}:\n\n",
                    conn_name,
                    schema.driver,
                    schema
                        .database
                        .as_deref()
                        .map(|d| format!(" / {d}"))
                        .unwrap_or_default()
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
                error: Some(format!(
                    "Schema fetch failed for '{}': {}",
                    conn_name,
                    sanitize_connection_error(&e, &conn_cfg.uri)
                )),
            }),
        }
    }
}
