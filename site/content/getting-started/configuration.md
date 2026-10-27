---
title: Configuration
description: Control what gets indexed with codegraph.json, and the environment variables CodeGraph reads.
---

Most projects need no configuration: CodeGraph respects `.gitignore`, skips dependency and build directories (`node_modules`, `target`, `vendor`, `dist` and similar), and recognises files by extension. When you need more control, add a `codegraph.json` at the project root.

## codegraph.json

```json
{
  "exclude": ["fixtures/**", "**/*.generated.ts"],
  "include": [],
  "includeIgnored": ["vendor/our-fork/**"],
  "deprioritize": ["legacy/**"],
  "extensions": { ".mjsx": "javascript", ".tpl": "php" }
}
```

| Field | Meaning |
|---|---|
| `exclude` | Gitignore-style patterns that are never indexed. |
| `include` | When non-empty, only matching paths are indexed. |
| `includeIgnored` | Gitignored paths to index anyway. |
| `deprioritize` | Paths that are indexed but ranked lower in search and `explore`. |
| `extensions` | Extra extension → language mappings. |

Invalid entries are dropped with a warning in `codegraph status`; the rest of the file still applies. Changes take effect on the next sync.

## Environment variables

| Variable | Effect |
|---|---|
| `CODEGRAPH_MCP_TOOLS` | Which MCP tools are listed: `all`, or a comma list such as `explore,search`. The default lists only `codegraph_explore`. |
| `CODEGRAPH_NO_DAEMON=1` | Serve MCP in-process instead of through the [shared daemon](/guides/daemon/). |
| `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` | How long a daemon stays up without clients (default 300000). |
| `CODEGRAPH_EXPLORE_DEDUP=1` | Let `codegraph_explore` skip source a session was already shown. |
| `CODEGRAPH_NO_UPDATE_CHECK=1` | Turn off the daily release check. |
| `CODEGRAPH_TELEMETRY=0`, `DO_NOT_TRACK=1` | Turn off [telemetry](/reference/telemetry/). |
| `CODEGRAPH_HOME` | Where machine-wide state lives (default `~/.codegraph`). |
| `CODEGRAPH_NO_PROMPT_HOOK=1` | Disable the Claude Code prompt hook for one session. |
