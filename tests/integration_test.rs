use relational_database_mcp::db::{self, DatabaseConnection, DbType};
use sqlx::{AnyPool, Row, pool::PoolOptions};

async fn setup_pool() -> AnyPool {
    sqlx::any::install_default_drivers();
    // max_connections(1): SQLite :memory: creates a separate DB per connection.
    // Restricting to 1 connection ensures all operations share the same DB.
    let pool = PoolOptions::new().max_connections(1).connect("sqlite::memory:").await.unwrap();

    sqlx::query("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email TEXT)")
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("INSERT INTO users (id, name, email) VALUES (1, 'Alice', 'alice@example.com')")
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("INSERT INTO users (id, name, email) VALUES (2, 'Bob', 'bob@example.com')")
        .execute(&pool)
        .await
        .unwrap();

    pool
}

#[tokio::test]
async fn test_select_query_returns_csv() {
    let pool = setup_pool().await;

    let rows = sqlx::query("SELECT id, name, email FROM users ORDER BY id")
        .fetch_all(&pool)
        .await
        .unwrap();
    let csv = db::rows_to_csv(&rows);

    assert!(csv.contains("id,name,email"));
    assert!(csv.contains("1,Alice,alice@example.com"));
    assert!(csv.contains("2,Bob,bob@example.com"));
}

#[tokio::test]
async fn test_insert_affects_rows() {
    let pool = setup_pool().await;

    let result = sqlx::query(
        "INSERT INTO users (id, name, email) VALUES (3, 'Charlie', 'charlie@example.com')",
    )
    .execute(&pool)
    .await
    .unwrap();

    assert_eq!(result.rows_affected(), 1);

    let rows = sqlx::query("SELECT COUNT(*) as cnt FROM users").fetch_all(&pool).await.unwrap();
    let csv = db::rows_to_csv(&rows);
    assert!(csv.contains("3"));
}

#[tokio::test]
async fn test_update_affects_rows() {
    let pool = setup_pool().await;

    let result = sqlx::query("UPDATE users SET email = 'updated@example.com' WHERE id = 1")
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(result.rows_affected(), 1);
}

#[tokio::test]
async fn test_delete_affects_rows() {
    let pool = setup_pool().await;

    let result = sqlx::query("DELETE FROM users WHERE id = 2").execute(&pool).await.unwrap();

    assert_eq!(result.rows_affected(), 1);

    let rows = sqlx::query("SELECT COUNT(*) as cnt FROM users").fetch_all(&pool).await.unwrap();
    let csv = db::rows_to_csv(&rows);
    assert!(csv.contains("1"));
}

#[tokio::test]
async fn test_list_tables_sqlite() {
    let pool = setup_pool().await;

    let sql = DbType::Sqlite.list_tables_query();
    let rows = sqlx::query(sql).fetch_all(&pool).await.unwrap();
    let tables: Vec<String> = rows.iter().map(|row| row.try_get::<String, _>(0).unwrap()).collect();

    assert_eq!(tables, vec!["users"]);
}

#[tokio::test]
async fn test_describe_table_sqlite() {
    let pool = setup_pool().await;

    let rows = sqlx::query("PRAGMA table_info('users')").fetch_all(&pool).await.unwrap();
    let csv = db::rows_to_csv(&rows);

    assert!(csv.contains("name"));
    assert!(csv.contains("id"));
    assert!(csv.contains("email"));
}

#[tokio::test]
async fn test_rows_to_csv_empty() {
    let csv = db::rows_to_csv(&[]);
    assert_eq!(csv, "");
}

#[tokio::test]
async fn test_rows_to_csv_with_null_values() {
    let pool = setup_pool().await;

    sqlx::query("INSERT INTO users (id, name, email) VALUES (3, 'NoEmail', NULL)")
        .execute(&pool)
        .await
        .unwrap();

    let rows = sqlx::query("SELECT id, name, email FROM users WHERE id = 3")
        .fetch_all(&pool)
        .await
        .unwrap();
    let csv = db::rows_to_csv(&rows);

    assert!(csv.contains("id,name,email"));
    assert!(csv.contains("3,NoEmail,NULL"));
}

#[tokio::test]
async fn test_resource_read_csv_format() {
    let pool = setup_pool().await;

    let rows = sqlx::query("SELECT * FROM users LIMIT 100").fetch_all(&pool).await.unwrap();
    let csv = db::rows_to_csv(&rows);

    assert!(csv.starts_with("id,name,email"));
    let lines: Vec<&str> = csv.lines().collect();
    assert_eq!(lines.len(), 3); // header + 2 data rows
}

#[tokio::test]
async fn test_list_multiple_tables() {
    let pool = setup_pool().await;

    sqlx::query("CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER, total REAL)")
        .execute(&pool)
        .await
        .unwrap();

    let sql = DbType::Sqlite.list_tables_query();
    let rows = sqlx::query(sql).fetch_all(&pool).await.unwrap();
    let tables: Vec<String> = rows.iter().map(|row| row.try_get::<String, _>(0).unwrap()).collect();

    assert!(tables.iter().any(|t| t == "users"));
    assert!(tables.iter().any(|t| t == "orders"));
    assert_eq!(tables.len(), 2);
}

#[tokio::test]
async fn test_database_connection_sqlite_memory() {
    sqlx::any::install_default_drivers();
    let db = DatabaseConnection::connect("sqlite::memory:").await;
    assert!(db.is_ok());
    let db = db.unwrap();
    assert_eq!(db.db_type(), DbType::Sqlite);
}

#[tokio::test]
async fn test_database_connection_invalid_url() {
    sqlx::any::install_default_drivers();
    let db = DatabaseConnection::connect("invalid://url").await;
    assert!(db.is_err());
}

#[tokio::test]
async fn test_query_nonexistent_table() {
    let pool = setup_pool().await;
    let result = sqlx::query("SELECT * FROM nonexistent_table").fetch_all(&pool).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_query_syntax_error() {
    let pool = setup_pool().await;
    let result = sqlx::query("SELEC * FORM users").fetch_all(&pool).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_rows_to_csv_f64_column() {
    let pool = setup_pool().await;

    sqlx::query("CREATE TABLE prices (id INTEGER PRIMARY KEY, amount REAL NOT NULL)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO prices (id, amount) VALUES (1, 9.99)").execute(&pool).await.unwrap();

    let rows = sqlx::query("SELECT id, amount FROM prices").fetch_all(&pool).await.unwrap();
    let csv = db::rows_to_csv(&rows);
    assert!(csv.contains("id,amount"));
    assert!(csv.contains("9.99"));
}

#[tokio::test]
async fn test_rows_to_csv_blob_valid_utf8() {
    let pool = setup_pool().await;

    sqlx::query("CREATE TABLE blobs (id INTEGER PRIMARY KEY, data BLOB)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO blobs (id, data) VALUES (1, X'68656C6C6F')")
        .execute(&pool)
        .await
        .unwrap();

    let rows = sqlx::query("SELECT id, data FROM blobs").fetch_all(&pool).await.unwrap();
    let csv = db::rows_to_csv(&rows);
    assert!(csv.contains("hello"));
}

#[tokio::test]
async fn test_rows_to_csv_blob_invalid_utf8() {
    let pool = setup_pool().await;

    sqlx::query("CREATE TABLE blobs2 (id INTEGER PRIMARY KEY, data BLOB)")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO blobs2 (id, data) VALUES (1, X'FFFEFD')")
        .execute(&pool)
        .await
        .unwrap();

    let rows = sqlx::query("SELECT id, data FROM blobs2").fetch_all(&pool).await.unwrap();
    let csv = db::rows_to_csv(&rows);
    assert!(csv.contains("binary data"));
}

#[tokio::test]
async fn test_describe_table_dispatch_sqlite() {
    let pool = setup_pool().await;
    let db = DatabaseConnection::from_pool(pool, DbType::Sqlite);
    let table = db::ValidatedTableName::new("users").unwrap();
    let rows = db.describe_table(&table).await.unwrap();
    let csv = db::rows_to_csv(&rows);

    assert!(csv.contains("id"));
    assert!(csv.contains("name"));
}
