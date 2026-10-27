//! MCP server over stdio (JSON-RPC 2.0, one message per line).

use crate::format;
use crate::project::{find_root, CodeGraph};
use codegraph_core::{NodeKind, SearchOptions};
use codegraph_graph::{DEPENDENCY_EDGES, FLOW_EDGES};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub const SERVER_INSTRUCTIONS: &str =
    "# Codegraph — code intelligence over an indexed knowledge graph

Codegraph is a SQLite knowledge graph of every symbol, edge and file in the
workspace, built from a full tree-sitter parse of 15+ languages
(TypeScript/JavaScript, Python, Go, Rust, Java, C#, C/C++, PHP, Ruby, Swift,
Kotlin, Scala, Dart, Lua, R). Reads are fast; a file watcher keeps the index
about a second behind your edits.

## Use codegraph_explore instead of reading files

`codegraph_explore` takes a natural-language question or a bag of symbol/file
names and returns the verbatim, line-numbered source of the relevant symbols
grouped by file (the same `<n>\\t<line>` shape Read gives you, safe to Edit
from), the call path among the symbols you named, and a blast-radius summary
of what depends on them. One call usually answers the question; treat the
source it returns as already Read.

- How does X work / where is X / a bug / architecture → `codegraph_explore`.
- A flow from X to Y → name both endpoints in one query.
- About to edit a symbol or file → put its name or path in the query.

## Anti-patterns

- Don't re-verify results with grep; they come from a full AST parse.
- Don't reconstruct a flow by hand; name its endpoints in one query.
- A file flagged \"⚠ changed on disk after the last index sync\" is shown from
  disk (or omitted); Read it directly if its source was omitted.

## Limitations

- If a project isn't indexed (no `.codegraph/`), use your built-in tools there;
  the user can run `codegraph init` to enable codegraph.
- Cross-file resolution is best-effort name matching; ambiguous calls may
  resolve to a likely candidate.
";

const READ_ONLY: &str = r#"{"readOnlyHint": true, "openWorldHint": false}"#;

fn project_path_prop() -> Value {
    json!({ "type": "string", "description": "Path to a different project with a .codegraph/ index (defaults to the server's project)" })
}

/// Tool definitions, in display order.
pub fn tool_definitions() -> Vec<Value> {
    let ro: Value = serde_json::from_str(READ_ONLY).unwrap();
    vec![
        json!({
            "name": "codegraph_explore",
            "description": "PRIMARY TOOL — call FIRST for almost any question OR before an edit: how does X work, architecture, a bug, where/what is X, or the symbols you are about to change. Returns the verbatim source of the relevant symbols grouped by file in ONE capped call (Read-equivalent — treat the shown source as already Read), plus the call path among them and what depends on them. Query can be a natural-language question OR a bag of symbol/file names.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string", "description": "Symbol names, file names, or short code terms (e.g. \"AuthService loginUser session.ts\"). For a flow question, name the symbols spanning it. A natural-language question works too." },
                "maxFiles": { "type": "number", "description": "Maximum number of files to return source from, 1–20 (default sized to the project)." },
                "projectPath": project_path_prop()
            }, "required": ["query"] },
            "annotations": ro,
            "_meta": { "anthropic/alwaysLoad": true }
        }),
        json!({
            "name": "codegraph_search",
            "description": "Quick symbol search by name. Returns locations only (no code). Use codegraph_explore to get the source.",
            "inputSchema": { "type": "object", "properties": {
                "query": { "type": "string", "description": "Symbol name or partial name; supports kind:/lang:/path:/name: filters" },
                "kind": { "type": "string", "description": "Filter by node kind", "enum": ["function", "method", "class", "interface", "type", "variable", "route", "component", "struct", "enum", "trait"] },
                "limit": { "type": "number", "description": "Maximum results (default 10)", "default": 10 },
                "projectPath": project_path_prop()
            }, "required": ["query"] },
            "annotations": ro
        }),
        json!({
            "name": "codegraph_callers",
            "description": "List symbols that call/use <symbol>.",
            "inputSchema": { "type": "object", "properties": {
                "symbol": { "type": "string", "description": "Function, method or class name (qualified names accepted)" },
                "file": { "type": "string", "description": "Narrow to the definition in this file (path or suffix)" },
                "limit": { "type": "number", "description": "Maximum results (default 20)", "default": 20 },
                "projectPath": project_path_prop()
            }, "required": ["symbol"] },
            "annotations": ro
        }),
        json!({
            "name": "codegraph_callees",
            "description": "List symbols that <symbol> calls/uses.",
            "inputSchema": { "type": "object", "properties": {
                "symbol": { "type": "string", "description": "Function, method or class name (qualified names accepted)" },
                "file": { "type": "string", "description": "Narrow to the definition in this file (path or suffix)" },
                "limit": { "type": "number", "description": "Maximum results (default 20)", "default": 20 },
                "projectPath": project_path_prop()
            }, "required": ["symbol"] },
            "annotations": ro
        }),
        json!({
            "name": "codegraph_impact",
            "description": "List symbols affected by changing <symbol>. Use before a refactor.",
            "inputSchema": { "type": "object", "properties": {
                "symbol": { "type": "string", "description": "Symbol to analyze" },
                "file": { "type": "string", "description": "Narrow to the definition in this file (path or suffix)" },
                "depth": { "type": "number", "description": "Levels of dependents to follow (default 2)", "default": 2 },
                "projectPath": project_path_prop()
            }, "required": ["symbol"] },
            "annotations": ro
        }),
        json!({
            "name": "codegraph_node",
            "description": "Two modes. (1) READ A FILE: pass `file` alone to get its current source with line numbers (like Read, with offset/limit) plus which files depend on it. (2) ONE SYMBOL: its location, signature, optional verbatim source (includeCode) and caller/callee trail; an ambiguous name returns every definition.",
            "inputSchema": { "type": "object", "properties": {
                "symbol": { "type": "string", "description": "Symbol name (symbol mode). Omit to read `file`." },
                "includeCode": { "type": "boolean", "description": "Symbol mode: include the full body", "default": false },
                "file": { "type": "string", "description": "File path or basename; alone it reads the file, with a symbol it disambiguates" },
                "offset": { "type": "number", "description": "File mode: 1-based start line" },
                "limit": { "type": "number", "description": "File mode: maximum lines (capped at 2000)" },
                "symbolsOnly": { "type": "boolean", "description": "File mode: just the symbol map + dependents", "default": false },
                "line": { "type": "number", "description": "Symbol mode: pick the definition at/around this line" },
                "projectPath": project_path_prop()
            } },
            "annotations": ro
        }),
        json!({
            "name": "codegraph_status",
            "description": "Index health: files, nodes, edges, last indexed time, and pending added/modified/removed files.",
            "inputSchema": { "type": "object", "properties": { "projectPath": project_path_prop() } },
            "annotations": ro
        }),
        json!({
            "name": "codegraph_files",
            "description": "Indexed file tree with language and symbol counts. Faster than Glob for project layout.",
            "inputSchema": { "type": "object", "properties": {
                "path": { "type": "string", "description": "Only files under this directory" },
                "pattern": { "type": "string", "description": "Glob filter (e.g. \"*.tsx\")" },
                "format": { "type": "string", "enum": ["tree", "flat", "grouped"], "default": "tree" },
                "includeMetadata": { "type": "boolean", "default": true },
                "maxDepth": { "type": "number", "description": "Maximum directory depth" },
                "projectPath": project_path_prop()
            } },
            "annotations": ro
        }),
    ]
}

/// Which tools are listed: `CODEGRAPH_MCP_TOOLS` = `all` or a comma list
/// (with or without the `codegraph_` prefix). Default: explore only.
pub fn enabled_tools() -> Vec<String> {
    let all: Vec<String> = tool_definitions()
        .iter()
        .map(|t| t["name"].as_str().unwrap_or("").to_string())
        .collect();
    match std::env::var("CODEGRAPH_MCP_TOOLS") {
        Ok(v) if v.trim() == "all" => all,
        Ok(v) if !v.trim().is_empty() => v
            .split(',')
            .map(|t| t.trim())
            .map(|t| {
                if t.starts_with("codegraph_") {
                    t.to_string()
                } else {
                    format!("codegraph_{t}")
                }
            })
            .filter(|t| all.contains(t))
            .collect(),
        _ => vec!["codegraph_explore".to_string()],
    }
}

/// Open projects and their watchers, shared by every session of a process
/// (one in stdio mode, many in the daemon).
pub struct Engine {
    projects: Mutex<HashMap<PathBuf, Arc<Mutex<CodeGraph>>>>,
    watchers: Mutex<HashMap<PathBuf, crate::watch::Watcher>>,
    watch: bool,
}

impl Engine {
    pub fn new(watch: bool) -> Arc<Self> {
        Arc::new(Engine {
            projects: Mutex::new(HashMap::new()),
            watchers: Mutex::new(HashMap::new()),
            watch,
        })
    }

    /// The open project at `root`, opening (and catching up) on first use.
    pub fn project(&self, root: &Path) -> Result<Arc<Mutex<CodeGraph>>, String> {
        let mut projects = self.projects.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(cg) = projects.get(root) {
            return Ok(cg.clone());
        }
        let mut cg = CodeGraph::open(root).map_err(|e| e.to_string())?;
        // Catch up with edits made while no server was watching.
        let _ = cg.sync(None);
        let cg = Arc::new(Mutex::new(cg));
        if self.watch {
            if let Ok(w) = crate::watch::watch(cg.clone(), |_| {}) {
                self.watchers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .insert(root.to_path_buf(), w);
            }
        }
        projects.insert(root.to_path_buf(), cg.clone());
        Ok(cg)
    }

    /// Changed-but-unsynced files, when a watcher covers `root`.
    pub fn pending(&self, root: &Path) -> Option<Vec<String>> {
        self.watchers
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(root)
            .map(|w| w.pending())
    }
}

/// Cross-call explore dedup is opt-in: an MCP connection isn't a reliable
/// conversation boundary (hosts reuse connections for subagents, and
/// compaction drops source while the connection lives).
pub fn explore_dedup_enabled() -> bool {
    crate::global::env_truthy("CODEGRAPH_EXPLORE_DEDUP")
}

/// One MCP session: protocol state over a shared engine.
pub struct Server {
    default_root: Option<PathBuf>,
    engine: Arc<Engine>,
    tools: Vec<String>,
    /// The connecting agent's `clientInfo` name and version.
    client: Option<(String, String)>,
    /// What this session's explore calls already showed, per project.
    memory: Option<HashMap<PathBuf, codegraph_context::ExploreMemory>>,
    /// The client can answer `roots/list`.
    client_roots: bool,
    /// Requests this server sends the client, written after the next reply.
    outbox: Vec<Value>,
    /// Indexed projects below the launch directory, when none was adopted.
    subprojects: Vec<PathBuf>,
}

/// The id of this server's `roots/list` request.
const ROOTS_REQUEST_ID: &str = "codegraph-roots";

/// Prefix `text` with a warning naming the files it mentions that changed on
/// disk but aren't re-indexed yet.
pub fn with_staleness_banner(text: String, pending: &[String]) -> String {
    let mentioned: Vec<&String> = pending
        .iter()
        .filter(|p| text.contains(p.as_str()))
        .collect();
    if mentioned.is_empty() {
        return text;
    }
    let list = mentioned
        .iter()
        .take(10)
        .map(|p| p.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "⚠️ Some files referenced below were edited since the last index sync and are pending re-index: {list}. Read those specific files for accurate content; everything else below is fresh.\n\n{text}"
    )
}

fn text_result(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn error_result(text: String) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
}

fn arg_num(args: &Value, key: &str) -> Option<u64> {
    args.get(key)
        .and_then(|v| v.as_f64())
        .filter(|n| *n >= 0.0)
        .map(|n| n as u64)
}

fn arg_bool(args: &Value, key: &str) -> Option<bool> {
    args.get(key).and_then(|v| v.as_bool())
}

impl Server {
    pub fn new(default_root: Option<PathBuf>, watch: bool) -> Self {
        Self::with_engine(default_root, Engine::new(watch))
    }

    pub fn with_engine(default_root: Option<PathBuf>, engine: Arc<Engine>) -> Self {
        Server {
            default_root,
            engine,
            tools: enabled_tools(),
            client: None,
            memory: explore_dedup_enabled().then(HashMap::new),
            client_roots: false,
            outbox: Vec::new(),
            subprojects: Vec::new(),
        }
    }

    /// Indexed sub-projects of the launch directory: exactly one becomes the
    /// default project; several are listed when a call names none.
    pub fn with_subprojects(mut self, found: Vec<PathBuf>) -> Self {
        if self.default_root.is_none() {
            match found.as_slice() {
                [one] => self.default_root = Some(one.clone()),
                _ => self.subprojects = found,
            }
        }
        self
    }

    fn no_default_guidance(&self) -> String {
        let mut out = "This server has no default project (no .codegraph/ at or above the directory it was started in). Pass `projectPath` with the project's directory.".to_string();
        if !self.subprojects.is_empty() {
            out.push_str("\nIndexed projects under the workspace:");
            for p in self.subprojects.iter().take(20) {
                out.push_str(&format!("\n- {}", p.display()));
            }
        }
        out
    }

    /// The project a call is about. Err holds guidance for the agent.
    fn project(&mut self, project_path: Option<&str>) -> Result<Arc<Mutex<CodeGraph>>, String> {
        let start = match project_path {
            Some(p) => PathBuf::from(p),
            None => self
                .default_root
                .clone()
                .ok_or_else(|| self.no_default_guidance())?,
        };
        let not_indexed = || {
            format!("{} is not indexed (no .codegraph/ found). Use your built-in tools for it; the user can run `codegraph init` there.", start.display())
        };
        let root = find_root(&start).ok_or_else(not_indexed)?;
        let cg = self.engine.project(&root)?;
        if let Some(nested) = unindexed_nested_repo(&start, &root, &cg) {
            return Err(format!(
                "{} is a separate repository inside {} that its index doesn't cover. Use your built-in tools for it; the user can run `codegraph init` there.",
                nested.display(),
                root.display()
            ));
        }
        Ok(cg)
    }

    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => {
                if let Some(info) = params.get("clientInfo") {
                    let field = |k: &str| {
                        info.get(k)
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string()
                    };
                    self.client = Some((field("name"), field("version")));
                }
                let requested = params
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or(PROTOCOL_VERSIONS[0]);
                let version = if PROTOCOL_VERSIONS.contains(&requested) {
                    requested
                } else {
                    PROTOCOL_VERSIONS[0]
                };
                self.client_roots = params
                    .pointer("/capabilities/roots")
                    .is_some_and(|r| !r.is_null());
                if self.default_root.is_none() {
                    let uris = params
                        .get("rootUri")
                        .and_then(Value::as_str)
                        .into_iter()
                        .chain(
                            params
                                .get("workspaceFolders")
                                .and_then(Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(|f| f.get("uri").and_then(Value::as_str)),
                        );
                    self.default_root = uris.filter_map(file_uri_path).find_map(|p| find_root(&p));
                }
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "codegraph", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": match crate::upgrade::cached_notice() {
                        Some(n) => format!("{SERVER_INSTRUCTIONS}\n\n{n}"),
                        None => SERVER_INSTRUCTIONS.to_string(),
                    },
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                let no_default = self.default_root.is_none();
                let tools: Vec<Value> = tool_definitions()
                    .into_iter()
                    .filter(|t| self.tools.iter().any(|n| t["name"] == n.as_str()))
                    .map(|mut t| {
                        // Without a default project every call must name one.
                        if no_default {
                            let schema = &mut t["inputSchema"];
                            let mut required =
                                schema["required"].as_array().cloned().unwrap_or_default();
                            required.push(json!("projectPath"));
                            schema["required"] = Value::Array(required);
                        }
                        t
                    })
                    .collect();
                Ok(json!({ "tools": tools }))
            }
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                let trimmed =
                    std::env::var("CODEGRAPH_MCP_TOOLS").is_ok_and(|v| !v.trim().is_empty());
                if !tool_definitions().iter().any(|t| t["name"] == name)
                    || trimmed && !self.tools.iter().any(|t| t == name)
                {
                    Err((-32602, format!("unknown tool: {name}")))
                } else {
                    let result = self.call_tool(name, &args);
                    crate::telemetry::record_usage(
                        crate::telemetry::UsageKind::McpTool,
                        name,
                        result.get("isError").and_then(Value::as_bool) == Some(true),
                        self.client.as_ref().map(|(n, v)| (n.as_str(), v.as_str())),
                    );
                    Ok(result)
                }
            }
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            "notifications/initialized" => {
                if self.default_root.is_none() && self.client_roots {
                    self.outbox.push(json!({
                        "jsonrpc": "2.0",
                        "id": ROOTS_REQUEST_ID,
                        "method": "roots/list",
                    }));
                }
                return None;
            }
            m if m.starts_with("notifications/") => return None,
            // A response to a request of ours.
            "" if msg.get("result").is_some() || msg.get("error").is_some() => {
                if id.as_ref().and_then(Value::as_str) == Some(ROOTS_REQUEST_ID)
                    && self.default_root.is_none()
                {
                    self.default_root = msg
                        .pointer("/result/roots")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(|r| r.get("uri").and_then(Value::as_str))
                        .filter_map(file_uri_path)
                        .find_map(|p| find_root(&p));
                }
                return None;
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        let id = id?;
        Some(match result {
            Ok(r) => json!({ "jsonrpc": "2.0", "id": id, "result": r }),
            Err((code, message)) => {
                json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
            }
        })
    }

    pub fn call_tool(&mut self, name: &str, args: &Value) -> Value {
        // Guidance, not an error: early errors teach an agent to give up on
        // the tools for the whole session.
        let cg = match self.project(arg_str(args, "projectPath")) {
            Ok(cg) => cg,
            Err(e) => return text_result(e),
        };
        let mut guard = match cg.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        let pending = match self.engine.pending(guard.root()) {
            Some(p) => p,
            None => {
                // No watcher: catch up now (unchanged files are skipped by stat).
                let _ = guard.sync(None);
                Vec::new()
            }
        };
        let memory = self
            .memory
            .as_mut()
            .map(|m| m.entry(guard.root().to_path_buf()).or_default());
        match run_tool_in_session(&guard, name, args, memory) {
            Ok(text) => text_result(with_staleness_banner(text, &pending)),
            Err(e) => error_result(e),
        }
    }

    /// Serve until stdin closes.
    pub fn run(&mut self, input: impl BufRead, output: impl Write) -> std::io::Result<()> {
        self.run_lines(input.lines().map_while(Result::ok), output)
    }

    /// Serve each line until the lines end.
    pub fn run_lines(
        &mut self,
        lines: impl Iterator<Item = String>,
        mut output: impl Write,
    ) -> std::io::Result<()> {
        for line in lines {
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(Value::Array(batch)) => {
                    let replies: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
                    (!replies.is_empty()).then_some(Value::Array(replies))
                }
                Ok(msg) => self.handle(&msg),
                Err(e) => Some(
                    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } }),
                ),
            };
            for r in reply.into_iter().chain(self.outbox.drain(..)) {
                writeln!(output, "{r}")?;
                output.flush()?;
            }
        }
        Ok(())
    }
}

/// The local path a `file://` URI names (percent-escapes decoded).
pub fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // `file://localhost/x` and `file:///x` both name `/x`.
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&rest[i + 1..i + 3], 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    let path = String::from_utf8(out).ok()?;
    // `/C:/work` on Windows is `C:/work`.
    let path = match path.as_bytes() {
        [b'/', d, b':', ..] if d.is_ascii_alphabetic() => path[1..].to_string(),
        _ => path,
    };
    Some(PathBuf::from(path))
}

/// A separate git repository between `start` and the index `root` that the
/// index holds no files of: the ancestor's index doesn't speak for it.
fn unindexed_nested_repo(start: &Path, root: &Path, cg: &Arc<Mutex<CodeGraph>>) -> Option<PathBuf> {
    let start = start.canonicalize().ok()?;
    let nested = start
        .ancestors()
        .take_while(|d| *d != root)
        .filter(|d| d.join(".git").exists())
        .last()?
        .to_path_buf();
    let rel = nested
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let guard = cg.lock().unwrap_or_else(|p| p.into_inner());
    let covered = guard.store().files_under(&rel).is_ok_and(|f| !f.is_empty());
    (!covered).then_some(nested)
}

/// Directories skipped when looking for indexed sub-projects.
const SCAN_SKIP: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "vendor",
    ".git",
    ".venv",
    "venv",
    "__pycache__",
];

/// Indexed projects below a workspace directory (one with `.git` or a
/// manifest), three levels deep at most.
pub fn scan_subprojects(base: &Path) -> Vec<PathBuf> {
    const MANIFESTS: &[&str] = &[
        ".git",
        "package.json",
        "Cargo.toml",
        "go.mod",
        "pyproject.toml",
        "pom.xml",
        "build.gradle",
        "composer.json",
        "Gemfile",
        "pnpm-workspace.yaml",
    ];
    if !MANIFESTS.iter().any(|m| base.join(m).exists()) {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut level = vec![base.to_path_buf()];
    let mut visited = 0;
    for _ in 0..3 {
        let mut next = Vec::new();
        for dir in level {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                visited += 1;
                if visited > 5000 {
                    return found;
                }
                let name = e.file_name().to_string_lossy().to_string();
                if name.starts_with('.') || SCAN_SKIP.contains(&name.as_str()) {
                    continue;
                }
                let path = e.path();
                if !e.file_type().is_ok_and(|t| t.is_dir()) {
                    continue;
                }
                if crate::project::is_initialized(&path) {
                    found.push(path);
                } else {
                    next.push(path);
                }
            }
        }
        level = next;
    }
    found.sort();
    found
}

/// A `codegraph_files` path filter as a project-relative directory: `/`, `.`
/// and an empty string mean the whole project; `./src`, backslashes and
/// absolute paths inside the project are accepted.
pub fn normalize_dir_filter(dir: &str, root: &Path) -> Option<String> {
    let d = dir.trim().replace('\\', "/");
    let root_s = root.to_string_lossy().replace('\\', "/");
    let d = d.strip_prefix(&root_s).unwrap_or(&d);
    let d = d.trim_start_matches("./").trim_matches('/');
    let d = d.strip_prefix("./").unwrap_or(d);
    (!d.is_empty() && d != ".").then(|| d.to_string())
}

/// Map the search tool's `type` shorthand to a kind.
fn kind_filter(kind: &str) -> Vec<NodeKind> {
    match kind {
        "type" => vec![
            NodeKind::TypeAlias,
            NodeKind::Interface,
            NodeKind::Struct,
            NodeKind::Enum,
        ],
        k => k.parse().map(|k| vec![k]).unwrap_or_default(),
    }
}

/// `run_tool`, with explore remembering what this session was shown.
pub fn run_tool_in_session(
    cg: &CodeGraph,
    name: &str,
    args: &Value,
    memory: Option<&mut codegraph_context::ExploreMemory>,
) -> Result<String, String> {
    match (name, memory) {
        ("codegraph_explore", Some(memory)) => {
            let query = arg_str(args, "query").ok_or("`query` is required")?;
            cg.explore_in_session(query, arg_num(args, "maxFiles").map(|n| n as usize), memory)
                .map_err(|e| e.to_string())
        }
        _ => run_tool(cg, name, args),
    }
}

/// Execute one tool against an open project.
pub fn run_tool(cg: &CodeGraph, name: &str, args: &Value) -> Result<String, String> {
    let e = |err: crate::project::ProjectError| err.to_string();
    let se = |err: codegraph_store::StoreError| err.to_string();
    match name {
        "codegraph_explore" => {
            let query = arg_str(args, "query").ok_or("`query` is required")?;
            cg.explore(query, arg_num(args, "maxFiles").map(|n| n as usize))
                .map_err(e)
        }
        "codegraph_search" => {
            let query = arg_str(args, "query").ok_or("`query` is required")?;
            let opts = SearchOptions {
                kinds: arg_str(args, "kind").map(kind_filter).unwrap_or_default(),
                limit: Some(arg_num(args, "limit").unwrap_or(10).clamp(1, 100) as usize),
                ..Default::default()
            };
            Ok(format::search_results(
                &cg.store().search_nodes(query, &opts).map_err(se)?,
            ))
        }
        "codegraph_callers" | "codegraph_callees" => {
            let symbol = arg_str(args, "symbol").ok_or("`symbol` is required")?;
            let limit = arg_num(args, "limit").unwrap_or(20).clamp(1, 200) as usize;
            let (defs, file_ok) = cg.find_symbols(symbol, arg_str(args, "file")).map_err(e)?;
            if defs.is_empty() {
                return Ok(format!("No symbol named `{symbol}` in the index."));
            }
            let g = cg.graph();
            let mut out = Vec::new();
            for d in defs.into_iter().take(8) {
                let list = if name == "codegraph_callers" {
                    g.callers(&d.id, DEPENDENCY_EDGES)
                } else {
                    g.callees(&d.id, FLOW_EDGES)
                };
                out.push((d, list.map_err(se)?));
            }
            let note = (!file_ok).then(|| {
                format!(
                    "No definition of `{symbol}` in a file matching `{}`; showing all.",
                    arg_str(args, "file").unwrap_or("")
                )
            });
            Ok(format::neighbor_list(
                if name == "codegraph_callers" {
                    "callers"
                } else {
                    "callees"
                },
                &out,
                limit,
                note.as_deref(),
            ))
        }
        "codegraph_impact" => {
            let symbol = arg_str(args, "symbol").ok_or("`symbol` is required")?;
            let depth = arg_num(args, "depth").unwrap_or(2).clamp(1, 6) as u32;
            let (defs, _) = cg.find_symbols(symbol, arg_str(args, "file")).map_err(e)?;
            if defs.is_empty() {
                return Ok(format!("No symbol named `{symbol}` in the index."));
            }
            let g = cg.graph();
            let mut out = Vec::new();
            for d in defs.into_iter().take(5) {
                let imp = g.impact(&d.id, depth, 300).map_err(se)?;
                out.push((d, imp));
            }
            Ok(format::impact(&out))
        }
        "codegraph_node" => node_tool(cg, args),
        "codegraph_status" => {
            let mut out = format::status(&cg.status().map_err(e)?);
            if let Some(n) = crate::upgrade::cached_notice() {
                out.push_str(&format!("\n{n}\n"));
            }
            Ok(out)
        }
        "codegraph_files" => {
            let files = cg.store().all_files().map_err(se)?;
            let format = arg_str(args, "format")
                .unwrap_or("tree")
                .parse()
                .map_err(|err: String| err)?;
            let dir = arg_str(args, "path").and_then(|d| normalize_dir_filter(d, cg.root()));
            Ok(format::files(
                &files,
                dir.as_deref(),
                arg_str(args, "pattern"),
                format,
                arg_bool(args, "includeMetadata").unwrap_or(true),
                arg_num(args, "maxDepth").map(|n| n as usize),
            ))
        }
        other => Err(format!("unknown tool: {other}")),
    }
}

fn resolve_file(cg: &CodeGraph, file: &str) -> Result<Option<codegraph_core::FileRecord>, String> {
    let f = file.trim_start_matches("./");
    let root = cg.root().to_string_lossy().to_string();
    let f = f.strip_prefix(&format!("{root}/")).unwrap_or(f);
    if let Some(rec) = cg.store().file(f).map_err(|e| e.to_string())? {
        return Ok(Some(rec));
    }
    let all = cg.store().all_files().map_err(|e| e.to_string())?;
    let mut hits: Vec<_> = all
        .into_iter()
        .filter(|r| r.path.ends_with(&format!("/{f}")))
        .collect();
    hits.sort_by_key(|r| r.path.len());
    Ok(hits.into_iter().next())
}

fn node_tool(cg: &CodeGraph, args: &Value) -> Result<String, String> {
    let file = arg_str(args, "file");
    let Some(symbol) = arg_str(args, "symbol") else {
        let file = file.ok_or("pass `symbol`, or `file` to read a file")?;
        let rec =
            resolve_file(cg, file)?.ok_or_else(|| format!("`{file}` is not an indexed file."))?;
        return format::file_view(
            cg,
            &rec,
            arg_num(args, "offset").map(|n| n as u32),
            arg_num(args, "limit").map(|n| n as u32),
            arg_bool(args, "symbolsOnly").unwrap_or(false),
        )
        .map_err(|e| e.to_string());
    };
    let (mut defs, _) = cg.find_symbols(symbol, file).map_err(|e| e.to_string())?;
    if defs.is_empty() {
        return Ok(format!("No symbol named `{symbol}` in the index."));
    }
    if let Some(line) = arg_num(args, "line") {
        let line = line as u32;
        defs.sort_by_key(|d| {
            if d.start_line <= line && line <= d.end_line {
                0
            } else {
                d.start_line.abs_diff(line) + 1
            }
        });
        defs.truncate(1);
    }
    // An ambiguous name returns every body, so the right overload needs no second call.
    let include_code = arg_bool(args, "includeCode").unwrap_or(false) || defs.len() > 1;
    let mut out = String::new();
    for d in defs.iter().take(6) {
        out.push_str(&format::node_details(cg, d, include_code).map_err(|e| e.to_string())?);
        out.push('\n');
    }
    if defs.len() > 6 {
        out.push_str(&format!(
            "… {} more definitions; pass `file` to narrow.\n",
            defs.len() - 6
        ));
    }
    Ok(out)
}

/// Exit when the agent that launched us dies. An agent killed without closing
/// our stdin would otherwise leave this server (and its watcher) running;
/// on Unix the orphan is reparented, which changes `getppid()`.
fn exit_with_parent() {
    #[cfg(unix)]
    {
        let parent = unsafe { libc::getppid() };
        std::thread::spawn(move || loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if unsafe { libc::getppid() } != parent {
                std::process::exit(0);
            }
        });
    }
}

/// Telemetry upkeep for a long-lived server: send completed days now, keep
/// counts on disk every minute, send again hourly.
pub fn start_telemetry_thread() {
    std::thread::spawn(|| {
        crate::telemetry::flush();
        let mut minutes = 0u32;
        loop {
            std::thread::sleep(std::time::Duration::from_secs(60));
            minutes = (minutes + 1) % 60;
            if minutes == 0 {
                crate::telemetry::flush();
            } else {
                crate::telemetry::persist();
            }
        }
    });
}

/// Default wait for the first byte of MCP traffic before assuming the
/// launch was abandoned (a host that kept our pipes open but went away).
const STARTUP_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(900);

/// `CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS`: 0 disables; invalid → default.
fn startup_handshake_timeout() -> Option<Duration> {
    match std::env::var("CODEGRAPH_STARTUP_HANDSHAKE_TIMEOUT_MS")
        .ok()
        .map(|v| v.trim().parse::<i64>())
    {
        Some(Ok(ms)) if ms <= 0 => None,
        Some(Ok(ms)) => Some(Duration::from_millis(ms as u64)),
        _ => Some(STARTUP_HANDSHAKE_TIMEOUT),
    }
}

/// Read stdin on a thread from the start, so the process answers probes
/// (`server/discover`) at once and can tell whether anyone is talking to it,
/// whatever the main thread is waiting on.
fn stdin_lines(out: Arc<Mutex<std::io::Stdout>>) -> std::sync::mpsc::Receiver<String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let heard = Arc::new(AtomicBool::new(false));
    if let Some(timeout) = startup_handshake_timeout() {
        let heard = heard.clone();
        std::thread::spawn(move || {
            std::thread::sleep(timeout);
            if !heard.load(Ordering::Relaxed) {
                eprintln!("[CodeGraph] no MCP traffic since startup; exiting");
                std::process::exit(0);
            }
        });
    }
    std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            heard.store(true, Ordering::Relaxed);
            // Newer clients probe with `server/discover` first; a server
            // without it answers "method not found" so they move on.
            if let Ok(msg) = serde_json::from_str::<Value>(&line) {
                if msg.get("method").and_then(Value::as_str) == Some("server/discover") {
                    if let Some(id) = msg.get("id") {
                        let reply = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "method not found: server/discover" } });
                        let mut o = out.lock().unwrap_or_else(|p| p.into_inner());
                        let _ = writeln!(o, "{reply}");
                        let _ = o.flush();
                    }
                    continue;
                }
            }
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

/// Run the stdio server rooted at `root` (or the client's root). With a
/// project root and watching on, sessions go through the project's shared
/// daemon unless `daemon` is false; if the daemon can't be reached the
/// server runs in-process.
pub fn serve_stdio(root: Option<&Path>, watch: bool, daemon: bool) -> std::io::Result<()> {
    exit_with_parent();
    let stdout = Arc::new(Mutex::new(std::io::stdout()));
    let lines = stdin_lines(stdout.clone());
    let default_root = root.and_then(find_root);
    if let (true, true, Some(r)) = (daemon && crate::daemon::enabled(), watch, &default_root) {
        match crate::daemon::connect_or_spawn(r) {
            Some(stream) => return crate::daemon::proxy(stream, lines, stdout),
            None => eprintln!("[CodeGraph] daemon unavailable; serving in-process"),
        }
    }
    let subprojects = match (&default_root, root) {
        (None, Some(base)) => {
            let found = scan_subprojects(base);
            match found.len() {
                0 => {}
                1 => eprintln!(
                    "[CodeGraph] using the indexed project {}",
                    found[0].display()
                ),
                n => eprintln!(
                    "[CodeGraph] {n} indexed projects under {}; calls must pass projectPath",
                    base.display()
                ),
            }
            found
        }
        _ => Vec::new(),
    };
    let mut server = Server::new(default_root, watch).with_subprojects(subprojects);
    start_telemetry_thread();
    let result = server.run_lines(lines.into_iter(), SharedOut(stdout));
    crate::telemetry::persist();
    result
}

/// Stdout shared with the stdin thread's direct replies.
struct SharedOut(Arc<Mutex<std::io::Stdout>>);

impl Write for SharedOut {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).write(buf)
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .write_all(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn banner_names_only_mentioned_pending_files() {
        let text = "## src/a.ts\n1\tx\n".to_string();
        assert_eq!(
            with_staleness_banner(text.clone(), &["src/b.ts".into()]),
            text
        );
        let out = with_staleness_banner(text, &["src/a.ts".into(), "src/b.ts".into()]);
        assert!(out.starts_with("⚠️") && out.contains("src/a.ts") && !out.contains("src/b.ts."));
    }

    #[test]
    fn handshake_and_unknown_project() {
        let mut s = Server::new(None, false);
        let init = s.handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2024-11-05"}})).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2024-11-05");
        assert!(s
            .handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .is_none());
        let call = s.handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "codegraph_explore", "arguments": {"query": "x"}}})).unwrap();
        // Guidance, never an error.
        assert!(call["result"].get("isError").is_none());
        assert!(call["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("projectPath"));
        let bad = s
            .handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "nope"}))
            .unwrap();
        assert_eq!(bad["error"]["code"], -32601);
    }

    fn indexed_project(dir: &Path) -> PathBuf {
        let root = dir.join("app");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.ts"), "export function hello() {}\n").unwrap();
        let mut cg = CodeGraph::init(&root).unwrap();
        cg.index_all(None).unwrap();
        root.canonicalize().unwrap()
    }

    #[test]
    fn asks_the_client_for_its_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = indexed_project(dir.path());
        let mut s = Server::new(None, false);
        s.handle(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"capabilities": {"roots": {}}}}));
        let tools = s
            .handle(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .unwrap();
        let required = &tools["result"]["tools"][0]["inputSchema"]["required"];
        assert!(required.as_array().unwrap().contains(&json!("projectPath")));
        s.handle(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        let request = s.outbox.pop().unwrap();
        assert_eq!(request["method"], "roots/list");
        let uri = format!("file://{}", root.display()).replace(' ', "%20");
        assert!(s
            .handle(
                &json!({"jsonrpc": "2.0", "id": request["id"], "result": {"roots": [{"uri": uri}]}})
            )
            .is_none());
        assert_eq!(s.default_root.as_deref(), Some(root.as_path()));
        let tools = s
            .handle(&json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}))
            .unwrap();
        assert!(tools["result"]["tools"][0]["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r != "projectPath"));
    }

    #[test]
    fn adopts_a_single_indexed_subproject() {
        let dir = tempfile::tempdir().unwrap();
        let root = indexed_project(dir.path());
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let found = scan_subprojects(dir.path());
        assert_eq!(
            found
                .iter()
                .map(|p| p.canonicalize().unwrap())
                .collect::<Vec<_>>(),
            vec![root.clone()]
        );
        let s = Server::new(None, false).with_subprojects(found);
        assert!(s.default_root.is_some());
        // Not a workspace: no scan.
        let bare = tempfile::tempdir().unwrap();
        assert!(scan_subprojects(bare.path()).is_empty());
    }

    #[test]
    fn nested_repositories_are_not_answered_by_the_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let root = indexed_project(dir.path());
        let nested = root.join("vendor-repo");
        std::fs::create_dir_all(nested.join(".git")).unwrap();
        let mut s = Server::new(Some(root.clone()), false);
        let out = s.call_tool(
            "codegraph_status",
            &json!({ "projectPath": nested.to_string_lossy() }),
        );
        let text = out["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("separate repository"), "{text}");
        let ok = s.call_tool(
            "codegraph_status",
            &json!({ "projectPath": root.join("src").to_string_lossy() }),
        );
        assert!(!ok["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("separate repository"));
    }

    #[test]
    fn uris_and_path_filters() {
        assert_eq!(
            file_uri_path("file:///home/me/my%20app").unwrap(),
            PathBuf::from("/home/me/my app")
        );
        assert_eq!(
            file_uri_path("file:///C:/work").unwrap(),
            PathBuf::from("C:/work")
        );
        assert!(file_uri_path("https://x").is_none());
        let root = Path::new("/p/proj");
        for whole in ["/", ".", "./", ""] {
            assert_eq!(normalize_dir_filter(whole, root), None, "{whole}");
        }
        assert_eq!(normalize_dir_filter("./src/", root).as_deref(), Some("src"));
        assert_eq!(
            normalize_dir_filter("src\\a", root).as_deref(),
            Some("src/a")
        );
        assert_eq!(
            normalize_dir_filter("/p/proj/src", root).as_deref(),
            Some("src")
        );
    }
}
