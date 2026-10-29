//! C++ type aliases in a declared type: `typedef X Y;` and `using Y = X;`
//! (upstream `cpp-type-aliases.ts`).
//!
//! A name is looked up the way C++ looks it up: from the innermost scope of
//! the code that uses it outwards — an alias declared earlier in the calling
//! function, the caller's class and its bases, the classes and namespaces
//! around it, the global scope. The first scope that declares the name
//! decides. An alias is read from its own declaration and followed from its
//! own scope, for a few hops at most; a qualified name is resolved a segment
//! at a time. Template arguments are dropped.

use super::ctx::{memo, Ctx, Ref};
use super::family::is_c_family;
use super::js;
use crate::symbols::SymId;
use codegraph_core::NodeKind;
use std::collections::HashSet;
use std::rc::Rc;

/// Three-valued, as upstream's `T | null | undefined`.
#[derive(Clone, Debug, PartialEq)]
pub enum Tri<T> {
    /// `undefined`: nothing decided here; the caller keeps its own reading.
    Undefined,
    /// `null`: decided, and it names no particular class.
    Null,
    Found(T),
}

impl<T> Tri<T> {
    pub fn is_undefined(&self) -> bool {
        matches!(self, Tri::Undefined)
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Tri::Null)
    }
    pub fn found(self) -> Option<T> {
        match self {
            Tri::Found(t) => Some(t),
            _ => None,
        }
    }
}

fn declaring_kind(k: NodeKind) -> bool {
    matches!(
        k,
        NodeKind::Class
            | NodeKind::Struct
            | NodeKind::Union
            | NodeKind::Enum
            | NodeKind::Interface
            | NodeKind::Namespace
            | NodeKind::Module
    )
}

pub fn class_kind(k: NodeKind) -> bool {
    matches!(k, NodeKind::Class | NodeKind::Struct | NodeKind::Union)
}

const MAX_HOPS: u32 = 4;
const MAX_BASE_DEPTH: u32 = 4;
const HEAD_LINES: usize = 12;

/// What a type named through an alias resolved to.
#[derive(Clone, Debug, PartialEq)]
pub struct AliasedType {
    pub target: Vec<String>,
    pub rest: Vec<String>,
    /// The qualified scope `target` is written in (`""` for the global scope).
    pub scope: String,
    pub pointer: bool,
    pub class_template: bool,
}

#[derive(Clone, Debug)]
struct TypeName {
    names: Vec<String>,
    templated: Vec<bool>,
    global: bool,
}

/// `SkipList<const char*, Cmp<int>>::Iterator` → `SkipList::Iterator`.
pub fn strip_template_arguments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0i32;
    for c in text.chars() {
        if c == '<' {
            depth += 1;
        } else if c == '>' {
            depth = (depth - 1).max(0);
        } else if depth == 0 {
            out.push(c);
        }
    }
    out
}

fn type_name(ctx: &Ctx, raw: &str) -> Option<TypeName> {
    let kw = ctx.re(r"\b(?:const|volatile|mutable|typename|template|class|struct|union|enum)\b");
    let ptr = ctx.re(r"[&*]+");
    let text = js::replace_with(&ptr, &js::replace_with(&kw, raw, " "), " ");
    let b = text.as_bytes();
    let mut parts: Vec<String> = Vec::new();
    let mut templated: Vec<bool> = Vec::new();
    let mut depth = 0i32;
    let mut parens = 0i32;
    let mut part = String::new();
    let mut args = false;
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if depth > 0 && (c == b'(' || c == b')') {
            parens = (parens + if c == b'(' { 1 } else { -1 }).max(0);
        } else if parens > 0 {
        } else if c == b'<' {
            depth += 1;
            args = true;
        } else if c == b'>' {
            depth = (depth - 1).max(0);
        } else if depth > 0 {
        } else if c == b':' && js::at(&text, i + 1) == b':' {
            parts.push(part.trim().to_string());
            templated.push(args);
            part.clear();
            args = false;
            i += 1;
        } else {
            // (A multi-byte character is copied whole.)
            let ch = text[i..].chars().next().unwrap();
            part.push(ch);
            i += ch.len_utf8();
            continue;
        }
        i += 1;
    }
    parts.push(part.trim().to_string());
    templated.push(args);
    let global = parts[0].is_empty();
    if global {
        parts.remove(0);
        templated.remove(0);
    }
    if parts.is_empty() || !parts.iter().all(|p| js::is_ident(p)) {
        return None;
    }
    Some(TypeName {
        names: parts,
        templated,
        global,
    })
}

fn scopes_for(name: &TypeName, scopes: &[String]) -> Vec<String> {
    if name.global {
        vec![String::new()]
    } else {
        scopes.to_vec()
    }
}

/// Is a written type a pointer, not counting its template arguments?
pub fn is_pointer_type(raw: &str) -> bool {
    strip_template_arguments(raw).contains('*')
}

/// The `::` segments of a written type: `const ns::Foo<T>&` → `['ns', 'Foo']`.
pub fn type_segments(ctx: &Ctx, raw: &str) -> Option<Vec<String>> {
    type_name(ctx, raw).map(|t| t.names)
}

/// The top-level arguments of the first template argument list in a written type.
pub fn template_arguments(raw: &str) -> Option<Vec<String>> {
    let open = raw.find('<')?;
    let b = raw.as_bytes();
    let mut depth = 0i32;
    for i in open..b.len() {
        if b[i] == b'<' {
            depth += 1;
        } else if b[i] == b'>' {
            depth -= 1;
            if depth == 0 {
                return Some(
                    split_top_level(&raw[open + 1..i])
                        .into_iter()
                        .map(|a| a.trim().to_string())
                        .collect(),
                );
            }
        }
    }
    None
}

/// Scopes visible from inside `qualified`, innermost first, ending with the global scope.
pub fn scopes_within(qualified: &str) -> Vec<String> {
    let parts: Vec<&str> = if qualified.is_empty() {
        Vec::new()
    } else {
        qualified.split("::").collect()
    };
    (0..=parts.len())
        .rev()
        .map(|i| parts[..i].join("::"))
        .collect()
}

/// The scope a declaration sits in: `leveldb::MemTable` for `leveldb::MemTable::Table`.
pub fn parent_scope(qualified: &str) -> &str {
    qualified.rfind("::").map_or("", |i| &qualified[..i])
}

/// The scope the code making `ref` is written in: its class for a method, itself for a class.
fn caller_scope(ctx: &Ctx, caller: SymId) -> String {
    let s = ctx.sym(caller);
    if s.kind == NodeKind::File {
        return String::new();
    }
    if declaring_kind(s.kind) {
        s.qualified_name.clone()
    } else {
        parent_scope(&s.qualified_name).to_string()
    }
}

/// The text of `node`'s declaration, cut to its columns when they fit the lines.
pub fn declaration_text(ctx: &Ctx, node: SymId) -> Option<String> {
    let s = ctx.sym(node);
    let src = ctx.source(&s.file)?;
    let from = (s.start_line as usize).checked_sub(1)?;
    let to = (s.end_line.max(s.start_line) as usize).min(src.lines.len());
    if from >= to {
        return None;
    }
    let mut span: Vec<String> = src.lines[from..to].to_vec();
    let whole = span.join("\n");
    let sc = s.start_column as usize;
    let ec = s.end_column as usize;
    if span.len() == 1 {
        span[0] = js::slice(&span[0], sc, ec).to_string();
    } else {
        span[0] = js::slice_from(&span[0], sc).to_string();
        let last = span.len() - 1;
        span[last] = js::slice(&span[last], 0, ec).to_string();
    }
    let cut = span.join("\n");
    Some(if js::test(&ctx.re(r"\b(?:typedef|using)\b"), &cut) {
        cut
    } else {
        whole
    })
}

/// Split `text` on commas outside `<>`, `()`, `[]` and `{}`.
pub fn split_top_level(text: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, c) in text.bytes().enumerate() {
        match c {
            b'<' | b'(' | b'[' | b'{' => depth += 1,
            b'>' | b')' | b']' | b'}' => depth = (depth - 1).max(0),
            b',' if depth == 0 => {
                parts.push(text[start..i].to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].to_string());
    parts
}

/// What an alias of `name` declares in `text`, as written.
pub fn aliased_type_in(ctx: &Ctx, text: &str, name: &str) -> Option<String> {
    let e = js::escape(name);
    if let Some(u) = js::caps(
        &ctx.re(&format!(
            r"\busing\s+{e}\s*(?:\[\[[^\]]*\]\]\s*)?=\s*([^;]+)"
        )),
        text,
    ) {
        return Some(js::group(&u, 1).trim().to_string());
    }
    let t = js::caps(&ctx.re(r"\btypedef\b([^;]+)"), text)?;
    let first = split_top_level(js::group(&t, 1)).into_iter().next()?;
    let declared = js::caps(&ctx.re(&format!(r"^([\s\S]*?)\b{e}\s*$")), &first)?;
    let ty = js::group(&declared, 1).trim().to_string();
    (!ty.is_empty()).then_some(ty)
}

/// Is `alias` an alias template (`template <typename T> using Vec = …;`)?
fn is_alias_template(ctx: &Ctx, alias: SymId) -> bool {
    let s = ctx.sym(alias);
    let Some(src) = ctx.source(&s.file) else {
        return false;
    };
    let line = s.start_line as usize;
    let from = line.saturating_sub(4);
    let mut parts: Vec<String> = src.lines
        [from.min(src.lines.len())..(line.saturating_sub(1)).min(src.lines.len())]
        .to_vec();
    parts.push(
        js::slice(
            src.lines.get(line - 1).map_or("", |l| l.as_str()),
            0,
            s.start_column as usize,
        )
        .to_string(),
    );
    js::test(&ctx.re(r"\btemplate\s*<[^;{}]*>\s*$"), &parts.join(" "))
}

/// The lines of a function's body that declare an alias, last first.
fn local_alias_lines(ctx: &Ctx, caller: SymId, lines: &[String]) -> Rc<Vec<usize>> {
    ctx.memo(memo::LOCAL_ALIAS_LINES, caller.to_string(), || {
        let s = ctx.sym(caller);
        let last = (s.end_line.max(s.start_line) as usize).min(lines.len());
        let re = ctx.re(r"\b(?:typedef\b|using\s+\w+\s*=)");
        let mut found = Vec::new();
        let first = (s.start_line as usize).saturating_sub(1);
        let mut i = last;
        while i > first {
            i -= 1;
            if js::test(&re, &lines[i]) {
                found.push(i);
            }
        }
        Rc::new(found)
    })
}

/// Does the function making `ref` declare, before it, an alias of the name a
/// written type starts with?
pub fn declares_local_alias(ctx: &Ctx, written: &str, r: &Ref) -> bool {
    match type_name(ctx, written) {
        Some(name) if !written.trim_start().starts_with("::") => {
            local_aliased_type(ctx, &name.names[0], Some(r.from), r).is_some()
        }
        _ => false,
    }
}

/// The type an alias declared earlier in the calling function names, as written.
/// `Some(None)`: an alias line whose type can't be read.
fn local_aliased_type(
    ctx: &Ctx,
    name: &str,
    caller: Option<SymId>,
    r: &Ref,
) -> Option<Option<String>> {
    let caller = caller?;
    let s = ctx.sym(caller);
    if !matches!(s.kind, NodeKind::Function | NodeKind::Method) || s.file != r.file {
        return None;
    }
    let src = ctx.source(&r.file)?;
    let e = js::escape(name);
    let declares = ctx.re(&format!(
        r"\b(?:using\s+{e}\s*=|typedef\b[^;]*\b{e}\s*[;,])"
    ));
    for &i in local_alias_lines(ctx, caller, &src.lines).iter() {
        if i as i64 > r.line as i64 - 1 {
            continue;
        }
        let line = &src.lines[i];
        if line.contains(name) && js::test(&declares, line) {
            return Some(aliased_type_in(ctx, line, name));
        }
    }
    None
}

/// What a scope declares under a name.
#[derive(Clone, Debug)]
pub enum Declared {
    Alias(SymId),
    /// A class, struct or union, by its qualified name.
    Cls(String),
    /// An enum, namespace or other non-class type.
    Other,
    /// Aliases naming different types, none of them the calling file's.
    Ambiguous(Vec<SymId>),
}

pub type Visible<'v> = Option<&'v dyn Fn(SymId) -> bool>;

/// C and C++ declarations named `name` that can stand for a type or scope.
fn type_declarations(ctx: &Ctx, name: &str, visible: Visible) -> Vec<SymId> {
    ctx.named(name)
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            is_c_family(s.language)
                && (s.kind == NodeKind::TypeAlias || declaring_kind(s.kind))
                && visible.is_none_or(|v| v(n))
        })
        .collect()
}

fn declared_in(
    ctx: &Ctx,
    scope: &str,
    name: &str,
    named: &[SymId],
    templated: bool,
    r: &Ref,
) -> Option<Declared> {
    let qualified = if scope.is_empty() {
        name.to_string()
    } else {
        format!("{scope}::{name}")
    };
    let mut here: Vec<SymId> = named
        .iter()
        .copied()
        .filter(|&n| ctx.sym(n).qualified_name == qualified)
        .collect();
    if !templated {
        here.retain(|&n| ctx.kind(n) != NodeKind::TypeAlias || !is_alias_template(ctx, n));
    }
    if here.is_empty() {
        return None;
    }
    if here.iter().any(|&n| class_kind(ctx.kind(n))) {
        return Some(Declared::Cls(qualified));
    }
    if here.iter().any(|&n| ctx.kind(n) != NodeKind::TypeAlias) {
        return Some(Declared::Other);
    }
    Some(match pick_alias(ctx, &here, r) {
        Some(a) => Declared::Alias(a),
        None => Declared::Ambiguous(here),
    })
}

/// `p.replace(/\.[^./\\]+$/, '')`: the path without its extension.
pub fn stem(p: &str) -> &str {
    if let Some(d) = p.rfind('.') {
        let ext = &p[d + 1..];
        if !ext.is_empty() && !ext.contains(['/', '\\']) {
            return &p[..d];
        }
    }
    p
}

/// One of several aliases a scope declares under one name.
fn pick_alias(ctx: &Ctx, aliases: &[SymId], r: &Ref) -> Option<SymId> {
    if aliases.len() == 1 {
        return Some(aliases[0]);
    }
    let same: Vec<SymId> = aliases
        .iter()
        .copied()
        .filter(|&a| ctx.sym(a).file == r.file)
        .collect();
    if same.len() == 1 {
        return Some(same[0]);
    }
    let header: Vec<SymId> = aliases
        .iter()
        .copied()
        .filter(|&a| stem(&ctx.sym(a).file) == stem(&r.file))
        .collect();
    if header.len() == 1 {
        return Some(header[0]);
    }
    let mut named: Vec<Option<String>> = Vec::new();
    for &a in aliases {
        let text = declaration_text(ctx, a);
        let ty = text.and_then(|t| aliased_type_in(ctx, &t, &ctx.sym(a).name));
        let v = ty.map(|t| type_segments(ctx, &t).map_or(t, |s| s.join("::")));
        if !named.contains(&v) {
            named.push(v);
        }
    }
    (named.len() == 1 && named[0].is_some()).then_some(aliases[0])
}

/// The template-parameter names a header ending where a declaration starts declares.
fn collect_template_header(
    ctx: &Ctx,
    lines: &[String],
    start_line: u32,
    start_column: u32,
    names: &mut HashSet<String>,
) {
    let line = start_line as usize;
    let from = line.saturating_sub(9).min(lines.len());
    let to = line.saturating_sub(1).min(lines.len());
    let mut parts: Vec<String> = lines[from..to.max(from)].to_vec();
    parts.push(
        js::slice(
            lines.get(line.wrapping_sub(1)).map_or("", |l| l.as_str()),
            0,
            start_column as usize,
        )
        .to_string(),
    );
    let before = parts.join("\n");
    let Some(at) = before.rfind("template") else {
        return;
    };
    let header = ctx.re(r"^template\s*<([\s\S]*)>(?:\s*(?:\[\[[^\]]*\]\]|(?:static|inline|constexpr|consteval|virtual|explicit|friend|extern)\b|[A-Z][A-Z0-9]*_[A-Z0-9_]*\b(?:\s*\([^()]*\))?))*\s*$");
    let Some(h) = js::caps(&header, &before[at..]) else {
        return;
    };
    for item in split_top_level(js::group(&h, 1)) {
        add_template_parameter(ctx, &item, names);
    }
}

fn add_template_parameter(ctx: &Ctx, item: &str, names: &mut HashSet<String>) {
    let stripped = strip_template_arguments(item);
    let declared = stripped.split('=').next().unwrap_or("");
    if let Some(m) = js::caps(&ctx.re(r"([A-Za-z_]\w*)\s*$"), declared) {
        let name = js::group(&m, 1);
        if name != "typename" && name != "class" {
            names.insert(name.to_string());
        }
    }
}

const MAX_TEMPLATE_HEADER: usize = 1000;

fn template_header_end(text: &str, open: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut parens = 0i32;
    let mut i = open;
    while i < b.len() && i < open + MAX_TEMPLATE_HEADER {
        let c = b[i];
        if c == b'(' || c == b'[' {
            parens += 1;
        } else if c == b')' || c == b']' {
            parens = (parens - 1).max(0);
        } else if parens > 0 {
        } else if c == b'<' {
            depth += 1;
        } else if c == b'>' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        } else if c == b';' || c == b'{' || c == b'}' {
            return None;
        }
        i += 1;
    }
    None
}

fn typedef_declarators(text: &str, from: usize) -> String {
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut start = from;
    let mut i = from;
    while i < b.len() && i < from + 4 * MAX_TEMPLATE_HEADER {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth < 0 {
                    return String::new();
                }
                if depth == 0 {
                    start = i + 1;
                }
            }
            b';' if depth == 0 => return text[start..i].to_string(),
            _ => {}
        }
        i += 1;
    }
    String::new()
}

/// The names C++ source text declares as template parameters or aliases.
pub fn type_names_declared(ctx: &Ctx, text: &str) -> HashSet<String> {
    let mut names = HashSet::new();
    for m in js::all(&ctx.re(r"\btemplate\s*<"), text) {
        let whole = m.get(0).unwrap();
        let open = whole.end() - 1;
        if let Some(close) = template_header_end(text, open) {
            if close > open {
                for item in split_top_level(&text[open + 1..close]) {
                    add_template_parameter(ctx, &item, &mut names);
                }
            }
        }
    }
    for m in js::all(
        &ctx.re(r"\busing\s+([A-Za-z_]\w*)\s*(?:\[\[[^\]]*\]\]\s*)?=(?!=)"),
        text,
    ) {
        names.insert(js::group(&m, 1).to_string());
    }
    let fn_ptr = ctx.re(r"\(\s*(?:[A-Za-z_]\w*\s*::\s*)*[*&^]+\s*(?:const\s+)?([A-Za-z_]\w*)\s*\)");
    let tail_params = ctx.re(r"\([^()]*\)\s*(?:const\s*)?$");
    let last_name = ctx.re(r"([A-Za-z_]\w*)\s*(?:\[[^\]]*\]\s*)*$");
    for m in js::all(&ctx.re(r"\btypedef\b"), text) {
        let end = m.get(0).unwrap().end();
        for declarator in split_top_level(&typedef_declarators(text, end)) {
            let name = js::caps(&fn_ptr, &declarator)
                .map(|c| js::group(&c, 1).to_string())
                .or_else(|| {
                    let cut = js::replace_first(&tail_params, &declarator, |_| String::new());
                    js::caps(&last_name, &cut).map(|c| js::group(&c, 1).to_string())
                });
            if let Some(n) = name {
                names.insert(n);
            }
        }
    }
    names
}

/// The template parameters in scope where `node` is declared.
pub fn template_parameters(ctx: &Ctx, node: SymId) -> Rc<HashSet<String>> {
    ctx.memo(memo::TEMPLATE_PARAMS, node.to_string(), || {
        let mut names = HashSet::new();
        let s = ctx.sym(node);
        if let Some(src) = ctx.source(&s.file) {
            collect_template_header(ctx, &src.lines, s.start_line, s.start_column, &mut names);
            let mut scope = parent_scope(&s.qualified_name).to_string();
            while !scope.is_empty() {
                for &cls in ctx.by_qname(&scope) {
                    let c = ctx.sym(cls);
                    if !is_c_family(c.language) || !class_kind(c.kind) || c.file != s.file {
                        continue;
                    }
                    if s.start_line < c.start_line || s.start_line > c.end_line.max(c.start_line) {
                        continue;
                    }
                    collect_template_header(
                        ctx,
                        &src.lines,
                        c.start_line,
                        c.start_column,
                        &mut names,
                    );
                }
                scope = parent_scope(&scope).to_string();
            }
        }
        Rc::new(names)
    })
}

/// The qualified names of the classes `cls` derives from, read from its base clause.
fn base_classes_of(ctx: &Ctx, cls: &str) -> Rc<Vec<String>> {
    if let Some(hit) = ctx.memo_get::<Rc<Vec<String>>>(memo::BASES, cls) {
        return hit;
    }
    ctx.memo_set(memo::BASES, cls.to_string(), Rc::new(Vec::<String>::new()));
    let mut bases: Vec<String> = Vec::new();
    let colon_re = ctx.re(r"(?<!:):(?!:)");
    let access = ctx.re(r"\b(?:public|protected|private|virtual)\b");
    for &decl in ctx.by_qname(cls) {
        let d = ctx.sym(decl);
        if !is_c_family(d.language) || !class_kind(d.kind) {
            continue;
        }
        let Some(src) = ctx.source(&d.file) else {
            continue;
        };
        let from = (d.start_line as usize).saturating_sub(1);
        if from >= src.lines.len() {
            continue;
        }
        let to = (from + HEAD_LINES).min(src.lines.len());
        let mut head: Vec<String> = src.lines[from..to].to_vec();
        head[0] = js::slice_from(&head[0], d.start_column as usize).to_string();
        let text = head.join("\n");
        let Some(brace) = text.find('{') else {
            continue;
        };
        if text[..brace].contains(';') {
            continue;
        }
        let Some(colon) = js::search(&colon_re, &text[..brace]) else {
            continue;
        };
        for part in split_top_level(&text[colon + 1..brace]) {
            let Some(name) = type_name(ctx, &js::replace_with(&access, &part, " ")) else {
                continue;
            };
            let scopes = scopes_for(&name, &scopes_within(parent_scope(cls)));
            if let Some(base) = class_named(ctx, &name.names, &scopes) {
                if base != cls && !bases.contains(&base) {
                    bases.push(base);
                }
            }
        }
    }
    let bases = Rc::new(bases);
    ctx.memo_set(memo::BASES, cls.to_string(), bases.clone());
    bases
}

/// The class `names` spells looked up from `scope`, spelled out or a segment at a time.
fn class_reached(ctx: &Ctx, names: &[String], scope: &str, r: &Ref) -> Option<String> {
    let scopes = scopes_within(scope);
    if let Some(spelled) = class_named(ctx, names, &scopes) {
        return Some(spelled);
    }
    let name = TypeName {
        names: names.to_vec(),
        templated: vec![false; names.len()],
        global: false,
    };
    match lookup_segments(ctx, &name, &scopes, r, None).0 {
        Some(Declared::Cls(c)) => Some(c),
        _ => None,
    }
}

/// The qualified name of the class `names` spells, looked up from `scopes`.
pub fn class_named(ctx: &Ctx, names: &[String], scopes: &[String]) -> Option<String> {
    let spelled = names.join("::");
    for scope in scopes {
        let qualified = if scope.is_empty() {
            spelled.clone()
        } else {
            format!("{scope}::{spelled}")
        };
        if ctx.by_qname(&qualified).iter().any(|&n| {
            let s = ctx.sym(n);
            is_c_family(s.language) && class_kind(s.kind)
        }) {
            return Some(qualified);
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn member_of(
    ctx: &Ctx,
    cls: &str,
    name: &str,
    named: &[SymId],
    templated: bool,
    r: &Ref,
    depth: u32,
    seen: &mut HashSet<String>,
) -> Option<Declared> {
    let own = declared_in(ctx, cls, name, named, templated, r);
    if own.is_some() || depth >= MAX_BASE_DEPTH {
        return own;
    }
    seen.insert(cls.to_string());
    for base in base_classes_of(ctx, cls).iter() {
        if seen.contains(base) {
            continue;
        }
        if let Some(inherited) = member_of(ctx, base, name, named, templated, r, depth + 1, seen) {
            return Some(inherited);
        }
    }
    None
}

fn lookup_unqualified(
    ctx: &Ctx,
    name: &str,
    templated: bool,
    scopes: &[String],
    r: &Ref,
    visible: Visible,
) -> Option<Declared> {
    let named = type_declarations(ctx, name, visible);
    if named.is_empty() {
        return None;
    }
    for scope in scopes {
        let found = if scope.is_empty() {
            declared_in(ctx, "", name, &named, templated, r)
        } else {
            member_of(
                ctx,
                scope,
                name,
                &named,
                templated,
                r,
                0,
                &mut HashSet::new(),
            )
        };
        if found.is_some() {
            return found;
        }
    }
    None
}

/// The type a C++ declared type names once the aliases in it are followed,
/// looked up from the code that makes `r`.
pub fn resolve_aliased_type(ctx: &Ctx, raw: &str, r: &Ref) -> Tri<AliasedType> {
    let Some(name) = type_name(ctx, raw) else {
        return Tri::Undefined;
    };
    let caller = r.from;
    let scope = caller_scope(ctx, caller);
    let local = if name.global {
        None
    } else {
        local_aliased_type(ctx, &name.names[0], Some(caller), r)
    };
    let resolved = match local {
        Some(written) => {
            let params = template_parameters(ctx, caller);
            match expand(
                ctx,
                written.as_deref(),
                &name.names[1..],
                &name.templated[1..],
                &scope,
                r,
                1,
                Some(&params),
            ) {
                Some(t) => Tri::Found(t),
                None => Tri::Null,
            }
        }
        None => follow(ctx, &name, &scope, r, 0),
    };
    match resolved {
        Tri::Found(mut t) if is_pointer_type(raw) => {
            t.pointer = true;
            Tri::Found(t)
        }
        other => other,
    }
}

/// The name receiver inference gives a type resolved through aliases.
pub fn aliased_type_name(resolved: &AliasedType) -> String {
    let mut parts = vec![resolved.target.last().cloned().unwrap_or_default()];
    parts.extend(resolved.rest.iter().cloned());
    parts.join("::")
}

/// The qualified name of the project class a type resolved through aliases is.
pub fn aliased_class(ctx: &Ctx, resolved: &AliasedType, r: Option<&Ref>) -> Option<String> {
    let mut names = resolved.target.clone();
    names.extend(resolved.rest.iter().cloned());
    match r {
        Some(r) => class_reached(ctx, &names, &resolved.scope, r),
        None => class_named(ctx, &names, &scopes_within(&resolved.scope)),
    }
}

/// What a C++ type name names when it is looked up for a class.
#[derive(Clone, Debug)]
pub enum NamedClass {
    Cls(String),
    /// An alias that leads to none of the project's classes.
    Alias(SymId),
}

/// The class a C++ type written as `written` names, looked up from `scopes`.
pub fn class_named_lookup(
    ctx: &Ctx,
    written: &str,
    scopes: &[String],
    r: &Ref,
    visible: Visible,
) -> Tri<NamedClass> {
    let Some(name) = type_name(ctx, written) else {
        return Tri::Null;
    };
    let (found, next) = lookup_segments(ctx, &name, scopes, r, visible);
    let Some(found) = found else {
        return Tri::Undefined;
    };
    match found {
        Declared::Other => Tri::Null,
        Declared::Cls(c) => Tri::Found(NamedClass::Cls(c)),
        Declared::Ambiguous(list) => {
            let mut sorted = list.clone();
            sorted.sort_by(|&a, &b| {
                let (x, y) = (ctx.sym(a), ctx.sym(b));
                x.file.cmp(&y.file).then(x.start_line.cmp(&y.start_line))
            });
            if next == name.names.len() {
                Tri::Found(NamedClass::Alias(sorted[0]))
            } else {
                Tri::Null
            }
        }
        Declared::Alias(alias) => {
            let a = ctx.sym(alias);
            let text = declaration_text(ctx, alias);
            let aliased_ty = text.and_then(|t| aliased_type_in(ctx, &t, &a.name));
            let params = template_parameters(ctx, alias);
            let aliased = expand(
                ctx,
                aliased_ty.as_deref(),
                &name.names[next..],
                &name.templated[next..],
                parent_scope(&a.qualified_name),
                r,
                1,
                Some(&params),
            );
            if let Some(aliased) = aliased.filter(|t| !t.pointer) {
                let mut names = aliased.target.clone();
                names.extend(aliased.rest.iter().cloned());
                if let Some(cls) = class_reached(ctx, &names, &aliased.scope, r) {
                    return Tri::Found(NamedClass::Cls(cls));
                }
            }
            if next == name.names.len() {
                Tri::Found(NamedClass::Alias(alias))
            } else {
                Tri::Null
            }
        }
    }
}

/// The type the C++ alias written as `written` names, as its declaration writes it.
pub fn aliased_type_written(
    ctx: &Ctx,
    written: &str,
    scopes: &[String],
    r: &Ref,
) -> Tri<(String, SymId)> {
    let Some(name) = type_name(ctx, written) else {
        return Tri::Undefined;
    };
    let (found, next) = lookup_segments(ctx, &name, &scopes_for(&name, scopes), r, None);
    let alias = match found {
        None | Some(Declared::Other) | Some(Declared::Cls(_)) => return Tri::Undefined,
        Some(Declared::Ambiguous(_)) => return Tri::Null,
        Some(Declared::Alias(a)) => a,
    };
    if next != name.names.len() || is_alias_template(ctx, alias) {
        return Tri::Null;
    }
    let text = declaration_text(ctx, alias);
    let aliased = text.and_then(|t| aliased_type_in(ctx, &t, &ctx.sym(alias).name));
    match aliased {
        Some(a) if !js::test(&ctx.re(r"\b(?:typename|decltype)\b"), &a) => Tri::Found((a, alias)),
        _ => Tri::Null,
    }
}

/// What a written name declares, looked up from `scopes`, and how many of its segments that took.
fn lookup_segments(
    ctx: &Ctx,
    name: &TypeName,
    scopes: &[String],
    r: &Ref,
    visible: Visible,
) -> (Option<Declared>, usize) {
    let mut found = lookup_unqualified(ctx, &name.names[0], name.templated[0], scopes, r, visible);
    let mut next = 1;
    if found.is_none() && name.names.len() > 1 {
        let mut i = name.names.len();
        while i >= 2 && found.is_none() {
            let spelled = &name.names[..i];
            let last = &spelled[spelled.len() - 1];
            let named = type_declarations(ctx, last, visible);
            let prefix = spelled[..spelled.len() - 1].join("::");
            for s in scopes {
                let scope = if s.is_empty() {
                    prefix.clone()
                } else {
                    format!("{s}::{prefix}")
                };
                found = declared_in(ctx, &scope, last, &named, name.templated[i - 1], r);
                if found.is_some() {
                    break;
                }
            }
            next = i;
            i -= 1;
        }
    }
    while let Some(Declared::Cls(cls)) = &found {
        if next >= name.names.len() {
            break;
        }
        let member = &name.names[next];
        let named = type_declarations(ctx, member, visible);
        found = member_of(
            ctx,
            &cls.clone(),
            member,
            &named,
            name.templated[next],
            r,
            0,
            &mut HashSet::new(),
        );
        next += 1;
    }
    (found, next)
}

fn follow(ctx: &Ctx, name: &TypeName, scope: &str, r: &Ref, hops: u32) -> Tri<AliasedType> {
    let (found, next) =
        lookup_segments(ctx, name, &scopes_for(name, &scopes_within(scope)), r, None);
    let Some(Declared::Alias(alias)) = found else {
        return Tri::Undefined;
    };
    if hops >= MAX_HOPS {
        return Tri::Null;
    }
    let a = ctx.sym(alias);
    let text = declaration_text(ctx, alias);
    let aliased = text.and_then(|t| aliased_type_in(ctx, &t, &a.name));
    let params = template_parameters(ctx, alias);
    match expand(
        ctx,
        aliased.as_deref(),
        &name.names[next..],
        &name.templated[next..],
        parent_scope(&a.qualified_name),
        r,
        hops + 1,
        Some(&params),
    ) {
        Some(t) => Tri::Found(t),
        None => Tri::Null,
    }
}

/// Continue from the type an alias names, as written. None: what it names is no particular class.
#[allow(clippy::too_many_arguments)]
fn expand(
    ctx: &Ctx,
    written: Option<&str>,
    rest: &[String],
    rest_templated: &[bool],
    scope: &str,
    r: &Ref,
    hops: u32,
    parameters: Option<&HashSet<String>>,
) -> Option<AliasedType> {
    let written = written?;
    if js::test(&ctx.re(r"\btypename\b"), written) {
        return None;
    }
    let target = type_name(ctx, written)?;
    if parameters.is_some_and(|p| p.contains(&target.names[0])) {
        return None;
    }
    let pointer = is_pointer_type(written);
    let mut names = target.names.clone();
    names.extend(rest.iter().cloned());
    let mut templated = target.templated.clone();
    templated.extend(rest_templated.iter().copied());
    let further = follow(
        ctx,
        &TypeName {
            names: names.clone(),
            templated,
            global: target.global,
        },
        scope,
        r,
        hops,
    );
    match further {
        Tri::Null => return None,
        Tri::Found(mut t) => {
            t.pointer = t.pointer || pointer;
            return Some(t);
        }
        Tri::Undefined => {}
    }
    let from = if target.global {
        String::new()
    } else {
        scope.to_string()
    };
    let cls = class_named(ctx, &names, &scopes_within(&from));
    let class_template = cls.as_ref().is_some_and(|c| {
        ctx.by_qname(c).iter().any(|&n| {
            let s = ctx.sym(n);
            is_c_family(s.language) && class_kind(s.kind) && !template_parameters(ctx, n).is_empty()
        })
    });
    Some(AliasedType {
        target: target.names,
        rest: rest.to_vec(),
        scope: from,
        pointer,
        class_template,
    })
}
