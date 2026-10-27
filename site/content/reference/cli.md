---
title: CLI
description: Every codegraph command and its options.
---

Commands that read an index find it by walking up from the current directory; `--path` points elsewhere.

## Project

| Command | What it does |
|---|---|
| `init [path] [--force] [--no-index]` | Create `.codegraph/` and index the project. |
| `index [path] [--force] [--quiet]` | Rebuild the whole index. |
| `sync [--quiet]` | Re-index files changed since the last sync. |
| `status [--json]` | Counts, languages, frameworks, pending changes and update notices. |
| `watch` | Keep the index in sync until interrupted. |
| `git-hooks install` / `git-hooks remove` | Sync in the background after commits, merges and checkouts. |
| `unlock` | Remove a writer lock left behind by a crashed process. |
| `uninit [path]` | Delete the project's index (and its git hooks). |

## Querying

| Command | What it does |
|---|---|
| `explore <question…> [--max-files N]` | Source of the symbols a question is about, with call paths, dispatch links and blast radius. |
| `context <task…> [--format markdown\|json] [-n N] [--no-code]` | Entry points, related symbols, relationships and code for a task. |
| `query <search> [--kind K] [--limit N] [--json]` | Ranked symbol search, with `kind:`, `lang:`, `path:` and `name:` filters. |
| `node [name] [--file F] [--code] [--line N] [--offset N --limit N] [--symbols-only]` | One symbol's details and call trail, or a file's source with its dependents. |
| `callers <symbol>` / `callees <symbol>` `[--file F] [--limit N] [--json]` | Who uses a symbol, or what it uses. |
| `impact <symbol> [--depth N] [--json]` | What changing a symbol could affect, by distance. |
| `affected [files…] [--stdin] [--depth N] [--filter GLOB] [--json] [--quiet]` | Test files that depend on the given files. See [Affected Tests in CI](/guides/affected-tests/). |
| `dead-code [--limit N]` | Symbols nothing references. |
| `files [--format tree\|flat\|grouped] [--filter DIR] [--pattern GLOB] [--max-depth N] [--json]` | The indexed file tree. |

## Agents, viewer and service

| Command | What it does |
|---|---|
| `install` / `uninstall` | Register CodeGraph with coding agents, or remove it. See [Agent Integrations](/reference/integrations/). |
| `serve --mcp [--no-watch] [--no-daemon]` | The MCP server on stdio. |
| `daemon [list\|stop] [--all]` | List or stop the [shared daemons](/guides/daemon/). |
| `ui [--port N] [--no-open]` | The [browser viewer](/guides/viewer/). |
| `upgrade [--check] [--version TAG] [--force]` | Install the latest release. |
| `telemetry [on\|off\|status]` | [Telemetry](/reference/telemetry/) settings. |
| `version` | Print the version. |
