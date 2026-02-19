# Relational Database MCP

MCP (Model Context Protocol) server for accessing relational databases. Supports MySQL, PostgreSQL, and SQLite through a unified interface.

Built with Rust using [rmcp](https://github.com/modelcontextprotocol/rust-sdk) and [sqlx](https://github.com/launchbadge/sqlx).

## Features

- **Multi-database support** -- MySQL, PostgreSQL, SQLite with automatic detection from connection URL
- **MCP Tools** -- Execute SQL, list tables, describe table schema
- **MCP Resources** -- Each table exposed as a readable resource (CSV format)
- **stdio transport** -- Standard MCP communication via stdin/stdout

## Installation

### Prerequisites

- Rust 1.92+

### Build from source

```bash
cargo build --release
```

The binary will be at `target/release/rdb-mcp`.

## Usage

```
rdb-mcp --database-url <CONNECTION_STRING>
```

The connection string can also be provided via the `DATABASE_URL` environment variable:

```bash
export DATABASE_URL="mysql://user:pass@localhost:3306/mydb"
rdb-mcp
```

When both `--database-url` and `DATABASE_URL` are provided, the CLI argument takes precedence. The database type is automatically detected from the URL scheme.

### Connection examples

```bash
# MySQL
rdb-mcp --database-url "mysql://user:pass@localhost:3306/mydb"

# PostgreSQL
rdb-mcp --database-url "postgres://user:pass@localhost:5432/mydb"

# SQLite
rdb-mcp --database-url "sqlite:./data.db"
```

## MCP Client Configuration

### Claude Desktop

Add the following to your Claude Desktop configuration file (`claude_desktop_config.json`):

```json
{
  "mcpServers": {
    "rdb-mcp": {
      "command": "/path/to/rdb-mcp",
      "env": {
        "DATABASE_URL": "sqlite:./data.db"
      }
    }
  }
}
```

### Claude Code

Add to your `.mcp.json`:

```json
{
  "mcpServers": {
    "rdb-mcp": {
      "command": "/path/to/rdb-mcp",
      "env": {
        "DATABASE_URL": "mysql://user:pass@localhost:3306/mydb"
      }
    }
  }
}
```

> **Note**: You can also use `"args": ["--database-url", "<CONNECTION_STRING>"]` instead of `"env"`. The `env` approach is recommended as it avoids placing credentials directly in the arguments list.

## Tools

### `execute_sql`

Execute an arbitrary SQL query. Read queries (SELECT, SHOW, EXPLAIN, PRAGMA, DESCRIBE, and WITH/CTE selects) return results as CSV. Write and DDL queries return the number of affected rows. Results are limited to 10,000 rows; larger result sets are truncated with a notification.

| Parameter | Type   | Required | Description          |
| --------- | ------ | -------- | -------------------- |
| `query`   | string | Yes      | SQL query to execute |

**Examples:**

```
-- Read queries return CSV
execute_sql({ "query": "SELECT id, name FROM users" })
→ id,name
  1,Alice
  2,Bob

-- Write/DDL queries return affected row count
execute_sql({ "query": "INSERT INTO users (name) VALUES ('Charlie')" })
→ Rows affected: 1
```

### `list_tables`

List all tables in the database. No parameters required.

```
list_tables()
→ users
  orders
  products
```

### `describe_table`

Describe the schema of a specific table, returning column names, data types, nullability, and defaults. Constraint details vary by database engine.

| Parameter    | Type   | Required | Description                   |
| ------------ | ------ | -------- | ----------------------------- |
| `table_name` | string | Yes      | Name of the table to describe |

```
describe_table({ "table_name": "users" })
→ (column details in CSV format)
```

Table names are validated to contain only alphanumeric characters and underscores.

## Resources

Each table in the database is exposed as an MCP resource.

| Property   | Value                             |
| ---------- | --------------------------------- |
| URI format | `{scheme}://{table_name}/data`    |
| MIME type  | `text/csv`                        |
| Content    | `SELECT * FROM {table} LIMIT 100` |

For example, a `users` table in a MySQL database is available at `mysql://users/data`.

## License

MIT
