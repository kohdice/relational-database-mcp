//! End-to-end tests of the database layer against a real PostgreSQL server.
//!
//! Every test starts its own throwaway `postgres:16-alpine` container, so a
//! container runtime reachable through `DOCKER_HOST` (Docker, or podman exposing
//! its Docker-compatible socket) is required. Because that runtime is not
//! available everywhere and each container costs seconds to boot, every test here
//! carries `#[ignore]` and stays out of the default `cargo test`; `just test-db`
//! runs them via `cargo test -- --include-ignored`.
//!
//! The assertions pin the exact strings the PostgreSQL branch of the row decoder
//! produces, which is the part of the crate no SQLite-backed test can reach: sqlx
//! resolves PostgreSQL type compatibility by OID, so each SQL type accepts exactly
//! one Rust decode target.

use rdb_mcp_core::db::{DatabaseConnection, DbType, ValidatedTableName};
use testcontainers_modules::{
    postgres::Postgres,
    testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner},
};

/// Starts a PostgreSQL container and connects to it.
///
/// The returned [`ContainerAsync`] must stay bound for the whole test: its `Drop`
/// stops and removes the container, which is also the only cleanup this file
/// needs.
// clippy's `allow-unwrap-in-tests` only covers `#[test]`-annotated items, so a shared
// fixture helper in an integration test file needs the exemption spelled out.
#[expect(clippy::unwrap_used, reason = "test fixture: a failed setup should abort the test")]
async fn start_postgres() -> (ContainerAsync<Postgres>, DatabaseConnection) {
    // The tag is pinned rather than left to the module default so that the exact
    // decoded strings asserted below are tied to one known server version.
    let container = Postgres::default().with_tag("16-alpine").start().await.unwrap();
    let host = container.get_host().await.unwrap();
    let port = container.get_host_port_ipv4(5432).await.unwrap();
    let url = format!("postgres://postgres:postgres@{host}:{port}/postgres");
    let db = DatabaseConnection::connect(&url).await.unwrap();

    (container, db)
}

/// Convenience for asserting on a single cell without repeating the Option/&str dance.
fn cell(rows: &[Vec<Option<String>>], row: usize, column: usize) -> Option<&str> {
    rows[row][column].as_deref()
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn integer_primary_key_decodes_to_a_decimal_string() {
    let (_container, db) = start_postgres().await;

    db.execute_sql("CREATE TABLE items (id INTEGER PRIMARY KEY, label TEXT NOT NULL)")
        .await
        .unwrap();
    db.execute_sql("INSERT INTO items (id, label) VALUES (42, 'answer')").await.unwrap();

    let result = db.fetch_streaming("SELECT id, label FROM items", 100).await.unwrap();

    assert_eq!(result.columns, vec!["id", "label"]);
    assert_eq!(result.row_count, 1);
    // INT4 is the regression this file exists for: decoding it through `i64`
    // fails the OID check, which used to turn every such column into an error
    // placeholder rather than the value.
    assert_eq!(cell(&result.rows, 0, 0), Some("42"));
    assert_eq!(cell(&result.rows, 0, 1), Some("answer"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn integer_widths_decode_at_their_boundaries() {
    let (_container, db) = start_postgres().await;

    db.execute_sql("CREATE TABLE ints (id INTEGER PRIMARY KEY, s SMALLINT, i INTEGER, b BIGINT)")
        .await
        .unwrap();
    // The bigint minimum is spelled as a cast literal because PostgreSQL parses
    // `-9223372036854775808` as a negated numeric that overflows int8.
    db.execute_sql(
        "INSERT INTO ints (id, s, i, b) VALUES \
         (1, -32768, -2147483648, '-9223372036854775808'::bigint), \
         (2, 32767, 2147483647, 9223372036854775807)",
    )
    .await
    .unwrap();

    let result = db.fetch_streaming("SELECT s, i, b FROM ints ORDER BY id", 100).await.unwrap();

    assert_eq!(result.row_count, 2);
    assert_eq!(cell(&result.rows, 0, 0), Some("-32768"));
    assert_eq!(cell(&result.rows, 0, 1), Some("-2147483648"));
    assert_eq!(cell(&result.rows, 0, 2), Some("-9223372036854775808"));
    assert_eq!(cell(&result.rows, 1, 0), Some("32767"));
    assert_eq!(cell(&result.rows, 1, 1), Some("2147483647"));
    assert_eq!(cell(&result.rows, 1, 2), Some("9223372036854775807"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn floating_point_and_numeric_types_decode() {
    let (_container, db) = start_postgres().await;

    db.execute_sql(
        "CREATE TABLE numbers (id INTEGER PRIMARY KEY, r REAL, d DOUBLE PRECISION, n NUMERIC(10,2))",
    )
    .await
    .unwrap();
    // Only binary-exact fractions are inserted, so the assertions pin the decoder
    // and not the shortest-round-trip float formatting.
    db.execute_sql(
        "INSERT INTO numbers (id, r, d, n) VALUES (1, 1.5, 2.25, 1234.56), (2, 0.5, 0.125, 10.00)",
    )
    .await
    .unwrap();

    let result = db.fetch_streaming("SELECT r, d, n FROM numbers ORDER BY id", 100).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("1.5"));
    assert_eq!(cell(&result.rows, 0, 1), Some("2.25"));
    assert_eq!(cell(&result.rows, 0, 2), Some("1234.56"));
    assert_eq!(cell(&result.rows, 1, 0), Some("0.5"));
    assert_eq!(cell(&result.rows, 1, 1), Some("0.125"));
    // NUMERIC carries its declared scale through the wire format, so the trailing
    // zeros survive the round trip.
    assert_eq!(cell(&result.rows, 1, 2), Some("10.00"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn numeric_columns_decode_without_precision_loss() {
    let (_container, db) = start_postgres().await;

    db.execute_sql(
        "CREATE TABLE precise_amounts (id INTEGER PRIMARY KEY, amount NUMERIC(65,30) NOT NULL)",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO precise_amounts (id, amount) VALUES \
         (1, 12345678901234567890123456789012345.123456789012345678901234567890)",
    )
    .await
    .unwrap();

    let result =
        db.fetch_streaming("SELECT amount FROM precise_amounts ORDER BY id", 100).await.unwrap();

    // PostgreSQL NUMERIC carries far more significant digits than the 96-bit mantissa
    // of a fixed-width decimal, whose parser rounds the excess away without reporting
    // an error — the value comes back altered rather than failing.
    assert_eq!(
        cell(&result.rows, 0, 0),
        Some("12345678901234567890123456789012345.123456789012345678901234567890")
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn numeric_columns_spell_out_every_digit_of_their_scale() {
    let (_container, db) = start_postgres().await;

    db.execute_sql(
        "CREATE TABLE scaled_amounts (id INTEGER PRIMARY KEY, tiny NUMERIC(20,10), amount NUMERIC(10,2))",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO scaled_amounts (id, tiny, amount) VALUES \
         (1, 0.0000001, 0.00), (2, 0.0000000001, 1234.56)",
    )
    .await
    .unwrap();

    let result = db
        .fetch_streaming("SELECT tiny, amount FROM scaled_amounts ORDER BY id", 100)
        .await
        .unwrap();

    // PostgreSQL coerces a NUMERIC to its declared scale, so these are the digits the
    // server holds; a value below 1e-6 must still be spelled out rather than
    // exponentiated.
    assert_eq!(cell(&result.rows, 0, 0), Some("0.0000001000"));
    assert_eq!(cell(&result.rows, 1, 0), Some("0.0000000001"));
    // Zero carries the column's scale exactly like every other value stored in it.
    assert_eq!(cell(&result.rows, 0, 1), Some("0.00"));
    assert_eq!(cell(&result.rows, 1, 1), Some("1234.56"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn boolean_and_text_types_decode() {
    let (_container, db) = start_postgres().await;

    db.execute_sql(
        "CREATE TABLE texts (id INTEGER PRIMARY KEY, flag BOOLEAN, t TEXT, v VARCHAR(20), c CHAR(5))",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO texts (id, flag, t, v, c) VALUES \
         (1, TRUE, 'text value', 'varchar value', 'ab'), \
         (2, FALSE, '', 'x', 'abcde')",
    )
    .await
    .unwrap();

    let result =
        db.fetch_streaming("SELECT flag, t, v, c FROM texts ORDER BY id", 100).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("true"));
    assert_eq!(cell(&result.rows, 0, 1), Some("text value"));
    assert_eq!(cell(&result.rows, 0, 2), Some("varchar value"));
    // CHAR(n) is blank-padded by PostgreSQL, and the decoder returns the stored
    // value verbatim rather than trimming it.
    assert_eq!(cell(&result.rows, 0, 3), Some("ab   "));
    assert_eq!(cell(&result.rows, 1, 0), Some("false"));
    assert_eq!(cell(&result.rows, 1, 1), Some(""));
    assert_eq!(cell(&result.rows, 1, 2), Some("x"));
    assert_eq!(cell(&result.rows, 1, 3), Some("abcde"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn uuid_and_json_types_decode() {
    let (_container, db) = start_postgres().await;

    db.execute_sql("CREATE TABLE docs (id UUID PRIMARY KEY, j JSON, b JSONB)").await.unwrap();
    db.execute_sql(
        "INSERT INTO docs (id, j, b) VALUES \
         ('550E8400-E29B-41D4-A716-446655440000', '{\"a\": 1}', '{\"b\": [1, 2]}')",
    )
    .await
    .unwrap();

    let result = db.fetch_streaming("SELECT id, j, b FROM docs", 100).await.unwrap();

    // `Uuid`'s Display is the lowercase hyphenated form regardless of input case.
    assert_eq!(cell(&result.rows, 0, 0), Some("550e8400-e29b-41d4-a716-446655440000"));
    // Both JSON flavours are re-serialized by `serde_json`, so the stored
    // whitespace is dropped.
    assert_eq!(cell(&result.rows, 0, 1), Some(r#"{"a":1}"#));
    assert_eq!(cell(&result.rows, 0, 2), Some(r#"{"b":[1,2]}"#));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn date_and_time_types_decode() {
    let (_container, db) = start_postgres().await;

    db.execute_sql(
        "CREATE TABLE moments (id INTEGER PRIMARY KEY, d DATE, t TIME, ts TIMESTAMP, tz TIMESTAMPTZ)",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO moments (id, d, t, ts, tz) VALUES \
         (1, '2024-03-15', '12:34:56', '2024-03-15 12:34:56', '2024-03-15 12:34:56+00')",
    )
    .await
    .unwrap();

    let result = db.fetch_streaming("SELECT d, t, ts, tz FROM moments", 100).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("2024-03-15"));
    assert_eq!(cell(&result.rows, 0, 1), Some("12:34:56"));
    assert_eq!(cell(&result.rows, 0, 2), Some("2024-03-15 12:34:56"));
    // TIMESTAMPTZ decodes into `DateTime<Utc>`, whose Display appends the zone,
    // so the rendering differs from the otherwise identical TIMESTAMP above.
    assert_eq!(cell(&result.rows, 0, 3), Some("2024-03-15 12:34:56 UTC"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn bytea_decodes_utf8_verbatim_and_other_bytes_as_base64() {
    let (_container, db) = start_postgres().await;

    db.execute_sql("CREATE TABLE blobs (id INTEGER PRIMARY KEY, data BYTEA)").await.unwrap();
    db.execute_sql(
        "INSERT INTO blobs (id, data) VALUES \
         (1, '\\x68656c6c6f'::bytea), (2, '\\xfffefd'::bytea)",
    )
    .await
    .unwrap();

    let result = db.fetch_streaming("SELECT data FROM blobs ORDER BY id", 100).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("hello"));
    assert_eq!(cell(&result.rows, 1, 0), Some("base64://79"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn sql_null_decodes_to_none() {
    let (_container, db) = start_postgres().await;

    db.execute_sql(
        "CREATE TABLE nullable (id INTEGER PRIMARY KEY, note TEXT, amount NUMERIC(10,2))",
    )
    .await
    .unwrap();
    db.execute_sql("INSERT INTO nullable (id, note, amount) VALUES (1, NULL, NULL)").await.unwrap();

    let result = db.fetch_streaming("SELECT id, note, amount FROM nullable", 100).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("1"));
    assert_eq!(cell(&result.rows, 0, 1), None);
    assert_eq!(cell(&result.rows, 0, 2), None);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn user_defined_enum_decodes_to_its_label() {
    let (_container, db) = start_postgres().await;

    db.execute_sql("CREATE TYPE mood AS ENUM ('sad', 'ok', 'happy')").await.unwrap();
    db.execute_sql("CREATE TABLE feelings (id INTEGER PRIMARY KEY, current mood NOT NULL)")
        .await
        .unwrap();
    db.execute_sql("INSERT INTO feelings (id, current) VALUES (1, 'happy'), (2, 'sad')")
        .await
        .unwrap();

    let result = db.fetch_streaming("SELECT current FROM feelings ORDER BY id", 100).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("happy"));
    assert_eq!(cell(&result.rows, 1, 0), Some("sad"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn list_tables_query_returns_public_schema_tables() {
    let (_container, db) = start_postgres().await;

    assert_eq!(db.db_type(), DbType::Postgres);
    db.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL)")
        .await
        .unwrap();
    db.execute_sql("CREATE TABLE orders (id INTEGER PRIMARY KEY, user_id INTEGER)").await.unwrap();

    let sql = DbType::Postgres.list_tables_query();
    let tables = db.fetch_column_as_strings(sql).await.unwrap();

    // The query is ordered by table_name and scoped to the `public` schema, so
    // the server's own catalogs are absent.
    assert_eq!(tables, vec!["orders", "users"]);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn describe_table_returns_column_metadata() {
    let (_container, db) = start_postgres().await;

    db.execute_sql(
        "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, email VARCHAR(255))",
    )
    .await
    .unwrap();

    let table = ValidatedTableName::new("users").unwrap();
    let result = db.describe_table(&table).await.unwrap();

    assert_eq!(
        result.columns,
        vec!["name", "data_type", "is_nullable", "column_default", "primary_key"]
    );
    let described: Vec<Option<&str>> = result.rows.iter().map(|row| row[0].as_deref()).collect();
    assert_eq!(described, vec![Some("id"), Some("name"), Some("email")]);
    // `information_schema` columns are domains over text and varchar; decoding
    // them proves the decoder resolves a domain to its base type. The type names
    // stay PostgreSQL's own — `data_type` is not normalized across engines.
    let types: Vec<Option<&str>> = result.rows.iter().map(|row| row[1].as_deref()).collect();
    assert_eq!(types, vec![Some("integer"), Some("text"), Some("character varying")]);
    assert_eq!(cell(&result.rows, 0, 2), Some("NO"));
    assert_eq!(cell(&result.rows, 2, 2), Some("YES"));
    assert_eq!(cell(&result.rows, 1, 3), None);
    let primary_key: Vec<Option<&str>> = result.rows.iter().map(|row| row[4].as_deref()).collect();
    assert_eq!(primary_key, vec![Some("YES"), Some("NO"), Some("NO")]);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn describe_table_reports_every_column_of_a_composite_primary_key() {
    let (_container, db) = start_postgres().await;

    // The primary key is joined in from two constraint views, so a key spanning
    // more than one column is where that join would drop or duplicate rows.
    db.execute_sql(
        "CREATE TABLE memberships (
            team_id INTEGER NOT NULL,
            user_id INTEGER NOT NULL,
            role TEXT,
            PRIMARY KEY (team_id, user_id)
        )",
    )
    .await
    .unwrap();

    let table = ValidatedTableName::new("memberships").unwrap();
    let result = db.describe_table(&table).await.unwrap();

    assert_eq!(result.row_count, 3);
    let primary_key: Vec<Option<&str>> = result.rows.iter().map(|row| row[4].as_deref()).collect();
    assert_eq!(primary_key, vec![Some("YES"), Some("YES"), Some("NO")]);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn fetch_streaming_truncates_only_beyond_the_row_limit() {
    let (_container, db) = start_postgres().await;

    db.execute_sql("CREATE TABLE numbers (id INTEGER PRIMARY KEY)").await.unwrap();
    db.execute_sql("INSERT INTO numbers (id) VALUES (1), (2), (3)").await.unwrap();

    let cut = db.fetch_streaming("SELECT id FROM numbers ORDER BY id", 2).await.unwrap();
    assert!(cut.truncated);
    assert_eq!(cut.row_count, 2);
    assert_eq!(cell(&cut.rows, 1, 0), Some("2"));

    let exact = db.fetch_streaming("SELECT id FROM numbers ORDER BY id", 3).await.unwrap();
    assert!(!exact.truncated, "a result that ends at the limit was not truncated");
    assert_eq!(exact.row_count, 3);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn empty_result_reports_column_names() {
    let (_container, db) = start_postgres().await;

    db.execute_sql("CREATE TABLE numbers (id INTEGER PRIMARY KEY, label TEXT NOT NULL)")
        .await
        .unwrap();
    db.execute_sql("INSERT INTO numbers (id, label) VALUES (1, 'one')").await.unwrap();

    let result =
        db.fetch_streaming("SELECT id, label FROM numbers WHERE id = 999", 100).await.unwrap();

    // No row reaches the decoder, so these names can only come from the prepared
    // statement the empty-result path falls back to.
    assert_eq!(result.columns, vec!["id", "label"]);
    assert_eq!(result.row_count, 0);
    assert!(result.rows.is_empty());
}
