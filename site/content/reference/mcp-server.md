---
title: MCP Server
description: The codegraph MCP server, its tools, and how agents use them.
---

`codegraph serve --mcp` speaks the [Model Context Protocol](https://modelcontextprotocol.io/) over stdio. `codegraph install` registers it with your agents; you can also add it by hand:

```json
{ "mcpServers": { "codegraph": { "command": "codegraph", "args": ["serve", "--mcp"] } } }
```

The server finds the project from the directory the agent starts it in, goes through the project's [shared daemon](/guides/daemon/), and watches files so answers stay current. When that directory has no index above it, the server tries, in order:

1. the `rootUri` or `workspaceFolders` the agent sends in `initialize`;
2. the agent's `roots/list`, if it advertises the `roots` capability;
3. the indexed projects below a workspace directory (one with `.git` or a manifest), up to three levels down. Exactly one is used as the default. With several, every tool marks `projectPath` as required, and a call without it lists the projects to choose from.

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

Every tool takes an optional `projectPath` to query another indexed project. A path inside a separate git repository that the enclosing project's index doesn't cover gets the same "not indexed" guidance as a project with no index. Such answers, and a call that names no project when there is no default, come back as ordinary text rather than errors, so the agent keeps using the tools for the projects that are indexed.

With `CODEGRAPH_MCP_TOOLS` set, tools outside the list are refused even when called by name.

## Freshness

When a file the answer mentions changed after the last sync, the answer starts with a warning naming it, so the agent knows to read that one file directly. Everything else in the answer is current.

## Lifecycle

The server exits when the agent that launched it exits or closes its input. A server that receives no traffic at all within 15 minutes of starting is assumed to be left over from an abandoned launch and exits; `CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS` changes the wait (`0` disables it). A `server/discover` probe is answered at once with "method not found", which sends newer clients on to `initialize`.

## Instructions

The server's `instructions` (shown to the agent at startup) explain when to reach for `codegraph_explore` instead of searching, and that its output can be treated as already-read source. The installer also adds a short marked block to `CLAUDE.md`, `AGENTS.md` or `GEMINI.md` for agents that read one.
