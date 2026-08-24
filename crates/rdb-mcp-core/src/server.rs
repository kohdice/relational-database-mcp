//! MCP server implementation exposing relational database operations as tools and resources.
//!
//! Provides three MCP tools (`execute_sql`, `list_tables`, `describe_table`) and
//! an MCP resource interface for browsing table data.

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    handler::server::tool::ToolRouter,
    handler::server::wrapper::{Json, Parameters},
    model::*,
    service::RequestContext,
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::{DatabaseConnection, ExecutionResult, QueryResult, ValidatedTableName};
use crate::error::sqlx_to_mcp_error;
use crate::sql;

/// Maximum number of rows returned in a single `execute_sql` response.
/// Queries returning this many rows or more are truncated with a notification to the caller.
const MAX_RESULT_ROWS: usize = 10_000;

/// Maximum number of rows returned in a single resource read response.
/// Kept deliberately smaller than [`MAX_RESULT_ROWS`] because resource reads
/// are intended for quick data previews, not full exports.
const MAX_RESOURCE_ROWS: usize = 100;

/// Implementation name advertised to clients during initialization.
const SERVER_NAME: &str = "rdb-mcp";

/// MIME type of table-data resources, which carry a JSON-serialized [`QueryResult`].
const RESOURCE_MIME_TYPE: &str = "application/json";

/// Core handler implementing [`ServerHandler`] from `rmcp`, dispatching MCP tool calls
/// to the underlying [`DatabaseConnection`].
#[derive(Clone)]
pub struct McpServer {
    db: DatabaseConnection,
    tool_router: ToolRouter<Self>,
}

/// Input parameters of the `execute_sql` tool.
#[derive(Deserialize, JsonSchema)]
pub struct ExecuteSqlParams {
    /// SQL query to execute.
    #[schemars(description = "SQL query to execute")]
    pub query: String,
}

/// Input parameters of the `describe_table` tool.
#[derive(Deserialize, JsonSchema)]
pub struct DescribeTableParams {
    /// Name of the table to describe.
    #[schemars(description = "Name of the table to describe")]
    pub table_name: String,
}

/// Output of the `execute_sql` tool.
///
/// Read queries and write/DDL statements produce different shapes, discriminated
/// by the `kind` field so that a single output schema covers both.
#[derive(Debug, Serialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecuteSqlResult {
    /// The statement was a read query and returned a result set.
    Query(QueryResult),
    /// The statement was a write or DDL statement and reported affected rows.
    Execution(ExecutionResult),
}

/// Output of the `list_tables` tool.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ListTablesResult {
    /// Names of the tables in the database's default user-facing schema.
    pub tables: Vec<String>,
}

impl McpServer {
    async fn fetch_table_names(&self) -> Result<Vec<String>, McpError> {
        let sql = self.db.db_type().list_tables_query();
        self.db.fetch_column_as_strings(sql).await.map_err(sqlx_to_mcp_error)
    }

    /// Resolves a table-resource URI and fetches a bounded preview of the table.
    ///
    /// Split out of [`Self::read_resource`] so that URI parsing, engine matching,
    /// name validation, and truncation reporting can be tested without building a
    /// [`RequestContext`].
    async fn fetch_table_preview(&self, uri: &str) -> Result<QueryResult, McpError> {
        let (scheme, raw_name) =
            parse_table_from_uri(uri).map_err(|e| McpError::resource_not_found(e, None))?;

        let expected_scheme = self.db.db_type().resource_uri_scheme();
        if scheme != expected_scheme {
            return Err(McpError::resource_not_found(
                format!("URI scheme '{scheme}' does not match expected '{expected_scheme}'"),
                None,
            ));
        }

        let table_name = ValidatedTableName::new(&raw_name).map_err(|e| e.into_mcp_error())?;
        let quoted = self.db.db_type().quote_identifier(&table_name);

        // No LIMIT clause: the row cap belongs to `fetch_streaming`, which has to
        // see one row past it to tell a cut-off preview from one that simply ends
        // at the cap. Streaming stops there, so this never scans the whole table.
        let sql = format!("SELECT * FROM {quoted}");
        self.db.fetch_streaming(&sql, MAX_RESOURCE_ROWS).await.map_err(|e| {
            tracing::error!(table = %table_name, error = %e, "read resource failed");
            sqlx_to_mcp_error(e)
        })
    }
}

#[tool_router]
impl McpServer {
    /// Creates a new MCP server backed by the given database connection.
    pub fn new(db: DatabaseConnection) -> Self {
        Self { db, tool_router: Self::tool_router() }
    }

    #[tool(
        name = "execute_sql",
        description = "Execute an arbitrary SQL query. Read queries (SELECT, SHOW, EXPLAIN, PRAGMA, DESCRIBE, and WITH/CTE selects) return JSON with `kind: \"query\"`, the column names, and the rows, where a null value means SQL NULL. Write and DDL statements return JSON with `kind: \"execution\"` and the number of affected rows. Binary values that are not valid UTF-8 are returned base64-encoded behind a \"base64:\" prefix. A column whose SQL type this server cannot render as text fails the call; cast it in the query instead.",
        annotations(destructive_hint = true)
    )]
    async fn execute_sql(
        &self,
        params: Parameters<ExecuteSqlParams>,
    ) -> Result<Json<ExecuteSqlResult>, McpError> {
        let raw_query = params.0.query;
        let query = raw_query.trim();

        if query.is_empty() {
            return Err(McpError::invalid_params("query must not be empty".to_string(), None));
        }

        if let Err(reason) = sql::validate_query_syntax(query) {
            return Err(McpError::invalid_params(format!("invalid SQL: {reason}"), None));
        }

        tracing::debug!(query = %query, "executing SQL");

        if sql::is_read_query(query) {
            let result = self.db.fetch_streaming(query, MAX_RESULT_ROWS).await.map_err(|e| {
                tracing::error!(query = %query, error = %e, "read query failed");
                sqlx_to_mcp_error(e)
            })?;
            Ok(Json(ExecuteSqlResult::Query(result)))
        } else {
            let result = self.db.execute_sql(query).await.map_err(|e| {
                tracing::error!(query = %query, error = %e, "write query failed");
                sqlx_to_mcp_error(e)
            })?;
            Ok(Json(ExecuteSqlResult::Execution(result)))
        }
    }

    #[tool(
        name = "list_tables",
        description = "List tables in the database's default user-facing schema (MySQL: the current database; PostgreSQL: the `public` schema; SQLite: non-system tables). Returns JSON with the table names.",
        annotations(read_only_hint = true)
    )]
    async fn list_tables(&self) -> Result<Json<ListTablesResult>, McpError> {
        let tables = self.fetch_table_names().await?;
        Ok(Json(ListTablesResult { tables }))
    }

    #[tool(
        name = "describe_table",
        description = "Describe the schema of a specific table. Returns JSON with one row per table column and always the same five result columns, in this order: `name` (the column name), `data_type`, `is_nullable` (`YES` or `NO`), `column_default` (null when the column has no default), and `primary_key` (`YES` or `NO`). The shape is identical on every engine, but `data_type` is the engine's own type name and is not normalized (MySQL `int`, PostgreSQL `integer`, SQLite `INTEGER`).",
        annotations(read_only_hint = true)
    )]
    async fn describe_table(
        &self,
        params: Parameters<DescribeTableParams>,
    ) -> Result<Json<QueryResult>, McpError> {
        let table_name =
            ValidatedTableName::new(&params.0.table_name).map_err(|e| e.into_mcp_error())?;

        let result = self.db.describe_table(&table_name).await.map_err(|e| {
            tracing::error!(table = %table_name, error = %e, "describe table failed");
            e.into_mcp_error()
        })?;

        if result.is_empty() {
            return Err(McpError::resource_not_found(
                format!("table '{table_name}' not found or has no columns"),
                None,
            ));
        }

        Ok(Json(result))
    }
}

// `router` is set explicitly because rmcp 3.x defaults to `Self::tool_router()`, which
// would rebuild and re-register the whole router on every tool call and listing.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            // `ProtocolVersion::LATEST` is still 2025-11-25, so 2026-07-28 must be named
            // explicitly. rmcp negotiates down for clients that ask for an older version.
            .with_protocol_version(ProtocolVersion::V_2026_07_28)
            // `Implementation::from_build_env()` resolves `env!` at rmcp's own compile time,
            // so it would advertise the SDK's crate name and version instead of this server's.
            .with_server_info(Implementation::new(SERVER_NAME, env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "MCP server to access Relational Database (MySQL, PostgreSQL, SQLite)",
            )
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, McpError> {
        let tables = self.fetch_table_names().await?;
        let scheme = self.db.db_type().resource_uri_scheme();
        let mut skipped_tables: Vec<String> = Vec::new();
        let resources: Vec<Resource> = tables
            .into_iter()
            .filter_map(|table_name| {
                if ValidatedTableName::new(&table_name).is_err() {
                    tracing::warn!(table_name = %table_name, "skipping table with invalid name");
                    skipped_tables.push(table_name);
                    return None;
                }
                let uri = format!("{scheme}://{table_name}/data");
                Some(
                    Resource::new(uri, format!("Table: {table_name}"))
                        .with_description(format!("Data in table {table_name}"))
                        .with_mime_type(RESOURCE_MIME_TYPE),
                )
            })
            .collect();

        let meta = if skipped_tables.is_empty() {
            None
        } else {
            let mut map = serde_json::Map::new();
            map.insert(
                "skippedTables".to_string(),
                serde_json::Value::Array(
                    skipped_tables.iter().map(|t| serde_json::Value::String(t.clone())).collect(),
                ),
            );
            map.insert(
                "warning".to_string(),
                serde_json::Value::String(format!(
                    "{} table(s) were excluded because their names contain characters not supported by this server (only ASCII alphanumeric and underscores are allowed): {}",
                    skipped_tables.len(),
                    skipped_tables.join(", ")
                )),
            );
            Some(MetaObject(map))
        };

        let mut result = ListResourcesResult::with_all_items(resources);
        result.meta = meta;
        Ok(result)
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, McpError> {
        let uri = &request.uri;

        let result = self.fetch_table_preview(uri).await?;
        let content = serde_json::to_string(&result).map_err(|e| {
            tracing::error!(uri = %uri, error = %e, "failed to serialize resource");
            McpError::internal_error(format!("failed to serialize resource: {e}"), None)
        })?;
        // `ResourceContents::text` defaults to text/plain; override it so the read
        // agrees with the MIME type advertised by `list_resources`.
        let contents =
            ResourceContents::text(content, uri.clone()).with_mime_type(RESOURCE_MIME_TYPE);
        Ok(ReadResourceResult::new(vec![contents]).into())
    }
}

/// Extracts the scheme and table name from a resource URI of the form
/// `{scheme}://{table_name}/data`. Returns `(scheme, table_name)` on success,
/// or an error describing the specific parsing failure.
fn parse_table_from_uri(uri: &str) -> Result<(String, String), String> {
    let (scheme, rest) = uri
        .split_once("://")
        .ok_or_else(|| format!("URI missing '://' scheme separator: {uri}"))?;
    let table_name = rest
        .strip_suffix("/data")
        .ok_or_else(|| format!("URI path must end with '/data': {uri}"))?;
    if table_name.is_empty() {
        return Err(format!("URI has empty table name: {uri}"));
    }
    Ok((scheme.to_string(), table_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_table_from_uri() {
        assert_eq!(
            parse_table_from_uri("mysql://users/data"),
            Ok(("mysql".to_string(), "users".to_string()))
        );
        assert_eq!(
            parse_table_from_uri("postgres://orders/data"),
            Ok(("postgres".to_string(), "orders".to_string()))
        );
        assert_eq!(
            parse_table_from_uri("sqlite://items/data"),
            Ok(("sqlite".to_string(), "items".to_string()))
        );
        assert!(parse_table_from_uri("mysql:///data").is_err());
        assert!(parse_table_from_uri("mysql://users/other").is_err());
        assert!(parse_table_from_uri("invalid").is_err());
    }

    /// Builds a server over an in-memory SQLite database holding a single `users` table.
    ///
    /// max_connections(1): SQLite :memory: creates a separate DB per connection,
    /// so a single connection keeps every operation on the same database.
    async fn setup_server() -> McpServer {
        let pool = sqlx::pool::PoolOptions::<sqlx::Sqlite>::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        let db = DatabaseConnection::from_sqlite_pool(pool);
        db.execute_sql("CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT, email TEXT)")
            .await
            .unwrap();
        db.execute_sql("INSERT INTO users VALUES (1, 'Alice', NULL)").await.unwrap();
        McpServer::new(db)
    }

    #[tokio::test]
    async fn test_get_info_advertises_latest_protocol_and_own_identity() {
        let server = setup_server().await;
        let info = server.get_info();

        assert_eq!(info.protocol_version, ProtocolVersion::V_2026_07_28);
        assert_eq!(info.server_info.name, SERVER_NAME);
        assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
        assert!(info.capabilities.tools.is_some(), "tools capability must be advertised");
        assert!(info.capabilities.resources.is_some(), "resources capability must be advertised");
        assert!(info.instructions.is_some());
    }

    #[test]
    fn test_tool_router_registers_every_tool_with_an_output_schema() {
        let tools = McpServer::tool_router().list_all();

        let mut names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        names.sort_unstable();
        assert_eq!(names, ["describe_table", "execute_sql", "list_tables"]);

        for tool in &tools {
            assert!(
                tool.output_schema.is_some(),
                "tool '{}' must declare an output schema so it can return structuredContent",
                tool.name
            );
        }
    }

    #[tokio::test]
    async fn test_execute_sql_read_returns_query_result() {
        let server = setup_server().await;

        let Json(result) = server
            .execute_sql(Parameters(ExecuteSqlParams {
                query: "SELECT id, name, email FROM users".to_string(),
            }))
            .await
            .unwrap();

        let ExecuteSqlResult::Query(query) = result else {
            panic!("a SELECT must be classified as a read query");
        };
        assert_eq!(query.columns, vec!["id", "name", "email"]);
        assert_eq!(query.row_count, 1);
        assert!(!query.truncated);
        assert_eq!(query.rows[0][1].as_deref(), Some("Alice"));
        assert_eq!(query.rows[0][2], None, "SQL NULL must decode to None");
    }

    #[tokio::test]
    async fn test_execute_sql_write_returns_execution_result() {
        let server = setup_server().await;

        let Json(result) = server
            .execute_sql(Parameters(ExecuteSqlParams {
                query: "INSERT INTO users VALUES (2, 'Bob', 'bob@example.com')".to_string(),
            }))
            .await
            .unwrap();

        let ExecuteSqlResult::Execution(execution) = result else {
            panic!("an INSERT must be classified as a write statement");
        };
        assert_eq!(execution.rows_affected, 1);
    }

    #[tokio::test]
    async fn test_execute_sql_rejects_empty_query() {
        let server = setup_server().await;

        let Err(error) =
            server.execute_sql(Parameters(ExecuteSqlParams { query: "   ".to_string() })).await
        else {
            panic!("a blank query must be rejected");
        };

        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn test_list_tables_returns_table_names() {
        let server = setup_server().await;

        let Json(result) = server.list_tables().await.unwrap();

        assert_eq!(result.tables, vec!["users"]);
    }

    #[tokio::test]
    async fn test_describe_table_returns_column_metadata() {
        let server = setup_server().await;

        let Json(result) = server
            .describe_table(Parameters(DescribeTableParams { table_name: "users".to_string() }))
            .await
            .unwrap();

        assert!(!result.is_empty());
        assert!(result.columns.iter().any(|c| c == "name"));
    }

    #[tokio::test]
    async fn test_describe_table_rejects_invalid_name() {
        let server = setup_server().await;

        let Err(error) = server
            .describe_table(Parameters(DescribeTableParams {
                table_name: "users; DROP TABLE users".to_string(),
            }))
            .await
        else {
            panic!("a table name with invalid characters must be rejected");
        };

        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn test_fetch_table_preview_returns_rows_for_a_small_table() {
        let server = setup_server().await;

        let result = server.fetch_table_preview("sqlite://users/data").await.unwrap();

        assert_eq!(result.row_count, 1);
        assert!(!result.truncated, "a table below the cap is not truncated");
        assert_eq!(result.columns, vec!["id", "name", "email"]);
    }

    #[tokio::test]
    async fn test_fetch_table_preview_reports_truncation() {
        let server = setup_server().await;
        // One row past MAX_RESOURCE_ROWS, so the preview really is cut short.
        for id in 2..=(MAX_RESOURCE_ROWS as i64 + 1) {
            server
                .db
                .execute_sql(&format!("INSERT INTO users VALUES ({id}, 'user', NULL)"))
                .await
                .unwrap();
        }

        let result = server.fetch_table_preview("sqlite://users/data").await.unwrap();

        assert_eq!(result.row_count, MAX_RESOURCE_ROWS);
        assert!(result.truncated, "a table above the cap must report truncation");
    }

    #[tokio::test]
    async fn test_fetch_table_preview_rejects_wrong_scheme() {
        let server = setup_server().await;

        let Err(error) = server.fetch_table_preview("mysql://users/data").await else {
            panic!("a URI for another engine must be rejected");
        };

        assert_eq!(error.code, ErrorCode::RESOURCE_NOT_FOUND);
    }

    #[tokio::test]
    async fn test_fetch_table_preview_rejects_invalid_table_name() {
        let server = setup_server().await;

        let Err(error) = server.fetch_table_preview("sqlite://users; DROP TABLE users/data").await
        else {
            panic!("a table name with invalid characters must be rejected");
        };

        assert_eq!(error.code, ErrorCode::INVALID_PARAMS);
    }

    #[tokio::test]
    async fn test_fetch_table_preview_rejects_malformed_uri() {
        let server = setup_server().await;

        let Err(error) = server.fetch_table_preview("sqlite://users/other").await else {
            panic!("a URI that does not end in /data must be rejected");
        };

        assert_eq!(error.code, ErrorCode::RESOURCE_NOT_FOUND);
    }

    #[tokio::test]
    async fn test_describe_table_reports_unknown_table_as_not_found() {
        let server = setup_server().await;

        let Err(error) = server
            .describe_table(Parameters(DescribeTableParams {
                table_name: "nonexistent".to_string(),
            }))
            .await
        else {
            panic!("describing an unknown table must fail");
        };

        assert_eq!(error.code, ErrorCode::RESOURCE_NOT_FOUND);
    }
}
