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

#[tokio::test]
async fn test_select_query_returns_csv() {
    let db = setup_db().await;

    let csv = db.fetch_all_as_csv("SELECT id, name, email FROM users ORDER BY id").await.unwrap();

    assert!(csv.contains("id,name,email"));
    assert!(csv.contains("1,Alice,alice@example.com"));
    assert!(csv.contains("2,Bob,bob@example.com"));
}

#[tokio::test]
async fn test_insert_affects_rows() {
    let db = setup_db().await;

    let affected = db
        .execute_sql(
            "INSERT INTO users (id, name, email) VALUES (3, 'Charlie', 'charlie@example.com')",
        )
        .await
        .unwrap();

    assert_eq!(affected, 1);

    let csv = db.fetch_all_as_csv("SELECT COUNT(*) as cnt FROM users").await.unwrap();
    assert!(csv.contains("3"));
}

#[tokio::test]
async fn test_update_affects_rows() {
    let db = setup_db().await;

    let affected = db
        .execute_sql("UPDATE users SET email = 'updated@example.com' WHERE id = 1")
        .await
        .unwrap();

    assert_eq!(affected, 1);
}

#[tokio::test]
async fn test_delete_affects_rows() {
    let db = setup_db().await;

    let affected = db.execute_sql("DELETE FROM users WHERE id = 2").await.unwrap();

    assert_eq!(affected, 1);

    let csv = db.fetch_all_as_csv("SELECT COUNT(*) as cnt FROM users").await.unwrap();
    assert!(csv.contains("1"));
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
    let csv = db.describe_table_as_csv(&table).await.unwrap();

    assert!(csv.contains("name"));
    assert!(csv.contains("id"));
    assert!(csv.contains("email"));
}

#[tokio::test]
async fn test_fetch_all_as_csv_empty() {
    let db = setup_db().await;

    db.execute_sql("DELETE FROM users").await.unwrap();
    let csv = db.fetch_all_as_csv("SELECT * FROM users").await.unwrap();
    assert_eq!(csv, "");
}

#[tokio::test]
async fn test_csv_with_null_values() {
    let db = setup_db().await;

    db.execute_sql("INSERT INTO users (id, name, email) VALUES (3, 'NoEmail', NULL)")
        .await
        .unwrap();

    let csv = db.fetch_all_as_csv("SELECT id, name, email FROM users WHERE id = 3").await.unwrap();

    assert!(csv.contains("id,name,email"));
    assert!(csv.contains("3,NoEmail,NULL"));
}

#[tokio::test]
async fn test_resource_read_csv_format() {
    let db = setup_db().await;

    let csv = db.fetch_all_as_csv("SELECT * FROM users LIMIT 100").await.unwrap();

    assert!(csv.starts_with("id,name,email"));
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 3); // header + 2 data rows
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
    let result = db.fetch_all_as_csv("SELECT * FROM nonexistent_table").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_query_syntax_error() {
    let db = setup_db().await;
    let result = db.fetch_all_as_csv("SELEC * FORM users").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_csv_f64_column() {
    let db = setup_db().await;

    db.execute_sql("CREATE TABLE prices (id INTEGER PRIMARY KEY, amount REAL NOT NULL)")
        .await
        .unwrap();
    db.execute_sql("INSERT INTO prices (id, amount) VALUES (1, 9.99)").await.unwrap();

    let csv = db.fetch_all_as_csv("SELECT id, amount FROM prices").await.unwrap();
    assert!(csv.contains("id,amount"));
    assert!(csv.contains("9.99"));
}

#[tokio::test]
async fn test_csv_blob_valid_utf8() {
    let db = setup_db().await;

    db.execute_sql("CREATE TABLE blobs (id INTEGER PRIMARY KEY, data BLOB)").await.unwrap();
    db.execute_sql("INSERT INTO blobs (id, data) VALUES (1, X'68656C6C6F')").await.unwrap();

    let csv = db.fetch_all_as_csv("SELECT id, data FROM blobs").await.unwrap();
    assert!(csv.contains("hello"));
}

#[tokio::test]
async fn test_csv_blob_invalid_utf8() {
    let db = setup_db().await;

    db.execute_sql("CREATE TABLE blobs2 (id INTEGER PRIMARY KEY, data BLOB)").await.unwrap();
    db.execute_sql("INSERT INTO blobs2 (id, data) VALUES (1, X'FFFEFD')").await.unwrap();

    let csv = db.fetch_all_as_csv("SELECT id, data FROM blobs2").await.unwrap();
    assert!(csv.contains("binary data"));
}

#[tokio::test]
async fn test_describe_table_dispatch_sqlite() {
    let db = setup_db().await;
    let table = db::ValidatedTableName::new("users").unwrap();
    let csv = db.describe_table_as_csv(&table).await.unwrap();

    assert!(csv.contains("id"));
    assert!(csv.contains("name"));
}
