#[tokio::main]
async fn main() -> anyhow::Result<()> {
    relational_database_mcp::run().await
}
