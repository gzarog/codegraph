//! Rendering: the app shell and one function per screen.

use crate::app::{arr, file_href, s, symbol_href, u, App, Msg, Page, Route, Side, Slot};
use crate::highlight::block_html;
use crate::layout::NODE_H;
use crate::vdom::{el, svg, text, El, Node};
use serde_json::Value;

type N = Node<Msg>;

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn fmt_num(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A short glyph per symbol kind.
pub fn glyph(kind: &str) -> N {
    let (label, class) = match kind {
        "function" => ("fn", "g-fn"),
        "method" => ("m", "g-fn"),
        "class" => ("C", "g-type"),
        "struct" => ("S", "g-type"),
        "interface" | "protocol" | "trait" => ("I", "g-type"),
        "enum" => ("E", "g-type"),
        "type_alias" => ("T", "g-type"),
        "route" => ("R", "g-route"),
        "component" => ("◇", "g-route"),
        "module" | "namespace" => ("M", "g-mod"),
        "file" => ("F", "g-mod"),
        "variable" | "constant" | "field" | "property" | "enum_member" => ("v", "g-var"),
        _ => ("·", "g-var"),
    };
    el("span")
        .class(format!("glyph {class}"))
        .attr("title", kind.replace('_', " "))
        .attr("aria-hidden", "true")
        .text(label)
        .into()
}

fn sym_link(v: &Value) -> El<Msg> {
    el("a")
        .class("sym")
        .href(symbol_href(s(v, "id")))
        .child(glyph(s(v, "kind")))
        .child(el("span").class("nm").text(s(v, "name")))
}

fn loc_link(file: &str, line: u64) -> El<Msg> {
    el("a")
        .class("loc")
        .href(file_href(file, Some(line)))
        .text(format!("{file}:{line}"))
}

fn empty(msg: &str) -> N {
    el("p").class("empty").text(msg).into()
}

fn section(title: &str, count: Option<u64>) -> El<Msg> {
    el("h3")
        .class("sec")
        .text(title)
        .opt(count.map(|c| el("span").class("count").text(fmt_num(c)).into()))
}

// -------------------------------------------------------------------- shell

pub fn view(app: &App) -> N {
    let project = app
        .stats
        .as_ref()
        .map(|v| s(v, "project").to_string())
        .unwrap_or_default();
    let nav = |label: &str, href: &str, active: bool| -> N {
        el("a")
            .href(href)
            .attr_if(active, "aria-current", "page")
            .text(label)
            .into()
    };
    let r = &app.route;
    let top = el("header")
        .class("top")
        .child(
            el("a")
                .class("brand")
                .href("#/")
                .child(
                    el("span")
                        .class("logo")
                        .attr("aria-hidden", "true")
                        .text("◆"),
                )
                .child(el("span").text(if project.is_empty() {
                    "CodeGraph".to_string()
                } else {
                    project
                })),
        )
        .child(
            el("nav")
                .attr("aria-label", "Views")
                .child(nav("Overview", "#/", *r == Route::Home))
                .child(nav("Entry points", "#/entry", *r == Route::Entry))
                .child(nav("Screens", "#/screens", *r == Route::Screens))
                .child(nav("Map", "#/map", *r == Route::Map))
                .child(nav("Routes", "#/routes", *r == Route::Routes))
                .child(nav("Flow", "#/flow", matches!(r, Route::Flow(..))))
                .child(nav("Dead code", "#/dead", *r == Route::Dead))
                .child(nav("Trails", "#/trails", *r == Route::Trails)),
        )
        .child(
            el("button")
                .class("search-btn")
                .attr("type", "button")
                .on_click(Msg::OpenPalette)
                .child(el("span").text("Search symbols"))
                .child(el("kbd").text("/")),
        );
    let mut shell = el("div").class("shell").attr("id", "top").child(top);
    if let Some(b) = drift_banner(app) {
        shell = shell.child(b);
    }
    if !app.trail.is_empty() {
        shell = shell.child(trail_bar(app));
    }
    let page: N = match &app.page {
        Page::Loading => el("div")
            .class("loading")
            .attr("role", "status")
            .text("Loading…")
            .into(),
        Page::Error(e) => el("div")
            .class("error-page")
            .child(el("h2").text("Couldn't load this view"))
            .child(el("p").text(e.clone()))
            .child(el("a").href("#/").text("Back to the overview"))
            .into(),
        Page::NotFound => el("div")
            .class("error-page")
            .child(el("h2").text("Nothing here"))
            .child(el("a").href("#/").text("Back to the overview"))
            .into(),
        Page::Home => home(app),
        Page::Symbol(v) => symbol(app, v),
        Page::File(v) => file(app, v),
        Page::Map(m) => map(m),
        Page::Flow(v) => flow(app, v.as_ref()),
        Page::Dead(v) => dead(v),
        Page::Routes(v) => routes(v),
        Page::Trails(v) => trails(v),
        Page::Entry(v) => crate::flowviews::entry(v),
        Page::Screens(m) => crate::flowviews::screens(m),
        Page::Steps(m) => crate::flowviews::steps(m),
    };
    shell = shell.child(el("main").attr("id", "main").child(page));
    if let Some(p) = palette(app) {
        shell = shell.child(p);
    }
    if let Some(t) = &app.toast {
        shell = shell.child(
            el("div")
                .class("toast")
                .attr("role", "status")
                .on_click(Msg::DismissToast)
                .text(t.clone()),
        );
    }
    shell.into()
}

fn drift_banner(app: &App) -> Option<N> {
    let st = app.stats.as_ref()?;
    let p = &st["pending"];
    let n = u(p, "added") + u(p, "modified") + u(p, "removed");
    if n == 0 {
        return None;
    }
    Some(
        el("div")
            .class("drift")
            .attr("role", "status")
            .child(el("span").text(format!(
                "{} changed since the last index; some views may be out of date.",
                plural(n, "file", "files")
            )))
            .child(
                el("button")
                    .attr("type", "button")
                    .attr_if(app.syncing, "disabled", "")
                    .on_click(Msg::Sync)
                    .text(if app.syncing {
                        "Syncing…"
                    } else {
                        "Sync now"
                    }),
            )
            .into(),
    )
}

fn trail_bar(app: &App) -> N {
    let current = match &app.route {
        Route::Symbol(id) => Some(id.as_str()),
        _ => None,
    };
    let crumbs: Vec<N> = app
        .trail
        .iter()
        .enumerate()
        .flat_map(|(i, c)| {
            let mut v: Vec<N> = Vec::new();
            if i > 0 {
                v.push(
                    el("span")
                        .class("sep")
                        .attr("aria-hidden", "true")
                        .text("›")
                        .into(),
                );
            }
            v.push(
                el("a")
                    .class("crumb")
                    .href(symbol_href(&c.id))
                    .attr_if(current == Some(c.id.as_str()), "aria-current", "page")
                    .child(glyph(&c.kind))
                    .text(c.name.clone())
                    .into(),
            );
            v
        })
        .collect();
    el("div")
        .class("trail")
        .attr("aria-label", "Trail")
        .child(el("div").class("crumbs").children(crumbs))
        .child(
            el("div")
                .class("trail-actions")
                .child(
                    el("button")
                        .attr("type", "button")
                        .on_click(Msg::SaveTrail)
                        .text("Save trail"),
                )
                .child(
                    el("button")
                        .attr("type", "button")
                        .class("quiet")
                        .on_click(Msg::ClearTrail)
                        .text("Clear"),
                ),
        )
        .into()
}

fn palette(app: &App) -> Option<N> {
    let p = app.palette.as_ref()?;
    let rows: Vec<N> = p
        .results
        .iter()
        .enumerate()
        .map(|(i, r)| {
            el("a")
                .class(if i == p.selected { "row sel" } else { "row" })
                .href(symbol_href(s(r, "id")))
                .attr("role", "option")
                .attr_if(i == p.selected, "aria-selected", "true")
                .on("mouseover", move |_| Some(Msg::PaletteHover(i)))
                .child(glyph(s(r, "kind")))
                .child(
                    el("span")
                        .class("main")
                        .child(el("span").class("nm").text(s(r, "name")))
                        .child(el("span").class("sig").text(s(r, "signature"))),
                )
                .child(
                    el("span")
                        .class("loc")
                        .text(format!("{}:{}", s(r, "file"), u(r, "line"))),
                )
                .into()
        })
        .collect();
    let status = if p.query.trim().is_empty() {
        Some("Type a symbol name, or part of one.")
    } else if p.loading && p.results.is_empty() {
        Some("Searching…")
    } else if p.results.is_empty() {
        Some("No matching symbols.")
    } else {
        None
    };
    Some(
        el("div")
            .class("overlay")
            .on_click(Msg::ClosePalette)
            .child(
                el("div")
                    .class("palette")
                    .attr("role", "dialog")
                    .attr("aria-label", "Search symbols")
                    .on("click", |_| Some(Msg::Noop))
                    .child(
                        el("input")
                            .attr("type", "search")
                            .attr("placeholder", "Search symbols…")
                            .attr("aria-label", "Search symbols")
                            .attr("autocomplete", "off")
                            .attr("spellcheck", "false")
                            .attr("value", p.query.clone())
                            .on_input(Msg::PaletteInput)
                            .on("keydown", |k| Some(Msg::PaletteKey(k.to_string())))
                            .focus(),
                    )
                    .child(
                        el("div")
                            .class("results")
                            .attr("role", "listbox")
                            .children(rows),
                    )
                    .opt(status.map(|t| el("p").class("hint").text(t).into()))
                    .child(
                        el("p")
                            .class("keys")
                            .text("↑↓ to move · Enter to open · Esc to close"),
                    ),
            )
            .into(),
    )
}

// --------------------------------------------------------------------- home

fn home(app: &App) -> N {
    let Some(st) = &app.stats else {
        return el("div").class("loading").text("Loading…").into();
    };
    let tiles = [
        ("Files", u(st, "files")),
        ("Symbols", u(st, "symbols")),
        ("Relationships", u(st, "edges")),
        ("Routes", u(&st["routes"], "total")),
    ];
    let langs = arr(st, "languages");
    let total: u64 = langs.iter().map(|l| u(l, "files")).sum::<u64>().max(1);
    let palette = ["l1", "l2", "l3", "l4", "l5", "l6"];
    let mut sorted: Vec<&Value> = langs.iter().collect();
    sorted.sort_by_key(|l| std::cmp::Reverse(u(l, "files")));
    let bar: Vec<N> = sorted
        .iter()
        .take(6)
        .enumerate()
        .map(|(i, l)| {
            el("span")
                .class(format!("seg {}", palette[i]))
                .attr(
                    "style",
                    format!("width:{:.2}%", 100.0 * u(l, "files") as f64 / total as f64),
                )
                .attr("title", format!("{}: {}", s(l, "name"), u(l, "files")))
                .into()
        })
        .collect();
    let legend: Vec<N> = sorted
        .iter()
        .take(6)
        .enumerate()
        .map(|(i, l)| {
            el("li")
                .child(
                    el("span")
                        .class(format!("dot {}", palette[i]))
                        .attr("aria-hidden", "true"),
                )
                .text(format!("{} ", s(l, "name")))
                .child(el("span").class("muted").text(fmt_num(u(l, "files"))))
                .into()
        })
        .collect();
    let hubs: Vec<N> = arr(st, "hubs")
        .iter()
        .map(|h| {
            el("li")
                .child(sym_link(h))
                .child(el("span").class("muted").text(if u(h, "fanIn") == 1 {
                    "1 user".to_string()
                } else {
                    format!("{} users", fmt_num(u(h, "fanIn")))
                }))
                .child(loc_link(s(h, "file"), u(h, "line")))
                .into()
        })
        .collect();
    let routes: Vec<N> = arr(&st["routes"], "shown")
        .iter()
        .map(|r| {
            el("li")
                .child(sym_link(r))
                .child(loc_link(s(r, "file"), u(r, "line")))
                .into()
        })
        .collect();
    let frameworks: Vec<N> = arr(st, "frameworks")
        .iter()
        .filter_map(Value::as_str)
        .map(|f| el("span").class("chip").text(f).into())
        .collect();
    el("div")
        .class("home")
        .child(
            el("section")
                .class("hero")
                .child(el("h1").text(s(st, "project")))
                .child(el("p").class("muted").text(format!(
                    "Indexed by CodeGraph {}. Press / to jump to any symbol.",
                    s(st, "version")
                )))
                .child(
                    el("div")
                        .class("tiles")
                        .children(tiles.iter().map(|(k, v)| {
                            el("div")
                                .class("tile")
                                .child(el("div").class("v").text(fmt_num(*v)))
                                .child(el("div").class("k").text(*k))
                                .into()
                        })),
                ),
        )
        .child(
            el("section")
                .class("card")
                .child(section("Languages", None))
                .child(
                    el("div")
                        .class("langbar")
                        .attr("role", "img")
                        .attr("aria-label", "Files by language")
                        .children(bar),
                )
                .child(el("ul").class("legend").children(legend))
                .opt(
                    (!frameworks.is_empty())
                        .then(|| el("div").class("chips").children(frameworks).into()),
                ),
        )
        .child(
            el("div")
                .class("cols")
                .child(
                    el("section")
                        .class("card")
                        .child(section("Most-used symbols", None))
                        .child(if hubs.is_empty() {
                            empty("No references yet.")
                        } else {
                            el("ul").class("list").children(hubs).into()
                        }),
                )
                .child(
                    el("section")
                        .class("card")
                        .child(section("Entry points", Some(u(&st["routes"], "total"))))
                        .child(if routes.is_empty() {
                            empty("No framework routes detected.")
                        } else {
                            el("ul").class("list").children(routes).into()
                        })
                        .opt((u(&st["routes"], "total") > 12).then(|| {
                            el("a")
                                .class("more")
                                .href("#/routes")
                                .text("All routes →")
                                .into()
                        })),
                ),
        )
        .into()
}

// ------------------------------------------------------------------- symbol

fn rail(app: &App, title: &str, data: &Value, side: Side) -> N {
    let show_uncertain = app.show_uncertain[side as usize];
    let groups = arr(data, "groups");
    let uncertain = u(data, "uncertain");
    let rows: Vec<N> = groups
        .iter()
        .filter(|g| show_uncertain || g["uncertain"].as_bool() != Some(true))
        .map(|g| {
            let lines: Vec<N> = arr(g, "lines")
                .iter()
                .filter_map(Value::as_u64)
                .map(|l| {
                    let file = if side == Side::Callers {
                        s(g, "file")
                    } else {
                        ""
                    };
                    let target = if file.is_empty() { None } else { Some(file) };
                    match target {
                        Some(f) => el("a")
                            .class("ln-link")
                            .href(file_href(f, Some(l)))
                            .text(format!("L{l}"))
                            .into(),
                        None => el("span").class("ln-link").text(format!("L{l}")).into(),
                    }
                })
                .collect();
            let dynamic = g.get("dynamic").filter(|d| !d.is_null()).map(|d| {
                el("span")
                    .class("badge dyn")
                    .attr(
                        "title",
                        "Linked by a heuristic (event, function pointer or HTTP route)",
                    )
                    .text(format!("via {} {}", s(d, "via"), s(d, "key")))
                    .into()
            });
            el("li")
                .class(if g["uncertain"].as_bool() == Some(true) {
                    "grp uncertain"
                } else {
                    "grp"
                })
                .child(sym_link(g))
                .child(
                    el("div")
                        .class("meta")
                        .child(loc_link(s(g, "file"), u(g, "line")))
                        .opt(dynamic),
                )
                .opt((!lines.is_empty()).then(|| el("div").class("lines").children(lines).into()))
                .into()
        })
        .collect();
    let total = u(data, "total");
    let shown = groups.len() as u64;
    el("aside")
        .class(format!(
            "rail {}",
            if side == Side::Callers {
                "callers"
            } else {
                "callees"
            }
        ))
        .attr("aria-label", title)
        .child(section(title, Some(total)))
        .child(if rows.is_empty() && uncertain == 0 {
            empty(if side == Side::Callers {
                "Nothing calls or references this."
            } else {
                "Uses nothing indexed."
            })
        } else {
            el("ul").class("groups").children(rows).into()
        })
        .opt((total > shown).then(|| {
            el("p")
                .class("muted small")
                .text(format!("+{} more not shown", total - shown))
                .into()
        }))
        .opt((uncertain > 0).then(|| {
            el("button")
                .attr("type", "button")
                .class("quiet small")
                .on_click(Msg::ToggleUncertain(side))
                .text(if show_uncertain {
                    format!("Hide {} name-only matches", uncertain)
                } else {
                    format!(
                        "Uncertain · {} name-only match{}",
                        uncertain,
                        if uncertain == 1 { "" } else { "es" }
                    )
                })
                .into()
        }))
        .into()
}

fn symbol(app: &App, v: &Value) -> N {
    let n = &v["node"];
    let mut badges: Vec<N> = Vec::new();
    if v["hub"].as_bool() == Some(true) {
        badges.push(
            el("span")
                .class("badge hub")
                .text(format!("hub · {}", u(v, "fanIn")))
                .into(),
        );
    }
    if n["exported"].as_bool() == Some(true) {
        badges.push(el("span").class("badge").text("exported").into());
    }
    if n["async"].as_bool() == Some(true) {
        badges.push(el("span").class("badge").text("async").into());
    }
    for d in arr(n, "decorators").iter().filter_map(Value::as_str) {
        badges.push(el("span").class("badge").text(format!("@{d}")).into());
    }
    let src = &v["source"];
    let code: N = if src.is_null() {
        empty("Source isn't available (the file is missing or outside the project).")
    } else {
        let lines: Vec<String> = arr(src, "lines")
            .iter()
            .map(|l| l.as_str().unwrap_or("").to_string())
            .collect();
        let start = u(src, "startLine") as usize;
        el("div")
            .class("codewrap")
            .opt((src["stale"].as_bool() == Some(true)).then(|| {
                el("p")
                    .class("warn")
                    .text("This file changed since it was indexed; lines may have moved.")
                    .into()
            }))
            .child(
                el("pre")
                    .class("code")
                    .attr("aria-label", "Source")
                    .html(block_html(&lines, start, s(n, "language"), &[])),
            )
            .opt((src["truncated"].as_bool() == Some(true)).then(|| {
                el("a")
                    .class("more")
                    .href(file_href(s(n, "file"), Some(u(n, "startLine"))))
                    .text("Continue in the file →")
                    .into()
            }))
            .into()
    };
    let members: Vec<N> = arr(v, "members")
        .iter()
        .map(|m| {
            el("li")
                .child(sym_link(m))
                .child(el("span").class("muted").text(format!("L{}", u(m, "line"))))
                .into()
        })
        .collect();
    let hier = &v["hierarchy"];
    let hierarchy: Option<N> = (!hier.is_null()
        && (!arr(hier, "ancestors").is_empty() || !arr(hier, "descendants").is_empty()))
    .then(|| {
        let chips = |list: &[Value]| -> Vec<N> {
            list.iter()
                .map(|x| sym_link(x).class("sym chip").into())
                .collect()
        };
        el("section")
            .class("card")
            .child(section("Type hierarchy", None))
            .child(
                el("div")
                    .class("hier")
                    .child(
                        el("div")
                            .class("col")
                            .child(el("span").class("muted small").text("Extends / implements"))
                            .children(chips(arr(hier, "ancestors"))),
                    )
                    .child(
                        el("div")
                            .class("col this")
                            .child(glyph(s(n, "kind")))
                            .text(s(n, "name")),
                    )
                    .child(
                        el("div")
                            .class("col")
                            .child(el("span").class("muted small").text("Extended by"))
                            .children(chips(arr(hier, "descendants"))),
                    ),
            )
            .into()
    });
    let blast = &v["blast"];
    let levels: Vec<String> = arr(blast, "levels")
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{} at depth {}", c.as_u64().unwrap_or(0), i + 1))
        .collect();
    let tests: Vec<N> = arr(blast, "tests")
        .iter()
        .filter_map(Value::as_str)
        .map(|t| {
            el("a")
                .class("chip")
                .href(file_href(t, None))
                .text(t)
                .into()
        })
        .collect();
    let blast_card = el("section")
        .class("card blast")
        .child(section("Blast radius", None))
        .child(el("p").text(if u(blast, "total") == 0 {
            "Nothing depends on this symbol.".to_string()
        } else {
            format!(
                "Changing it reaches {} in {} within 3 hops{}.",
                plural(u(blast, "total"), "symbol", "symbols"),
                plural(u(blast, "files"), "file", "files"),
                if blast["truncated"].as_bool() == Some(true) {
                    " (and more)"
                } else {
                    ""
                }
            )
        }))
        .opt(
            (!levels.is_empty())
                .then(|| el("p").class("muted small").text(levels.join(" · ")).into()),
        )
        .opt((!tests.is_empty()).then(|| {
            el("div")
                .class("chips")
                .child(el("span").class("muted small").text("Tests: "))
                .children(tests)
                .into()
        }));
    let parent = v.get("parent").filter(|p| !p.is_null()).map(|p| {
        el("span")
            .class("muted")
            .text("in ")
            .child(sym_link(p))
            .into()
    });
    let main = el("section")
        .class("center")
        .child(
            el("div")
                .class("sym-head")
                .child(
                    el("div")
                        .class("title")
                        .child(glyph(s(n, "kind")))
                        .child(el("h1").text(s(n, "name")))
                        .children(badges),
                )
                .child(
                    el("div")
                        .class("sub")
                        .child(
                            el("span")
                                .class("kind")
                                .text(s(n, "kind").replace('_', " ")),
                        )
                        .opt(parent)
                        .child(loc_link(s(n, "file"), u(n, "startLine"))),
                )
                .opt(
                    (!s(n, "qualifiedName").is_empty())
                        .then(|| el("p").class("qname").text(s(n, "qualifiedName")).into()),
                )
                .opt(
                    (!s(n, "docstring").is_empty())
                        .then(|| el("p").class("doc").text(s(n, "docstring")).into()),
                )
                .child(
                    el("div")
                        .class("actions")
                        .child(
                            el("a")
                                .class("btn")
                                .href(format!("#/flow?from={}", crate::app::encode(s(n, "id"))))
                                .text("Trace a flow from here"),
                        )
                        .child(
                            el("a")
                                .class("btn")
                                .href(format!("#/steps?id={}", crate::app::encode(s(n, "id"))))
                                .text("Steps from here"),
                        )
                        .child(
                            el("a")
                                .class("btn quiet")
                                .href(file_href(s(n, "file"), Some(u(n, "startLine"))))
                                .text("Open in file"),
                        ),
                ),
        )
        .child(code)
        .opt((!members.is_empty()).then(|| {
            el("section")
                .class("card")
                .child(section("Members", Some(members.len() as u64)))
                .child(el("ul").class("list cols2").children(members))
                .into()
        }))
        .opt(hierarchy)
        .child(blast_card);
    el("div")
        .class("symbol")
        .child(rail(app, "Callers", &v["callers"], Side::Callers))
        .child(main)
        .child(rail(app, "Callees", &v["callees"], Side::Callees))
        .into()
}

// --------------------------------------------------------------------- file

fn file(app: &App, v: &Value) -> N {
    let f = &v["file"];
    let path = s(f, "path");
    let line = match &app.route {
        Route::File(_, l) => *l,
        _ => None,
    };
    let outline: Vec<N> = arr(v, "outline")
        .iter()
        .map(|o| {
            el("li")
                .attr(
                    "style",
                    format!("padding-left:{}px", 4 + 14 * u(o, "depth")),
                )
                .child(
                    el("a")
                        .class("sym")
                        .href(file_href(path, Some(u(o, "line"))))
                        .attr_if(
                            line.is_some_and(|l| l >= u(o, "line") && l <= u(o, "endLine")),
                            "aria-current",
                            "true",
                        )
                        .child(glyph(s(o, "kind")))
                        .child(el("span").class("nm").text(s(o, "name"))),
                )
                .child(
                    el("a")
                        .class("go")
                        .href(symbol_href(s(o, "id")))
                        .attr("title", "Open symbol")
                        .attr("aria-label", format!("Open {}", s(o, "name")))
                        .text("→"),
                )
                .into()
        })
        .collect();
    let lines: Vec<String> = arr(v, "source")
        .iter()
        .map(|l| l.as_str().unwrap_or("").to_string())
        .collect();
    let mark: Vec<usize> = line.map(|l| vec![l as usize]).unwrap_or_default();
    let files_list = |list: &[Value], none: &str| -> N {
        let items: Vec<N> = list
            .iter()
            .filter_map(Value::as_str)
            .map(|p| {
                el("li")
                    .child(el("a").class("loc").href(file_href(p, None)).text(p))
                    .into()
            })
            .collect();
        if items.is_empty() {
            empty(none)
        } else {
            el("ul").class("list").children(items).into()
        }
    };
    let mut badges: Vec<N> = vec![el("span").class("badge").text(s(f, "language")).into()];
    if f["generated"].as_bool() == Some(true) {
        badges.push(el("span").class("badge").text("generated").into());
    }
    let errors: Vec<N> = arr(f, "errors")
        .iter()
        .filter_map(Value::as_str)
        .map(|e| el("li").text(e).into())
        .collect();
    el("div")
        .class("file")
        .child(
            el("aside")
                .class("rail outline")
                .attr("aria-label", "Outline")
                .child(section("Outline", Some(arr(v, "outline").len() as u64)))
                .child(if outline.is_empty() { empty("No symbols.") } else { el("ul").class("tree").children(outline).into() }),
        )
        .child(
            el("section")
                .class("center")
                .child(
                    el("div")
                        .class("sym-head")
                        .child(el("div").class("title").child(glyph("file")).child(el("h1").class("path").text(path)).children(badges))
                        .child(el("div").class("sub").child(el("span").class("muted").text(plural(u(f, "lines"), "line", "lines")))),
                )
                .opt((f["stale"].as_bool() == Some(true)).then(|| {
                    el("p").class("warn").text("This file changed since it was indexed; the outline may be out of date.").into()
                }))
                .opt((!errors.is_empty()).then(|| el("ul").class("warn").children(errors).into()))
                .child(
                    el("div")
                        .class("codearcs")
                        .child(crate::flowviews::arcs(&v["calls"], path, lines.len()))
                        .child(el("pre").class("code arced").attr("aria-label", "Source").html(block_html(&lines, 1, s(f, "language"), &mark))),
                )
                .opt((v["truncated"].as_bool() == Some(true)).then(|| empty("The file is longer than the viewer shows."))),
        )
        .child(
            el("aside")
                .class("rail deps")
                .attr("aria-label", "Dependencies")
                .child(section("Uses", Some(arr(v, "imports").len() as u64)))
                .child(files_list(arr(v, "imports"), "Uses no other indexed file."))
                .child(section("Used by", Some(arr(v, "dependents").len() as u64)))
                .child(files_list(arr(v, "dependents"), "No other file uses this one.")),
        )
        .into()
}

// ---------------------------------------------------------------------- map

fn map(m: &crate::app::MapState) -> N {
    let modules = arr(&m.data, "modules");
    let edges = arr(&m.data, "edges");
    let idx = |id: &str| m.ids.iter().position(|x| x == id);
    let max_w = edges
        .iter()
        .map(|e| u(e, "weight"))
        .max()
        .unwrap_or(1)
        .max(1) as f64;
    let sel = m.selected.as_deref();
    let mut g_edges: Vec<N> = Vec::new();
    for e in edges {
        let (Some(a), Some(b)) = (idx(s(e, "from")), idx(s(e, "to"))) else {
            continue;
        };
        let (pa, pb) = (&m.layout.nodes[a], &m.layout.nodes[b]);
        let (x1, y1) = (pa.x + pa.w / 2.0, pa.y + NODE_H);
        let (x2, y2) = (pb.x + pb.w / 2.0, pb.y);
        // Upward edges (cycles) leave from the top and arrive at the bottom.
        let (y1, y2) = if y2 <= y1 {
            (pa.y, pb.y + NODE_H)
        } else {
            (y1, y2)
        };
        let dy = (y2 - y1) / 2.0;
        let d = format!(
            "M{x1:.1},{y1:.1} C{x1:.1},{:.1} {x2:.1},{:.1} {x2:.1},{y2:.1}",
            y1 + dy,
            y2 - dy
        );
        let w = 1.0 + 3.0 * ((u(e, "weight") as f64).ln_1p() / max_w.ln_1p());
        let on = sel.is_some_and(|x| x == s(e, "from") || x == s(e, "to"));
        let class = match (sel, on) {
            (None, _) => "edge",
            (Some(_), true) => "edge on",
            (Some(_), false) => "edge dim",
        };
        g_edges.push(
            svg("path")
                .attr("d", d)
                .attr("class", class)
                .attr("stroke-width", format!("{w:.2}"))
                .attr("marker-end", "url(#arrow)")
                .child(svg("title").text(format!(
                    "{} → {}: {} references",
                    s(e, "from"),
                    s(e, "to"),
                    u(e, "weight")
                )))
                .into(),
        );
    }
    let g_nodes: Vec<N> = modules
        .iter()
        .enumerate()
        .map(|(i, md)| {
            let p = &m.layout.nodes[i];
            let id = s(md, "id").to_string();
            let active = sel == Some(id.as_str());
            svg("g")
                .attr("class", if active { "mod on" } else { "mod" })
                .attr("transform", format!("translate({:.1},{:.1})", p.x, p.y))
                .attr("tabindex", "0")
                .attr("role", "button")
                .attr("aria-label", format!("Module {id}"))
                .on_click(Msg::MapSelect(Some(id.clone())))
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
                        .text(id.clone()),
                )
                .child(
                    svg("text")
                        .attr("x", format!("{:.1}", p.w / 2.0))
                        .attr("y", "34")
                        .attr("class", "sub")
                        .text(format!(
                            "{} files · {} symbols",
                            u(md, "files"),
                            fmt_num(u(md, "symbols"))
                        )),
                )
                .into()
        })
        .collect();
    let graph = svg("svg")
        .attr(
            "viewBox",
            format!("0 0 {:.0} {:.0}", m.layout.width, m.layout.height),
        )
        .attr("width", format!("{:.0}", m.layout.width))
        .attr("class", "mapsvg")
        .attr("id", "map-svg")
        .attr("role", "img")
        .attr("aria-label", "Module dependency map")
        .child(
            svg("defs").child(
                svg("marker")
                    .attr("id", "arrow")
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
        .child(svg("g").children(g_nodes));
    let side: N = match sel.and_then(|id| modules.iter().find(|md| s(md, "id") == id)) {
        Some(md) => {
            let id = s(md, "id");
            let deps = |dir_from: bool| -> Vec<N> {
                let mut list: Vec<&Value> = edges
                    .iter()
                    .filter(|e| s(e, if dir_from { "from" } else { "to" }) == id)
                    .collect();
                list.sort_by_key(|e| std::cmp::Reverse(u(e, "weight")));
                list.iter()
                    .map(|e| {
                        let other = s(e, if dir_from { "to" } else { "from" }).to_string();
                        el("li")
                            .child(el("button").attr("type", "button").class("link").on_click(Msg::MapSelect(Some(other.clone()))).text(other))
                            .child(el("span").class("muted").text(fmt_num(u(e, "weight"))))
                            .into()
                    })
                    .collect()
            };
            let (uses, used_by) = (deps(true), deps(false));
            let files: Vec<N> = arr(md, "sample")
                .iter()
                .filter_map(Value::as_str)
                .map(|p| el("li").child(el("a").class("loc").href(file_href(p, None)).text(p.rsplit('/').next().unwrap_or(p))).into())
                .collect();
            el("aside")
                .class("rail mapside")
                .child(el("h2").class("path").text(id))
                .child(el("p").class("muted").text(format!("{} · {} symbols", plural(u(md, "files"), "file", "files"), fmt_num(u(md, "symbols")))))
                .child(section("Depends on", Some(uses.len() as u64)))
                .child(if uses.is_empty() { empty("Nothing.") } else { el("ul").class("list").children(uses).into() })
                .child(section("Used by", Some(used_by.len() as u64)))
                .child(if used_by.is_empty() { empty("Nothing.") } else { el("ul").class("list").children(used_by).into() })
                .child(section("Files", Some(u(md, "files"))))
                .child(el("ul").class("list").children(files))
                .into()
        }
        None => el("aside")
            .class("rail mapside")
            .child(el("h2").text("Module map"))
            .child(el("p").class("muted").text(format!(
                "Directories {} levels deep. Arrows point from a module to what it depends on; thicker means more references. Select a module for details.",
                u(&m.data, "depth")
            )))
            .into(),
    };
    el("div")
        .class("graphpage")
        .child(
            el("div")
                .class("pagehead")
                .child(el("h1").text("Module map"))
                .opt(
                    (!modules.is_empty())
                        .then(|| crate::flowviews::export_buttons("map-svg", "codegraph-map")),
                ),
        )
        .child(
            el("div")
                .class("map")
                .child(el("div").class("mapcanvas").child(if modules.is_empty() {
                    empty("Nothing indexed yet.")
                } else {
                    graph.into()
                }))
                .child(side),
        )
        .into()
}

// --------------------------------------------------------------------- flow

fn flow_slot(app: &App, slot: Slot, label: &str) -> N {
    let st = &app.flow[slot as usize];
    let body: N = match &st.id {
        Some(id) => el("div")
            .class("picked")
            .child(
                el("a")
                    .class("sym")
                    .href(symbol_href(id))
                    .child(el("span").class("nm").text(st.label.clone())),
            )
            .child(
                el("button")
                    .attr("type", "button")
                    .class("quiet small")
                    .attr("aria-label", format!("Clear {label}"))
                    .on_click(Msg::FlowClear(slot))
                    .text("×"),
            )
            .into(),
        None => {
            let results: Vec<N> = st
                .results
                .iter()
                .map(|r| {
                    let id = s(r, "id").to_string();
                    el("li")
                        .child(
                            el("button")
                                .attr("type", "button")
                                .class("pick")
                                .on_click(Msg::FlowPick(slot, id))
                                .child(glyph(s(r, "kind")))
                                .child(el("span").class("nm").text(s(r, "name")))
                                .child(el("span").class("loc").text(format!(
                                    "{}:{}",
                                    s(r, "file"),
                                    u(r, "line")
                                ))),
                        )
                        .into()
                })
                .collect();
            el("div")
                .class("picker")
                .child(
                    el("input")
                        .attr("type", "search")
                        .attr("placeholder", "Search a symbol…")
                        .attr("aria-label", label)
                        .attr("value", st.query.clone())
                        .on_input(move |q| Msg::FlowInput(slot, q)),
                )
                .opt(
                    (!results.is_empty())
                        .then(|| el("ul").class("dropdown").children(results).into()),
                )
                .into()
        }
    };
    el("div")
        .class("slot")
        .child(el("span").class("muted small").text(label))
        .child(body)
        .into()
}

fn flow(app: &App, v: Option<&Value>) -> N {
    let pickers = el("div")
        .class("flowpick card")
        .child(flow_slot(app, Slot::From, "From"))
        .child(
            el("button")
                .attr("type", "button")
                .class("quiet")
                .attr("aria-label", "Swap")
                .on_click(Msg::FlowSwap)
                .text("⇄"),
        )
        .child(flow_slot(app, Slot::To, "To"));
    let result: N = match v {
        None => empty("Pick two symbols to see how one reaches the other through calls, references and routes."),
        Some(v) if v["found"].as_bool() != Some(true) => {
            empty("No path within 8 hops in either direction.")
        }
        Some(v) => {
            let steps = arr(v, "steps");
            let mut chain: Vec<N> = Vec::new();
            for (i, st) in steps.iter().enumerate() {
                if i > 0 {
                    chain.push(
                        el("div")
                            .class("hop")
                            .child(el("span").class("arrow").attr("aria-hidden", "true").text("↓"))
                            .child(el("span").class("via").text(s(st, "via")))
                            .opt(st.get("line").and_then(Value::as_u64).map(|l| {
                                let prev = &steps[i - 1]["node"];
                                el("a").class("loc").href(file_href(s(prev, "file"), Some(l))).text(format!("line {l}")).into()
                            }))
                            .into(),
                    );
                }
                let n = &st["node"];
                chain.push(el("div").class("flowcard").child(sym_link(n)).child(loc_link(s(n, "file"), u(n, "line"))).into());
            }
            el("div")
                .class("chain")
                .opt((v["reversed"].as_bool() == Some(true)).then(|| {
                    el("p").class("muted small").text("No path that way; this is the path in the other direction.").into()
                }))
                .children(chain)
                .into()
        }
    };
    el("div")
        .class("flow")
        .child(el("h1").text("Flow"))
        .child(pickers)
        .child(result)
        .into()
}

// ----------------------------------------------------------- lists & trails

fn dead(v: &Value) -> N {
    let files: Vec<N> = arr(v, "files")
        .iter()
        .map(|f| {
            el("section")
                .class("card")
                .child(
                    el("h3").class("sec").child(
                        el("a")
                            .class("loc")
                            .href(file_href(s(f, "file"), None))
                            .text(s(f, "file")),
                    ),
                )
                .child(
                    el("ul")
                        .class("list")
                        .children(arr(f, "symbols").iter().map(|sym| {
                            el("li")
                                .child(sym_link(sym))
                                .child(
                                    el("span")
                                        .class("muted")
                                        .text(format!("L{}", u(sym, "line"))),
                                )
                                .into()
                        })),
                )
                .into()
        })
        .collect();
    el("div")
        .class("listpage")
        .child(el("h1").text("Dead code"))
        .child(el("p").class("muted").text(format!(
            "{} that nothing references. Exported symbols, tests and conventional entry points are left out; dynamic use (reflection, string lookups) can hide real callers.",
            plural(u(v, "total"), "symbol", "symbols")
        )))
        .children(if files.is_empty() { vec![empty("Nothing found.")] } else { files })
        .into()
}

fn routes(v: &Value) -> N {
    let rows: Vec<N> = arr(v, "routes")
        .iter()
        .map(|r| {
            let handlers: Vec<N> = arr(r, "handlers")
                .iter()
                .map(|h| sym_link(h).into())
                .collect();
            el("tr")
                .child(el("td").child(sym_link(r)))
                .child(el("td").class("muted").text(s(r, "framework")))
                .child(el("td").children(if handlers.is_empty() {
                    vec![text("—")]
                } else {
                    handlers
                }))
                .child(el("td").class("num").text(fmt_num(u(r, "requests"))))
                .child(el("td").child(loc_link(s(r, "file"), u(r, "line"))))
                .into()
        })
        .collect();
    el("div")
        .class("listpage")
        .child(el("h1").text("Routes"))
        .child(el("p").class("muted").text(format!("{} detected from framework conventions. Requests counts calls from the project's own HTTP clients.", plural(u(v, "total"), "route", "routes"))))
        .child(if rows.is_empty() {
            empty("No framework routes detected.")
        } else {
            el("div").class("tablewrap").child(
                el("table")
                    .child(el("thead").child(el("tr")
                        .child(el("th").text("Route"))
                        .child(el("th").text("Framework"))
                        .child(el("th").text("Handlers"))
                        .child(el("th").class("num").text("Requests"))
                        .child(el("th").text("Defined at"))))
                    .child(el("tbody").children(rows)),
            ).into()
        })
        .into()
}

fn trails(v: &Value) -> N {
    let list: Vec<N> = v
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
        .iter()
        .map(|t| {
            let steps: Vec<N> = arr(t, "steps")
                .iter()
                .enumerate()
                .flat_map(|(i, st)| {
                    let mut out: Vec<N> = Vec::new();
                    if i > 0 {
                        out.push(el("span").class("sep").text("›").into());
                    }
                    out.push(sym_link(st).class("sym chip").into());
                    out
                })
                .collect();
            el("section")
                .class("card")
                .child(
                    el("div")
                        .class("trailhead")
                        .child(el("h3").class("sec").text(s(t, "name")))
                        .child(
                            el("span").class("muted small").text(
                                s(t, "createdAt")
                                    .replace('T', " ")
                                    .trim_end_matches('Z')
                                    .to_string(),
                            ),
                        )
                        .child(
                            el("button")
                                .attr("type", "button")
                                .class("quiet small")
                                .on_click(Msg::DeleteTrail(s(t, "id").to_string()))
                                .text("Delete"),
                        ),
                )
                .child(el("div").class("chips").children(steps))
                .into()
        })
        .collect();
    el("div")
        .class("listpage")
        .child(el("h1").text("Saved trails"))
        .child(el("p").class("muted").text(
            "Paths you walked through the code, saved with the project in .codegraph/trails.json.",
        ))
        .children(if list.is_empty() {
            vec![empty(
                "No saved trails. Visit a few symbols and press “Save trail”.",
            )]
        } else {
            list
        })
        .into()
}
