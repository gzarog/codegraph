# Telemetry

CodeGraph can collect a small set of **anonymous usage statistics** — which
commands and MCP tools get used, which languages get indexed, which agents it
is installed into — so effort goes where it matters. This page is the complete
list of what is collected. If a field isn't here, it isn't collected: the
client builds payloads from, and the server enforces, one allowlist
([`crates/codegraph-telemetry/src/protocol.rs`](crates/codegraph-telemetry/src/protocol.rs)).

## Nothing is sent unless an endpoint is configured

This port has no built-in collection endpoint. Telemetry is recorded and sent
only when `CODEGRAPH_TELEMETRY_ENDPOINT` points at a server — at runtime, or
set when a release binary is built. Without one, CodeGraph records nothing and
opens no connection. To collect for your own builds, run the bundled server
(below) and set the variable.

## Turning it off

Any of these works, permanently:

```bash
codegraph telemetry off        # stores your choice and deletes the machine ID and any unsent data
export CODEGRAPH_TELEMETRY=0   # per shell / per CI
export DO_NOT_TRACK=1          # the cross-tool standard; always wins
```

`codegraph telemetry status` shows the state, what decided it, the endpoint
and the machine ID. `CODEGRAPH_TELEMETRY=1` forces it on for one process even
when the stored choice is off; `DO_NOT_TRACK=1` takes precedence over
everything. Before the first send, a one-line notice is printed to stderr.

Separately, `codegraph serve` checks GitHub once a day for a newer release so
it can tell you one exists. It fetches a version number and sends nothing
about you. `DO_NOT_TRACK=1` or `CODEGRAPH_NO_UPDATE_CHECK=1` turn it off.

## What is collected

Every request carries this envelope:

| field | example | notes |
|---|---|---|
| `machine_id` | `b3a8c1…` | random UUID minted on first send, derived from nothing |
| `codegraph_version` | `0.1.0` | |
| `os` / `arch` | `linux` / `x86_64` | platform identifiers only |
| `ci` | `false` | whether the `CI` variable was set |
| `schema_version` | `1` | bumped when this page changes |

And one of four events:

- **`install`** — when `codegraph install` configures agents: which agents
  (`["claude","cursor"]`), global or local, and whether it was a fresh
  install, an upgrade of an existing entry, or a re-run.
- **`index`** — when a full index completes: the **language names** present
  (`["rust","typescript"]`), the file count as a **bucket** (`<100`, `100-1k`,
  `1k-10k`, `10k+`) and the duration as a bucket (`<10s`, `10-60s`, `1-5m`,
  `5m+`).
- **`usage_rollup`** — one line per day per command or tool: its **name**
  (`codegraph_explore`, `init`), how many times it ran and how many errored,
  and for MCP tools the connecting agent's name and version from the MCP
  handshake (`Claude Code 2.1`).
- **`uninstall`** — when `codegraph uninstall` runs: which agents were removed.

Usage is added into **daily totals locally**, and only completed days are
sent. There is no per-call stream and nothing is sent in real time.

## What is never collected

- **No source code**, file paths or names, directory or repository names,
  symbol names, search queries, prompts, or anything derived from a project.
- **No IP addresses.** The server never reads, logs or stores the client
  address.
- **No fingerprinting.** The machine ID is a random UUID in
  `~/.codegraph/telemetry.json`; `codegraph telemetry off` deletes it.
- **No personal data**: no usernames, hostnames, emails or environment
  variables.

## How it travels

Sends are fire-and-forget through the system `curl` with a 5-second limit;
nothing waits on them and nothing retries. Offline machines keep a bounded
local buffer (256 KB) in `~/.codegraph/`.

## The server

`codegraph-telemetry-server` (in `crates/codegraph-telemetry`) is the whole
receiving side: it validates every event and property against the allowlist
(anything else is dropped), rate-limits per machine ID, never reads the peer
address, and makes no outbound connections. It stores into a local SQLite
database whose schema
([`schema.sql`](crates/codegraph-telemetry/src/schema.sql)) is the complete
list of what a row can hold. Raw events are deleted after the retention window
(90 days by default); anonymous daily totals and which days each machine ID was
active outlive them.

```bash
cargo build --release -p codegraph-telemetry
CODEGRAPH_DASHBOARD_TOKEN=$(openssl rand -hex 16) \
  ./target/release/codegraph-telemetry-server --listen 127.0.0.1:8787 --db telemetry.db
```

Put it behind a TLS-terminating reverse proxy, pass `--secure-cookies`, and
point clients at `https://your-host/v1/events`. The dashboard at `/dashboard`
(active machines, calls per day, tool/agent/language/platform breakdowns,
weekly retention) is enabled by `CODEGRAPH_DASHBOARD_TOKEN`.
