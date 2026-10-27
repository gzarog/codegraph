---
title: How It Works
description: The pipeline from source files to answers, step by step.
---

CodeGraph turns a project into a graph in five steps, then keeps it current.

1. **Discover.** Walk the project, honouring `.gitignore`, the built-in ignore list and [`codegraph.json`](/getting-started/configuration/). Each file's language comes from its extension (and, for ambiguous ones like `.h`, its content).
2. **Extract.** Parse every file with tree-sitter, in parallel, and walk the syntax tree for declarations (functions, classes, methods, types, variables…), imports, and the names each symbol uses: calls, type references, inheritance. Languages without a bundled grammar use lighter [pattern-based extractors](/reference/languages/). Framework detectors add [routes](/guides/framework-routes/); dispatch detectors add events, function pointers, HTTP requests, navigations and JSX renders.
3. **Store.** Write symbols, structural edges and the unresolved names into SQLite, with a full-text index over names.
4. **Resolve.** Turn each name into an edge to the symbol it means, using imports, module paths, scopes, receiver types and inheritance. See [Resolution & Frameworks](/core-concepts/resolution/).
5. **Answer.** Queries walk the graph: callers, callees, impact, call paths, and `explore`, which ranks the symbols a question is about and returns their source within a size budget.

## Staying current

Every file's content hash is stored. A sync re-reads only files whose size or modification time changed, re-extracts only those whose content changed, and re-resolves only the references that could be affected: those from changed files, those that pointed into them, and those that failed before but might now find a new symbol of the same name.

While an agent is connected, the MCP server watches the file system and syncs about a second after you stop typing. Answers that mention a file changed since the last sync carry a warning naming it.
