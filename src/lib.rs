//! MCP (Model Context Protocol) server for relational databases.
//! Supports MySQL, PostgreSQL, and SQLite.

use anyhow::{Context, Result};
use clap::Parser;
use rmcp::{ServiceExt, transport::stdio};

pub mod cli;
pub mod db;
pub mod error;
pub mod server;

/// Parses CLI arguments, connects to the database, and runs the MCP server on stdio.
pub async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .try_init()
        .map_err(|e| anyhow::anyhow!("failed to initialize tracing subscriber: {e}"))?;

    let args = cli::Args::parse();

    let db = db::DatabaseConnection::connect(&args.database_url)
        .await
        .context("failed to connect to database")?;
    tracing::info!(db_type = %db.db_type(), "connected to database");

    let server = server::McpServer::new(db);
    let service = server.serve(stdio()).await.context("failed to start MCP server")?;

    tracing::info!("MCP server started on stdio");
    service.waiting().await.context("MCP server exited with error")?;

    Ok(())
}
