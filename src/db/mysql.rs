use super::{ColumnInfo, DbAdapter, DbSchema, QueryResult, TableInfo};
use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::Engine;
use mysql::prelude::Queryable;
use mysql::{AccessMode, Opts, Pool, PooledConn, TxOpts, Value as MysqlValue};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

pub struct MysqlAdapter {
    uri: String,
    read_only: bool,
}

impl MysqlAdapter {
    pub fn new(uri: String, read_only: bool) -> Self {
        Self { uri, read_only }
    }
}

fn connect(uri: &str) -> Result<PooledConn> {
    let options = Opts::from_url(uri).context("invalid MySQL/MariaDB connection URI")?;
    let pool = Pool::new(options).context("failed to create MySQL/MariaDB connection pool")?;
    pool.get_conn()
        .context("failed to connect to MySQL/MariaDB")
}

struct MysqlCancellationGuard {
    uri: String,
    cancelled: Arc<AtomicBool>,
    connection_id: Arc<Mutex<Option<u64>>>,
    armed: bool,
}

impl MysqlCancellationGuard {
    fn new(
        uri: String,
        cancelled: Arc<AtomicBool>,
        connection_id: Arc<Mutex<Option<u64>>>,
    ) -> Self {
        Self {
            uri,
            cancelled,
            connection_id,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for MysqlCancellationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.cancelled.store(true, Ordering::SeqCst);
        let connection_id = self
            .connection_id
            .lock()
            .ok()
            .and_then(|connection_id| *connection_id);
        let Some(connection_id) = connection_id else {
            return;
        };
        let uri = self.uri.clone();
        let _ = std::thread::Builder::new()
            .name("llamafarm-mysql-cancel".to_string())
            .spawn(move || {
                if let Ok(mut control) = connect(&uri) {
                    let _ = control.query_drop(format!("KILL QUERY {connection_id}"));
                }
            });
    }
}

async fn run_mysql_operation<T, F>(uri: String, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut PooledConn) -> Result<T> + Send + 'static,
{
    let cancelled = Arc::new(AtomicBool::new(false));
    let connection_id = Arc::new(Mutex::new(None));
    let mut cancellation = MysqlCancellationGuard::new(
        uri.clone(),
        Arc::clone(&cancelled),
        Arc::clone(&connection_id),
    );
    let worker_cancelled = Arc::clone(&cancelled);
    let worker_connection_id = Arc::clone(&connection_id);
    let result = tokio::task::spawn_blocking(move || {
        let mut connection = connect(&uri)?;
        let id = connection
            .query_first::<u64, _>("SELECT CONNECTION_ID()")?
            .context("MySQL/MariaDB did not return a connection id")?;
        *worker_connection_id
            .lock()
            .map_err(|_| anyhow::anyhow!("MySQL cancellation state lock was poisoned"))? = Some(id);
        if worker_cancelled.load(Ordering::SeqCst) {
            anyhow::bail!("MySQL/MariaDB operation cancelled");
        }
        operation(&mut connection)
    })
    .await
    .map_err(|error| anyhow::anyhow!("spawn_blocking join error: {error}"))?;
    cancellation.disarm();
    result
}

/// Extract SQL words and statement delimiters while ignoring quoted values,
/// quoted identifiers, and ordinary comments.
///
/// MySQL/MariaDB executable comments (`/*! ... */` and `/*M! ... */`) are
/// rejected because the server evaluates their contents even though a
/// superficial SQL scanner sees a comment.
fn sql_tokens(sql: &str) -> Option<(Vec<String>, Vec<usize>)> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut semicolons = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                index += 1;
                let mut closed = false;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = (index + 2).min(bytes.len());
                    } else if bytes[index] == quote {
                        if index + 1 < bytes.len() && bytes[index + 1] == quote {
                            index += 2;
                        } else {
                            index += 1;
                            closed = true;
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
                if !closed {
                    return None;
                }
            }
            b'#' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'-' if index + 1 < bytes.len() && bytes[index + 1] == b'-' => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if index + 1 < bytes.len() && bytes[index + 1] == b'*' => {
                let mysql_executable = index + 2 < bytes.len() && bytes[index + 2] == b'!';
                let mariadb_executable = index + 3 < bytes.len()
                    && bytes[index + 2].eq_ignore_ascii_case(&b'm')
                    && bytes[index + 3] == b'!';
                if mysql_executable || mariadb_executable {
                    return None;
                }
                let Some(end) = sql[index + 2..].find("*/") else {
                    return None;
                };
                index += end + 4;
            }
            b';' => {
                semicolons.push(tokens.len());
                index += 1;
            }
            byte if byte.is_ascii_alphabetic() || byte == b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || matches!(bytes[index], b'_' | b'$'))
                {
                    index += 1;
                }
                tokens.push(sql[start..index].to_ascii_uppercase());
            }
            _ => index += 1,
        }
    }

    Some((tokens, semicolons))
}

fn is_read_only_sql(sql: &str) -> bool {
    let Some((tokens, semicolons)) = sql_tokens(sql) else {
        return false;
    };
    let Some(first) = tokens.first().map(String::as_str) else {
        return false;
    };

    // One optional trailing semicolon is accepted. A delimiter before another
    // token, or multiple delimiters, would enable a second statement.
    if semicolons.len() > 1
        || semicolons
            .first()
            .is_some_and(|token_index| *token_index != tokens.len())
    {
        return false;
    }

    if !matches!(
        first,
        "SELECT" | "EXPLAIN" | "SHOW" | "DESCRIBE" | "DESC" | "TABLE" | "WITH"
    ) {
        return false;
    }

    // SELECT ... INTO OUTFILE/DUMPFILE writes outside the transaction. CTEs
    // may prefix UPDATE/DELETE/INSERT, so reject mutating CTE bodies too.
    if tokens
        .iter()
        .any(|token| matches!(token.as_str(), "OUTFILE" | "DUMPFILE"))
    {
        return false;
    }
    if first == "WITH"
        && tokens.iter().any(|token| {
            matches!(
                token.as_str(),
                "ALTER"
                    | "CALL"
                    | "CREATE"
                    | "DELETE"
                    | "DO"
                    | "DROP"
                    | "GRANT"
                    | "HANDLER"
                    | "INSERT"
                    | "LOAD"
                    | "LOCK"
                    | "RENAME"
                    | "REPLACE"
                    | "REVOKE"
                    | "SET"
                    | "TRUNCATE"
                    | "UNLOCK"
                    | "UPDATE"
                    | "USE"
            )
        })
    {
        return false;
    }

    true
}

fn mysql_value_to_json(value: MysqlValue) -> Value {
    match value {
        MysqlValue::NULL => Value::Null,
        MysqlValue::Bytes(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Value::String(text),
            Err(error) => Value::String(format!(
                "base64:{}",
                base64::engine::general_purpose::STANDARD.encode(error.into_bytes())
            )),
        },
        MysqlValue::Int(value) => Value::Number(value.into()),
        MysqlValue::UInt(value) => Value::Number(value.into()),
        MysqlValue::Float(value) => serde_json::Number::from_f64(f64::from(value))
            .map(Value::Number)
            .unwrap_or(Value::Null),
        MysqlValue::Double(value) => serde_json::Number::from_f64(value)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        MysqlValue::Date(year, month, day, hour, minute, second, micros) => {
            let mut rendered =
                format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}");
            if micros != 0 {
                rendered.push_str(&format!(".{micros:06}"));
            }
            Value::String(rendered)
        }
        MysqlValue::Time(negative, days, hours, minutes, seconds, micros) => {
            let sign = if negative { "-" } else { "" };
            let total_hours = u64::from(days) * 24 + u64::from(hours);
            let mut rendered = format!("{sign}{total_hours:02}:{minutes:02}:{seconds:02}");
            if micros != 0 {
                rendered.push_str(&format!(".{micros:06}"));
            }
            Value::String(rendered)
        }
    }
}

type SchemaRow = (String, String, Option<String>, Option<String>);

fn schema_rows_to_tables(rows: Vec<SchemaRow>) -> Vec<TableInfo> {
    let mut tables: BTreeMap<String, (String, Vec<ColumnInfo>)> = BTreeMap::new();

    for (table_name, table_type, column_name, column_type) in rows {
        let kind = if table_type.to_ascii_uppercase().contains("VIEW") {
            "view"
        } else {
            "table"
        };
        let entry = tables
            .entry(table_name)
            .or_insert_with(|| (kind.to_string(), Vec::new()));
        if let Some(name) = column_name {
            entry.1.push(ColumnInfo {
                name,
                data_type: column_type.unwrap_or_default(),
            });
        }
    }

    tables
        .into_iter()
        .map(|(name, (kind, columns))| TableInfo {
            name,
            columns,
            kind,
        })
        .collect()
}

fn collect_query_rows<Q: Queryable>(
    connection: &mut Q,
    sql: &str,
    max_rows: usize,
) -> Result<QueryResult> {
    let mut result = connection.query_iter(sql)?;
    let columns = result
        .columns()
        .as_ref()
        .iter()
        .map(|column| column.name_str().into_owned())
        .collect();
    let mut rows = Vec::new();
    let mut truncated = false;

    for row in result.by_ref() {
        let row = row?;
        if rows.len() >= max_rows {
            truncated = true;
            break;
        }
        rows.push(row.unwrap().into_iter().map(mysql_value_to_json).collect());
    }

    let row_count = rows.len();
    Ok(QueryResult {
        columns,
        rows,
        row_count,
        truncated,
    })
}

#[async_trait]
impl DbAdapter for MysqlAdapter {
    fn driver_name(&self) -> &str {
        "mysql"
    }

    async fn schema(&self) -> Result<DbSchema> {
        let uri = self.uri.clone();
        run_mysql_operation(uri, move |connection| {
            let database = connection
                .query_first::<Option<String>, _>("SELECT DATABASE()")?
                .flatten();
            let rows: Vec<SchemaRow> = connection.query(
                "SELECT t.TABLE_NAME, t.TABLE_TYPE, c.COLUMN_NAME, c.COLUMN_TYPE \
                 FROM information_schema.TABLES AS t \
                 LEFT JOIN information_schema.COLUMNS AS c \
                   ON c.TABLE_SCHEMA = t.TABLE_SCHEMA \
                  AND c.TABLE_NAME = t.TABLE_NAME \
                 WHERE t.TABLE_SCHEMA = DATABASE() \
                 ORDER BY t.TABLE_NAME, c.ORDINAL_POSITION",
            )?;

            Ok(DbSchema {
                driver: "mysql".to_string(),
                database,
                tables: schema_rows_to_tables(rows),
            })
        })
        .await
    }

    async fn query(&self, sql: &str, max_rows: usize) -> Result<QueryResult> {
        if self.read_only && !is_read_only_sql(sql) {
            anyhow::bail!(
                "Connection is read-only; only one SELECT/EXPLAIN/SHOW/DESCRIBE/TABLE/WITH query is allowed"
            );
        }

        let uri = self.uri.clone();
        let sql = sql.to_string();
        let read_only = self.read_only;
        run_mysql_operation(uri, move |connection| {
            if read_only {
                let options = TxOpts::default().set_access_mode(Some(AccessMode::ReadOnly));
                let mut transaction = connection
                    .start_transaction(options)
                    .context("failed to start read-only MySQL/MariaDB transaction")?;
                let output = collect_query_rows(&mut transaction, &sql, max_rows)?;
                transaction
                    .rollback()
                    .context("failed to roll back read-only MySQL/MariaDB transaction")?;
                Ok(output)
            } else {
                collect_query_rows(connection, &sql, max_rows)
            }
        })
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_sql_accepts_single_query_forms() {
        for sql in [
            "SELECT * FROM papers",
            "  /* context */ SELECT 1;",
            "EXPLAIN SELECT * FROM papers",
            "SHOW TABLES",
            "DESCRIBE papers",
            "DESC papers",
            "TABLE papers",
            "WITH recent AS (SELECT * FROM papers) SELECT * FROM recent",
        ] {
            assert!(is_read_only_sql(sql), "expected read-only SQL: {sql}");
        }
    }

    #[test]
    fn read_only_sql_rejects_mutation_and_multi_statement_forms() {
        for sql in [
            "INSERT INTO papers VALUES (1)",
            "UPDATE papers SET title = 'changed'",
            "WITH recent AS (SELECT 1) DELETE FROM papers",
            "SELECT 1; DROP TABLE papers",
            "SELECT 1;;",
            "SELECT * FROM papers INTO OUTFILE '/tmp/papers'",
            "/*!50000 DROP TABLE papers */",
            "SELECT 1 /*M!100100 INTO OUTFILE '/tmp/value' */",
            "/* unterminated SELECT 1",
            "SELECT 'unterminated",
            "",
        ] {
            assert!(!is_read_only_sql(sql), "expected rejected SQL: {sql}");
        }
    }

    #[test]
    fn read_only_sql_ignores_keywords_inside_values_and_comments() {
        assert!(is_read_only_sql(
            "WITH note AS (SELECT 'UPDATE; DROP' AS value) SELECT value FROM note"
        ));
        assert!(is_read_only_sql(
            "SELECT 'INTO OUTFILE', `UPDATE` FROM papers -- DELETE"
        ));
    }

    #[test]
    fn mysql_values_convert_to_stable_json() {
        assert_eq!(mysql_value_to_json(MysqlValue::NULL), Value::Null);
        assert_eq!(
            mysql_value_to_json(MysqlValue::Bytes(b"text".to_vec())),
            Value::String("text".to_string())
        );
        assert_eq!(
            mysql_value_to_json(MysqlValue::Bytes(vec![0xff, 0x00])),
            Value::String("base64:/wA=".to_string())
        );
        assert_eq!(
            mysql_value_to_json(MysqlValue::Int(-7)),
            serde_json::json!(-7)
        );
        assert_eq!(
            mysql_value_to_json(MysqlValue::UInt(7)),
            serde_json::json!(7)
        );
        assert_eq!(
            mysql_value_to_json(MysqlValue::Float(1.5)),
            serde_json::json!(1.5)
        );
        assert_eq!(
            mysql_value_to_json(MysqlValue::Double(f64::NAN)),
            Value::Null
        );
        assert_eq!(
            mysql_value_to_json(MysqlValue::Date(2026, 7, 31, 9, 8, 7, 123)),
            Value::String("2026-07-31 09:08:07.000123".to_string())
        );
        assert_eq!(
            mysql_value_to_json(MysqlValue::Time(true, 2, 3, 4, 5, 6)),
            Value::String("-51:04:05.000006".to_string())
        );
    }

    #[test]
    fn dropping_pending_operation_marks_it_cancelled_before_connection() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let connection_id = Arc::new(Mutex::new(None));
        let guard = MysqlCancellationGuard::new(
            "mysql://unused.invalid/database".to_string(),
            Arc::clone(&cancelled),
            connection_id,
        );

        drop(guard);

        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn schema_rows_are_grouped_and_views_are_identified() {
        let tables = schema_rows_to_tables(vec![
            (
                "papers".to_string(),
                "BASE TABLE".to_string(),
                Some("id".to_string()),
                Some("bigint unsigned".to_string()),
            ),
            (
                "papers".to_string(),
                "BASE TABLE".to_string(),
                Some("title".to_string()),
                Some("varchar(255)".to_string()),
            ),
            (
                "recent_papers".to_string(),
                "VIEW".to_string(),
                Some("id".to_string()),
                Some("bigint unsigned".to_string()),
            ),
        ]);

        assert_eq!(tables.len(), 2);
        assert_eq!(tables[0].name, "papers");
        assert_eq!(tables[0].kind, "table");
        assert_eq!(tables[0].columns.len(), 2);
        assert_eq!(tables[1].name, "recent_papers");
        assert_eq!(tables[1].kind, "view");
    }

    #[tokio::test]
    async fn read_only_rejection_happens_before_any_connection_attempt() {
        let adapter = MysqlAdapter::new(
            "mysql://invalid.invalid:3306/never_contact".to_string(),
            true,
        );
        let error = adapter
            .query("DELETE FROM papers", 10)
            .await
            .expect_err("mutation must be rejected");
        assert!(error.to_string().contains("Connection is read-only"));
    }
}
