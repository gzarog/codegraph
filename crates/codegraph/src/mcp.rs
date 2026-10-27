//! MCP server over stdio (JSON-RPC 2.0, one message per line).

use crate::format;
use crate::project::{find_root, CodeGraph};
use codegraph_core::{NodeKind, SearchOptions};
use codegraph_graph::{DEPENDENCY_EDGES, FLOW_EDGES};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

pub struct Server {
    default_root: Option<PathBuf>,
    projects: HashMap<PathBuf, Arc<Mutex<CodeGraph>>>,
    tools: Vec<String>,
    watch: bool,
    watchers: Vec<crate::watch::Watcher>,
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
        Server {
            default_root,
            projects: HashMap::new(),
            tools: enabled_tools(),
            watch,
            watchers: Vec::new(),
        }
    }

    fn project(&mut self, project_path: Option<&str>) -> Result<Arc<Mutex<CodeGraph>>, String> {
        let start = match project_path {
            Some(p) => PathBuf::from(p),
            None => self.default_root.clone().ok_or_else(|| {
                "This server has no default project (no .codegraph/ at its root). Pass `projectPath` to a project that has one.".to_string()
            })?,
        };
        let root = find_root(&start).ok_or_else(|| {
            format!("{} is not indexed (no .codegraph/ found). Use your built-in tools for it; the user can run `codegraph init` there.", start.display())
        })?;
        if let Some(cg) = self.projects.get(&root) {
            return Ok(cg.clone());
        }
        let mut cg = CodeGraph::open(&root).map_err(|e| e.to_string())?;
        // Catch up with edits made while no server was watching.
        let _ = cg.sync(None);
        let cg = Arc::new(Mutex::new(cg));
        if self.watch {
            if let Ok(w) = crate::watch::watch(cg.clone(), |_| {}) {
                self.watchers.push(w);
            }
        }
        self.projects.insert(root, cg.clone());
        Ok(cg)
    }

    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => {
                let requested = params
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or(PROTOCOL_VERSIONS[0]);
                let version = if PROTOCOL_VERSIONS.contains(&requested) {
                    requested
                } else {
                    PROTOCOL_VERSIONS[0]
                };
                if self.default_root.is_none() {
                    if let Some(uri) = params.get("rootUri").and_then(|u| u.as_str()) {
                        self.default_root = uri
                            .strip_prefix("file://")
                            .map(PathBuf::from)
                            .and_then(|p| find_root(&p));
                    }
                }
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": { "name": "codegraph", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": SERVER_INSTRUCTIONS,
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => {
                let tools: Vec<Value> = tool_definitions()
                    .into_iter()
                    .filter(|t| self.tools.iter().any(|n| t["name"] == n.as_str()))
                    .collect();
                Ok(json!({ "tools": tools }))
            }
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                if !tool_definitions().iter().any(|t| t["name"] == name) {
                    Err((-32602, format!("unknown tool: {name}")))
                } else {
                    Ok(self.call_tool(name, &args))
                }
            }
            "resources/list" => Ok(json!({ "resources": [] })),
            "prompts/list" => Ok(json!({ "prompts": [] })),
            m if m.starts_with("notifications/") => return None,
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
        let cg = match self.project(arg_str(args, "projectPath")) {
            Ok(cg) => cg,
            Err(e) => return error_result(e),
        };
        let guard = match cg.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        match run_tool(&guard, name, args) {
            Ok(text) => text_result(text),
            Err(e) => error_result(e),
        }
    }

    /// Serve until stdin closes.
    pub fn run(&mut self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(Value::Array(batch)) => {
                    let replies: Vec<Value> = batch.iter().filter_map(|m| self.handle(m)).collect();
                    (!replies.is_empty()).then(|| Value::Array(replies))
                }
                Ok(msg) => self.handle(&msg),
                Err(e) => Some(
                    json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": format!("parse error: {e}") } }),
                ),
            };
            if let Some(r) = reply {
                writeln!(output, "{r}")?;
                output.flush()?;
            }
        }
        Ok(())
    }
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
        "codegraph_status" => Ok(format::status(&cg.status().map_err(e)?)),
        "codegraph_files" => {
            let files = cg.store().all_files().map_err(se)?;
            let format = arg_str(args, "format")
                .unwrap_or("tree")
                .parse()
                .map_err(|err: String| err)?;
            Ok(format::files(
                &files,
                arg_str(args, "path"),
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

/// Run the stdio server rooted at `root` (or the client's root).
pub fn serve_stdio(root: Option<&Path>, watch: bool) -> std::io::Result<()> {
    let default_root = root.and_then(find_root);
    let mut server = Server::new(default_root, watch);
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    server.run(stdin.lock(), stdout.lock())
}
