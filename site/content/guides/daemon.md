---
title: The Shared Daemon
description: How several agent sessions share one CodeGraph server per project.
---

Every agent session in a project talks to one background **daemon**. The sessions share one file watcher, one database connection and a warm index instead of each paying for their own.

## How it works

1. Your agent starts `codegraph serve --mcp`, as configured by `codegraph install`.
2. That process looks for the project's daemon. If none is running it starts one, detached from the agent, so closing one session never cuts off the others.
3. It relays the agent's MCP messages to the daemon until the agent disconnects.
4. Five minutes after its last client leaves, the daemon exits.

The daemon listens on `127.0.0.1` at a random port. Every connection has to present a secret token stored in `.codegraph/daemon.json`, which only you can read. When you upgrade CodeGraph, the first session on the new version replaces the old daemon. If no daemon can be reached, `serve` falls back to running in-process.

## Managing daemons

```bash
codegraph daemon                 # list running daemons
codegraph daemon stop            # stop this project's daemon
codegraph daemon stop --all      # stop every daemon
```

## Opting out

`codegraph serve --mcp --no-daemon`, or `CODEGRAPH_NO_DAEMON=1` in the agent's environment, serves in the agent's own process instead. `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS` changes how long an idle daemon lingers.

## Explore without repeats

With `CODEGRAPH_EXPLORE_DEDUP=1`, each session remembers which source `codegraph_explore` already sent. A later call replaces a run of eight or more unchanged lines with a pointer back to it. A file that changed in between is sent again in full. This is off by default because an MCP connection is not always one conversation: some agents reuse it for subagents, or discard old context while keeping the connection.
