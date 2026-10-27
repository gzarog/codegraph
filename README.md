# codegraph (Rust)

A Rust port of [CodeGraph](https://github.com/colbymchenry/codegraph): a
local-first knowledge graph of your codebase for AI coding agents. It parses
source with tree-sitter, stores symbols, relationships and files in SQLite
(FTS5), and serves the graph to agents over MCP and to you over a CLI.
Everything runs locally; one static binary is the indexer, the CLI and the MCP
server.

## Quick start

```bash
# a release binary (checksum-verified, into ~/.local/bin)
curl -fsSL https://raw.githubusercontent.com/gzarog/codegraph/main/install.sh | sh
# Windows: irm https://raw.githubusercontent.com/gzarog/codegraph/main/install.ps1 | iex
# or from source
cargo install --path crates/codegraph

cd your-project
codegraph init                 # create .codegraph/ and index the project
codegraph explore "how does login reach the session store"
codegraph status
```

Register it with your coding agents:

```bash
codegraph install                 # prompts for location and agents
codegraph install --yes           # global config, every detected agent
codegraph install --target claude,cursor --location local
codegraph install --print-config codex   # just show the snippet
codegraph uninstall --yes
```

Supported agents: Claude Code, Cursor, Codex CLI, opencode, Gemini CLI, Kiro,
Antigravity, Hermes Agent, and GitHub Copilot (VS Code, CLI, JetBrains). The
installer edits only CodeGraph's own entry (comments and other settings in
your config files are preserved), adds a short marked instructions block for
agents that read one (`CLAUDE.md`, `AGENTS.md`, `GEMINI.md`), and pre-allows
the `mcp__codegraph__*` tools for Claude Code unless `--no-permissions`.
Re-running is a no-op; `uninstall` reverses it.

Or wire it by hand as a stdio MCP server:

```json
{ "mcpServers": { "codegraph": { "command": "codegraph", "args": ["serve", "--mcp"] } } }
```

The server watches the project and re-indexes changed files about a second
after you stop typing. Every agent session in a project goes through one shared
background daemon (one watcher, one database connection), which exits five
minutes after its last client; `CODEGRAPH_NO_DAEMON=1` or `--no-daemon` serves
in-process instead. With `CODEGRAPH_EXPLORE_DEDUP=1`, `codegraph_explore` stops
re-sending source a session was already shown (unchanged files only) and points
back to it instead. Once a day it also checks GitHub for a newer release
and mentions it in `status` (`CODEGRAPH_NO_UPDATE_CHECK=1` or `DO_NOT_TRACK=1`
turns that off).

## Commands

| Command | What it does |
| --- | --- |
| `init [path]` | Create the index and build it |
| `index [path]` | Rebuild the whole index |
| `sync [path]` | Re-index files changed since the last sync |
| `status [path]` | Counts, languages, pending changes (`--json`) |
| `explore <query…>` | Source of the symbols a question is about, grouped by file, with the call path among named symbols and their blast radius |
| `context <task…>` | Entry points, related symbols, relationships and code for a task (`--format json`) |
| `query <search>` | Symbol search (`kind:`, `lang:`, `path:`, `name:` filters) |
| `node [name] [--file f]` | One symbol's details and call trail, or a file's source with its dependents |
| `callers` / `callees <symbol>` | Who uses a symbol / what it uses |
| `impact <symbol>` | What changing a symbol affects, by depth |
| `affected [files…]` | Test files that depend on the given files |
| `dead-code` | Symbols nothing references |
| `files` | Indexed file tree (`--format tree\|flat\|grouped`) |
| `watch [path]` | Keep the index in sync until interrupted |
| `serve --mcp [--no-daemon]` | MCP server on stdio, through the project's shared daemon |
| `daemon [list\|stop] [--all]` | List or stop the shared background daemons |
| `ui [--port N] [--no-open]` | Browser viewer on 127.0.0.1: symbol pages with callers/callees and blast radius, files with call arcs, entry points, a screen graph with the conditions on each transition, steps from any symbol, a module map, call-path flows, routes, dead code and saved trails; diagrams export as SVG or PNG |
| `install` / `uninstall` | Register CodeGraph with coding agents, or remove it |
| `upgrade [--check] [--version vX.Y.Z]` | Install the latest release (or `cargo install` for source installs) |
| `telemetry [on\|off\|status]` | Anonymous usage statistics; nothing is sent unless an endpoint is configured ([TELEMETRY.md](TELEMETRY.md)) |

## MCP tools

`codegraph_explore` is listed by default. Set `CODEGRAPH_MCP_TOOLS=all` (or a
comma list such as `explore,node,callers`) to also expose `codegraph_search`,
`codegraph_callers`, `codegraph_callees`, `codegraph_impact`, `codegraph_node`,
`codegraph_status` and `codegraph_files`. Every tool takes an optional
`projectPath` to query another indexed project.

## Languages

Full symbol and reference extraction: TypeScript, TSX, JavaScript, JSX,
Python, Go, Rust, Java, Kotlin, Scala, C, C++, C#, PHP, Ruby, Swift, Dart, Lua,
Luau and R. Svelte, Vue and Astro components (script blocks, template
component tags and event handlers), ArkTS, Liquid (render/include/section)
and Razor (`@using`, `@inject`, component tags) are extracted too.
Objective-C, Pascal/Delphi, Solidity, Terraform/HCL, Nix, Erlang, COBOL,
VB.NET and CFML use lighter pattern-based extractors (declarations, imports,
inheritance and call sites) without a bundled grammar. Other recognized
files are tracked as files.

Framework routes become `route` nodes linked to their handlers: Express, Koa,
Fastify, Hono, NestJS, Next.js, SvelteKit, Nuxt, Astro, Expo Router, Remix,
React/Vue/Angular routers, TanStack Router, Flask, FastAPI, Django, Spring,
JAX-RS, ASP.NET, Go routers, Axum, Actix, Rocket, Laravel, Symfony, Rails,
Sinatra, Vapor and Play.

Dispatch the call graph can't see is joined heuristically: an event emit
(`bus.emit('saved')`, `dispatchEvent(new Event('x'))`) calls the handlers bound
with `on`/`addEventListener`/`subscribe`; a C function-pointer call
(`f->ops->read(f)`) calls the functions assigned to that slot (`.read =
disk_read`); and a `fetch`/`axios`/`requests` call to a literal path calls the
project route that serves it; and a navigation (`router.push('/x')`,
`<Link href>`, `redirect('/x')`) links to the page route. JSX elements
(`<ItemCard/>`) count as calls to the component they render.

## Configuration

An optional `codegraph.json` at the project root:

```json
{
  "extensions": { ".jsm": "javascript" },
  "exclude": ["generated/", "*.pb.go"],
  "include": ["src/", "lib/"],
  "includeIgnored": ["vendor/our-fork/"],
  "deprioritize": ["legacy/"]
}
```

`.gitignore` is honored, and dependency, build and cache directories
(`node_modules`, `target`, `dist`, `.venv`, …) are always skipped. Files over
1 MB are tracked but not parsed. `CODEGRAPH_DIR` renames the `.codegraph`
data directory.

## Workspace

| Crate | Role |
| --- | --- |
| `codegraph-core` | Node/edge/language vocabulary, language detection, query parsing and ranking helpers |
| `codegraph-extract` | tree-sitter walkers per language (from upstream's Rust extraction kernel) |
| `codegraph-store` | SQLite schema, writes, lookups and ranked search |
| `codegraph-index` | File discovery, parallel extraction, incremental sync |
| `codegraph-resolve` | Import parsing, module resolution and reference matching |
| `codegraph-frameworks` | Framework detection, route extraction and dynamic-dispatch sites |
| `codegraph-http` | Minimal std-only HTTP/1.1 server |
| `codegraph-telemetry` | Telemetry allowlist, self-hosted ingest server and dashboard |
| `codegraph-graph` | Callers/callees, impact, paths, type hierarchy, dead code |
| `codegraph-context` | Relevance ranking, `explore` and task context |
| `codegraph-viewer` | The `codegraph ui` app: Rust compiled to WebAssembly (virtual DOM, routing, highlighting, graph layout) behind a small JS bridge |
| `codegraph` | Project facade, CLI binary and MCP server |

```bash
rustup target add wasm32-unknown-unknown   # the viewer is built into the binary
cargo test --workspace
```

Without the wasm target the binary still builds; `codegraph ui` then says the
viewer is missing. `CODEGRAPH_SKIP_VIEWER=1` skips the viewer build on purpose.

## License

MIT, as upstream. See [LICENSE](LICENSE).
