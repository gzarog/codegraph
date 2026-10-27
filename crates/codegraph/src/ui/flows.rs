//! The viewer's flow screens: where to start reading (entry points), how
//! screens lead to each other (screens), and what one symbol sets in motion
//! (steps). Everything is read from the graph at request time; the "when"
//! labels come from the source as it stands.

use super::api::{brief, ApiResult};
use crate::project::CodeGraph;
use codegraph_core::search::is_test_path;
use codegraph_core::{Edge, EdgeKind, Language, Node, NodeKind};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::LazyLock;

fn internal(e: impl std::fmt::Display) -> (u16, String) {
    (500, e.to_string())
}

// ------------------------------------------------------------------ guards

static INLINE_GUARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:\{|^|\s)([A-Za-z_$!][\w$.!()\[\]'\x22 =<>|-]{0,60}?)\s*(?:&&|\?)\s*[<(\w]")
        .unwrap()
});
static BLOCK_GUARD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:\}\s*)?(?:else\s+)?(if|elif|elsif|unless|while|when|case|guard)\b\s*\(?(.*?)\)?\s*(?:\{|:|then|do)?\s*$").unwrap()
});

static FUNCTION_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:function|def|fn|func|fun|sub|class|impl)\b|=>\s*\{?\s*$|^\s*(?:public|private|protected|static|async|export)\b").unwrap()
});

/// Lines of project files, read once per request.
struct Sources<'a> {
    cg: &'a CodeGraph,
    files: HashMap<String, Vec<String>>,
}

impl<'a> Sources<'a> {
    fn new(cg: &'a CodeGraph) -> Self {
        Sources {
            cg,
            files: HashMap::new(),
        }
    }

    fn lines(&mut self, file: &str) -> &[String] {
        let root = self.cg.root().to_path_buf();
        self.files.entry(file.to_string()).or_insert_with(|| {
            super::resolve_project_file(&root, file)
                .and_then(|p| std::fs::read_to_string(p).ok())
                .map(|t| t.lines().map(str::to_string).collect())
                .unwrap_or_default()
        })
    }

    /// The condition guarding `line` of `file`, if one is written near it:
    /// an inline `cond && …` / `cond ? …`, or the nearest enclosing
    /// `if`/`case`/`when` above it.
    fn guard(&mut self, file: &str, line: u32) -> Option<String> {
        let lines = self.lines(file);
        let idx = (line as usize).checked_sub(1)?;
        let here = lines.get(idx)?;
        if let Some(c) = INLINE_GUARD.captures(here) {
            let cond = c[1].trim().trim_start_matches(['{', '(']).trim();
            if !cond.is_empty() && !cond.starts_with("return") {
                return Some(shorten(cond));
            }
        }
        let indent = |s: &str| s.len() - s.trim_start().len();
        let mut limit = indent(here);
        for prev in lines[..idx].iter().rev().take(30) {
            if prev.trim().is_empty() {
                continue;
            }
            let ind = indent(prev);
            if ind >= limit {
                continue;
            }
            limit = ind;
            if let Some(c) = BLOCK_GUARD.captures(prev) {
                let cond = c[2].trim().trim_end_matches(['{', ':']).trim();
                return Some(match &c[1] {
                    "case" | "when" => format!("case {}", shorten(cond)),
                    "unless" => format!("not {}", shorten(cond)),
                    _ if cond.is_empty() => return None,
                    _ => shorten(cond),
                });
            }
            if prev.trim_start().starts_with("} else") || prev.trim() == "else:" {
                return Some("otherwise".into());
            }
            // The enclosing function: nothing guards the line inside it.
            if FUNCTION_HEADER.is_match(prev) {
                return None;
            }
        }
        None
    }
}

fn shorten(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(59).collect::<String>())
    } else {
        s
    }
}

// ------------------------------------------------------------ entry points

pub fn entrypoints(cg: &CodeGraph) -> ApiResult {
    let store = cg.store();
    let routes = store.nodes_by_kind(NodeKind::Route).map_err(internal)?;
    let runners: Vec<Value> = store
        .file_call_roots(40)
        .map_err(internal)?
        .into_iter()
        .filter(|(f, ..)| !is_test_path(f))
        .take(12)
        .map(|(f, calls, files)| json!({ "file": f, "calls": calls, "files": files }))
        .collect();
    let mut reach: HashMap<String, HashSet<String>> = HashMap::new();
    for (a, b, _) in store.file_dependency_counts().map_err(internal)? {
        if is_test_path(&a) {
            reach.entry(a).or_default().insert(b);
        }
    }
    let mut tests: Vec<(String, usize)> = reach.into_iter().map(|(f, s)| (f, s.len())).collect();
    tests.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let hubs = store
        .most_referenced(codegraph_graph::DEPENDENCY_EDGES, 30)
        .map_err(internal)?;
    Ok(json!({
        "routes": { "total": routes.len(), "shown": routes.iter().take(30).map(brief).collect::<Vec<_>>() },
        "runners": runners,
        "tests": tests.iter().take(12).map(|(f, n)| json!({ "file": f, "files": n })).collect::<Vec<_>>(),
        "hubs": hubs.iter().filter(|(n, _)| !is_test_path(&n.file_path)).take(12)
            .map(|(n, c)| { let mut b = brief(n); b["fanIn"] = (*c).into(); b }).collect::<Vec<_>>(),
    }))
}

// ----------------------------------------------------------------- screens

const SCREEN_HOPS: usize = 6;
const MAX_SCREENS: usize = 80;

/// Edges that carry control into a node (renders are calls/instantiations).
const INTO: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Instantiates,
    EdgeKind::References,
];

fn route_renders(
    cg: &CodeGraph,
    routes: &[Node],
) -> Result<HashMap<String, Vec<String>>, (u16, String)> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();
    for r in routes {
        map.entry(r.id.clone()).or_default().push(r.id.clone());
        for e in cg
            .store()
            .outgoing_edges(&r.id, &[EdgeKind::References, EdgeKind::Calls])
            .map_err(internal)?
        {
            map.entry(e.target).or_default().push(r.id.clone());
        }
    }
    Ok(map)
}

/// Walk back from `start` to the screens that reach it: `(screen ids,
/// via chain from the screen's component down to start, call edges on it)`.
#[allow(clippy::type_complexity)]
fn back_to_screen(
    cg: &CodeGraph,
    start: &str,
    renders: &HashMap<String, Vec<String>>,
) -> Result<Option<(Vec<String>, Vec<String>, Vec<Edge>)>, (u16, String)> {
    let mut seen: HashSet<String> = HashSet::from([start.to_string()]);
    let mut queue: VecDeque<(String, Vec<String>, Vec<Edge>)> =
        VecDeque::from([(start.to_string(), vec![start.to_string()], Vec::new())]);
    while let Some((id, path, edges)) = queue.pop_front() {
        if let Some(screens) = renders.get(&id) {
            return Ok(Some((screens.clone(), path, edges)));
        }
        if path.len() > SCREEN_HOPS {
            continue;
        }
        for e in cg
            .store()
            .incoming_edges(&id, INTO)
            .map_err(internal)?
            .into_iter()
            .take(25)
        {
            if seen.insert(e.source.clone()) {
                let mut p = vec![e.source.clone()];
                p.extend(path.iter().cloned());
                let mut es = vec![e.clone()];
                es.extend(edges.iter().cloned());
                queue.push_back((e.source.clone(), p, es));
            }
        }
    }
    Ok(None)
}

pub fn screens(cg: &CodeGraph) -> ApiResult {
    let store = cg.store();
    let routes = store.nodes_by_kind(NodeKind::Route).map_err(internal)?;
    let renders = route_renders(cg, &routes)?;
    let mut src = Sources::new(cg);
    let mut links: Vec<Value> = Vec::new();
    let mut origins: Vec<Value> = Vec::new();
    let mut names: HashMap<String, Node> = HashMap::new();
    let mut name_of = |id: &str| -> Option<Node> {
        if let Some(n) = names.get(id) {
            return Some(n.clone());
        }
        let n = store.node(id).ok().flatten()?;
        names.insert(id.to_string(), n.clone());
        Some(n)
    };
    let mut degree: HashMap<String, (u64, u64)> = HashMap::new();
    let mut seen_links: HashSet<(String, String, String)> = HashSet::new();
    for nav in store.edges_of_kind(EdgeKind::Navigates).map_err(internal)? {
        let Some(site_node) = name_of(&nav.source) else {
            continue;
        };
        let mut when: Vec<String> = Vec::new();
        if let Some(g) = nav.line.and_then(|l| src.guard(&site_node.file_path, l)) {
            when.push(g);
        }
        let site = json!({ "file": site_node.file_path, "line": nav.line.unwrap_or(site_node.start_line) });
        match back_to_screen(cg, &nav.source, &renders)? {
            Some((screens, path, edges)) => {
                for e in &edges {
                    let Some(caller) = name_of(&e.source) else {
                        continue;
                    };
                    if let Some(g) = e.line.and_then(|l| src.guard(&caller.file_path, l)) {
                        if !when.contains(&g) {
                            when.insert(0, g);
                        }
                    }
                }
                let via: Vec<String> = path
                    .iter()
                    .filter(|id| !renders.contains_key(*id))
                    .filter_map(|id| name_of(id).map(|n| n.name))
                    .collect();
                for from in screens.iter().take(3) {
                    if !seen_links.insert((from.clone(), nav.target.clone(), via.join(">"))) {
                        continue;
                    }
                    degree.entry(from.clone()).or_default().1 += 1;
                    degree.entry(nav.target.clone()).or_default().0 += 1;
                    links.push(json!({ "from": from, "to": nav.target, "via": via, "when": when, "site": site }));
                }
            }
            None => {
                degree.entry(nav.target.clone()).or_default().0 += 1;
                origins.push(json!({ "origin": brief(&site_node), "to": nav.target, "when": when, "site": site }));
            }
        }
    }
    // Screens: page routes and anything a link touches.
    let involved: HashSet<&str> = links
        .iter()
        .flat_map(|l| {
            [
                l["from"].as_str().unwrap_or(""),
                l["to"].as_str().unwrap_or(""),
            ]
        })
        .chain(origins.iter().map(|o| o["to"].as_str().unwrap_or("")))
        .collect();
    let mut screens: Vec<Value> = Vec::new();
    for r in &routes {
        let is_page = !r
            .name
            .split_whitespace()
            .next()
            .is_some_and(|w| w.chars().all(|c| c.is_ascii_uppercase() || c == '|'));
        if !is_page && !involved.contains(r.id.as_str()) {
            continue;
        }
        let component = store
            .outgoing_edges(&r.id, &[EdgeKind::References])
            .map_err(internal)?
            .first()
            .and_then(|e| store.node(&e.target).ok().flatten())
            .map(|n| brief(&n));
        let (inc, out) = degree.get(&r.id).copied().unwrap_or_default();
        screens.push(json!({
            "id": r.id, "path": r.name, "file": r.file_path, "line": r.start_line,
            "component": component, "incoming": inc, "outgoing": out,
        }));
        if screens.len() >= MAX_SCREENS {
            break;
        }
    }
    Ok(json!({ "screens": screens, "links": links, "origins": origins }))
}

// ------------------------------------------------------------------- steps

const MAX_STEPS: usize = 40;
const MAX_STEP_DEPTH: usize = 4;
const MAX_FANOUT: usize = 10;
const MAX_FOLD: usize = 12;

static EFFECT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:fetch|\$fetch|axios|ky|got|superagent|requests|httpx|http|https|client|api)(?:\.\w+)?$|(?i)(?:^|\.)(?:localStorage|sessionStorage|AsyncStorage|SecureStore|indexedDB|cookies?)\b|(?:^|\.)(?:writeFile|readFile|unlink|mkdir|open|exec|spawn|query|execute|sendBeacon|postMessage|track|capture|logEvent)$").unwrap()
});

fn effect_kind(name: &str) -> &'static str {
    let n = name.to_lowercase();
    if n.contains("storage")
        || n.contains("cookie")
        || n.contains("indexeddb")
        || n.contains("securestore")
    {
        "storage"
    } else if n.contains("file")
        || n.contains("mkdir")
        || n.contains("unlink")
        || n.ends_with(".open")
        || n == "open"
    {
        "filesystem"
    } else if n.contains("query") || n.contains("execute") {
        "database"
    } else if n.contains("track")
        || n.contains("capture")
        || n.contains("logevent")
        || n.contains("beacon")
    {
        "telemetry"
    } else if n.contains("exec") || n.contains("spawn") {
        "process"
    } else {
        "network"
    }
}

fn is_store_file(path: &str) -> bool {
    let p = path.to_lowercase();
    let mut segs: Vec<&str> = p.split('/').collect();
    let file = segs.pop().unwrap_or("");
    let stem = file.split('.').next().unwrap_or("");
    segs.iter().any(|d| {
        matches!(
            *d,
            "store" | "stores" | "state" | "redux" | "slices" | "reducers"
        )
    }) || ["store", "slice", "reducer"]
        .iter()
        .any(|k| stem.ends_with(k))
}

/// A function handed over as a handler, not just as a value (`.map(f)`).
static HANDLER_SITE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bon[A-Z]\w*\s*[=:]|addEventListener|addListener|\.on\(|\.once\(|subscribe\(|setTimeout|setInterval|requestAnimationFrame|\.then\(|@\w*(?:click|submit|change|input)|on:\w+").unwrap()
});

fn family(l: Language) -> u8 {
    use Language::*;
    match l {
        TypeScript | JavaScript | Tsx | Jsx | Svelte | Vue | Astro | ArkTs => 1,
        Swift | ObjC => 2,
        Kotlin | Java => 3,
        C | Cpp => 4,
        other => 100 + other as u8,
    }
}

/// What a forward edge from `u` to `v` is, as a step (None: fold it).
/// `site` is the source line the edge comes from.
fn step_kind(u: &Node, v: &Node, e: &Edge, site: &str) -> Option<&'static str> {
    let dispatch = e
        .metadata
        .as_ref()
        .and_then(|m| m.get("dispatch"))
        .and_then(Value::as_str);
    if v.kind == NodeKind::Route {
        return Some(if dispatch == Some("http") {
            "request"
        } else {
            "screen"
        });
    }
    match dispatch {
        Some("event") => return Some("event"),
        Some("fnptr" | "object-registry") => return Some("trigger"),
        Some(
            "celery" | "sidekiq" | "spring-event" | "laravel-event" | "mediatr" | "queue" | "bus",
        ) => return Some("event"),
        Some("pinia" | "vuex" | "redux-thunk" | "rtk-query") => return Some("store"),
        _ => {}
    }
    let function_ref = e
        .metadata
        .as_ref()
        .and_then(|m| m.get("functionRef"))
        .is_some_and(|f| f == true);
    if function_ref && v.kind.is_callable() && HANDLER_SITE.is_match(site) {
        return Some("trigger");
    }
    if family(u.language) != family(v.language) {
        return Some("bridge");
    }
    if is_store_file(&v.file_path) && !is_store_file(&u.file_path) {
        return Some("store");
    }
    None
}

pub fn steps(cg: &CodeGraph, id: &str) -> ApiResult {
    let store = cg.store();
    let anchor = store
        .node(id)
        .map_err(internal)?
        .ok_or((404, format!("no symbol {id}")))?;
    let mut src = Sources::new(cg);
    let mut steps: Vec<Value> =
        vec![json!({ "id": "s0", "kind": "anchor", "node": brief(&anchor), "depth": 0 })];
    let mut links: Vec<Value> = Vec::new();
    let mut capped: Vec<String> = Vec::new();
    let mut step_of: HashMap<String, String> =
        HashMap::from([(anchor.id.clone(), "s0".to_string())]);
    // (step id, node, depth)
    let mut frontier: VecDeque<(String, Node, usize)> =
        VecDeque::from([("s0".into(), anchor.clone(), 0)]);
    let forward = [
        EdgeKind::Calls,
        EdgeKind::Navigates,
        EdgeKind::References,
        EdgeKind::Instantiates,
    ];
    while let Some((step_id, start, depth)) = frontier.pop_front() {
        if depth >= MAX_STEP_DEPTH {
            capped.push(format!("{}: stopped at depth {MAX_STEP_DEPTH}", start.name));
            continue;
        }
        // Fold plain calls until something step-worthy appears.
        let mut seen: HashSet<String> = HashSet::from([start.id.clone()]);
        let mut queue: VecDeque<(Node, Vec<String>, Vec<String>)> =
            VecDeque::from([(start.clone(), Vec::new(), Vec::new())]);
        let mut folded = 0;
        let mut made = 0;
        while let Some((u, via, when)) = queue.pop_front() {
            let edges = store.outgoing_edges(&u.id, &forward).map_err(internal)?;
            // A request to this project's own route is drawn as that route.
            let own_requests: HashSet<u32> = edges
                .iter()
                .filter(|e| {
                    e.metadata
                        .as_ref()
                        .and_then(|m| m.get("dispatch"))
                        .and_then(Value::as_str)
                        == Some("http")
                })
                .filter_map(|e| e.line)
                .collect();
            // Effects leave the index: unresolved platform calls from `u`.
            for (name, line) in store.failed_refs_from(&u.id).map_err(internal)? {
                if !EFFECT.is_match(&name) || made >= MAX_FANOUT || own_requests.contains(&line) {
                    continue;
                }
                let sid = format!("s{}", steps.len());
                steps.push(json!({ "id": sid, "kind": "effect", "label": name, "effect": effect_kind(&name), "depth": depth + 1 }));
                let mut w = when.clone();
                if let Some(g) = src.guard(&u.file_path, line) {
                    w.push(g);
                }
                links.push(
                    json!({ "from": step_id, "to": sid, "kind": "effect", "via": via, "when": w,
                    "site": { "file": u.file_path, "line": line } }),
                );
                made += 1;
            }
            if edges.len() > 40 {
                capped.push(format!(
                    "{}: {} outgoing edges, first 40 followed",
                    u.name,
                    edges.len()
                ));
            }
            for e in edges.into_iter().take(40) {
                if !seen.insert(e.target.clone()) {
                    continue;
                }
                let Some(v) = store.node(&e.target).map_err(internal)? else {
                    continue;
                };
                if v.kind == NodeKind::File || (!v.kind.is_callable() && v.kind != NodeKind::Route)
                {
                    continue;
                }
                let mut w = when.clone();
                if let Some(g) = e.line.and_then(|l| src.guard(&u.file_path, l)) {
                    w.push(g);
                }
                let site = e
                    .line
                    .and_then(|l| (l as usize).checked_sub(1))
                    .and_then(|i| src.lines(&u.file_path).get(i).cloned())
                    .unwrap_or_default();
                match step_kind(&u, &v, &e, &site) {
                    Some(kind) => {
                        if made >= MAX_FANOUT || steps.len() >= MAX_STEPS {
                            capped.push(format!("{}: more steps than shown", start.name));
                            break;
                        }
                        let (sid, fresh) = match step_of.get(&v.id) {
                            Some(s) => (s.clone(), false),
                            None => {
                                let sid = format!("s{}", steps.len());
                                steps.push(json!({ "id": sid, "kind": kind, "node": brief(&v), "depth": depth + 1 }));
                                step_of.insert(v.id.clone(), sid.clone());
                                (sid, true)
                            }
                        };
                        links.push(json!({ "from": step_id, "to": sid, "kind": kind, "via": via, "when": w,
                            "site": { "file": u.file_path, "line": e.line.unwrap_or(u.start_line) } }));
                        made += 1;
                        if fresh && kind != "screen" && kind != "request" {
                            frontier.push_back((sid, v, depth + 1));
                        }
                    }
                    None => {
                        if folded >= MAX_FOLD {
                            continue;
                        }
                        folded += 1;
                        let mut via2 = via.clone();
                        via2.push(v.name.clone());
                        queue.push_back((v, via2, w));
                    }
                }
            }
        }
        if folded >= MAX_FOLD {
            capped.push(format!(
                "{}: followed {MAX_FOLD} intermediate calls",
                start.name
            ));
        }
    }
    capped.dedup();
    Ok(json!({ "anchor": brief(&anchor), "steps": steps, "links": links, "capped": capped }))
}

// ---------------------------------------------------------------- file arcs

/// Calls among symbols of one file (arcs) and call sites leaving it (ports).
pub fn file_calls(cg: &CodeGraph, path: &str) -> Result<Value, (u16, String)> {
    let store = cg.store();
    let nodes = store.nodes_in_file(path).map_err(internal)?;
    let by_id: HashMap<&str, &Node> = nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let ids: Vec<String> = nodes
        .iter()
        .filter(|n| n.kind != NodeKind::File)
        .map(|n| n.id.clone())
        .collect();
    let mut arcs = Vec::new();
    let mut ports: BTreeMap<u32, Vec<String>> = BTreeMap::new();
    for e in store
        .outgoing_edges_from(&ids, &[EdgeKind::Calls, EdgeKind::Instantiates])
        .map_err(internal)?
    {
        let Some(site) = e.line else { continue };
        match by_id.get(e.target.as_str()) {
            Some(t) if t.start_line != site => arcs.push(json!({
                "site": site, "def": t.start_line, "name": t.name, "id": t.id,
            })),
            Some(_) => {}
            None => {
                if let Ok(Some(t)) = store.node(&e.target) {
                    let names = ports.entry(site).or_default();
                    if !names.contains(&t.name) && names.len() < 6 {
                        names.push(t.name);
                    }
                }
            }
        }
    }
    arcs.truncate(600);
    Ok(json!({
        "arcs": arcs,
        "ports": ports.into_iter().map(|(l, n)| json!({ "line": l, "names": n })).collect::<Vec<_>>(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_guards() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("a.ts"),
            "function f(user) {\n  if (user.collected) {\n    items.forEach((i) => {\n      open(i);\n    });\n    open(user);\n  }\n  {isAdmin && <Btn onPress={go} />}\n  switch (k) {\n    case 'x':\n      run();\n  }\n}\n",
        )
        .unwrap();
        let cg = CodeGraph::init(dir.path()).unwrap();
        let mut s = Sources::new(&cg);
        assert_eq!(s.guard("a.ts", 6).as_deref(), Some("user.collected"));
        assert_eq!(
            s.guard("a.ts", 4),
            None,
            "a callback body is its own function"
        );
        assert_eq!(s.guard("a.ts", 8).as_deref(), Some("isAdmin"));
        assert_eq!(s.guard("a.ts", 11).as_deref(), Some("case 'x'"));
        assert_eq!(s.guard("a.ts", 1), None);
    }
}
