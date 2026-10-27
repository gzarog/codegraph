//! `codegraph ui`: a local, read-mostly browser viewer for an indexed
//! project. The page is a Rust/WebAssembly app (crates/codegraph-viewer)
//! served with its JSON API from 127.0.0.1 only.
//!
//! Security model: the server binds loopback only and answers only requests
//! whose `Host` (and `Origin`, when sent) name that loopback address, which
//! defeats DNS rebinding. Writes (sync, saved trails) also need the
//! `x-codegraph-ui` header and a JSON body, which no cross-site form can
//! send. Files are served only from inside the project root. The server makes
//! no outbound connections.

pub mod api;

use crate::project::CodeGraph;
use codegraph_http::{Request, Response};
use serde_json::{json, Value};
use std::net::TcpListener;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const WRITE_HEADER: &str = "x-codegraph-ui";
const MAX_BODY: usize = 64 * 1024;
const MAX_TRAILS: usize = 50;

static INDEX_HTML: &str = include_str!("../../../codegraph-viewer/static/index.html");
static VIEWER_JS: &str = include_str!("../../../codegraph-viewer/static/viewer.js");
static VIEWER_CSS: &str = include_str!("../../../codegraph-viewer/static/viewer.css");
static VIEWER_WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/codegraph_viewer.wasm"));

/// A project-relative path resolved inside `root`, or None for anything
/// absolute, escaping, or missing.
pub fn resolve_project_file(root: &Path, rel: &str) -> Option<PathBuf> {
    let rel = Path::new(rel);
    if rel.is_absolute()
        || rel
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return None;
    }
    let full = root.join(rel).canonicalize().ok()?;
    let root = root.canonicalize().ok()?;
    (full.starts_with(&root) && full.is_file()).then_some(full)
}

pub struct Viewer {
    cg: Mutex<CodeGraph>,
    port: u16,
}

fn host_ok(value: &str, port: u16) -> bool {
    let value = value.trim_start_matches("http://").trim_end_matches('/');
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|h| value == format!("{h}:{port}"))
}

fn error(status: u16, message: &str) -> Response {
    Response::new(
        status,
        "application/json",
        json!({ "error": message }).to_string(),
    )
}

impl Viewer {
    pub fn new(cg: CodeGraph, port: u16) -> Self {
        Viewer {
            cg: Mutex::new(cg),
            port,
        }
    }

    pub fn handle(&self, req: Request) -> Response {
        if !req.header("host").is_some_and(|h| host_ok(h, self.port)) {
            return Response::text(403, "forbidden host\n");
        }
        if req.header("origin").is_some_and(|o| !host_ok(o, self.port)) {
            return Response::text(403, "forbidden origin\n");
        }
        let write = matches!(req.method.as_str(), "POST" | "DELETE");
        if write
            && (req.header(WRITE_HEADER) != Some("1")
                || !req
                    .header("content-type")
                    .is_some_and(|c| c.starts_with("application/json")))
        {
            return Response::text(403, "writes need the viewer's header\n");
        }
        if !matches!(req.method.as_str(), "GET" | "HEAD" | "POST" | "DELETE") {
            return Response::text(405, "method not allowed\n");
        }
        let resp = match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/") | ("GET", "/index.html") => {
                Response::new(200, "text/html; charset=utf-8", INDEX_HTML)
            }
            ("GET", "/viewer.js") => Response::new(200, "text/javascript; charset=utf-8", VIEWER_JS),
            ("GET", "/viewer.css") => Response::new(200, "text/css; charset=utf-8", VIEWER_CSS),
            ("GET", "/viewer.wasm") if !VIEWER_WASM.is_empty() => {
                Response::new(200, "application/wasm", VIEWER_WASM)
            }
            ("GET", "/viewer.wasm") => Response::text(
                503,
                "this codegraph was built without the viewer (install the wasm32-unknown-unknown target and rebuild)\n",
            ),
            (_, p) if p.starts_with("/api/") => self.api(&req),
            _ => Response::text(404, "not found\n"),
        };
        resp.with_header(
            "content-security-policy",
            "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'",
        )
        .with_header("referrer-policy", "no-referrer")
        .with_header("cache-control", "no-store")
    }

    fn api(&self, req: &Request) -> Response {
        let q = |k: &str| req.query.get(k).map(String::as_str).unwrap_or("");
        let mut cg = match self.cg.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let result = match (req.method.as_str(), req.path.as_str()) {
            ("GET", "/api/stats") => api::stats(&cg),
            ("GET", "/api/search") => api::search(&cg, q("q"), q("limit").parse().unwrap_or(20)),
            ("GET", "/api/node") => api::node(&cg, q("id")),
            ("GET", "/api/file") => api::file(&cg, q("path")),
            ("GET", "/api/map") => api::map(&cg),
            ("GET", "/api/flow") => api::flow(&cg, q("from"), q("to")),
            ("GET", "/api/deadcode") => api::dead_code(&cg),
            ("GET", "/api/routes") => api::routes(&cg),
            ("POST", "/api/sync") => cg
                .sync(None)
                .map(|r| json!({ "changed": r.files_indexed() + r.files_removed.len() }))
                .map_err(|e| (409, e.to_string())),
            ("GET", "/api/trails") => Ok(Value::Array(read_trails(cg.root()))),
            ("POST", "/api/trails") => save_trail(cg.root(), &req.body),
            ("DELETE", "/api/trails") => delete_trail(cg.root(), q("id")),
            _ => Err((404, "no such endpoint".into())),
        };
        match result {
            Ok(v) => Response::json(&v),
            Err((status, message)) => error(status, &message),
        }
    }
}

fn trails_path(root: &Path) -> PathBuf {
    crate::project::data_dir(root).join("trails.json")
}

fn read_trails(root: &Path) -> Vec<Value> {
    std::fs::read_to_string(trails_path(root))
        .ok()
        .and_then(|t| serde_json::from_str::<Vec<Value>>(&t).ok())
        .unwrap_or_default()
}

fn write_trails(root: &Path, trails: &[Value]) -> api::ApiResult {
    let json = serde_json::to_vec_pretty(trails).map_err(|e| (500, e.to_string()))?;
    crate::global::write_atomic(&trails_path(root), &json).map_err(|e| (500, e.to_string()))?;
    Ok(Value::Array(trails.to_vec()))
}

/// Save a named trail: `{ name, steps: [{ id, name, kind }] }`.
fn save_trail(root: &Path, body: &[u8]) -> api::ApiResult {
    let v: Value = serde_json::from_slice(body).map_err(|_| (400, "bad json".to_string()))?;
    let name: String = v
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("")
        .chars()
        .take(80)
        .collect();
    let steps: Vec<Value> = v
        .get("steps")
        .and_then(Value::as_array)
        .ok_or((400, "steps required".to_string()))?
        .iter()
        .take(50)
        .filter_map(|s| {
            Some(json!({
                "id": s.get("id")?.as_str()?,
                "name": s.get("name").and_then(Value::as_str).unwrap_or(""),
                "kind": s.get("kind").and_then(Value::as_str).unwrap_or(""),
            }))
        })
        .collect();
    if steps.is_empty() {
        return Err((400, "a trail needs at least one step".into()));
    }
    let mut trails = read_trails(root);
    let id = format!("t{}", crate::global::now_ms());
    trails.insert(
        0,
        json!({
            "id": id,
            "name": if name.trim().is_empty() { format!("Trail of {}", steps.len()) } else { name },
            "steps": steps,
            "createdAt": codegraph_telemetry::protocol::iso_timestamp(crate::global::now_ms()),
        }),
    );
    trails.truncate(MAX_TRAILS);
    write_trails(root, &trails)
}

fn delete_trail(root: &Path, id: &str) -> api::ApiResult {
    let mut trails = read_trails(root);
    trails.retain(|t| t.get("id").and_then(Value::as_str) != Some(id));
    write_trails(root, &trails)
}

/// Open `url` in the default browser.
pub fn open_browser(url: &str) -> bool {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]);
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    cmd.arg(url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .is_ok()
}

/// Serve the viewer for `cg` on 127.0.0.1:`port` (0 picks a free port).
/// Calls `ready` with the URL once listening, then serves forever.
pub fn serve(cg: CodeGraph, port: u16, ready: impl FnOnce(&str)) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let port = listener.local_addr()?.port();
    let url = format!("http://127.0.0.1:{port}/");
    let viewer = Arc::new(Viewer::new(cg, port));
    ready(&url);
    codegraph_http::serve(listener, MAX_BODY, Arc::new(move |req| viewer.handle(req)));
    Ok(())
}

pub fn viewer_built() -> bool {
    !VIEWER_WASM.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn req(method: &str, path: &str, headers: &[(&str, &str)]) -> Request {
        let (path, query) = path.split_once('?').unwrap_or((path, ""));
        Request {
            method: method.into(),
            path: path.into(),
            query: codegraph_http::parse_form(query),
            headers: headers
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<HashMap<_, _>>(),
            body: Vec::new(),
        }
    }

    fn viewer() -> (tempfile::TempDir, Viewer) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/a.ts"),
            "export function helper() { return 1; }\nexport function main() { return helper(); }\n",
        )
        .unwrap();
        let mut cg = CodeGraph::init(dir.path()).unwrap();
        cg.index_all(None).unwrap();
        (dir, Viewer::new(cg, 4000))
    }

    #[test]
    fn refuses_foreign_hosts_and_unguarded_writes() {
        let (_d, v) = viewer();
        assert_eq!(
            v.handle(req("GET", "/api/stats", &[("host", "evil.com:4000")]))
                .status,
            403
        );
        assert_eq!(
            v.handle(req(
                "GET",
                "/api/stats",
                &[("host", "127.0.0.1:4000"), ("origin", "http://evil.com")]
            ))
            .status,
            403
        );
        assert_eq!(
            v.handle(req("POST", "/api/sync", &[("host", "127.0.0.1:4000")]))
                .status,
            403
        );
        assert_eq!(
            v.handle(req("GET", "/api/stats", &[("host", "localhost:4000")]))
                .status,
            200
        );
    }

    #[test]
    fn serves_screens() {
        let (_d, v) = viewer();
        let h = [("host", "127.0.0.1:4000")];
        let body = |r: Response| serde_json::from_slice::<Value>(&r.body).unwrap();
        let stats = body(v.handle(req("GET", "/api/stats", &h)));
        assert_eq!(stats["files"], 1);
        let hits = body(v.handle(req("GET", "/api/search?q=helper", &h)));
        let id = hits[0]["id"].as_str().unwrap().to_string();
        let node = body(v.handle(req("GET", &format!("/api/node?id={id}"), &h)));
        assert_eq!(node["node"]["name"], "helper");
        assert_eq!(node["callers"]["groups"][0]["name"], "main");
        assert_eq!(
            node["source"]["lines"][0],
            "export function helper() { return 1; }"
        );
        let file = body(v.handle(req("GET", "/api/file?path=src/a.ts", &h)));
        assert_eq!(file["outline"].as_array().unwrap().len(), 2);
        assert_eq!(
            v.handle(req("GET", "/api/file?path=../etc/passwd", &h))
                .status,
            404
        );
        let map = body(v.handle(req("GET", "/api/map", &h)));
        assert_eq!(map["modules"][0]["id"], "src");
    }

    #[test]
    fn resolves_only_inside_root() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "x").unwrap();
        assert!(resolve_project_file(dir.path(), "a.txt").is_some());
        assert!(resolve_project_file(dir.path(), "../a.txt").is_none());
        assert!(resolve_project_file(dir.path(), "/etc/passwd").is_none());
    }
}
