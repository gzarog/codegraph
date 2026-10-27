//! The viewer's read API: one request per screen, every capped list paired
//! with its true total.

use crate::project::CodeGraph;
use codegraph_core::{Edge, EdgeKind, Node, NodeKind, SearchOptions};
use codegraph_graph::{DEPENDENCY_EDGES, FLOW_EDGES};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Fan-in at which a symbol is a hub.
pub const HUB_THRESHOLD: u64 = 40;
/// Below this confidence an edge is a name-only guess, shown apart.
pub const UNCERTAIN_BELOW: f64 = 0.6;
const MAX_GROUPS: usize = 120;
const MAX_LINES_PER_GROUP: usize = 12;
const MAX_SOURCE_LINES: usize = 400;
const MAX_FILE_LINES: usize = 4000;
const MAX_MODULES: usize = 36;

pub type ApiResult = Result<Value, (u16, String)>;

fn internal(e: impl std::fmt::Display) -> (u16, String) {
    (500, e.to_string())
}

pub fn brief(n: &Node) -> Value {
    json!({
        "id": n.id,
        "name": n.name,
        "kind": n.kind.as_str(),
        "file": n.file_path,
        "line": n.start_line,
    })
}

fn full(n: &Node) -> Value {
    json!({
        "id": n.id,
        "name": n.name,
        "kind": n.kind.as_str(),
        "qualifiedName": n.qualified_name,
        "file": n.file_path,
        "language": n.language.as_str(),
        "startLine": n.start_line,
        "endLine": n.end_line,
        "signature": n.signature,
        "docstring": n.docstring,
        "exported": n.is_exported,
        "async": n.is_async,
        "decorators": n.decorators,
    })
}

fn confidence(e: &Edge) -> f64 {
    e.metadata
        .as_ref()
        .and_then(|m| m.get("confidence"))
        .and_then(Value::as_f64)
        .unwrap_or(1.0)
}

/// Source lines `start..=end` of a project file, plus whether the file
/// changed on disk since it was indexed.
fn read_lines(cg: &CodeGraph, path: &str) -> Option<(Vec<String>, bool)> {
    let full = crate::ui::resolve_project_file(cg.root(), path)?;
    let bytes = std::fs::read(full).ok()?;
    let stale = cg
        .store()
        .file(path)
        .ok()
        .flatten()
        .is_some_and(|r| r.content_hash != codegraph_index::content_hash(&bytes));
    let text = String::from_utf8_lossy(&bytes);
    Some((text.lines().map(str::to_string).collect(), stale))
}

pub fn stats(cg: &CodeGraph) -> ApiResult {
    let s = cg.status().map_err(internal)?;
    let pending = &s.pending;
    let hubs = cg
        .store()
        .most_referenced(DEPENDENCY_EDGES, 10)
        .map_err(internal)?;
    let routes = cg
        .store()
        .nodes_by_kind(NodeKind::Route)
        .map_err(internal)?;
    let name = cg
        .root()
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    Ok(json!({
        "project": name,
        "files": s.stats.file_count,
        "symbols": s.stats.node_count,
        "edges": s.stats.edge_count,
        "languages": s.stats.files_by_language.iter().map(|(l, n)| json!({ "name": l, "files": n })).collect::<Vec<_>>(),
        "frameworks": s.frameworks,
        "pending": {
            "added": pending.added.len(),
            "modified": pending.modified.len(),
            "removed": pending.removed.len(),
            "files": pending.added.iter().chain(&pending.modified).chain(&pending.removed).take(8).collect::<Vec<_>>(),
        },
        "hubs": hubs.iter().map(|(n, c)| { let mut b = brief(n); b["fanIn"] = (*c).into(); b }).collect::<Vec<_>>(),
        "routes": { "total": routes.len(), "shown": routes.iter().take(12).map(brief).collect::<Vec<_>>() },
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

pub fn search(cg: &CodeGraph, q: &str, limit: usize) -> ApiResult {
    if q.trim().is_empty() {
        return Ok(json!([]));
    }
    let hits = cg
        .store()
        .search_nodes(
            q,
            &SearchOptions {
                limit: Some(limit.clamp(1, 50)),
                ..Default::default()
            },
        )
        .map_err(internal)?;
    Ok(Value::Array(
        hits.iter()
            .filter(|h| h.node.kind != NodeKind::Import)
            .map(|h| {
                let mut b = brief(&h.node);
                b["qualifiedName"] = h.node.qualified_name.clone().into();
                b["signature"] = h.node.signature.clone().into();
                b
            })
            .collect(),
    ))
}

/// Edges grouped by the symbol on the other end, certain ones first.
fn groups(cg: &CodeGraph, edges: &[(Edge, String)]) -> Result<Value, (u16, String)> {
    let mut by: BTreeMap<String, Vec<&Edge>> = BTreeMap::new();
    for (e, other) in edges {
        by.entry(other.clone()).or_default().push(e);
    }
    let ids: Vec<String> = by.keys().cloned().collect();
    let nodes: HashMap<String, Node> = cg
        .store()
        .nodes_by_ids(&ids)
        .map_err(internal)?
        .into_iter()
        .collect();
    let mut out: Vec<(bool, String, Value)> = Vec::new();
    for (id, es) in &by {
        let Some(n) = nodes.get(id) else { continue };
        if n.kind == NodeKind::File && es.iter().all(|e| e.kind == EdgeKind::Contains) {
            continue;
        }
        let uncertain = es.iter().all(|e| confidence(e) < UNCERTAIN_BELOW);
        let mut lines: Vec<u32> = es.iter().filter_map(|e| e.line).collect();
        lines.sort_unstable();
        lines.dedup();
        let kinds: HashSet<&str> = es.iter().map(|e| e.kind.as_str()).collect();
        let dynamic = es.iter().find_map(|e| {
            let m = e.metadata.as_ref()?;
            (m.get("synthesizedBy")?.as_str()? == "dispatch")
                .then(|| json!({ "via": m.get("dispatch"), "key": m.get("key") }))
        });
        let mut b = brief(n);
        b["uncertain"] = uncertain.into();
        b["kinds"] = kinds.into_iter().collect::<Vec<_>>().into();
        b["count"] = es.len().into();
        b["lines"] = lines
            .iter()
            .take(MAX_LINES_PER_GROUP)
            .copied()
            .collect::<Vec<_>>()
            .into();
        b["dynamic"] = dynamic.unwrap_or(Value::Null);
        out.push((uncertain, format!("{}:{:08}", n.file_path, n.start_line), b));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let total = out.len();
    let uncertain = out.iter().filter(|o| o.0).count();
    Ok(json!({
        "total": total,
        "uncertain": uncertain,
        "groups": out.into_iter().take(MAX_GROUPS).map(|o| o.2).collect::<Vec<_>>(),
    }))
}

pub fn node(cg: &CodeGraph, id: &str) -> ApiResult {
    let store = cg.store();
    let n = store
        .node(id)
        .map_err(internal)?
        .ok_or((404, format!("no symbol {id}")))?;
    let graph = cg.graph();
    let flow: Vec<EdgeKind> = DEPENDENCY_EDGES.to_vec();
    let incoming: Vec<(Edge, String)> = store
        .incoming_edges(&n.id, &flow)
        .map_err(internal)?
        .into_iter()
        .filter(|e| e.source != n.id)
        .map(|e| {
            let o = e.source.clone();
            (e, o)
        })
        .collect();
    let outgoing: Vec<(Edge, String)> = store
        .outgoing_edges(&n.id, &flow)
        .map_err(internal)?
        .into_iter()
        .filter(|e| e.target != n.id)
        .map(|e| {
            let o = e.target.clone();
            (e, o)
        })
        .collect();
    let fan_in = incoming
        .iter()
        .map(|(e, _)| e.source.as_str())
        .collect::<HashSet<_>>()
        .len() as u64;
    let members: Vec<Value> =
        if n.kind.is_type() || matches!(n.kind, NodeKind::Module | NodeKind::Namespace) {
            graph
                .children(&n.id)
                .map_err(internal)?
                .iter()
                .filter(|c| !matches!(c.kind, NodeKind::Parameter | NodeKind::Import))
                .map(brief)
                .collect()
        } else {
            Vec::new()
        };
    let hierarchy = if n.kind.is_type() {
        let h = graph.type_hierarchy(&n.id, 30).map_err(internal)?;
        json!({
            "ancestors": h.ancestors.iter().map(brief).collect::<Vec<_>>(),
            "descendants": h.descendants.iter().map(brief).collect::<Vec<_>>(),
        })
    } else {
        Value::Null
    };
    let impact = graph.impact(&n.id, 3, 400).map_err(internal)?;
    let blast = json!({
        "levels": impact.levels.iter().map(Vec::len).collect::<Vec<_>>(),
        "total": impact.total(),
        "files": impact.files.len(),
        "sampleFiles": impact.files.iter().take(6).collect::<Vec<_>>(),
        "truncated": impact.truncated,
        "tests": impact.files.iter().filter(|f| codegraph_core::search::is_test_path(f)).take(6).collect::<Vec<_>>(),
    });
    let source = match read_lines(cg, &n.file_path) {
        Some((lines, stale)) => {
            let start = n.start_line.max(1) as usize;
            let end = (n.end_line as usize).max(start).min(lines.len());
            let shown_end = end.min(start + MAX_SOURCE_LINES - 1);
            json!({
                "startLine": start,
                "lines": lines.get(start - 1..shown_end).unwrap_or(&[]),
                "truncated": shown_end < end,
                "stale": stale,
            })
        }
        None => Value::Null,
    };
    let parent = store
        .incoming_edges(&n.id, &[EdgeKind::Contains])
        .map_err(internal)?
        .first()
        .and_then(|e| store.node(&e.source).ok().flatten())
        .filter(|p| p.kind != NodeKind::File)
        .map(|p| brief(&p));
    Ok(json!({
        "node": full(&n),
        "parent": parent,
        "fanIn": fan_in,
        "hub": fan_in >= HUB_THRESHOLD,
        "source": source,
        "callers": groups(cg, &incoming)?,
        "callees": groups(cg, &outgoing)?,
        "members": members,
        "hierarchy": hierarchy,
        "blast": blast,
    }))
}

pub fn file(cg: &CodeGraph, path: &str) -> ApiResult {
    let store = cg.store();
    let rec = store
        .file(path)
        .map_err(internal)?
        .ok_or((404, format!("{path} is not indexed")))?;
    let nodes = store.nodes_in_file(path).map_err(internal)?;
    // Nesting depth from the line ranges (types around their members).
    let mut outline = Vec::new();
    let mut stack: Vec<u32> = Vec::new();
    for n in nodes.iter().filter(|n| {
        !matches!(
            n.kind,
            NodeKind::File | NodeKind::Parameter | NodeKind::Import | NodeKind::Export
        )
    }) {
        while stack.last().is_some_and(|&end| n.start_line > end) {
            stack.pop();
        }
        let mut b = brief(n);
        b["depth"] = stack.len().into();
        b["endLine"] = n.end_line.into();
        outline.push(b);
        if n.kind.is_type() || matches!(n.kind, NodeKind::Module | NodeKind::Namespace) {
            stack.push(n.end_line);
        }
    }
    let (lines, stale) = read_lines(cg, path).unwrap_or_default();
    let total_lines = lines.len();
    Ok(json!({
        "file": {
            "path": rec.path,
            "language": rec.language.as_str(),
            "lines": total_lines,
            "generated": rec.generated,
            "stale": stale,
            "errors": rec.errors.iter().map(|e| e.message.clone()).take(5).collect::<Vec<_>>(),
        },
        "outline": outline,
        "source": lines.into_iter().take(MAX_FILE_LINES).collect::<Vec<_>>(),
        "truncated": total_lines > MAX_FILE_LINES,
        "calls": super::flows::file_calls(cg, path)?,
        "imports": store.dependency_files(path).map_err(internal)?,
        "dependents": store.dependent_files(path).map_err(internal)?,
    }))
}

/// Directory prefix with `depth` segments (the file's directory when shallower).
fn module_of(path: &str, depth: usize) -> String {
    let dirs: Vec<&str> = path.split('/').collect();
    let dirs = &dirs[..dirs.len().saturating_sub(1)];
    if dirs.is_empty() {
        return ".".into();
    }
    dirs[..dirs.len().min(depth)].join("/")
}

/// The module graph: directories (at the depth that gives a readable count)
/// and the dependency weight between them.
pub fn map(cg: &CodeGraph) -> ApiResult {
    let store = cg.store();
    let files = store.all_files().map_err(internal)?;
    // Deepen while it adds detail and stays readable.
    let mut depth = 1;
    let count = |d: usize| {
        files
            .iter()
            .map(|f| module_of(&f.path, d))
            .collect::<HashSet<_>>()
            .len()
    };
    while depth < 6 {
        let (now, next) = (count(depth), count(depth + 1));
        if next > MAX_MODULES || next == now || now >= 12 {
            break;
        }
        depth += 1;
    }
    let mut modules: BTreeMap<String, (usize, u64, Vec<String>)> = BTreeMap::new();
    for f in &files {
        let m = modules.entry(module_of(&f.path, depth)).or_default();
        m.0 += 1;
        m.1 += u64::from(f.node_count);
        if m.2.len() < 40 {
            m.2.push(f.path.clone());
        }
    }
    // Keep the largest modules; fold the rest into "(other)".
    let mut ranked: Vec<(&String, usize)> = modules.iter().map(|(k, v)| (k, v.0)).collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
    let kept: HashSet<String> = ranked
        .iter()
        .take(MAX_MODULES)
        .map(|(k, _)| (*k).clone())
        .collect();
    let key = |p: &str| {
        let m = module_of(p, depth);
        if kept.contains(&m) {
            m
        } else {
            "(other)".to_string()
        }
    };
    let mut weights: HashMap<(String, String), u64> = HashMap::new();
    for (a, b, n) in store.file_dependency_counts().map_err(internal)? {
        let (ma, mb) = (key(&a), key(&b));
        if ma != mb {
            *weights.entry((ma, mb)).or_default() += n;
        }
    }
    let mut out_modules: BTreeMap<String, Value> = BTreeMap::new();
    for (k, (nfiles, nsyms, sample)) in modules {
        let id = if kept.contains(&k) {
            k
        } else {
            "(other)".into()
        };
        let e = out_modules
            .entry(id.clone())
            .or_insert_with(|| json!({ "id": id, "files": 0, "symbols": 0, "sample": [] }));
        e["files"] = (e["files"].as_u64().unwrap_or(0) + nfiles as u64).into();
        e["symbols"] = (e["symbols"].as_u64().unwrap_or(0) + nsyms).into();
        if let Some(arr) = e["sample"].as_array_mut() {
            for p in sample {
                if arr.len() < 40 {
                    arr.push(p.into());
                }
            }
        }
    }
    let mut edges: Vec<Value> = weights
        .into_iter()
        .map(|((a, b), w)| json!({ "from": a, "to": b, "weight": w }))
        .collect();
    edges.sort_by(|a, b| b["weight"].as_u64().cmp(&a["weight"].as_u64()));
    edges.truncate(160);
    Ok(json!({
        "depth": depth,
        "modules": out_modules.into_values().collect::<Vec<_>>(),
        "edges": edges,
    }))
}

pub fn flow(cg: &CodeGraph, from: &str, to: &str) -> ApiResult {
    let graph = cg.graph();
    let path = match graph
        .find_path(from, to, FLOW_EDGES, 8, false)
        .map_err(internal)?
    {
        Some(p) => Some((p, false)),
        None => graph
            .find_path(to, from, FLOW_EDGES, 8, false)
            .map_err(internal)?
            .map(|p| (p, true)),
    };
    Ok(match path {
        Some((steps, reversed)) => json!({
            "found": true,
            "reversed": reversed,
            "steps": steps.iter().map(|s| json!({
                "node": brief(&s.node),
                "via": s.via.as_ref().map(|e| e.kind.as_str()),
                "line": s.via.as_ref().and_then(|e| e.line),
            })).collect::<Vec<_>>(),
        }),
        None => json!({ "found": false, "steps": [] }),
    })
}

pub fn dead_code(cg: &CodeGraph) -> ApiResult {
    let nodes = cg.graph().dead_code(500).map_err(internal)?;
    let mut by: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for n in &nodes {
        by.entry(n.file_path.as_str()).or_default().push(brief(n));
    }
    Ok(json!({
        "total": nodes.len(),
        "files": by.into_iter().map(|(f, s)| json!({ "file": f, "symbols": s })).collect::<Vec<_>>(),
    }))
}

pub fn routes(cg: &CodeGraph) -> ApiResult {
    let store = cg.store();
    let routes = store.nodes_by_kind(NodeKind::Route).map_err(internal)?;
    let mut out = Vec::new();
    for r in routes.iter().take(500) {
        let handlers: Vec<Value> = store
            .outgoing_edges(&r.id, &[EdgeKind::References, EdgeKind::Calls])
            .map_err(internal)?
            .iter()
            .filter_map(|e| store.node(&e.target).ok().flatten())
            .map(|n| brief(&n))
            .collect();
        let callers = store
            .incoming_edges(&r.id, &[EdgeKind::Calls])
            .map_err(internal)?
            .len();
        let mut b = brief(r);
        b["framework"] = r.decorators.first().cloned().into();
        b["handlers"] = handlers.into();
        b["requests"] = callers.into();
        out.push(b);
    }
    Ok(json!({ "total": routes.len(), "routes": out }))
}
