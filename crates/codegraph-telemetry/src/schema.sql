-- The complete list of what the telemetry server keeps. Nothing else is
-- stored: no IP addresses, no request logs, no payload bodies.

-- Lifecycle events (install / index / uninstall), one row each. Deleted after
-- the retention window; the daily_* rollups below outlive them.
CREATE TABLE IF NOT EXISTS events (
  id                INTEGER PRIMARY KEY,
  received_at       TEXT    NOT NULL,          -- ISO 8601 UTC, server clock
  ts                TEXT,                      -- client timestamp, clamped (>10min future / >30d past dropped)
  day               TEXT    NOT NULL,          -- UTC YYYY-MM-DD from ts, else received_at
  event             TEXT    NOT NULL,          -- install | index | uninstall
  machine_id        TEXT    NOT NULL,          -- random UUID minted by the client
  codegraph_version TEXT,
  os                TEXT,                      -- linux | macos | windows | …
  arch              TEXT,                      -- x86_64 | aarch64 | …
  ci                INTEGER,                   -- 0/1; NULL when absent
  schema_version    INTEGER,
  props             TEXT    NOT NULL DEFAULT '{}'  -- sanitized event-specific props (JSON)
);
CREATE INDEX IF NOT EXISTS events_day_event ON events (day, event);

-- Usage counters, added into one row per machine × day × tool.
CREATE TABLE IF NOT EXISTS usage_daily (
  day               TEXT    NOT NULL,
  machine_id        TEXT    NOT NULL,
  kind              TEXT    NOT NULL,          -- mcp_tool | cli_command
  name              TEXT    NOT NULL,          -- tool or command name
  client_name       TEXT    NOT NULL DEFAULT '',  -- MCP clientInfo name, e.g. "Claude Code"
  client_version    TEXT    NOT NULL DEFAULT '',
  codegraph_version TEXT    NOT NULL DEFAULT '',
  os                TEXT    NOT NULL DEFAULT '',
  arch              TEXT    NOT NULL DEFAULT '',
  count             INTEGER NOT NULL DEFAULT 0,
  error_count       INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (day, machine_id, kind, name, client_name, client_version, codegraph_version, os, arch)
) WITHOUT ROWID;

-- Which days each machine was active (never purged: retention survives the event purge).
CREATE TABLE IF NOT EXISTS machine_days (
  machine_id TEXT    NOT NULL,
  day        TEXT    NOT NULL,
  prod       INTEGER NOT NULL DEFAULT 1,       -- 0 only if every event that day came from CI
  PRIMARY KEY (machine_id, day)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS machine_days_day ON machine_days (day);

-- Each machine's first active day and first index day (activation funnel).
CREATE TABLE IF NOT EXISTS machine_first_seen (
  machine_id      TEXT PRIMARY KEY,
  first_day       TEXT NOT NULL,
  first_index_day TEXT
);
CREATE INDEX IF NOT EXISTS machine_first_seen_day ON machine_first_seen (first_day);

-- Anonymous daily rollups.
CREATE TABLE IF NOT EXISTS daily_machines (
  day           TEXT    PRIMARY KEY,
  machines      INTEGER NOT NULL DEFAULT 0,
  prod_machines INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS daily_event_counts (
  day      TEXT    NOT NULL,
  event    TEXT    NOT NULL,
  count    INTEGER NOT NULL DEFAULT 0,
  machines INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (day, event)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS daily_dim_counts (
  day      TEXT    NOT NULL,
  event    TEXT    NOT NULL,
  dim      TEXT    NOT NULL,                   -- os | arch | codegraph_version | language | target | name | …
  value    TEXT    NOT NULL,
  count    INTEGER NOT NULL DEFAULT 0,
  machines INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (day, event, dim, value)
) WITHOUT ROWID;
CREATE INDEX IF NOT EXISTS daily_dim_counts_dim_day ON daily_dim_counts (dim, day);
