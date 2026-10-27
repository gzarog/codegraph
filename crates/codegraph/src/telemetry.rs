//! Anonymous usage telemetry, client side. What is collected is the
//! allowlist in `codegraph_telemetry::protocol` and nothing else (see
//! TELEMETRY.md).
//!
//! - Nothing is sent unless an endpoint is configured: the
//!   `CODEGRAPH_TELEMETRY_ENDPOINT` environment variable, or the same
//!   variable set when the release binary was built.
//! - Off is off: `codegraph telemetry off`, `CODEGRAPH_TELEMETRY=0` or
//!   `DO_NOT_TRACK=1` record nothing and open no connection.
//! - Usage is counted in memory, added into daily totals in a bounded local
//!   buffer, and only completed days are sent, fire-and-forget through the
//!   system `curl`. Nothing ever blocks a command or an MCP call.

use crate::global;
use codegraph_telemetry::protocol::{self, SCHEMA_VERSION};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

pub const DOCS: &str = "https://github.com/gzarog/codegraph/blob/main/TELEMETRY.md";
const MAX_BUFFER_BYTES: u64 = 256 * 1024;
const MAX_EVENTS_PER_REQUEST: usize = 100;
/// A crashed sender's claimed buffer is merged back after this long.
const STALE_CLAIM_MS: u64 = 60 * 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageKind {
    McpTool,
    CliCommand,
}

impl UsageKind {
    fn as_str(self) -> &'static str {
        match self {
            UsageKind::McpTool => "mcp_tool",
            UsageKind::CliCommand => "cli_command",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigFile {
    enabled: bool,
    machine_id: Option<String>,
    /// installer | default-notice | cli
    consent_source: String,
    #[serde(default)]
    first_run_notice_shown: bool,
    updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecidedBy {
    DoNotTrack,
    Env,
    Config,
    Default,
}

impl DecidedBy {
    pub fn describe(self) -> &'static str {
        match self {
            DecidedBy::DoNotTrack => "DO_NOT_TRACK is set",
            DecidedBy::Env => "CODEGRAPH_TELEMETRY is set",
            DecidedBy::Config => "your stored choice",
            DecidedBy::Default => "the default",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub enabled: bool,
    pub decided_by: DecidedBy,
    pub machine_id: Option<String>,
    pub config_path: PathBuf,
    pub endpoint: Option<String>,
}

/// One buffered line: a usage-count delta or a lifecycle event.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
enum Line {
    Count {
        v: u64,
        d: String,
        k: UsageKind,
        n: String,
        c: u64,
        e: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cn: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cv: Option<String>,
    },
    Event {
        v: u64,
        ev: String,
        ts: String,
        props: Map<String, Value>,
    },
}

type CountKey = (String, UsageKind, String, Option<String>, Option<String>);

fn config_path(dir: &Path) -> PathBuf {
    dir.join("telemetry.json")
}

fn buffer_path(dir: &Path) -> PathBuf {
    dir.join("telemetry-buffer.jsonl")
}

fn read_config(dir: &Path) -> Option<ConfigFile> {
    serde_json::from_str(&std::fs::read_to_string(config_path(dir)).ok()?).ok()
}

fn write_config(dir: &Path, cfg: &ConfigFile) {
    if let Ok(json) = serde_json::to_vec_pretty(cfg) {
        let _ = global::write_atomic(&config_path(dir), &json);
    }
}

/// A random UUIDv4, derived from nothing about the machine.
fn new_machine_id() -> String {
    let mut bytes: [u8; 16] = global::random_bytes();
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = global::hex(&bytes);
    format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    )
}

/// The configured ingest URL, if any.
pub fn endpoint() -> Option<String> {
    let url = std::env::var("CODEGRAPH_TELEMETRY_ENDPOINT")
        .ok()
        .or_else(|| option_env!("CODEGRAPH_TELEMETRY_ENDPOINT").map(str::to_string))?;
    let ok = url.starts_with("https://")
        || url.starts_with("http://127.0.0.1")
        || url.starts_with("http://localhost");
    ok.then_some(url)
}

pub fn status_in(dir: &Path) -> Status {
    let cfg = read_config(dir);
    let (enabled, decided_by) = if global::env_truthy("DO_NOT_TRACK") {
        (false, DecidedBy::DoNotTrack)
    } else if global::env_falsy("CODEGRAPH_TELEMETRY") {
        (false, DecidedBy::Env)
    } else if global::env_truthy("CODEGRAPH_TELEMETRY") {
        (true, DecidedBy::Env)
    } else if let Some(c) = &cfg {
        (c.enabled, DecidedBy::Config)
    } else {
        (true, DecidedBy::Default)
    };
    Status {
        enabled,
        decided_by,
        machine_id: cfg.and_then(|c| c.machine_id),
        config_path: config_path(dir),
        endpoint: endpoint(),
    }
}

pub fn status() -> Status {
    status_in(&global::dir())
}

/// Store a choice. Turning off deletes the identity and anything unsent;
/// turning on mints a fresh identity.
pub fn set_enabled_in(dir: &Path, on: bool, source: &str) -> Status {
    let cfg = ConfigFile {
        enabled: on,
        machine_id: on.then(new_machine_id),
        consent_source: source.to_string(),
        first_run_notice_shown: true,
        updated_at: protocol::iso_timestamp(global::now_ms()),
    };
    write_config(dir, &cfg);
    if !on {
        let _ = std::fs::remove_file(buffer_path(dir));
        for claim in claims(dir) {
            let _ = std::fs::remove_file(claim.0);
        }
        if let Some(t) = TELEMETRY.get() {
            t.clear();
        }
    }
    status_in(dir)
}

pub fn set_enabled(on: bool, source: &str) -> Status {
    set_enabled_in(&global::dir(), on, source)
}

// ------------------------------------------------------------- recording

struct Recorder {
    dir: PathBuf,
    counts: Mutex<HashMap<CountKey, (u64, u64)>>,
    events: Mutex<Vec<Line>>,
}

static TELEMETRY: OnceLock<Recorder> = OnceLock::new();

fn recorder() -> &'static Recorder {
    TELEMETRY.get_or_init(|| Recorder {
        dir: global::dir(),
        counts: Mutex::new(HashMap::new()),
        events: Mutex::new(Vec::new()),
    })
}

impl Recorder {
    /// Recording only happens when there is somewhere to send it.
    fn enabled(&self) -> bool {
        let s = status_in(&self.dir);
        s.enabled && s.endpoint.is_some()
    }

    fn clear(&self) {
        if let Ok(mut c) = self.counts.lock() {
            c.clear();
        }
        if let Ok(mut e) = self.events.lock() {
            e.clear();
        }
    }

    fn drain(&self) -> Vec<Line> {
        let mut out = Vec::new();
        if let Ok(mut c) = self.counts.lock() {
            for ((d, k, n, cn, cv), (calls, errors)) in c.drain() {
                out.push(Line::Count {
                    v: SCHEMA_VERSION,
                    d,
                    k,
                    n,
                    c: calls,
                    e: errors,
                    cn,
                    cv,
                });
            }
        }
        if let Ok(mut e) = self.events.lock() {
            out.append(&mut e);
        }
        out
    }
}

/// Count one tool call or command run.
pub fn record_usage(kind: UsageKind, name: &str, error: bool, client: Option<(&str, &str)>) {
    let r = recorder();
    if !r.enabled() {
        return;
    }
    let day = protocol::utc_day(global::now_ms());
    let (cn, cv) = match client {
        Some((n, v)) => (
            (!n.is_empty()).then(|| n.chars().take(64).collect()),
            (!v.is_empty()).then(|| v.chars().take(32).collect()),
        ),
        None => (None, None),
    };
    if let Ok(mut c) = r.counts.lock() {
        let entry = c.entry((day, kind, name.to_string(), cn, cv)).or_default();
        entry.0 += 1;
        entry.1 += u64::from(error);
    }
}

/// Record an install / index / uninstall event.
pub fn record_event(event: &str, props: Value) {
    let r = recorder();
    if !r.enabled() {
        return;
    }
    let props = props.as_object().cloned().unwrap_or_default();
    if let Ok(mut e) = r.events.lock() {
        e.push(Line::Event {
            v: SCHEMA_VERSION,
            ev: event.to_string(),
            ts: protocol::iso_timestamp(global::now_ms()),
            props,
        });
    }
}

/// An `index` event: language names and coarse size/duration buckets only.
pub fn record_index(languages: &[(String, u64)], files: usize, duration_ms: u128) {
    let languages: Vec<&str> = languages
        .iter()
        .filter(|(_, n)| *n > 0)
        .map(|(l, _)| l.as_str())
        .collect();
    record_event(
        "index",
        json!({
            "languages": languages,
            "file_count_bucket": protocol::file_count_bucket(files),
            "duration_bucket": protocol::duration_bucket(duration_ms),
        }),
    );
}

fn coalesce(lines: Vec<Line>) -> Vec<Line> {
    let mut counts: HashMap<CountKey, usize> = HashMap::new();
    let mut out: Vec<Line> = Vec::new();
    for line in lines {
        match line {
            Line::Count {
                v,
                d,
                k,
                n,
                c,
                e,
                cn,
                cv,
            } => {
                let key = (d.clone(), k, n.clone(), cn.clone(), cv.clone());
                if let Some(&i) = counts.get(&key) {
                    if let Line::Count { c: c0, e: e0, .. } = &mut out[i] {
                        *c0 += c;
                        *e0 += e;
                    }
                } else {
                    counts.insert(key, out.len());
                    out.push(Line::Count {
                        v,
                        d,
                        k,
                        n,
                        c,
                        e,
                        cn,
                        cv,
                    });
                }
            }
            ev => out.push(ev),
        }
    }
    out
}

fn parse_lines(text: &str) -> Vec<Line> {
    text.lines()
        .filter_map(|l| serde_json::from_str::<Line>(l).ok())
        .filter(|l| match l {
            Line::Count { v, .. } | Line::Event { v, .. } => *v == SCHEMA_VERSION,
        })
        .collect()
}

fn append_lines(dir: &Path, lines: &[Line]) {
    if lines.is_empty() {
        return;
    }
    let path = buffer_path(dir);
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    let mut text = String::new();
    for l in lines {
        if let Ok(s) = serde_json::to_string(l) {
            text.push_str(&s);
            text.push('\n');
        }
    }
    // Bounded: an offline machine drops new data rather than growing forever.
    if size + text.len() as u64 > MAX_BUFFER_BYTES {
        return;
    }
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(text.as_bytes());
    }
}

/// Move what was recorded in memory into the local buffer.
pub fn persist() {
    let r = recorder();
    let lines = r.drain();
    if lines.is_empty() || !r.enabled() {
        return;
    }
    append_lines(&r.dir, &coalesce(lines));
}

fn claims(dir: &Path) -> Vec<(PathBuf, u64)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let at = name
                .strip_prefix("telemetry-buffer.claim.")?
                .split('.')
                .nth(1)?
                .parse()
                .ok()?;
            Some((e.path(), at))
        })
        .collect()
}

fn envelope(machine_id: &str, events: Vec<Value>) -> Value {
    json!({
        "machine_id": machine_id,
        "codegraph_version": env!("CARGO_PKG_VERSION"),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "ci": std::env::var_os("CI").is_some(),
        "schema_version": SCHEMA_VERSION,
        "events": events,
    })
}

/// The payload events for buffered lines: lifecycle events, plus usage for
/// completed days. Today's counts are returned to be kept for later.
fn split_sendable(lines: Vec<Line>, today: &str) -> (Vec<Value>, Vec<Line>) {
    let mut send = Vec::new();
    let mut keep = Vec::new();
    for l in coalesce(lines) {
        match &l {
            Line::Count { d, .. } if d.as_str() >= today => keep.push(l),
            Line::Count {
                d,
                k,
                n,
                c,
                e,
                cn,
                cv,
                ..
            } => {
                let mut props =
                    json!({ "kind": k.as_str(), "name": n, "count": c, "error_count": e });
                if let Some(cn) = cn {
                    props["client_name"] = cn.clone().into();
                }
                if let Some(cv) = cv {
                    props["client_version"] = cv.clone().into();
                }
                send.push(json!({ "event": "usage_rollup", "ts": format!("{d}T12:00:00Z"), "props": props }));
            }
            Line::Event { ev, ts, props, .. } => {
                send.push(json!({ "event": ev, "ts": ts, "props": props }));
            }
        }
    }
    (send, keep)
}

/// Send what is ready, without waiting: claim the buffer, hand completed
/// days to `curl` in the background and keep today's counts.
pub fn flush() {
    let r = recorder();
    persist();
    let status = status_in(&r.dir);
    let (true, Some(url)) = (status.enabled, status.endpoint.clone()) else {
        return;
    };
    let dir = &r.dir;
    let path = buffer_path(dir);
    let now = global::now_ms();
    // Merge back claims a crashed sender left behind.
    for (claim, at) in claims(dir) {
        if now.saturating_sub(at) > STALE_CLAIM_MS {
            if let Ok(text) = std::fs::read_to_string(&claim) {
                let _ = std::fs::remove_file(&claim);
                append_lines(dir, &parse_lines(&text));
            }
        }
    }
    if !path.exists() {
        return;
    }
    let claim = dir.join(format!(
        "telemetry-buffer.claim.{}.{now}",
        std::process::id()
    ));
    if std::fs::rename(&path, &claim).is_err() {
        return; // another process is sending
    }
    let text = std::fs::read_to_string(&claim).unwrap_or_default();
    let _ = std::fs::remove_file(&claim);
    let (send, keep) = split_sendable(parse_lines(&text), &protocol::utc_day(now));
    append_lines(dir, &keep);
    if send.is_empty() {
        return;
    }
    // Recheck: the choice may have changed while we worked.
    let status = status_in(dir);
    if !status.enabled {
        return;
    }
    let machine_id = match status.machine_id {
        Some(id) => id,
        None => mint_identity(dir, status.decided_by),
    };
    for chunk in send.chunks(MAX_EVENTS_PER_REQUEST) {
        let body = envelope(&machine_id, chunk.to_vec()).to_string();
        post(&url, body.as_bytes());
    }
}

/// First send: create the identity, and say once that telemetry is on.
fn mint_identity(dir: &Path, decided_by: DecidedBy) -> String {
    let mut cfg = read_config(dir).unwrap_or(ConfigFile {
        enabled: true,
        machine_id: None,
        consent_source: "default-notice".into(),
        first_run_notice_shown: false,
        updated_at: protocol::iso_timestamp(global::now_ms()),
    });
    if !cfg.first_run_notice_shown && decided_by == DecidedBy::Default {
        eprintln!(
            "CodeGraph sends anonymous usage counts (no code, paths or names). Turn off: `codegraph telemetry off`. Details: {DOCS}"
        );
        cfg.first_run_notice_shown = true;
    }
    let id = cfg.machine_id.clone().unwrap_or_else(new_machine_id);
    cfg.machine_id = Some(id.clone());
    write_config(dir, &cfg);
    id
}

/// Fire-and-forget POST through curl: no waiting, no retries, no output.
fn post(url: &str, body: &[u8]) {
    let child = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" })
        .args([
            "-fsS",
            "-m",
            "5",
            "-o",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
            "-X",
            "POST",
            "-H",
            "content-type: application/json",
            "--data-binary",
            "@-",
            url,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = child {
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(body);
        }
        // Reap it in the background so it never lingers as a zombie in
        // long-lived processes; short-lived ones simply exit first.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_out_removes_identity_and_buffer() {
        let dir = tempfile::tempdir().unwrap();
        let on = set_enabled_in(dir.path(), true, "cli");
        assert!(on.machine_id.as_deref().is_some_and(protocol::is_uuid));
        append_lines(
            dir.path(),
            &[Line::Event {
                v: SCHEMA_VERSION,
                ev: "index".into(),
                ts: "2026-01-01T00:00:00Z".into(),
                props: Map::new(),
            }],
        );
        assert!(buffer_path(dir.path()).exists());
        let off = set_enabled_in(dir.path(), false, "cli");
        assert!(!buffer_path(dir.path()).exists());
        assert!(off.machine_id.is_none());
        if !global::env_truthy("DO_NOT_TRACK") && std::env::var_os("CODEGRAPH_TELEMETRY").is_none()
        {
            assert!(!off.enabled);
            assert_eq!(off.decided_by, DecidedBy::Config);
        }
    }

    #[test]
    fn only_completed_days_are_sent() {
        let line = |d: &str, n: &str, c: u64| Line::Count {
            v: SCHEMA_VERSION,
            d: d.into(),
            k: UsageKind::McpTool,
            n: n.into(),
            c,
            e: 0,
            cn: Some("Claude Code".into()),
            cv: None,
        };
        let lines = vec![
            line("2026-01-01", "codegraph_explore", 2),
            line("2026-01-01", "codegraph_explore", 3),
            line("2026-01-02", "codegraph_explore", 1),
        ];
        let (send, keep) = split_sendable(lines, "2026-01-02");
        assert_eq!(send.len(), 1);
        assert_eq!(send[0]["props"]["count"], 5);
        assert_eq!(send[0]["ts"], "2026-01-01T12:00:00Z");
        assert_eq!(keep.len(), 1);
        // What we send passes the server's allowlist unchanged.
        let e = protocol::sanitize_event(&send[0]).unwrap();
        assert_eq!(e.props["client_name"], "Claude Code");
        let body = envelope("3f2b8c1e-0a4d-4c5e-9f7a-1b2c3d4e5f60", send);
        assert_eq!(protocol::sanitize_envelope(&body).len(), 5);
    }

    #[test]
    fn buffer_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let l = Line::Count {
            v: SCHEMA_VERSION,
            d: "2026-01-01".into(),
            k: UsageKind::CliCommand,
            n: "init".into(),
            c: 1,
            e: 1,
            cn: None,
            cv: None,
        };
        append_lines(dir.path(), std::slice::from_ref(&l));
        let text = std::fs::read_to_string(buffer_path(dir.path())).unwrap();
        assert_eq!(parse_lines(&text), vec![l]);
    }
}
