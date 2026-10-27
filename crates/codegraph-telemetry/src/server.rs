//! The self-hosted telemetry endpoint: `POST /v1/events` ingest behind the
//! allowlist, an hourly rollup/retention pass, and a token-protected
//! dashboard. It makes no outbound connections and never reads peer
//! addresses.

use crate::db::Db;
use crate::protocol::{self, MAX_BODY_BYTES, MAX_EVENTS_PER_BATCH};
use codegraph_http::{parse_form, Request, Response};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub struct Config {
    pub db_path: PathBuf,
    pub retention_days: i64,
    /// Dashboard access; the dashboard is off when unset.
    pub dashboard_token: Option<String>,
    /// Mark the session cookie `Secure` (serving over HTTPS).
    pub secure_cookies: bool,
    /// Requests per machine per minute.
    pub rate_per_minute: u32,
}

pub struct Server {
    db: Mutex<Db>,
    cfg: Config,
    limiter: Mutex<HashMap<String, (u64, u32)>>,
    /// When the rollups last ran; the dashboard refreshes them when stale.
    rolled_at: AtomicU64,
}

/// The dashboard re-rolls recent days when its numbers are older than this.
const FRESH_MS: u64 = 5 * 60_000;

const SESSION_COOKIE: &str = "cg_dash";
const DASHBOARD_HTML: &str = include_str!("../static/dashboard.html");

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn info_text(keep_days: i64) -> String {
    format!(
        "CodeGraph anonymous-telemetry ingest.

What gets collected (and what never does) is documented field by field in
TELEMETRY.md in the codegraph repository, and this server's source is in
crates/codegraph-telemetry.

No code, file paths, repository/file/symbol names or queries are ever sent;
the client IP is never read or stored; the machine ID is a random UUID the
client mints locally and can delete at any time. Accepted events stay in this
server's own database and are never forwarded anywhere.

Individual events are deleted after {keep_days} days. Anonymous daily totals,
and which days each machine ID was active, outlive them.

Disable any time: codegraph telemetry off  |  CODEGRAPH_TELEMETRY=0  |  DO_NOT_TRACK=1
"
    )
}

fn session_value(token: &str) -> String {
    let digest = Sha256::digest(format!("codegraph-dashboard\0{token}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Server {
    pub fn new(cfg: Config) -> rusqlite::Result<Self> {
        let db = Db::open(&cfg.db_path)?;
        Ok(Server {
            db: Mutex::new(db),
            cfg,
            limiter: Mutex::new(HashMap::new()),
            rolled_at: AtomicU64::new(0),
        })
    }

    #[cfg(test)]
    fn in_memory(cfg: Config) -> Self {
        Server {
            db: Mutex::new(Db::open_in_memory().unwrap()),
            cfg,
            limiter: Mutex::new(HashMap::new()),
            rolled_at: AtomicU64::new(0),
        }
    }

    pub fn maintenance(&self) {
        self.rolled_at.store(now_ms(), Ordering::Relaxed);
        if let Ok(mut db) = self.db.lock() {
            if let Err(e) = db.run_maintenance(now_ms(), self.cfg.retention_days) {
                eprintln!("rollup failed: {e}");
            }
        }
    }

    pub fn handle(&self, req: Request) -> Response {
        match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") => Response::text(200, &info_text(self.cfg.retention_days)),
            ("GET", "/robots.txt") => Response::text(200, "User-agent: *\nDisallow: /\n"),
            (_, "/v1/events") if req.method != "POST" => {
                Response::text(405, "method not allowed\n").with_header("allow", "POST")
            }
            ("POST", "/v1/events") => self.ingest(&req),
            (_, "/login") => self.login(&req),
            (_, "/logout") => Response::empty(303)
                .with_header("location", "/login")
                .with_header(
                    "set-cookie",
                    &format!("{SESSION_COOKIE}=; Max-Age=0; Path=/"),
                ),
            ("GET", "/dashboard") => self.authed(&req, || {
                Response::new(200, "text/html; charset=utf-8", DASHBOARD_HTML)
                    .with_header("cache-control", "no-store")
            }),
            ("GET", p) if p.starts_with("/api/") => self.authed(&req, || self.api(&req)),
            _ => Response::text(404, "not found\n"),
        }
    }

    // -------------------------------------------------------------- ingest

    fn rate_limited(&self, machine_id: &str) -> bool {
        let Ok(mut l) = self.limiter.lock() else {
            return false; // fail open
        };
        let minute = now_ms() / 60_000;
        if l.len() > 100_000 {
            l.retain(|_, (m, _)| *m == minute);
        }
        let entry = l.entry(machine_id.to_string()).or_insert((minute, 0));
        if entry.0 != minute {
            *entry = (minute, 0);
        }
        entry.1 += 1;
        entry.1 > self.cfg.rate_per_minute
    }

    fn ingest(&self, req: &Request) -> Response {
        if req.body.is_empty() {
            return Response::text(411, "length required\n");
        }
        if req.body.len() > MAX_BODY_BYTES {
            return Response::text(413, "payload too large\n");
        }
        let Ok(body) = serde_json::from_slice::<Value>(&req.body) else {
            return Response::text(400, "bad request\n");
        };
        if !body.is_object() {
            return Response::text(400, "bad request\n");
        }
        let Some(machine_id) = body
            .get("machine_id")
            .and_then(Value::as_str)
            .filter(|m| protocol::is_uuid(m))
        else {
            return Response::text(400, "bad request\n");
        };
        let machine_id = machine_id.to_ascii_lowercase();
        if self.rate_limited(&machine_id) {
            return Response::text(429, "rate limited\n");
        }
        let envelope = protocol::sanitize_envelope(&body);
        let batch: Vec<protocol::Event> = body
            .get("events")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .take(MAX_EVENTS_PER_BATCH)
                    .filter_map(protocol::sanitize_event)
                    .collect()
            })
            .unwrap_or_default();
        // Nothing survived the allowlist: write nothing, not even bookkeeping.
        if !batch.is_empty() {
            if let Ok(mut db) = self.db.lock() {
                if let Err(e) = db.ingest(&machine_id, &envelope, &batch, now_ms()) {
                    // Counts only, never the payload.
                    eprintln!("write failed ({} events): {e}", batch.len());
                }
            }
        }
        // Every response is final: clients never retry.
        Response::empty(204)
    }

    // ----------------------------------------------------------- dashboard

    fn authorized(&self, req: &Request) -> bool {
        let Some(token) = &self.cfg.dashboard_token else {
            return false;
        };
        let expected = session_value(token);
        if let Some(c) = req.cookie(SESSION_COOKIE) {
            if constant_time_eq(c.as_bytes(), expected.as_bytes()) {
                return true;
            }
        }
        req.header("authorization")
            .and_then(|h| h.strip_prefix("Bearer "))
            .is_some_and(|t| constant_time_eq(t.as_bytes(), token.as_bytes()))
    }

    fn authed(&self, req: &Request, f: impl FnOnce() -> Response) -> Response {
        if self.cfg.dashboard_token.is_none() {
            return Response::text(404, "dashboard disabled (set CODEGRAPH_DASHBOARD_TOKEN)\n");
        }
        if self.authorized(req) {
            return f();
        }
        if req.path.starts_with("/api/") {
            Response::text(401, "unauthorized\n")
        } else {
            Response::empty(303).with_header("location", "/login")
        }
    }

    fn login(&self, req: &Request) -> Response {
        let Some(token) = &self.cfg.dashboard_token else {
            return Response::text(404, "dashboard disabled (set CODEGRAPH_DASHBOARD_TOKEN)\n");
        };
        let mut failed = false;
        if req.method == "POST" {
            let form = parse_form(&String::from_utf8_lossy(&req.body));
            let given = form.get("token").map(String::as_str).unwrap_or("");
            if constant_time_eq(given.as_bytes(), token.as_bytes()) {
                let secure = if self.cfg.secure_cookies {
                    "; Secure"
                } else {
                    ""
                };
                return Response::empty(303)
                    .with_header("location", "/dashboard")
                    .with_header(
                        "set-cookie",
                        &format!(
                            "{SESSION_COOKIE}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000{secure}",
                            session_value(token)
                        ),
                    );
            }
            // Slow down guessing.
            std::thread::sleep(Duration::from_millis(500));
            failed = true;
        }
        Response::new(200, "text/html; charset=utf-8", login_page(failed))
    }

    fn api(&self, req: &Request) -> Response {
        let days: i64 = req
            .query
            .get("days")
            .and_then(|d| d.parse().ok())
            .unwrap_or(30)
            .clamp(1, 365);
        if now_ms().saturating_sub(self.rolled_at.load(Ordering::Relaxed)) > FRESH_MS {
            self.maintenance();
        }
        let today = (now_ms() / 86_400_000) as i64;
        let from = protocol::day_from_epoch_days(today - days + 1);
        let to = protocol::day_from_epoch_days(today);
        let Ok(db) = self.db.lock() else {
            return Response::text(500, "internal error\n");
        };
        let result = match req.path.as_str() {
            "/api/session" => Ok(json!({ "authenticated": true })),
            "/api/health" => db.last_day().map(|d| json!({ "ok": true, "lastDay": d })),
            "/api/summary" => db.summary(&from, &to),
            "/api/timeseries" => db.timeseries(&from, &to),
            "/api/breakdown" => {
                let dim = req.query.get("dim").map(String::as_str).unwrap_or("os");
                let event = req
                    .query
                    .get("event")
                    .map(String::as_str)
                    .filter(|e| !e.is_empty());
                db.breakdown(dim, event, &from, &to)
            }
            "/api/retention" => {
                let weeks = (days / 7).clamp(2, 12);
                db.retention(weeks, now_ms())
            }
            _ => return Response::text(404, "not found\n"),
        };
        match result {
            Ok(v) => Response::json(&v).with_header("cache-control", "no-store"),
            Err(e) => {
                eprintln!("query failed: {e}");
                Response::text(500, "internal error\n")
            }
        }
    }
}

fn login_page(failed: bool) -> String {
    format!(
        r#"<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>CodeGraph Telemetry</title>
<style>
:root {{ --bg:#f7f7f5; --fg:#1d1d1b; --muted:#6b6b66; --card:#fff; --line:#e3e3de; --accent:#3559d6; --bad:#c2410c; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg:#141413; --fg:#ecece8; --muted:#a3a39c; --card:#1f1f1d; --line:#2f2f2c; --accent:#7f9cff; --bad:#fb923c; }} }}
body {{ margin:0; min-height:100vh; display:grid; place-items:center; background:var(--bg); color:var(--fg); font:15px/1.5 system-ui, sans-serif; }}
form {{ background:var(--card); border:1px solid var(--line); border-radius:12px; padding:28px; width:min(340px, calc(100vw - 32px)); box-sizing:border-box; }}
h1 {{ font-size:18px; margin:0 0 4px; }} p {{ color:var(--muted); margin:0 0 18px; font-size:13px; }}
input {{ width:100%; box-sizing:border-box; padding:9px 11px; border:1px solid var(--line); border-radius:8px; background:var(--bg); color:var(--fg); font:inherit; }}
button {{ margin-top:12px; width:100%; padding:9px; border:0; border-radius:8px; background:var(--accent); color:#fff; font:inherit; font-weight:600; cursor:pointer; }}
.err {{ color:var(--bad); font-size:13px; margin-top:10px; }}
</style></head><body>
<form method="post" action="/login">
<h1>CodeGraph telemetry</h1><p>Enter the dashboard token.</p>
<input type="password" name="token" autocomplete="current-password" autofocus aria-label="Dashboard token">
<button type="submit">Sign in</button>
{}
</form></body></html>"#,
        if failed {
            r#"<div class="err" role="alert">That token didn't match.</div>"#
        } else {
            ""
        }
    )
}

/// Serve on `listener` with an hourly rollup pass.
pub fn run(listener: TcpListener, cfg: Config) -> rusqlite::Result<()> {
    let server = Arc::new(Server::new(cfg)?);
    server.maintenance();
    let s = server.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(3600));
        s.maintenance();
    });
    let handler = Arc::new(move |req: Request| server.handle(req));
    codegraph_http::serve(listener, MAX_BODY_BYTES, handler);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server(token: Option<&str>) -> Server {
        Server::in_memory(Config {
            db_path: PathBuf::new(),
            retention_days: 90,
            dashboard_token: token.map(str::to_string),
            secure_cookies: false,
            rate_per_minute: 2,
        })
    }

    fn request(method: &str, path: &str, body: &str) -> Request {
        Request {
            method: method.into(),
            path: path.into(),
            query: HashMap::new(),
            headers: HashMap::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    #[test]
    fn ingest_validates_and_rate_limits() {
        let s = server(None);
        assert_eq!(s.handle(request("GET", "/v1/events", "")).status, 405);
        assert_eq!(
            s.handle(request("POST", "/v1/events", "{\"machine_id\":\"x\"}"))
                .status,
            400
        );
        let body = json!({
            "machine_id": "3F2B8C1E-0A4D-4C5E-9F7A-1B2C3D4E5F60",
            "codegraph_version": "0.1.0",
            "events": [{ "event": "index", "props": { "languages": ["go"] } }]
        })
        .to_string();
        assert_eq!(s.handle(request("POST", "/v1/events", &body)).status, 204);
        assert_eq!(s.handle(request("POST", "/v1/events", &body)).status, 204);
        assert_eq!(s.handle(request("POST", "/v1/events", &body)).status, 429);
        let n: i64 =
            s.db.lock()
                .unwrap()
                .conn()
                .query_row("SELECT count(*) FROM events", [], |r| r.get(0))
                .unwrap();
        assert_eq!(n, 2);
    }

    #[test]
    fn dashboard_requires_the_token() {
        assert_eq!(
            server(None).handle(request("GET", "/dashboard", "")).status,
            404
        );
        let s = server(Some("hunter2"));
        assert_eq!(s.handle(request("GET", "/dashboard", "")).status, 303);
        assert_eq!(s.handle(request("GET", "/api/summary", "")).status, 401);
        let ok = s.handle(request("POST", "/login", "token=hunter2"));
        assert_eq!(ok.status, 303);
        let cookie = ok
            .headers
            .iter()
            .find(|(k, _)| k == "set-cookie")
            .map(|(_, v)| v.split(';').next().unwrap().to_string())
            .unwrap();
        let mut req = request("GET", "/api/summary", "");
        req.headers.insert("cookie".into(), cookie);
        let r = s.handle(req);
        assert_eq!(r.status, 200);
        let v: Value = serde_json::from_slice(&r.body).unwrap();
        assert_eq!(v["machines"], 0);
    }
}
