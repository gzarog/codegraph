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

## Pattern-based extraction

These languages use lighter extractors without a bundled grammar: declarations sized by each language's block rules, imports, inheritance and call sites. Resolution works across files as for the others.

| Language | What is extracted |
|---|---|
| Objective-C | `@interface`, `@implementation`, `@protocol`, methods, C functions, `#import`, message sends |
| Pascal / Delphi | units, `uses`, classes and records, procedures and functions (nested too), method implementations |
| Solidity | contracts, interfaces, libraries, functions, modifiers, events, errors, structs, enums, `import`, inheritance |
| Terraform / HCL | resources, data sources, modules, variables, outputs, locals, and `var.`/`local.`/`module.`/resource references |
| Nix | bindings and functions, `import` and `callPackage` paths |
| Erlang | modules, functions (all clauses as one), records, includes, behaviours, local and remote calls |
| COBOL | programs, paragraphs and sections, `PERFORM`, `CALL`, `COPY` |
| VB.NET | classes, modules, structures, interfaces, enums, subs, functions, properties, `Imports`, `Inherits`, `Implements` |
| CFML | components (script and tag syntax), functions, `extends`, `new`/`createObject`, includes |

## Other files

Other recognised source files are tracked as files: they appear in `codegraph files` and in file-level results, without symbols.

To map an unusual extension to a language, use `extensions` in [`codegraph.json`](/getting-started/configuration/).
