use super::{ColumnInfo, DbAdapter, DbSchema, QueryResult, TableInfo};
use anyhow::Result;
use async_trait::async_trait;
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;

pub struct SqliteAdapter {
    path: String,
    read_only: bool,
}

impl SqliteAdapter {
    pub fn new(path: &str, read_only: bool) -> Result<Self> {
        let flags = open_flags(read_only);
        // Validate the file is openable at construction time.
        Connection::open_with_flags(path, flags)
            .map_err(|e| anyhow::anyhow!("Cannot open SQLite '{}': {}", path, e))?;
        Ok(Self {
            path: path.to_string(),
            read_only,
        })
    }
}

fn open_flags(read_only: bool) -> OpenFlags {
    if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
    }
}

fn is_read_only_sql(sql: &str) -> bool {
    let t = sql.trim().to_ascii_uppercase();
    t.starts_with("SELECT")
        || t.starts_with("EXPLAIN")
        || t.starts_with("PRAGMA")
        || t.starts_with("WITH")
}

fn value_ref_to_json(v: rusqlite::types::ValueRef<'_>) -> Value {
    use rusqlite::types::ValueRef;
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => Value::Number(i.into()),
        ValueRef::Real(f) => serde_json::Number::from_f64(f)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        ValueRef::Text(t) => Value::String(String::from_utf8_lossy(t).into_owned()),
        ValueRef::Blob(b) => Value::String(format!("<BLOB {} bytes>", b.len())),
    }
}

#[async_trait]
impl DbAdapter for SqliteAdapter {
    fn driver_name(&self) -> &str {
        "sqlite"
    }

    async fn schema(&self) -> Result<DbSchema> {
        let path = self.path.clone();
        let read_only = self.read_only;
        tokio::task::spawn_blocking(move || {
            let conn = Connection::open_with_flags(&path, open_flags(read_only))?;
            let mut stmt = conn.prepare(
                "SELECT name, type FROM sqlite_master \
                 WHERE type IN ('table','view') ORDER BY name",
            )?;

            let entries: Vec<(String, String)> = stmt
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                .filter_map(|r| r.ok())
                .collect();

            let mut tables = Vec::new();
            for (tname, kind) in entries {
                let safe = tname.replace('\'', "''");
                let mut col_stmt =
                    conn.prepare(&format!("PRAGMA table_info('{safe}')"))?;
                let columns: Vec<ColumnInfo> = col_stmt
                    .query_map([], |row| {
                        Ok(ColumnInfo {
                            name: row.get::<_, String>(1)?,
                            data_type: row.get::<_, String>(2).unwrap_or_default(),
                        })
                    })?
                    .filter_map(|r| r.ok())
                    .collect();
                tables.push(TableInfo {
                    name: tname,
                    columns,
                    kind,
                });
            }

            Ok(DbSchema {
                driver: "sqlite".into(),
                database: Some(path),
                tables,
            })
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))?
    }

    async fn query(&self, sql: &str, max_rows: usize) -> Result<QueryResult> {
        if self.read_only && !is_read_only_sql(sql) {
            anyhow::bail!(
                "Connection is read-only; only SELECT/EXPLAIN/PRAGMA/WITH queries are allowed"
            );
        }

        let path = self.path.clone();
        let sql = sql.to_string();
        let read_only = self.read_only;

        tokio::task::spawn_blocking(move || {
            let conn = Connection::open_with_flags(&path, open_flags(read_only))?;
            let mut stmt = conn.prepare(&sql)?;
            let col_count = stmt.column_count();
            let columns: Vec<String> = (0..col_count)
                .map(|i| stmt.column_name(i).unwrap_or("?").to_string())
                .collect();

            let mut rows: Vec<Vec<Value>> = Vec::new();
            let mut result_rows = stmt.query([])?;
            let mut truncated = false;

            while let Some(row) = result_rows.next()? {
                if rows.len() >= max_rows {
                    truncated = true;
                    break;
                }
                let values: Vec<Value> =
                    (0..col_count).map(|i| value_ref_to_json(row.get_ref_unwrap(i))).collect();
                rows.push(values);
            }

            let row_count = rows.len();
            Ok(QueryResult {
                columns,
                rows,
                row_count,
                truncated,
            })
        })
        .await
        .map_err(|e| anyhow::anyhow!("spawn_blocking join error: {e}"))?
    }
}
