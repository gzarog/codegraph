//! A local C++ object construction → the constructor it runs (#1839,
//! upstream `cpp-constructor.ts`). `T obj(args);` is recorded as a `calls` ref
//! `ns::T::T/<arity>`; the type is looked up in the call site's lexical
//! namespaces, through an alias it is named by, and the single constructor
//! overload whose parameter count admits the argument count wins.

use super::aliases::{self, Tri};
use super::ctx::{Ctx, Ref};
use super::js;
use crate::matcher::Match;
use crate::symbols::SymId;
use codegraph_core::{Language, NodeKind, ReferenceKind};

pub fn is_constructor_ref(ctx: &Ctx, r: &Ref) -> bool {
    r.lang == Language::Cpp
        && r.kind == ReferenceKind::Calls
        && js::test(&ctx.re(r"::[^:]+/\d+$"), &r.name)
}

struct Shape {
    min: usize,
    max: usize,
    key: String,
}

/// Admissible argument counts of a `(params)` signature; None when it can't be read.
fn constructor_shape(ctx: &Ctx, signature: Option<&str>) -> Option<Shape> {
    let sig = signature?;
    let sig = sig.strip_suffix(';').unwrap_or(sig);
    if !sig.starts_with('(') || !sig.ends_with(')') || sig.len() < 2 {
        return None;
    }
    let text = sig[1..sig.len() - 1].trim();
    if text.is_empty() || text == "void" {
        return Some(Shape {
            min: 0,
            max: 0,
            key: String::new(),
        });
    }
    let b = text.as_bytes();
    let mut parts: Vec<&str> = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let mut quote = 0u8;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if quote != 0 {
            if c == b'\\' {
                i += 1;
            } else if c == quote {
                quote = 0;
            }
            i += 1;
            continue;
        }
        if c == b'"' || c == b'\'' {
            quote = c;
            i += 1;
            continue;
        }
        if b"(<[{".contains(&c) {
            depth += 1;
        }
        if b")>]}".contains(&c) {
            depth -= 1;
        }
        if c == b',' && depth == 0 {
            parts.push(&text[start..i]);
            start = i + 1;
        }
        i += 1;
    }
    if depth != 0 || quote != 0 {
        return None;
    }
    parts.push(&text[start..]);
    let compare = ctx.re(r"[<>]=|==|!=");
    if parts.iter().any(|p| js::test(&compare, p)) {
        return None;
    }
    let variadic = parts.iter().any(|p| p.contains("..."));
    let name_re = ctx.re(r"(.*[\s*&>])([A-Za-z_]\w*)$");
    let builtin =
        ctx.re(r"^(?:void|bool|char|short|int|long|float|double|signed|unsigned|const|volatile)$");
    let qualifier = ctx.re(r"^(?:const|volatile|struct|class|enum)\s*$");
    let types: Vec<String> = parts
        .iter()
        .map(|p| {
            let ty = p.split('=').next().unwrap_or("").trim();
            let ty = js::replace_first(&name_re, ty, |c| {
                let prefix = js::group(c, 1);
                let name = js::group(c, 2);
                if js::test(&builtin, name) || js::test(&qualifier, prefix) {
                    c.get(0).unwrap().as_str().to_string()
                } else {
                    prefix.to_string()
                }
            });
            js::squash(&ty)
        })
        .collect();
    Some(Shape {
        key: types.join(","),
        min: parts
            .iter()
            .filter(|p| !p.contains('=') && !p.contains("..."))
            .count(),
        max: if variadic { usize::MAX } else { parts.len() },
    })
}

pub fn match_constructor(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let m = js::caps(&ctx.re(r"^(.*)::([^:]+)/(\d+)$"), &r.name)?;
    let raw_type = js::group(&m, 1).to_string();
    let ctor_name = js::group(&m, 2).to_string();
    let argc: usize = js::group(&m, 3).parse().ok()?;
    let mut ty = raw_type.strip_prefix("::").unwrap_or(&raw_type).to_string();
    if js::last_segment(&ty) != ctor_name {
        return None;
    }
    let global = raw_type.starts_with("::");
    let caller = ctx.sym(r.from);
    let mut scopes: Vec<String> = if global {
        Vec::new()
    } else {
        caller
            .qualified_name
            .split("::")
            .map(str::to_string)
            .collect()
    };
    if !global {
        match aliases::resolve_aliased_type(ctx, &ty, r) {
            Tri::Null => return None,
            Tri::Found(aliased) => {
                let mut names = aliased.target.clone();
                names.extend(aliased.rest.iter().cloned());
                ty = names.join("::");
                scopes = if aliased.scope.is_empty() {
                    Vec::new()
                } else {
                    aliased.scope.split("::").map(str::to_string).collect()
                };
            }
            Tri::Undefined => {}
        }
    }
    let name = js::last_segment(&ty).to_string();
    let mut candidates: Vec<String> = Vec::new();
    for i in (1..=scopes.len()).rev() {
        candidates.push(format!("{}::{ty}", scopes[..i].join("::")));
    }
    candidates.push(ty.clone());

    for qualified in candidates {
        let owners = ctx.by_qname(&qualified).iter().any(|&n| {
            let s = ctx.sym(n);
            s.language == Language::Cpp
                && matches!(s.kind, NodeKind::Class | NodeKind::Struct | NodeKind::Union)
        });
        if !owners {
            continue;
        }
        let want = format!("{qualified}::{name}");
        let constructors: Vec<SymId> = ctx
            .named(&name)
            .iter()
            .copied()
            .filter(|&n| {
                let s = ctx.sym(n);
                s.language == Language::Cpp
                    && s.kind == NodeKind::Method
                    && s.qualified_name == want
            })
            .collect();
        let init_list = ctx.re(r"\binitializer_list\b");
        if constructors
            .iter()
            .any(|&n| js::test(&init_list, ctx.sym(n).signature.as_deref().unwrap_or("")))
        {
            return None;
        }
        // A prototype and its out-of-line definition describe one overload.
        let mut overloads: Vec<(String, Vec<SymId>, usize, usize)> = Vec::new();
        for &node in &constructors {
            let shape = constructor_shape(ctx, ctx.sym(node).signature.as_deref())?;
            if let Some(prior) = overloads.iter_mut().find(|o| o.0 == shape.key) {
                prior.1.push(node);
                prior.2 = prior.2.min(shape.min);
            } else {
                overloads.push((shape.key, vec![node], shape.min, shape.max));
            }
        }
        let admitting: Vec<&(String, Vec<SymId>, usize, usize)> = overloads
            .iter()
            .filter(|o| o.2 <= argc && argc <= o.3)
            .collect();
        if admitting.len() != 1 {
            return None;
        }
        let nodes = &admitting[0].1;
        let definitions: Vec<SymId> = nodes
            .iter()
            .copied()
            .filter(|&n| !ctx.sym(n).signature.as_deref().unwrap_or("").ends_with(';'))
            .collect();
        let targets = if definitions.is_empty() {
            nodes.clone()
        } else {
            definitions
        };
        if targets.len() != 1 {
            return None;
        }
        return Some(Match::new(targets[0], "qualified-name", 0.9));
    }
    None
}
