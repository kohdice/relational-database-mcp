use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};

use relational_database_mcp::cli::Args;
use relational_database_mcp::db::DatabaseConnection;
use relational_database_mcp::server::McpServer;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to initialize tracing subscriber: {e}"))?;

    let args = Args::parse();

    sqlx::any::install_default_drivers();

    let db = DatabaseConnection::connect(&args.database_url)
        .await
        .context("failed to connect to database")?;
    tracing::info!(db_type = %db.db_type(), "connected to database");

    let server = McpServer::new(db);
    let service = server.serve(stdio()).await.context("failed to start MCP server")?;

    tracing::info!("MCP server started on stdio");
    service.waiting().await.context("MCP server exited with error")?;

    Ok(())
}
