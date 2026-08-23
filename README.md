# Relational Database MCP

MCP (Model Context Protocol) server for accessing relational databases. Supports MySQL, PostgreSQL, and SQLite through a unified interface.

Built with Rust using [rmcp](https://github.com/modelcontextprotocol/rust-sdk) and [sqlx](https://github.com/launchbadge/sqlx).

## Features

- **Multi-database support** -- MySQL, PostgreSQL, SQLite with automatic detection from connection URL
- **MCP Tools** -- Execute SQL, list tables, describe table schema
- **Structured output** -- Every tool declares an output schema and returns JSON as `structuredContent`, mirrored into a text block for clients that do not read structured results
- **MCP Resources** -- Each table exposed as a readable resource (JSON format)
- **stdio transport** -- Standard MCP communication via stdin/stdout

## Protocol version

The server advertises MCP protocol version **2026-07-28**. Clients requesting an
earlier version (2025-11-25 and older) are supported through version negotiation.

## Installation

### Prerequisites

- Rust 1.97+

### Build from source

```bash
cargo build --release -p rdb-mcp
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

All tools return JSON as `structuredContent`, alongside the same JSON serialized into a text block.

### Result set shape

Read queries and `describe_table` share one shape:

| Field       | Type                       | Description                                                                                         |
| ----------- | -------------------------- | --------------------------------------------------------------------------------------------------- |
| `columns`   | `string[]`                 | Column names in the order the database returned them. Empty when no rows were returned, because column metadata comes from the rows themselves. |
| `rows`      | `(string \| null)[][]`     | One entry per row, holding one value per column. `null` means SQL NULL.                              |
| `row_count` | `number`                   | Number of entries in `rows`.                                                                          |
| `truncated` | `boolean`                  | `true` when the result was cut off at the row limit.                                                  |

Values are returned as strings so that engine-specific types (fixed-point decimals, dates, unsigned integers) survive without lossy conversion. Binary values that are not valid UTF-8 are replaced by `<binary data: N bytes>`.

### `execute_sql`

Execute an arbitrary SQL query. Results are limited to 10,000 rows; larger result sets are cut off and reported with `truncated: true`.

| Parameter | Type   | Required | Description          |
| --------- | ------ | -------- | -------------------- |
| `query`   | string | Yes      | SQL query to execute |

The output is discriminated by `kind`: read queries (SELECT, SHOW, EXPLAIN, PRAGMA, DESCRIBE, and WITH/CTE selects) return a result set, and write and DDL statements return an affected-row count.

**Examples:**

```jsonc
// Read queries return a result set
execute_sql({ "query": "SELECT id, name, email FROM users" })
→ {
    "kind": "query",
    "columns": ["id", "name", "email"],
    "rows": [["1", "Alice", "alice@example.com"], ["2", "Bob", null]],
    "row_count": 2,
    "truncated": false
  }

// Write and DDL statements return the affected row count
execute_sql({ "query": "INSERT INTO users (name) VALUES ('Charlie')" })
→ { "kind": "execution", "rows_affected": 1 }
```

### `list_tables`

List all tables in the database. No parameters required.

```jsonc
list_tables()
→ { "tables": ["users", "orders", "products"] }
```

### `describe_table`

Describe the schema of a specific table, returning one row per column with its data type, nullability, and default. Constraint details vary by database engine.

| Parameter    | Type   | Required | Description                   |
| ------------ | ------ | -------- | ----------------------------- |
| `table_name` | string | Yes      | Name of the table to describe |

```jsonc
describe_table({ "table_name": "users" })
→ {
    "columns": ["cid", "name", "type", "notnull", "dflt_value", "pk"],
    "rows": [["0", "id", "INTEGER", "0", null, "1"], ["1", "name", "TEXT", "0", null, "0"]],
    "row_count": 2,
    "truncated": false
  }
```

The column names above are SQLite's; MySQL and PostgreSQL report their own `information_schema` columns.

Table names are validated to contain only alphanumeric characters and underscores.

## Resources

Each table in the database is exposed as an MCP resource.

| Property   | Value                                                            |
| ---------- | ---------------------------------------------------------------- |
| URI format | `{scheme}://{table_name}/data`                                   |
| MIME type  | `application/json`                                               |
| Content    | `SELECT * FROM {table} LIMIT 100`, in the result set shape above |

For example, a `users` table in a MySQL database is available at `mysql://users/data`.

## License

MIT
