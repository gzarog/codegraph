---
title: Indexing a Project
description: Create, update and maintain a project's index, and keep large repositories fast.
---

## Creating the index

```bash
codegraph init            # create .codegraph/ and index
codegraph init --no-index # create it now, index later
codegraph index           # rebuild everything from scratch
```

`init` refuses to index your home directory or a filesystem root unless you pass `--force`.

## Updating

```bash
codegraph sync
```

A sync only re-reads files whose size or modification time changed, and only re-extracts files whose content changed. On a 1,400-file TypeScript project a one-file sync takes about half a second.

Three ways to keep the index current without thinking about it:

- **The MCP server** watches the project while an agent is connected.
- **`codegraph watch`** does the same from a terminal.
- **Git hooks**, which sync in the background after commits, merges and checkouts:

```bash
codegraph git-hooks install
codegraph git-hooks remove
```

## What gets indexed

Everything with a recognised source extension that isn't ignored. Dependency and build directories (`node_modules`, `target`, `vendor`, `dist`, `build`, `.venv` and others) are always skipped, as are files over 1 MB and binary files. Generated files are detected by their headers and paths, indexed, and ranked lower. Use [`codegraph.json`](/getting-started/configuration/) to exclude more, include gitignored paths, or deprioritize directories.

## Concurrency

One process writes to an index at a time. A writer lock in `.codegraph/` makes a second writer wait up to 30 seconds and then give up with a clear message; readers are never blocked. If a crashed process left the lock behind, CodeGraph notices the process is gone; `codegraph unlock` removes a lock by hand.

## Errors

A file that fails to parse is still indexed as far as the parser got, with a warning visible in `codegraph status` and in the viewer's file page.
