//! End-to-end tests of the database layer against an in-memory SQLite database.

use rdb_mcp_core::db::{self, DatabaseConnection, DbType};
use sqlx::pool::PoolOptions;

// clippy's `allow-unwrap-in-tests` only covers `#[test]`-annotated items, so a shared
// fixture helper in an integration test file needs the exemption spelled out.
#[expect(clippy::unwrap_used, reason = "test fixture: a failed setup should abort the test")]
async fn setup_db() -> DatabaseConnection {
    // max_connections(1): SQLite :memory: creates a separate DB per connection.
    // Restricting to 1 connection ensures all operations share the same DB.
    let pool = PoolOptions::<sqlx::Sqlite>::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();

    let db = DatabaseConnection::from_sqlite_pool(pool);

    db.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT)")
        .await
        .unwrap();

    db.execute_sql("INSERT INTO users (id, name, email) VALUES (1, 'Alice', 'alice@example.com')")
        .await
        .unwrap();

    db.execute_sql("INSERT INTO users (id, name, email) VALUES (2, 'Bob', 'bob@example.com')")
        .await
        .unwrap();

    db
}

/// Convenience for asserting on a single cell without repeating the Option/&str dance.
fn cell(rows: &[Vec<Option<String>>], row: usize, column: usize) -> Option<&str> {
    rows[row][column].as_deref()
}

#[tokio::test]
async fn test_select_query_returns_columns_and_rows() {
    let db = setup_db().await;

    let result =
        db.fetch_streaming("SELECT id, name, email FROM users ORDER BY id", 100).await.unwrap();

    assert_eq!(result.columns, vec!["id", "name", "email"]);
    assert_eq!(result.row_count, 2);
    assert!(!result.truncated);
    assert_eq!(cell(&result.rows, 0, 0), Some("1"));
    assert_eq!(cell(&result.rows, 0, 1), Some("Alice"));
    assert_eq!(cell(&result.rows, 0, 2), Some("alice@example.com"));
    assert_eq!(cell(&result.rows, 1, 1), Some("Bob"));
    assert_eq!(cell(&result.rows, 1, 2), Some("bob@example.com"));
}

#[tokio::test]
async fn test_insert_affects_rows() {
    let db = setup_db().await;

    let result = db
        .execute_sql(
            "INSERT INTO users (id, name, email) VALUES (3, 'Charlie', 'charlie@example.com')",
        )
        .await
        .unwrap();

    assert_eq!(result.rows_affected, 1);

    let count = db.fetch_streaming("SELECT COUNT(*) as cnt FROM users", 100).await.unwrap();
    assert_eq!(cell(&count.rows, 0, 0), Some("3"));
}

#[tokio::test]
async fn test_update_affects_rows() {
    let db = setup_db().await;

    let result = db
        .execute_sql("UPDATE users SET email = 'updated@example.com' WHERE id = 1")
        .await
        .unwrap();

    assert_eq!(result.rows_affected, 1);
}

#[tokio::test]
async fn test_delete_affects_rows() {
    let db = setup_db().await;

    let result = db.execute_sql("DELETE FROM users WHERE id = 2").await.unwrap();

    assert_eq!(result.rows_affected, 1);

    let count = db.fetch_streaming("SELECT COUNT(*) as cnt FROM users", 100).await.unwrap();
    assert_eq!(cell(&count.rows, 0, 0), Some("1"));
}

#[tokio::test]
async fn test_list_tables_sqlite() {
    let db = setup_db().await;

    let sql = DbType::Sqlite.list_tables_query();
    let tables = db.fetch_column_as_strings(sql).await.unwrap();

    assert_eq!(tables, vec!["users"]);
}

#[tokio::test]
async fn test_describe_table_sqlite() {
    let db = setup_db().await;

    let table = db::ValidatedTableName::new("users").unwrap();
    let result = db.describe_table(&table).await.unwrap();

    // PRAGMA table_info returns one row per column, with the column name in `name`.
    let name_index = result.columns.iter().position(|c| c == "name").unwrap();
    let described: Vec<Option<&str>> =
        result.rows.iter().map(|row| row[name_index].as_deref()).collect();
    assert_eq!(described, vec![Some("id"), Some("name"), Some("email")]);
}

#[tokio::test]
async fn test_fetch_streaming_empty_result() {
    let db = setup_db().await;

    db.execute_sql("DELETE FROM users").await.unwrap();
    let result = db.fetch_streaming("SELECT * FROM users", 100).await.unwrap();

    assert!(result.is_empty());
    assert_eq!(result.row_count, 0);
    assert!(result.rows.is_empty());
    // Column metadata comes from the returned rows, so an empty result set has none.
    assert!(result.columns.is_empty());
}

#[tokio::test]
async fn test_null_values_decode_to_none() {
    let db = setup_db().await;

    db.execute_sql("INSERT INTO users (id, name, email) VALUES (3, 'NoEmail', NULL)")
        .await
        .unwrap();

    let result =
        db.fetch_streaming("SELECT id, name, email FROM users WHERE id = 3", 100).await.unwrap();

    assert_eq!(result.columns, vec!["id", "name", "email"]);
    assert_eq!(result.row_count, 1);
    assert_eq!(cell(&result.rows, 0, 1), Some("NoEmail"));
    assert_eq!(cell(&result.rows, 0, 2), None);
}

#[tokio::test]
async fn test_fetch_streaming_truncates_at_max_rows() {
    let db = setup_db().await;

    let result = db.fetch_streaming("SELECT id FROM users ORDER BY id", 1).await.unwrap();

    assert!(result.truncated);
    assert_eq!(result.row_count, 1);
    assert_eq!(cell(&result.rows, 0, 0), Some("1"));
}

#[tokio::test]
async fn test_fetch_streaming_not_truncated_under_limit() {
    let db = setup_db().await;

    let result = db.fetch_streaming("SELECT id FROM users ORDER BY id", 10).await.unwrap();

    assert!(!result.truncated);
    assert_eq!(result.row_count, 2);
}

#[tokio::test]
async fn test_fetch_streaming_exact_limit_is_not_truncated() {
    let db = setup_db().await;

    // The fixture holds exactly 2 rows, so a limit of 2 cuts nothing short.
    let result = db.fetch_streaming("SELECT id FROM users ORDER BY id", 2).await.unwrap();

    assert_eq!(result.row_count, 2);
    assert!(!result.truncated, "a result that ends at the limit was not truncated");
}

#[tokio::test]
async fn test_fetch_streaming_zero_max_rows_returns_no_rows() {
    let db = setup_db().await;

    let result = db.fetch_streaming("SELECT id FROM users ORDER BY id", 0).await.unwrap();

    assert_eq!(result.row_count, 0);
    assert!(result.truncated, "rows exist beyond a zero limit");
    // Column metadata comes from a kept row, so a zero limit reports none.
    assert!(result.columns.is_empty());
}

#[tokio::test]
async fn test_query_result_serializes_null_as_json_null() {
    let db = setup_db().await;

    db.execute_sql("INSERT INTO users (id, name, email) VALUES (3, 'NoEmail', NULL)")
        .await
        .unwrap();

    let result = db.fetch_streaming("SELECT email FROM users WHERE id = 3", 100).await.unwrap();
    let json = serde_json::to_value(&result).unwrap();

    assert_eq!(json["columns"], serde_json::json!(["email"]));
    assert_eq!(json["rows"], serde_json::json!([[serde_json::Value::Null]]));
    assert_eq!(json["row_count"], 1);
    assert_eq!(json["truncated"], false);
}

#[tokio::test]
async fn test_list_multiple_tables() {
    let db = setup_db().await;

    db.execute_sql("CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, total REAL)")
        .await
        .unwrap();

    let sql = DbType::Sqlite.list_tables_query();
    let tables = db.fetch_column_as_strings(sql).await.unwrap();

    assert!(tables.iter().any(|t| t == "users"));
    assert!(tables.iter().any(|t| t == "orders"));
    assert_eq!(tables.len(), 2);
}

#[tokio::test]
async fn test_database_connection_sqlite_memory() {
    let db = DatabaseConnection::connect("sqlite::memory:").await;
    assert!(db.is_ok());
    let db = db.unwrap();
    assert_eq!(db.db_type(), DbType::Sqlite);
}

#[tokio::test]
async fn test_database_connection_invalid_url() {
    let db = DatabaseConnection::connect("invalid://url").await;
    assert!(db.is_err());
}

#[tokio::test]
async fn test_query_nonexistent_table() {
    let db = setup_db().await;
    let result = db.fetch_streaming("SELECT * FROM nonexistent_table", 100).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_query_syntax_error() {
    let db = setup_db().await;
    let result = db.fetch_streaming("SELEC * FORM users", 100).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_f64_column() {
    let db = setup_db().await;

    db.execute_sql("CREATE TABLE prices (id INTEGER PRIMARY KEY, amount REAL NOT NULL)")
        .await
        .unwrap();
    db.execute_sql("INSERT INTO prices (id, amount) VALUES (1, 9.99)").await.unwrap();

    let result = db.fetch_streaming("SELECT id, amount FROM prices", 100).await.unwrap();

    assert_eq!(result.columns, vec!["id", "amount"]);
    assert_eq!(cell(&result.rows, 0, 1), Some("9.99"));
}

#[tokio::test]
async fn test_blob_valid_utf8() {
    let db = setup_db().await;

    db.execute_sql("CREATE TABLE blobs (id INTEGER PRIMARY KEY, data BLOB)").await.unwrap();
    db.execute_sql("INSERT INTO blobs (id, data) VALUES (1, X'68656C6C6F')").await.unwrap();

    let result = db.fetch_streaming("SELECT id, data FROM blobs", 100).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 1), Some("hello"));
}

#[tokio::test]
async fn test_blob_invalid_utf8() {
    let db = setup_db().await;

    db.execute_sql("CREATE TABLE blobs2 (id INTEGER PRIMARY KEY, data BLOB)").await.unwrap();
    db.execute_sql("INSERT INTO blobs2 (id, data) VALUES (1, X'FFFEFD')").await.unwrap();

    let result = db.fetch_streaming("SELECT id, data FROM blobs2", 100).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 1), Some("base64://79"));
}

#[tokio::test]
async fn test_describe_table_dispatch_sqlite() {
    let db = setup_db().await;
    let table = db::ValidatedTableName::new("users").unwrap();
    let result = db.describe_table(&table).await.unwrap();

    assert!(!result.is_empty());
    assert!(result.columns.iter().any(|c| c == "name"));
    assert!(result.columns.iter().any(|c| c == "type"));
}
