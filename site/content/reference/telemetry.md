---
title: Telemetry
description: What CodeGraph can collect, when it sends anything, and how to turn it off.
---

CodeGraph can collect a small set of **anonymous usage statistics**: which commands and MCP tools are used, which languages get indexed, and which agents it is installed into. The complete list is in [TELEMETRY.md](https://github.com/gzarog/codegraph/blob/main/TELEMETRY.md); the client and the server enforce the same allowlist.

## Nothing is sent without an endpoint

This build has no built-in collection endpoint. Telemetry is recorded and sent only when `CODEGRAPH_TELEMETRY_ENDPOINT` points at a server, at runtime or when the binary was built. Without one, nothing is recorded and no connection is opened.

## Turning it off

```bash
codegraph telemetry off       # also deletes the machine ID and anything unsent
export CODEGRAPH_TELEMETRY=0
export DO_NOT_TRACK=1         # always wins
```

`codegraph telemetry status` shows the state, what decided it, the endpoint and the machine ID.

## What is never collected

No source code, file paths, file or repository names, symbol names, queries, prompts, IP addresses, usernames, hostnames or environment variables. The machine ID is a random UUID minted on first send.

## Running your own collector

`codegraph-telemetry-server` receives events, stores them in SQLite with daily rollups and a retention window, and serves a dashboard behind a token:

```bash
cargo build --release -p codegraph-telemetry
CODEGRAPH_DASHBOARD_TOKEN=$(openssl rand -hex 16) \
  ./target/release/codegraph-telemetry-server --listen 127.0.0.1:8787 --db telemetry.db
```

Put it behind a TLS-terminating proxy and set `CODEGRAPH_TELEMETRY_ENDPOINT=https://your-host/v1/events` for the builds you want to measure.

## Update checks

Separately, the MCP server checks GitHub once a day for a newer release. It fetches a version number and sends nothing about you. `CODEGRAPH_NO_UPDATE_CHECK=1` or `DO_NOT_TRACK=1` turns it off.
