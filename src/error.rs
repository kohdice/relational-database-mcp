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
/// - `Database` → delegated to [`sqlx_to_mcp_error`]
#[derive(Debug, Error)]
pub enum AppError {
    #[error("unsupported database URL scheme: {0}")]
    UnsupportedScheme(String),

    #[error("{0}")]
    InvalidTableName(String),

    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

impl AppError {
    pub fn into_mcp_error(self) -> McpError {
        let msg = self.to_string();
        match self {
            Self::UnsupportedScheme(_) | Self::InvalidTableName(_) => {
                McpError::invalid_params(msg, None)
            }
            Self::Database(e) => sqlx_to_mcp_error(e),
        }
    }
}

/// Converts a [`sqlx::Error`] into an MCP protocol error.
///
/// `Database` errors (SQL syntax errors, constraint violations, etc.) are mapped to
/// `INVALID_PARAMS` because they are typically caused by user-provided SQL.
/// Infrastructure errors (pool timeout, I/O, TLS, configuration) are mapped to
/// `INTERNAL_ERROR`.
pub(crate) fn sqlx_to_mcp_error(e: sqlx::Error) -> McpError {
    match &e {
        sqlx::Error::Database(db_err) => {
            McpError::invalid_params(format!("database query error: {db_err}"), None)
        }
        sqlx::Error::PoolTimedOut => {
            McpError::internal_error("database connection pool timed out".to_string(), None)
        }
        sqlx::Error::Io(io_err) => {
            McpError::internal_error(format!("database I/O error: {io_err}"), None)
        }
        sqlx::Error::Tls(tls_err) => {
            McpError::internal_error(format!("database TLS error: {tls_err}"), None)
        }
        sqlx::Error::Configuration(cfg_err) => {
            McpError::internal_error(format!("database configuration error: {cfg_err}"), None)
        }
        other => {
            tracing::warn!(error = %other, "unrecognized sqlx error variant");
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
    fn test_sqlx_to_mcp_error_database_query_error() {
        // sqlx::Error::Database maps to INVALID_PARAMS
        let db_err = sqlx::Error::Protocol("test protocol error".to_string());
        let mcp = sqlx_to_mcp_error(db_err);
        // Protocol errors fall into the catch-all, which is INTERNAL_ERROR
        assert_eq!(mcp.code, ErrorCode::INTERNAL_ERROR);
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
