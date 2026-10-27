//! Telemetry storage: ingest writes, the daily rollups and retention purge,
//! and the read queries behind the dashboard.

use crate::protocol::{self, Event};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::path::Path;

pub const DEFAULT_RETENTION_DAYS: i64 = 90;
/// The rollup re-rolls this many completed days on every run (late uploads).
pub const ROLLUP_LOOKBACK_DAYS: i64 = 3;
const DAY_MS: u64 = 86_400_000;

/// day, kind, name, client name, client version.
type UsageKey = (String, String, String, String, String);

pub struct Db {
    conn: Connection,
}

const INSERT_EVENT: &str = "INSERT INTO events (received_at, ts, day, event, machine_id,
    codegraph_version, os, arch, ci, schema_version, props)
  VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";

const UPSERT_MACHINE_DAY: &str = "INSERT INTO machine_days (machine_id, day, prod) VALUES (?, ?, ?)
  ON CONFLICT (machine_id, day) DO UPDATE SET prod = max(machine_days.prod, excluded.prod)";

// A late upload moves a machine's first day (and first index day) earlier, never later.
const UPSERT_FIRST_SEEN: &str =
    "INSERT INTO machine_first_seen (machine_id, first_day, first_index_day)
  VALUES (?, ?, ?)
  ON CONFLICT (machine_id) DO UPDATE SET
    first_day = min(machine_first_seen.first_day, excluded.first_day),
    first_index_day = coalesce(min(machine_first_seen.first_index_day, excluded.first_index_day),
                               machine_first_seen.first_index_day, excluded.first_index_day)";

const UPSERT_USAGE: &str = "INSERT INTO usage_daily (day, machine_id, kind, name, client_name,
    client_version, codegraph_version, os, arch, count, error_count)
  VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
  ON CONFLICT (day, machine_id, kind, name, client_name, client_version, codegraph_version, os, arch)
  DO UPDATE SET count = usage_daily.count + excluded.count,
                error_count = usage_daily.error_count + excluded.error_count";

/// A client timestamp within (now − 30 days, now + 10 minutes], else None.
pub fn clamp_ts(ts: Option<&str>, now_ms: u64) -> Option<String> {
    let t = protocol::parse_iso(ts?)?;
    if t > now_ms + 10 * 60_000 || t + 30 * DAY_MS < now_ms {
        return None;
    }
    Some(protocol::iso_timestamp(t))
}

fn text(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str).map(str::to_string)
}

impl Db {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "busy_timeout", 5000)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(include_str!("schema.sql"))?;
        Ok(Db { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Store one sanitized batch: lifecycle events as rows, usage counters
    /// added into `usage_daily`, plus the machine-day bookkeeping.
    pub fn ingest(
        &mut self,
        machine_id: &str,
        envelope: &Map<String, Value>,
        batch: &[Event],
        now_ms: u64,
    ) -> rusqlite::Result<()> {
        let received_at = protocol::iso_timestamp(now_ms);
        let today = protocol::utc_day(now_ms);
        let version = text(envelope.get("codegraph_version"));
        let os = text(envelope.get("os"));
        let arch = text(envelope.get("arch"));
        let ci = envelope.get("ci").and_then(Value::as_bool);
        let schema = envelope.get("schema_version").and_then(Value::as_u64);
        let tx = self.conn.transaction()?;
        let mut days: BTreeSet<String> = BTreeSet::new();
        let mut first_index_day: Option<String> = None;
        {
            // Fold this batch's usage counters per day × tool × client first.
            let mut usage: HashMap<UsageKey, (u64, u64)> = HashMap::new();
            let mut insert = tx.prepare_cached(INSERT_EVENT)?;
            for e in batch {
                let ts = clamp_ts(e.ts.as_deref(), now_ms);
                let day = ts
                    .as_deref()
                    .map_or_else(|| today.clone(), |t| t[..10].to_string());
                days.insert(day.clone());
                if e.event == "usage_rollup" {
                    let s = |k: &str| text(e.props.get(k)).unwrap_or_default();
                    let n = |k: &str| e.props.get(k).and_then(Value::as_u64).unwrap_or(0);
                    let entry = usage
                        .entry((
                            day,
                            s("kind"),
                            s("name"),
                            s("client_name"),
                            s("client_version"),
                        ))
                        .or_default();
                    entry.0 += n("count");
                    entry.1 += n("error_count");
                    continue;
                }
                if e.event == "index" && first_index_day.as_ref().is_none_or(|d| day < *d) {
                    first_index_day = Some(day.clone());
                }
                insert.execute(params![
                    received_at,
                    ts,
                    day,
                    e.event,
                    machine_id,
                    version,
                    os,
                    arch,
                    ci.map(i64::from),
                    schema.map(|v| v as i64),
                    Value::Object(e.props.clone()).to_string(),
                ])?;
            }
            let mut upsert = tx.prepare_cached(UPSERT_USAGE)?;
            for ((day, kind, name, cname, cver), (count, errors)) in usage {
                upsert.execute(params![
                    day,
                    machine_id,
                    kind,
                    name,
                    cname,
                    cver,
                    version.clone().unwrap_or_default(),
                    os.clone().unwrap_or_default(),
                    arch.clone().unwrap_or_default(),
                    count as i64,
                    errors as i64,
                ])?;
            }
            let prod = i64::from(ci != Some(true));
            let mut day_stmt = tx.prepare_cached(UPSERT_MACHINE_DAY)?;
            for day in &days {
                day_stmt.execute(params![machine_id, day, prod])?;
            }
            if let Some(first) = days.iter().next() {
                tx.execute(
                    UPSERT_FIRST_SEEN,
                    params![machine_id, first, first_index_day],
                )?;
            }
        }
        tx.commit()
    }

    // ------------------------------------------------------------- rollups

    /// Recompute every daily_* row for `day` from the raw tables.
    pub fn rollup_day(&mut self, day: &str) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM daily_event_counts WHERE day = ?", [day])?;
        tx.execute("DELETE FROM daily_dim_counts WHERE day = ?", [day])?;
        for sql in rollup_statements() {
            tx.execute(&sql, [day])?;
        }
        tx.commit()
    }

    /// Delete raw events and usage rows older than the retention window.
    pub fn purge(&mut self, cutoff_day: &str) -> rusqlite::Result<usize> {
        let mut n = self
            .conn
            .execute("DELETE FROM events WHERE day < ?", [cutoff_day])?;
        n += self
            .conn
            .execute("DELETE FROM usage_daily WHERE day < ?", [cutoff_day])?;
        Ok(n)
    }

    /// Re-roll today and the last few days, then purge expired rows.
    pub fn run_maintenance(&mut self, now_ms: u64, retention_days: i64) -> rusqlite::Result<()> {
        let today = (now_ms / DAY_MS) as i64;
        for d in (today - ROLLUP_LOOKBACK_DAYS)..=today {
            self.rollup_day(&protocol::day_from_epoch_days(d))?;
        }
        // Days with raw data but no rollup yet (the server was down for a while).
        let missed: Vec<String> = {
            let mut stmt = self.conn.prepare(
                "SELECT DISTINCT day FROM machine_days
                  WHERE day NOT IN (SELECT day FROM daily_machines) ORDER BY day LIMIT 31",
            )?;
            let rows = stmt.query_map([], |r| r.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for day in missed {
            self.rollup_day(&day)?;
        }
        self.purge(&protocol::day_from_epoch_days(today - retention_days))?;
        Ok(())
    }

    // ------------------------------------------------------------- queries

    pub fn summary(&self, from: &str, to: &str) -> rusqlite::Result<Value> {
        let c = &self.conn;
        let one = |sql: &str| -> rusqlite::Result<i64> {
            c.query_row(sql, [from, to], |r| r.get::<_, Option<i64>>(0))
                .map(|v| v.unwrap_or(0))
        };
        let mut events = Map::new();
        let mut stmt = c.prepare(
            "SELECT event, sum(count) FROM daily_event_counts
              WHERE day BETWEEN ? AND ? GROUP BY event",
        )?;
        for row in stmt.query_map([from, to], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
        })? {
            let (e, n) = row?;
            events.insert(e, n.into());
        }
        Ok(json!({
            "from": from,
            "to": to,
            "machines": one("SELECT count(DISTINCT machine_id) FROM machine_days WHERE day BETWEEN ? AND ?")?,
            "prodMachines": one("SELECT count(DISTINCT machine_id) FROM machine_days WHERE day BETWEEN ? AND ? AND prod = 1")?,
            "newMachines": one("SELECT count(*) FROM machine_first_seen WHERE first_day BETWEEN ? AND ?")?,
            "activated": one("SELECT count(*) FROM machine_first_seen WHERE first_day BETWEEN ? AND ? AND first_index_day IS NOT NULL AND julianday(first_index_day) - julianday(first_day) <= 7")?,
            "events": events,
        }))
    }

    pub fn timeseries(&self, from: &str, to: &str) -> rusqlite::Result<Value> {
        let mut days: Vec<Value> = Vec::new();
        let (Some(a), Some(b)) = (protocol::epoch_days(from), protocol::epoch_days(to)) else {
            return Ok(json!([]));
        };
        let mut machines: HashMap<String, (i64, i64)> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT day, machines, prod_machines FROM daily_machines WHERE day BETWEEN ? AND ?",
        )?;
        for row in stmt.query_map([from, to], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
            let (d, m, p): (String, i64, i64) = row?;
            machines.insert(d, (m, p));
        }
        let mut counts: HashMap<String, Map<String, Value>> = HashMap::new();
        let mut stmt = self.conn.prepare(
            "SELECT day, event, count FROM daily_event_counts WHERE day BETWEEN ? AND ?",
        )?;
        for row in stmt.query_map([from, to], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
            let (d, e, n): (String, String, i64) = row?;
            counts.entry(d).or_default().insert(e, n.into());
        }
        for d in a..=b {
            let day = protocol::day_from_epoch_days(d);
            let (m, p) = machines.get(&day).copied().unwrap_or((0, 0));
            days.push(json!({
                "day": day,
                "machines": m,
                "prodMachines": p,
                "events": counts.remove(&day).unwrap_or_default(),
            }));
        }
        Ok(Value::Array(days))
    }

    /// Top values of one dimension over the range.
    pub fn breakdown(
        &self,
        dim: &str,
        event: Option<&str>,
        from: &str,
        to: &str,
    ) -> rusqlite::Result<Value> {
        let mut stmt = self.conn.prepare(
            "SELECT value, sum(count), sum(machines) FROM daily_dim_counts
              WHERE dim = ?1 AND (?2 IS NULL OR event = ?2) AND day BETWEEN ?3 AND ?4
              GROUP BY value ORDER BY sum(count) DESC, value LIMIT 25",
        )?;
        let rows = stmt.query_map(params![dim, event, from, to], |r| {
            Ok(json!({ "value": r.get::<_, String>(0)?, "count": r.get::<_, i64>(1)?, "machineDays": r.get::<_, i64>(2)? }))
        })?;
        Ok(Value::Array(rows.collect::<rusqlite::Result<_>>()?))
    }

    /// Weekly cohorts by first-seen week: the share active in each later week.
    pub fn retention(&self, weeks: i64, now_ms: u64) -> rusqlite::Result<Value> {
        let today = (now_ms / DAY_MS) as i64;
        // Weeks start on the epoch-aligned 7-day grid (Thursdays); fine for cohorts.
        let this_week = today - today.rem_euclid(7);
        let mut out = Vec::new();
        for w in (0..weeks).rev() {
            let start = this_week - w * 7;
            let (s, e) = (
                protocol::day_from_epoch_days(start),
                protocol::day_from_epoch_days(start + 6),
            );
            let size: i64 = self.conn.query_row(
                "SELECT count(*) FROM machine_first_seen WHERE first_day BETWEEN ? AND ?",
                [&s, &e],
                |r| r.get(0),
            )?;
            let mut active = Vec::new();
            for k in 1..=w {
                let ks = protocol::day_from_epoch_days(start + k * 7);
                let ke = protocol::day_from_epoch_days(start + k * 7 + 6);
                let n: i64 = self.conn.query_row(
                    "SELECT count(DISTINCT d.machine_id) FROM machine_days d
                       JOIN machine_first_seen f ON f.machine_id = d.machine_id
                      WHERE f.first_day BETWEEN ?1 AND ?2 AND d.day BETWEEN ?3 AND ?4",
                    [&s, &e, &ks, &ke],
                    |r| r.get(0),
                )?;
                active.push(n);
            }
            out.push(json!({ "week": s, "size": size, "active": active }));
        }
        Ok(Value::Array(out))
    }

    /// Day of the most recent stored activity, for the health endpoint.
    pub fn last_day(&self) -> rusqlite::Result<Option<String>> {
        self.conn
            .query_row("SELECT max(day) FROM machine_days", [], |r| r.get(0))
            .optional()
            .map(Option::flatten)
    }
}

fn rollup_statements() -> Vec<String> {
    const CONFLICT: &str = "ON CONFLICT (day, event, dim, value) DO UPDATE
       SET count = excluded.count, machines = excluded.machines";
    let dim = |dim: &str, value: &str, only: &str| {
        format!(
            "INSERT INTO daily_dim_counts (day, event, dim, value, count, machines)
             SELECT e.day, e.event, '{dim}', CAST({value} AS TEXT), count(*), count(DISTINCT e.machine_id)
               FROM events e
              WHERE e.day = ?1 AND {value} IS NOT NULL AND {value} <> ''{only}
              GROUP BY e.day, e.event, {value}
             {CONFLICT}"
        )
    };
    let usage_dim = |dim: &str, column: &str, total: &str, filter: &str| {
        format!(
            "INSERT INTO daily_dim_counts (day, event, dim, value, count, machines)
             SELECT day, 'usage_rollup', '{dim}', {column}, sum({total}), count(DISTINCT machine_id)
               FROM usage_daily
              WHERE day = ?1 AND {column} <> ''{filter}
              GROUP BY day, {column}
             {CONFLICT}"
        )
    };
    let array_dim = |dim: &str, path: &str, events: &str| {
        format!(
            "INSERT INTO daily_dim_counts (day, event, dim, value, count, machines)
             SELECT e.day, e.event, '{dim}', CAST(j.value AS TEXT), count(*), count(DISTINCT e.machine_id)
               FROM events e, json_each(e.props, '{path}') j
              WHERE e.day = ?1 AND e.event IN ({events}) AND j.value <> ''
              GROUP BY e.day, e.event, j.value
             {CONFLICT}"
        )
    };
    let prop = |name: &str| format!("json_extract(e.props, '$.{name}')");
    vec![
        "INSERT INTO daily_machines (day, machines, prod_machines)
           SELECT day, count(*), coalesce(sum(prod), 0) FROM machine_days WHERE day = ?1 GROUP BY day
           ON CONFLICT (day) DO UPDATE
             SET machines = excluded.machines, prod_machines = excluded.prod_machines"
            .into(),
        "UPDATE machine_first_seen SET first_index_day = ?1
          WHERE (first_index_day IS NULL OR first_index_day > ?1)
            AND machine_id IN (SELECT machine_id FROM events WHERE day = ?1 AND event = 'index')"
            .into(),
        "INSERT INTO daily_event_counts (day, event, count, machines)
           SELECT day, event, count(*), count(DISTINCT machine_id) FROM events
            WHERE day = ?1 GROUP BY day, event
           ON CONFLICT (day, event) DO UPDATE SET count = excluded.count, machines = excluded.machines"
            .into(),
        "INSERT INTO daily_event_counts (day, event, count, machines)
           SELECT day, 'usage_rollup', sum(count), count(DISTINCT machine_id) FROM usage_daily
            WHERE day = ?1 GROUP BY day
           ON CONFLICT (day, event) DO UPDATE SET count = excluded.count, machines = excluded.machines"
            .into(),
        dim("os", "e.os", ""),
        dim("arch", "e.arch", ""),
        dim("codegraph_version", "e.codegraph_version", ""),
        usage_dim("os", "os", "count", ""),
        usage_dim("arch", "arch", "count", ""),
        usage_dim("codegraph_version", "codegraph_version", "count", ""),
        dim("file_count_bucket", &prop("file_count_bucket"), " AND e.event = 'index'"),
        dim("duration_bucket", &prop("duration_bucket"), " AND e.event = 'index'"),
        dim("scope", &prop("scope"), " AND e.event = 'install'"),
        dim("kind", &prop("kind"), " AND e.event = 'install'"),
        usage_dim("kind", "kind", "count", ""),
        usage_dim("name", "name", "count", ""),
        usage_dim("client_name", "client_name", "count", ""),
        usage_dim("name_error", "name", "error_count", " AND error_count > 0"),
        array_dim("language", "$.languages", "'index'"),
        array_dim("target", "$.targets", "'install', 'uninstall'"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::sanitize_event;

    const MACHINE: &str = "3f2b8c1e-0a4d-4c5e-9f7a-1b2c3d4e5f60";

    #[test]
    fn ingest_rollup_and_query() {
        let mut db = Db::open_in_memory().unwrap();
        let now = protocol::parse_iso("2026-05-10T15:00:00Z").unwrap();
        let envelope = protocol::sanitize_envelope(&json!({
            "codegraph_version": "0.1.0", "os": "linux", "arch": "x86_64", "ci": false,
            "schema_version": 1, "hostname": "secret-box"
        }));
        assert!(!envelope.contains_key("hostname"));
        let raw = [
            json!({ "event": "index", "props": { "languages": ["rust", "typescript"], "file_count_bucket": "100-1k" } }),
            json!({ "event": "usage_rollup", "ts": "2026-05-09T12:00:00Z", "props": { "kind": "mcp_tool", "name": "codegraph_explore", "count": 4, "client_name": "Claude Code" } }),
            json!({ "event": "usage_rollup", "ts": "2026-05-09T12:00:00Z", "props": { "kind": "mcp_tool", "name": "codegraph_explore", "count": 2, "error_count": 1, "client_name": "Claude Code" } }),
            json!({ "event": "install", "ts": "2020-01-01T00:00:00Z", "props": { "scope": "global", "kind": "fresh", "targets": ["claude"] } }),
        ];
        let batch: Vec<Event> = raw.iter().filter_map(sanitize_event).collect();
        db.ingest(MACHINE, &envelope, &batch, now).unwrap();
        db.run_maintenance(now, DEFAULT_RETENTION_DAYS).unwrap();

        let s = db.summary("2026-05-01", "2026-05-10").unwrap();
        assert_eq!(s["machines"], 1);
        assert_eq!(s["newMachines"], 1);
        assert_eq!(s["activated"], 1);
        assert_eq!(s["events"]["usage_rollup"], 6);
        // The implausible 2020 timestamp was clamped to the receive day.
        assert_eq!(s["events"]["install"], 1);
        let langs = db
            .breakdown("language", None, "2026-05-01", "2026-05-10")
            .unwrap();
        assert_eq!(langs.as_array().unwrap().len(), 2);
        let errors = db
            .breakdown("name_error", None, "2026-05-01", "2026-05-10")
            .unwrap();
        assert_eq!(errors[0]["count"], 1);
        let ts = db.timeseries("2026-05-08", "2026-05-10").unwrap();
        assert_eq!(ts.as_array().unwrap().len(), 3);
        assert_eq!(ts[1]["machines"], 1);

        // Retention: raw rows go, the rollups stay.
        let later = now + 200 * DAY_MS;
        db.run_maintenance(later, DEFAULT_RETENTION_DAYS).unwrap();
        let left: i64 = db
            .conn()
            .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
            .unwrap();
        assert_eq!(left, 0);
        let s = db.summary("2026-05-01", "2026-05-10").unwrap();
        assert_eq!(s["events"]["usage_rollup"], 6);
    }
}
