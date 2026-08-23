//! Database abstraction layer for MySQL, PostgreSQL, and SQLite.
//!
//! Provides database type detection, connection management, table name validation,
//! and query execution returning serializable results. Classifying SQL text is the
//! job of [`crate::sql`].

use std::fmt;

use futures_util::TryStreamExt;
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::{AssertSqlSafe, Row};

use crate::decode::{CellDecode, bytes_to_string, column_names, decode_row};
use crate::error::AppError;

/// Database-specific connection pool.
///
/// Each variant wraps a native pool for its respective database engine.
/// Using native pools instead of `AnyPool` avoids the limited type-mapping layer
/// in sqlx's `Any` driver, which fails on types like `TINYINT UNSIGNED` and `TIMESTAMP`.
#[derive(Debug, Clone)]
pub(crate) enum DatabasePool {
    Mysql(sqlx::MySqlPool),
    Postgres(sqlx::PgPool),
    Sqlite(sqlx::SqlitePool),
}

/// Dispatches a method call across the three `DatabasePool` variants.
///
/// The body is monomorphized for each native pool type, allowing generic
/// sqlx operations to resolve to the correct concrete types.
macro_rules! with_pool {
    ($self:expr, |$pool:ident| $body:expr) => {
        match $self {
            DatabasePool::Mysql($pool) => $body,
            DatabasePool::Postgres($pool) => $body,
            DatabasePool::Sqlite($pool) => $body,
        }
    };
}

/// Maximum allowed length for a validated table name.
/// Set to 128 as a generous upper bound; individual engines may enforce stricter limits
/// (MySQL: 64, PostgreSQL: 63). Names exceeding the engine's limit will be rejected at
/// query time.
const MAX_TABLE_NAME_LEN: usize = 128;

/// Supported database engine types.
///
/// Each variant encapsulates dialect-specific behavior: SQL system catalog queries,
/// identifier quoting, and resource URI schemes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DbType {
    /// MySQL (and protocol-compatible engines such as MariaDB).
    Mysql,
    /// PostgreSQL.
    Postgres,
    /// SQLite.
    Sqlite,
}

impl fmt::Display for DbType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Mysql => write!(f, "MySQL"),
            Self::Postgres => write!(f, "PostgreSQL"),
            Self::Sqlite => write!(f, "SQLite"),
        }
    }
}

/// Describes how to execute a table-description query.
///
/// The safety of the `Interpolated` variant depends on the [`ValidatedTableName`] invariant
/// guaranteeing that only `[a-zA-Z0-9_]` characters are present.
pub(crate) enum DescribeQuery {
    /// Query with a bind parameter (`?` or `$1`) for the table name.
    Parameterized(&'static str),
    /// Query with the table name already interpolated (e.g., SQLite PRAGMA).
    Interpolated(String),
}

impl DbType {
    /// Detects the database type from a connection URL by examining the scheme prefix.
    ///
    /// Supports `mysql://`, `postgres://`, `postgresql://`, `sqlite://`, and `sqlite:`.
    ///
    /// `mysql+*` / `postgres+*` (the SQLAlchemy dialect spelling) are deliberately
    /// rejected: sqlx ignores the scheme entirely when parsing a URL, so
    /// `mysql+unix:///var/run/mysqld/mysqld.sock` would silently become a TCP
    /// connection to localhost with `var/run/mysqld/mysqld.sock` as the database
    /// name. Point a UNIX socket at the engine the way sqlx expects instead:
    /// `mysql://root@localhost/db?socket=/var/run/mysqld/mysqld.sock` or
    /// `postgres://user@%2Fvar%2Frun%2Fpostgresql/db`.
    ///
    /// Error messages intentionally include only the scheme portion to avoid leaking
    /// credentials from the URL.
    ///
    /// # Errors
    /// Returns [`AppError::UnsupportedScheme`] when the URL's scheme maps to no
    /// supported engine. The error carries only the scheme, never the full URL.
    pub fn from_url(url: &str) -> Result<Self, AppError> {
        if url.starts_with("mysql://") {
            Ok(Self::Mysql)
        } else if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            Ok(Self::Postgres)
        } else if url.starts_with("sqlite://") || url.starts_with("sqlite:") {
            Ok(Self::Sqlite)
        } else {
            let scheme = url.split(':').next().unwrap_or("unknown");
            Err(AppError::UnsupportedScheme(scheme.to_string()))
        }
    }

    /// Returns the SQL query to list tables in the default user-facing schema
    /// (MySQL: current database, PostgreSQL: `public`, SQLite: non-system tables).
    pub fn list_tables_query(&self) -> &'static str {
        match self {
            Self::Mysql => {
                "SELECT table_name FROM information_schema.tables WHERE table_schema = DATABASE() ORDER BY table_name"
            }
            Self::Postgres => {
                "SELECT table_name FROM information_schema.tables WHERE table_schema = 'public' ORDER BY table_name"
            }
            Self::Sqlite => {
                "SELECT name AS table_name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
            }
        }
    }

    /// Returns a [`DescribeQuery`] for retrieving column metadata of the given table.
    ///
    /// MySQL and PostgreSQL use parameterized `information_schema` queries.
    /// SQLite uses `PRAGMA table_info` with the table name interpolated (safe because
    /// [`ValidatedTableName`] guarantees only `[a-zA-Z0-9_]` characters).
    pub(crate) fn describe_table_query(&self, table: &ValidatedTableName) -> DescribeQuery {
        match self {
            Self::Mysql => DescribeQuery::Parameterized(
                "SELECT column_name, data_type, is_nullable, column_default, column_key \
                 FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                 ORDER BY ordinal_position",
            ),
            Self::Postgres => DescribeQuery::Parameterized(
                "SELECT column_name, data_type, is_nullable, column_default \
                 FROM information_schema.columns \
                 WHERE table_schema = 'public' AND table_name = $1 \
                 ORDER BY ordinal_position",
            ),
            Self::Sqlite => {
                let name = table.as_str();
                DescribeQuery::Interpolated(format!("PRAGMA table_info('{name}')"))
            }
        }
    }

    /// Wraps a validated table name in dialect-appropriate quotes
    /// (backticks for MySQL, double quotes for PostgreSQL/SQLite).
    pub fn quote_identifier(&self, name: &ValidatedTableName) -> String {
        match self {
            Self::Mysql => format!("`{}`", name.as_str()),
            Self::Postgres | Self::Sqlite => format!("\"{}\"", name.as_str()),
        }
    }

    /// Returns the URI scheme used for MCP resource identifiers.
    pub fn resource_uri_scheme(&self) -> &'static str {
        match self {
            Self::Mysql => "mysql",
            Self::Postgres => "postgres",
            Self::Sqlite => "sqlite",
        }
    }
}

/// A table name validated to be non-empty, at most 128 characters, and containing only ASCII
/// alphanumeric characters and underscores.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedTableName(String);

impl ValidatedTableName {
    /// Creates a new `ValidatedTableName` after validating the input.
    ///
    /// # Validation rules
    /// - Must not be empty.
    /// - Must contain only ASCII alphanumeric characters and underscores (`[a-zA-Z0-9_]`).
    /// - Must not exceed [`MAX_TABLE_NAME_LEN`] (128) characters.
    ///
    /// # Errors
    /// Returns [`AppError::InvalidTableName`] describing the specific validation failure.
    pub fn new(name: &str) -> Result<Self, AppError> {
        if name.is_empty() {
            return Err(AppError::InvalidTableName("table name must not be empty".to_string()));
        }
        // Character validation MUST run before the length check so that the byte-level
        // slice in the length error message (`&name[..32]`) never lands inside a
        // multi-byte UTF-8 code point.
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            let preview: String = name.chars().take(64).collect();
            return Err(AppError::InvalidTableName(format!(
                "table name '{preview}' contains invalid characters (only ASCII alphanumeric and underscores allowed)",
            )));
        }
        // At this point all characters are ASCII, so byte length == character count
        // and byte-level slicing is safe.
        if name.len() > MAX_TABLE_NAME_LEN {
            return Err(AppError::InvalidTableName(format!(
                "table name '{}...' exceeds maximum length of {} characters",
                &name[..32],
                MAX_TABLE_NAME_LEN
            )));
        }
        Ok(Self(name.to_string()))
    }

    /// Returns the validated table name as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ValidatedTableName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Result of a read query, as column names plus row values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct QueryResult {
    /// Column names, in the order the database returned them.
    /// Empty when the query produced no rows, because column metadata is
    /// only available from a returned row.
    pub columns: Vec<String>,
    /// Each row holds one decoded value per column; `None` represents SQL NULL.
    pub rows: Vec<Vec<Option<String>>>,
    /// Number of rows in `rows`.
    pub row_count: usize,
    /// True when the result was cut off at the row limit.
    pub truncated: bool,
}

impl QueryResult {
    /// Builds a result from decoded columns and rows, deriving `row_count` from `rows`.
    fn new(columns: Vec<String>, rows: Vec<Vec<Option<String>>>, truncated: bool) -> Self {
        Self { columns, row_count: rows.len(), rows, truncated }
    }

    /// Returns true when the query produced no rows.
    pub fn is_empty(&self) -> bool {
        self.row_count == 0
    }
}

/// Result of a write or DDL statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ExecutionResult {
    /// Number of rows the statement inserted, updated, or deleted.
    pub rows_affected: u64,
}

/// Builds a [`QueryResult`] from a slice of database rows.
fn rows_to_query_result<R: CellDecode>(
    rows: &[R],
    truncated: bool,
) -> Result<QueryResult, sqlx::Error> {
    // Column metadata is carried by the rows themselves, so an empty
    // result set cannot report which columns the query selected.
    let Some(first) = rows.first() else {
        return Ok(QueryResult::new(Vec::new(), Vec::new(), truncated));
    };
    let names = column_names(first);
    let values = rows.iter().map(decode_row).collect::<Result<Vec<_>, _>>()?;
    Ok(QueryResult::new(names, values, truncated))
}

/// A database connection bundling a connection pool with its detected database type.
#[derive(Debug, Clone)]
pub struct DatabaseConnection {
    pool: DatabasePool,
    db_type: DbType,
}

impl DatabaseConnection {
    /// Connects to a database using the given URL, auto-detecting the engine type.
    ///
    /// Supports the same URL schemes as [`DbType::from_url`]. Connection errors are
    /// wrapped in [`AppError::ConnectionFailed`] to avoid potentially leaking
    /// credentials embedded in the URL through raw sqlx error messages.
    ///
    /// # Errors
    /// Returns [`AppError::UnsupportedScheme`] when the URL's scheme maps to no
    /// supported engine, or [`AppError::ConnectionFailed`] when the pool cannot
    /// establish its first connection (host unreachable, authentication rejected,
    /// unknown database).
    pub async fn connect(url: &str) -> Result<Self, AppError> {
        let db_type = DbType::from_url(url)?;
        let connection_error = |e: sqlx::Error| {
            tracing::error!(db_type = %db_type, error = %e, "database connection failed");
            AppError::ConnectionFailed(format!(
                "{db_type}: failed to establish database connection"
            ))
        };
        let pool = match db_type {
            DbType::Mysql => {
                DatabasePool::Mysql(sqlx::MySqlPool::connect(url).await.map_err(connection_error)?)
            }
            DbType::Postgres => {
                DatabasePool::Postgres(sqlx::PgPool::connect(url).await.map_err(connection_error)?)
            }
            DbType::Sqlite => DatabasePool::Sqlite(
                sqlx::SqlitePool::connect(url).await.map_err(connection_error)?,
            ),
        };
        Ok(Self { pool, db_type })
    }

    /// Creates a `DatabaseConnection` from an existing SQLite pool.
    /// Intended for testing only. Prefer [`connect`](Self::connect) for production use.
    pub fn from_sqlite_pool(pool: sqlx::SqlitePool) -> Self {
        Self { pool: DatabasePool::Sqlite(pool), db_type: DbType::Sqlite }
    }

    /// Returns the detected database engine type.
    pub fn db_type(&self) -> DbType {
        self.db_type
    }

    /// Streams rows for a SELECT query, keeping at most `max_rows` of them.
    ///
    /// The returned [`QueryResult`] reports `truncated` only when a row beyond the
    /// limit actually exists, so a result of exactly `max_rows` rows is not flagged.
    /// Each row is decoded as it arrives and the raw row is then dropped, so the raw
    /// and decoded representations never occupy memory at the same time.
    ///
    /// `sql` is wrapped in [`AssertSqlSafe`] because running caller-supplied SQL is the
    /// purpose of this server; injection is not a meaningful threat here.
    /// [`crate::sql::is_read_query`] does not gate this call: it only picks the response
    /// shape for the `execute_sql` tool and never blocks a statement. The resource-read
    /// path does not forward caller
    /// SQL at all — it builds `SELECT * FROM {table}` itself, with `{table}` quoted by
    /// [`DbType::quote_identifier`] from a [`ValidatedTableName`] restricted to
    /// `[a-zA-Z0-9_]`.
    ///
    /// # Errors
    /// Returns the underlying [`sqlx::Error`] when the statement fails to execute
    /// or when a returned value cannot be decoded into its string representation.
    pub async fn fetch_streaming(
        &self,
        sql: &str,
        max_rows: usize,
    ) -> Result<QueryResult, sqlx::Error> {
        with_pool!(&self.pool, |pool| {
            let mut stream = sqlx::query(AssertSqlSafe(sql)).fetch(pool);
            let mut columns: Vec<String> = Vec::new();
            let mut rows: Vec<Vec<Option<String>>> = Vec::new();
            let mut truncated = false;
            while let Some(row) = stream.try_next().await? {
                if rows.len() == max_rows {
                    // Reading one row past the limit is what distinguishes a result
                    // that was cut short from one that merely ends at the limit.
                    truncated = true;
                    break;
                }
                if columns.is_empty() {
                    columns = column_names(&row);
                }
                rows.push(decode_row(&row)?);
            }
            drop(stream);
            Ok(QueryResult::new(columns, rows, truncated))
        })
    }

    /// Fetches the first column of each row as a `String`.
    /// Useful for retrieving table name lists.
    ///
    /// # Errors
    /// Returns the underlying [`sqlx::Error`] when the statement fails to execute,
    /// or when the first column of a row decodes as neither `String` nor `Vec<u8>`.
    pub async fn fetch_column_as_strings(&self, sql: &str) -> Result<Vec<String>, sqlx::Error> {
        with_pool!(&self.pool, |pool| {
            let rows = sqlx::query(AssertSqlSafe(sql)).fetch_all(pool).await?;
            rows.iter()
                .map(|row| {
                    row.try_get::<String, _>(0)
                        .or_else(|_| row.try_get::<Vec<u8>, _>(0).map(bytes_to_string))
                })
                .collect::<Result<Vec<_>, _>>()
        })
    }

    /// Executes a write/DDL statement, returning the number of affected rows.
    ///
    /// # Errors
    /// Returns the underlying [`sqlx::Error`] when the statement fails to execute
    /// (syntax error, unknown table, constraint violation).
    pub async fn execute_sql(&self, sql: &str) -> Result<ExecutionResult, sqlx::Error> {
        with_pool!(&self.pool, |pool| {
            let result = sqlx::query(AssertSqlSafe(sql)).execute(pool).await?;
            Ok(ExecutionResult { rows_affected: result.rows_affected() })
        })
    }

    /// Fetches column metadata for the given table.
    ///
    /// # Errors
    /// Returns [`AppError::Database`] when the catalog query fails to execute or a
    /// returned value cannot be decoded. An unknown table is not an error here: it
    /// yields an empty [`QueryResult`], which callers report as "not found".
    pub async fn describe_table(
        &self,
        table: &ValidatedTableName,
    ) -> Result<QueryResult, AppError> {
        let describe = self.db_type.describe_table_query(table);
        let result = match describe {
            DescribeQuery::Parameterized(sql) => with_pool!(&self.pool, |pool| {
                let rows = sqlx::query(sql).bind(table.as_str()).fetch_all(pool).await?;
                rows_to_query_result(&rows, false)
            })?,
            DescribeQuery::Interpolated(sql) => with_pool!(&self.pool, |pool| {
                let rows = sqlx::query(AssertSqlSafe(sql.as_str())).fetch_all(pool).await?;
                rows_to_query_result(&rows, false)
            })?,
        };
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db_type_from_url_mysql() {
        assert_eq!(DbType::from_url("mysql://user:pass@localhost/db").unwrap(), DbType::Mysql);
    }

    #[test]
    fn test_db_type_from_url_postgres() {
        assert_eq!(
            DbType::from_url("postgres://user:pass@localhost/db").unwrap(),
            DbType::Postgres
        );
        assert_eq!(
            DbType::from_url("postgresql://user:pass@localhost/db").unwrap(),
            DbType::Postgres
        );
    }

    #[test]
    fn test_db_type_from_url_sqlite() {
        assert_eq!(DbType::from_url("sqlite:./data.db").unwrap(), DbType::Sqlite);
        assert_eq!(DbType::from_url("sqlite://./data.db").unwrap(), DbType::Sqlite);
    }

    #[test]
    fn test_db_type_from_url_unsupported() {
        assert!(DbType::from_url("oracle://localhost/db").is_err());
    }

    #[test]
    fn test_db_type_from_url_error_does_not_leak_credentials() {
        let err = DbType::from_url("oracle://user:secret@host/db").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("oracle"), "error should contain scheme");
        assert!(!msg.contains("secret"), "error must not contain credentials");
    }

    #[test]
    fn test_db_type_display() {
        assert_eq!(DbType::Mysql.to_string(), "MySQL");
        assert_eq!(DbType::Postgres.to_string(), "PostgreSQL");
        assert_eq!(DbType::Sqlite.to_string(), "SQLite");
    }

    #[test]
    fn test_validated_table_name_valid() {
        assert!(ValidatedTableName::new("users").is_ok());
        assert!(ValidatedTableName::new("user_accounts").is_ok());
        assert!(ValidatedTableName::new("t1").is_ok());
    }

    #[test]
    fn test_validated_table_name_invalid() {
        assert!(ValidatedTableName::new("").is_err());
        assert!(ValidatedTableName::new("users; DROP TABLE users").is_err());
        assert!(ValidatedTableName::new("table-name").is_err());
        assert!(ValidatedTableName::new("table name").is_err());
    }

    #[test]
    fn test_validated_table_name_error_messages() {
        let err = ValidatedTableName::new("").unwrap_err();
        assert!(err.to_string().contains("must not be empty"));

        // An all-ASCII name that exceeds the max length should trigger the length error
        // (the character check passes first since all characters are valid).
        let too_long = "a".repeat(MAX_TABLE_NAME_LEN + 1);
        let err = ValidatedTableName::new(&too_long).unwrap_err();
        assert!(err.to_string().contains("exceeds maximum length"));

        let err = ValidatedTableName::new("bad;name").unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn test_validated_table_name_multibyte_utf8_does_not_panic() {
        // 31 ASCII chars + 4-byte emoji + enough ASCII to exceed MAX_TABLE_NAME_LEN bytes.
        // This must not panic; the character validation should reject it before the length check.
        let mut name = "a".repeat(31);
        name.push('\u{1F600}'); // 4-byte emoji
        name.push_str(&"b".repeat(100));
        let err = ValidatedTableName::new(&name).unwrap_err();
        assert!(err.to_string().contains("invalid characters"));
    }

    #[test]
    fn test_validated_table_name_max_length() {
        let long_name = "a".repeat(MAX_TABLE_NAME_LEN);
        assert!(ValidatedTableName::new(&long_name).is_ok());
        let too_long = "a".repeat(MAX_TABLE_NAME_LEN + 1);
        assert!(ValidatedTableName::new(&too_long).is_err());
    }

    #[test]
    fn test_validated_table_name_equality() {
        let a = ValidatedTableName::new("users").unwrap();
        let b = ValidatedTableName::new("users").unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn test_validated_table_name_as_str() {
        let name = ValidatedTableName::new("users").unwrap();
        assert_eq!(name.as_str(), "users");
    }

    #[test]
    fn test_list_tables_query() {
        let q = DbType::Mysql.list_tables_query();
        assert!(q.contains("information_schema"));
        assert!(q.contains("DATABASE()"));

        let q = DbType::Postgres.list_tables_query();
        assert!(q.contains("information_schema"));
        assert!(q.contains("'public'"));

        let q = DbType::Sqlite.list_tables_query();
        assert!(q.contains("sqlite_master"));
    }

    #[test]
    fn test_describe_table_query_sqlite_interpolated() {
        let table = ValidatedTableName::new("users").unwrap();
        let query = DbType::Sqlite.describe_table_query(&table);
        match query {
            DescribeQuery::Interpolated(sql) => {
                assert!(sql.contains("PRAGMA"));
                assert!(sql.contains("users"));
            }
            DescribeQuery::Parameterized(_) => panic!("expected Interpolated for SQLite"),
        }
    }

    #[test]
    fn test_describe_table_query_mysql_parameterized() {
        let table = ValidatedTableName::new("users").unwrap();
        let query = DbType::Mysql.describe_table_query(&table);
        match query {
            DescribeQuery::Parameterized(sql) => {
                assert!(sql.contains("information_schema"));
                assert!(sql.contains('?'));
            }
            DescribeQuery::Interpolated(_) => panic!("expected Parameterized for MySQL"),
        }
    }

    #[test]
    fn test_describe_table_query_postgres_parameterized() {
        let table = ValidatedTableName::new("users").unwrap();
        let query = DbType::Postgres.describe_table_query(&table);
        match query {
            DescribeQuery::Parameterized(sql) => {
                assert!(sql.contains("information_schema"));
                assert!(sql.contains("$1"));
            }
            DescribeQuery::Interpolated(_) => panic!("expected Parameterized for PostgreSQL"),
        }
    }

    #[test]
    fn test_resource_uri_scheme() {
        assert_eq!(DbType::Mysql.resource_uri_scheme(), "mysql");
        assert_eq!(DbType::Postgres.resource_uri_scheme(), "postgres");
        assert_eq!(DbType::Sqlite.resource_uri_scheme(), "sqlite");
    }

    #[test]
    fn test_quote_identifier() {
        let name = ValidatedTableName::new("users").unwrap();
        assert_eq!(DbType::Mysql.quote_identifier(&name), "`users`");
        assert_eq!(DbType::Postgres.quote_identifier(&name), "\"users\"");
        assert_eq!(DbType::Sqlite.quote_identifier(&name), "\"users\"");
    }

    #[test]
    fn test_db_type_from_url_rejects_plus_schemes() {
        // sqlx never looks at the scheme, so accepting these would connect
        // somewhere the caller did not ask for instead of failing.
        let err = DbType::from_url("mysql+unix:///var/run/mysqld/mysqld.sock").unwrap_err();
        assert!(err.to_string().contains("mysql+unix"), "error should name the scheme");

        let err = DbType::from_url("postgres+unix:///var/run/postgresql").unwrap_err();
        assert!(err.to_string().contains("postgres+unix"), "error should name the scheme");
    }
}
