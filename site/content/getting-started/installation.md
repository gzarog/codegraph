---
title: Installation
description: Install the codegraph binary on macOS, Linux or Windows, upgrade it, or build it from source.
---

CodeGraph is a single binary with no runtime dependencies.

## Install script (macOS and Linux)

```bash
curl -fsSL https://raw.githubusercontent.com/gzarog/codegraph/main/install.sh | sh
```

The script downloads the release archive for your platform, checks it against the release's `SHA256SUMS`, and installs `codegraph` into `~/.local/bin`. Set `CODEGRAPH_BIN_DIR` to install elsewhere, or `CODEGRAPH_VERSION` (for example `v0.2.0`) to pin a release.

## Install script (Windows)

```powershell
irm https://raw.githubusercontent.com/gzarog/codegraph/main/install.ps1 | iex
```

This installs into `%LOCALAPPDATA%\codegraph\bin` and adds that folder to your user `PATH`.

## From source

With a Rust toolchain:

```bash
rustup target add wasm32-unknown-unknown   # the browser viewer is built into the binary
cargo install --git https://github.com/gzarog/codegraph codegraph --locked
```

Without the wasm target the binary still builds; only `codegraph ui` is unavailable.

## Upgrading

```bash
codegraph upgrade           # install the latest release
codegraph upgrade --check   # only report whether one exists
```

A release binary is replaced in place after its checksum is verified. A binary installed with `cargo install` upgrades through `cargo install` again. The MCP server also checks for new releases once a day and mentions an available update in `codegraph status`.

## Uninstalling

```bash
codegraph uninstall --yes                 # remove CodeGraph from your agents' configs
curl -fsSL https://raw.githubusercontent.com/gzarog/codegraph/main/install.sh | sh -s -- --uninstall
```

Delete a project's index with `codegraph uninit`.
