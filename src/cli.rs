use clap::Parser;

#[derive(Parser)]
#[command(version, about = "MCP server to access Relational Database")]
pub struct Args {
    #[arg(long, env = "DATABASE_URL")]
    pub database_url: String,
}
