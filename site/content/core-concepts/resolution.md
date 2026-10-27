---
title: Resolution & Frameworks
description: How a name in the source becomes an edge to the right symbol, and how frameworks and dynamic dispatch add links that aren't in the syntax.
---

Extraction records each use of a name as an **unresolved reference**: `login` called from `handleSubmit` at line 12. Resolution decides which symbol that name means.

## The order of lookups

For a plain name, CodeGraph tries, in order:

1. A member of the enclosing type, for implicit `this`/`self` access.
2. A declaration in the same file that is visible from the reference (not a local of another function, not another type's field).
3. An import binding: named, default, aliased and namespace imports, re-exports, and barrel files.
4. The same package or namespace: Go packages, Java and C# namespaces, wildcard imports.
5. A project-wide match by name, preferring the closest file. These are marked as name-only matches.

For a qualified name (`user.save()`, `Auth::login`, `pkg.Func`) it resolves the left side first: `self`/`super`, an imported namespace, a module path (`other_crate::f`, `crate::util::g`), a variable whose declared type it can read, or a type name. It then looks for the member, walking up the inheritance chain.

Module paths are resolved per language: `tsconfig.json` paths and `package.json` workspaces, Python packages, Go modules, Cargo crates, Dart packages, and relative paths elsewhere.

## Frameworks

Routes come from framework conventions, detected from your manifests: Express, NestJS, Next.js, Flask, Django, Spring, Rails and [many more](/guides/framework-routes/). Each route becomes a `route` node with a `references` edge to its handler.

## Dynamic dispatch

Some links never appear as a call in the syntax. CodeGraph joins them heuristically:

- **Events.** `bus.on('saved', onSaved)` binds a handler, and `bus.emit('saved')` calls every handler bound to `saved`. Lifecycle events every stream emits (`error`, `close`, `data`, …) only join within one file.
- **Function pointers.** In C, `.read = disk_read` assigns a slot, and `f->ops->read(f)` calls every function assigned to `read`.
- **HTTP.** `fetch('/users/42')`, `axios.post('/login')` and `requests.get(...)` call the project's own route that serves the path.
- **Navigation.** `router.push('/items/' + id)`, `<Link href="/about">` and `redirect('/login')` navigate to the page route.
- **JSX.** `<ItemCard/>` inside `ItemGrid` is a call from `ItemGrid` to `ItemCard`.

These edges are marked as heuristic. `explore` lists them in their own section, and the viewer badges them.
