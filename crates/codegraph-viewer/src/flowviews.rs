//! Diagram screens: entry points, the screen graph, and steps from a
//! symbol, plus the shared SVG graph drawing and image export.

use crate::app::{arr, file_href, s, symbol_href, u, MapState, Msg};
use crate::layout::{Layout, NODE_H};
use crate::vdom::{el, svg, El, Node};
use crate::views::glyph;
use serde_json::Value;

type N = Node<Msg>;

pub struct GNode {
    pub id: String,
    pub title: String,
    pub sub: String,
    pub class: String,
}

pub struct GEdge {
    pub a: usize,
    pub b: usize,
    pub width: f64,
    pub label: Option<String>,
    pub title: String,
    pub class: String,
}

fn shorten(t: &str, n: usize) -> String {
    if t.chars().count() > n {
        format!("{}…", t.chars().take(n - 1).collect::<String>())
    } else {
        t.to_string()
    }
}

/// A layered graph as SVG; selecting a node highlights its edges.
pub fn graph_svg(
    layout: &Layout,
    nodes: &[GNode],
    edges: &[GEdge],
    selected: Option<&str>,
    id: &'static str,
    label: &str,
) -> N {
    let sel_idx = selected.and_then(|s| nodes.iter().position(|n| n.id == s));
    let mut g_edges: Vec<N> = Vec::new();
    let mut g_labels: Vec<N> = Vec::new();
    for e in edges {
        let (pa, pb) = (&layout.nodes[e.a], &layout.nodes[e.b]);
        let (x1, x2) = (pa.x + pa.w / 2.0, pb.x + pb.w / 2.0);
        let (y1, y2) = if pb.y > pa.y {
            (pa.y + NODE_H, pb.y)
        } else {
            (pa.y, pb.y + NODE_H)
        };
        let dy = (y2 - y1) / 2.0;
        let d = format!(
            "M{x1:.1},{y1:.1} C{x1:.1},{:.1} {x2:.1},{:.1} {x2:.1},{y2:.1}",
            y1 + dy,
            y2 - dy
        );
        let on = sel_idx.is_some_and(|i| i == e.a || i == e.b);
        let state = match (sel_idx, on) {
            (None, _) => "",
            (Some(_), true) => " on",
            (Some(_), false) => " dim",
        };
        g_edges.push(
            svg("path")
                .attr("d", d)
                .attr("class", format!("edge {}{state}", e.class))
                .attr("stroke-width", format!("{:.2}", e.width))
                .attr("marker-end", format!("url(#{id}-arrow)"))
                .child(svg("title").text(e.title.clone()))
                .into(),
        );
        if let Some(l) = &e.label {
            if sel_idx.is_none() || on {
                // Near the target end, where edges fanning out of one node
                // have already spread apart.
                let t: f64 = 0.72;
                let mt = 1.0 - t;
                let bx = mt.powi(3) * x1
                    + 3.0 * mt * mt * t * x1
                    + 3.0 * mt * t * t * x2
                    + t.powi(3) * x2;
                let by = mt.powi(3) * y1
                    + 3.0 * mt * mt * t * (y1 + dy)
                    + 3.0 * mt * t * t * (y2 - dy)
                    + t.powi(3) * y2;
                g_labels.push(
                    svg("text")
                        .attr("x", format!("{bx:.1}"))
                        .attr("y", format!("{:.1}", by + 3.0))
                        .attr("class", "elabel")
                        .text(shorten(l, 28))
                        .into(),
                );
            }
        }
    }
    let g_nodes: Vec<N> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let p = &layout.nodes[i];
            svg("g")
                .attr(
                    "class",
                    format!(
                        "mod {}{}",
                        n.class,
                        if Some(i) == sel_idx { " on" } else { "" }
                    ),
                )
                .attr("transform", format!("translate({:.1},{:.1})", p.x, p.y))
                .attr("tabindex", "0")
                .attr("role", "button")
                .attr("aria-label", n.title.clone())
                .on_click(Msg::MapSelect(Some(n.id.clone())))
                .child(
                    svg("rect")
                        .attr("width", format!("{:.1}", p.w))
                        .attr("height", format!("{NODE_H}"))
                        .attr("rx", "8"),
                )
                .child(
                    svg("text")
                        .attr("x", format!("{:.1}", p.w / 2.0))
                        .attr("y", "19")
                        .attr("class", "lbl")
                        .text(shorten(&n.title, 34)),
                )
                .child(
                    svg("text")
                        .attr("x", format!("{:.1}", p.w / 2.0))
                        .attr("y", "34")
                        .attr("class", "sub")
                        .text(shorten(&n.sub, 40)),
                )
                .into()
        })
        .collect();
    svg("svg")
        .attr("id", id)
        .attr(
            "viewBox",
            format!("0 0 {:.0} {:.0}", layout.width, layout.height),
        )
        .attr("width", format!("{:.0}", layout.width))
        .attr("class", "mapsvg")
        .attr("role", "img")
        .attr("aria-label", label.to_string())
        .child(
            svg("defs").child(
                svg("marker")
                    .attr("id", format!("{id}-arrow"))
                    .attr("viewBox", "0 0 10 10")
                    .attr("refX", "9")
                    .attr("refY", "5")
                    .attr("markerWidth", "7")
                    .attr("markerHeight", "7")
                    .attr("orient", "auto-start-reverse")
                    .child(
                        svg("path")
                            .attr("d", "M0,0 L10,5 L0,10 z")
                            .attr("class", "arrowhead"),
                    ),
            ),
        )
        .child(svg("g").children(g_edges))
        .child(svg("g").children(g_labels))
        .child(svg("g").children(g_nodes))
        .into()
}

/// "Copy PNG" and "Download SVG" for the diagram with element id `target`.
pub fn export_buttons(target: &str, stem: &str) -> N {
    let selector = format!("#{target}");
    el("div")
        .class("exports")
        .child(
            el("button")
                .attr("type", "button")
                .class("quiet small")
                .on_click(Msg::Export(selector.clone(), stem.to_string(), "png"))
                .text("Copy PNG"),
        )
        .child(
            el("button")
                .attr("type", "button")
                .class("quiet small")
                .on_click(Msg::Export(selector, stem.to_string(), "svg"))
                .text("Download SVG"),
        )
        .into()
}

fn sec(title: &str, count: Option<u64>) -> El<Msg> {
    el("h3")
        .class("sec")
        .text(title)
        .opt(count.map(|c| el("span").class("count").text(c.to_string()).into()))
}

fn sym(v: &Value) -> El<Msg> {
    el("a")
        .class("sym")
        .href(symbol_href(s(v, "id")))
        .child(glyph(s(v, "kind")))
        .child(el("span").class("nm").text(s(v, "name")))
}

fn steps_link(v: &Value) -> N {
    el("a")
        .class("go")
        .href(format!("#/steps?id={}", crate::app::encode(s(v, "id"))))
        .attr("title", "Steps from here")
        .attr("aria-label", format!("Steps from {}", s(v, "name")))
        .text("⇢")
        .into()
}

fn file_row(f: &str, detail: String) -> N {
    el("li")
        .child(el("a").class("loc").href(file_href(f, None)).text(f))
        .child(el("span").class("muted").text(detail))
        .into()
}

fn list_or_empty(items: Vec<N>, none: &str) -> N {
    if items.is_empty() {
        el("p").class("empty").text(none).into()
    } else {
        el("ul").class("list").children(items).into()
    }
}

// ------------------------------------------------------------------- entry

pub fn entry(v: &Value) -> N {
    let routes: Vec<N> = arr(&v["routes"], "shown")
        .iter()
        .map(|r| el("li").child(sym(r)).child(steps_link(r)).into())
        .collect();
    let runners: Vec<N> = arr(v, "runners")
        .iter()
        .map(|r| {
            file_row(
                s(r, "file"),
                format!("{} calls · {} files", u(r, "calls"), u(r, "files")),
            )
        })
        .collect();
    let tests: Vec<N> = arr(v, "tests")
        .iter()
        .map(|t| file_row(s(t, "file"), format!("reaches {} files", u(t, "files"))))
        .collect();
    let hubs: Vec<N> = arr(v, "hubs")
        .iter()
        .map(|h| {
            el("li")
                .child(sym(h))
                .child(el("span").class("muted").text(if u(h, "fanIn") == 1 {
                    "1 user".to_string()
                } else {
                    format!("{} users", u(h, "fanIn"))
                }))
                .into()
        })
        .collect();
    el("div")
        .class("listpage")
        .child(el("h1").text("Entry points"))
        .child(el("p").class("muted").text(
            "Where to start reading: requests arriving from outside, files whose top-level code runs something, the tests that reach the most code, and the symbols everything depends on.",
        ))
        .child(
            el("div")
                .class("cols")
                .child(
                    el("section")
                        .class("card")
                        .child(sec("Routes", Some(u(&v["routes"], "total"))))
                        .child(list_or_empty(routes, "No framework routes detected.")),
                )
                .child(
                    el("section")
                        .class("card")
                        .child(sec("Files that run something", None))
                        .child(list_or_empty(runners, "No file makes top-level calls.")),
                )
                .child(
                    el("section")
                        .class("card")
                        .child(sec("Tests", None))
                        .child(list_or_empty(tests, "No tests found.")),
                )
                .child(
                    el("section")
                        .class("card")
                        .child(sec("Hubs", None))
                        .child(list_or_empty(hubs, "No references yet.")),
                ),
        )
        .into()
}

// ----------------------------------------------------------------- screens

pub fn screens(m: &MapState) -> N {
    let screens = arr(&m.data, "screens");
    let links = arr(&m.data, "links");
    let origins = arr(&m.data, "origins");
    let idx = |id: &str| m.ids.iter().position(|x| x == id);
    let nodes: Vec<GNode> = screens
        .iter()
        .map(|sc| GNode {
            id: s(sc, "id").to_string(),
            title: s(sc, "path").to_string(),
            sub: sc
                .get("component")
                .filter(|c| !c.is_null())
                .map(|c| s(c, "name").to_string())
                .unwrap_or_else(|| s(sc, "file").to_string()),
            class: "screen".into(),
        })
        .collect();
    let edges: Vec<GEdge> = links
        .iter()
        .filter_map(|l| {
            let when: Vec<&str> = arr(l, "when").iter().filter_map(Value::as_str).collect();
            Some(GEdge {
                a: idx(s(l, "from"))?,
                b: idx(s(l, "to"))?,
                width: 1.6,
                label: when.first().map(|w| w.to_string()),
                title: format!(
                    "{} → {}{}",
                    arr(l, "via")
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(" → "),
                    s(&l["site"], "file"),
                    if when.is_empty() {
                        String::new()
                    } else {
                        format!(" when {}", when.join(" and "))
                    }
                ),
                class: "nav".into(),
            })
        })
        .collect();
    let transition = |l: &Value, other_key: &str| -> N {
        let other = idx(s(l, other_key))
            .and_then(|i| screens.get(i))
            .map(|sc| s(sc, "path").to_string())
            .unwrap_or_default();
        let when: Vec<&str> = arr(l, "when").iter().filter_map(Value::as_str).collect();
        let via: Vec<&str> = arr(l, "via").iter().filter_map(Value::as_str).collect();
        el("li")
            .class("trans")
            .child(
                el("button")
                    .attr("type", "button")
                    .class("link")
                    .on_click(Msg::MapSelect(Some(s(l, other_key).to_string())))
                    .text(other),
            )
            .opt((!when.is_empty()).then(|| {
                el("span")
                    .class("badge dyn")
                    .text(format!("when {}", when.join(" and ")))
                    .into()
            }))
            .opt((!via.is_empty()).then(|| {
                el("div")
                    .class("muted small")
                    .text(format!("via {}", via.join(" → ")))
                    .into()
            }))
            .child(
                el("a")
                    .class("loc")
                    .href(file_href(
                        s(&l["site"], "file"),
                        Some(u(&l["site"], "line")),
                    ))
                    .text(format!(
                        "{}:{}",
                        s(&l["site"], "file"),
                        u(&l["site"], "line")
                    )),
            )
            .into()
    };
    let side: N = match m.selected.as_deref().and_then(|id| screens.iter().find(|sc| s(sc, "id") == id)) {
        Some(sc) => {
            let id = s(sc, "id");
            let out: Vec<N> = links.iter().filter(|l| s(l, "from") == id).map(|l| transition(l, "to")).collect();
            let inc: Vec<N> = links.iter().filter(|l| s(l, "to") == id).map(|l| transition(l, "from")).collect();
            let from_outside: Vec<N> = origins
                .iter()
                .filter(|o| s(o, "to") == id)
                .map(|o| el("li").child(sym(&o["origin"])).into())
                .collect();
            el("aside")
                .class("rail mapside")
                .child(el("h2").class("path").text(s(sc, "path")))
                .opt(sc.get("component").filter(|c| !c.is_null()).map(|c| {
                    el("p").child(el("span").class("muted").text("Renders ")).child(sym(c)).into()
                }))
                .child(
                    el("p").child(
                        el("a")
                            .class("loc")
                            .href(file_href(s(sc, "file"), Some(u(sc, "line"))))
                            .text(format!("{}:{}", s(sc, "file"), u(sc, "line"))),
                    ),
                )
                .child(sec("Leads to", Some(out.len() as u64)))
                .child(list_or_empty(out, "No navigation out of this screen."))
                .child(sec("Reached from", Some((inc.len() + from_outside.len()) as u64)))
                .child(list_or_empty(inc.into_iter().chain(from_outside).collect(), "Nothing navigates here."))
                .into()
        }
        None => el("aside")
            .class("rail mapside")
            .child(el("h2").text("Screens"))
            .child(el("p").class("muted").text(format!(
                "{} screens and {} transitions. Each arrow is a navigation found in code, traced back to the screen that triggers it and labelled with the condition guarding it. Select a screen for details.",
                screens.len(),
                links.len()
            )))
            .child(sec("From outside any screen", Some(origins.len() as u64)))
            .child(list_or_empty(
                origins
                    .iter()
                    .take(20)
                    .map(|o| {
                        let to = idx(s(o, "to")).and_then(|i| screens.get(i)).map(|sc| s(sc, "path")).unwrap_or("");
                        el("li").child(sym(&o["origin"])).child(el("span").class("muted").text(format!("→ {to}"))).into()
                    })
                    .collect(),
                "None.",
            ))
            .into(),
    };
    let canvas: N = if nodes.is_empty() {
        el("p").class("empty").text("No screens found: this needs a routed app (Next.js, SvelteKit, Expo Router, React Router, …).").into()
    } else {
        graph_svg(
            &m.layout,
            &nodes,
            &edges,
            m.selected.as_deref(),
            "screens-svg",
            "Screen transitions",
        )
    };
    el("div")
        .class("graphpage")
        .child(
            el("div")
                .class("pagehead")
                .child(el("h1").text("Screens"))
                .opt(
                    (!nodes.is_empty()).then(|| export_buttons("screens-svg", "codegraph-screens")),
                ),
        )
        .child(
            el("div")
                .class("map")
                .child(el("div").class("mapcanvas").child(canvas))
                .child(side),
        )
        .into()
}

// ------------------------------------------------------------------- steps

fn step_title(st: &Value) -> String {
    match st.get("node").filter(|n| !n.is_null()) {
        Some(n) => s(n, "name").to_string(),
        None => s(st, "label").to_string(),
    }
}

pub fn steps(m: &MapState) -> N {
    let steps = arr(&m.data, "steps");
    let links = arr(&m.data, "links");
    let idx = |id: &str| m.ids.iter().position(|x| x == id);
    let nodes: Vec<GNode> = steps
        .iter()
        .map(|st| {
            let kind = s(st, "kind");
            GNode {
                id: s(st, "id").to_string(),
                title: step_title(st),
                sub: match kind {
                    "effect" => format!("effect · {}", s(st, "effect")),
                    k => k.to_string(),
                },
                class: format!("step-{kind}"),
            }
        })
        .collect();
    let edges: Vec<GEdge> = links
        .iter()
        .filter_map(|l| {
            let when: Vec<&str> = arr(l, "when").iter().filter_map(Value::as_str).collect();
            let via: Vec<&str> = arr(l, "via").iter().filter_map(Value::as_str).collect();
            Some(GEdge {
                a: idx(s(l, "from"))?,
                b: idx(s(l, "to"))?,
                width: 1.5,
                label: when.first().map(|w| w.to_string()),
                title: format!(
                    "{}{}",
                    if via.is_empty() {
                        String::from("directly")
                    } else {
                        format!("via {}", via.join(" → "))
                    },
                    if when.is_empty() {
                        String::new()
                    } else {
                        format!(", when {}", when.join(" and "))
                    }
                ),
                class: s(l, "kind").to_string(),
            })
        })
        .collect();
    let anchor = &m.data["anchor"];
    let side: N = match m
        .selected
        .as_deref()
        .and_then(|id| steps.iter().find(|st| s(st, "id") == id))
    {
        Some(st) => {
            let id = s(st, "id");
            let into: Vec<N> = links
                .iter()
                .filter(|l| s(l, "to") == id)
                .map(|l| {
                    let via: Vec<&str> = arr(l, "via").iter().filter_map(Value::as_str).collect();
                    let when: Vec<&str> = arr(l, "when").iter().filter_map(Value::as_str).collect();
                    el("li")
                        .class("trans")
                        .child(el("span").text(if via.is_empty() {
                            "directly".to_string()
                        } else {
                            format!("via {}", via.join(" → "))
                        }))
                        .opt((!when.is_empty()).then(|| {
                            el("span")
                                .class("badge dyn")
                                .text(format!("when {}", when.join(" and ")))
                                .into()
                        }))
                        .child(
                            el("a")
                                .class("loc")
                                .href(file_href(
                                    s(&l["site"], "file"),
                                    Some(u(&l["site"], "line")),
                                ))
                                .text(format!(
                                    "{}:{}",
                                    s(&l["site"], "file"),
                                    u(&l["site"], "line")
                                )),
                        )
                        .into()
                })
                .collect();
            el("aside")
                .class("rail mapside")
                .child(el("h2").text(step_title(st)))
                .child(el("p").class("muted").text(match s(st, "kind") {
                    "effect" => format!(
                        "Leaves the project: {} ({})",
                        s(st, "effect"),
                        s(st, "label")
                    ),
                    "screen" => "A screen this flow navigates to.".into(),
                    "request" => "A route of this project this flow requests.".into(),
                    "trigger" => {
                        "A handler passed as a value: the user's or the platform's way in.".into()
                    }
                    "event" => "A listener reached through an event.".into(),
                    "bridge" => "The call crosses into another language.".into(),
                    "store" => "A state store action.".into(),
                    _ => "Where this walk starts.".into(),
                }))
                .opt(st.get("node").filter(|n| !n.is_null()).map(|n| {
                    el("div")
                        .class("actions")
                        .child(
                            el("a")
                                .class("btn")
                                .href(symbol_href(s(n, "id")))
                                .text("Open symbol"),
                        )
                        .child(
                            el("a")
                                .class("btn quiet")
                                .href(format!("#/steps?id={}", crate::app::encode(s(n, "id"))))
                                .text("Steps from here"),
                        )
                        .into()
                }))
                .child(sec("How it gets here", Some(into.len() as u64)))
                .child(list_or_empty(into, "This is the start."))
                .into()
        }
        None => {
            let capped: Vec<N> = arr(&m.data, "capped")
                .iter()
                .filter_map(Value::as_str)
                .map(|c| el("li").class("muted small").text(c).into())
                .collect();
            el("aside")
                .class("rail mapside")
                .child(el("h2").text(format!("Steps from {}", s(anchor, "name"))))
                .child(el("p").class("muted").text(
                    "What this symbol sets in motion, walking forward through calls. Screens, requests, handlers, events, language bridges, store actions and effects that leave the project get a box; everything in between is listed on the arrow.",
                ))
                .opt((!capped.is_empty()).then(|| {
                    el("div")
                        .child(sec("Not drawn", Some(capped.len() as u64)))
                        .child(el("ul").class("list").children(capped))
                        .into()
                }))
                .into()
        }
    };
    el("div")
        .class("graphpage")
        .child(
            el("div")
                .class("pagehead")
                .child(el("h1").child(el("span").class("muted").text("Steps from ")).child(sym(anchor)))
                .child(export_buttons("steps-svg", "codegraph-steps")),
        )
        .child(
            el("div")
                .class("map")
                .child(el("div").class("mapcanvas").child(if steps.len() <= 1 {
                    el("p").class("empty").text("Nothing step-worthy is reachable from here: no screens, requests, handlers, events or effects.").into()
                } else {
                    graph_svg(&m.layout, &nodes, &edges, m.selected.as_deref(), "steps-svg", "Steps")
                }))
                .child(side),
        )
        .into()
}

/// Call arcs beside a file's source: same-file calls as arcs from the call
/// site to the definition, other calls as ports.
pub fn arcs(calls: &Value, path: &str, total_lines: usize) -> N {
    const LINE: f64 = 20.0;
    const TOP: f64 = 10.0;
    const W: f64 = 64.0;
    let y = |line: u64| TOP + (line.saturating_sub(1)) as f64 * LINE + LINE / 2.0;
    let mut parts: Vec<N> = Vec::new();
    for a in arr(calls, "arcs") {
        let (y1, y2) = (y(u(a, "site")), y(u(a, "def")));
        let bulge = (W - 6.0) - (8.0 + (y2 - y1).abs() * 0.08).min(W - 12.0);
        parts.push(
            svg("a")
                .attr("href", file_href(path, Some(u(a, "def"))))
                .child(
                    svg("path")
                        .attr(
                            "d",
                            format!(
                                "M{W},{y1:.1} C{bulge:.1},{y1:.1} {bulge:.1},{y2:.1} {W},{y2:.1}"
                            ),
                        )
                        .attr("class", "arc")
                        .child(svg("title").text(format!(
                            "line {} calls {} (line {})",
                            u(a, "site"),
                            s(a, "name"),
                            u(a, "def")
                        ))),
                )
                .into(),
        );
    }
    for p in arr(calls, "ports") {
        let names: Vec<&str> = arr(p, "names").iter().filter_map(Value::as_str).collect();
        parts.push(
            svg("circle")
                .attr("cx", format!("{}", W - 4.0))
                .attr("cy", format!("{:.1}", y(u(p, "line"))))
                .attr("r", "3")
                .attr("class", "port")
                .child(svg("title").text(format!("calls {}", names.join(", "))))
                .into(),
        );
    }
    let h = TOP * 2.0 + total_lines as f64 * LINE;
    svg("svg")
        .attr("class", "arcs")
        .attr("width", format!("{W}"))
        .attr("height", format!("{h:.0}"))
        .attr("viewBox", format!("0 0 {W} {h:.0}"))
        .attr("aria-hidden", "true")
        .children(parts)
        .into()
}
