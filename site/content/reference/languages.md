---
title: Languages
description: Which languages CodeGraph indexes, and how deeply.
---

## Full extraction

These are parsed with tree-sitter grammars: declarations with signatures and docstrings, imports, calls, type references, inheritance and decorators.

TypeScript, TSX, JavaScript, JSX, Python, Go, Rust, Java, Kotlin, Scala, C, C++, C#, PHP, Ruby, Swift, Dart, Lua, Luau and R.

## Components and templates

- **Svelte, Vue and Astro**: script blocks are parsed in place, so line numbers stay true. Templates contribute component tags and event handlers. The component itself is a node.
- **ArkTS**: parsed as TypeScript.
- **Liquid**: `render`, `include` and `section` tags become imports.
- **Razor**: `@using`, `@inject`, `@model` and component tags.

## Grammar walker

These languages are parsed with their tree-sitter grammars by a walker each language configures, so declarations carry their real extent, signature and docstring. Resolution works across files as for the others.

| Language | What is extracted |
|---|---|
| Objective-C | `@interface` and `@implementation` as one class, `@protocol`, `@property`, methods named by selector (`store:forKey:`), C functions, enums (`NS_ENUM` too), `#import`, message sends as `receiver.selector` |
| Pascal / Delphi | `uses`, classes, records and interfaces with their base types, enums, method implementations (`TShape.Draw` is TShape's method), nested routines, parameterless calls, `inherited` |
| Solidity | contracts, interfaces, libraries, functions, modifiers, constructors, `fallback`/`receive`, events and errors (so `emit` and `revert` resolve), structs, enums, `import`, inheritance |
| Terraform / HCL | resources, data sources, modules (and a local module's `main.tf`), variables, outputs, providers, locals, and `var.`/`local.`/`module.`/`data.`/resource references; `.tfvars` assignments |
| Nix | bindings and functions (exported when the file returns them), applications as calls, `import`, `callPackage` and module `imports` paths |
| Erlang | modules, functions by name and arity (`shop::add/2`, all clauses as one), records, types, macros, includes, behaviours, local and remote calls, `fun f/1`, `gen_server:call` to the target's `handle_call/3`, `spawn`/`apply` targets |
| CFML | components (script and tag syntax) named after their file, functions, properties, `extends`/`implements`, `new`/`createObject`, `<cfinclude>`, `<cfinvoke>` |

## Pattern-based extraction

COBOL and VB.NET use lighter extractors: declarations sized by each language's block rules, imports, inheritance and call sites.

| Language | What is extracted |
|---|---|
| COBOL | programs, paragraphs and sections, `PERFORM`, `CALL`, `COPY` |
| VB.NET | classes, modules, structures, interfaces, enums, subs, functions, properties, `Imports`, `Inherits`, `Implements` |

## Other files

Other recognised source files are tracked as files: they appear in `codegraph files` and in file-level results, without symbols.

To map an unusual extension to a language, use `extensions` in [`codegraph.json`](/getting-started/configuration/).
