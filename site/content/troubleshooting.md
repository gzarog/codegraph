---
title: Troubleshooting
description: Fixes for the problems people run into most.
---

## My agent doesn't use CodeGraph

- Restart the agent after `codegraph install`; most agents read MCP configuration only at startup.
- Check the agent lists a `codegraph` server. In Claude Code, `/mcp` shows its status.
- Make sure the project is indexed: `codegraph status` in the project directory.
- Ask a structural question ("how does X reach Y"). Agents reach for `codegraph_explore` for those, and for their own search tools on plain text searches.

## "Another process is writing to this index"

A sync was already running. CodeGraph waits up to 30 seconds and then gives up. If no CodeGraph process is running, the lock is stale; `codegraph unlock` removes it.

## Results mention files I changed

Answers that rely on files edited since the last sync say so. The MCP server re-indexes about a second after you stop typing; from a terminal, run `codegraph sync`.

## A symbol has no callers that I know exist

- The caller may resolve only by name. In the viewer these are folded under "Uncertain".
- Calls through reflection, string lookups or dependency injection can't be seen statically.
- Event, HTTP, navigation and function-pointer calls are joined heuristically; check the "Dynamic dispatch" section of `explore`.

## Indexing is slow or the database is large

Exclude generated or vendored directories with `exclude` in [`codegraph.json`](/getting-started/configuration/). Files over 1 MB are skipped automatically. A full index of a 1,400-file TypeScript project takes about 10 seconds.

## `codegraph ui` says the viewer is missing

The binary was built without the `wasm32-unknown-unknown` target. Install a release binary, or `rustup target add wasm32-unknown-unknown` and rebuild.

## The daemon misbehaves

`codegraph daemon stop` stops it; the next agent session starts a fresh one. `CODEGRAPH_NO_DAEMON=1` bypasses it entirely.

## Still stuck?

[Open an issue](https://github.com/gzarog/codegraph/issues) with the output of `codegraph status` and `codegraph version`.
