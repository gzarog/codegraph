//! JavaScript/TypeScript state and registry dispatch: Redux thunks, RTK
//! Query hooks, Pinia and Vuex stores, and object-literal handler registries.

use super::{Ctx, LineIndex, Proposal, Sink};
use codegraph_core::{Language, NodeKind};
use regex::Regex;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;

/// Compile a pattern with ASCII word boundaries, which keep the regex
/// engine on its fast path over non-ASCII text.
fn re(p: &str) -> Regex {
    Regex::new(&p.replace(r"\b", r"(?-u:\b)")).unwrap()
}

const JS: &[Language] = &[
    Language::TypeScript,
    Language::JavaScript,
    Language::Tsx,
    Language::Jsx,
];
const JS_AND_SFC: &[Language] = &[
    Language::TypeScript,
    Language::JavaScript,
    Language::Tsx,
    Language::Jsx,
    Language::Vue,
];

fn is_ident_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// Whether `at` begins an identifier (nothing identifier-like before it).
fn at_ident_start(src: &str, at: usize) -> bool {
    !src[..at].chars().next_back().is_some_and(is_ident_char)
}

// ── Redux: a thunk's `dispatch(other(…))` → the dispatched thunk ──

static THUNK_DECL: LazyLock<Regex> = LazyLock::new(|| re(r"create(?:Async)?Thunk"));
static THUNK_DISPATCH: LazyLock<Regex> =
    LazyLock::new(|| re(r"\bdispatch\s*\(\s*([A-Za-z_]\w*)\s*[(),]"));

pub fn redux_thunks(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(JS) {
        return;
    }
    let mut sink = Sink::new(out);
    for (i, n) in ctx.nodes.iter().enumerate() {
        if n.kind != NodeKind::Constant
            || !n
                .signature
                .as_deref()
                .is_some_and(|s| THUNK_DECL.is_match(s))
        {
            continue;
        }
        let Some(body) = ctx.lines(&n.file_path, n.start_line, n.end_line) else {
            continue;
        };
        let safe = codegraph_frameworks::strip::strip_comments(&body, n.language);
        let lines = LineIndex::new(&safe);
        let mut added = 0;
        for c in THUNK_DISPATCH.captures_iter(&safe) {
            if added >= 24 {
                break;
            }
            let name = &c[1];
            if name == n.name {
                continue;
            }
            let cands: Vec<usize> = ctx
                .named(name)
                .filter(|&t| {
                    matches!(
                        ctx.node(t).kind,
                        NodeKind::Constant | NodeKind::Function | NodeKind::Method
                    )
                })
                .collect();
            // A thunk or action creator over a same-named helper.
            let target = cands
                .iter()
                .find(|&&t| {
                    ctx.node(t)
                        .signature
                        .as_deref()
                        .is_some_and(|s| THUNK_DECL.is_match(s))
                })
                .or_else(|| {
                    cands
                        .iter()
                        .find(|&&t| ctx.node(t).kind == NodeKind::Constant)
                })
                .or_else(|| {
                    cands
                        .iter()
                        .find(|&&t| ctx.node(t).file_path == n.file_path)
                })
                .or(cands.first());
            let Some(&t) = target else { continue };
            let line = n.start_line + lines.line(c.get(0).unwrap().start()) - 1;
            if sink.push(i, t, line, name) {
                added += 1;
            }
        }
    }
}

// ── Object-literal registries: `this.commands[cmd](…)` → each registered handler ──

static REGISTRY_ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    re(r"(?:(?:const|let|var)\s+([A-Za-z_$][\w$]*)|((?:this\.)?[A-Za-z_$][\w$]*))\s*=\s*\{")
});
static REGISTRY_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?:\bnew\s+)?((?:this\.)?[A-Za-z_$][\w$]*)\s*\[\s*([A-Za-z_$][\w$.]*)\s*\]\s*(?:\(|\.[A-Za-z_$])",
    )
});
static CHAINED_AFTER_CALL: LazyLock<Regex> =
    LazyLock::new(|| re(r"\]\s*\([^)]*\)\s*\.\s*([A-Za-z_$][\w$]*)"));
static CHAINED: LazyLock<Regex> = LazyLock::new(|| re(r"\]\s*\.\s*([A-Za-z_$][\w$]*)"));
static REGISTRY_ENTRY: LazyLock<Regex> =
    LazyLock::new(|| re(r#"^\s*(?:\[[^\]]+\]|['"]?[\w$]+['"]?)\s*:\s*([A-Za-z_$][\w$]*)\s*$"#));
static COMPUTED_ACCESS: LazyLock<Regex> = LazyLock::new(|| re(r"[\w$]\s*\[\s*[A-Za-z_$]"));
const CLASS_ENTRY: &[&str] = &[
    "execute", "run", "handle", "perform", "process", "call", "apply", "dispatch",
];

fn brace_body(src: &str, open: usize) -> Option<&str> {
    let mut depth = 0;
    for (i, ch) in src[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&src[open + 1..open + i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Depth-0 `key: Identifier` entries of an object literal's body.
fn registry_entries(body: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, ch) in body.char_indices() {
        match ch {
            '{' | '(' | '[' => depth += 1,
            '}' | ')' | ']' => depth -= 1,
            ',' if depth == 0 => {
                segs.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    segs.push(&body[start..]);
    let mut names: Vec<String> = Vec::new();
    for s in segs {
        if let Some(c) = REGISTRY_ENTRY.captures(s) {
            let n = c[1].to_string();
            if n.len() >= 3 && !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names
}

/// A registered name's callable entry: a function, a class's `execute`-like
/// method (the one chained at the site first), or a method.
fn registry_handler(ctx: &Ctx<'_>, name: &str, chained: Option<&str>) -> Option<usize> {
    let cands: Vec<usize> = ctx.named(name).collect();
    if let Some(&f) = cands
        .iter()
        .find(|&&i| ctx.node(i).kind == NodeKind::Function)
    {
        return Some(f);
    }
    if let Some(&cls) = cands
        .iter()
        .find(|&&i| matches!(ctx.node(i).kind, NodeKind::Class | NodeKind::Struct))
    {
        let want = chained.filter(|c| CLASS_ENTRY.contains(c));
        let entry = want
            .and_then(|w| ctx.member_of(cls, NodeKind::Method, w))
            .or_else(|| {
                CLASS_ENTRY
                    .iter()
                    .find_map(|m| ctx.member_of(cls, NodeKind::Method, m))
            })
            .or_else(|| ctx.member_of(cls, NodeKind::Method, "constructor"));
        return Some(entry.unwrap_or(cls));
    }
    cands
        .into_iter()
        .find(|&i| ctx.node(i).kind == NodeKind::Method)
}

pub fn object_registries(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let mut sink = Sink::new(out);
    let norm = |r: &str| r.strip_prefix("this.").unwrap_or(r).to_string();
    for (file, lang) in &ctx.files {
        if !JS.contains(lang) {
            continue;
        }
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !COMPUTED_ACCESS.is_match(&src) {
            continue;
        }
        // Minified bundles: long average lines.
        if src.len() / (src.matches('\n').count() + 1) > 200 {
            continue;
        }
        let Some(safe) = ctx.stripped(file, *lang) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        let mut dispatches = Vec::new();
        for c in REGISTRY_DISPATCH.captures_iter(&safe) {
            let r = c.get(1).unwrap();
            if !r.as_str().starts_with("this.") && !at_ident_start(&safe, r.start()) {
                continue;
            }
            let m = c.get(0).unwrap();
            let end = (m.start() + 160).min(safe.len());
            let end = (end..=safe.len())
                .find(|&e| safe.is_char_boundary(e))
                .unwrap_or(safe.len());
            let win = &safe[m.start()..end];
            let chained = CHAINED_AFTER_CALL
                .captures(win)
                .or_else(|| CHAINED.captures(win))
                .map(|x| x[1].to_string());
            dispatches.push((norm(r.as_str()), lines.line(m.start()), chained));
        }
        if dispatches.is_empty() {
            continue;
        }
        let mut registries: HashMap<String, (Vec<String>, u32)> = HashMap::new();
        for c in REGISTRY_ASSIGN.captures_iter(&safe) {
            let lhs = c.get(1).or_else(|| c.get(2)).unwrap();
            if c.get(2).is_some()
                && !lhs.as_str().starts_with("this.")
                && !at_ident_start(&safe, lhs.start())
            {
                continue;
            }
            let lhs = norm(lhs.as_str());
            if registries.contains_key(&lhs) || !dispatches.iter().any(|d| d.0 == lhs) {
                continue;
            }
            let m = c.get(0).unwrap();
            let Some(body) = brace_body(&safe, m.end() - 1) else {
                continue;
            };
            let names = registry_entries(body);
            if names.len() >= 2 {
                registries.insert(lhs, (names, lines.line(m.start())));
            }
        }
        for (r, line, chained) in &dispatches {
            let Some((names, _)) = registries.get(r) else {
                continue;
            };
            let Some(disp) = ctx.enclosing_fn(file, *line) else {
                continue;
            };
            let mut added = 0;
            for name in names {
                if added >= 40 {
                    break;
                }
                if let Some(t) = registry_handler(ctx, name, chained.as_deref()) {
                    if sink.push(disp, t, *line, name) {
                        added += 1;
                    }
                }
            }
        }
    }
}

// ── RTK Query: a generated `useGetXQuery` hook → its `getX` endpoint ──

static RTK_HOOK: LazyLock<Regex> =
    LazyLock::new(|| re(r"^use([A-Z][A-Za-z0-9]*?)(?:Query|Mutation)$"));
const RTK_SIGNATURE: &str = "= RTK Query generated hook";

fn rtk_endpoint(hook: &str) -> Option<String> {
    let c = RTK_HOOK.captures(hook)?;
    let mid = c[1].strip_prefix("Lazy").unwrap_or(&c[1]);
    let mut chars = mid.chars();
    let first = chars.next()?;
    Some(first.to_lowercase().chain(chars).collect())
}

pub fn rtk_query(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let mut sink = Sink::new(out);
    for (i, hook) in ctx.nodes.iter().enumerate() {
        if hook.kind != NodeKind::Function || hook.signature.as_deref() != Some(RTK_SIGNATURE) {
            continue;
        }
        let Some(endpoint) = rtk_endpoint(&hook.name) else {
            continue;
        };
        let target = ctx.named(&endpoint).find(|&t| {
            let n = ctx.node(t);
            n.kind == NodeKind::Function && n.file_path == hook.file_path
        });
        if let Some(t) = target {
            sink.push(i, t, hook.start_line, &endpoint);
        }
    }
}

// ── Pinia: `const s = useXStore(); s.action()` → the action in the store's file ──

static PINIA_FACTORY: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b(?:export\s+)?const\s+(\w+)\s*=\s*defineStore\s*\("));
static PINIA_BIND: LazyLock<Regex> =
    LazyLock::new(|| re(r"\bconst\s+(\w+)\s*=\s*(?:await\s+)?(\w+)\s*\("));
static PINIA_CALL: LazyLock<Regex> = LazyLock::new(|| re(r"(\w+)\s*\.\s*(\w+)\s*\("));

/// The component a single-file component's top-level code belongs to.
fn file_component(ctx: &Ctx<'_>, file: &str) -> Option<usize> {
    ctx.in_file(file)
        .iter()
        .copied()
        .find(|&i| ctx.node(i).kind == NodeKind::Component)
}

pub fn pinia(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let mut factories: HashMap<String, String> = HashMap::new();
    for file in ctx.files_of(JS_AND_SFC) {
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains("defineStore") {
            continue;
        }
        for c in PINIA_FACTORY.captures_iter(&src) {
            factories.insert(c[1].to_string(), file.to_string());
        }
    }
    if factories.is_empty() {
        return;
    }
    let mut sink = Sink::new(out);
    for (file, lang) in &ctx.files {
        if !JS_AND_SFC.contains(lang) {
            continue;
        }
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains("Store") {
            continue;
        }
        let Some(safe) = ctx.stripped(file, *lang) else {
            continue;
        };
        let stores: HashMap<&str, &str> = PINIA_BIND
            .captures_iter(&safe)
            .filter_map(|c| {
                let f = factories.get(&c[2])?;
                Some((c.get(1).unwrap().as_str(), f.as_str()))
            })
            .collect();
        if stores.is_empty() {
            continue;
        }
        let lines = LineIndex::new(&safe);
        let fallback = file_component(ctx, file);
        let mut added = 0;
        for c in PINIA_CALL.captures_iter(&safe) {
            if added >= 80 {
                break;
            }
            let Some(store_file) = stores.get(&c[1]) else {
                continue;
            };
            let line = lines.line(c.get(0).unwrap().start());
            let Some(disp) = ctx.enclosing_fn(file, line).or(fallback) else {
                continue;
            };
            let target = ctx.named(&c[2]).find(|&t| {
                let n = ctx.node(t);
                n.kind == NodeKind::Function && n.file_path == *store_file
            });
            if let Some(t) = target {
                if sink.push(disp, t, line, &c[2]) {
                    added += 1;
                }
            }
        }
    }
}

// ── Vuex: `dispatch('user/login')` / `commit('SET_TOKEN')` → the action or mutation ──

static VUEX_DISPATCH: LazyLock<Regex> =
    LazyLock::new(|| re(r#"\b(?:dispatch|commit)\s*\(\s*['"]([A-Za-z][\w/]*)['"]"#));
static VUEX_SIGNAL: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\bdefineStore\b|\bcreateStore\b|\bVuex\b|\bmutations\b|\bactions\b|\bgetters\b|\bnamespaced\b",
    )
});

/// Whether a path has `seg` as a directory or a file stem.
fn path_has_segment(path: &str, seg: &str) -> bool {
    path.split(['/', '\\'])
        .any(|part| part == seg || part.split('.').next() == Some(seg))
}

pub fn vuex(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let store_files: RefCell<HashMap<String, bool>> = RefCell::default();
    let is_store_file = |file: &str| -> bool {
        if let Some(&v) = store_files.borrow().get(file) {
            return v;
        }
        let v = ctx.source(file).is_some_and(|src| {
            let mut kinds: Vec<&str> = VUEX_SIGNAL.find_iter(&src).map(|m| m.as_str()).collect();
            kinds.sort_unstable();
            kinds.dedup();
            kinds.len() >= 2
        });
        store_files.borrow_mut().insert(file.to_string(), v);
        v
    };
    let resolve = |key: &str, from_file: &str| -> Option<usize> {
        let segs: Vec<&str> = key.split('/').collect();
        let action = *segs.last()?;
        let cands: Vec<usize> = ctx
            .named(action)
            .filter(|&i| {
                let n = ctx.node(i);
                n.kind == NodeKind::Function && is_store_file(&n.file_path)
            })
            .collect();
        let only = (cands.len() == 1).then(|| cands[0]);
        if segs.len() > 1 {
            let module = segs[segs.len() - 2];
            return cands
                .iter()
                .copied()
                .find(|&i| path_has_segment(&ctx.node(i).file_path, module))
                .or(only);
        }
        cands
            .iter()
            .copied()
            .find(|&i| ctx.node(i).file_path == from_file)
            .or(only)
    };
    let mut sink = Sink::new(out);
    for (file, lang) in &ctx.files {
        if !JS_AND_SFC.contains(lang) {
            continue;
        }
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains("dispatch(") && !src.contains("commit(") {
            continue;
        }
        let Some(safe) = ctx.stripped(file, *lang) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        let fallback = file_component(ctx, file);
        let mut added = 0;
        for c in VUEX_DISPATCH.captures_iter(&safe) {
            if added >= 120 {
                break;
            }
            let line = lines.line(c.get(0).unwrap().start());
            let Some(disp) = ctx.enclosing_fn(file, line).or(fallback) else {
                continue;
            };
            if let Some(t) = resolve(&c[1], file) {
                if sink.push(disp, t, line, &c[1]) {
                    added += 1;
                }
            }
        }
    }
}
