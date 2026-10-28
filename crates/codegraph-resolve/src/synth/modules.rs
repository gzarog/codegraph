//! Module-system dispatch: an Erlang behaviour's callbacks reached through
//! a variable module (`Handler:init(Req, Opts)`), and a Nix option written
//! in one module and declared in another.

use super::{Ctx, LineIndex, Proposal, Sink};
use codegraph_core::{EdgeKind, Language, NodeKind};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

/// `Var:fn(` — a variable module; `?MODULE:f(` and `a:b(` are not.
static ERLANG_DISPATCH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(^|[^?\w@'])([A-Z][A-Za-z0-9_@]*):([a-z][A-Za-z0-9_@]*)\(").unwrap()
});
static ERLANG_CALLBACK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^\s*-callback\s+('[^'\n]+'|[a-z][A-Za-z0-9_@]*)\s*\(").unwrap()
});
const BEHAVIOUR_FANOUT: usize = 24;

/// Arguments of the call whose `(` is at `open`: top-level commas + 1, `()`
/// → 0, None when unbalanced. Strings, atoms, chars and `<<…>>` binaries
/// are skipped.
fn erlang_arity(src: &str, open: usize) -> Option<usize> {
    let b = src.as_bytes();
    let (mut depth, mut bin, mut commas, mut saw) = (1i32, 0i32, 0usize, false);
    let limit = b.len().min(open + 4000);
    let mut i = open + 1;
    while i < limit {
        let c = b[i];
        match c {
            b'"' | b'\'' => {
                i += 1;
                while i < limit && b[i] != c {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                saw = true;
            }
            b'$' => {
                i += 1;
                if b.get(i) == Some(&b'\\') {
                    i += 1;
                }
                saw = true;
            }
            b'<' if b.get(i + 1) == Some(&b'<') => {
                bin += 1;
                i += 1;
                saw = true;
            }
            b'>' if b.get(i + 1) == Some(&b'>') && bin > 0 => {
                bin -= 1;
                i += 1;
            }
            b'(' | b'[' | b'{' => {
                depth += 1;
                saw = true;
            }
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(if saw { commas + 1 } else { 0 });
                }
            }
            b',' if depth == 1 && bin == 0 => commas += 1,
            c if !c.is_ascii_whitespace() => saw = true,
            _ => {}
        }
        i += 1;
    }
    None
}

/// `Handler:init(Req, Opts)` → every in-repo implementer's `init/2` of the
/// one behaviour that declares `-callback init/2`.
pub fn erlang_behaviours(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Erlang]) {
        return;
    }
    let module_of: HashMap<&str, usize> = ctx
        .nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.language == Language::Erlang && n.kind == NodeKind::Namespace)
        .map(|(i, n)| (n.file_path.as_str(), i))
        .collect();
    let mut declaring: HashMap<String, Vec<usize>> = HashMap::new();
    let mut callback_names = HashSet::new();
    for file in ctx.files_of(&[Language::Erlang]) {
        let Some(&behaviour) = module_of.get(file) else {
            continue;
        };
        let Some(src) = ctx.source(file) else {
            continue;
        };
        if !src.contains("-callback") {
            continue;
        }
        let Some(safe) = ctx.stripped(file, Language::Erlang) else {
            continue;
        };
        for c in ERLANG_CALLBACK.captures_iter(&safe) {
            let name = c[1].trim_matches('\'').to_string();
            let Some(arity) = erlang_arity(&safe, c.get(0).unwrap().end() - 1) else {
                continue;
            };
            let v = declaring.entry(format!("{name}/{arity}")).or_default();
            if !v.contains(&behaviour) {
                v.push(behaviour);
            }
            callback_names.insert(name);
        }
    }
    if declaring.is_empty() {
        return;
    }
    // Implementers of each behaviour: modules whose `-behaviour` resolved to it.
    let mut implementers: HashMap<usize, Vec<usize>> = HashMap::new();
    for &m in module_of.values() {
        for (sup, _) in ctx.supers_of(m) {
            implementers.entry(sup).or_default().push(m);
        }
    }
    let target_in = |module: usize, key: &str| -> Option<usize> {
        let file = &ctx.node(module).file_path;
        ctx.in_file(file).iter().copied().find(|&i| {
            let n = ctx.node(i);
            n.kind == NodeKind::Function
                && n.qualified_name.ends_with(&format!("::{key}"))
                && n.is_exported
        })
    };
    let mut sink = Sink::new(out);
    for file in ctx.files_of(&[Language::Erlang]) {
        let Some(safe) = ctx.stripped(file, Language::Erlang) else {
            continue;
        };
        let lines = LineIndex::new(&safe);
        for c in ERLANG_DISPATCH.captures_iter(&safe) {
            let fun = &c[3];
            if !callback_names.contains(fun) {
                continue;
            }
            let Some(arity) = erlang_arity(&safe, c.get(0).unwrap().end() - 1) else {
                continue;
            };
            let key = format!("{fun}/{arity}");
            // A name and arity two behaviours declare is ambiguous.
            let Some([behaviour]) = declaring.get(&key).map(Vec::as_slice) else {
                continue;
            };
            let targets: Vec<usize> = implementers
                .get(behaviour)
                .into_iter()
                .flatten()
                .filter_map(|&m| target_in(m, &key))
                .collect();
            if targets.is_empty() || targets.len() > BEHAVIOUR_FANOUT {
                continue;
            }
            let line = lines.line(c.get(2).unwrap().start());
            let Some(disp) = ctx.enclosing_fn(file, line) else {
                continue;
            };
            let via = format!("{}:{key}", ctx.node(*behaviour).name);
            for t in targets {
                sink.push(disp, t, line, &via);
            }
        }
    }
}

/// The leading static segments of a Nix attribute path: identifiers, and
/// quoted segments verbatim; an interpolation ends it.
fn nix_segments(name: &str) -> Vec<String> {
    let mut segs = Vec::new();
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'"' {
            let mut j = i + 1;
            while j < b.len() && b[j] != b'"' {
                if b[j] == b'\\' {
                    j += 1;
                }
                j += 1;
            }
            if j >= b.len() {
                return segs;
            }
            let tok = &name[i..=j];
            if tok.contains("${") {
                return segs;
            }
            segs.push(tok.to_string());
            i = j + 1;
            if i >= b.len() {
                break;
            }
            if b[i] != b'.' {
                return segs;
            }
            i += 1;
            continue;
        }
        let mut j = i;
        while j < b.len() && b[j] != b'.' {
            if b[j] == b'"' || (b[j] == b'$' && b.get(j + 1) == Some(&b'{')) {
                return segs;
            }
            j += 1;
        }
        let seg = &name[i..j];
        let ok = seg
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_'-".contains(c));
        if !ok {
            return segs;
        }
        segs.push(seg.to_string());
        i = j + 1;
    }
    segs
}

/// A Nix config write (`services.yabai.enable = true`) references the option
/// declaration (`options.services.yabai.enable = mkOption …`) whose path is
/// its longest static prefix, when one file declares it.
pub fn nix_options(ctx: &Ctx<'_>, out: &mut Vec<Proposal>) {
    if !ctx.has_language(&[Language::Nix]) {
        return;
    }
    struct Rec {
        id: usize,
        file: String,
        start: u32,
        end: u32,
        segs: Vec<String>,
    }
    let mut by_file: HashMap<&str, Vec<Rec>> = HashMap::new();
    for (i, n) in ctx.nodes.iter().enumerate() {
        if n.language != Language::Nix || !matches!(n.kind, NodeKind::Variable | NodeKind::Function)
        {
            continue;
        }
        let segs = nix_segments(&n.name);
        if segs.is_empty() {
            continue;
        }
        by_file.entry(n.file_path.as_str()).or_default().push(Rec {
            id: i,
            file: n.file_path.clone(),
            start: n.start_line,
            end: n.end_line,
            segs,
        });
    }
    const SUBMODULE: &str = "\0submodule";
    let mut decls: HashMap<String, Vec<(usize, String)>> = HashMap::new();
    let mut writes: Vec<(usize, Vec<String>, u32)> = Vec::new();
    let mut register = |path: &[String], r: &Rec| {
        if path.len() < 2 || path.iter().any(|s| s == SUBMODULE) {
            return;
        }
        decls
            .entry(path.join("."))
            .or_default()
            .push((r.id, r.file.clone()));
    };
    let mut files: Vec<&&str> = by_file.keys().collect();
    files.sort();
    for f in files {
        let mut recs: Vec<&Rec> = by_file[*f].iter().collect();
        recs.sort_by_key(|r| (r.start, std::cmp::Reverse(r.end)));
        let mut stack: Vec<(u32, u32, Vec<String>)> = Vec::new();
        for r in recs {
            while stack.last().is_some_and(|s| s.1 < r.start) {
                stack.pop();
            }
            let enclosing = stack
                .last()
                .filter(|s| r.start >= s.0 && r.end <= s.1 && !(r.start == s.0 && r.end == s.1))
                .map(|s| s.2.clone());
            if r.segs[0] == "options" {
                let prefix = if enclosing.is_some() {
                    vec![SUBMODULE.to_string()]
                } else {
                    r.segs[1..].to_vec()
                };
                register(&prefix, r);
                stack.push((r.start, r.end, prefix));
                continue;
            }
            if let Some(mut composed) = enclosing {
                composed.extend(r.segs.iter().cloned());
                register(&composed, r);
                stack.push((r.start, r.end, composed));
                continue;
            }
            if r.segs.len() >= 2 {
                writes.push((r.id, r.segs.clone(), r.start));
            }
        }
    }
    if decls.is_empty() {
        return;
    }
    let mut sink = Sink::new(out);
    for (id, segs, line) in writes {
        let segs: &[String] = if segs[0] == "config" {
            &segs[1..]
        } else {
            &segs
        };
        if segs.len() < 2 {
            continue;
        }
        for len in (2..=segs.len().min(6)).rev() {
            let Some(cands) = decls.get(&segs[..len].join(".")) else {
                continue;
            };
            let files: HashSet<&str> = cands.iter().map(|(_, f)| f.as_str()).collect();
            if files.len() == 1 {
                sink.push_kind(
                    id,
                    cands[0].0,
                    line,
                    &segs[..len].join("."),
                    EdgeKind::References,
                );
            }
            // The longest match decides, matched or ambiguous.
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{erlang_arity, nix_segments};

    #[test]
    fn erlang_arities() {
        let s = "f(fun((a, b) -> ok), #{k => v}, <<1,2>>)";
        assert_eq!(erlang_arity(s, 1), Some(3));
        assert_eq!(erlang_arity("g()", 1), Some(0));
    }

    #[test]
    fn nix_paths() {
        assert_eq!(
            nix_segments("services.foo.enable"),
            vec!["services", "foo", "enable"]
        );
        assert_eq!(nix_segments("a.\"b.c\".d"), vec!["a", "\"b.c\"", "d"]);
        assert_eq!(nix_segments("a.${x}.b"), vec!["a"]);
    }
}
