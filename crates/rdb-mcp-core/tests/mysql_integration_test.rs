//! End-to-end tests of the database layer against a real MySQL server.
//!
//! Every test here starts a MySQL container, so each one needs a container runtime
//! (Docker or Podman) reachable through `DOCKER_HOST` and takes tens of seconds.
//! They are therefore `#[ignore]`d: a plain `cargo test` stays fast and passes on a
//! machine with no runtime at all. Run them with `just test-db`.
//!
//! The container is stopped and removed by `ContainerAsync`'s `Drop`, which is why
//! the handle is kept in the fixture for the whole test rather than discarded.

use std::time::Duration;

use rdb_mcp_core::db::{DatabaseConnection, DbType, ValidatedTableName};
use testcontainers_modules::{
    mysql::Mysql,
    testcontainers::{ContainerAsync, ImageExt, runners::AsyncRunner},
};
use tokio::sync::{Semaphore, SemaphorePermit};

/// Pinned so the expected strings below describe one known server version.
const IMAGE_TAG: &str = "8.4";

/// The test harness runs these tests in parallel, but a MySQL container needs a few
/// hundred megabytes inside the container runtime's VM (2 GB by default on Podman's
/// macOS machine), so unrestricted parallelism exhausts it. Containers are capped
/// instead of tests, because a fixture holds its slot only while its container lives.
static CONTAINER_SLOTS: Semaphore = Semaphore::const_new(2);

/// A running MySQL container together with a connection to its `test` database.
///
/// Field order is the drop order: the container is removed before the slot that
/// bounds how many containers exist at once is handed to the next waiting test.
struct MysqlFixture {
    db: DatabaseConnection,
    host: String,
    port: u16,
    _container: ContainerAsync<Mysql>,
    _slot: SemaphorePermit<'static>,
}

// clippy's `allow-unwrap-in-tests` only covers `#[test]`-annotated items, so a shared
// fixture helper in an integration test file needs the exemption spelled out.
#[expect(clippy::unwrap_used, reason = "test fixture: a failed setup should abort the test")]
async fn start_mysql() -> MysqlFixture {
    // `DatabaseConnection::connect` reports the sqlx cause only through `tracing::error!`,
    // masking it in the returned error to keep URL credentials out of client-facing
    // messages. Without a subscriber that cause is dropped, so a connection failure here
    // would be indistinguishable from any other. `try_init` tolerates the repeat calls the
    // other tests in this file make.
    let _ = tracing_subscriber::fmt().with_test_writer().try_init();

    let slot = CONTAINER_SLOTS.acquire().await.unwrap();
    let container = Mysql::default().with_tag(IMAGE_TAG).start().await.unwrap();
    let host = container.get_host().await.unwrap().to_string();
    let port = container.get_host_port_ipv4(3306).await.unwrap();

    // The module boots the server with an empty root password and a `test` database.
    let url = format!("mysql://root@{host}:{port}/test");

    MysqlFixture {
        db: connect_with_retry(&url).await,
        host,
        port,
        _container: container,
        _slot: slot,
    }
}

/// Number of connection attempts before a failure is reported as a test failure.
const CONNECT_ATTEMPTS: u32 = 30;

/// Connects to `url`, retrying while the server refuses connections.
///
/// The MySQL entrypoint logs "ready for connections" once for the temporary server it
/// uses to initialize the data directory and shuts that one down again, so the log
/// line the image waits for can be observed while the port is still closed.
#[expect(clippy::unwrap_used, reason = "test fixture: a failed setup should abort the test")]
async fn connect_with_retry(url: &str) -> DatabaseConnection {
    for _ in 1..CONNECT_ATTEMPTS {
        if let Ok(db) = DatabaseConnection::connect(url).await {
            return db;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    // The last attempt is not swallowed, so a permanent failure names its cause.
    DatabaseConnection::connect(url).await.unwrap()
}

/// Convenience for asserting on a single cell without repeating the Option/&str dance.
fn cell(rows: &[Vec<Option<String>>], row: usize, column: usize) -> Option<&str> {
    rows[row][column].as_deref()
}

/// Returns one column of a result set, in row order.
fn column(rows: &[Vec<Option<String>>], index: usize) -> Vec<Option<&str>> {
    rows.iter().map(|row| row[index].as_deref()).collect()
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn unsigned_integer_columns_decode_as_decimal_strings() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql(
        "CREATE TABLE unsigned_ints (
            id INT UNSIGNED PRIMARY KEY,
            tiny TINYINT UNSIGNED NOT NULL,
            small SMALLINT UNSIGNED NOT NULL,
            medium MEDIUMINT UNSIGNED NOT NULL,
            big BIGINT UNSIGNED NOT NULL
        )",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO unsigned_ints (id, tiny, small, medium, big) VALUES
            (0, 0, 0, 0, 0),
            (42, 42, 42, 42, 42),
            (200, 200, 200, 200, 200),
            (4294967295, 255, 65535, 16777215, 18446744073709551615)",
    )
    .await
    .unwrap();

    let result = db
        .fetch_streaming("SELECT id, tiny, small, medium, big FROM unsigned_ints ORDER BY id", 100)
        .await
        .unwrap();

    // sqlx's MySQL `bool` accepts UNSIGNED columns while its signed integer types
    // reject them, so a decoder that tried types in order rendered these four ids as
    // "false", "true", a decode error, and a decode error.
    assert_eq!(
        column(&result.rows, 0),
        vec![Some("0"), Some("42"), Some("200"), Some("4294967295")]
    );
    assert_eq!(cell(&result.rows, 3, 1), Some("255"));
    assert_eq!(cell(&result.rows, 3, 2), Some("65535"));
    assert_eq!(cell(&result.rows, 3, 3), Some("16777215"));
    assert_eq!(cell(&result.rows, 3, 4), Some("18446744073709551615"));
    assert_eq!(cell(&result.rows, 0, 4), Some("0"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn signed_integer_columns_decode_across_their_full_range() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql(
        "CREATE TABLE signed_ints (
            tiny TINYINT PRIMARY KEY,
            small SMALLINT NOT NULL,
            medium MEDIUMINT NOT NULL,
            plain INT NOT NULL,
            big BIGINT NOT NULL
        )",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO signed_ints (tiny, small, medium, plain, big) VALUES
            (-128, -32768, -8388608, -2147483648, -9223372036854775808),
            (0, 0, 0, 0, 0),
            (127, 32767, 8388607, 2147483647, 9223372036854775807)",
    )
    .await
    .unwrap();

    let result = db
        .fetch_streaming(
            "SELECT tiny, small, medium, plain, big FROM signed_ints ORDER BY tiny",
            100,
        )
        .await
        .unwrap();

    assert_eq!(column(&result.rows, 0), vec![Some("-128"), Some("0"), Some("127")]);
    assert_eq!(column(&result.rows, 1), vec![Some("-32768"), Some("0"), Some("32767")]);
    assert_eq!(column(&result.rows, 2), vec![Some("-8388608"), Some("0"), Some("8388607")]);
    assert_eq!(column(&result.rows, 3), vec![Some("-2147483648"), Some("0"), Some("2147483647")]);
    assert_eq!(
        column(&result.rows, 4),
        vec![Some("-9223372036854775808"), Some("0"), Some("9223372036854775807")]
    );
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn boolean_bit_and_year_columns_decode_by_their_own_rules() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql(
        "CREATE TABLE flags (
            label VARCHAR(16) PRIMARY KEY,
            flag BOOLEAN NOT NULL,
            counter TINYINT UNSIGNED NOT NULL,
            bits BIT(8) NOT NULL,
            released YEAR NOT NULL
        )",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO flags (label, flag, counter, bits, released) VALUES
            ('false-row', FALSE, 0, b'0', 1901),
            ('true-row', TRUE, 1, b'10101010', 2026),
            ('two-row', 2, 2, b'11111111', 2155)",
    )
    .await
    .unwrap();

    let result = db
        .fetch_streaming("SELECT flag, counter, bits, released FROM flags ORDER BY label", 100)
        .await
        .unwrap();

    // MySQL stores BOOLEAN as TINYINT(1) and sqlx reports that type as BOOLEAN, so the
    // value is rendered as a Rust bool: every non-zero value collapses to "true", while
    // the identically-valued TINYINT UNSIGNED column keeps its number.
    assert_eq!(column(&result.rows, 0), vec![Some("false"), Some("true"), Some("true")]);
    assert_eq!(column(&result.rows, 1), vec![Some("0"), Some("1"), Some("2")]);
    assert_eq!(column(&result.rows, 2), vec![Some("0"), Some("170"), Some("255")]);
    assert_eq!(column(&result.rows, 3), vec![Some("1901"), Some("2026"), Some("2155")]);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn float_double_and_decimal_columns_keep_their_written_form() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql(
        "CREATE TABLE numerics (
            id INT PRIMARY KEY,
            single FLOAT NOT NULL,
            wide DOUBLE NOT NULL,
            amount DECIMAL(10,2) NOT NULL
        )",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO numerics (id, single, wide, amount) VALUES
            (1, 3.5, 0.1, 1234.5),
            (2, -1.25, 2.5, -0.05)",
    )
    .await
    .unwrap();

    let result = db
        .fetch_streaming("SELECT single, wide, amount FROM numerics ORDER BY id", 100)
        .await
        .unwrap();

    assert_eq!(column(&result.rows, 0), vec![Some("3.5"), Some("-1.25")]);
    assert_eq!(column(&result.rows, 1), vec![Some("0.1"), Some("2.5")]);
    // DECIMAL(10,2) is exact, and the decoded Decimal carries the column's scale, so
    // the trailing zero MySQL stored survives into the string.
    assert_eq!(column(&result.rows, 2), vec![Some("1234.50"), Some("-0.05")]);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn text_enum_set_and_json_columns_decode_as_strings() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql(
        "CREATE TABLE documents (
            id INT PRIMARY KEY,
            fixed_text CHAR(5) NOT NULL,
            short_text VARCHAR(32) NOT NULL,
            long_text TEXT NOT NULL,
            size_enum ENUM('small', 'large') NOT NULL,
            tag_set SET('a', 'b', 'c') NOT NULL,
            payload JSON NOT NULL
        )",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO documents
            (id, fixed_text, short_text, long_text, size_enum, tag_set, payload) VALUES
            (1, 'abc', 'héllo', 'a longer piece of text', 'large', 'a,c', '{\"b\": 1, \"a\": [1, 2]}')",
    )
    .await
    .unwrap();

    let result = db
        .fetch_streaming(
            "SELECT fixed_text, short_text, long_text, size_enum, tag_set, payload FROM documents",
            100,
        )
        .await
        .unwrap();

    // CHAR(5) drops the padding MySQL added on the way in.
    assert_eq!(cell(&result.rows, 0, 0), Some("abc"));
    assert_eq!(cell(&result.rows, 0, 1), Some("héllo"));
    assert_eq!(cell(&result.rows, 0, 2), Some("a longer piece of text"));
    assert_eq!(cell(&result.rows, 0, 3), Some("large"));
    // SET is the one type read through `try_get_unchecked`, because sqlx does not list
    // it as compatible with `String`; a multi-member value proves the arm decodes the
    // whole comma-separated list rather than a single member.
    assert_eq!(cell(&result.rows, 0, 4), Some("a,c"));
    // Rendered by serde_json, which sorts object keys and omits insignificant space.
    assert_eq!(cell(&result.rows, 0, 5), Some(r#"{"a":[1,2],"b":1}"#));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn temporal_columns_decode_in_chrono_display_format() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql(
        "CREATE TABLE events (
            id INT PRIMARY KEY,
            day DATE NOT NULL,
            moment TIME NOT NULL,
            local_at DATETIME NOT NULL,
            recorded_at TIMESTAMP NOT NULL
        )",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO events (id, day, moment, local_at, recorded_at) VALUES
            (1, '2026-08-24', '12:34:56', '2026-08-24 12:34:56', '2026-08-24 12:34:56')",
    )
    .await
    .unwrap();

    let result = db
        .fetch_streaming("SELECT day, moment, local_at, recorded_at FROM events", 100)
        .await
        .unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("2026-08-24"));
    assert_eq!(cell(&result.rows, 0, 1), Some("12:34:56"));
    assert_eq!(cell(&result.rows, 0, 2), Some("2026-08-24 12:34:56"));
    // TIMESTAMP is decoded as an instant, so the rendering names the zone. The value
    // matches what was written because the image's session time zone is UTC.
    assert_eq!(cell(&result.rows, 0, 3), Some("2026-08-24 12:34:56 UTC"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn binary_columns_decode_as_text_or_base64() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql(
        "CREATE TABLE payloads (
            id INT PRIMARY KEY,
            fixed_bytes BINARY(4) NOT NULL,
            var_bytes VARBINARY(16) NOT NULL,
            text_blob BLOB NOT NULL,
            raw_blob BLOB NOT NULL
        )",
    )
    .await
    .unwrap();
    db.execute_sql(
        "INSERT INTO payloads (id, fixed_bytes, var_bytes, text_blob, raw_blob) VALUES
            (1, X'DEADBEEF', 'hi', 'hello', X'FFFEFD')",
    )
    .await
    .unwrap();

    let result = db
        .fetch_streaming("SELECT fixed_bytes, var_bytes, text_blob, raw_blob FROM payloads", 100)
        .await
        .unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("base64:3q2+7w=="));
    assert_eq!(cell(&result.rows, 0, 1), Some("hi"));
    assert_eq!(cell(&result.rows, 0, 2), Some("hello"));
    assert_eq!(cell(&result.rows, 0, 3), Some("base64://79"));
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn sql_null_decodes_to_none_for_every_column_type() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql(
        "CREATE TABLE nullables (
            id INT PRIMARY KEY,
            number INT UNSIGNED,
            note VARCHAR(16),
            amount DECIMAL(10,2),
            happened_at DATETIME,
            payload BLOB
        )",
    )
    .await
    .unwrap();
    db.execute_sql("INSERT INTO nullables (id) VALUES (1)").await.unwrap();

    let result = db
        .fetch_streaming("SELECT number, note, amount, happened_at, payload FROM nullables", 100)
        .await
        .unwrap();

    assert_eq!(result.columns, vec!["number", "note", "amount", "happened_at", "payload"]);
    assert_eq!(result.rows[0], vec![None, None, None, None, None]);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn list_tables_query_returns_the_tables_of_the_current_database() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql("CREATE TABLE accounts (id INT UNSIGNED PRIMARY KEY)").await.unwrap();
    db.execute_sql("CREATE TABLE orders (id INT PRIMARY KEY)").await.unwrap();

    let tables = db.fetch_column_as_strings(DbType::Mysql.list_tables_query()).await.unwrap();

    assert_eq!(tables, vec!["accounts", "orders"]);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn describe_table_returns_column_metadata_in_ordinal_order() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql(
        "CREATE TABLE accounts (
            id INT UNSIGNED PRIMARY KEY,
            email VARCHAR(255) NOT NULL,
            nickname VARCHAR(32)
        )",
    )
    .await
    .unwrap();

    let table = ValidatedTableName::new("accounts").unwrap();
    let result = db.describe_table(&table).await.unwrap();

    // Without the explicit aliases in the query, MySQL would label the result with the
    // information_schema column's own upper-case names (`COLUMN_NAME`, ...).
    assert_eq!(
        result.columns,
        vec!["name", "data_type", "is_nullable", "column_default", "primary_key"]
    );
    assert_eq!(column(&result.rows, 0), vec![Some("id"), Some("email"), Some("nickname")]);
    // The type names stay MySQL's own — `data_type` is not normalized across engines.
    assert_eq!(column(&result.rows, 1), vec![Some("int"), Some("varchar"), Some("varchar")]);
    assert_eq!(column(&result.rows, 2), vec![Some("NO"), Some("NO"), Some("YES")]);
    assert_eq!(cell(&result.rows, 2, 3), None);
    assert_eq!(column(&result.rows, 4), vec![Some("YES"), Some("NO"), Some("NO")]);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn fetch_streaming_truncates_at_max_rows() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    db.execute_sql("CREATE TABLE numbers (id INT PRIMARY KEY)").await.unwrap();
    db.execute_sql("INSERT INTO numbers (id) VALUES (1), (2), (3)").await.unwrap();

    let truncated = db.fetch_streaming("SELECT id FROM numbers ORDER BY id", 2).await.unwrap();
    assert!(truncated.truncated);
    assert_eq!(truncated.row_count, 2);
    assert_eq!(column(&truncated.rows, 0), vec![Some("1"), Some("2")]);

    // A result that ends exactly at the limit was not cut short.
    let complete = db.fetch_streaming("SELECT id FROM numbers ORDER BY id", 3).await.unwrap();
    assert!(!complete.truncated);
    assert_eq!(complete.row_count, 3);
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn caching_sha2_password_user_connects_without_tls() {
    let fixture = start_mysql().await;
    let db = &fixture.db;

    // The fixture's own root account has an empty password, which skips the exchange
    // under test; a freshly created account always misses the server-side auth cache,
    // so connecting as it forces `caching_sha2_password` full authentication.
    db.execute_sql(
        "CREATE USER 'pw_user'@'%' IDENTIFIED WITH caching_sha2_password BY 'pw_secret'",
    )
    .await
    .unwrap();
    db.execute_sql("GRANT ALL PRIVILEGES ON test.* TO 'pw_user'@'%'").await.unwrap();

    // `ssl-mode=disabled` pins the non-TLS route: under sqlx's default `PREFERRED` the
    // connection would upgrade to TLS and authenticate over the encrypted channel
    // instead, leaving the RSA password-encryption path untested.
    let url = format!(
        "mysql://pw_user:pw_secret@{}:{}/test?ssl-mode=disabled",
        fixture.host, fixture.port
    );

    let pw_db = DatabaseConnection::connect(&url).await.unwrap();
    let result = pw_db.fetch_streaming("SELECT 1", 1).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("1"));

    // `Ssl_cipher` is empty exactly when the session is unencrypted, which is this test's
    // premise rather than its subject: sqlx silently ignores query keys it does not know,
    // so a renamed or mistyped `ssl-mode` would fall back to `PREFERRED`, authenticate over
    // TLS, and leave the RSA path `mysql-rsa` guards untouched while still passing above.
    let session = session_ssl_cipher(&pw_db).await;
    assert_eq!(session.as_deref(), Some(""));
}

/// Returns the session's `Ssl_cipher` status value: the negotiated cipher suite over TLS,
/// and the empty string on a plaintext connection.
#[expect(clippy::unwrap_used, reason = "test helper: a failed query should abort the test")]
async fn session_ssl_cipher(db: &DatabaseConnection) -> Option<String> {
    let status = db.fetch_streaming("SHOW SESSION STATUS LIKE 'Ssl_cipher'", 1).await.unwrap();
    // `SHOW STATUS` returns one `Variable_name`, `Value` pair per matched variable.
    status.rows[0][1].clone()
}

#[tokio::test]
#[ignore = "requires a container runtime; run with `just test-db`"]
async fn ssl_mode_required_connects_over_tls() {
    let fixture = start_mysql().await;

    // `required` demands an encrypted channel but verifies no certificate, which is what
    // makes it usable here: the image generates a self-signed certificate at startup,
    // and only `verify_ca` / `verify_identity` would reject it.
    let url = format!("mysql://root@{}:{}/test?ssl-mode=required", fixture.host, fixture.port);

    let tls_db = DatabaseConnection::connect(&url).await.unwrap();
    let result = tls_db.fetch_streaming("SELECT 1", 1).await.unwrap();

    assert_eq!(cell(&result.rows, 0, 0), Some("1"));

    // The mirror of the non-TLS test's premise check: a named cipher suite is what
    // distinguishes an encrypted session from a silent fallback to plaintext.
    let cipher = session_ssl_cipher(&tls_db).await;
    assert!(cipher.is_some_and(|name| !name.is_empty()), "expected a negotiated TLS cipher");
}
