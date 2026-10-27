---
title: Your First Graph
description: Index a project and explore it from the command line.
---

Once `codegraph init` has finished, the graph is ready to query. These commands work from anywhere inside the project.

## See what was indexed

```bash
codegraph status
```

`status` reports the number of files, symbols and relationships, the languages and frameworks found, and any files changed since the last sync.

## Find a symbol

```bash
codegraph query login
codegraph query "kind:function path:src/auth"
```

Search is ranked: exact names first, then prefixes, word parts (`getUserName` matches `user name`) and close spellings. Filters narrow it: `kind:`, `lang:`, `path:` and `name:`.

## Read code by question

```bash
codegraph explore "how does login reach the session store"
codegraph explore src/auth/session.ts:42
```

`explore` returns the source of the symbols the question is about, grouped by file and capped to a readable size, plus:

- **Flow**: the call path among the symbols you named.
- **Dynamic dispatch**: event, function-pointer, HTTP and navigation links into and out of them.
- **Blast radius**: what depends on them.

A `path:line` anchor shows the symbol around that line.

## Follow relationships

```bash
codegraph callers createSession
codegraph callees createSession
codegraph impact createSession
codegraph node createSession --code
```

`impact` lists everything that could be affected by changing a symbol, grouped by distance.

## Keep it current

```bash
codegraph sync              # re-index what changed since the last sync
codegraph watch             # keep syncing until interrupted
codegraph git-hooks install # sync after every commit, merge and checkout
```

While an agent is connected, the MCP server watches the project itself, so you rarely need these.
