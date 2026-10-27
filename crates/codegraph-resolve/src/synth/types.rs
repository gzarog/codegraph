//! Dispatch through types: an interface or base method reaches the methods
//! that override it, Go types implement interfaces structurally, a Kotlin
//! `expect` reaches its `actual`s, and a generated gRPC stub its server.

use super::{Ctx, Proposal, Sink};
use codegraph_core::{EdgeKind, Language, NodeKind};
use std::collections::{HashMap, HashSet};

/// Most links one type, interface or channel contributes.
const MAX_PER_TYPE: usize = 40;

fn dir_of(path: &str) -> &str {
    path.rfind(['/', '\\']).map_or("", |i| &path[..i])
}

fn is_go_type(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Struct
            | NodeKind::Interface
            | NodeKind::TypeAlias
            | NodeKind::Class
            | NodeKind::Enum
    )
}

/// The receiver of a Go method from its `Recv::name` qualified name.
fn receiver(qualified: &str) -> Option<&str> {
    qualified
        .rsplit_once("::")
        .map(|(r, _)| r)
        .filter(|r| !r.is_empty())
}

/// A Go method declared in another file than its type: contained by the
/// same-package type named by its receiver.
pub fn go_method_contains(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Go]) {
        return;
    }
    let mut owned: HashSet<usize> = HashSet::new();
    for (i, n) in ctx.nodes.iter().enumerate() {
        if n.language == Language::Go && is_go_type(n.kind) {
            owned.extend(ctx.methods_of(i));
        }
    }
    let mut sink = Sink::new(out);
    for (i, m) in ctx.nodes.iter().enumerate() {
        if m.kind != NodeKind::Method || m.language != Language::Go || owned.contains(&i) {
            continue;
        }
        let Some(recv) = receiver(&m.qualified_name) else {
            continue;
        };
        let dir = dir_of(&m.file_path);
        let owner = ctx.named(recv).find(|&t| {
            let n = ctx.node(t);
            n.language == Language::Go && is_go_type(n.kind) && dir_of(&n.file_path) == dir
        });
        if let Some(t) = owner {
            ctx.add_method(t, i);
            sink.push_kind(t, i, m.start_line, recv, EdgeKind::Contains);
        }
    }
}

/// What a Go method matches an interface method by: its name, and for an
/// unexported name its package too.
fn go_key(ctx: &Ctx<'_>, m: usize) -> String {
    let n = ctx.node(m);
    if n.name.chars().next().is_some_and(char::is_uppercase) {
        n.name.clone()
    } else {
        format!("{}\0{}", dir_of(&n.file_path), n.name)
    }
}

/// Top-level items of the bracketed list opening at `text[open]`, and where it closes.
fn list_items(text: &str, open: usize) -> Option<(Vec<String>, usize)> {
    let mut items = Vec::new();
    let mut depth = 0;
    let mut item = String::new();
    let b: Vec<char> = text.chars().collect();
    let mut i = open;
    while i < b.len() {
        let c = b[i];
        match c {
            '"' | '`' | '\'' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != c {
                    j += if c != '`' && b[j] == '\\' { 2 } else { 1 };
                }
                if j >= b.len() {
                    return None;
                }
                item.extend(&b[i..=j]);
                i = j;
            }
            '(' | '[' | '{' => {
                depth += 1;
                if depth > 1 {
                    item.push(c);
                }
            }
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    if !item.trim().is_empty() {
                        items.push(item.trim().to_string());
                    }
                    return Some((items, i));
                }
                item.push(c);
            }
            ',' if depth == 1 => {
                if !item.trim().is_empty() {
                    items.push(item.trim().to_string());
                }
                item.clear();
            }
            _ => item.push(c),
        }
        i += 1;
    }
    None
}

/// A Go signature's parameter and result counts: `(ctx Context, a, b int)
/// (int, error)` is (3, 2). None when it doesn't read.
fn go_arity(sig: Option<&str>) -> Option<(usize, usize)> {
    let sig = sig?.trim();
    if !sig.starts_with('(') {
        return None;
    }
    let (params, end) = list_items(sig, 0)?;
    let rest: String = sig.chars().skip(end + 1).collect();
    let rest = rest.trim();
    let results = if rest.starts_with('(') {
        let (r, e) = list_items(rest, 0)?;
        if !rest
            .chars()
            .skip(e + 1)
            .collect::<String>()
            .trim()
            .is_empty()
        {
            return None;
        }
        r.len()
    } else {
        usize::from(!rest.is_empty())
    };
    Some((params.len(), results))
}

fn same_arity(ctx: &Ctx<'_>, a: usize, b: usize) -> bool {
    match (
        go_arity(ctx.node(a).signature.as_deref()),
        go_arity(ctx.node(b).signature.as_deref()),
    ) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

fn is_go(ctx: &Ctx<'_>, i: usize, kinds: &[NodeKind]) -> bool {
    let n = ctx.node(i);
    n.language == Language::Go && kinds.contains(&n.kind)
}

/// The types a Go type embeds: its declared supertype links into structs,
/// interfaces and defined types (an interface embeds only interfaces).
fn go_embeds(ctx: &Ctx<'_>, ty: usize) -> Vec<usize> {
    let from_iface = ctx.node(ty).kind == NodeKind::Interface;
    ctx.supers_of(ty)
        .into_iter()
        .filter(|&(t, synthesized)| {
            !synthesized
                && is_go(
                    ctx,
                    t,
                    if from_iface {
                        &[NodeKind::Interface]
                    } else {
                        &[NodeKind::Struct, NodeKind::Interface, NodeKind::TypeAlias]
                    },
                )
        })
        .map(|(t, _)| t)
        .collect()
}

/// An interface's methods, its embedded interfaces' included, by key.
fn iface_methods(ctx: &Ctx<'_>, iface: usize) -> HashMap<String, usize> {
    let mut out = HashMap::new();
    let mut reached = HashSet::from([iface]);
    let mut pending = vec![iface];
    while let Some(at) = pending.pop() {
        for m in ctx.methods_of(at) {
            out.entry(go_key(ctx, m)).or_insert(m);
        }
        for e in go_embeds(ctx, at) {
            if reached.insert(e) {
                pending.push(e);
            }
        }
    }
    out
}

/// The method Go's selector picks on `ty` for `key` through its embeddings:
/// breadth-first, the shallowest unique provider, none through an interface.
fn go_promoted(ctx: &Ctx<'_>, ty: usize, key: &str) -> Option<usize> {
    let mut level: HashMap<usize, usize> = HashMap::from([(ty, 1)]);
    let mut met = HashSet::from([ty]);
    while !level.is_empty() {
        let mut next: HashMap<usize, usize> = HashMap::new();
        for (&id, &paths) in &level {
            if ctx.node(id).kind == NodeKind::Interface {
                continue;
            }
            for e in go_embeds(ctx, id) {
                if !met.contains(&e) {
                    *next.entry(e).or_default() += paths;
                }
            }
        }
        let mut paths = 0;
        let mut provider = None;
        for (&id, &p) in &next {
            met.insert(id);
            let provides = if ctx.node(id).kind == NodeKind::Interface {
                iface_methods(ctx, id).contains_key(key)
            } else {
                ctx.methods_of(id).iter().any(|&m| go_key(ctx, m) == key)
            };
            if provides {
                paths += p;
                provider = Some(id);
            }
        }
        if let Some(p) = provider {
            if paths == 1 && ctx.node(p).kind != NodeKind::Interface {
                return ctx
                    .methods_of(p)
                    .into_iter()
                    .find(|&m| go_key(ctx, m) == key);
            }
            return None;
        }
        level = next;
    }
    None
}

/// Whether an interface embeds a type the index doesn't hold (`io.Reader`):
/// its method set is then unknown.
fn embeds_outside(ctx: &Ctx<'_>, iface: usize) -> bool {
    let n = ctx.node(iface);
    let Some(body) = ctx.lines(&n.file_path, n.start_line, n.end_line) else {
        return true;
    };
    let inner = body
        .find('{')
        .map_or("", |i| &body[i + 1..body.rfind('}').unwrap_or(body.len())]);
    let embedded = inner
        .split([';', '\n'])
        .map(str::trim)
        .filter(|l| {
            !l.is_empty()
                && !l.starts_with("//")
                && !l.contains('(')
                && l.chars()
                    .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '*')
        })
        .count();
    embedded > go_embeds(ctx, iface).len()
}

/// Go types implement interfaces by having their methods: a struct or
/// defined type whose own and promoted methods cover an interface's (names
/// and parameter/result counts) implements it.
pub fn go_implements(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Go]) {
        return;
    }
    let types: Vec<usize> = (0..ctx.nodes.len())
        .filter(|&i| is_go(ctx, i, &[NodeKind::Struct, NodeKind::TypeAlias]))
        .collect();
    // Each type's method keys: its own, then what embedding promotes.
    let mut own: HashMap<usize, HashMap<String, usize>> = HashMap::new();
    let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
    for &t in &types {
        let mut keys: HashMap<String, usize> = HashMap::new();
        for m in ctx.methods_of(t) {
            keys.entry(go_key(ctx, m)).or_insert(m);
        }
        // Promoted keys: everything the embedded types have, at any depth.
        let mut seen = HashSet::from([t]);
        let mut pending = go_embeds(ctx, t);
        while let Some(e) = pending.pop() {
            if !seen.insert(e) {
                continue;
            }
            let methods = if ctx.node(e).kind == NodeKind::Interface {
                iface_methods(ctx, e).into_values().collect()
            } else {
                ctx.methods_of(e)
            };
            for m in methods {
                keys.entry(go_key(ctx, m)).or_insert(m);
            }
            pending.extend(go_embeds(ctx, e));
        }
        if keys.is_empty() {
            continue;
        }
        for k in keys.keys() {
            by_key.entry(k.clone()).or_default().push(t);
        }
        own.insert(t, keys);
    }
    let mut sink = Sink::new(out);
    for iface in 0..ctx.nodes.len() {
        if !is_go(ctx, iface, &[NodeKind::Interface]) {
            continue;
        }
        let wanted = iface_methods(ctx, iface);
        if wanted.is_empty() || embeds_outside(ctx, iface) {
            continue;
        }
        // Candidates: the types with the interface's rarest method.
        let Some(rarest) = wanted
            .keys()
            .min_by_key(|k| by_key.get(*k).map_or(0, Vec::len))
        else {
            continue;
        };
        let mut added = 0;
        for &t in by_key.get(rarest).into_iter().flatten() {
            if added >= MAX_PER_TYPE {
                break;
            }
            let has = &own[&t];
            let covers = wanted
                .iter()
                .all(|(k, &want)| has.get(k).is_some_and(|&m| same_arity(ctx, m, want)));
            if !covers || ctx.supers_of(t).iter().any(|(s, _)| *s == iface) {
                continue;
            }
            ctx.add_super(t, iface);
            if sink.push_kind(
                t,
                iface,
                ctx.node(t).start_line,
                &ctx.node(iface).name,
                EdgeKind::Implements,
            ) {
                added += 1;
            }
        }
    }
}

/// Languages whose supertype links make a base method dispatch to overrides.
fn overrides_in(lang: Language) -> bool {
    matches!(
        lang,
        Language::Java
            | Language::Kotlin
            | Language::CSharp
            | Language::TypeScript
            | Language::Tsx
            | Language::JavaScript
            | Language::Jsx
            | Language::Swift
            | Language::Scala
            | Language::Go
            | Language::Rust
            | Language::ArkTs
            | Language::Cpp
            | Language::Dart
    )
}

fn js_like(lang: Language) -> bool {
    matches!(
        lang,
        Language::TypeScript | Language::Tsx | Language::JavaScript | Language::Jsx
    )
}

/// A base's methods; a Swift protocol's include its extensions'.
fn base_methods(ctx: &Ctx<'_>, base: usize) -> Vec<usize> {
    let b = ctx.node(base);
    let mut out = ctx.methods_of(base);
    if b.language == Language::Swift && matches!(b.kind, NodeKind::Interface | NodeKind::Protocol) {
        for ext in ctx.named(&b.name) {
            let n = ctx.node(ext);
            if n.language == Language::Swift && n.kind == NodeKind::Class {
                out.extend(ctx.methods_of(ext));
            }
        }
    }
    out
}

/// A base or interface method reaches each subtype method that overrides it.
pub fn overrides(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    let mut sink = Sink::new(out);
    let mut promoted: Vec<(usize, Vec<usize>, usize)> = Vec::new();
    for cls in 0..ctx.nodes.len() {
        let c = ctx.node(cls);
        if !overrides_in(c.language)
            || !matches!(
                c.kind,
                NodeKind::Class | NodeKind::Struct | NodeKind::Union | NodeKind::Enum
            ) && !(c.language == Language::Go && c.kind == NodeKind::TypeAlias)
        {
            continue;
        }
        let sups = ctx.supers_of(cls);
        if sups.is_empty() {
            continue;
        }
        let impls = ctx.methods_of(cls);
        let go_struct = c.language == Language::Go && c.kind == NodeKind::Struct;
        if impls.is_empty() && !(go_struct && sups.iter().any(|s| s.1)) {
            continue;
        }
        for (base, synthesized) in sups {
            let b = ctx.node(base);
            if !overrides_in(b.language) || base == cls {
                continue;
            }
            // A Go embedding of a struct runs the embedded type's own method.
            if c.language == Language::Go && b.kind != NodeKind::Interface {
                continue;
            }
            let go_iface = b.language == Language::Go && b.kind == NodeKind::Interface;
            let key = |m: usize| {
                if go_iface {
                    go_key(ctx, m)
                } else {
                    ctx.node(m).name.clone()
                }
            };
            let mut by_key: HashMap<String, Vec<usize>> = HashMap::new();
            for &m in &impls {
                by_key.entry(key(m)).or_default().push(m);
            }
            let mut added = 0;
            let mut unmatched = Vec::new();
            for bm in base_methods(ctx, base) {
                let Some(ms) = by_key.get(&key(bm)) else {
                    if go_struct && synthesized {
                        unmatched.push(bm);
                    }
                    continue;
                };
                for &m in ms {
                    if added >= MAX_PER_TYPE {
                        break;
                    }
                    // A JS static overrides only a static.
                    if js_like(ctx.node(m).language)
                        && ctx.node(m).is_static != ctx.node(bm).is_static
                    {
                        continue;
                    }
                    if sink.push(bm, m, ctx.node(bm).start_line, &ctx.node(m).name) {
                        added += 1;
                    }
                }
            }
            if !unmatched.is_empty() && added < MAX_PER_TYPE {
                promoted.push((cls, unmatched, MAX_PER_TYPE - added));
            }
        }
    }
    // A Go struct satisfying an interface with promoted methods: the call
    // runs the embedded type's method.
    for (cls, methods, mut budget) in promoted {
        for bm in methods {
            if budget == 0 {
                break;
            }
            if let Some(m) = go_promoted(ctx, cls, &go_key(ctx, bm)) {
                if sink.push(bm, m, ctx.node(bm).start_line, &ctx.node(m).name) {
                    budget -= 1;
                }
            }
        }
    }
}

fn kmp_compatible(a: NodeKind, b: NodeKind) -> bool {
    let ty = |k| {
        matches!(
            k,
            NodeKind::Class
                | NodeKind::Interface
                | NodeKind::Struct
                | NodeKind::Enum
                | NodeKind::TypeAlias
        )
    };
    a == b || ty(a) && ty(b)
}

/// A Kotlin `expect` declaration reaches each platform's `actual`.
pub fn kotlin_expect_actual(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Kotlin]) {
        return;
    }
    let mut sink = Sink::new(out);
    for (i, act) in ctx.nodes.iter().enumerate() {
        if act.language != Language::Kotlin || !act.decorators.iter().any(|d| d == "actual") {
            continue;
        }
        let mut added = 0;
        for c in ctx.qualified(&act.qualified_name) {
            let n = ctx.node(c);
            if added >= MAX_PER_TYPE
                || n.language != Language::Kotlin
                || c == i
                || n.file_path == act.file_path
                || !kmp_compatible(n.kind, act.kind)
                || n.decorators.iter().any(|d| d == "actual")
            {
                continue;
            }
            if sink.push(c, i, n.start_line, &act.name) {
                added += 1;
            }
        }
    }
}

/// A generated `UnimplementedXServer` stub's RPC methods reach the
/// hand-written structs that implement all of them.
pub fn go_grpc_stubs(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Go]) {
        return;
    }
    let structs: Vec<usize> = (0..ctx.nodes.len())
        .filter(|&i| is_go(ctx, i, &[NodeKind::Struct]))
        .collect();
    let names = |s: usize| -> HashSet<String> {
        ctx.methods_of(s)
            .into_iter()
            .map(|m| ctx.node(m).name.clone())
            .collect()
    };
    let mut sink = Sink::new(out);
    for &stub in &structs {
        let s = ctx.node(stub);
        if !(s.name.starts_with("Unimplemented") && s.name.ends_with("Server"))
            || !ctx.is_generated(&s.file_path)
        {
            continue;
        }
        let rpcs: Vec<usize> = ctx
            .methods_of(stub)
            .into_iter()
            .filter(|&m| {
                let n = &ctx.node(m).name;
                !n.starts_with("mustEmbed") && n != "testEmbeddedByValue"
            })
            .collect();
        if rpcs.is_empty() {
            continue;
        }
        for &cand in &structs {
            if cand == stub || ctx.is_generated(&ctx.node(cand).file_path) {
                continue;
            }
            let have = names(cand);
            if !rpcs.iter().all(|&m| have.contains(&ctx.node(m).name)) {
                continue;
            }
            let mut added = 0;
            for &sm in &rpcs {
                for cm in ctx.methods_of(cand) {
                    if added < MAX_PER_TYPE
                        && ctx.node(cm).name == ctx.node(sm).name
                        && sink.push(sm, cm, ctx.node(sm).start_line, &ctx.node(cm).name)
                    {
                        added += 1;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::go_arity;

    #[test]
    fn go_signature_arity() {
        assert_eq!(
            go_arity(Some("(ctx context.Context, a, b int) (int, error)")),
            Some((3, 2))
        );
        assert_eq!(go_arity(Some("() bool")), Some((0, 1)));
        assert_eq!(go_arity(Some("(f func(int) error)")), Some((1, 0)));
        assert_eq!(go_arity(Some("struct{}")), None);
    }
}
