---
title: The Knowledge Graph
description: The kinds of nodes and edges CodeGraph stores, and what each one means.
---

## Nodes

| Kind | Examples |
|---|---|
| `file` | One per indexed file. |
| `module`, `namespace` | Rust modules, C# namespaces, Pascal units, Erlang modules. |
| `class`, `struct`, `interface`, `trait`, `protocol`, `enum`, `type_alias`, `union` | Type declarations. |
| `function`, `method` | Callables, including nested and anonymous ones that are named by assignment. |
| `property`, `field`, `variable`, `constant`, `enum_member` | Data declarations. |
| `route` | An HTTP route or page from a [framework](/guides/framework-routes/), such as `GET /users/:id`. |
| `component` | Svelte, Vue and Astro components. |
| `import`, `export`, `parameter` | Kept for precise resolution; hidden from most results. |

Every node has a file, a line range, a qualified name (`src/auth.ts::AuthService::login`) and, where the language has them, a signature, docstring, visibility and decorators.

## Edges

| Kind | Meaning |
|---|---|
| `contains` | A file contains a class, a class contains a method. |
| `calls` | A call site, a JSX render, or a fired event reaching a listener. |
| `imports` | A file imports a module or file. |
| `extends`, `implements` | Inheritance. |
| `references` | Any other use: a type in a signature, a function passed as a value, a route's handler. |
| `instantiates` | `new Foo()` and constructor calls. |
| `type_of`, `returns` | Declared types. |
| `decorates`, `overrides` | Decorators and overridden methods. |
| `navigates` | Code that navigates to a page route. |

Edges carry the line they come from and how they were resolved, with a confidence. Links made by name alone, or by the dynamic-dispatch heuristics, are marked as heuristic and shown apart in the viewer.

## Where it lives

The graph is one SQLite database in `.codegraph/codegraph.db` (WAL mode), next to a writer lock, the daemon's address while one runs, and the viewer's saved trails. Delete the directory, or run `codegraph uninit`, to remove everything.
