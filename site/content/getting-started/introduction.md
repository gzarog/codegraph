---
title: Introduction
description: What CodeGraph is, and why it makes AI coding agents faster and more precise.
---

CodeGraph is a **local-first code-intelligence tool**. It parses your codebase with [tree-sitter](https://tree-sitter.github.io/), stores every symbol, relationship and file in a SQLite database inside your project, and serves the result as a queryable **knowledge graph**: to coding agents over the [Model Context Protocol](/reference/mcp-server/), to you over a [CLI](/reference/cli/) and a [browser viewer](/guides/viewer/), and to programs through a [Rust library](/reference/api/).

This is the Rust port of [colbymchenry/codegraph](https://github.com/colbymchenry/codegraph). One static binary is the indexer, the CLI, the MCP server and the viewer.

## Why it matters

When an agent works in a codebase, most of its effort goes into *discovery*: finding the right files before it can read them, then reconstructing how they connect with `grep`, `glob` and file reads. CodeGraph does that work once, ahead of time. An agent asks one question, such as "how does login reach the session store", and gets back the source of the symbols involved, the call path between them, and what else depends on them.

## What's in the graph

- **Symbols**: functions, methods, classes, structs, interfaces, enums, variables, routes, components and more.
- **Relationships**: calls, imports, inheritance, references, type uses, framework routes, and heuristic links for dynamic dispatch (events, function pointers, HTTP requests, navigations).
- **Files**: structure, language, and full-text search over symbol names (FTS5).

Extraction is **deterministic**: everything comes from syntax trees and name resolution, never from a language model.

## 100% local

Your code never leaves your machine. There are no API keys and no external services: the graph is a SQLite database in `.codegraph/`. The only network access is an optional once-a-day check for a new release, which you can [turn off](/reference/telemetry/).

Ready to try it? Head to the [Quickstart](/getting-started/quickstart/).
