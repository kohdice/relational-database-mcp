//! Error types and conversion to MCP protocol errors.
//!
//! Application errors ([`AppError`]) are mapped to MCP error codes so that clients
//! receive semantically correct responses: user input errors become `INVALID_PARAMS`,
//! while infrastructure failures become `INTERNAL_ERROR`.

use rmcp::ErrorData as McpError;
use thiserror::Error;

/// Application-level error enum covering all failure modes.
///
/// Each variant maps to a specific MCP error code via [`into_mcp_error`](Self::into_mcp_error):
/// - `UnsupportedScheme` / `InvalidTableName` → `INVALID_PARAMS`
/// - `ConnectionFailed` → `INTERNAL_ERROR`
/// - `Database` → delegated to [`sqlx_to_mcp_error`]
#[derive(Debug, Error)]
pub enum AppError {
    /// The connection URL uses a scheme that maps to no supported engine.
    /// Holds the offending scheme only, never the full URL.
    #[error("unsupported database URL scheme: {0}")]
    UnsupportedScheme(String),

    /// A table name failed [`ValidatedTableName`](crate::db::ValidatedTableName) validation.
    #[error("{0}")]
    InvalidTableName(String),

    /// Establishing the connection pool failed. The message is sanitized to
    /// avoid leaking credentials embedded in the connection URL.
    #[error("connection failed: {0}")]
    ConnectionFailed(String),

    /// A query or statement failed at the driver level.
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

impl AppError {
    /// Converts this error into an MCP protocol error with the appropriate error code.
    pub fn into_mcp_error(self) -> McpError {
        let msg = self.to_string();
        match self {
            Self::UnsupportedScheme(_) | Self::InvalidTableName(_) => {
                McpError::invalid_params(msg, None)
            }
            Self::ConnectionFailed(_) => McpError::internal_error(msg, None),
            Self::Database(e) => sqlx_to_mcp_error(e),
        }
    }
}

/// Converts a [`sqlx::Error`] into an MCP protocol error.
///
/// User-triggered errors (SQL syntax errors, constraint violations, missing rows/columns,
/// column index out of bounds, decode failures) are mapped to `INVALID_PARAMS`.
/// Infrastructure errors (pool timeout, I/O, TLS, configuration, protocol, worker crash)
/// are mapped to `INTERNAL_ERROR`.
pub(crate) fn sqlx_to_mcp_error(e: sqlx::Error) -> McpError {
    match &e {
        // User-triggered errors → INVALID_PARAMS
        sqlx::Error::Database(db_err) => {
            McpError::invalid_params(format!("database query error: {db_err}"), None)
        }
        sqlx::Error::RowNotFound => {
            McpError::invalid_params("no matching row found".to_string(), None)
        }
        sqlx::Error::ColumnNotFound(col) => {
            McpError::invalid_params(format!("column not found: {col}"), None)
        }
        sqlx::Error::ColumnDecode { .. } => {
            McpError::invalid_params(format!("failed to decode column value: {e}"), None)
        }
        sqlx::Error::ColumnIndexOutOfBounds { index, len } => McpError::invalid_params(
            format!("column index {index} out of bounds (columns: {len})"),
            None,
        ),
        // Infrastructure errors → INTERNAL_ERROR
        sqlx::Error::PoolTimedOut => {
            tracing::error!("database connection pool timed out");
            McpError::internal_error("database connection pool timed out".to_string(), None)
        }
        sqlx::Error::Io(io_err) => {
            tracing::error!(error = %io_err, "database I/O error");
            McpError::internal_error(format!("database I/O error: {io_err}"), None)
        }
        sqlx::Error::Tls(tls_err) => {
            tracing::error!(error = %tls_err, "database TLS error");
            McpError::internal_error(format!("database TLS error: {tls_err}"), None)
        }
        sqlx::Error::Configuration(cfg_err) => {
            tracing::error!(error = %cfg_err, "database configuration error");
            McpError::internal_error(format!("database configuration error: {cfg_err}"), None)
        }
        sqlx::Error::Protocol(msg) => {
            tracing::error!(error = %msg, "database protocol error");
            McpError::internal_error(format!("database protocol error: {msg}"), None)
        }
        sqlx::Error::WorkerCrashed => {
            tracing::error!("database connection pool worker crashed");
            McpError::internal_error("database worker crashed".to_string(), None)
        }
        other => {
            tracing::error!(error = %other, "unrecognized sqlx error variant");
            McpError::internal_error(format!("database error: {other}"), None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rmcp::model::ErrorCode;

    #[test]
    fn test_into_mcp_error_unsupported_scheme() {
        let err = AppError::UnsupportedScheme("oracle".to_string());
        let mcp = err.into_mcp_error();
        assert_eq!(mcp.code, ErrorCode::INVALID_PARAMS);
        assert!(mcp.message.contains("oracle"));
    }

    #[test]
    fn test_into_mcp_error_invalid_table_name() {
        let err = AppError::InvalidTableName(
            "table name 'bad;name' contains invalid characters".to_string(),
        );
        let mcp = err.into_mcp_error();
        assert_eq!(mcp.code, ErrorCode::INVALID_PARAMS);
        assert!(mcp.message.contains("bad;name"));
        assert!(mcp.message.contains("invalid characters"));
    }

    #[test]
    fn test_sqlx_to_mcp_error_protocol() {
        let err = sqlx::Error::Protocol("test protocol error".to_string());
        let mcp = sqlx_to_mcp_error(err);
        assert_eq!(mcp.code, ErrorCode::INTERNAL_ERROR);
        assert!(mcp.message.contains("protocol error"));
    }

    #[test]
    fn test_sqlx_to_mcp_error_row_not_found() {
        let err = sqlx::Error::RowNotFound;
        let mcp = sqlx_to_mcp_error(err);
        assert_eq!(mcp.code, ErrorCode::INVALID_PARAMS);
    }

    #[test]
    fn test_sqlx_to_mcp_error_column_not_found() {
        let err = sqlx::Error::ColumnNotFound("nonexistent".to_string());
        let mcp = sqlx_to_mcp_error(err);
        assert_eq!(mcp.code, ErrorCode::INVALID_PARAMS);
        assert!(mcp.message.contains("nonexistent"));
    }

    #[test]
    fn test_sqlx_to_mcp_error_pool_timed_out() {
        let err = sqlx::Error::PoolTimedOut;
        let mcp = sqlx_to_mcp_error(err);
        assert_eq!(mcp.code, ErrorCode::INTERNAL_ERROR);
        assert!(mcp.message.contains("pool timed out"));
    }

    #[test]
    fn test_sqlx_to_mcp_error_io() {
        let io_err = std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused");
        let err = sqlx::Error::Io(io_err);
        let mcp = sqlx_to_mcp_error(err);
        assert_eq!(mcp.code, ErrorCode::INTERNAL_ERROR);
        assert!(mcp.message.contains("I/O error"));
    }

    #[test]
    fn test_sqlx_to_mcp_error_configuration() {
        let err = sqlx::Error::Configuration("bad config".into());
        let mcp = sqlx_to_mcp_error(err);
        assert_eq!(mcp.code, ErrorCode::INTERNAL_ERROR);
        assert!(mcp.message.contains("configuration error"));
    }
}
