use super::{ColumnInfo, DbAdapter, DbSchema, QueryResult, TableInfo};
use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tokio_postgres::NoTls;

pub struct PostgresAdapter {
    uri: String,
    read_only: bool,
}

impl PostgresAdapter {
    pub fn new(uri: String, read_only: bool) -> Self {
        Self { uri, read_only }
    }
}

fn is_read_only_sql(sql: &str) -> bool {
    let t = sql.trim().to_ascii_uppercase();
    t.starts_with("SELECT")
        || t.starts_with("EXPLAIN")
        || t.starts_with("SHOW")
        || t.starts_with("WITH")
        || t.starts_with("TABLE ")
        || t.starts_with("\\")
}

fn pg_col_to_json(row: &tokio_postgres::Row, i: usize) -> Value {
    use tokio_postgres::types::Type;
    let col_type = row.columns()[i].type_().clone();
    match col_type {
        Type::BOOL => row.try_get::<_, bool>(i).map(Value::Bool).unwrap_or(Value::Null),
        Type::INT2 => row
            .try_get::<_, i16>(i)
            .map(|v| Value::Number(v.into()))
            .unwrap_or(Value::Null),
        Type::INT4 => row
            .try_get::<_, i32>(i)
            .map(|v| Value::Number(v.into()))
            .unwrap_or(Value::Null),
        Type::INT8 | Type::OID => row
            .try_get::<_, i64>(i)
            .map(|v| Value::Number(v.into()))
            .unwrap_or(Value::Null),
        Type::FLOAT4 => row
            .try_get::<_, f32>(i)
            .ok()
            .and_then(|v| serde_json::Number::from_f64(f64::from(v)).map(Value::Number))
            .unwrap_or(Value::Null),
        Type::FLOAT8 | Type::NUMERIC => row
            .try_get::<_, f64>(i)
            .ok()
            .and_then(|v| serde_json::Number::from_f64(v).map(Value::Number))
            .unwrap_or(Value::Null),
        Type::TEXT | Type::VARCHAR | Type::NAME | Type::BPCHAR => {
            row.try_get::<_, String>(i).map(Value::String).unwrap_or(Value::Null)
        }
        _ => {
            // Fall back to string representation for all other types
            row.try_get::<_, String>(i)
                .map(Value::String)
                .unwrap_or_else(|_| Value::String(format!("<{}>", col_type.name())))
        }
    }
}

#[async_trait]
impl DbAdapter for PostgresAdapter {
    fn driver_name(&self) -> &str {
        "postgres"
    }

    async fn schema(&self) -> Result<DbSchema> {
        let (client, conn) = tokio_postgres::connect(&self.uri, NoTls).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });

        let rows = client
            .query(
                "SELECT table_name, table_type \
                 FROM information_schema.tables \
                 WHERE table_schema = 'public' \
                 ORDER BY table_name",
                &[],
            )
            .await?;

        let db: Option<String> = client
            .query_one("SELECT current_database()", &[])
            .await
            .map(|r| r.get::<_, String>(0))
            .ok();

        let mut tables = Vec::new();
        for row in rows {
            let tname: String = row.get(0);
            let ttype: String = row.get(1);
            let kind = if ttype == "VIEW" { "view" } else { "table" }.to_string();

            let col_rows = client
                .query(
                    "SELECT column_name, data_type \
                     FROM information_schema.columns \
                     WHERE table_schema = 'public' AND table_name = $1 \
                     ORDER BY ordinal_position",
                    &[&tname],
                )
                .await?;

            let columns: Vec<ColumnInfo> = col_rows
                .iter()
                .map(|r| ColumnInfo {
                    name: r.get(0),
                    data_type: r.get(1),
                })
                .collect();

            tables.push(TableInfo {
                name: tname,
                columns,
                kind,
            });
        }

        Ok(DbSchema {
            driver: "postgres".into(),
            database: db,
            tables,
        })
    }

    async fn query(&self, sql: &str, max_rows: usize) -> Result<QueryResult> {
        if self.read_only && !is_read_only_sql(sql) {
            anyhow::bail!(
                "Connection is read-only; only SELECT/EXPLAIN/SHOW/WITH queries are allowed"
            );
        }

        let (client, conn) = tokio_postgres::connect(&self.uri, NoTls).await?;
        tokio::spawn(async move {
            let _ = conn.await;
        });

        if self.read_only {
            let _ = client
                .execute("SET LOCAL default_transaction_read_only = on", &[])
                .await;
        }

        let rows = client.query(sql, &[]).await?;

        if rows.is_empty() {
            return Ok(QueryResult {
                columns: Vec::new(),
                rows: Vec::new(),
                row_count: 0,
                truncated: false,
            });
        }

        let columns: Vec<String> =
            rows[0].columns().iter().map(|c| c.name().to_string()).collect();
        let truncated = rows.len() > max_rows;

        let row_data: Vec<Vec<Value>> = rows
            .iter()
            .take(max_rows)
            .map(|row| (0..columns.len()).map(|i| pg_col_to_json(row, i)).collect())
            .collect();

        let row_count = row_data.len();
        Ok(QueryResult {
            columns,
            rows: row_data,
            row_count,
            truncated,
        })
    }
}
