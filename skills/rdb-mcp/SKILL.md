---
name: rdb-mcp
description: This skill should be used when working against a relational database (MySQL, PostgreSQL, SQLite) through the rdb-mcp MCP server — exploring an unknown schema, writing and running SQL, reading table data, or interpreting the server's results. Triggers on requests such as "query the database", "what tables exist", "show me the schema of <table>", "run this SQL", "DB を調べて", "テーブル一覧を見せて", "このクエリを実行して". It explains the three tools (execute_sql, list_tables, describe_table), the table-data resources, the result shape and its string-only values, the row caps and truncation flag, the single-statement and classification rules of execute_sql, engine differences, and the confirmation discipline required before write and DDL statements. Do NOT use it for developing this server's own Rust code — that is ordinary repository work.
---

# Using the rdb-mcp MCP Server

`rdb-mcp` exposes one relational database — MySQL, PostgreSQL, or SQLite — over MCP. The
engine is fixed at server startup by `DATABASE_URL` (or `--database-url`); no tool switches
databases, schemas, or credentials at call time.

This skill is runtime-neutral: it describes the MCP server's own contract, which every MCP
client sees identically. It names no runtime-specific command, agent, or file path.

## Tool names

The server registers three tools: `execute_sql`, `list_tables`, `describe_table`. Runtimes
prefix MCP tools with the server name in their own way, and the server name is whatever key
the host's MCP configuration used — so the same tool may be listed as `execute_sql`,
`rdb-mcp__execute_sql`, or `mcp__rdb-mcp__execute_sql`. Call the tools by the names the
current runtime lists; never assume a prefix. The names in this document are the server-side
names, always the suffix of whatever the runtime shows.

If none of the three is listed, the server is not connected — say so instead of guessing at
the data, and point the user at the connection setup in this repository's `README.md`.

## When to use

- Any question answerable from the database's data or schema.
- Running SQL the user hands over.
- Exploring a schema that is not documented elsewhere.

Do not use it to:

- Guess at data. If a query fails, fix the query — never fabricate rows.
- Manage the database's lifecycle (users, backups, replication). The server is one connection
  pool over one database; use the engine's own tooling for that.

## Workflow

### Step 1: Orient before querying

Never write SQL against a schema assumed from a table name. Start with `list_tables`, then
`describe_table` on the tables that look relevant. Both are read-only and cheap.

```jsonc
list_tables()
→ { "tables": ["orders", "products", "users"] }
```

`list_tables` covers only the default user-facing schema — MySQL: the current database;
PostgreSQL: the `public` schema; SQLite: non-system tables. Tables in another PostgreSQL
schema exist but are not listed; reach them through `execute_sql` with a schema-qualified
name (`analytics.events`).

### Step 2: Inspect the columns

```jsonc
describe_table({ "table_name": "users" })
→ {
    "columns": ["name", "data_type", "is_nullable", "column_default", "primary_key"],
    "rows": [["id", "integer", "NO", null, "YES"], ["email", "text", "YES", null, "NO"]],
    "row_count": 2,
    "truncated": false
  }
```

The five result columns and their order are identical on every engine. `data_type` is **not**
normalized: the same declaration reads `int` on MySQL, `integer` on PostgreSQL, `INTEGER` on
SQLite. Compare types by meaning, not by string equality across engines.

`table_name` is validated to `[a-zA-Z0-9_]`, 1–128 characters. A table whose real name falls
outside that set (spaces, hyphens, non-ASCII) cannot be described by this tool — query the
engine's catalog through `execute_sql` instead, quoting the identifier yourself.

`describe_table` reports columns only. Indexes, foreign keys, check constraints, and triggers
are not exposed; read them from the catalog when needed:

- MySQL: `SELECT * FROM information_schema.key_column_usage WHERE table_schema = DATABASE() AND table_name = 'orders'`
- PostgreSQL: `SELECT conname, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'public.orders'::regclass`
- SQLite: `SELECT * FROM pragma_foreign_key_list('orders')` / `SELECT * FROM pragma_index_list('orders')`

### Step 3: Query with `execute_sql`

```jsonc
execute_sql({ "query": "SELECT id, name, email FROM users WHERE active = 1 ORDER BY id LIMIT 50" })
→ {
    "kind": "query",
    "columns": ["id", "name", "email"],
    "rows": [["1", "Alice", "alice@example.com"], ["2", "Bob", null]],
    "row_count": 2,
    "truncated": false
  }
```

Rules that decide whether a call succeeds and how its result is shaped:

- **One statement per call.** A semicolon anywhere but at the very end makes the server treat
  the input as a write, so a `SELECT` in a multi-statement string silently returns
  `rows_affected` instead of rows. Split the statements into separate calls.
- **Read shape** (`kind: "query"`) is chosen by the leading keyword: `SELECT`, `SHOW`,
  `PRAGMA`, `DESCRIBE`, `EXPLAIN`, and `WITH` whose body contains no `INSERT`/`UPDATE`/
  `DELETE`/`MERGE`. Everything else returns `kind: "execution"` with `rows_affected`.
- **A data-modifying CTE still writes.** `WITH x AS (INSERT ... RETURNING *) SELECT * FROM x`
  is classified as a write, so its `RETURNING` rows are discarded. Same for
  `EXPLAIN ANALYZE DELETE ...`, which really deletes. Treat both as writes.
- **No bind parameters.** The tool takes one SQL string; there is no argument list. Values
  must be written into the statement, so escape quotes by doubling them (`'it''s'`) and never
  interpolate untrusted text.
- **Two literal forms are rejected**, because the server's scanner does not understand them
  and refuses rather than misclassify: backslash escapes inside MySQL strings (`'a\'b'`) and
  PostgreSQL dollar quoting (`$$...$$`). Rewrite them — `'a''b'` and a standard `'...'`
  literal — or the call fails with "unterminated quoted string or identifier".

### Step 4: Read the result honestly

Read queries and `describe_table` share one shape:

| Field       | Meaning                                                                                                            |
| ----------- | ------------------------------------------------------------------------------------------------------------------ |
| `columns`   | Column names in result order. **Empty when zero rows came back**, because the names come from the rows themselves. |
| `rows`      | One array per row, one value per column. `null` is SQL NULL.                                                       |
| `row_count` | `rows.length`.                                                                                                     |
| `truncated` | `true` when the result was cut off at the row cap.                                                                 |

- **Every value is a string.** `"1"` is an integer column, `"12.50"` a decimal,
  `"2026-08-24"` a date. Strings preserve fixed-point decimals, unsigned integers, and dates
  without lossy conversion — so do arithmetic, comparison, and aggregation **in SQL**
  (`SUM(amount)`, `COUNT(*)`), not by parsing the strings afterwards.
- **`base64:` prefix** marks a binary value that is not valid UTF-8; the rest of the string is
  standard base64 of the original bytes.
- **`truncated: true` means the answer is incomplete.** Say so, and re-run with a narrower
  `WHERE`, an aggregate, or an `ORDER BY ... LIMIT ... OFFSET` page. Never present a truncated
  result as the whole set.

Row caps: `execute_sql` keeps at most **10,000** rows, a resource read at most **100**. A
result of exactly the cap with `truncated: false` really did end there.

## Writes and DDL

`execute_sql` runs arbitrary SQL, writes included — it is annotated `destructive_hint` and
performs **no** permission check. `is_read_query` only picks the response shape; it never
blocks a statement.

- **Confirm with the user before any `INSERT`, `UPDATE`, `DELETE`, `TRUNCATE`, `DROP`,
  `ALTER`, or `CREATE`** unless they already asked for that exact statement. Show the SQL you
  intend to run first.
- **Preview destructive scope first.** Run `SELECT COUNT(*) FROM t WHERE <same predicate>`
  before the `DELETE`/`UPDATE`, and compare it against the `rows_affected` you get back.
- **Never send an unqualified `UPDATE` or `DELETE`.** Without a `WHERE` clause the statement
  hits every row.
- **Transactions do not span calls.** Each call takes a connection from the pool, so `BEGIN`
  in one call and `COMMIT` in another may land on different connections. There is no undo:
  wrap what must be atomic into a single statement, or have the user run a multi-statement
  transaction through a real client.

## Resources

Each listed table is also readable as a resource:

| Property | Value                                                                         |
| -------- | ----------------------------------------------------------------------------- |
| URI      | `{scheme}://{table_name}/data`, scheme being `mysql`, `postgres`, or `sqlite` |
| MIME     | `application/json`                                                            |
| Content  | The first 100 rows, in the result shape above                                 |

The scheme must match the connected engine — `mysql://users/data` against a SQLite server is
a "resource not found" error, not an empty read. The read is `SELECT * FROM <table>` with no
`ORDER BY`, so "first 100" is whatever the engine returns first. For a deliberate sample, use
`execute_sql` with an explicit `ORDER BY ... LIMIT`.

`list_resources` skips tables whose names contain characters outside `[a-zA-Z0-9_]` and
reports them in the result's `_meta.skippedTables`. Such tables are still reachable through
`execute_sql`.

## Errors and what they mean

| Message                                                          | Cause and fix                                                                                                                   |
| ---------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| `invalid SQL: unterminated quoted string or identifier`          | Odd quote count, or an unsupported literal form (see Step 3). Rewrite it.                                                       |
| `invalid SQL: unterminated block comment`                        | A `/*` with no `*/`. Note `/*/` is unterminated, not an empty comment.                                                          |
| `database query error: ...`                                      | The engine rejected the statement (syntax, unknown column, constraint). Fix the SQL.                                            |
| `table '<name>' not found or has no columns`                     | `describe_table` on a table absent from the default schema. Check `list_tables`.                                                |
| `table name '...' contains invalid characters`                   | Outside `[a-zA-Z0-9_]`. Use `execute_sql` on the catalog instead.                                                               |
| `failed to decode column value: unsupported SQL type ...`        | The column's type has no text mapping. Cast it: `CAST(col AS CHAR)` on MySQL, `col::text` on PostgreSQL.                        |
| `database connection pool timed out` / `I/O error` / `TLS error` | Infrastructure, not your query. Report it; retrying the same SQL will not help until the server or database is reachable again. |

A decode failure fails the **whole call**, not the one cell — the server refuses to substitute
a placeholder for a value it cannot represent. Re-run the query with that column cast to text.

## Common recipes

```sql
-- Exact size before pulling rows (avoids a wasted 10,000-row read)
SELECT COUNT(*) FROM orders;

-- Deterministic page
SELECT * FROM orders ORDER BY id LIMIT 100 OFFSET 200;

-- Distinct values of a low-cardinality column
SELECT status, COUNT(*) FROM orders GROUP BY status ORDER BY 2 DESC;

-- Which engine and version am I on?
SELECT VERSION();                    -- MySQL
SELECT version();                    -- PostgreSQL
SELECT sqlite_version();             -- SQLite

-- Find tables by name pattern when list_tables is too coarse
SELECT table_name FROM information_schema.tables WHERE table_name LIKE '%order%';
```

## Checklist before reporting an answer

- [ ] Schema confirmed with `list_tables` / `describe_table`, not assumed.
- [ ] `truncated` checked and stated whenever it is `true`.
- [ ] Numeric work done in SQL, not by parsing the returned strings.
- [ ] Any write or DDL confirmed with the user beforehand, and `rows_affected` reported back.
- [ ] Failures reported as failures, with the server's message — never filled in with invented data.
