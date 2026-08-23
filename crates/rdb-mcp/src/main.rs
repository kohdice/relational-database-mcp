//! MCP (Model Context Protocol) server for relational databases over stdio.
//!
//! Supports MySQL, PostgreSQL, and SQLite.

use anyhow::{Context, Result};
use clap::Parser;
use rdb_mcp_core::{db::DatabaseConnection, server::McpServer};
use rmcp::{ServiceExt, transport::stdio};

mod cli;

/// Parses CLI arguments, connects to the database, and runs the MCP server on stdio.
#[tokio::main]
async fn main() -> Result<()> {
    // stdout is reserved for the MCP JSON-RPC stream, so logs must go to stderr.
    // ANSI escape codes are disabled because the sink is not assumed to be a terminal.
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to initialize tracing subscriber: {e}"))?;

    let args = cli::Args::parse();

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
