use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;

pub mod sqlite;

#[cfg(feature = "db-postgres")]
pub mod postgres;

#[cfg(feature = "db-mongo")]
pub mod mongodb;

#[cfg(feature = "db-mysql")]
pub mod mysql;

// ── Shared output types ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    pub data_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableInfo {
    /// Table, view, or collection name.
    pub name: String,
    pub columns: Vec<ColumnInfo>,
    /// "table", "view", or "collection"
    pub kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DbSchema {
    pub driver: String,
    pub database: Option<String>,
    pub tables: Vec<TableInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<serde_json::Value>>,
    pub row_count: usize,
    pub truncated: bool,
}

// ── Adapter trait ─────────────────────────────────────────────────────────────

#[async_trait]
pub trait DbAdapter: Send + Sync {
    fn driver_name(&self) -> &str;
    async fn schema(&self) -> anyhow::Result<DbSchema>;
    async fn query(&self, sql: &str, max_rows: usize) -> anyhow::Result<QueryResult>;
}

/// Remove configured database URIs and embedded passwords from errors before
/// they cross an API or tool boundary.
///
/// Driver libraries generally avoid echoing credentials, but parse and
/// connection errors are not a stable contract. Keep the original error useful
/// while ensuring a future driver version cannot expose a saved URI.
pub fn sanitize_connection_error(error: &dyn std::fmt::Display, connection_uri: &str) -> String {
    let mut message = error.to_string();
    if !connection_uri.trim().is_empty() {
        message = message.replace(connection_uri, "<redacted database URI>");
    }

    static URI_USERINFO: OnceLock<regex::Regex> = OnceLock::new();
    let regex = URI_USERINFO.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\b(?P<scheme>mongodb(?:\+srv)?|postgres(?:ql)?|mysql|mariadb)://(?P<user>[^/\s:@]+):[^@\s/]+@",
        )
        .expect("database URI credential regex must compile")
    });
    regex
        .replace_all(&message, "${scheme}://${user}:***MASKED***@")
        .into_owned()
}

// ── Factory ───────────────────────────────────────────────────────────────────

pub fn build_adapter(
    conn: &crate::config::DbConnectionConfig,
) -> anyhow::Result<Box<dyn DbAdapter>> {
    use crate::config::DbDriver;
    match &conn.driver {
        DbDriver::Sqlite => Ok(Box::new(sqlite::SqliteAdapter::new(
            &conn.uri,
            conn.read_only,
        )?)),
        DbDriver::Postgres => build_postgres(conn),
        DbDriver::Mongodb => build_mongo(conn),
        DbDriver::Mysql => build_mysql(conn),
    }
}

#[cfg(feature = "db-postgres")]
fn build_postgres(conn: &crate::config::DbConnectionConfig) -> anyhow::Result<Box<dyn DbAdapter>> {
    Ok(Box::new(postgres::PostgresAdapter::new(
        conn.uri.clone(),
        conn.read_only,
    )))
}

#[cfg(not(feature = "db-postgres"))]
fn build_postgres(_conn: &crate::config::DbConnectionConfig) -> anyhow::Result<Box<dyn DbAdapter>> {
    anyhow::bail!(
        "PostgreSQL support requires the 'db-postgres' Cargo feature \
         (add db-postgres to LLAMAFARM_CARGO_FEATURES)"
    )
}

#[cfg(feature = "db-mongo")]
fn build_mongo(conn: &crate::config::DbConnectionConfig) -> anyhow::Result<Box<dyn DbAdapter>> {
    Ok(Box::new(mongodb::MongoAdapter::new(
        conn.uri.clone(),
        conn.database.clone(),
        conn.read_only,
    )))
}

#[cfg(not(feature = "db-mongo"))]
fn build_mongo(_conn: &crate::config::DbConnectionConfig) -> anyhow::Result<Box<dyn DbAdapter>> {
    anyhow::bail!(
        "MongoDB support requires the 'db-mongo' Cargo feature \
         (add db-mongo to LLAMAFARM_CARGO_FEATURES)"
    )
}

#[cfg(feature = "db-mysql")]
fn build_mysql(conn: &crate::config::DbConnectionConfig) -> anyhow::Result<Box<dyn DbAdapter>> {
    Ok(Box::new(mysql::MysqlAdapter::new(
        conn.uri.clone(),
        conn.read_only,
    )))
}

#[cfg(not(feature = "db-mysql"))]
fn build_mysql(_conn: &crate::config::DbConnectionConfig) -> anyhow::Result<Box<dyn DbAdapter>> {
    anyhow::bail!(
        "MySQL/MariaDB support requires the 'db-mysql' Cargo feature \
         (add db-mysql to LLAMAFARM_CARGO_FEATURES)"
    )
}

#[cfg(test)]
mod tests {
    use super::{build_adapter, sanitize_connection_error};
    use crate::config::{DbConnectionConfig, DbDriver};

    fn mysql_connection() -> DbConnectionConfig {
        DbConnectionConfig {
            name: "mysql_test".to_string(),
            driver: DbDriver::Mysql,
            uri: "mysql://reader@db.example.com/research".to_string(),
            database: None,
            read_only: true,
            max_rows: 25,
            label: None,
        }
    }

    #[test]
    fn connection_error_removes_the_exact_configured_uri() {
        let uri = "mongodb://reader:private-value@db.internal:27017/ArXivDB";
        let error = format!("failed to parse {uri}");

        let sanitized = sanitize_connection_error(&error, uri);

        assert_eq!(sanitized, "failed to parse <redacted database URI>");
        assert!(!sanitized.contains("private-value"));
    }

    #[test]
    fn connection_error_masks_driver_rendered_userinfo() {
        for scheme in ["postgresql", "mysql", "mariadb"] {
            let error =
                format!("server rejected {scheme}://reader:private-value@db.internal/research");
            let sanitized = sanitize_connection_error(&error, "different stored value");

            assert_eq!(
                sanitized,
                format!(
                    "server rejected {scheme}://reader:***MASKED***@db.internal/research"
                )
            );
            assert!(!sanitized.contains("private-value"));
        }
    }

    #[cfg(feature = "db-mysql")]
    #[test]
    fn mysql_factory_builds_adapter_when_feature_is_enabled() {
        let adapter = build_adapter(&mysql_connection()).expect("MySQL adapter should build");
        assert_eq!(adapter.driver_name(), "mysql");
    }

    #[cfg(not(feature = "db-mysql"))]
    #[test]
    fn mysql_factory_reports_required_feature() {
        let error = match build_adapter(&mysql_connection()) {
            Ok(_) => panic!("MySQL adapter must require the db-mysql feature"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("'db-mysql' Cargo feature"));
    }
}
