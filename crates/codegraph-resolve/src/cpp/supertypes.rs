//! A C or C++ base class is the class C++ name lookup finds for it (upstream
//! `cpp-supertypes.ts`): from the scope the class is declared in outwards,
//! then through the file's namespace aliases, using-declarations and
//! using-directives before it, then the global scope and the namespaces a
//! macro opens. A class a source file defines belongs to its translation unit.

use super::aliases::{self, class_kind, NamedClass, Tri};
use super::ctx::{memo, Ctx, Ref};
use super::family::is_c_family;
use super::js;
use super::namespaces;
use crate::matcher::Match;
use crate::symbols::SymId;
use codegraph_core::{NodeKind, ReferenceKind};
use std::collections::HashSet;
use std::rc::Rc;

/// A C or C++ source file, whose definitions belong to its own translation unit.
pub fn is_source_file(ctx: &Ctx, path: &str) -> bool {
    js::test(&ctx.re(r"(?i)\.(?:c|cc|cpp|cxx|c\+\+|cu|metal)$"), path)
}

pub fn is_supertype_ref(r: &Ref) -> bool {
    is_c_family(r.lang) && matches!(r.kind, ReferenceKind::Extends | ReferenceKind::Implements)
}

/// Where a C or C++ type name is written, which is where its lookup starts.
#[derive(Clone)]
pub struct NameSite {
    pub scope: String,
    pub file: String,
    pub line: u32,
    pub parameters: Rc<HashSet<String>>,
    pub self_id: Option<SymId>,
    pub class_scope: Option<String>,
}

/// The class a C or C++ type name names.
#[derive(Clone, Copy, Debug)]
pub struct WrittenClass {
    pub node: SymId,
    pub exact: bool,
}

/// The function a C or C++ class is local to, if any.
fn function_around(ctx: &Ctx, cls: SymId) -> Option<SymId> {
    ctx.memo(memo::LOCAL_TO, cls.to_string(), || {
        let c = ctx.sym(cls);
        let scope = aliases::parent_scope(&c.qualified_name);
        if scope.is_empty() {
            return None;
        }
        ctx.by_qname(scope).iter().copied().find(|&f| {
            let s = ctx.sym(f);
            matches!(s.kind, NodeKind::Function | NodeKind::Method)
                && s.file == c.file
                && s.start_line <= c.start_line
                && s.end_line >= c.start_line
        })
    })
}

fn visible_from(ctx: &Ctx, file: &str, unit: Option<&str>, n: SymId) -> bool {
    let p = &ctx.sym(n).file;
    p == file || unit.is_some_and(|u| p == u) || !is_source_file(ctx, p)
}

fn seen_from(ctx: &Ctx, site: &NameSite, unit: Option<&str>, n: SymId) -> bool {
    if !visible_from(ctx, &site.file, unit, n) {
        return false;
    }
    match function_around(ctx, n) {
        None => true,
        Some(f) => {
            let fs = ctx.sym(f);
            fs.file == site.file && site.line >= fs.start_line && site.line <= fs.end_line
        }
    }
}

pub fn match_supertype(ctx: &Ctx, r: &Ref) -> Option<Match> {
    let derived = r.from;
    let d = ctx.sym(derived);
    if !is_c_family(d.language) || !class_kind(d.kind) {
        return None;
    }
    let site = NameSite {
        scope: aliases::parent_scope(&d.qualified_name).to_string(),
        file: d.file.clone(),
        line: r.line,
        parameters: aliases::template_parameters(ctx, derived),
        self_id: Some(derived),
        class_scope: None,
    };
    let found = class_written(ctx, &written_base(ctx, r), &site, r);
    match found {
        Tri::Found(f) if f.exact => Some(Match::new(f.node, "qualified-name", 0.9)),
        Tri::Found(f) => Some(Match::new(f.node, "exact-match", 0.7)),
        _ => None,
    }
}

/// The class a C or C++ type name written at `site` names.
pub fn class_written(ctx: &Ctx, written: &str, site: &NameSite, r: &Ref) -> Tri<WrittenClass> {
    let Some(segments) = aliases::type_segments(ctx, written) else {
        return Tri::Null;
    };
    let global = written.starts_with("::");
    if !global && site.parameters.contains(&segments[0]) {
        return Tri::Null;
    }
    let unit = r.file.clone();
    let visible = |n: SymId| seen_from(ctx, site, Some(&unit), n);
    let mut scopes = aliases::scopes_within(&site.scope);
    if let Some(cs) = &site.class_scope {
        scopes.insert(1.min(scopes.len()), cs.clone());
    }
    let resolved = |node: SymId| WrittenClass { node, exact: true };

    let lexical_scopes: Vec<String> = if global {
        vec![String::new()]
    } else {
        scopes[..scopes.len() - 1].to_vec()
    };
    let lexical = class_node(
        ctx,
        aliases::class_named_lookup(ctx, written, &lexical_scopes, r, Some(&visible)),
        site,
        &visible,
    );
    match lexical {
        Tri::Found(n) => return Tri::Found(resolved(n)),
        Tri::Null => return Tri::Null,
        Tri::Undefined => {}
    }

    let path = segments.join("::");
    let first = js::caps(&ctx.re(r"^\s*(?:::\s*)?[A-Za-z_]\w*"), written)
        .map(|c| c.get(0).unwrap().as_str().len())
        .unwrap_or(0);
    let after = &written[first..];
    let mut usings: Vec<Using> = usings_in(ctx, &site.file)
        .iter()
        .filter(|u| u.line < site.line)
        .cloned()
        .collect();
    usings.reverse();
    let look_up = |spelled: &str| -> Option<SymId> {
        let absolute = spelled.starts_with("::");
        let sc: Vec<String> = if absolute {
            vec![String::new()]
        } else {
            scopes.clone()
        };
        class_node(
            ctx,
            aliases::class_named_lookup(ctx, spelled, &sc, r, Some(&visible)),
            site,
            &visible,
        )
        .found()
    };
    if segments.len() > 1 {
        let local = usings
            .iter()
            .find(|u| u.kind == UsingKind::Alias && u.name == segments[0]);
        let target = local.map(|u| u.path.clone()).or_else(|| {
            namespaces::namespace_aliases(ctx)
                .get(&segments[0])
                .cloned()
        });
        if let Some(t) = target {
            if let Some(hit) = look_up(&format!("{t}{after}")) {
                return Tri::Found(resolved(hit));
            }
        }
    }
    if !global {
        for u in &usings {
            if u.kind != UsingKind::Declaration || u.name != segments[0] {
                continue;
            }
            if let Some(hit) = look_up(&format!("{}{after}", u.path)) {
                return Tri::Found(resolved(hit));
            }
        }
        for u in &usings {
            if u.kind != UsingKind::Directive {
                continue;
            }
            if let Some(hit) = look_up(&format!("{}::{written}", u.path)) {
                return Tri::Found(resolved(hit));
            }
        }
        let outermost = class_node(
            ctx,
            aliases::class_named_lookup(ctx, written, &[String::new()], r, Some(&visible)),
            site,
            &visible,
        );
        match outermost {
            Tri::Found(n) => return Tri::Found(resolved(n)),
            Tri::Null => return Tri::Null,
            Tri::Undefined => {}
        }
    }
    if segments.len() > 1 {
        let macro_named: Vec<SymId> = named_classes(ctx, &segments, site, &visible)
            .into_iter()
            .filter(|&n| {
                let s = ctx.sym(n);
                namespaces::macro_qualified_name(ctx, &s.file, s.start_line, &s.qualified_name)
                    == path
            })
            .collect();
        if let Some(hit) = pick(ctx, &macro_named, &site.file) {
            return Tri::Found(resolved(hit));
        }
    }
    let tail: Vec<SymId> = named_classes(ctx, &segments, site, &visible)
        .into_iter()
        .filter(|&n| {
            let q = &ctx.sym(n).qualified_name;
            *q == path || q.ends_with(&format!("::{path}"))
        })
        .collect();
    let own: Vec<SymId> = tail
        .iter()
        .copied()
        .filter(|&n| ctx.sym(n).file == site.file)
        .collect();
    let sole = if own.is_empty() { tail } else { own };
    if sole.len() == 1 {
        Tri::Found(WrittenClass {
            node: sole[0],
            exact: false,
        })
    } else {
        Tri::Undefined
    }
}

/// The base as the base clause writes it, template arguments included.
fn written_base(ctx: &Ctx, r: &Ref) -> String {
    let src = ctx.source(&r.file);
    let text = src
        .map(|s| {
            let from = (r.line as usize).saturating_sub(1).min(s.lines.len());
            let to = (r.line as usize + 11).min(s.lines.len());
            js::slice_from(&s.lines[from..to].join("\n"), r.column as usize).to_string()
        })
        .unwrap_or_default();
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut end = b.len();
    for i in 0..b.len() {
        let c = b[i];
        let prev = if i > 0 { b[i - 1] } else { 0 };
        let next = b.get(i + 1).copied().unwrap_or(0);
        let opens = c == b'(' || (c == b'<' && js::is_word(prev) && next != b'=' && next != b'<');
        let closes = c == b')' || (c == b'>' && prev != b'-' && next != b'=');
        if opens {
            depth += 1;
        } else if closes {
            if depth == 0 {
                end = i;
                break;
            }
            depth -= 1;
        } else if depth == 0 && (c == b',' || c == b'{' || c == b';') {
            end = i;
            break;
        }
    }
    let written = text[..end].trim();
    if js::squash(&aliases::strip_template_arguments(written)) == js::squash(&r.name) {
        written.to_string()
    } else {
        r.name.trim().to_string()
    }
}

fn class_node(
    ctx: &Ctx,
    named: Tri<NamedClass>,
    site: &NameSite,
    visible: &dyn Fn(SymId) -> bool,
) -> Tri<SymId> {
    match named {
        Tri::Null => Tri::Null,
        Tri::Undefined => Tri::Undefined,
        Tri::Found(NamedClass::Alias(a)) => Tri::Found(a),
        Tri::Found(NamedClass::Cls(cls)) => {
            let classes: Vec<SymId> = ctx
                .by_qname(&cls)
                .iter()
                .copied()
                .filter(|&n| {
                    let s = ctx.sym(n);
                    is_c_family(s.language) && class_kind(s.kind) && Some(n) != site.self_id
                })
                .collect();
            if classes.is_empty() {
                return Tri::Undefined;
            }
            let shown: Vec<SymId> = classes.iter().copied().filter(|&n| visible(n)).collect();
            match pick(
                ctx,
                if shown.is_empty() { &classes } else { &shown },
                &site.file,
            ) {
                Some(n) => Tri::Found(n),
                None => Tri::Undefined,
            }
        }
    }
}

/// One of several declarations of a class: in `file`, then its header, then the nearest by path.
pub fn pick(ctx: &Ctx, classes: &[SymId], file: &str) -> Option<SymId> {
    if classes.len() <= 1 {
        return classes.first().copied();
    }
    if let Some(&own) = classes.iter().find(|&&n| ctx.sym(n).file == file) {
        return Some(own);
    }
    if let Some(&header) = classes
        .iter()
        .find(|&&n| aliases::stem(&ctx.sym(n).file) == aliases::stem(file))
    {
        return Some(header);
    }
    let shared = |p: &str| -> usize {
        let a: Vec<&str> = p.split('/').collect();
        let b: Vec<&str> = file.split('/').collect();
        let mut i = 0;
        while i + 1 < a.len() && i + 1 < b.len() && a[i] == b[i] {
            i += 1;
        }
        i
    };
    let mut best = classes[0];
    for &n in classes {
        if shared(&ctx.sym(n).file) > shared(&ctx.sym(best).file) {
            best = n;
        }
    }
    Some(best)
}

fn named_classes(
    ctx: &Ctx,
    segments: &[String],
    site: &NameSite,
    visible: &dyn Fn(SymId) -> bool,
) -> Vec<SymId> {
    ctx.named(segments.last().unwrap())
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            is_c_family(s.language) && class_kind(s.kind) && Some(n) != site.self_id && visible(n)
        })
        .collect()
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum UsingKind {
    Declaration,
    Directive,
    Alias,
}

#[derive(Clone, Debug)]
pub struct Using {
    pub line: u32,
    pub kind: UsingKind,
    pub name: String,
    pub path: String,
}

/// The `using` declarations and directives and namespace aliases of a file, in order.
pub fn usings_in(ctx: &Ctx, file: &str) -> Rc<Vec<Using>> {
    ctx.memo(memo::USINGS, file.to_string(), || {
        let mut found = Vec::new();
        if let Some(src) = ctx.source(file) {
            if src.text.contains("using") || src.text.contains("namespace") {
                let text = js::strip_comments_cpp(&src.text);
                let path = r"(?:::\s*)?[A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)*";
                let re = ctx.re(&format!(
                    r"\busing\s+namespace\s+({path})\s*;|\busing\s+({path}\s*::\s*[A-Za-z_]\w*)\s*;|\bnamespace\s+([A-Za-z_]\w*)\s*=\s*({path})\s*;"
                ));
                let mut line = 1u32;
                let mut at = 0usize;
                let bytes = text.as_bytes();
                for m in js::all(&re, &text) {
                    let idx = m.get(0).unwrap().start();
                    while at < idx {
                        if bytes[at] == b'\n' {
                            line += 1;
                        }
                        at += 1;
                    }
                    if let Some(p) = m.get(1) {
                        found.push(Using {
                            line,
                            kind: UsingKind::Directive,
                            name: String::new(),
                            path: js::squash(p.as_str()),
                        });
                    } else if let Some(p) = m.get(2) {
                        let path = js::squash(p.as_str());
                        let name = path.rsplit("::").next().unwrap_or(&path).to_string();
                        found.push(Using {
                            line,
                            kind: UsingKind::Declaration,
                            name,
                            path,
                        });
                    } else if let (Some(n), Some(p)) = (m.get(3), m.get(4)) {
                        found.push(Using {
                            line,
                            kind: UsingKind::Alias,
                            name: n.as_str().to_string(),
                            path: js::squash(p.as_str()),
                        });
                    }
                }
            }
        }
        Rc::new(found)
    })
}
