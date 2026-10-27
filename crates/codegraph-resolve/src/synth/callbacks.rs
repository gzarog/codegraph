//! Callbacks a framework or a field invokes later: observers registered
//! into a field and fired from it, closures appended to a collection, a
//! re-render after `setState`, a middleware chain, a page's loader.

use super::{Ctx, LineIndex, Proposal, Sink};
use codegraph_core::{EdgeKind, Language, NodeKind};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// Compile a pattern with ASCII word boundaries, which keep the regex
/// engine on its fast path over non-ASCII text.
fn re(p: &str) -> Regex {
    Regex::new(&p.replace(r"\b", r"(?-u:\b)")).unwrap()
}

const MAX_PER_CHANNEL: usize = 40;

static REGISTRAR_NAME: LazyLock<Regex> = LazyLock::new(|| {
    re(r"^(?:on[A-Z]\w*|subscribe|addListener|addEventListener|register|watch|listen|addCallback)$")
});
static DISPATCHER_NAME: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?i)emit|trigger|notify|dispatch|fire|publish|flush"));
static REGISTRAR_FIELD: LazyLock<Regex> = LazyLock::new(|| re(r"this\.(\w+)\.(?:add|push|set)\("));
static FOR_OF_FIELD: LazyLock<Regex> =
    LazyLock::new(|| re(r"\bof\s+(?:Array\.from\(\s*)?this\.(\w+)"));
static ANY_CALL: LazyLock<Regex> = LazyLock::new(|| re(r"\b\w+\s*\("));
static FOR_EACH_FIELD: LazyLock<Regex> = LazyLock::new(|| re(r"this\.(\w+)\.forEach\("));

fn body(ctx: &Ctx<'_>, i: usize) -> Option<String> {
    let n = ctx.node(i);
    ctx.lines(&n.file_path, n.start_line, n.end_line)
}

fn callables<'c>(ctx: &'c Ctx<'_>) -> impl Iterator<Item = usize> + 'c {
    (0..ctx.nodes.len())
        .filter(|&i| matches!(ctx.node(i).kind, NodeKind::Method | NodeKind::Function))
}

/// Observers stored in a field: `onUpdate(fn) { this.listeners.add(fn) }`
/// and `notify() { this.listeners.forEach(…) }` in the same file. Each
/// function passed to the registrar is reached from the dispatcher.
pub fn field_channels(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let Some(store) = ctx.store else { return };
    let mut registrars = Vec::new();
    let mut dispatchers: Vec<(usize, String)> = Vec::new();
    for i in callables(ctx) {
        let name = &ctx.node(i).name;
        let (reg, disp) = (
            REGISTRAR_NAME.is_match(name),
            DISPATCHER_NAME.is_match(name),
        );
        if !reg && !disp {
            continue;
        }
        let Some(src) = body(ctx, i) else { continue };
        if reg {
            if let Some(c) = REGISTRAR_FIELD.captures(&src) {
                registrars.push((i, c[1].to_string()));
            }
        }
        if disp {
            let field = FOR_OF_FIELD
                .captures(&src)
                .filter(|_| ANY_CALL.is_match(&src))
                .or_else(|| FOR_EACH_FIELD.captures(&src))
                .map(|c| c[1].to_string());
            if let Some(f) = field {
                dispatchers.push((i, f));
            }
        }
    }
    let mut sink = Sink::new(out);
    for (reg, field) in registrars {
        let r = ctx.node(reg);
        let channel: Vec<usize> = dispatchers
            .iter()
            .filter(|(d, f)| *f == field && ctx.node(*d).file_path == r.file_path)
            .map(|(d, _)| *d)
            .collect();
        if channel.is_empty() {
            continue;
        }
        let arg = re(&format!(
            r"{}\s*\(\s*(this\.\w+|\w+)\s*[,)]",
            regex::escape(&r.name)
        ));
        let Ok(calls) = store.incoming_edges(&r.id, &[EdgeKind::Calls]) else {
            continue;
        };
        let mut added = 0;
        for e in calls {
            let (Some(line), Some(caller)) = (e.line, ctx.index_of(&e.source)) else {
                continue;
            };
            let c = ctx.node(caller);
            let Some(text) = ctx.lines(&c.file_path, line, line) else {
                continue;
            };
            let Some(a) = arg.captures(&text) else {
                continue;
            };
            let passed = a[1].strip_prefix("this.").unwrap_or(&a[1]).to_string();
            // The function value passed there, as resolution bound it.
            let Ok(refs) = store.outgoing_edges(&c.id, &[EdgeKind::References]) else {
                continue;
            };
            let fns: Vec<usize> = refs
                .iter()
                .filter(|x| {
                    x.line == Some(line)
                        && x.metadata
                            .as_ref()
                            .is_some_and(|m| m.get("functionRef").is_some_and(|f| f == true))
                })
                .filter_map(|x| ctx.index_of(&x.target))
                .filter(|&t| {
                    let n = ctx.node(t);
                    n.name == passed && matches!(n.kind, NodeKind::Method | NodeKind::Function)
                })
                .collect();
            let [f] = fns.as_slice() else { continue };
            for &d in &channel {
                if added < MAX_PER_CHANNEL && sink.push(d, *f, ctx.node(d).start_line, &r.name) {
                    added += 1;
                }
            }
        }
    }
}

static CC_DISPATCH: LazyLock<Regex> =
    LazyLock::new(|| re(r"(\w+)\.forEach\s*\{\s*(?:\$0|it)\s*\("));
static CC_APPEND_WRITE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(\w+)\.write\s*\{\s*\$0(?:\.(\w+))?\.(?:append|add|push|insert)\s*\("));
static CC_APPEND: LazyLock<Regex> = LazyLock::new(|| re(r"(\w+)\.(?:append|add|push|insert)\s*\("));
const CC_FANOUT: usize = 8;

/// Swift/Kotlin closure collections: `validators.forEach { $0(…) }` reaches
/// every method that appends to a `validators` collection.
pub fn closure_collections(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Swift, Language::Kotlin]) {
        return;
    }
    let mut dispatchers: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
    let mut registrars: HashMap<String, Vec<(usize, u32)>> = HashMap::new();
    let add = |map: &mut HashMap<String, Vec<(usize, u32)>>, field: &str, i: usize, line: u32| {
        if field.chars().all(|c| c.is_ascii_digit()) {
            return;
        }
        let v = map.entry(field.to_string()).or_default();
        if !v.iter().any(|(x, _)| *x == i) {
            v.push((i, line));
        }
    };
    for i in callables(ctx) {
        let n = ctx.node(i);
        if !matches!(n.language, Language::Swift | Language::Kotlin) {
            continue;
        }
        let Some(src) = body(ctx, i) else { continue };
        let for_each = src.contains(".forEach");
        let appends = [".append(", ".add(", ".push(", ".insert("]
            .iter()
            .any(|p| src.contains(p));
        if !for_each && !appends {
            continue;
        }
        let lines = LineIndex::new(&src);
        let abs = |off: usize| n.start_line + lines.line(off) - 1;
        if for_each {
            for c in CC_DISPATCH.captures_iter(&src) {
                add(&mut dispatchers, &c[1], i, abs(c.get(0).unwrap().start()));
            }
        }
        if appends {
            for c in CC_APPEND_WRITE.captures_iter(&src) {
                let field = c.get(2).unwrap_or_else(|| c.get(1).unwrap()).as_str();
                add(&mut registrars, field, i, abs(c.get(0).unwrap().start()));
            }
            for c in CC_APPEND.captures_iter(&src) {
                add(&mut registrars, &c[1], i, abs(c.get(0).unwrap().start()));
            }
        }
    }
    let mut sink = Sink::new(out);
    let mut fields: Vec<&String> = dispatchers.keys().collect();
    fields.sort();
    for field in fields {
        let ds = &dispatchers[field];
        let Some(rs) = registrars.get(field) else {
            continue;
        };
        if ds.len() > CC_FANOUT || rs.len() > CC_FANOUT {
            continue;
        }
        for &(d, line) in ds {
            for &(r, _) in rs {
                sink.push(d, r, line, field);
            }
        }
    }
}

static SET_STATE_THIS: LazyLock<Regex> = LazyLock::new(|| re(r"this\.setState\s*\("));
static SET_STATE: LazyLock<Regex> = LazyLock::new(|| re(r"\bsetState\s*\("));

/// Methods of a class that call `setState` reach its `render`/`build`.
fn rerender(
    ctx: &Ctx<'_>,
    out: &mut Vec<Proposal>,
    target: &str,
    setter: &Regex,
    only: Option<Language>,
) {
    let mut sink = Sink::new(out);
    let owners: HashSet<usize> = (0..ctx.nodes.len())
        .filter(|&c| {
            ctx.node(c).kind == NodeKind::Class && only.is_none_or(|l| ctx.node(c).language == l)
        })
        .collect();
    for cls in owners {
        let methods = ctx.methods_of(cls);
        let Some(&render) = methods.iter().find(|&&m| ctx.node(m).name == target) else {
            continue;
        };
        let mut added = 0;
        for &m in &methods {
            if added >= MAX_PER_CHANNEL || m == render {
                continue;
            }
            if body(ctx, m).is_some_and(|src| setter.is_match(&src))
                && sink.push(m, render, ctx.node(m).start_line, "setState")
            {
                added += 1;
            }
        }
    }
}

pub fn react_render(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    rerender(ctx, out, "render", &SET_STATE_THIS, None);
}

pub fn flutter_build(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if ctx.has_language(&[Language::Dart]) {
        rerender(ctx, out, "build", &SET_STATE, Some(Language::Dart));
    }
}

static GIN_DISPATCH: LazyLock<Regex> = LazyLock::new(|| re(r"\.handlers\s*\[[^\]]*\]\s*\("));
static GIN_REGISTER: LazyLock<Regex> =
    LazyLock::new(|| re(r"\.(?:Use|GET|POST|PUT|PATCH|DELETE|OPTIONS|HEAD|Any|Handle)\s*\("));
static TRAILING_CALL: LazyLock<Regex> = LazyLock::new(|| re(r"\(\s*\)$"));
static TAIL_IDENT: LazyLock<Regex> = LazyLock::new(|| re(r"(?:\.|^)([A-Za-z_]\w*)$"));

fn balanced_args(s: &str, open: usize) -> Option<&str> {
    let mut depth = 0;
    for (i, c) in s[open..].char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_args(args: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in args.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&args[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if !args[start..].trim().is_empty() {
        out.push(&args[start..]);
    }
    out
}

/// A Gin-style chain (`c.handlers[c.index](c)`) reaches every handler
/// registered with `.Use(…)`, `.GET(…)` and the like.
pub fn gin_middleware(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Go]) {
        return;
    }
    let dispatchers: Vec<usize> = callables(ctx)
        .filter(|&i| {
            let n = ctx.node(i);
            n.kind == NodeKind::Method
                && n.language == Language::Go
                && body(ctx, i).is_some_and(|s| GIN_DISPATCH.is_match(&s))
        })
        .collect();
    if dispatchers.is_empty() {
        return;
    }
    let mut registered: Vec<String> = Vec::new();
    for file in ctx.files_of(&[Language::Go]) {
        let Some(safe) = ctx.stripped(file, Language::Go) else {
            continue;
        };
        for m in GIN_REGISTER.find_iter(&safe) {
            let Some(args) = balanced_args(&safe, m.end() - 1) else {
                continue;
            };
            for arg in split_args(args) {
                let a = arg.trim();
                let a = TRAILING_CALL.replace(a, "");
                if a.is_empty() || a.starts_with(['"', '`']) || a.starts_with("func") {
                    continue;
                }
                if let Some(c) = TAIL_IDENT.captures(&a) {
                    if !registered.iter().any(|r| r == &c[1]) {
                        registered.push(c[1].to_string());
                    }
                }
            }
        }
    }
    let mut sink = Sink::new(out);
    for &d in &dispatchers {
        let mut added = 0;
        for name in &registered {
            if added >= MAX_PER_CHANNEL {
                break;
            }
            let handler = ctx.named(name).find(|&h| {
                let n = ctx.node(h);
                n.language == Language::Go
                    && matches!(n.kind, NodeKind::Function | NodeKind::Method)
            });
            if let Some(h) = handler {
                if sink.push(d, h, ctx.node(d).start_line, name) {
                    added += 1;
                }
            }
        }
    }
}

static SVELTEKIT_PAGE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^(.*/)?(\+(?:page|layout))\.svelte$"));

/// A SvelteKit `+page.svelte` reaches the `load`, `actions` and functions
/// of its sibling `+page(.server).ts`.
pub fn sveltekit_load(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Svelte]) {
        return;
    }
    let all: HashSet<&str> = ctx.files.iter().map(|(p, _)| p.as_str()).collect();
    let mut sink = Sink::new(out);
    for (file, _) in &ctx.files {
        let Some(c) = SVELTEKIT_PAGE.captures(file) else {
            continue;
        };
        let dir = c.get(1).map_or("", |m| m.as_str());
        let prefix = &c[2];
        let Some(page) = ctx
            .in_file(file)
            .iter()
            .copied()
            .find(|&i| ctx.node(i).kind == NodeKind::Component)
        else {
            continue;
        };
        for ext in [".server.ts", ".server.js", ".ts", ".js"] {
            let loader = format!("{dir}{prefix}{ext}");
            if !all.contains(loader.as_str()) {
                continue;
            }
            for &h in ctx.in_file(&loader) {
                let n = ctx.node(h);
                let named = matches!(
                    n.kind,
                    NodeKind::Function | NodeKind::Method | NodeKind::Constant | NodeKind::Variable
                ) && matches!(n.name.as_str(), "load" | "actions");
                if named || matches!(n.kind, NodeKind::Function | NodeKind::Method) {
                    sink.push(page, h, ctx.node(page).start_line, &n.name);
                }
            }
        }
    }
}
