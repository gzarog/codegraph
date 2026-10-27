//! The telemetry wire format and THE allowlist. The client builds payloads
//! from these types and the server sanitizes every incoming batch against
//! the same table, so what can be collected is defined in exactly one place
//! (and documented field by field in TELEMETRY.md).

use serde_json::{Map, Value};

/// Bumped whenever the collected fields change.
pub const SCHEMA_VERSION: u64 = 1;
pub const MAX_BODY_BYTES: usize = 64 * 1024;
pub const MAX_EVENTS_PER_BATCH: usize = 100;

pub const FILE_COUNT_BUCKETS: &[&str] = &["<100", "100-1k", "1k-10k", "10k+"];
pub const DURATION_BUCKETS: &[&str] = &["<10s", "10-60s", "1-5m", "5m+"];

pub fn file_count_bucket(n: usize) -> &'static str {
    match n {
        0..100 => "<100",
        100..1000 => "100-1k",
        1000..10000 => "1k-10k",
        _ => "10k+",
    }
}

pub fn duration_bucket(ms: u128) -> &'static str {
    match ms {
        0..10_000 => "<10s",
        10_000..60_000 => "10-60s",
        60_000..300_000 => "1-5m",
        _ => "5m+",
    }
}

/// Bare identifiers: tool/command/target/language names, versions.
fn is_token(s: &str) -> bool {
    s.bytes()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b':' | b'+' | b'-'))
}

/// Human-ish labels: MCP client names like "Claude Code", "cursor-vscode/1.2".
fn is_label(s: &str) -> bool {
    s.bytes().all(|c| {
        c.is_ascii_alphanumeric()
            || matches!(
                c,
                b'_' | b'.' | b':' | b'+' | b'/' | b' ' | b'@' | b'(' | b')' | b'-'
            )
    })
}

/// A sanitizer returns the value to keep, or None to strip the property.
#[derive(Clone, Copy)]
pub enum Rule {
    OneOf(&'static [&'static str]),
    Token(usize),
    Label(usize),
    TokenArray { items: usize, len: usize },
    NonNegInt(u64),
    Bool,
}

impl Rule {
    pub fn apply(self, v: &Value) -> Option<Value> {
        let text = |max: usize, ok: fn(&str) -> bool| -> Option<Value> {
            let s = v.as_str()?;
            (!s.is_empty() && s.len() <= max && ok(s)).then(|| v.clone())
        };
        match self {
            Rule::OneOf(allowed) => {
                let s = v.as_str()?;
                allowed.contains(&s).then(|| v.clone())
            }
            Rule::Token(max) => text(max, is_token),
            Rule::Label(max) => text(max, is_label),
            Rule::TokenArray { items, len } => {
                let a = v.as_array()?;
                (a.len() <= items
                    && a.iter().all(|x| {
                        x.as_str()
                            .is_some_and(|s| !s.is_empty() && s.len() <= len && is_token(s))
                    }))
                .then(|| v.clone())
            }
            Rule::NonNegInt(max) => {
                let n = v.as_u64()?;
                (n <= max).then(|| v.clone())
            }
            Rule::Bool => v.is_boolean().then(|| v.clone()),
        }
    }
}

pub struct EventSpec {
    pub name: &'static str,
    pub required: &'static [&'static str],
    pub props: &'static [(&'static str, Rule)],
}

pub const EVENTS: &[EventSpec] = &[
    EventSpec {
        name: "install",
        required: &["scope", "kind"],
        props: &[
            ("targets", Rule::TokenArray { items: 12, len: 24 }),
            ("scope", Rule::OneOf(&["local", "global"])),
            ("kind", Rule::OneOf(&["fresh", "upgrade", "reinstall"])),
        ],
    },
    EventSpec {
        name: "index",
        required: &[],
        props: &[
            ("languages", Rule::TokenArray { items: 32, len: 24 }),
            ("file_count_bucket", Rule::OneOf(FILE_COUNT_BUCKETS)),
            ("duration_bucket", Rule::OneOf(DURATION_BUCKETS)),
        ],
    },
    EventSpec {
        name: "usage_rollup",
        required: &["kind", "name", "count"],
        props: &[
            ("kind", Rule::OneOf(&["mcp_tool", "cli_command"])),
            ("name", Rule::Token(64)),
            ("count", Rule::NonNegInt(1_000_000)),
            ("error_count", Rule::NonNegInt(1_000_000)),
            ("client_name", Rule::Label(64)),
            ("client_version", Rule::Label(32)),
        ],
    },
    EventSpec {
        name: "uninstall",
        required: &[],
        props: &[("targets", Rule::TokenArray { items: 12, len: 24 })],
    },
];

/// Envelope fields shared by every event in a batch.
pub const ENVELOPE: &[(&str, Rule)] = &[
    ("codegraph_version", Rule::Token(32)),
    ("os", Rule::Token(16)),
    ("arch", Rule::Token(16)),
    ("ci", Rule::Bool),
    ("schema_version", Rule::NonNegInt(99)),
];

/// One sanitized event.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    pub event: String,
    /// ISO 8601 UTC, as sent (validated by the server against its clock).
    pub ts: Option<String>,
    pub props: Map<String, Value>,
}

/// Keep only allowlisted props; None when the event is unknown or a
/// required prop didn't survive.
pub fn sanitize_event(raw: &Value) -> Option<Event> {
    let name = raw.get("event")?.as_str()?;
    let spec = EVENTS.iter().find(|s| s.name == name)?;
    let empty = Map::new();
    let raw_props = raw
        .get("props")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let mut props = Map::new();
    for (key, rule) in spec.props {
        if let Some(v) = raw_props.get(*key).and_then(|v| rule.apply(v)) {
            props.insert((*key).to_string(), v);
        }
    }
    if spec.required.iter().any(|r| !props.contains_key(*r)) {
        return None;
    }
    Some(Event {
        event: name.to_string(),
        ts: raw.get("ts").and_then(Value::as_str).map(str::to_string),
        props,
    })
}

pub fn sanitize_envelope(body: &Value) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, rule) in ENVELOPE {
        if let Some(v) = body.get(*key).and_then(|v| rule.apply(v)) {
            out.insert((*key).to_string(), v);
        }
    }
    out
}

/// A random-UUID-shaped machine id (`8-4-4-4-12` hex).
pub fn is_uuid(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && parts
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.bytes().all(|c| c.is_ascii_hexdigit()))
}

// ------------------------------------------------------------------- dates

/// `YYYY-MM-DD` for days since the Unix epoch (proleptic Gregorian, UTC).
pub fn day_from_epoch_days(days: i64) -> String {
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Days since the epoch for `YYYY-MM-DD`.
pub fn epoch_days(day: &str) -> Option<i64> {
    let mut it = day.splitn(3, '-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.get(..2)?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

pub fn utc_day(ms: u64) -> String {
    day_from_epoch_days((ms / 86_400_000) as i64)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for epoch milliseconds.
pub fn iso_timestamp(ms: u64) -> String {
    let secs = ms / 1000;
    let day = utc_day(ms);
    let s = secs % 86_400;
    format!("{day}T{:02}:{:02}:{:02}Z", s / 3600, s / 60 % 60, s % 60)
}

/// Epoch ms for an ISO 8601 UTC timestamp (`…Z`, fractional seconds ok).
pub fn parse_iso(ts: &str) -> Option<u64> {
    let (date, time) = ts.split_once('T')?;
    let days = epoch_days(date)?;
    let time = time.strip_suffix('Z')?;
    let mut it = time.split(':');
    let h: u64 = it.next()?.parse().ok()?;
    let m: u64 = it.next()?.parse().ok()?;
    let s: f64 = it.next()?.parse().ok()?;
    if h > 23 || m > 59 || !(0.0..61.0).contains(&s) || days < 0 {
        return None;
    }
    Some(days as u64 * 86_400_000 + (h * 3600 + m * 60) * 1000 + (s * 1000.0) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn allowlist_strips_and_requires() {
        let e = sanitize_event(&json!({
            "event": "usage_rollup",
            "ts": "2026-01-02T12:00:00Z",
            "props": { "kind": "mcp_tool", "name": "codegraph_explore", "count": 3,
                       "path": "/home/me/secret", "client_name": "Claude Code" }
        }))
        .unwrap();
        assert!(!e.props.contains_key("path"));
        assert_eq!(e.props["client_name"], "Claude Code");
        assert!(sanitize_event(
            &json!({ "event": "usage_rollup", "props": { "kind": "mcp_tool" } })
        )
        .is_none());
        assert!(sanitize_event(&json!({ "event": "exfiltrate", "props": {} })).is_none());
        assert!(sanitize_event(&json!({
            "event": "index",
            "props": { "languages": ["rust", "../etc/passwd"] }
        }))
        .unwrap()
        .props
        .is_empty());
    }

    #[test]
    fn dates_round_trip() {
        assert_eq!(day_from_epoch_days(0), "1970-01-01");
        assert_eq!(day_from_epoch_days(20_454), "2026-01-01");
        assert_eq!(epoch_days("2026-01-01"), Some(20_454));
        assert_eq!(
            epoch_days("2024-02-29").map(day_from_epoch_days).as_deref(),
            Some("2024-02-29")
        );
        let ms = parse_iso("2026-03-04T05:06:07.5Z").unwrap();
        assert_eq!(iso_timestamp(ms), "2026-03-04T05:06:07Z");
        assert!(is_uuid("3f2b8c1e-0a4d-4c5e-9f7a-1b2c3d4e5f60"));
        assert!(!is_uuid("not-a-uuid"));
    }
}
