---
title: Reading Your Graph in the Browser
description: The codegraph ui viewer: symbol pages, files with call arcs, a module map, screens, steps, flows and trails.
---

```bash
codegraph ui            # opens your browser
codegraph ui --no-open  # just print the address
codegraph ui --port 4000
```

The viewer runs on `127.0.0.1` only and makes no outbound connections. It is a Rust app compiled to WebAssembly and built into the `codegraph` binary.

Press `/` (or ⌘K / Ctrl+K) anywhere to search for a symbol.

## Symbol pages

A symbol's page shows its source with syntax highlighting, flanked by its **callers** and **callees**. Each one is grouped by the symbol on the other end, with the lines where the use happens. Links made by name alone are folded away under "Uncertain". Heuristic dispatch links are badged.

Below the source:

- **Members**, for types.
- **Type hierarchy**: what it extends and what extends it.
- **Blast radius**: how many symbols and files a change could reach within three hops, and which tests are among them.

A symbol with 40 or more distinct users is marked as a **hub**.

## Files

A file page shows the outline, the full source, and which files it uses and is used by. **Call arcs** in the gutter connect each call site to the definition it calls in the same file; dots mark calls that leave the file. Click an arc to jump to the definition.

## Entry points

Where to start reading an unfamiliar project:

- **Routes**: requests arriving from outside.
- **Files whose top-level code runs something**: CLIs, workers, scripts.
- **Tests**: the ones that reach the most files.
- **Hubs**: the most depended-on symbols.

## Screens

For a routed app, the screen graph shows the pages and the navigations between them. Each arrow is a `router.push`, `<Link>` or `redirect` found in code, traced back through the components that render it to the screen where it starts, and labelled with the condition guarding it: `item.collected`, `!user`. Navigations that no screen reaches, such as a redirect inside a store action, are listed separately.

## Steps

**Steps from here**, on any symbol, walks forward from it and draws only the moments that matter:

- screens it navigates to;
- requests to the project's own routes;
- handlers it wires up (`onClick={…}`, `addEventListener`);
- events, bridges into another language, and store actions;
- **effects** that leave the project: network, storage, file system, database, telemetry.

The ordinary calls in between are listed on the arrows, with the conditions along the way.

## Map, flow and trails

- **Map**: the project's directories as a dependency graph, laid out so modules sit above what they depend on.
- **Flow**: pick two symbols to see the call path from one to the other.
- **Trails**: the symbols you visit form a trail at the top of the page; save it to come back to later. Saved trails live in `.codegraph/trails.json`.

The map, screens and steps can be saved as SVG or copied as PNG, always in the light theme.

## When the index is behind

If files changed since the last sync, a banner says so and offers **Sync now**.
