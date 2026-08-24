# Skills

Agent skills shipped with this repository. Each subdirectory holds one `SKILL.md` in the
standard format: YAML front matter with `name` and `description`, followed by the instructions
an agent reads when the skill matches.

| Skill                   | Purpose                                                                 |
| ----------------------- | ----------------------------------------------------------------------- |
| [`rdb-mcp`](./rdb-mcp/) | Using this MCP server's tools and resources to work against a database. |

These skills are runtime-neutral. They describe the MCP server's contract — tool names,
result shape, row caps, error messages — which every MCP client sees identically, and they
reference no runtime-specific command, agent type, or file path.

## Installing

Skills are discovered from a directory the runtime scans, so install by copying or
symlinking. Symlinking keeps the skill in step with this repository.

```bash
# Claude Code — project scope
mkdir -p .claude/skills && ln -s "$PWD/skills/rdb-mcp" .claude/skills/rdb-mcp

# Claude Code — user scope, available in every project
ln -s "$PWD/skills/rdb-mcp" "$HOME/.claude/skills/rdb-mcp"

# Codex and other runtimes that read the shared agent skills directory
mkdir -p "$HOME/.agents/skills" && ln -s "$PWD/skills/rdb-mcp" "$HOME/.agents/skills/rdb-mcp"
```

Check the runtime's own documentation for the directory it scans; the layout
(`<skills-dir>/<skill-name>/SKILL.md`) is the same either way.

## Connecting the server

A skill only tells the agent how to use the tools — the MCP server still has to be connected.
See [Installation](../README.md#installation) and
[MCP Client Configuration](../README.md#mcp-client-configuration) in the top-level README for
the binary and the connection URL, then register it wherever the runtime configures MCP
servers (`.mcp.json` for Claude Code, `~/.codex/config.toml` under `[mcp_servers.<name>]` for
Codex).
