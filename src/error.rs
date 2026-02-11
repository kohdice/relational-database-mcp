use rmcp::ErrorData as McpError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error("unsupported database URL scheme: {0}")]
    UnsupportedScheme(String),

    #[error("invalid table name: {0}")]
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
            Self::Database(e) => {
                tracing::error!(error = %e, "database error");
                sqlx_to_mcp_error(e)
            }
        }
    }
}

pub(crate) fn sqlx_to_mcp_error(e: sqlx::Error) -> McpError {
    let message = match &e {
        sqlx::Error::Database(db_err) => format!("database query error: {db_err}"),
        sqlx::Error::PoolTimedOut => "database connection pool timed out".to_string(),
        sqlx::Error::Io(io_err) => format!("database I/O error: {io_err}"),
        sqlx::Error::Tls(tls_err) => format!("database TLS error: {tls_err}"),
        sqlx::Error::Configuration(cfg_err) => format!("database configuration error: {cfg_err}"),
        other => {
            tracing::warn!(error = %other, "unrecognized sqlx error variant");
            format!("database error: {other}")
        }
    };
    McpError::internal_error(message, None)
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
        let err = AppError::InvalidTableName("bad;name".to_string());
        let mcp = err.into_mcp_error();
        assert_eq!(mcp.code, ErrorCode::INVALID_PARAMS);
        assert!(mcp.message.contains("bad;name"));
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
