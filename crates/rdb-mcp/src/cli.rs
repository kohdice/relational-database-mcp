//! Command-line argument definitions for the stdio MCP server binary.

use clap::Parser;

/// Command-line arguments accepted by the `rdb-mcp` binary.
#[derive(Parser)]
#[command(version, about = "MCP server to access Relational Database")]
pub struct Args {
    /// Connection URL of the target database (`mysql://`, `postgres://`, or `sqlite:`).
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,
}
