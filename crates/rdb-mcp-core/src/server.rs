//! MCP server implementation exposing relational database operations as tools and resources.
//!
//! Provides three MCP tools (`execute_sql`, `list_tables`, `describe_table`) and
//! an MCP resource interface for browsing table data.

use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler, handler::server::tool::ToolRouter,
    handler::server::wrapper::Parameters, model::*, service::RequestContext, tool, tool_handler,
    tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::db::{self, DatabaseConnection, ValidatedTableName};
use crate::error::sqlx_to_mcp_error;

/// Maximum number of rows returned in a single `execute_sql` response.
/// Queries returning this many rows or more are truncated with a notification to the caller.
const MAX_RESULT_ROWS: usize = 10_000;

/// Maximum number of rows returned in a single resource read response.
/// Kept deliberately smaller than [`MAX_RESULT_ROWS`] because resource reads
/// are intended for quick data previews, not full exports.
const MAX_RESOURCE_ROWS: usize = 100;

/// Implementation name advertised to clients during initialization.
const SERVER_NAME: &str = "rdb-mcp";

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

impl McpServer {
    async fn fetch_table_names(&self) -> Result<Vec<String>, McpError> {
        let sql = self.db.db_type().list_tables_query();
        self.db.fetch_column_as_strings(sql).await.map_err(sqlx_to_mcp_error)
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
        description = "Execute an arbitrary SQL query. Read queries (SELECT, SHOW, EXPLAIN, PRAGMA, DESCRIBE, and WITH/CTE selects) return results as CSV. Write and DDL queries return the number of affected rows.",
        annotations(destructive_hint = true)
    )]
    async fn execute_sql(
        &self,
        params: Parameters<ExecuteSqlParams>,
    ) -> Result<CallToolResult, McpError> {
        let raw_query = params.0.query;
        let query = raw_query.trim();

        if query.is_empty() {
            return Err(McpError::invalid_params("query must not be empty".to_string(), None));
        }

        if let Err(reason) = db::validate_query_syntax(query) {
            return Err(McpError::invalid_params(format!("invalid SQL: {reason}"), None));
        }

        tracing::debug!(query = %query, "executing SQL");

        if db::is_read_query(query) {
            let (csv, truncated) =
                self.db.fetch_streaming_as_csv(query, MAX_RESULT_ROWS).await.map_err(|e| {
                    tracing::error!(query = %query, error = %e, "read query failed");
                    sqlx_to_mcp_error(e)
                })?;

            if csv.is_empty() {
                return Ok(CallToolResult::success(vec![ContentBlock::text(
                    "Query returned 0 rows.",
                )]));
            }

            let mut result_csv = csv;
            if truncated {
                result_csv.push_str(&format!(
                    "\n\n(Note: Results truncated. Showing first {MAX_RESULT_ROWS} rows.)"
                ));
            }
            Ok(CallToolResult::success(vec![ContentBlock::text(result_csv)]))
        } else {
            let rows_affected = self.db.execute_sql(query).await.map_err(|e| {
                tracing::error!(query = %query, error = %e, "write query failed");
                sqlx_to_mcp_error(e)
            })?;
            let text = format!("Rows affected: {rows_affected}");
            Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
        }
    }

    #[tool(
        name = "list_tables",
        description = "List all tables in the database.",
        annotations(read_only_hint = true)
    )]
    async fn list_tables(&self) -> Result<CallToolResult, McpError> {
        let tables = self.fetch_table_names().await?;
        if tables.is_empty() {
            return Ok(CallToolResult::success(vec![ContentBlock::text(
                "No tables found in the database.",
            )]));
        }
        let text = tables.join("\n");
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]))
    }

    #[tool(
        name = "describe_table",
        description = "Describe the schema of a specific table, returning column names, data types, nullability, and defaults. Constraint details vary by database engine.",
        annotations(read_only_hint = true)
    )]
    async fn describe_table(
        &self,
        params: Parameters<DescribeTableParams>,
    ) -> Result<CallToolResult, McpError> {
        let table_name =
            ValidatedTableName::new(&params.0.table_name).map_err(|e| e.into_mcp_error())?;

        let csv = self.db.describe_table_as_csv(&table_name).await.map_err(|e| {
            tracing::error!(table = %table_name, error = %e, "describe table failed");
            e.into_mcp_error()
        })?;

        if csv.is_empty() {
            return Err(McpError::resource_not_found(
                format!("table '{}' not found or has no columns", table_name),
                None,
            ));
        }

        Ok(CallToolResult::success(vec![ContentBlock::text(csv)]))
    }
}

// `router` is set explicitly because rmcp 3.x defaults to `Self::tool_router()`, which
// would rebuild and re-register the whole router on every tool call and listing.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().enable_resources().build())
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
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
                        .with_mime_type("text/csv"),
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
        let sql = format!("SELECT * FROM {quoted} LIMIT {MAX_RESOURCE_ROWS}");
        let csv = self.db.fetch_all_as_csv(&sql).await.map_err(|e| {
            tracing::error!(table = %table_name, error = %e, "read resource failed");
            sqlx_to_mcp_error(e)
        })?;
        let content = if csv.is_empty() {
            format!("Table '{}' exists but contains no rows.", table_name)
        } else {
            csv
        };
        Ok(ReadResourceResult::new(vec![ResourceContents::text(content, uri.clone())]).into())
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
}
