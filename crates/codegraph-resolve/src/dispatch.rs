//! Joins for dynamic dispatch: a `fire:` reference reaches every handler
//! bound to the same key, an `http:` reference reaches the routes serving
//! the requested path. See `codegraph_frameworks::dispatch` for extraction.

use crate::symbols::{SymId, Symbols};
use codegraph_core::{Edge, NodeKind, Provenance, UnresolvedReference};
use codegraph_frameworks::dispatch::{
    route_matches, BIND_PREFIX, FIRE_PREFIX, HTTP_PREFIX, NAV_PREFIX,
};
use std::collections::HashMap;

/// Most targets one dispatch site links to; beyond that the key is too
/// generic to be informative.
const MAX_TARGETS: usize = 12;

pub fn is_join(name: &str) -> bool {
    name.starts_with(FIRE_PREFIX) || name.starts_with(HTTP_PREFIX) || name.starts_with(NAV_PREFIX)
}

/// The handler a `bind:` reference registers.
pub fn bind_handler(r: &UnresolvedReference) -> Option<&str> {
    if r.reference_name.starts_with(BIND_PREFIX) {
        r.candidates.first().map(String::as_str)
    } else {
        None
    }
}

/// Tag a resolved bind's edge with its channel and key.
pub fn mark_bind(edge: &mut Edge, name: &str) {
    let key = &name[BIND_PREFIX.len()..];
    let meta = edge.metadata.get_or_insert_with(|| serde_json::json!({}));
    meta["dispatch"] = "bind".into();
    meta["key"] = key.into();
    edge.provenance = Some(Provenance::Heuristic);
}

/// Lifecycle events every stream, socket and process emits: a fire of one
/// only reaches listeners bound in the same file, not every `on('error')`
/// in the project.
const GENERIC_EVENTS: &[&str] = &[
    "event:error",
    "event:close",
    "event:end",
    "event:data",
    "event:exit",
    "event:message",
    "event:open",
    "event:connect",
    "event:connection",
    "event:ready",
    "event:finish",
    "event:drain",
    "event:change",
    "event:click",
    "event:load",
    "event:input",
    "event:submit",
    "event:disconnect",
    "event:timeout",
    "event:response",
    "event:request",
];

/// Bound handlers by `channel:key`: `(target id, file of the bind)`.
pub struct Binds(HashMap<String, Vec<(String, String)>>);

impl Binds {
    pub fn new(resolved: Vec<(String, String, String)>) -> Self {
        let mut map: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (name, target, file) in resolved {
            let Some(key) = name.strip_prefix(BIND_PREFIX) else {
                continue;
            };
            let v = map.entry(key.to_string()).or_default();
            if !v.iter().any(|(t, f)| *t == target && *f == file) {
                v.push((target, file));
            }
        }
        Binds(map)
    }
}

struct Route {
    id: SymId,
    methods: Vec<String>,
    path: String,
}

/// The project's route nodes, parsed into methods and path.
pub struct Routes(Vec<Route>);

impl Routes {
    pub fn new(syms: &Symbols) -> Self {
        let mut out = Vec::new();
        for (id, s) in syms.syms.iter().enumerate() {
            if s.kind != NodeKind::Route {
                continue;
            }
            let (methods, path) = split_route(&s.name);
            out.push(Route { id, methods, path });
        }
        Routes(out)
    }
}

/// `GET|POST /users/:id` → (["GET", "POST"], "/users/:id"); a bare path has no methods.
fn split_route(name: &str) -> (Vec<String>, String) {
    let name = name.trim();
    if let Some((head, rest)) = name.split_once(char::is_whitespace) {
        if !head.is_empty()
            && head
                .chars()
                .all(|c| c.is_ascii_uppercase() || matches!(c, '|' | ','))
        {
            let methods = head
                .split(['|', ','])
                .filter(|m| !m.is_empty())
                .map(str::to_string)
                .collect();
            return (methods, normalize_route(rest.trim()));
        }
    }
    (Vec::new(), normalize_route(name))
}

fn normalize_route(path: &str) -> String {
    let p = path.trim_end_matches('/');
    if p.starts_with('/') {
        p.to_string()
    } else {
        format!("/{p}")
    }
}

fn accepts(methods: &[String], method: &str) -> bool {
    methods
        .iter()
        .any(|m| m == method || m == "ANY" || m == "ALL" || m == "MATCH")
}

/// What a join reference reaches.
pub fn targets(
    r: &UnresolvedReference,
    binds: &Binds,
    routes: &Routes,
    syms: &Symbols,
) -> Vec<SymId> {
    let from = syms.by_id(&r.from_node_id);
    let mut out: Vec<SymId> = Vec::new();
    if let Some(key) = r.reference_name.strip_prefix(FIRE_PREFIX) {
        let local_only = GENERIC_EVENTS.contains(&key);
        for (id, file) in binds.0.get(key).into_iter().flatten() {
            if local_only && r.file_path.as_deref() != Some(file.as_str()) {
                continue;
            }
            if let Some(t) = syms.by_id(id) {
                if Some(t) != from && !out.contains(&t) {
                    out.push(t);
                }
            }
        }
    } else if let Some(req) = r.reference_name.strip_prefix(HTTP_PREFIX) {
        let (method, path) = req.split_once(' ').unwrap_or(("GET", req));
        out = route_targets(routes, path, method, false);
    } else if let Some(path) = r.reference_name.strip_prefix(NAV_PREFIX) {
        out = route_targets(routes, path, "GET", true);
    }
    out.truncate(MAX_TARGETS);
    out
}

/// Routes serving `path`: those declaring `method` (or method-less page
/// routes when none do, or first when `pages_first`), most specific first.
fn route_targets(routes: &Routes, path: &str, method: &str, pages_first: bool) -> Vec<SymId> {
    let served: Vec<&Route> = routes
        .0
        .iter()
        .filter(|rt| route_matches(&rt.path, path))
        .collect();
    let pages: Vec<SymId> = served
        .iter()
        .filter(|rt| rt.methods.is_empty())
        .map(|rt| rt.id)
        .collect();
    let exact: Vec<SymId> = served
        .iter()
        .filter(|rt| accepts(&rt.methods, method))
        .map(|rt| rt.id)
        .collect();
    let mut out = match (pages_first, pages.is_empty(), exact.is_empty()) {
        (true, false, _) | (false, false, true) => pages,
        _ => exact,
    };
    // A literal segment beats a parameter: keep the most specific routes.
    if out.len() > 1 {
        let literal = |id: &SymId| {
            routes
                .0
                .iter()
                .find(|rt| rt.id == *id)
                .map(|rt| {
                    rt.path
                        .split('/')
                        .filter(|s| !s.is_empty() && !is_param(s))
                        .count()
                })
                .unwrap_or(0)
        };
        let best = out.iter().map(literal).max().unwrap_or(0);
        out.retain(|id| literal(id) == best);
    }
    out
}

fn is_param(seg: &str) -> bool {
    seg.starts_with([':', '{', '[', '<', '$', '*'])
}

/// The edge a join reference makes to `target`.
pub fn edge(r: &UnresolvedReference, syms: &Symbols, target: SymId) -> (Edge, String) {
    let t = syms.get(target);
    let mut edge = Edge::new(&r.from_node_id, &t.id, r.reference_kind.edge_kind());
    edge.line = Some(r.line);
    edge.column = Some(r.column);
    let (via, key) = match (
        r.reference_name.strip_prefix(HTTP_PREFIX),
        r.reference_name.strip_prefix(NAV_PREFIX),
    ) {
        (Some(req), _) => ("http", req),
        (_, Some(path)) => ("nav", path),
        _ => {
            let key = &r.reference_name[FIRE_PREFIX.len()..];
            (key.split(':').next().unwrap_or("event"), key)
        }
    };
    edge.metadata = Some(serde_json::json!({
        "synthesizedBy": "dispatch",
        "dispatch": via,
        "key": key,
        "resolvedBy": "dispatch",
        "confidence": 0.6,
    }));
    edge.provenance = Some(Provenance::Heuristic);
    (edge, t.file.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_route_names() {
        assert_eq!(
            split_route("GET /users/:id"),
            (vec!["GET".into()], "/users/:id".into())
        );
        assert_eq!(
            split_route("GET|POST /a/"),
            (vec!["GET".into(), "POST".into()], "/a".into())
        );
        assert_eq!(split_route("home/"), (vec![], "/home".into()));
    }
}
