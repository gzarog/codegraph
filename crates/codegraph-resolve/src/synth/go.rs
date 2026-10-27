//! Go functions held in struct fields: `d.refreshf(ctx)` reaches each
//! function or method value the field is given, in a composite literal
//! (`Options{RefreshF: d.refresh}`) or an assignment (`x.F = v`), and through
//! one copy from another field (`Discovery{refreshf: opts.RefreshF}`).

use super::{Ctx, LineIndex, Proposal, Sink};
use codegraph_core::{Language, NodeKind};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

fn re(p: &str) -> Regex {
    Regex::new(&p.replace(r"\b", r"(?-u:\b)")).unwrap()
}

const MAX_VALUES: usize = 40;
/// Field names too generic to pair across structs.
const GENERIC_FIELDS: &[&str] = &[
    "fn", "f", "handler", "h", "cb", "callback", "run", "do", "call",
];

static FIELD_LINE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^\s*([A-Za-z_]\w*(?:\s*,\s*[A-Za-z_]\w*)*)\s+(\*?[\w.]+|func\b.*)"));
static FUNC_TYPE_DECL: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?m)^\s*type\s+([A-Za-z_]\w*)\s+func\b"));
static COMPOSITE: LazyLock<Regex> =
    LazyLock::new(|| re(r"&?\b(?:[A-Za-z_]\w*\s*\.\s*)?([A-Z_a-z]\w*)\s*\{"));
static KEYED_VALUE: LazyLock<Regex> =
    LazyLock::new(|| re(r"(?s)^\s*([A-Za-z_]\w*)\s*:\s*(.+?)\s*$"));
static ASSIGN: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"\b([A-Za-z_]\w*(?:\s*\.\s*[A-Za-z_]\w*)?)\s*\.\s*([A-Za-z_]\w*)\s*=\s*([A-Za-z_][\w.]*)\s*(?:\n|;|$)",
    )
});
static SELECTOR_CALL: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b([A-Za-z_]\w*(?:\s*\.\s*[A-Za-z_]\w*)?)\s*\.\s*([A-Za-z_]\w*)\s*\("));
static PLAIN_VALUE: LazyLock<Regex> =
    LazyLock::new(|| re(r"^([A-Za-z_]\w*)(?:\s*\.\s*([A-Za-z_]\w*))?$"));
static CONVERSION: LazyLock<Regex> =
    LazyLock::new(|| re(r"^[A-Za-z_][\w.]*\s*\(\s*([A-Za-z_][\w.]*)\s*\)$"));

fn dir_of(path: &str) -> &str {
    path.rfind(['/', '\\']).map_or("", |i| &path[..i])
}

fn brace_body(src: &str, open: usize) -> Option<(usize, usize)> {
    let mut depth = 0;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((open + 1, open + i));
                }
            }
            _ => {}
        }
    }
    None
}

/// Depth-0 comma-separated items of a literal's body.
fn items(body: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut start) = (0i32, 0);
    for (i, c) in body.char_indices() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                out.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if !body[start..].trim().is_empty() {
        out.push(&body[start..]);
    }
    out
}

/// A struct declaration: its node, fields in order, and its function fields.
type StructDef = (usize, Vec<String>, HashSet<String>);

/// A struct's fields in order, and which of them hold functions.
fn struct_fields(
    ctx: &Ctx<'_>,
    s: usize,
    func_types: &HashSet<String>,
) -> Option<(Vec<String>, HashSet<String>)> {
    let n = ctx.node(s);
    let text = ctx.lines(&n.file_path, n.start_line, n.end_line)?;
    let open = text.find("struct")? + text[text.find("struct")?..].find('{')?;
    let (a, b) = brace_body(&text, open)?;
    let mut fields = Vec::new();
    let mut funcs = HashSet::new();
    for line in text[a..b].lines() {
        let line = line.split("//").next().unwrap_or("");
        let Some(c) = FIELD_LINE.captures(line) else {
            // An embedded field: named for its type.
            let t = line.trim().trim_start_matches('*');
            if !t.is_empty() {
                fields.push(t.rsplit('.').next().unwrap_or(t).to_string());
            }
            continue;
        };
        let ty = c[2].trim_start_matches('*');
        let is_func =
            ty.starts_with("func") || func_types.contains(ty.rsplit('.').next().unwrap_or(ty));
        for name in c[1].split(',') {
            let name = name.trim().to_string();
            if is_func {
                funcs.insert(name.clone());
            }
            fields.push(name);
        }
    }
    Some((fields, funcs))
}

pub fn func_fields(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Go]) {
        return;
    }
    let go_files: Vec<&str> = ctx
        .files_of(&[Language::Go])
        .filter(|f| !ctx.is_generated(f) && !f.ends_with(".pb.go"))
        .collect();
    // Defined function types: `type HandlerFunc func(…)`.
    let mut func_types = HashSet::new();
    for f in &go_files {
        if let Some(src) = ctx.source(f) {
            for c in FUNC_TYPE_DECL.captures_iter(&src) {
                func_types.insert(c[1].to_string());
            }
        }
    }
    // Structs by name: their field order and function fields.
    let mut structs: HashMap<String, Vec<StructDef>> = HashMap::new();
    let mut holders: HashMap<String, Vec<String>> = HashMap::new();
    for (i, n) in ctx.nodes.iter().enumerate() {
        if n.language != Language::Go
            || n.kind != NodeKind::Struct
            || ctx.is_generated(&n.file_path)
        {
            continue;
        }
        let Some((fields, funcs)) = struct_fields(ctx, i, &func_types) else {
            continue;
        };
        if funcs.is_empty() {
            continue;
        }
        for f in &funcs {
            holders.entry(f.clone()).or_default().push(n.name.clone());
        }
        structs
            .entry(n.name.clone())
            .or_default()
            .push((i, fields, funcs));
    }
    if structs.is_empty() {
        return;
    }
    // A value: the function it names, a method of that name, or a copy.
    enum Value {
        Fn(usize),
        Copy(String),
    }
    let resolve_value = |raw: &str, file: &str| -> Option<Value> {
        let raw = raw.trim().trim_start_matches('&');
        let raw = CONVERSION
            .captures(raw)
            .map_or(raw.to_string(), |c| c[1].to_string());
        let c = PLAIN_VALUE.captures(&raw)?;
        let (head, tail) = (c.get(1)?.as_str(), c.get(2).map(|m| m.as_str()));
        let dir = dir_of(file);
        let callable = |name: &str, same_dir: bool| -> Vec<usize> {
            ctx.named(name)
                .filter(|&i| {
                    let n = ctx.node(i);
                    n.language == Language::Go
                        && matches!(n.kind, NodeKind::Function | NodeKind::Method)
                        && (!same_dir || dir_of(&n.file_path) == dir)
                })
                .collect()
        };
        match tail {
            None => match callable(head, true).as_slice() {
                [one] => Some(Value::Fn(*one)),
                _ => None,
            },
            Some(name) => {
                // `opts.RefreshF`: a copy from another struct's function field.
                if holders.contains_key(name) && !GENERIC_FIELDS.contains(&name) {
                    return Some(Value::Copy(name.to_string()));
                }
                // `pkg.Handle` or `d.refresh`: unique by name, this package first.
                let local = callable(name, true);
                let any = callable(name, false);
                match (local.as_slice(), any.as_slice()) {
                    ([one], _) | ([], [one]) => Some(Value::Fn(*one)),
                    _ => None,
                }
            }
        }
    };
    // (struct, field) → values, with where they were put.
    let mut values: HashMap<(String, String), Vec<(Value, u32)>> = HashMap::new();
    for f in &go_files {
        let Some(safe) = ctx.stripped(f, Language::Go) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        for c in COMPOSITE.captures_iter(&safe) {
            let name = &c[1];
            let Some(defs) = structs.get(name) else {
                continue;
            };
            let m = c.get(0).unwrap();
            // `type S struct {`: a declaration, not a literal.
            if safe[..m.start()].trim_end().ends_with("struct")
                || safe[..m.start()].trim_end().ends_with("type")
            {
                continue;
            }
            let Some((a, b)) = brace_body(&safe, m.end() - 1) else {
                continue;
            };
            let line = lines.line(m.start());
            let (_, fields, funcs) = &defs[0];
            for (pos, item) in items(&safe[a..b]).into_iter().enumerate() {
                let (field, value) = match KEYED_VALUE.captures(item) {
                    Some(k) if fields.contains(&k[1].to_string()) => {
                        (k[1].to_string(), k.get(2).unwrap().as_str().to_string())
                    }
                    _ => match fields.get(pos) {
                        Some(fname) => (fname.clone(), item.to_string()),
                        None => continue,
                    },
                };
                if !funcs.contains(&field) {
                    continue;
                }
                if let Some(v) = resolve_value(&value, f) {
                    values
                        .entry((name.to_string(), field))
                        .or_default()
                        .push((v, line));
                }
            }
        }
        for c in ASSIGN.captures_iter(&safe) {
            let field = &c[2];
            let owners = holders.get(field).cloned().unwrap_or_default();
            // Without the receiver's type, a field name only pairs when one
            // struct holds it.
            let [owner] = owners.as_slice() else { continue };
            if GENERIC_FIELDS.contains(&field) {
                continue;
            }
            if let Some(v) = resolve_value(&c[3], f) {
                let line = lines.line(c.get(0).unwrap().start());
                values
                    .entry((owner.clone(), field.to_string()))
                    .or_default()
                    .push((v, line));
            }
        }
    }
    // Resolve copies one level: S.F given T.G gets what T.G is given.
    let direct = |s: &str, f: &str| -> Vec<usize> {
        values
            .get(&(s.to_string(), f.to_string()))
            .into_iter()
            .flatten()
            .filter_map(|(v, _)| match v {
                Value::Fn(t) => Some(*t),
                Value::Copy(_) => None,
            })
            .collect()
    };
    let mut channel: HashMap<(String, String), Vec<usize>> = HashMap::new();
    for ((s, f), vs) in &values {
        let mut targets = Vec::new();
        for (v, _) in vs {
            match v {
                Value::Fn(t) => targets.push(*t),
                Value::Copy(g) => {
                    for owner in holders.get(g).into_iter().flatten() {
                        targets.extend(direct(owner, g));
                    }
                }
            }
        }
        targets.sort_unstable();
        targets.dedup();
        channel.insert((s.clone(), f.clone()), targets);
    }
    // Calls through a function field.
    let mut sink = Sink::new(out);
    for f in &go_files {
        let Some(safe) = ctx.stripped(f, Language::Go) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        for c in SELECTOR_CALL.captures_iter(&safe) {
            let field = &c[2];
            let Some(owners) = holders.get(field) else {
                continue;
            };
            if GENERIC_FIELDS.contains(&field) && owners.len() > 1 {
                continue;
            }
            let line = lines.line(c.get(0).unwrap().start());
            let Some(caller) = ctx.enclosing_fn(f, line) else {
                continue;
            };
            // Owners in this package come first; a name held by several
            // structs elsewhere is ambiguous.
            let dir = dir_of(f);
            let local: Vec<&String> = owners
                .iter()
                .filter(|o| {
                    structs[*o]
                        .iter()
                        .any(|(i, _, _)| dir_of(&ctx.node(*i).file_path) == dir)
                })
                .collect();
            let chosen: Vec<&String> = if local.is_empty() {
                owners.iter().collect()
            } else {
                local
            };
            if chosen.len() != 1 {
                continue;
            }
            let key = (chosen[0].clone(), field.to_string());
            let mut added = 0;
            for &t in channel.get(&key).into_iter().flatten() {
                if added < MAX_VALUES && sink.push(caller, t, line, &format!("{}.{}", key.0, key.1))
                {
                    added += 1;
                }
            }
        }
    }
}
