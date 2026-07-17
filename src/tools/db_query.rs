use super::traits::{Tool, ToolResult};
use crate::config::DbConnectionConfig;
use crate::db::build_adapter;
use async_trait::async_trait;
use serde_json::json;

/// Execute a query against a configured database connection.
pub struct DbQueryTool {
    connections: Vec<DbConnectionConfig>,
    description: String,
}

impl DbQueryTool {
    pub fn new(connections: Vec<DbConnectionConfig>) -> Self {
        let desc = Self::build_description(&connections);
        Self {
            connections,
            description: desc,
        }
    }

    fn build_description(connections: &[DbConnectionConfig]) -> String {
        use crate::config::DbDriver;
        let parts: Vec<String> = connections.iter().map(|c| {
            match c.driver {
                DbDriver::Mongodb => format!(
                    "'{}' (MongoDB/{label}): Use this tool — NOT shell/curl — for any ArXiv paper queries. \
                     This database contains 3.5M ArXiv papers. \
                     JSON fields: collection (required), filter, projection, sort, limit. \
                     ALWAYS include projection to select only needed fields — omitting it returns huge blobs. \
                     Indexed fields (ONLY these are fast): _id, id, categories, $text (title+abstract). \
                     Sort ONLY by {{\"_id\":-1}} for newest-first — sorting by date/submittedDate has no index and will time out. \
                     Two fast query patterns: \
                     (1) Full-text search on title+abstract: {{\"collection\":\"Papers\",\
                     \"filter\":{{\"$text\":{{\"$search\":\"transformer attention\"}}}},\
                     \"projection\":{{\"title\":1,\"abstract\":1,\"categories\":1,\"_id\":0}},\"limit\":5}} \
                     (2) Category filter (stored as strings like \"['cs.AI', 'cs.LG']\") — use $regex: \
                     {{\"collection\":\"Papers\",\"filter\":{{\"categories\":{{\"$regex\":\"cs.AI\"}}}},\
                     \"projection\":{{\"title\":1,\"categories\":1,\"_id\":0}},\"sort\":{{\"_id\":-1}},\"limit\":5}}",
                    c.name,
                    label = c.label.as_deref().unwrap_or(c.database.as_deref().unwrap_or("db"))
                ),
                DbDriver::Sqlite | DbDriver::Postgres | DbDriver::Mysql => format!(
                    "'{}' (SQL): pass a SELECT statement — e.g. SELECT col1, col2 FROM schema.table LIMIT 10",
                    c.name
                ),
            }
        }).collect();
        format!(
            "Run a query against a database. Call this directly — you do NOT need to call db_schema first, \
             the connection and collection info is already in this description. \
             NEVER use SQL syntax for MongoDB connections. {}",
            parts.join(". ")
        )
    }

    fn find_conn(&self, name: &str) -> Option<&DbConnectionConfig> {
        self.connections.iter().find(|c| c.name == name)
    }
}

#[async_trait]
impl Tool for DbQueryTool {
    fn name(&self) -> &str {
        "db_query"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters_schema(&self) -> serde_json::Value {
        let names: Vec<String> = self.connections.iter().map(|c| c.name.clone()).collect();
        let only_one = names.len() == 1;
        let conn_desc = if only_one {
            format!(
                "Name of the database connection to query. Use \"{}\"",
                names[0]
            )
        } else {
            format!(
                "Name of the database connection to query. Available: {}",
                names.join(", ")
            )
        };
        let mut schema = json!({
            "type": "object",
            "properties": {
                "connection": {
                    "type": "string",
                    "description": conn_desc,
                    "enum": names
                },
                "collection": {
                    "type": "string",
                    "description": "MongoDB only: the collection name (e.g. 'Papers'). Can be provided here or inside the query JSON."
                },
                "query": {
                    "type": ["string", "object"],
                    "description": "For MongoDB: JSON with filter, projection, sort, limit — e.g. {\"filter\":{\"$text\":{\"$search\":\"attention\"}},\"projection\":{\"title\":1,\"abstract\":1,\"_id\":0},\"limit\":5}. Collection can be included here or as the top-level 'collection' param. For SQL: SELECT statement."
                },
                "max_rows": {
                    "type": "integer",
                    "description": "Maximum rows to return (default: connection's max_rows setting)"
                }
            },
            "required": if only_one { json!(["query"]) } else { json!(["connection","query"]) }
        });
        if only_one {
            schema["required"] = json!(["query"]);
        }
        schema
    }

    fn is_read_only(&self) -> bool {
        self.connections.iter().all(|c| c.read_only)
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        // Accept "connection", "database", or "db" as the connection name field
        let conn_name_raw = ["connection", "database", "db"]
            .iter()
            .find_map(|k| args[k].as_str().filter(|s| !s.trim().is_empty()));
        let conn_name = match conn_name_raw {
            Some(n) => n,
            None => {
                if self.connections.len() == 1 {
                    self.connections[0].name.as_str()
                } else {
                    let available: Vec<&str> =
                        self.connections.iter().map(|c| c.name.as_str()).collect();
                    return Ok(ToolResult {
                        success: false,
                        output: String::new(),
                        error: Some(format!(
                            "Missing 'connection' parameter. Available: {}",
                            available.join(", ")
                        )),
                    });
                }
            }
        };

        let conn_cfg = match self.find_conn(conn_name) {
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

        // Accept query as a string OR as a JSON object (models sometimes pass the object directly).
        // Also fall back to any other non-empty string value in args.
        let query_owned: String;
        let query_injected: String;
        let query: &str = {
            let raw: &str = if let Some(s) = args["query"].as_str().filter(|s| !s.trim().is_empty())
            {
                s
            } else if args["query"].is_object() || args["query"].is_array() {
                query_owned = args["query"].to_string();
                &query_owned
            } else {
                // Models sometimes pass MongoDB args flat (filter, projection, sort, limit)
                // as top-level params instead of wrapping them in a "query" object.
                // Detect this and build the query object automatically.
                const MONGO_FLAT_KEYS: &[&str] =
                    &["filter", "projection", "sort", "limit", "pipeline"];
                let flat_obj = args.as_object().and_then(|m| {
                    let has_mongo_keys = m.keys().any(|k| MONGO_FLAT_KEYS.contains(&k.as_str()));
                    if has_mongo_keys {
                        let mut q = serde_json::Map::new();
                        for key in MONGO_FLAT_KEYS {
                            if let Some(v) = m.get(*key) {
                                if !v.is_null() {
                                    q.insert(key.to_string(), v.clone());
                                }
                            }
                        }
                        Some(serde_json::Value::Object(q))
                    } else {
                        None
                    }
                });
                if let Some(flat_q) = flat_obj {
                    query_owned = flat_q.to_string();
                    &query_owned
                } else {
                    // Last resort: pick any non-empty string value from args (covers alternate key names)
                    let fallback = args.as_object().and_then(|m| {
                        m.iter()
                            .filter(|(k, _)| {
                                !["connection", "database", "db", "max_rows", "collection"]
                                    .contains(&k.as_str())
                            })
                            .find_map(|(_, v)| v.as_str().filter(|s| !s.trim().is_empty()))
                    });
                    match fallback {
                        Some(s) => s,
                        None => return Ok(ToolResult {
                            success: false,
                            output: String::new(),
                            error: Some("Missing or empty 'query' parameter. For MongoDB use JSON: {\"collection\":\"Papers\",\"filter\":{},\"limit\":5}".into()),
                        }),
                    }
                }
            };

            // For MongoDB: if top-level 'collection' arg was provided and the query JSON
            // lacks a 'collection' field, inject it so the model doesn't have to repeat it.
            use crate::config::DbDriver;
            if matches!(conn_cfg.driver, DbDriver::Mongodb) {
                if let Some(coll) = args["collection"].as_str().filter(|s| !s.trim().is_empty()) {
                    if let Ok(mut q) = serde_json::from_str::<serde_json::Value>(raw) {
                        if q.get("collection")
                            .map(|v| {
                                v.is_null() || v.as_str().map(|s| s.is_empty()).unwrap_or(false)
                            })
                            .unwrap_or(true)
                        {
                            q["collection"] = json!(coll);
                            query_injected = q.to_string();
                            &query_injected
                        } else {
                            raw
                        }
                    } else {
                        raw
                    }
                } else {
                    raw
                }
            } else {
                raw
            }
        };

        let max_rows = args["max_rows"]
            .as_u64()
            .map(|v| v as usize)
            .unwrap_or(conn_cfg.max_rows)
            .min(conn_cfg.max_rows);

        let adapter: Box<dyn crate::db::DbAdapter> = match build_adapter(conn_cfg) {
            Ok(a) => a,
            Err(e) => {
                return Ok(ToolResult {
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to connect to '{}': {e}", conn_cfg.name)),
                });
            }
        };

        match adapter.query(query, max_rows).await {
            Ok(result) => {
                if result.row_count == 0 {
                    return Ok(ToolResult {
                        success: true,
                        output: "Query returned no rows.".into(),
                        error: None,
                    });
                }

                let mut out = format!(
                    "Query returned {} row{}{} from '{}':\n\n",
                    result.row_count,
                    if result.row_count == 1 { "" } else { "s" },
                    if result.truncated { " (truncated)" } else { "" },
                    conn_name
                );

                // Header row
                out.push_str(&result.columns.join(" | "));
                out.push('\n');
                out.push_str(&"-".repeat(out.lines().last().map(|l| l.len()).unwrap_or(40)));
                out.push('\n');

                for row in &result.rows {
                    let cells: Vec<String> = row
                        .iter()
                        .map(|v| match v {
                            serde_json::Value::Null => "NULL".to_string(),
                            serde_json::Value::String(s) => {
                                if s.len() > 200 {
                                    format!("{}…", &s[..200])
                                } else {
                                    s.clone()
                                }
                            }
                            other => other.to_string(),
                        })
                        .collect();
                    out.push_str(&cells.join(" | "));
                    out.push('\n');
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
                error: Some(format!("Query failed on '{}': {e}", conn_name)),
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DbConnectionConfig, DbDriver};

    fn make_tool() -> DbQueryTool {
        DbQueryTool::new(vec![DbConnectionConfig {
            name: "test".into(),
            driver: DbDriver::Sqlite,
            uri: ":memory:".into(),
            database: None,
            read_only: false,
            max_rows: 100,
            label: None,
        }])
    }

    #[test]
    fn tool_name() {
        assert_eq!(make_tool().name(), "db_query");
    }

    #[tokio::test]
    async fn missing_connection_returns_error() {
        let tool = make_tool();
        let result = tool
            .execute(json!({"connection": "nope", "query": "SELECT 1"}))
            .await
            .unwrap();
        assert!(!result.success);
    }

    #[tokio::test]
    async fn missing_query_returns_error() {
        let tool = make_tool();
        let result = tool
            .execute(json!({"connection": "test", "query": ""}))
            .await
            .unwrap();
        assert!(!result.success);
    }
}
