---
title: MCP Server
description: The codegraph MCP server, its tools, and how agents use them.
---

`codegraph serve --mcp` speaks the [Model Context Protocol](https://modelcontextprotocol.io/) over stdio. `codegraph install` registers it with your agents; you can also add it by hand:

```json
{ "mcpServers": { "codegraph": { "command": "codegraph", "args": ["serve", "--mcp"] } } }
```

The server finds the project from the directory the agent starts it in (or the agent's root URI), goes through the project's [shared daemon](/guides/daemon/), and watches files so answers stay current.

## Tools

By default only `codegraph_explore` is listed: one well-described tool is what agents use most reliably. Set `CODEGRAPH_MCP_TOOLS=all`, or a list such as `explore,callers,impact`, to list more.

| Tool | What it returns |
|---|---|
| `codegraph_explore` | The source of the symbols a question is about, grouped by file and capped in size, plus the call path among them, dynamic-dispatch links and their blast radius. Takes `query` (a question or symbol and file names, `path:line` anchors allowed) and `maxFiles`. |
| `codegraph_search` | Symbol locations by name, with `kind`, `limit` and the `kind:`/`lang:`/`path:`/`name:` filters. |
| `codegraph_callers` | Who calls or uses a symbol. `symbol`, optional `file` to pick a definition, `limit`. |
| `codegraph_callees` | What a symbol calls or uses. |
| `codegraph_impact` | What changing a symbol could affect, by distance. `symbol`, `depth`. |
| `codegraph_node` | One symbol's location, signature, optional source and call trail; or, with `file` alone, a file's source with line numbers and its dependents. |
| `codegraph_files` | The indexed file tree with languages and symbol counts. |
| `codegraph_status` | Index health: counts, last sync, pending changes, and whether an update is available. |

Every tool takes an optional `projectPath` to query another indexed project.

## Freshness

When a file the answer mentions changed after the last sync, the answer starts with a warning naming it, so the agent knows to read that one file directly. Everything else in the answer is current.

## Instructions

The server's `instructions` (shown to the agent at startup) explain when to reach for `codegraph_explore` instead of searching, and that its output can be treated as already-read source. The installer also adds a short marked block to `CLAUDE.md`, `AGENTS.md` or `GEMINI.md` for agents that read one.
