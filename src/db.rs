//! Database abstraction layer for MySQL, PostgreSQL, and SQLite.
//!
//! Provides database type detection, connection management, SQL query classification,
//! table name validation, and CSV serialization of query results.

use std::borrow::Cow;
use std::fmt;

use sqlx::{AnyPool, Column, Row, any::AnyRow};

use crate::error::AppError;

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
    Mysql,
    Postgres,
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
    /// Supports `mysql://`, `mysql+*`, `postgres://`, `postgresql://`, `postgres+*`,
    /// `sqlite://`, and `sqlite:` schemes. Note: `postgresql+*` variants
    /// (e.g., `postgresql+unix://`) are not supported; use `postgres+*` instead.
    ///
    /// Error messages intentionally include only the scheme portion to avoid leaking
    /// credentials from the URL.
    pub fn from_url(url: &str) -> Result<Self, AppError> {
        if url.starts_with("mysql://") || url.starts_with("mysql+") {
            Ok(Self::Mysql)
        } else if url.starts_with("postgres://")
            || url.starts_with("postgresql://")
            || url.starts_with("postgres+")
        {
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

/// A database connection bundling a connection pool with its detected database type.
#[derive(Debug, Clone)]
pub struct DatabaseConnection {
    pool: AnyPool,
    db_type: DbType,
}

impl DatabaseConnection {
    /// Connects to a database using the given URL, auto-detecting the engine type.
    ///
    /// Supports the same URL schemes as [`DbType::from_url`]. Connection errors are
    /// wrapped in [`AppError::ConnectionFailed`] to avoid potentially leaking
    /// credentials embedded in the URL through raw sqlx error messages.
    pub async fn connect(url: &str) -> Result<Self, AppError> {
        let db_type = DbType::from_url(url)?;
        let pool = AnyPool::connect(url).await.map_err(|e| {
            tracing::error!(db_type = %db_type, error = %e, "database connection failed");
            AppError::ConnectionFailed(format!(
                "{db_type}: failed to establish database connection"
            ))
        })?;
        Ok(Self { pool, db_type })
    }

    /// Creates a `DatabaseConnection` from an existing pool. Intended for testing only;
    /// the caller must ensure `db_type` matches the actual database behind `pool`.
    /// Prefer [`connect`](Self::connect) for production use.
    pub fn from_pool(pool: AnyPool, db_type: DbType) -> Self {
        Self { pool, db_type }
    }

    /// Returns a reference to the underlying connection pool.
    pub fn pool(&self) -> &AnyPool {
        &self.pool
    }

    /// Returns the detected database engine type.
    pub fn db_type(&self) -> DbType {
        self.db_type
    }

    /// Fetches column metadata for the given table using a dialect-appropriate query.
    pub async fn describe_table(
        &self,
        table: &ValidatedTableName,
    ) -> Result<Vec<AnyRow>, AppError> {
        let describe = self.db_type.describe_table_query(table);
        match describe {
            DescribeQuery::Parameterized(sql) => {
                Ok(sqlx::query(sql).bind(table.as_str()).fetch_all(&self.pool).await?)
            }
            DescribeQuery::Interpolated(sql) => Ok(sqlx::query(&sql).fetch_all(&self.pool).await?),
        }
    }
}

/// Strips leading SQL comments (`--` line comments and `/* */` block comments)
/// from a query string. Returns the remaining SQL with leading whitespace trimmed.
///
/// A `--` line comment extends to the next newline or end of input (per SQL standard).
/// Nested block comments (`/* /* */ */`) are not supported; only the first `*/`
/// after the opening `/*` is matched. This is safe for downstream classification
/// since the remnant will not match any read-query prefix.
/// Returns an error only for unterminated block comments (`/*` without closing `*/`).
fn strip_leading_sql_comments(sql: &str) -> Result<&str, &'static str> {
    let mut s = sql.trim_start();
    loop {
        if s.starts_with("--") {
            s = match s.find('\n') {
                Some(pos) => s[pos + 1..].trim_start(),
                None => "",
            };
        } else if s.starts_with("/*") {
            s = match s[2..].find("*/") {
                Some(pos) => s[pos + 4..].trim_start(),
                None => return Err("unterminated block comment (/* without closing */)"),
            };
        } else {
            break;
        }
    }
    Ok(s)
}

/// Checks the query for syntactic issues that would cause misclassification.
///
/// Currently detects unterminated block comments (`/* ... */`). Call this before
/// [`is_read_query`] to get a proper error for malformed queries; `is_read_query`
/// conservatively returns `false` on parse failure.
pub fn validate_query_syntax(query: &str) -> Result<(), &'static str> {
    strip_leading_sql_comments(query)?;
    Ok(())
}

/// Checks that a keyword at position `0..prefix_len` is followed by a word boundary
/// (whitespace, `(`, or end of string).
///
/// # Panics
/// Panics if `prefix_len > s.len()`. Callers must ensure
/// `s.starts_with(keyword)` before calling with `keyword.len()`.
fn is_keyword_at_boundary(s: &str, prefix_len: usize) -> bool {
    debug_assert!(prefix_len <= s.len(), "prefix_len exceeds string length");
    if s.len() == prefix_len {
        return true;
    }
    let next = s.as_bytes()[prefix_len];
    next.is_ascii_whitespace() || next == b'('
}

/// Determines whether a SQL query is read-only based on its leading keyword.
/// WITH (CTE) queries are classified by checking whether a DML keyword
/// (INSERT, UPDATE, DELETE, MERGE) appears as a standalone token.
/// Multi-statement queries (containing embedded semicolons) are treated as writes.
///
/// Leading SQL comments (`--` and `/* */`) are stripped before classification.
///
/// Note: semicolons inside string literals would cause a false rejection,
/// but this is an acceptable trade-off for preventing multi-statement injection.
/// Similarly, write keywords appearing inside string literals within a CTE
/// query would cause a false classification as non-read, which is again
/// accepted as a conservative safety trade-off.
pub fn is_read_query(query: &str) -> bool {
    let no_comments = match strip_leading_sql_comments(query) {
        Ok(s) => s,
        // Unterminated comment: conservatively classify as write (non-read).
        // Callers should use validate_query_syntax() first to get a proper error.
        Err(_) => return false,
    };
    let upper = no_comments.to_uppercase();

    if upper.is_empty() {
        return false;
    }

    // Multi-statement queries: reject anything with embedded semicolons.
    // Trailing semicolons (one or more) are stripped before checking.
    let stripped = upper.trim_end_matches(';').trim();
    if stripped.contains(';') {
        return false;
    }

    // EXPLAIN always returns plan rows, so we classify it as read for
    // response-format purposes. EXPLAIN ANALYZE may execute the underlying
    // query as a side effect, but the response is still a result set.
    // This means EXPLAIN ANALYZE of DML (e.g., DELETE) will actually execute
    // the statement, which is accepted since the tool is intended for arbitrary
    // SQL execution including writes (see `execute_sql` tool in server.rs).
    const READ_PREFIXES: &[&str] = &["SELECT", "SHOW", "PRAGMA", "DESCRIBE", "EXPLAIN"];

    if READ_PREFIXES
        .iter()
        .any(|p| stripped.starts_with(p) && is_keyword_at_boundary(stripped, p.len()))
    {
        return true;
    }

    if stripped.starts_with("WITH") && is_keyword_at_boundary(stripped, 4) {
        const WRITE_KEYWORDS: &[&str] = &["INSERT", "UPDATE", "DELETE", "MERGE"];
        return !WRITE_KEYWORDS
            .iter()
            .any(|kw| stripped.split_whitespace().any(|word| word == *kw));
    }

    false
}

/// Converts database rows to a CSV string (RFC 4180 field escaping).
/// Returns an empty string if the input slice is empty.
/// The first line is a header row of column names.
pub fn rows_to_csv(rows: &[AnyRow]) -> String {
    if rows.is_empty() {
        return String::new();
    }

    let columns = rows[0].columns();
    let mut csv = String::new();

    for (i, col) in columns.iter().enumerate() {
        if i > 0 {
            csv.push(',');
        }
        csv.push_str(&escape_csv_field(col.name()));
    }

    for row in rows {
        csv.push('\n');
        for (i, col) in columns.iter().enumerate() {
            if i > 0 {
                csv.push(',');
            }
            csv.push_str(&escape_csv_field(&row_value_to_string(row, col.ordinal())));
        }
    }

    csv
}

/// Escapes a CSV field according to RFC 4180: fields containing commas,
/// double quotes, newlines (`\n`), or carriage returns (`\r`) are enclosed
/// in double quotes, with internal double quotes doubled.
fn escape_csv_field(field: &str) -> Cow<'_, str> {
    if field.contains([',', '"', '\n', '\r']) {
        Cow::Owned(format!("\"{}\"", field.replace('"', "\"\"")))
    } else {
        Cow::Borrowed(field)
    }
}

/// Converts raw bytes to a UTF-8 string, or a placeholder describing the binary data size.
fn bytes_to_string(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes)
        .unwrap_or_else(|e| format!("<binary data: {} bytes>", e.into_bytes().len()))
}

fn row_value_to_string(row: &AnyRow, index: usize) -> String {
    // sqlx Any driver supports limited type decoding.
    // Try types in order: String, i64, f64, bool, Vec<u8> (Blob).
    //
    // MySQL's information_schema columns (e.g. DATA_TYPE, COLUMN_TYPE) are longtext,
    // which MySQL's wire protocol reports as Blob type. The Any driver maps these to
    // AnyValueKind::Blob, so they fail String decoding. We fall through to Vec<u8>
    // and convert the bytes to a UTF-8 string.
    if let Ok(v) = row.try_get::<String, _>(index) {
        return v;
    }
    if let Ok(v) = row.try_get::<i64, _>(index) {
        return v.to_string();
    }
    if let Ok(v) = row.try_get::<f64, _>(index) {
        return v.to_string();
    }
    if let Ok(v) = row.try_get::<bool, _>(index) {
        return v.to_string();
    }
    if let Ok(v) = row.try_get::<Vec<u8>, _>(index) {
        return bytes_to_string(v);
    }
    // Non-Option decodings all failed — the value is likely NULL.
    if let Ok(v) = row.try_get::<Option<String>, _>(index) {
        return v.unwrap_or_else(|| "NULL".to_string());
    }
    if let Ok(v) = row.try_get::<Option<Vec<u8>>, _>(index) {
        return v.map_or_else(|| "NULL".to_string(), bytes_to_string);
    }

    let col_name = row.columns().get(index).map_or("<unknown>", |c| c.name());
    let col_type =
        row.columns().get(index).map(|c| format!("{:?}", c.type_info())).unwrap_or_default();
    tracing::warn!(
        column_index = index,
        column_name = col_name,
        column_type = %col_type,
        "failed to decode column value as any known type"
    );
    "<error: unsupported type>".to_string()
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
    fn test_is_read_query() {
        assert!(is_read_query("SELECT * FROM users"));
        assert!(is_read_query("  select * from users  "));
        assert!(is_read_query("SHOW TABLES"));
        assert!(is_read_query("PRAGMA table_info('users')"));
        assert!(is_read_query("EXPLAIN SELECT 1"));
        assert!(is_read_query("DESCRIBE users"));
        assert!(is_read_query("WITH cte AS (SELECT 1) SELECT * FROM cte"));

        assert!(!is_read_query("INSERT INTO users VALUES (1)"));
        assert!(!is_read_query("UPDATE users SET name = 'x'"));
        assert!(!is_read_query("DELETE FROM users"));
    }

    #[test]
    fn test_is_read_query_trailing_semicolon() {
        assert!(is_read_query("SELECT * FROM users;"));
        assert!(is_read_query("SELECT * FROM users ;"));
    }

    #[test]
    fn test_is_read_query_multi_statement() {
        assert!(!is_read_query("SELECT 1; DROP TABLE users"));
        assert!(!is_read_query("SELECT 1; SELECT 2"));
    }

    #[test]
    fn test_escape_csv_field() {
        assert_eq!(escape_csv_field("hello"), "hello");
        assert_eq!(escape_csv_field("hello,world"), "\"hello,world\"");
        assert_eq!(escape_csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(escape_csv_field("line1\nline2"), "\"line1\nline2\"");
        assert_eq!(escape_csv_field(""), "");
    }

    #[test]
    fn test_is_read_query_empty_and_whitespace() {
        assert!(!is_read_query(""));
        assert!(!is_read_query("   "));
        assert!(!is_read_query("\t\n"));
    }

    #[test]
    fn test_is_read_query_ddl() {
        assert!(!is_read_query("CREATE TABLE t (id INT)"));
        assert!(!is_read_query("ALTER TABLE t ADD COLUMN x INT"));
        assert!(!is_read_query("DROP TABLE t"));
    }

    #[test]
    fn test_is_read_query_cte_with_dml() {
        assert!(!is_read_query("WITH cte AS (SELECT 1) INSERT INTO users SELECT * FROM cte"));
        assert!(!is_read_query("WITH cte AS (SELECT 1) UPDATE users SET id = 1"));
        assert!(!is_read_query("WITH cte AS (SELECT 1) DELETE FROM users"));
    }

    #[test]
    fn test_is_read_query_explain_analyze() {
        assert!(is_read_query("EXPLAIN ANALYZE SELECT 1"));
        assert!(is_read_query("EXPLAIN ANALYZE DELETE FROM users"));
    }

    #[test]
    fn test_is_read_query_multiple_trailing_semicolons() {
        assert!(is_read_query("SELECT 1;;;"));
        // Spaces between semicolons are treated as embedded semicolons (multi-statement).
        assert!(!is_read_query("SELECT 1;  ;"));
    }

    #[test]
    fn test_is_read_query_sql_comments() {
        assert!(is_read_query("-- comment\nSELECT 1"));
        assert!(is_read_query("/* block comment */ SELECT 1"));
        assert!(is_read_query("-- line1\n-- line2\nSELECT 1"));
        assert!(is_read_query("/* comment */ -- another\nSELECT 1"));
        assert!(!is_read_query("-- comment\nINSERT INTO t VALUES (1)"));
    }

    #[test]
    fn test_is_read_query_word_boundary() {
        assert!(!is_read_query("SELECTFOO"));
        assert!(is_read_query("SELECT(1)"));
        assert!(!is_read_query("SHOWING"));
        assert!(is_read_query("SHOW TABLES"));
    }

    #[test]
    fn test_db_type_from_url_mysql_plus_scheme() {
        assert_eq!(
            DbType::from_url("mysql+unix:///var/run/mysqld/mysqld.sock").unwrap(),
            DbType::Mysql
        );
    }

    #[test]
    fn test_db_type_from_url_postgres_plus_scheme() {
        assert_eq!(
            DbType::from_url("postgres+unix:///var/run/postgresql").unwrap(),
            DbType::Postgres
        );
    }

    #[test]
    fn test_bytes_to_string_valid_utf8() {
        let s = bytes_to_string(b"hello".to_vec());
        assert_eq!(s, "hello");
    }

    #[test]
    fn test_bytes_to_string_invalid_utf8() {
        let s = bytes_to_string(vec![0xFF, 0xFE, 0xFD]);
        assert!(s.contains("binary data"));
        assert!(s.contains("3 bytes"));
    }

    #[test]
    fn test_strip_leading_sql_comments_line_comment() {
        assert_eq!(strip_leading_sql_comments("-- comment\nSELECT 1"), Ok("SELECT 1"));
    }

    #[test]
    fn test_strip_leading_sql_comments_block_comment() {
        assert_eq!(strip_leading_sql_comments("/* comment */ SELECT 1"), Ok("SELECT 1"));
    }

    #[test]
    fn test_strip_leading_sql_comments_no_comment() {
        assert_eq!(strip_leading_sql_comments("SELECT 1"), Ok("SELECT 1"));
    }

    #[test]
    fn test_strip_leading_sql_comments_unterminated_block() {
        assert!(strip_leading_sql_comments("/* unterminated").is_err());
    }

    #[test]
    fn test_strip_leading_sql_comments_slash_star_slash_is_unterminated() {
        // "/*/" is an unterminated block comment, NOT a zero-length comment.
        // The `*/` at positions 1-2 overlaps with the opening `/*` at positions 0-1,
        // so it must not be treated as a closing delimiter.
        assert!(strip_leading_sql_comments("/*/ SELECT 1").is_err());
    }

    #[test]
    fn test_strip_leading_sql_comments_line_comment_no_newline() {
        // Per SQL standard, `--` extends to end of input when there is no newline.
        assert_eq!(strip_leading_sql_comments("-- no newline"), Ok(""));
    }

    #[test]
    fn test_validate_query_syntax() {
        assert!(validate_query_syntax("SELECT 1").is_ok());
        assert!(validate_query_syntax("-- comment\nSELECT 1").is_ok());
        assert!(validate_query_syntax("-- no newline").is_ok());
        assert!(validate_query_syntax("/* unterminated").is_err());
    }

    #[test]
    fn test_is_keyword_at_boundary() {
        assert!(is_keyword_at_boundary("SELECT 1", 6));
        assert!(is_keyword_at_boundary("SELECT(1)", 6));
        assert!(is_keyword_at_boundary("SELECT", 6));
        assert!(!is_keyword_at_boundary("SELECTFOO", 6));
    }
}
