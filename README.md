# codegraph (Rust)

A Rust port of [CodeGraph](https://github.com/colbymchenry/codegraph): a
local-first knowledge graph of your codebase for AI coding agents. It parses
source with tree-sitter, stores symbols, relationships and files in SQLite
(FTS5), and serves the graph to agents over MCP and to you over a CLI.
Everything runs locally; one static binary is the indexer, the CLI and the MCP
server.

## Quick start

```bash
cargo install --path crates/codegraph

cd your-project
codegraph init                 # create .codegraph/ and index the project
codegraph explore "how does login reach the session store"
codegraph status
```

Wire it into an agent as a stdio MCP server:

```json
{ "mcpServers": { "codegraph": { "command": "codegraph", "args": ["serve", "--mcp"] } } }
```

The server watches the project and re-indexes changed files about a second
after you stop typing.

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
| `serve --mcp` | MCP server on stdio |

## MCP tools

`codegraph_explore` is listed by default. Set `CODEGRAPH_MCP_TOOLS=all` (or a
comma list such as `explore,node,callers`) to also expose `codegraph_search`,
`codegraph_callers`, `codegraph_callees`, `codegraph_impact`, `codegraph_node`,
`codegraph_status` and `codegraph_files`. Every tool takes an optional
`projectPath` to query another indexed project.

## Languages

Full symbol and reference extraction: TypeScript, TSX, JavaScript, JSX,
Python, Go, Rust, Java, Kotlin, Scala, C, C++, C#, PHP, Ruby, Swift, Dart, Lua,
Luau and R. Other recognized files (Svelte, Vue, Astro, Pascal, Solidity,
Terraform, …) are tracked as files.

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
| `codegraph-graph` | Callers/callees, impact, paths, type hierarchy, dead code |
| `codegraph-context` | Relevance ranking, `explore` and task context |
| `codegraph` | Project facade, CLI binary and MCP server |

```bash
cargo test --workspace
```

## License

MIT, as upstream. See [LICENSE](LICENSE).
