---
title: Rust API
description: Use CodeGraph as a library from Rust.
---

The `codegraph` crate exposes the same project facade the CLI uses. Add it from the repository:

```toml
[dependencies]
codegraph = { git = "https://github.com/gzarog/codegraph" }
codegraph-graph = { git = "https://github.com/gzarog/codegraph" }
```

## Open, index and query

```rust
use codegraph::CodeGraph;
use std::path::Path;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = Path::new(".");
    let mut cg = if codegraph::is_initialized(root) {
        CodeGraph::open(root)?
    } else {
        CodeGraph::init(root)?
    };
    let report = cg.sync(None)?;
    println!("{} files re-indexed", report.files_indexed());

    // The same text `codegraph explore` prints.
    println!("{}", cg.explore("how does login reach the session store", None)?);

    // Symbols by name, optionally in one file.
    let (defs, _) = cg.find_symbols("createSession", None)?;
    for d in &defs {
        let callers = cg.graph().callers(&d.id, codegraph_graph::DEPENDENCY_EDGES)?;
        println!("{} is used by {} symbols", d.name, callers.len());
    }
    Ok(())
}
```

## The crates

| Crate | Use it for |
|---|---|
| `codegraph` | The project facade (`CodeGraph`), MCP server, viewer server and CLI. |
| `codegraph-core` | Node, edge and language types; query parsing and ranking helpers. |
| `codegraph-extract` | Extracting symbols and references from one file's source. |
| `codegraph-store` | The SQLite store: writes, lookups and ranked search. |
| `codegraph-index` | Discovery, parallel extraction and incremental sync. |
| `codegraph-resolve` | Import parsing, module resolution and reference matching. |
| `codegraph-frameworks` | Framework detection, routes and dispatch sites. |
| `codegraph-graph` | Callers, callees, impact, paths, type hierarchy and dead code. |
| `codegraph-context` | `explore` and task context. |

`codegraph_extract::extract_file(path, source, language)` is useful on its own: it returns the symbols, edges and unresolved references of one file without touching a database.
