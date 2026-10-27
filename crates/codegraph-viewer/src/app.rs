//! Application state and the update loop: routes come from the URL hash,
//! each screen loads with one API request, and every update re-renders
//! through the virtual DOM.

use crate::host;
use crate::layout::{self, Layout};
use crate::vdom::{self, Handlers, Mounted, Node};
use serde_json::{json, Value};
use std::collections::HashMap;

// ---------------------------------------------------------------- helpers

pub fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

pub fn u(v: &Value, k: &str) -> u64 {
    v.get(k).and_then(Value::as_u64).unwrap_or(0)
}

pub fn arr<'a>(v: &'a Value, k: &str) -> &'a [Value] {
    v.get(k)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

pub fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(if b[i] == b'+' { b' ' } else { b[i] });
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| decode(v))
    })
}

pub fn symbol_href(id: &str) -> String {
    format!("#/symbol/{}", encode(id))
}

pub fn file_href(path: &str, line: Option<u64>) -> String {
    match line {
        Some(l) if l > 0 => format!("#/file/{}?line={l}", encode(path)),
        _ => format!("#/file/{}", encode(path)),
    }
}

// ----------------------------------------------------------------- routes

#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    Home,
    Symbol(String),
    File(String, Option<u64>),
    Map,
    Flow(Option<String>, Option<String>),
    Dead,
    Routes,
    Trails,
    NotFound,
}

pub fn parse_route(hash: &str) -> Route {
    let h = hash.trim_start_matches('#').trim_start_matches('/');
    let (path, query) = h.split_once('?').unwrap_or((h, ""));
    let (head, rest) = path.split_once('/').unwrap_or((path, ""));
    match head {
        "" => Route::Home,
        "symbol" if !rest.is_empty() => Route::Symbol(decode(rest)),
        "file" if !rest.is_empty() => Route::File(
            decode(rest),
            query_param(query, "line").and_then(|l| l.parse().ok()),
        ),
        "map" => Route::Map,
        "flow" => Route::Flow(
            query_param(query, "from").filter(|s| !s.is_empty()),
            query_param(query, "to").filter(|s| !s.is_empty()),
        ),
        "dead" => Route::Dead,
        "routes" => Route::Routes,
        "trails" => Route::Trails,
        _ => Route::NotFound,
    }
}

// ------------------------------------------------------------------ state

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    From,
    To,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Callers,
    Callees,
}

pub type Fetched = Result<Value, String>;

#[derive(Clone)]
pub enum Msg {
    Route(String),
    Page(u32, Fetched),
    Stats(Fetched),
    OpenPalette,
    ClosePalette,
    PaletteInput(String),
    PaletteResults(u32, Fetched),
    PaletteKey(String),
    PaletteHover(usize),
    GlobalKey(String),
    Timer(u32),
    Sync,
    Synced(Fetched),
    SaveTrail,
    TrailSaved(Fetched),
    DeleteTrail(String),
    ClearTrail,
    MapSelect(Option<String>),
    FlowInput(Slot, String),
    FlowResults(Slot, u32, Fetched),
    FlowPick(Slot, String),
    FlowClear(Slot),
    FlowSwap,
    ToggleUncertain(Side),
    DismissToast,
    Noop,
}

pub struct MapState {
    pub data: Value,
    pub ids: Vec<String>,
    pub layout: Layout,
    pub selected: Option<String>,
}

pub struct FlowSlot {
    pub id: Option<String>,
    pub label: String,
    pub query: String,
    pub results: Vec<Value>,
    pub seq: u32,
}

impl FlowSlot {
    fn new(id: Option<String>) -> Self {
        FlowSlot {
            label: id.as_deref().map(short_id).unwrap_or_default(),
            id,
            query: String::new(),
            results: Vec::new(),
            seq: 0,
        }
    }
}

/// A readable fallback label for a symbol id before its name is known.
fn short_id(id: &str) -> String {
    id.rsplit([':', '/']).next().unwrap_or(id).to_string()
}

pub enum Page {
    Loading,
    Error(String),
    Home,
    Symbol(Value),
    File(Value),
    Map(Box<MapState>),
    Flow(Option<Value>),
    Dead(Value),
    Routes(Value),
    Trails(Value),
    NotFound,
}

pub struct Palette {
    pub query: String,
    pub results: Vec<Value>,
    pub selected: usize,
    pub seq: u32,
    pub loading: bool,
}

#[derive(Clone)]
pub struct Crumb {
    pub id: String,
    pub name: String,
    pub kind: String,
}

pub struct App {
    pub route: Route,
    pub page: Page,
    pub page_seq: u32,
    pub stats: Option<Value>,
    pub palette: Option<Palette>,
    pub trail: Vec<Crumb>,
    pub flow: [FlowSlot; 2],
    pub show_uncertain: [bool; 2],
    pub toast: Option<String>,
    pub syncing: bool,
    /// Scroll this element id into view after the next render.
    pub scroll_to: Option<String>,
    next_fetch: u32,
    pending: HashMap<u32, Box<dyn FnOnce(Fetched) -> Msg>>,
    timers: HashMap<u32, Msg>,
    next_timer: u32,
    mounted: Vec<Mounted>,
    next_node: u32,
    pub handlers: Handlers<Msg>,
}

const MAX_TRAIL: usize = 30;

impl App {
    pub fn new(storage: &Value) -> Self {
        let trail = storage
            .get("cg-trail")
            .and_then(Value::as_str)
            .and_then(|t| serde_json::from_str::<Vec<Value>>(t).ok())
            .unwrap_or_default()
            .iter()
            .map(|c| Crumb {
                id: s(c, "id").into(),
                name: s(c, "name").into(),
                kind: s(c, "kind").into(),
            })
            .filter(|c| !c.id.is_empty())
            .collect();
        App {
            route: Route::NotFound,
            page: Page::Loading,
            page_seq: 0,
            stats: None,
            palette: None,
            trail,
            flow: [FlowSlot::new(None), FlowSlot::new(None)],
            show_uncertain: [false, false],
            toast: None,
            syncing: false,
            scroll_to: None,
            next_fetch: 0,
            pending: HashMap::new(),
            timers: HashMap::new(),
            next_timer: 0,
            mounted: Vec::new(),
            next_node: 0,
            handlers: Handlers::default(),
        }
    }

    pub fn fetch(
        &mut self,
        method: &str,
        url: &str,
        body: Option<&Value>,
        then: impl FnOnce(Fetched) -> Msg + 'static,
    ) {
        self.next_fetch += 1;
        let id = self.next_fetch;
        self.pending.insert(id, Box::new(then));
        let body = body.map(Value::to_string).unwrap_or_default();
        host::start_fetch(id, method, url, &body);
    }

    pub fn fetched(&mut self, id: u32, status: u32, body: &str) -> Option<Msg> {
        let then = self.pending.remove(&id)?;
        let parsed: Result<Value, String> = serde_json::from_str(body).map_err(|_| {
            if status == 0 {
                "The viewer server is not reachable.".to_string()
            } else {
                body.trim().to_string()
            }
        });
        Some(then(match (status, parsed) {
            (200..=299, Ok(v)) => Ok(v),
            (_, Ok(v)) => Err(v
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("request failed")
                .to_string()),
            (_, Err(e)) => Err(e),
        }))
    }

    fn after(&mut self, ms: u32, msg: Msg) -> u32 {
        self.next_timer += 1;
        self.timers.insert(self.next_timer, msg);
        host::after(self.next_timer, ms);
        self.next_timer
    }

    pub fn start(&mut self, hash: &str) {
        self.fetch("GET", "/api/stats", None, Msg::Stats);
        self.update(Msg::Route(hash.to_string()));
    }

    fn save_trail_local(&self) {
        let list: Vec<Value> = self
            .trail
            .iter()
            .map(|c| json!({ "id": c.id, "name": c.name, "kind": c.kind }))
            .collect();
        host::store("cg-trail", &Value::Array(list).to_string());
    }

    fn load_page(&mut self) {
        self.page_seq += 1;
        let seq = self.page_seq;
        let then = move |r: Fetched| Msg::Page(seq, r);
        let url = match &self.route {
            Route::Home => {
                self.page = Page::Home;
                return;
            }
            Route::Symbol(id) => format!("/api/node?id={}", encode(id)),
            Route::File(p, _) => format!("/api/file?path={}", encode(p)),
            Route::Map => "/api/map".into(),
            Route::Dead => "/api/deadcode".into(),
            Route::Routes => "/api/routes".into(),
            Route::Trails => "/api/trails".into(),
            Route::Flow(from, to) => {
                let (from, to) = (from.clone(), to.clone());
                for (i, id) in [from.clone(), to.clone()].into_iter().enumerate() {
                    if self.flow[i].id != id {
                        self.flow[i] = FlowSlot::new(id);
                    }
                }
                match (from, to) {
                    (Some(a), Some(b)) => {
                        format!("/api/flow?from={}&to={}", encode(&a), encode(&b))
                    }
                    _ => {
                        self.page = Page::Flow(None);
                        return;
                    }
                }
            }
            Route::NotFound => {
                self.page = Page::NotFound;
                return;
            }
        };
        // Keep the previous screen on a flow re-query; others show a loader.
        if !matches!(self.route, Route::Flow(..)) {
            self.page = Page::Loading;
        }
        self.fetch("GET", &url, None, then);
    }

    fn page_loaded(&mut self, v: Value) {
        self.page = match &self.route {
            Route::Symbol(_) => {
                let n = &v["node"];
                let crumb = Crumb {
                    id: s(n, "id").into(),
                    name: s(n, "name").into(),
                    kind: s(n, "kind").into(),
                };
                if self.trail.last().map(|c| &c.id) != Some(&crumb.id) {
                    self.trail.retain(|c| c.id != crumb.id);
                    self.trail.push(crumb);
                    if self.trail.len() > MAX_TRAIL {
                        self.trail.remove(0);
                    }
                    self.save_trail_local();
                }
                host::title(&format!("{} · CodeGraph", s(n, "name")));
                self.scroll_to = Some("top".into());
                Page::Symbol(v)
            }
            Route::File(path, line) => {
                host::title(&format!(
                    "{} · CodeGraph",
                    path.rsplit('/').next().unwrap_or(path)
                ));
                self.scroll_to = Some(match line {
                    Some(l) => format!("L{l}"),
                    None => "top".into(),
                });
                Page::File(v)
            }
            Route::Map => {
                let ids: Vec<String> = arr(&v, "modules")
                    .iter()
                    .map(|m| s(m, "id").to_string())
                    .collect();
                let index: HashMap<&str, usize> = ids
                    .iter()
                    .enumerate()
                    .map(|(i, m)| (m.as_str(), i))
                    .collect();
                let edges: Vec<(usize, usize)> = arr(&v, "edges")
                    .iter()
                    .filter_map(|e| Some((*index.get(s(e, "from"))?, *index.get(s(e, "to"))?)))
                    .collect();
                let layout = layout::layered(&ids, &edges);
                host::title("Map · CodeGraph");
                Page::Map(Box::new(MapState {
                    data: v,
                    ids,
                    layout,
                    selected: None,
                }))
            }
            Route::Flow(..) => {
                // Name the two ends from the path (it may run either way).
                let steps = arr(&v, "steps");
                let (first, last) = (steps.first(), steps.last());
                let (a, b) = if v["reversed"].as_bool() == Some(true) {
                    (last, first)
                } else {
                    (first, last)
                };
                for (i, end) in [a, b].into_iter().enumerate() {
                    if let Some(p) = end {
                        self.flow[i].label = s(&p["node"], "name").to_string();
                    }
                }
                host::title("Flow · CodeGraph");
                Page::Flow(Some(v))
            }
            Route::Dead => {
                host::title("Dead code · CodeGraph");
                Page::Dead(v)
            }
            Route::Routes => {
                host::title("Routes · CodeGraph");
                Page::Routes(v)
            }
            Route::Trails => {
                host::title("Trails · CodeGraph");
                Page::Trails(v)
            }
            Route::Home | Route::NotFound => return,
        };
    }

    fn open_palette(&mut self) {
        if self.palette.is_none() {
            self.palette = Some(Palette {
                query: String::new(),
                results: Vec::new(),
                selected: 0,
                seq: 0,
                loading: false,
            });
        }
    }

    pub fn update(&mut self, msg: Msg) {
        match msg {
            Msg::Noop => {}
            Msg::Route(hash) => {
                let route = parse_route(&hash);
                self.palette = None;
                if route == self.route {
                    // Same file, new line: just scroll.
                    if let Route::File(_, Some(l)) = &route {
                        self.scroll_to = Some(format!("L{l}"));
                    }
                    return;
                }
                let same_file = matches!((&self.route, &route), (Route::File(a, _), Route::File(b, _)) if a == b);
                self.route = route;
                if same_file {
                    if let Route::File(_, line) = &self.route {
                        self.scroll_to = Some(line.map_or("top".into(), |l| format!("L{l}")));
                    }
                    return;
                }
                if self.route == Route::Home {
                    host::title("CodeGraph");
                }
                self.load_page();
            }
            Msg::Page(seq, r) => {
                if seq != self.page_seq {
                    return;
                }
                match r {
                    Ok(v) => self.page_loaded(v),
                    Err(e) => self.page = Page::Error(e),
                }
            }
            Msg::Stats(r) => {
                if let Ok(v) = r {
                    if self.route == Route::Home {
                        host::title(&format!("{} · CodeGraph", s(&v, "project")));
                    }
                    self.stats = Some(v);
                }
            }
            Msg::OpenPalette => self.open_palette(),
            Msg::ClosePalette => self.palette = None,
            Msg::PaletteInput(q) => {
                let Some(p) = &mut self.palette else { return };
                p.query = q;
                p.seq += 1;
                p.selected = 0;
                let seq = p.seq;
                if p.query.trim().is_empty() {
                    p.results.clear();
                    p.loading = false;
                    return;
                }
                p.loading = true;
                self.after(110, Msg::PaletteResults(seq, Err("debounce".into())));
            }
            Msg::PaletteResults(seq, r) => {
                let Some(p) = &mut self.palette else { return };
                if seq != p.seq {
                    return;
                }
                match r {
                    // The debounce timer fired: run the search.
                    Err(e) if e == "debounce" => {
                        let url = format!("/api/search?q={}&limit=30", encode(&p.query));
                        self.fetch("GET", &url, None, move |r| {
                            Msg::PaletteResults(seq, r.map(|v| json!({ "results": v })))
                        });
                    }
                    Ok(v) => {
                        p.results = arr(&v, "results").to_vec();
                        p.loading = false;
                    }
                    Err(_) => p.loading = false,
                }
            }
            Msg::PaletteHover(i) => {
                if let Some(p) = &mut self.palette {
                    p.selected = i;
                }
            }
            Msg::PaletteKey(k) => {
                let Some(p) = &mut self.palette else { return };
                match k.as_str() {
                    "ArrowDown" => {
                        p.selected = (p.selected + 1).min(p.results.len().saturating_sub(1))
                    }
                    "ArrowUp" => p.selected = p.selected.saturating_sub(1),
                    "Enter" => {
                        if let Some(r) = p.results.get(p.selected) {
                            host::go(&symbol_href(s(r, "id")));
                        }
                    }
                    "Escape" => self.palette = None,
                    _ => {}
                }
            }
            Msg::GlobalKey(k) => match k.as_str() {
                "Meta+k" | "Ctrl+k" | "/" => self.open_palette(),
                "Escape" => {
                    self.palette = None;
                    if let Page::Map(m) = &mut self.page {
                        m.selected = None;
                    }
                }
                "g" => host::go("#/"),
                "m" => host::go("#/map"),
                _ => {}
            },
            Msg::Timer(id) => {
                if let Some(m) = self.timers.remove(&id) {
                    self.update(m);
                }
            }
            Msg::Sync => {
                if !self.syncing {
                    self.syncing = true;
                    self.fetch("POST", "/api/sync", Some(&json!({})), Msg::Synced);
                }
            }
            Msg::Synced(r) => {
                self.syncing = false;
                match r {
                    Ok(v) => {
                        self.toast = Some(match u(&v, "changed") {
                            0 => "Index already up to date.".into(),
                            n => format!("Re-indexed {n} file{}.", if n == 1 { "" } else { "s" }),
                        });
                        self.fetch("GET", "/api/stats", None, Msg::Stats);
                        self.load_page();
                    }
                    Err(e) => self.toast = Some(format!("Sync failed: {e}")),
                }
                self.after(4000, Msg::DismissToast);
            }
            Msg::SaveTrail => {
                if self.trail.is_empty() {
                    return;
                }
                let names: Vec<&str> = self.trail.iter().map(|c| c.name.as_str()).collect();
                let name = if names.len() <= 3 {
                    names.join(" → ")
                } else {
                    format!("{} → … → {}", names[0], names[names.len() - 1])
                };
                let body = json!({
                    "name": name,
                    "steps": self.trail.iter().map(|c| json!({ "id": c.id, "name": c.name, "kind": c.kind })).collect::<Vec<_>>(),
                });
                self.fetch("POST", "/api/trails", Some(&body), Msg::TrailSaved);
            }
            Msg::TrailSaved(r) => {
                self.toast = Some(match &r {
                    Ok(_) => "Trail saved.".into(),
                    Err(e) => format!("Could not save the trail: {e}"),
                });
                if let (Ok(v), Page::Trails(_)) = (r, &self.page) {
                    self.page = Page::Trails(v);
                }
                self.after(3000, Msg::DismissToast);
            }
            Msg::DeleteTrail(id) => {
                let seq = self.page_seq;
                self.fetch(
                    "DELETE",
                    &format!("/api/trails?id={}", encode(&id)),
                    Some(&json!({})),
                    move |r| Msg::Page(seq, r),
                );
                // Drop it now; the response replaces the list when it arrives.
                if let Page::Trails(v) = &mut self.page {
                    if let Some(a) = v.as_array_mut() {
                        a.retain(|t| s(t, "id") != id);
                    }
                }
            }
            Msg::ClearTrail => {
                self.trail.clear();
                self.save_trail_local();
            }
            Msg::MapSelect(sel) => {
                if let Page::Map(m) = &mut self.page {
                    m.selected = if m.selected == sel { None } else { sel };
                }
            }
            Msg::FlowInput(slot, q) => {
                let i = slot as usize;
                self.flow[i].query = q;
                self.flow[i].seq += 1;
                let seq = self.flow[i].seq;
                if self.flow[i].query.trim().is_empty() {
                    self.flow[i].results.clear();
                    return;
                }
                self.after(110, Msg::FlowResults(slot, seq, Err("debounce".into())));
            }
            Msg::FlowResults(slot, seq, r) => {
                let i = slot as usize;
                if seq != self.flow[i].seq {
                    return;
                }
                match r {
                    Err(e) if e == "debounce" => {
                        let url = format!("/api/search?q={}&limit=8", encode(&self.flow[i].query));
                        self.fetch("GET", &url, None, move |r| {
                            Msg::FlowResults(slot, seq, r.map(|v| json!({ "results": v })))
                        });
                    }
                    Ok(v) => self.flow[i].results = arr(&v, "results").to_vec(),
                    Err(_) => {}
                }
            }
            Msg::FlowPick(slot, id) => {
                let i = slot as usize;
                let label = self.flow[i]
                    .results
                    .iter()
                    .find(|r| s(r, "id") == id)
                    .map(|r| s(r, "name").to_string())
                    .unwrap_or_else(|| short_id(&id));
                self.flow[i] = FlowSlot::new(Some(id));
                self.flow[i].label = label;
                self.go_flow();
            }
            Msg::FlowClear(slot) => {
                self.flow[slot as usize] = FlowSlot::new(None);
                self.go_flow();
            }
            Msg::FlowSwap => {
                self.flow.swap(0, 1);
                self.go_flow();
            }
            Msg::ToggleUncertain(side) => {
                let i = side as usize;
                self.show_uncertain[i] = !self.show_uncertain[i];
            }
            Msg::DismissToast => self.toast = None,
        }
    }

    fn go_flow(&mut self) {
        let mut q = Vec::new();
        if let Some(a) = &self.flow[0].id {
            q.push(format!("from={}", encode(a)));
        }
        if let Some(b) = &self.flow[1].id {
            q.push(format!("to={}", encode(b)));
        }
        let hash = if q.is_empty() {
            "#/flow".to_string()
        } else {
            format!("#/flow?{}", q.join("&"))
        };
        host::go(&hash);
    }

    /// Re-render and apply the patch.
    pub fn render(&mut self) {
        let tree: Vec<Node<Msg>> = vec![crate::views::view(self)];
        let scroll = self.scroll_to.take();
        let (ops, handlers, next) =
            vdom::render(&mut self.mounted, &tree, self.next_node, scroll.as_deref());
        self.next_node = next;
        self.handlers = handlers;
        if ops != "[]" {
            host::apply_ops(&ops);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_round_trip() {
        assert_eq!(parse_route("#/"), Route::Home);
        assert_eq!(parse_route(""), Route::Home);
        let id = "function:src/a.ts:helper:3";
        assert_eq!(parse_route(&symbol_href(id)), Route::Symbol(id.into()));
        assert_eq!(
            parse_route(&file_href("src/my file.ts", Some(12))),
            Route::File("src/my file.ts".into(), Some(12))
        );
        assert_eq!(
            parse_route("#/flow?from=a%3Ab&to="),
            Route::Flow(Some("a:b".into()), None)
        );
        assert_eq!(parse_route("#/nope"), Route::NotFound);
        assert_eq!(decode("%E2%86%92"), "→");
    }

    #[test]
    fn navigation_loads_and_records_the_trail() {
        host::take();
        let mut app = App::new(&json!({}));
        app.start("#/symbol/abc");
        let calls = host::take();
        assert!(calls
            .iter()
            .any(|(k, v)| *k == "fetch" && v.ends_with("GET /api/stats")));
        let node_fetch = calls
            .iter()
            .find(|(k, v)| *k == "fetch" && v.contains("/api/node?id=abc"))
            .unwrap();
        let id: u32 = node_fetch.1.split(' ').next().unwrap().parse().unwrap();
        let body = json!({
            "node": { "id": "abc", "name": "helper", "kind": "function", "file": "src/a.ts", "startLine": 1, "language": "typescript" },
            "callers": { "total": 0, "groups": [] }, "callees": { "total": 0, "groups": [] },
            "members": [], "hierarchy": null, "blast": { "levels": [], "total": 0, "files": 0 },
            "source": { "startLine": 1, "lines": ["function helper() {}"] }
        });
        let msg = app.fetched(id, 200, &body.to_string()).unwrap();
        app.update(msg);
        assert!(matches!(app.page, Page::Symbol(_)));
        assert_eq!(app.trail.len(), 1);
        app.render();
        let calls = host::take();
        let ops = &calls.iter().find(|(k, _)| *k == "apply").unwrap().1;
        assert!(ops.contains("helper"));
        assert!(ops.contains("<span class=\\\"k\\\">function</span>"));
    }
}
