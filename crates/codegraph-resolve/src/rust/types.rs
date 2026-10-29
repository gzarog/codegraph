//! A Rust method call on a value — a local, a parameter, a field, a call's
//! value — is a call on the type that value is written with where the call
//! is, resolved as Rust resolves it on that type.

use super::ctx::{Ctx, Ref};
use super::impls::{angle_end, idents_of, strip_amp};
use super::scopes::{bounds_of, split_top_level, BindingKind, Binding, Generics, PatternSite, Scopes};
use super::uses::{is_ident, module_dir, module_path, modules_lead, parent_dir};
use super::*;
use crate::matcher::Match;
use crate::modules::common_prefix_len;
use crate::symbols::SymId;
use codegraph_core::NodeKind;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A Rust type as some declaration writes it: its text, and the file and
/// offset it is written at, whose `use`s, generics and `impl` say what its
/// names are.
#[derive(Debug, Clone, Default)]
pub struct Written {
    pub text: String,
    pub file: String,
    pub at: usize,
    /// The type `Self` names in a type's own declaration (an enum's variant).
    pub self_: Option<String>,
    /// That declaration's own generic parameters.
    pub generics: Option<String>,
    /// The `Box`, `Rc`, `Arc`… the value is held in.
    pub wrappers: Vec<String>,
    /// Generic parameters a call's value fixes, which nothing here says.
    pub opaque: Vec<String>,
    /// What a declaration's own parameters are known by where the type is reached.
    pub bounds: Option<Rc<HashMap<String, (Vec<String>, String, usize)>>>,
}

impl Written {
    pub fn new(text: impl Into<String>, file: &str, at: usize) -> Written {
        Written {
            text: text.into(),
            file: file.to_string(),
            at,
            ..Default::default()
        }
    }
    fn with_text(&self, text: impl Into<String>) -> Written {
        Written {
            text: text.into(),
            ..self.clone()
        }
    }
}

/// What a Rust type is, for calling a method on it.
#[derive(Debug, Clone)]
pub enum Read {
    /// A type by its path.
    Named { name: String, path: Vec<String>, args: String, wrappers: Vec<String>, by_ref: bool, written: Written },
    /// A trait object, an `impl Trait` or a bounded generic: the traits it is known by.
    Traits { traits: Vec<String>, wrappers: Vec<String>, by_ref: bool, written: Written },
    /// A type the project doesn't declare.
    Outside { name: String, wrappers: Vec<String>, by_ref: bool, written: Written },
    Unread,
}

impl Read {
    fn wrappers(&self) -> &[String] {
        match self {
            Read::Named { wrappers, .. } | Read::Traits { wrappers, .. } | Read::Outside { wrappers, .. } => wrappers,
            Read::Unread => &[],
        }
    }
    fn written(&self) -> Option<&Written> {
        match self {
            Read::Named { written, .. } | Read::Traits { written, .. } | Read::Outside { written, .. } => Some(written),
            Read::Unread => None,
        }
    }
    fn without_outer_wrapper(&self) -> Read {
        let mut r = self.clone();
        match &mut r {
            Read::Named { wrappers, .. } | Read::Traits { wrappers, .. } | Read::Outside { wrappers, .. } => {
                if !wrappers.is_empty() {
                    wrappers.remove(0);
                }
            }
            Read::Unread => {}
        }
        r
    }
}

/// A call's outcome: a target, no project target, or a type that can't be told.
#[derive(Debug, Clone, Copy)]
pub enum Hit {
    Found(Match),
    Nothing,
    Unread,
}

impl Hit {
    pub fn known(self) -> Option<Option<Match>> {
        match self {
            Hit::Found(m) => Some(Some(m)),
            Hit::Nothing => Some(None),
            Hit::Unread => None,
        }
    }
}

pub fn instance(target: SymId, confidence: f32) -> Match {
    Match {
        target,
        strategy: "instance-method",
        confidence,
    }
}

/// A type's path: `crate::work::Worker` → (`crate`, `work`), `Worker`; `Overrides<BuildArgs>` its arguments too.
pub fn path_head(text: &str) -> Option<(Vec<String>, String, String)> {
    let mut t = text.trim().trim_start_matches("::").trim_start().to_string();
    let mut args = String::new();
    if let Some(open) = t.find('<') {
        if angle_end(&t, open) != Some(t.len() - 1) {
            return None;
        }
        args = t[open + 1..t.len() - 1].trim().to_string();
        t = t[..open].trim().trim_end_matches("::").trim().to_string();
    }
    let mut parts: Vec<String> = t.split("::").map(|s| s.trim().to_string()).collect();
    if !parts.iter().all(|p| is_ident(p)) {
        return None;
    }
    let name = parts.pop()?;
    Some((parts, name, args))
}

fn head_name(text: &str) -> String {
    path_head(text).map(|h| h.1).unwrap_or_default()
}

/// A `{…}` or `(…)` that closes only at the text's end.
pub fn group_is_whole(text: &str) -> bool {
    let b = text.as_bytes();
    let mut depth = 0;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i == b.len() - 1;
                }
            }
            _ => {}
        }
    }
    false
}

/// A value that ends in a call — `Foo::new(a)`, `x.unwrap()` — as its callee's text and arguments.
pub fn trailing_call(value: &str) -> Option<(String, String)> {
    if !value.ends_with(')') {
        return None;
    }
    let b = value.as_bytes();
    let mut depth = 0;
    let mut i = b.len();
    while i > 0 {
        i -= 1;
        match b[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth -= 1;
                if depth == 0 {
                    if b[i] != b'(' {
                        return None;
                    }
                    return Some((value[..i].trim().to_string(), value[i + 1..value.len() - 1].to_string()));
                }
            }
            _ => {}
        }
    }
    None
}

/// The text after a Rust function signature's `->`.
pub fn return_text(signature: Option<&str>) -> Option<String> {
    let sig = signature?;
    if !sig.starts_with('(') {
        return None;
    }
    let b = sig.as_bytes();
    let mut depth = 0;
    for i in 0..b.len() {
        match b[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    let rest = sig[i + 1..].trim_start().strip_prefix("->")?.trim();
                    // A signature may carry a `where` clause.
                    let rest = super::uses::find_word(rest, "where").map_or(rest, |w| rest[..w].trim());
                    return (!rest.is_empty()).then(|| rest.to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// What `times` `?`s / `.unwrap()`s take a value of the written type to.
fn unwrapped(w: &Written, times: usize) -> Option<Written> {
    let mut text = w.text.clone();
    for _ in 0..times {
        let (_, name, args) = path_head(&text)?;
        if name != "Result" && name != "Option" {
            return None;
        }
        text = split_top_level(&args, ',').into_iter().next()?;
    }
    Some(w.with_text(text))
}

/// A path a `use` binds a name to, and the block it is written in.
#[derive(Debug, Clone)]
struct UseBinding {
    path: Vec<String>,
    from: isize,
    to: usize,
}

fn expand_use(tree: &str, prefix: &[String], bound: &mut Vec<(String, Vec<String>)>) {
    let t = tree.trim().trim_start_matches("::").trim();
    if t.is_empty() {
        return;
    }
    if let Some(open) = t.find('{') {
        if t.ends_with('}') {
            let head = t[..open].trim().trim_end_matches("::").trim();
            let mut within: Vec<String> = prefix.to_vec();
            if !head.is_empty() {
                within.extend(head.split("::").map(|s| s.trim().to_string()));
            }
            let b = t.as_bytes();
            let mut depth = 0;
            let mut start = open + 1;
            for i in open + 1..t.len() - 1 {
                match b[i] {
                    b'{' => depth += 1,
                    b'}' => depth -= 1,
                    b',' if depth == 0 => {
                        expand_use(&t[start..i], &within, bound);
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            expand_use(&t[start..t.len() - 1], &within, bound);
            return;
        }
    }
    let (path, alias) = match super::uses::find_word(t, "as") {
        Some(i) => (t[..i].trim(), Some(t[i + 2..].trim())),
        None => (t, None),
    };
    let segs: Vec<&str> = path.split("::").map(str::trim).collect();
    if segs.iter().any(|s| !(is_ident(s) || *s == "*")) {
        return;
    }
    let last = *segs.last().unwrap();
    if last == "*" || alias == Some("_") {
        return;
    }
    let mut segments: Vec<String> = prefix.to_vec();
    segments.extend(segs[..segs.len() - 1].iter().map(|s| s.to_string()));
    if last == "self" {
        let name = alias.map(str::to_string).or_else(|| segments.last().cloned());
        if let Some(n) = name {
            bound.push((n, segments));
        }
        return;
    }
    let name = alias.unwrap_or(last).to_string();
    segments.push(last.to_string());
    bound.push((name, segments));
}

impl<'a> Ctx<'a> {
    /// The names a file's `use`s bind, each to the path it binds, by the block they are written in.
    fn use_bindings(&self, file: &str) -> Rc<HashMap<String, Vec<UseBinding>>> {
        self.memo(20, file.to_string(), || {
            let mut out: HashMap<String, Vec<UseBinding>> = HashMap::new();
            if let Some(sc) = self.scopes(file) {
                for (at, tree) in super::uses::use_statements(&sc.code) {
                    let mut bound = Vec::new();
                    expand_use(tree, &[], &mut bound);
                    let block = sc.inner_block_at(at);
                    for (name, path) in bound {
                        out.entry(name).or_default().push(UseBinding {
                            path,
                            from: block.map_or(-1, |b| b.0 as isize),
                            to: block.map_or(usize::MAX, |b| b.1),
                        });
                    }
                }
            }
            Rc::new(out)
        })
    }

    /// The path a `use` in scope at `at` binds `name` to — the innermost block's — or a file-level `use`'s without `at`.
    pub fn use_path(&self, name: &str, file: &str, at: Option<usize>) -> Option<Vec<String>> {
        let b = self.use_bindings(file);
        let mut best: Option<&UseBinding> = None;
        for u in b.get(name).map(Vec::as_slice).unwrap_or(&[]) {
            let visible = match at {
                None => u.from < 0,
                Some(at) => at as isize > u.from && at < u.to,
            };
            if visible && best.is_none_or(|b| u.from > b.from) {
                best = Some(u);
            }
        }
        best.map(|b| b.path.clone())
    }

    fn rust_types_named(&self, name: &str) -> Vec<SymId> {
        self.rust_named(name).filter(|&i| DECLARED_TYPE_KINDS.contains(&self.syms.get(i).kind)).collect()
    }

    /// The project types a Rust type name can mean where it is written.
    pub fn type_lookup(&self, name: &str, written_path: &[String], file: &str, at: Option<usize>) -> Lookup {
        let mut segments: Vec<String> = written_path.to_vec();
        let mut real = name.to_string();
        let mut via_use = false;
        if segments.is_empty() {
            if let Some(bound) = self.use_path(name, file, at).filter(|b| b.len() > 1) {
                real = bound.last().unwrap().clone();
                segments = bound[..bound.len() - 1].to_vec();
                via_use = true;
            }
        } else if let Some(head) = self.use_path(&segments[0], file, at).filter(|h| !h.is_empty()) {
            let mut s = head;
            s.extend(segments[1..].iter().cloned());
            segments = s;
        }
        let outside = Lookup {
            nodes: Vec::new(),
            outside: true,
            undeclared: false,
            name: real.clone(),
        };
        let root = segments.first().cloned();
        let g = self.graph();
        if let Some(r) = &root {
            if matches!(r.as_str(), "std" | "core" | "alloc") {
                return outside;
            }
            if !g.project.contains(r) && g.dependencies.contains(r) {
                return outside;
            }
        }
        if segments.is_empty() && PRIMITIVE_TYPES.contains(&real.as_str()) {
            return outside;
        }
        let all = self.rust_types_named(&real);
        if all.is_empty() {
            if segments.is_empty() && (STD_TYPES.contains(&real.as_str()) || PRELUDE.contains(&real.as_str())) {
                return outside;
            }
            return Lookup {
                nodes: Vec::new(),
                outside: false,
                undeclared: true,
                name: real,
            };
        }
        let mut nodes: Vec<SymId> = all.into_iter().filter(|&n| self.item_visible_from(n, file)).collect();
        if !segments.is_empty() {
            let mut path = segments.clone();
            path.push(real.clone());
            if let Some(walked) = self.walked_items(&path, file, at) {
                let named: Vec<SymId> = nodes.iter().copied().filter(|n| walked.contains(n)).collect();
                if !named.is_empty() {
                    return Lookup {
                        nodes: named,
                        outside: false,
                        undeclared: false,
                        name: real,
                    };
                }
            }
        }
        let modules: Vec<String> = segments
            .iter()
            .filter(|s| !matches!(s.as_str(), "crate" | "self" | "super") && !g.project.contains(*s))
            .cloned()
            .collect();
        if !modules.is_empty() {
            let mut within: Vec<SymId> = nodes.iter().copied().filter(|&n| modules_lead(&modules, &self.syms.get(n).file)).collect();
            if within.len() > 1 && root.as_deref().is_some_and(|r| r == "crate" || g.project.contains(r)) {
                let depth = |n: SymId| module_path(&self.syms.get(n).file).len();
                let nearest = within.iter().map(|&n| depth(n)).min().unwrap_or(0);
                within.retain(|&n| depth(n) == nearest);
            }
            if !within.is_empty() {
                nodes = within;
            }
        }
        if let Some(r) = &root {
            nodes.retain(|&n| g.use_can_reach([r.as_str()], &self.syms.get(n).file, file));
        }
        if segments.is_empty() && !via_use {
            let same: Vec<SymId> = nodes.iter().copied().filter(|&n| self.syms.get(n).file == file).collect();
            if !same.is_empty() {
                return Lookup {
                    nodes: same,
                    outside: false,
                    undeclared: false,
                    name: real,
                };
            }
            let u = self.uses(file);
            if u.external.contains(&real) && !u.bound.contains(&real) {
                return outside;
            }
            let globbed: Vec<SymId> = nodes.iter().copied().filter(|&n| self.glob_scope(&real, n, &u, file)).collect();
            if !globbed.is_empty() {
                return Lookup {
                    nodes: globbed,
                    outside: false,
                    undeclared: false,
                    name: real,
                };
            }
            if STD_TYPES.contains(&real.as_str()) || PRELUDE.contains(&real.as_str()) {
                return outside;
            }
            let dir = parent_dir(file);
            let near: Vec<SymId> = nodes
                .iter()
                .copied()
                .filter(|&n| {
                    let f = &self.syms.get(n).file;
                    file.starts_with(&format!("{}/", module_dir(f))) || parent_dir(f) == dir
                })
                .collect();
            if !near.is_empty() {
                nodes = near;
            }
        }
        Lookup {
            nodes,
            outside: false,
            undeclared: false,
            name: real,
        }
    }

    /// What a Rust type as written is, for a method call on it.
    pub fn read_type(&self, written: &Written, r: &Ref<'_>) -> Read {
        let mut wrappers = written.wrappers.clone();
        let by_ref = written.text.trim().starts_with('&');
        let mut t = written.text.trim().to_string();
        for _ in 0..8 {
            let before = t.clone();
            t = strip_amp(&t).trim_start_matches("mut ").trim().to_string();
            if let Some((name, open)) = wrapper_head(&t) {
                if DEREF_WRAPPERS.contains(&name.as_str()) && angle_end(&t, open) == Some(t.len() - 1) {
                    let inner = t[open + 1..t.len() - 1].trim();
                    let inner = strip_lifetime_arg(inner);
                    t = inner.trim().to_string();
                    wrappers.push(name);
                }
            }
            if t == before {
                break;
            }
        }
        if t.is_empty() || t == "_" || t == "!" {
            return Read::Unread;
        }
        for kw in ["dyn", "impl"] {
            if let Some(rest) = t.strip_prefix(kw).filter(|r| r.starts_with(char::is_whitespace)) {
                let traits: Vec<String> = split_top_level(rest.trim(), '+')
                    .into_iter()
                    .filter(|b| !b.starts_with(['\'', '?']) && !MARKER_TRAITS.contains(&head_name(b).as_str()))
                    .collect();
                return if traits.is_empty() {
                    Read::Outside {
                        name: kw.into(),
                        wrappers,
                        by_ref,
                        written: written.clone(),
                    }
                } else {
                    Read::Traits {
                        traits,
                        wrappers,
                        by_ref,
                        written: written.clone(),
                    }
                };
            }
        }
        if t.starts_with(['(', '[', '*']) || is_fn_type(&t) {
            return Read::Outside {
                name: t[..1].to_string(),
                wrappers,
                by_ref,
                written: written.clone(),
            };
        }
        let Some((segments, name, args)) = path_head(&t) else { return Read::Unread };
        if !segments.is_empty() && (segments[0] == "Self" || self.generic_bounds(&segments[0], written).is_some()) {
            return Read::Unread;
        }
        if segments.is_empty() && name == "Self" {
            if let Some(s) = &written.self_ {
                return Read::Named {
                    name: s.clone(),
                    path: Vec::new(),
                    args: String::new(),
                    wrappers,
                    by_ref,
                    written: written.clone(),
                };
            }
            let line = self.scopes(&written.file).map_or(r.line, |sc| sc.line_at(written.at));
            let Some(st) = self.self_type(&written.file, line) else { return Read::Unread };
            if st.is_trait {
                return Read::Traits {
                    traits: vec![st.ty],
                    wrappers,
                    by_ref,
                    written: written.clone(),
                };
            }
            return Read::Named {
                name: st.ty,
                path: st.root.into_iter().collect(),
                args: String::new(),
                wrappers,
                by_ref,
                written: written.clone(),
            };
        }
        if segments.is_empty() {
            if written.opaque.contains(&name) {
                return Read::Unread;
            }
            if let Some((traits, file, at)) = written.bounds.as_ref().and_then(|b| b.get(&name)) {
                let traits: Vec<String> = traits
                    .iter()
                    .filter(|b| !b.trim().starts_with(['\'', '?']) && !MARKER_TRAITS.contains(&head_name(b).as_str()))
                    .cloned()
                    .collect();
                if traits.is_empty() {
                    return Read::Unread;
                }
                return Read::Traits {
                    traits,
                    wrappers,
                    by_ref,
                    written: Written::new(written.text.clone(), file, *at),
                };
            }
            if let Some(bounds) = self.generic_bounds(&name, written) {
                let traits: Vec<String> = bounds.into_iter().filter(|b| !MARKER_TRAITS.contains(&head_name(b).as_str())).collect();
                return if traits.is_empty() {
                    Read::Unread
                } else {
                    Read::Traits {
                        traits,
                        wrappers,
                        by_ref,
                        written: written.clone(),
                    }
                };
            }
            if PRIMITIVE_TYPES.contains(&name.as_str()) {
                return Read::Outside {
                    name,
                    wrappers,
                    by_ref,
                    written: written.clone(),
                };
            }
        }
        Read::Named {
            name,
            path: segments,
            args,
            wrappers,
            by_ref,
            written: written.clone(),
        }
    }

    /// The bounds of a generic parameter `name` where a type is written.
    pub fn generic_bounds(&self, name: &str, written: &Written) -> Option<Vec<String>> {
        if let Some(g) = &written.generics {
            return bounds_of(name, &[Generics { generics: g, where_: "" }]).map(|_| Vec::new());
        }
        let sc = self.scopes(&written.file)?;
        let f = sc.fn_at(written.at);
        let b = sc.impl_at(written.at);
        let mut items = Vec::new();
        if let Some(f) = f {
            items.push(Generics {
                generics: &f.generics,
                where_: &f.where_,
            });
        }
        if let Some(b) = b {
            items.push(Generics {
                generics: &b.generics,
                where_: &b.where_,
            });
        }
        if items.is_empty() {
            return None;
        }
        bounds_of(name, &items)
    }

    /// What the binding a Rust receiver names writes of its type, if it writes it.
    pub fn binding_type(&self, b: &Binding, sc: &Scopes, r: &Ref<'_>, depth: u8) -> Option<Written> {
        if let Some(t) = &b.ty {
            return Some(Written::new(t.clone(), r.file, b.at));
        }
        if b.kind == BindingKind::For {
            let element = self.iterated_element(b.scrutinee.as_deref()?, b.at, sc, r, depth)?;
            let Some(site) = &b.site else { return Some(element) };
            return if site.path.is_empty() { site.index.and_then(|i| tuple_field(&element, i)) } else { None };
        }
        if let Some(init) = &b.init {
            return self.value_type(init, b.at, sc, r, depth);
        }
        if let Some(site) = &b.site {
            return self.site_type(site, b.scrutinee.as_deref(), b.at, sc, r, depth);
        }
        None
    }

    /// The type of another local, as its binding in scope at `at` writes it.
    pub fn local_type(&self, name: &str, at: usize, sc: &Scopes, r: &Ref<'_>, depth: u8) -> Option<Written> {
        if depth > 3 {
            return None;
        }
        let (b, ambiguous) = sc.binding_at(name, at)?;
        if ambiguous {
            return None;
        }
        self.binding_type(b, sc, r, depth + 1)
    }

    /// The type a Rust value is, where its text says.
    pub fn value_type(&self, value: &str, at: usize, sc: &Scopes, r: &Ref<'_>, depth: u8) -> Option<Written> {
        if depth > 3 {
            return None;
        }
        let mut v = value.trim().to_string();
        let mut unwraps = 0;
        let mut awaits = 0;
        loop {
            let before = v.clone();
            if let Some(s) = strip_suffix_ws(&v, "await").and_then(|s| strip_suffix_ws(s, ".")) {
                v = s.to_string();
                awaits += 1;
            }
            if let Some(s) = v.strip_suffix('?') {
                v = s.trim_end().to_string();
                unwraps += 1;
            }
            if let Some((callee, _)) = trailing_call(&v) {
                for m in ["unwrap", "expect"] {
                    if let Some(c) = strip_suffix_ws(&callee, m).and_then(|s| strip_suffix_ws(s, ".")) {
                        v = c.to_string();
                        unwraps += 1;
                        break;
                    }
                }
            }
            if v == before {
                break;
            }
        }
        let v = strip_amp(&v).trim().to_string();
        let local = |text: &str| Written::new(text, r.file, at);
        let unwrap = |t: Option<Written>| -> Option<Written> {
            match t {
                Some(t) if unwraps > 0 => unwrapped(&t, unwraps),
                t => t,
            }
        };
        if is_literal(&v) || (v.starts_with('[') && group_is_whole(&v)) {
            return Some(local("()"));
        }
        for (mac, ty) in [("vec", "Vec<_>"), ("format", "String")] {
            if let Some(rest) = v.strip_prefix(mac) {
                let rest = rest.trim_start();
                if let Some(rest) = rest.strip_prefix('!') {
                    let rest = rest.trim_start();
                    if rest.starts_with(['[', '(', '{']) && group_is_whole(rest) {
                        return Some(local(ty));
                    }
                }
            }
        }
        if v.starts_with('(') {
            if !group_is_whole(&v) {
                return None;
            }
            let inner = &v[1..v.len() - 1];
            return if split_top_level(inner, ',').len() == 1 && !inner.trim_end().ends_with(',') {
                unwrap(self.value_type(inner, at, sc, r, depth + 1))
            } else {
                Some(local(&format!("({inner})")))
            };
        }
        let call = trailing_call(&v);
        if let Some((callee, args)) = &call {
            if !callee.contains('.') && callee.chars().all(|c| c.is_ascii_alphanumeric() || "_:<>, \t\n".contains(c)) && callee.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                let mut callee: String = callee.chars().filter(|c| !c.is_whitespace()).collect();
                if let Some(i) = callee.rfind("::<") {
                    if callee.ends_with('>') && !callee[i..].contains(['(', ')']) {
                        callee.truncate(i);
                    }
                }
                let segments: Vec<String> = callee.split("::").map(str::to_string).collect();
                if !segments.iter().all(|s| is_ident(s)) {
                    return None;
                }
                let last = segments.last().unwrap().clone();
                if let Some(w) = box_like(&callee) {
                    if split_top_level(args, ',').len() == 1 {
                        let held = self.value_type(args, at, sc, r, depth + 1)?;
                        if unwraps != 0 {
                            return None;
                        }
                        let mut held = held;
                        held.wrappers.insert(0, w.to_string());
                        return Some(held);
                    }
                }
                let upper = |s: &str| s.starts_with(|c: char| c.is_ascii_uppercase());
                if upper(&last) {
                    if segments.len() == 1 && last == "Some" {
                        return unwrap(Some(local("Option<_>")));
                    }
                    if segments.len() == 1 && (last == "Ok" || last == "Err") {
                        return unwrap(Some(local("Result<_, _>")));
                    }
                    if is_screaming(&last) {
                        return None;
                    }
                    if segments.len() >= 2 && upper(&segments[segments.len() - 2]) {
                        return unwrap(Some(local(&segments[..segments.len() - 1].join("::"))));
                    }
                    return unwrap(Some(local(&callee)));
                }
                if segments.len() >= 2 && upper(&segments[segments.len() - 2]) {
                    return self.associated_return(&segments[..segments.len() - 1], &last, unwraps, at, r);
                }
                return self.function_return(&segments[..segments.len() - 1], &last, unwraps, at, r);
            }
        }
        if let Some((callee, args)) = &call {
            // `x.clone()` is a value of `x`'s type.
            if args.trim().is_empty() {
                if let Some(x) = strip_suffix_ws(callee, "clone").and_then(|s| strip_suffix_ws(s, ".")) {
                    if is_ident(x.trim()) && x.trim().starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
                        return unwrap(self.local_type(x.trim(), at, sc, r, depth));
                    }
                }
            }
            // `x.m(…)`, `self.m(…)`: what the method it reaches returns.
            let (recv, method, turbofish) = split_method_callee(callee)?;
            return self.method_return(&recv, &method, turbofish.as_deref(), unwraps, awaits, at, sc, r, depth);
        }
        // `self.items`, `opts.config.port`, `pair.0`: the field's type as its struct declares it.
        if !v.starts_with(|c: char| c.is_ascii_digit()) {
            if let Some((recv, field)) = split_field_access(&v) {
                return unwrap(self.field_value(&recv, &field, at, sc, r, depth));
            }
        }
        if let Some(lit) = struct_literal_path(&v) {
            if !group_is_whole(&v[v.find('{').unwrap()..]) {
                return None;
            }
            let segments: Vec<&str> = lit.split("::").collect();
            let upper = |s: &str| s.starts_with(|c: char| c.is_ascii_uppercase());
            let ty = if segments.len() >= 2 && upper(segments[segments.len() - 2]) {
                segments[..segments.len() - 1].join("::")
            } else {
                segments.join("::")
            };
            return unwrap(Some(local(&ty)));
        }
        let segments: Vec<String> = v.split("::").map(|s| s.trim().to_string()).collect();
        if !segments.is_empty() && segments.iter().all(|s| is_ident(s)) {
            let last = segments.last().unwrap();
            if segments.len() == 1 && last.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
                return unwrap(self.local_type(last, at, sc, r, depth));
            }
            if !last.starts_with(|c: char| c.is_ascii_uppercase()) || is_screaming(last) {
                return None;
            }
            if segments.len() == 1 && last == "None" {
                return Some(local("Option<_>"));
            }
            if segments.len() >= 2 && segments[segments.len() - 2].starts_with(|c: char| c.is_ascii_uppercase()) {
                return Some(local(&segments[..segments.len() - 1].join("::")));
            }
            return unwrap(Some(local(&segments.join("::"))));
        }
        None
    }

    /// Where a function's return type is read from: its parameter list.
    pub fn signature_offset(&self, f: SymId) -> usize {
        let start = self.node_offset(f);
        let file = self.syms.get(f).file.clone();
        let Some(sc) = self.scopes(&file) else { return start };
        sc.fns.iter().find(|x| x.at >= start && x.at <= start + 512).map_or(start, |x| x.from + 1)
    }

    /// What `Type::f(…)` returns, read from the signature of the type's `f`.
    fn associated_return(&self, owner: &[String], f: &str, unwraps: usize, at: usize, r: &Ref<'_>) -> Option<Written> {
        let written = Written::new(owner.join("::"), r.file, at);
        let read = self.read_type(&written, r);
        let constructs = matches!(f, "new" | "default" | "from" | "open" | "create" | "connect" | "bind" | "empty")
            || ["new_", "with_", "from_"].iter().any(|p| f.starts_with(p) && f.len() > p.len());
        let (name, path, rw) = match &read {
            Read::Outside { .. } => return constructs.then_some(written),
            Read::Named { name, path, written: w, .. } => (name.clone(), path.clone(), w.clone()),
            _ => return None,
        };
        let lookup = self.type_lookup(&name, &path, &rw.file, Some(rw.at));
        if lookup.outside {
            return constructs.then_some(written);
        }
        let fns = self.owned_methods(&lookup.name, f, &lookup);
        if fns.is_empty() {
            return ((f == "new" || f == "default") && unwraps == 0).then_some(written);
        }
        let returns: HashSet<String> = fns.iter().map(|&x| return_text(self.syms.get(x).signature.as_deref()).unwrap_or_default()).collect();
        if returns.len() != 1 {
            return None;
        }
        let text = returns.into_iter().next().unwrap();
        if text.is_empty() {
            return None;
        }
        let made = Written::new(text, &self.syms.get(fns[0]).file, self.signature_offset(fns[0]));
        let value = if unwraps > 0 { unwrapped(&made, unwraps) } else { Some(made) };
        if let Some(v) = &value {
            if v.text.trim() == "Self" || head_name(&v.text) == name {
                return Some(written);
            }
        }
        let files: HashSet<&str> = fns.iter().map(|&x| self.syms.get(x).file.as_str()).collect();
        if files.len() > 1 {
            None
        } else {
            value
        }
    }

    /// What a free function `f(…)` or `module::f(…)` returns.
    fn function_return(&self, modules: &[String], f: &str, unwraps: usize, at: usize, r: &Ref<'_>) -> Option<Written> {
        let mut fns: Vec<SymId> = self.rust_named(f).filter(|&i| self.syms.get(i).kind == NodeKind::Function).collect();
        let same: Vec<SymId> = fns.iter().copied().filter(|&i| self.syms.get(i).file == r.file).collect();
        if !same.is_empty() && modules.is_empty() {
            fns = same;
        } else {
            let path: Vec<String> = if modules.is_empty() {
                self.use_path(f, r.file, Some(at))?
            } else {
                let head = self.use_path(&modules[0], r.file, Some(at)).filter(|h| !h.is_empty());
                let mut p = head.unwrap_or_else(|| vec![modules[0].clone()]);
                p.extend(modules[1..].iter().cloned());
                p.push(f.to_string());
                p
            };
            let walked = self.walked_items(&path, r.file, Some(at));
            let named: Vec<SymId> = match &walked {
                Some(w) => fns.iter().copied().filter(|n| w.contains(n)).collect(),
                None => Vec::new(),
            };
            if !named.is_empty() {
                fns = named;
            } else {
                let g = self.graph();
                let segs: Vec<String> = path[..path.len() - 1]
                    .iter()
                    .filter(|s| !matches!(s.as_str(), "crate" | "self" | "super") && !g.project.contains(*s))
                    .cloned()
                    .collect();
                if !segs.is_empty() {
                    fns.retain(|&n| modules_lead(&segs, &self.syms.get(n).file));
                }
            }
        }
        let returns: HashSet<String> = fns.iter().map(|&x| return_text(self.syms.get(x).signature.as_deref()).unwrap_or_default()).collect();
        if fns.is_empty() || returns.len() != 1 {
            return None;
        }
        let text = returns.into_iter().next().unwrap();
        if text.is_empty() {
            return None;
        }
        let made = Written::new(text, &self.syms.get(fns[0]).file, self.signature_offset(fns[0]));
        if unwraps > 0 {
            unwrapped(&made, unwraps)
        } else {
            Some(made)
        }
    }

    /// What a method call's value is, read from the signature of the method the call reaches.
    #[allow(clippy::too_many_arguments)]
    fn method_return(
        &self,
        receiver: &str,
        method: &str,
        turbofish: Option<&str>,
        unwraps: usize,
        awaits: usize,
        at: usize,
        sc: &Scopes,
        r: &Ref<'_>,
        depth: u8,
    ) -> Option<Written> {
        let recv = receiver.trim();
        let written = if recv == "self" {
            Some(Written::new("Self", r.file, at))
        } else if is_ident(recv) && recv.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
            self.local_type(recv, at, sc, r, depth)
        } else if recv.ends_with([')', '?']) || split_field_access(recv).is_some() {
            self.value_type(recv, at, sc, r, depth + 1)
        } else {
            return None;
        }?;
        let read = self.read_type(&written, r);
        let Hit::Found(m) = self.typed_method_call(&read, method, r, 0) else { return None };
        let target = m.target;
        let text = return_text(self.syms.get(target).signature.as_deref())?;
        let mut made = Written::new(text, &self.syms.get(target).file, self.signature_offset(target));
        made = self.return_instance(made, target, turbofish, &read, at, r);
        let is_async = self.is_async_fn(target);
        if awaits > 1 || (awaits == 0 && is_async) {
            return None;
        }
        let made = if awaits == 1 && !is_async { future_output(&made)? } else { made };
        if unwraps > 0 {
            unwrapped(&made, unwraps)
        } else {
            Some(made)
        }
    }

    /// A method's return type as the call makes it, where it names a generic or `Self`.
    fn return_instance(&self, made: Written, target: SymId, turbofish: Option<&str>, read: &Read, at: usize, r: &Ref<'_>) -> Written {
        let names: Vec<String> = idents_of(&made.text)
            .into_iter()
            .filter(|n| !matches!(n.as_str(), "mut" | "dyn" | "impl" | "const" | "for"))
            .collect();
        let generics: Vec<String> = names.iter().filter(|n| *n != "Self" && self.generic_bounds(n, &made).is_some()).cloned().collect();
        if generics.is_empty() {
            let bare = strip_amp(&made.text).trim();
            if bare == "Self" {
                if let Read::Named { name, path, args, written, .. } = read {
                    if self.trait_of(target).is_some() {
                        let mut ty = path.clone();
                        ty.push(name.clone());
                        let mut ty = ty.join("::");
                        if !args.is_empty() {
                            ty = format!("{ty}<{args}>");
                        }
                        let text = made.text.trim_end().strip_suffix("Self").map_or(made.text.clone(), |p| format!("{p}{ty}"));
                        return Written::new(text, &written.file, written.at);
                    }
                }
            }
            return made;
        }
        let mut unique: Vec<String> = Vec::new();
        for g in &generics {
            if !unique.contains(g) {
                unique.push(g.clone());
            }
        }
        let opaque = Written {
            opaque: unique.clone(),
            ..made.clone()
        };
        let Some(turbofish) = turbofish else { return opaque };
        let Some(sc) = self.scopes(&made.file) else { return opaque };
        let params: Vec<String> = sc
            .fn_at(made.at)
            .map(|f| {
                split_top_level(&f.generics, ',')
                    .iter()
                    .filter_map(|p| {
                        let p = p.trim_start().strip_prefix("const ").unwrap_or(p.trim_start());
                        let id: String = p.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
                        (!id.is_empty()).then_some(id)
                    })
                    .collect()
            })
            .unwrap_or_default();
        let args: Vec<String> = split_top_level(turbofish, ',').into_iter().filter(|a| !a.starts_with('\'')).collect();
        let mut bound: HashMap<String, String> = HashMap::new();
        for g in &unique {
            let Some(i) = params.iter().position(|p| p == g) else { return opaque };
            if i >= args.len() || args[i] == "_" {
                return opaque;
            }
            bound.insert(g.clone(), args[i].clone());
        }
        if names.iter().any(|n| {
            !bound.contains_key(n)
                && (n == "Self" || !(STD_TYPES.contains(&n.as_str()) || PRELUDE.contains(&n.as_str()) || PRIMITIVE_TYPES.contains(&n.as_str())))
        }) {
            return opaque;
        }
        let text = replace_idents(&made.text, &bound);
        Written::new(text, r.file, at)
    }

    /// Whether a Rust function is declared `async`, read off its source.
    pub fn is_async_fn(&self, f: SymId) -> bool {
        let s = self.syms.get(f);
        let Some(sc) = self.scopes(&s.file) else { return false };
        let start = self.node_offset(f);
        let end = (start + 300).min(sc.code.len());
        let head = &sc.code[start..floor_char(&sc.code, end)];
        let name = s.name.trim_start_matches("r#");
        let mut from = 0;
        while let Some(i) = super::uses::find_word(&head[from..], "fn").map(|i| from + i) {
            from = i + 2;
            let rest = head[i + 2..].trim_start().trim_start_matches("r#");
            if rest.starts_with(name) && !rest[name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                return super::uses::find_word(&head[..i], "async").is_some();
            }
        }
        false
    }

    /// What a Rust field access is, as the receiver's struct declares the field.
    fn field_value(&self, receiver: &str, field: &str, at: usize, sc: &Scopes, r: &Ref<'_>, depth: u8) -> Option<Written> {
        let recv = receiver.trim();
        let written = if recv == "self" {
            Written::new("Self", r.file, at)
        } else if is_ident(recv) && recv.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
            self.local_type(recv, at, sc, r, depth)?
        } else {
            self.value_type(recv, at, sc, r, depth + 1)?
        };
        let read = self.read_type(&written, r);
        let is_index = field.chars().all(|c| c.is_ascii_digit());
        if let Read::Outside { name, .. } = &read {
            if name == "(" && is_index {
                return tuple_field(&written, field.parse().ok()?);
            }
        }
        let Read::Named { name, path, written: rw, .. } = &read else { return None };
        let lookup = self.type_lookup(name, path, &rw.file, Some(rw.at));
        let structs: Vec<SymId> = if lookup.outside {
            Vec::new()
        } else {
            lookup.nodes.iter().copied().filter(|&n| matches!(self.syms.get(n).kind, NodeKind::Struct | NodeKind::Union)).collect()
        };
        if structs.len() != 1 {
            return None;
        }
        let decl = structs[0];
        let fields = self.declared_fields(decl, false)?;
        let text = if is_index {
            fields.tuple.as_ref()?.get(field.parse::<usize>().ok()?)?.clone()
        } else {
            fields.named.as_ref()?.get(field)?.clone()
        };
        let d = self.syms.get(decl);
        let mut value = Written {
            text,
            file: d.file.clone(),
            at: self.node_offset(decl),
            self_: Some(d.name.clone()),
            generics: Some(fields.generics.clone()),
            ..Default::default()
        };
        if recv == "self" {
            if let Some(b) = self_param_bounds(&fields.generics, sc, at, r.file) {
                value.bounds = Some(Rc::new(b));
            }
        }
        Some(value)
    }

    /// What a Rust `for` loop binds, an item of what it iterates.
    fn iterated_element(&self, iter: &str, at: usize, sc: &Scopes, r: &Ref<'_>, depth: u8) -> Option<Written> {
        let mut v = iter.trim().to_string();
        let mut steps: Vec<String> = Vec::new();
        for _ in 0..8 {
            if let Some(value) = self.value_type(strip_amp(&v), at, sc, r, depth + 1) {
                steps.reverse();
                return self.step_items(&value, &steps);
            }
            let (callee, _) = trailing_call(&v)?;
            let (recv, m, _) = split_method_callee(&callee)?;
            if !(ITEM_ADAPTERS.contains(&m.as_str()) || item_projection(&m).is_some()) {
                return None;
            }
            steps.push(m);
            v = recv;
        }
        None
    }

    fn step_items(&self, value: &Written, steps: &[String]) -> Option<Written> {
        let mut projection: Option<&str> = None;
        let mut numbered = false;
        for s in steps {
            match item_projection(s) {
                Some("enumerate") => {
                    if numbered {
                        return None;
                    }
                    numbered = true;
                }
                Some(p) => {
                    if numbered || projection.is_some() {
                        return None;
                    }
                    projection = Some(p);
                }
                None => {}
            }
        }
        let item = self.item_of(value, projection)?;
        Some(if numbered { item.with_text(format!("(usize, {})", item.text)) } else { item })
    }

    /// The item type a value of a Rust type hands out when iterated.
    fn item_of(&self, w: &Written, projection: Option<&str>) -> Option<Written> {
        let mut t = w.text.trim().to_string();
        for _ in 0..6 {
            let before = t.clone();
            t = strip_amp(&t).trim().to_string();
            if let Some((name, open)) = wrapper_head(&t) {
                if ["Box", "Rc", "Arc", "Cow", "Ref", "RefMut", "MutexGuard", "RwLockReadGuard", "RwLockWriteGuard"].contains(&name.as_str())
                    && angle_end(&t, open) == Some(t.len() - 1)
                {
                    t = strip_lifetime_arg(t[open + 1..t.len() - 1].trim()).trim().to_string();
                }
            }
            if t == before {
                break;
            }
        }
        let as_ = |text: Option<String>| -> Option<Written> {
            text.map(|tx| Written {
                text: tx.trim().to_string(),
                wrappers: Vec::new(),
                ..w.clone()
            })
        };
        let type_args = |args: &str| -> Vec<String> { split_top_level(args, ',').into_iter().filter(|a| !a.starts_with('\'')).collect() };
        let std = |name: &str| !self.is_project_named(name, DECLARED_TYPE_KINDS);
        if t.starts_with('[') {
            if !group_is_whole(&t) || projection.is_some() {
                return None;
            }
            return as_(split_top_level(&t[1..t.len() - 1], ';').into_iter().next());
        }
        for kw in ["impl", "dyn"] {
            if let Some(rest) = t.strip_prefix(kw).filter(|r| r.starts_with(char::is_whitespace)) {
                if projection.is_some() {
                    return None;
                }
                for bound in split_top_level(rest.trim(), '+') {
                    let Some((_, name, args)) = path_head(&bound) else { continue };
                    if !ITERATOR_TRAITS.contains(&name.as_str()) {
                        continue;
                    }
                    return as_(type_args(&args).iter().find_map(|a| {
                        let a = a.trim();
                        a.strip_prefix("Item").map(str::trim_start).and_then(|r| r.strip_prefix('=')).map(|r| r.trim().to_string())
                    }));
                }
                return None;
            }
        }
        let (_, name, args) = path_head(&t)?;
        if !std(&name) {
            return None;
        }
        let args = type_args(&args);
        if STD_SEQUENCES.contains(&name.as_str()) {
            return if projection.is_some() || args.len() != 1 { None } else { as_(args.into_iter().next()) };
        }
        if STD_MAPS.contains(&name.as_str()) {
            if args.len() < 2 {
                return None;
            }
            return match projection {
                Some("key") => as_(Some(args[0].clone())),
                Some("value") => as_(Some(args[1].clone())),
                _ => as_(Some(format!("({}, {})", args[0], args[1]))),
            };
        }
        let iterator = std_iterator(&name)?;
        if iterator == "key" || iterator == "value" {
            return if projection.is_some() || args.len() != 2 {
                None
            } else {
                as_(Some(args[if iterator == "key" { 0 } else { 1 }].clone()))
            };
        }
        match args.len() {
            1 => {
                if projection.is_some() {
                    None
                } else {
                    as_(Some(args[0].clone()))
                }
            }
            2 => match projection {
                Some("key") => as_(Some(args[0].clone())),
                Some("value") => as_(Some(args[1].clone())),
                _ => as_(Some(format!("({}, {})", args[0], args[1]))),
            },
            _ => None,
        }
    }

    /// The type a name a pattern binds is.
    fn site_type(&self, site: &PatternSite, scrutinee: Option<&str>, at: usize, sc: &Scopes, r: &Ref<'_>, depth: u8) -> Option<Written> {
        if site.path.is_empty() {
            let index = site.index?;
            let value = self.value_type(scrutinee?, at, sc, r, depth + 1)?;
            return tuple_field(&value, index);
        }
        let segments: Vec<String> = site.path.split("::").map(str::to_string).collect();
        let last = segments.last().unwrap().as_str();
        if segments.len() == 1 && matches!(last, "Some" | "Ok" | "Err") {
            if site.index != Some(0) {
                return None;
            }
            let value = self.value_type(scrutinee?, at, sc, r, depth + 1)?;
            let (_, name, args) = path_head(strip_amp(&value.text))?;
            let args = split_top_level(&args, ',');
            return match (last, name.as_str()) {
                ("Some", "Option") if args.len() == 1 => Some(value.with_text(args[0].clone())),
                ("Ok", "Result") if !args.is_empty() => Some(value.with_text(args[0].clone())),
                ("Err", "Result") if args.len() == 2 => Some(value.with_text(args[1].clone())),
                _ => None,
            };
        }
        let (fields, file, decl_at, self_, generics) = self.pattern_declaration(&segments, at, r)?;
        let text = match (site.index, &site.field) {
            (Some(i), _) => fields.tuple.as_ref()?.get(i)?.clone(),
            (None, Some(f)) => fields.named.as_ref()?.get(f)?.clone(),
            _ => return None,
        };
        Some(Written {
            text,
            file,
            at: decl_at,
            self_: Some(self_),
            generics: Some(generics),
            ..Default::default()
        })
    }

    /// The declaration a tuple-struct, variant or struct pattern's path names, with its fields' types.
    fn pattern_declaration(&self, segments: &[String], at: usize, r: &Ref<'_>) -> Option<(Rc<Fields>, String, usize, String, String)> {
        let last = segments.last()?;
        if segments.len() >= 2 {
            let owner = self.read_type(&Written::new(segments[..segments.len() - 1].join("::"), r.file, at), r);
            let Read::Named { name, path, written, .. } = owner else { return None };
            let enums: Vec<SymId> = self
                .type_lookup(&name, &path, &written.file, Some(written.at))
                .nodes
                .into_iter()
                .filter(|&n| self.syms.get(n).kind == NodeKind::Enum)
                .collect();
            if enums.len() != 1 {
                return None;
            }
            let e = self.syms.get(enums[0]);
            let q = format!("{}::{last}", e.qualified_name);
            let variant = self.syms.in_file(&e.file).iter().copied().find(|&i| {
                let s = self.syms.get(i);
                s.kind == NodeKind::EnumMember && s.qualified_name == q
            })?;
            let fields = self.declared_fields(variant, false)?;
            let generics = self.declared_fields(enums[0], true).map(|f| f.generics.clone()).unwrap_or_default();
            return Some((fields, self.syms.get(variant).file.clone(), self.node_offset(variant), e.name.clone(), generics));
        }
        let structs: Vec<SymId> = self
            .type_lookup(last, &[], r.file, Some(at))
            .nodes
            .into_iter()
            .filter(|&n| self.syms.get(n).kind == NodeKind::Struct)
            .collect();
        if structs.len() != 1 {
            return None;
        }
        let fields = self.declared_fields(structs[0], false)?;
        let s = self.syms.get(structs[0]);
        let g = fields.generics.clone();
        Some((fields, s.file.clone(), self.node_offset(structs[0]), s.name.clone(), g))
    }

    /// The fields a struct, an enum's variant (or, for its generics, an enum) declares.
    pub fn declared_fields(&self, decl: SymId, generics_only: bool) -> Option<Rc<Fields>> {
        self.memo(21, format!("{decl}\0{generics_only}"), || self.read_declared_fields(decl, generics_only).map(Rc::new))
    }

    fn read_declared_fields(&self, decl: SymId, generics_only: bool) -> Option<Fields> {
        let d = self.syms.get(decl);
        let sc = self.scopes(&d.file)?;
        let code = sc.code.as_str();
        let start = self.node_offset(decl);
        let window = &code[start..floor_char(code, (start + 512).min(code.len()))];
        let name_at = super::uses::find_word(window, &d.name)?;
        let b = code.as_bytes();
        let mut p = super::scopes::skip_space(b, start + name_at + d.name.len());
        let mut generics = String::new();
        if b.get(p) == Some(&b'<') {
            let end = angle_end(&code[p..floor_char(code, (p + 2000).min(code.len()))], 0)?;
            generics = code[p + 1..p + end].to_string();
            p += end + 1;
        }
        if generics_only {
            return Some(Fields {
                generics,
                ..Default::default()
            });
        }
        let mut open = p;
        while open < b.len() && !matches!(b[open], b'(' | b'{' | b';' | b',' | b'}') {
            open += 1;
        }
        let bracket = b.get(open).copied();
        if !matches!(bracket, Some(b'(') | Some(b'{')) {
            return Some(Fields {
                generics,
                ..Default::default()
            });
        }
        let close = sc.close.get(&open).copied().unwrap_or(b.len());
        let field = |text: &str| -> String {
            let t = strip_attributes(text);
            let t = t.trim_start();
            let t = match t.strip_prefix("pub") {
                Some(r) => {
                    let r = r.trim_start();
                    if r.starts_with('(') {
                        r.find(')').map_or(r, |e| &r[e + 1..])
                    } else {
                        r
                    }
                }
                None => t,
            };
            t.trim().to_string()
        };
        let parts: Vec<String> = split_top_level(&code[open + 1..close], ',').iter().map(|p| field(p)).collect();
        if bracket == Some(b'(') {
            return Some(Fields {
                tuple: Some(parts),
                generics,
                ..Default::default()
            });
        }
        let mut named = HashMap::new();
        for part in parts {
            if let Some((n, t)) = part.split_once(':') {
                let n = n.trim();
                if is_ident(n) && !t.starts_with(':') {
                    named.insert(n.to_string(), t.trim().to_string());
                }
            }
        }
        Some(Fields {
            named: Some(named),
            generics,
            ..Default::default()
        })
    }

    /// `Name::method` methods the index holds, of any type of that name.
    pub fn methods_named(&self, name: &str, method: &str) -> Vec<SymId> {
        let want = format!("{name}::{method}");
        let suffix = format!("::{want}");
        self.rust_named(method)
            .filter(|&i| {
                let s = self.syms.get(i);
                s.kind == NodeKind::Method && (s.qualified_name == want || s.qualified_name.ends_with(&suffix))
            })
            .collect()
    }

    /// The `Name::method` methods of the types a lookup found: a namesake type's methods are its own.
    pub fn owned_methods(&self, name: &str, method: &str, lookup: &Lookup) -> Vec<SymId> {
        let matches: Vec<SymId> = self.methods_named(name, method).into_iter().filter(|&m| self.trait_of(m).is_none()).collect();
        let types: Vec<SymId> = lookup.nodes.iter().copied().filter(|&n| OWNER_KINDS.contains(&self.syms.get(n).kind)).collect();
        if types.is_empty() {
            return matches;
        }
        let namesakes = self.rust_named(name).filter(|&n| OWNER_KINDS.contains(&self.syms.get(n).kind)).count();
        if namesakes <= types.len() {
            return matches;
        }
        let ids: HashSet<SymId> = types.iter().copied().collect();
        let files: HashSet<&str> = types.iter().map(|&t| self.syms.get(t).file.as_str()).collect();
        matches
            .into_iter()
            .filter(|&m| {
                let mf = &self.syms.get(m).file;
                files.contains(mf.as_str())
                    || self.type_lookup(name, &[], mf, Some(self.node_offset(m))).nodes.iter().any(|n| ids.contains(n))
            })
            .collect()
    }

    /// The text an `impl` header above a Rust method gives its implementing type's arguments.
    fn impl_self_args(&self, method: SymId) -> Option<(String, String, bool)> {
        let s = self.syms.get(method);
        let sc = self.scopes(&s.file)?;
        let blk = sc.impl_at(self.node_offset(method))?;
        if blk.kind != super::scopes::BlockKind::Impl {
            return None;
        }
        let header = &sc.code[blk.at..blk.from];
        let header = super::uses::find_word(header, "where").map_or(header, |w| &header[..w]);
        let mut head = header.trim_start().strip_prefix("impl")?.trim_start();
        if head.starts_with('<') {
            let e = angle_end(head, 0)?;
            head = &head[e + 1..];
        }
        let self_ = match super::uses::find_word(head, "for") {
            Some(i) => &head[i + 3..],
            None => head,
        };
        let (_, _, args) = path_head(strip_amp(self_.trim()))?;
        Some((args, blk.generics.clone(), self_.trim().starts_with('&')))
    }

    /// Among methods of one name in several impls of a type, those whose impl is for the receiver's arguments.
    fn specialized(&self, methods: Vec<SymId>, args: &str) -> Vec<SymId> {
        if methods.len() < 2 || args.is_empty() {
            return methods;
        }
        let norm = |text: &str| -> Vec<String> {
            split_top_level(text, ',')
                .iter()
                .map(|a| {
                    let s: String = a.chars().filter(|c| !c.is_whitespace()).collect();
                    let mut t = s.as_str();
                    while let Some(i) = t.find("::") {
                        if t[..i].chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                            t = &t[i + 2..];
                        } else {
                            break;
                        }
                    }
                    t.to_string()
                })
                .collect()
        };
        let want = norm(args);
        let mut exact = Vec::new();
        let mut generic = Vec::new();
        for &m in &methods {
            let Some((a, g, _)) = self.impl_self_args(m) else {
                generic.push(m);
                continue;
            };
            let have = norm(&a);
            if have.len() != want.len() {
                continue;
            }
            if have.iter().zip(&want).all(|(x, y)| x == y) {
                exact.push(m);
            } else if have
                .iter()
                .zip(&want)
                .all(|(x, y)| x == y || bounds_of(x, &[Generics { generics: &g, where_: "" }]).is_some())
            {
                generic.push(m);
            }
        }
        if !exact.is_empty() {
            exact
        } else if !generic.is_empty() {
            generic
        } else {
            methods
        }
    }

    /// The method among `methods` a call reaches: its own file's, the type's file's, then the nearest.
    pub fn pick_method(&self, methods: &[SymId], type_files: &HashSet<String>, r: &Ref<'_>, confidence: f32) -> Hit {
        let chosen = methods
            .iter()
            .copied()
            .find(|&m| self.syms.get(m).file == r.file)
            .or_else(|| methods.iter().copied().find(|&m| type_files.contains(&self.syms.get(m).file)))
            .or_else(|| {
                let mut best = *methods.first()?;
                for &m in &methods[1..] {
                    if proximity(r.file, &self.syms.get(m).file) > proximity(r.file, &self.syms.get(best).file) {
                        best = m;
                    }
                }
                Some(best)
            });
        match chosen {
            Some(c) => Hit::Found(instance(c, confidence)),
            None => Hit::Nothing,
        }
    }

    /// Whether the file of `r` has the project trait `trait_` in scope.
    pub fn trait_in_scope(&self, trait_: &str, r: &Ref<'_>, target: SymId) -> bool {
        if r.file == self.syms.get(target).file {
            return true;
        }
        let traits: Vec<SymId> = self.rust_named(trait_).filter(|&n| self.syms.get(n).kind == NodeKind::Trait).collect();
        if traits.iter().any(|&t| self.syms.get(t).file == r.file) {
            return true;
        }
        let uses = self.uses(r.file);
        if uses.names.contains(trait_) {
            return true;
        }
        let g = self.graph();
        if uses.globs.iter().any(|x| x == "prelude" || x == "crate" || g.project.contains(x)) {
            return true;
        }
        traits.iter().any(|&t| super::uses::glob_covers(&uses, &self.syms.get(t).file, r.file))
    }

    fn trait_callable(&self, trait_: &str, r: &Ref<'_>, sample: SymId) -> bool {
        !self.is_project_named(trait_, &[NodeKind::Trait]) || self.trait_in_scope(trait_, r, sample)
    }

    /// Whether the trait `trait_` is implemented for the type `name` (or by a blanket impl the value meets).
    fn trait_implemented(
        &self,
        trait_: &str,
        trait_file: Option<&str>,
        name: Option<&str>,
        type_files: &HashSet<String>,
        receiver_traits: &[String],
        depth: u8,
    ) -> bool {
        if !is_ident(trait_) || name.is_some_and(|n| !is_ident(n)) {
            return false;
        }
        let mut files: Vec<String> = trait_file.map(str::to_string).into_iter().collect();
        let mut tf: Vec<&String> = type_files.iter().collect();
        tf.sort();
        for f in tf {
            if !files.contains(f) {
                files.push(f.clone());
            }
        }
        let key = format!("{trait_}\0{trait_file:?}\0{name:?}\0{}\0{depth}\0{}", receiver_traits.join(","), files.join("\0"));
        {
            let memo = self.memo.borrow();
            if let Some(Some(v)) = memo.get(&(22, key.clone())) {
                if let Some(v) = v.downcast_ref::<bool>() {
                    return *v;
                }
            }
        }
        self.memo.borrow_mut().insert((22, key.clone()), Some(Rc::new(false)));
        let meets = |bound: &str| -> bool {
            let head = head_name(bound);
            if MARKER_TRAITS.contains(&head.as_str()) || receiver_traits.iter().any(|t| head_name(t) == head) {
                return true;
            }
            let Some(n) = name else { return false };
            if depth >= 2 {
                return false;
            }
            let declared = self.rust_named(&head).find(|&i| self.syms.get(i).kind == NodeKind::Trait);
            let df = declared.map(|d| self.syms.get(d).file.clone());
            self.trait_implemented(&head, df.as_deref(), Some(n), type_files, receiver_traits, depth + 1)
        };
        let mut found = false;
        'files: for file in &files {
            let Some(sc) = self.scopes(file) else { continue };
            let code = sc.code.as_str();
            if !code.contains(trait_) {
                continue;
            }
            for (impl_generics, impl_where, self_written) in impl_headers_for(code, trait_) {
                if let Some(n) = name {
                    if head_name(&self_written) == n || self_written.trim() == n {
                        found = true;
                        break 'files;
                    }
                }
                // A blanket impl over one of its own parameters.
                let self_name = self_written.trim();
                if !self_name.starts_with(|c: char| c.is_ascii_uppercase()) || !is_ident(self_name) {
                    continue;
                }
                let Some(g) = impl_generics else { continue };
                if let Some(bounds) = bounds_of(self_name, &[Generics { generics: &g, where_: &impl_where }]) {
                    if bounds.iter().all(|b| meets(b)) {
                        found = true;
                        break 'files;
                    }
                }
            }
        }
        self.memo.borrow_mut().insert((22, key), Some(Rc::new(found)));
        found
    }

    /// The method a call reaches as one a project trait provides.
    fn provided_method(&self, name: Option<&str>, type_files: &HashSet<String>, receiver_traits: &[String], method: &str, r: &Ref<'_>) -> Option<Hit> {
        let declared: Vec<SymId> = self
            .rust_named(method)
            .filter(|&m| self.syms.get(m).kind == NodeKind::Method && self.trait_of(m).is_some() && self.has_body(m))
            .collect();
        if declared.is_empty() {
            return None;
        }
        let reached: Vec<SymId> = declared
            .into_iter()
            .filter(|&m| {
                let t = self.trait_of(m).unwrap();
                let ts = self.syms.get(t);
                self.trait_implemented(&ts.name, Some(&ts.file), name, type_files, receiver_traits, 0) && self.trait_in_scope(&ts.name, r, m)
            })
            .collect();
        let traits: HashSet<SymId> = reached.iter().filter_map(|&m| self.trait_of(m)).collect();
        match traits.len() {
            1 => Some(Hit::Found(instance(reached[0], 0.85))),
            0 => None,
            _ => Some(Hit::Nothing),
        }
    }

    /// The inherent methods named `method` that impls of aliases of the type `name` declare.
    fn alias_impl_methods(&self, name: &str, method: &str) -> Vec<SymId> {
        let mut alias_of: HashMap<String, bool> = HashMap::new();
        self.rust_named(method)
            .filter(|&m| {
                let s = self.syms.get(m);
                if s.kind != NodeKind::Method || self.trait_of(m).is_some() || self.is_trait_impl_method(m) {
                    return false;
                }
                let Some(owner) = s.owner().and_then(|o| o.rsplit("::").next()) else { return false };
                *alias_of.entry(owner.to_string()).or_insert_with(|| {
                    owner != name
                        && self.rust_named(owner).any(|n| {
                            self.syms.get(n).kind == NodeKind::TypeAlias
                                && self.alias_target(n).is_some_and(|t| head_name(&t.text) == name)
                        })
                })
            })
            .collect()
    }

    /// Whether a trait's method declaration has a body.
    pub fn has_body(&self, m: SymId) -> bool {
        let s = self.syms.get(m);
        let Some(sc) = self.scopes(&s.file) else { return false };
        let start = self.node_offset(m);
        sc.fns.iter().any(|f| f.at >= start && sc.line_at(f.at) <= s.end_line.max(s.start_line))
    }

    /// What `impl Deref for Name { type Target = … }` makes a value of `name` hand calls on to.
    fn deref_target(&self, name: &str, type_files: &HashSet<String>) -> Option<Written> {
        for file in type_files {
            let Some(sc) = self.scopes(file) else { continue };
            let code = sc.code.as_str();
            if !code.contains("Deref") {
                continue;
            }
            for (_, _, self_written) in impl_headers_for(code, "Deref") {
                if head_name(&self_written) != name {
                    continue;
                }
                // The body after this header: find `type Target = …;`.
                if let Some(pos) = code.find(&format!("Deref for {self_written}")).or_else(|| code.find("Deref for")) {
                    let body = code[pos..].find('{').map(|b| pos + b + 1)?;
                    let window = &code[body..floor_char(code, (body + 600).min(code.len()))];
                    if let Some(t) = window.find("type Target") {
                        let rest = &window[t + "type Target".len()..];
                        let rest = rest.trim_start().strip_prefix('=')?;
                        let end = rest.find(';')?;
                        return Some(Written::new(rest[..end].trim(), file, body + t));
                    }
                }
            }
        }
        None
    }

    /// What a `type Name<…> = Target;` alias names, as its declaration writes it.
    pub fn alias_target(&self, alias: SymId) -> Option<Written> {
        let s = self.syms.get(alias);
        let sc = self.scopes(&s.file)?;
        let start = self.node_offset(alias);
        let code = sc.code.as_str();
        let from = floor_char(code, start.saturating_sub(64));
        let window = &code[from..floor_char(code, (start + 2000).min(code.len()))];
        let mut search = 0;
        while let Some(i) = super::uses::find_word(&window[search..], "type").map(|i| search + i) {
            search = i + 4;
            let rest = window[i + 4..].trim_start();
            if !rest.starts_with(&s.name) || rest[s.name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let mut rest = rest[s.name.len()..].trim_start();
            let mut generics = String::new();
            if rest.starts_with('<') {
                let e = angle_end(rest, 0)?;
                generics = rest[1..e].to_string();
                rest = rest[e + 1..].trim_start();
            }
            let rest = rest.strip_prefix('=')?;
            let end = rest.find(';')?;
            return Some(Written {
                text: rest[..end].trim().to_string(),
                file: s.file.clone(),
                at: start,
                generics: Some(generics),
                ..Default::default()
            });
        }
        None
    }

    /// The methods named `method` a Rust trait declares, or the traits it extends do.
    pub fn trait_methods(&self, traits: &[SymId], method: &str, depth: u8) -> Vec<SymId> {
        let mut found = Vec::new();
        for &t in traits {
            let ts = self.syms.get(t);
            let own = self.rust_named(method).find(|&m| {
                let s = self.syms.get(m);
                s.kind == NodeKind::Method && s.file == ts.file && s.start_line >= ts.start_line && s.start_line <= ts.end_line && self.trait_of(m) == Some(t)
            });
            if let Some(o) = own {
                found.push(o);
                continue;
            }
            if depth >= 4 {
                continue;
            }
            let Some(supers) = self.trait_supers(t) else { continue };
            for bound in supers {
                if bound.starts_with('\'') {
                    continue;
                }
                let Some((segs, name, _)) = path_head(&bound) else { continue };
                let sup: Vec<SymId> = self
                    .type_lookup(&name, &segs, &ts.file, Some(self.node_offset(t)))
                    .nodes
                    .into_iter()
                    .filter(|&n| self.syms.get(n).kind == NodeKind::Trait)
                    .collect();
                found.extend(self.trait_methods(&sup, method, depth + 1));
            }
        }
        found
    }

    /// The supertraits a trait's header names: `trait Name<…>: Super + Other where … {`.
    fn trait_supers(&self, t: SymId) -> Option<Vec<String>> {
        let ts = self.syms.get(t);
        let sc = self.scopes(&ts.file)?;
        let code = sc.code.as_str();
        let start = self.node_offset(t);
        let window = &code[start..floor_char(code, (start + 2000).min(code.len()))];
        let i = super::uses::find_word(window, "trait")?;
        let rest = window[i + 5..].trim_start();
        let rest = rest.strip_prefix(ts.name.as_str())?.trim_start();
        let rest = if rest.starts_with('<') { &rest[angle_end(rest, 0)? + 1..] } else { rest };
        let rest = rest.trim_start().strip_prefix(':')?;
        let end = rest.find('{')?;
        let mut header = &rest[..end];
        if let Some(w) = super::uses::find_word(header, "where") {
            header = &header[..w];
        }
        Some(split_top_level(header, '+'))
    }

    /// A call on a value known by its traits.
    pub fn traits_method_call(&self, traits: &[String], written: &Written, method: &str, r: &Ref<'_>) -> Hit {
        let mut found = Vec::new();
        let mut project_traits = 0;
        for bound in traits {
            let Some((segs, name, _)) = path_head(bound) else { continue };
            let nodes: Vec<SymId> = self
                .type_lookup(&name, &segs, &written.file, Some(written.at))
                .nodes
                .into_iter()
                .filter(|&n| self.syms.get(n).kind == NodeKind::Trait)
                .collect();
            if !nodes.is_empty() {
                project_traits += 1;
            }
            found.extend(self.trait_methods(&nodes, method, 0));
        }
        if project_traits == 0 {
            return Hit::Unread;
        }
        let owners: HashSet<Option<SymId>> = found.iter().map(|&m| self.trait_of(m)).collect();
        if owners.len() == 1 {
            return Hit::Found(instance(found[0], 0.9));
        }
        if owners.len() > 1 {
            return Hit::Nothing;
        }
        self.provided_method(None, &HashSet::new(), traits, method, r).unwrap_or(Hit::Nothing)
    }

    /// Whether a method's body calls the function of its own name on its type.
    fn delegates_to_own(&self, method: SymId, type_name: &str) -> bool {
        let s = self.syms.get(method);
        let Some(sc) = self.scopes(&s.file) else { return false };
        let start = sc.offset(s.start_line, 0);
        let end = sc.offset(s.end_line + 1, 0);
        let body = &sc.code[start..end.max(start)];
        [format!("Self::{}(", s.name), format!("{type_name}::{}(", s.name)]
            .iter()
            .any(|p| collapse_ws(body).contains(p.as_str()))
    }

    /// A call on a value of a type from outside the project: a project trait's method in scope.
    fn outside_type_call(&self, name: &str, args: &str, by_ref: bool, method: &str, r: &Ref<'_>) -> Hit {
        let for_outside = |m: SymId| -> bool {
            let written = self.trait_impl_header(m).map(|h| strip_amp(&h.self_text).to_string()).unwrap_or_default();
            let head = path_head(&written);
            let (segs, n) = head.map(|h| (h.0, h.1)).unwrap_or((Vec::new(), name.to_string()));
            let lookup = self.type_lookup(&n, &segs, &self.syms.get(m).file, Some(self.node_offset(m)));
            lookup.outside || !lookup.nodes.iter().any(|&x| OWNER_KINDS.contains(&self.syms.get(x).kind))
        };
        let candidates: Vec<SymId> = self
            .methods_named(name, method)
            .into_iter()
            .filter(|&m| {
                let Some(h) = self.trait_impl_header(m) else { return false };
                h.self_ == name
                    && self.is_project_named(&h.trait_, &[NodeKind::Trait])
                    && self.trait_in_scope(&h.trait_, r, m)
                    && !self.delegates_to_own(m, name)
                    && for_outside(m)
            })
            .collect();
        let impls = self.specialized(candidates, args);
        if impls.len() == 1 {
            return Hit::Found(instance(impls[0], 0.85));
        }
        if impls.len() > 1 {
            let matching: Vec<SymId> = impls.iter().copied().filter(|&m| self.impl_self_args(m).is_some_and(|a| a.2) == by_ref).collect();
            let pool = if matching.is_empty() { impls } else { matching };
            if pool.len() == 1 {
                return Hit::Found(instance(pool[0], 0.85));
            }
            let traits: HashSet<String> = pool
                .iter()
                .map(|&m| self.trait_impl_header(m).map(|h| format!("{}<{}>", h.trait_, h.trait_args.join(","))).unwrap_or_default())
                .collect();
            return if traits.len() == 1 { self.pick_method(&pool, &HashSet::new(), r, 0.85) } else { Hit::Nothing };
        }
        if PRIMITIVE_TYPES.contains(&name) {
            let declared: Vec<SymId> = self
                .rust_named(method)
                .filter(|&m| self.syms.get(m).kind == NodeKind::Method && self.trait_of(m).is_some())
                .filter(|&m| {
                    let t = self.trait_of(m).unwrap();
                    let ts = self.syms.get(t);
                    self.trait_implemented(&ts.name, Some(&ts.file), Some(name), &HashSet::new(), &[], 0) && self.trait_in_scope(&ts.name, r, m)
                })
                .collect();
            let traits: HashSet<SymId> = declared.iter().filter_map(|&m| self.trait_of(m)).collect();
            if traits.len() == 1 {
                return Hit::Found(instance(declared[0], 0.8));
            }
            if traits.len() > 1 {
                return Hit::Nothing;
            }
        }
        self.provided_method(is_ident(name).then_some(name), &HashSet::new(), &[], method, r).unwrap_or(Hit::Nothing)
    }

    /// The method a Rust call reaches on a value of a type as read.
    pub fn typed_method_call(&self, read: &Read, method: &str, r: &Ref<'_>, depth: u8) -> Hit {
        let Some(written) = read.written() else { return Hit::Unread };
        let wrappers = read.wrappers();
        if wrappers.iter().any(|w| wrapper_own_method(w, method)) {
            return Hit::Nothing;
        }
        for w in wrappers {
            let own: Vec<SymId> = self
                .methods_named(w, method)
                .into_iter()
                .filter(|&m| !self.is_trait_impl_method(m) && self.trait_of(m).is_none())
                .collect();
            if own.is_empty() {
                continue;
            }
            let declared = self.type_lookup(w, &[], &written.file, Some(written.at));
            if !declared.outside && !declared.nodes.is_empty() {
                let files = declared.nodes.iter().map(|&n| self.syms.get(n).file.clone()).collect();
                return self.pick_method(&own, &files, r, 0.9);
            }
        }
        if let Some(outer) = wrappers.first() {
            if SELF_WRAPPERS.contains(&outer.as_str()) && !self.is_project_named(outer, DECLARED_TYPE_KINDS) {
                let held = match read {
                    Read::Named { name, .. } => Some(name.as_str()),
                    _ => None,
                };
                let on_wrapper: Vec<SymId> = self
                    .methods_named(outer, method)
                    .into_iter()
                    .filter(|&n| self.self_wrapper(n).is_none() && self.impl_for(n, held))
                    .collect();
                if !on_wrapper.is_empty() {
                    let inner = self.typed_method_call(&read.without_outer_wrapper(), method, r, depth);
                    if let Hit::Found(m) = inner {
                        if self.self_wrapper(m.target) == Some(outer.as_str()) {
                            return inner;
                        }
                    }
                    let first = self.prefer_file(&on_wrapper, r.file)[0];
                    return Hit::Found(instance(first, 0.9));
                }
            }
        }
        match read {
            Read::Unread => Hit::Unread,
            Read::Outside { name, by_ref, .. } => self.outside_type_call(name, "", *by_ref, method, r),
            Read::Traits { traits, written, .. } => self.traits_method_call(traits, written, method, r),
            Read::Named { name, path, args, by_ref, written, wrappers } => {
                let lookup = self.type_lookup(name, path, &written.file, Some(written.at));
                if lookup.outside {
                    return self.outside_type_call(&lookup.name, args, *by_ref, method, r);
                }
                if lookup.undeclared && self.methods_named(&lookup.name, method).is_empty() {
                    return self.outside_type_call(&lookup.name, args, *by_ref, method, r);
                }
                if !lookup.nodes.is_empty() && lookup.nodes.iter().all(|&n| self.syms.get(n).kind == NodeKind::Trait) {
                    return self.traits_method_call(std::slice::from_ref(name), written, method, r);
                }
                if lookup.nodes.len() == 1 && self.syms.get(lookup.nodes[0]).kind == NodeKind::TypeAlias {
                    let alias = lookup.nodes[0];
                    let a = self.syms.get(alias);
                    let inherent: Vec<SymId> = self
                        .methods_named(&a.name, method)
                        .into_iter()
                        .filter(|&m| !self.is_trait_impl_method(m) && self.trait_of(m).is_none())
                        .collect();
                    if !inherent.is_empty() {
                        return self.pick_method(&inherent, &[a.file.clone()].into(), r, 0.9);
                    }
                    let target = if depth < 4 { self.alias_target(alias) } else { None };
                    return match target {
                        Some(mut t) => {
                            t.wrappers = wrappers.clone();
                            self.typed_method_call(&self.read_type(&t, r), method, r, depth + 1)
                        }
                        None => Hit::Unread,
                    };
                }
                let mut types: Vec<SymId> = lookup.nodes.iter().copied().filter(|&n| OWNER_KINDS.contains(&self.syms.get(n).kind)).collect();
                let distinct_files: HashSet<&str> = types.iter().map(|&t| self.syms.get(t).file.as_str()).collect();
                if types.len() > 1 && distinct_files.len() > 1 {
                    let having: Vec<SymId> = types
                        .iter()
                        .copied()
                        .filter(|&t| {
                            let one = Lookup {
                                nodes: vec![t],
                                ..lookup.clone()
                            };
                            !self.owned_methods(&lookup.name, method, &one).is_empty()
                        })
                        .collect();
                    if having.len() != 1 {
                        return Hit::Unread;
                    }
                    types = having;
                }
                let type_files: HashSet<String> = types.iter().map(|&t| self.syms.get(t).file.clone()).collect();
                let scoped = Lookup {
                    nodes: if lookup.nodes.is_empty() { Vec::new() } else { types.clone() },
                    ..lookup.clone()
                };
                let matches = self.owned_methods(&lookup.name, method, &scoped);
                let inherent: Vec<SymId> = matches.iter().copied().filter(|&m| !self.is_trait_impl_method(m)).collect();
                if !inherent.is_empty() {
                    return self.pick_method(&self.specialized(inherent, args), &type_files, r, 0.9);
                }
                let mut via_traits: Vec<(String, Vec<SymId>)> = Vec::new();
                for &m in &matches {
                    let t = self.trait_impl_header(m).map(|h| h.trait_).unwrap_or_default();
                    match via_traits.iter_mut().find(|(k, _)| *k == t) {
                        Some((_, v)) => v.push(m),
                        None => via_traits.push((t, vec![m])),
                    }
                }
                if !via_traits.is_empty() {
                    if via_traits.len() > 1 {
                        via_traits.retain(|(t, ms)| self.trait_callable(t, r, ms[0]));
                    }
                    if via_traits.len() != 1 {
                        return Hit::Nothing;
                    }
                    let mut methods = self.specialized(via_traits.remove(0).1, args);
                    let trait_args = |m: SymId| self.trait_impl_header(m).map(|h| h.trait_args.join(",")).unwrap_or_default();
                    let distinct: HashSet<String> = methods.iter().map(|&m| trait_args(m)).collect();
                    if distinct.len() > 1 {
                        let fit: Vec<SymId> = methods
                            .iter()
                            .copied()
                            .filter(|&m| {
                                let a = self.trait_impl_header(m).map(|h| h.trait_args).unwrap_or_default();
                                let head = a.iter().find(|x| x.starts_with(|c: char| c.is_ascii_uppercase())).or(a.first());
                                head.is_some_and(|h| self.first_arg_is(h, r))
                            })
                            .collect();
                        let fd: HashSet<String> = fit.iter().map(|&m| trait_args(m)).collect();
                        if fit.is_empty() || fd.len() > 1 {
                            return Hit::Nothing;
                        }
                        methods = fit;
                    }
                    return self.pick_method(&methods, &type_files, r, 0.9);
                }
                let via_alias = self.alias_impl_methods(&lookup.name, method);
                if !via_alias.is_empty() {
                    let owners: HashSet<&str> = via_alias.iter().filter_map(|&m| self.syms.get(m).owner()).collect();
                    return if owners.len() == 1 { self.pick_method(&via_alias, &type_files, r, 0.85) } else { Hit::Nothing };
                }
                if let Some(p) = self.provided_method(Some(&lookup.name), &type_files, &[], method, r) {
                    return p;
                }
                if depth < 3 {
                    if let Some(t) = self.deref_target(&lookup.name, &type_files) {
                        return self.typed_method_call(&self.read_type(&t, r), method, r, depth + 1);
                    }
                }
                Hit::Nothing
            }
        }
    }

    /// Same-file candidates first.
    pub fn prefer_file(&self, ids: &[SymId], file: &str) -> Vec<SymId> {
        let mut v = ids.to_vec();
        v.sort_by_key(|&i| self.syms.get(i).file != file);
        v
    }

    /// Whether a call's first argument is evidently a `ty`.
    pub fn first_arg_is(&self, ty: &str, r: &Ref<'_>) -> bool {
        let Some(src) = self.source(r.file) else { return true };
        let lines: Vec<&str> = src.lines().collect();
        let li = r.line as usize - 1;
        let method = r.name.rsplit([':', '.']).next().unwrap_or(r.name);
        let mut text = lines.get(li).map(|l| l.get(r.column as usize..).unwrap_or("")).unwrap_or("").to_string();
        for l in lines.iter().skip(li + 1).take(4) {
            text.push(' ');
            text.push_str(l);
        }
        let Some(open) = find_call_open(&text, method) else { return true };
        let b = text.as_bytes();
        let mut depth = 0;
        let mut end = open;
        while end < b.len() {
            match b[end] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    if depth == 0 {
                        break;
                    }
                    depth -= 1;
                }
                b',' if depth == 0 => break,
                _ => {}
            }
            end += 1;
        }
        let mut arg = text[open..end].trim();
        loop {
            let before = arg;
            if let Some(a) = arg.strip_prefix('&') {
                arg = a.trim_start();
                if let Some(a) = arg.strip_prefix("mut ") {
                    arg = a.trim_start();
                }
            }
            if let Some(a) = arg.strip_prefix('*') {
                arg = a;
            }
            if arg == before {
                break;
            }
        }
        let arg = arg.strip_suffix(".clone()").unwrap_or(arg);
        let path_is = |a: &str| {
            let mut t = a;
            loop {
                if t.starts_with(ty) && !t[ty.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
                    return true;
                }
                let Some(id) = super::scopes::ident_at(t, 0) else { return false };
                match t[id.len()..].strip_prefix("::") {
                    Some(r) => t = r,
                    None => return false,
                }
            }
        };
        if path_is(arg) {
            return true;
        }
        if !is_ident(arg) {
            return false;
        }
        let mut i = li + 1;
        while i > 0 {
            i -= 1;
            let line = lines.get(i).copied().unwrap_or("");
            if arg == "self" && is_impl_line_text(line) {
                let h = super::impls::parse_impl_header(&format!("{line} {{"));
                let self_ = h.map(|h| h.self_).or_else(|| {
                    let at = super::uses::find_word(line, "impl")?;
                    let mut rest = line[at + 4..].trim_start();
                    if rest.starts_with('<') {
                        rest = &rest[angle_end(rest, 0)? + 1..];
                    }
                    let segs: Vec<&str> = rest.trim_start().split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')).next()?.split("::").collect();
                    segs.last().map(|s| s.to_string())
                });
                return self_.as_deref() == Some(ty);
            }
            if arg != "self" && (declares_with(line, arg, ty) || builds_with(line, arg, ty)) {
                return true;
            }
            if arg != "self" && is_fn_line(line) {
                break;
            }
        }
        false
    }
}

/// `impl [<…>] [path::]Trait[<…>] for [&…] Self … {` headers for `trait_` in blanked code: (generics, where, self text).
fn impl_headers_for(code: &str, trait_: &str) -> Vec<(Option<String>, String, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = super::uses::find_word(&code[from..], "impl").map(|i| from + i) {
        from = i + 4;
        let mut rest = &code[i + 4..];
        let mut generics = None;
        let t = rest.trim_start();
        if t.starts_with('<') {
            let Some(e) = angle_end(t, 0) else { continue };
            generics = Some(t[1..e].to_string());
            rest = &t[e + 1..];
        }
        let head_end = rest.find(['{', ';']).unwrap_or(rest.len());
        let header = &rest[..head_end];
        let Some(f) = super::uses::find_word(header, "for") else { continue };
        let trait_part = header[..f].trim();
        let tp = trait_part.split('<').next().unwrap_or("").trim();
        let tname = tp.rsplit("::").next().unwrap_or(tp).trim();
        if tname != trait_ {
            continue;
        }
        let mut self_part = header[f + 3..].trim();
        let mut where_ = String::new();
        if let Some(w) = super::uses::find_word(self_part, "where") {
            where_ = self_part[w + 5..].to_string();
            self_part = self_part[..w].trim();
        }
        let self_part = strip_amp(self_part).trim().to_string();
        out.push((generics, where_, self_part));
    }
    out
}

fn is_impl_line_text(line: &str) -> bool {
    let t = line.trim_start();
    let t = t.strip_prefix("pub").map_or(t, |r| {
        let r = r.trim_start();
        if r.starts_with('(') { r.find(')').map_or(r, |e| r[e + 1..].trim_start()) } else { r }
    });
    let t = t.strip_prefix("unsafe").map_or(t, str::trim_start);
    super::uses::starts_with_word(t, "impl")
}

fn is_fn_line(line: &str) -> bool {
    let mut t = line.trim_start();
    for w in ["pub", "const", "async", "unsafe"] {
        if let Some(r) = t.strip_prefix(w) {
            let r2 = r.trim_start();
            if w == "pub" && r2.starts_with('(') {
                t = r2.find(')').map_or(r2, |e| r2[e + 1..].trim_start());
            } else if r.starts_with(char::is_whitespace) {
                t = r2;
            }
        }
    }
    if let Some(r) = t.strip_prefix("extern") {
        t = r.trim_start();
        if t.starts_with('"') {
            t = t[1..].find('"').map_or(t, |e| t[e + 2..].trim_start());
        }
    }
    t.starts_with("fn ")
}

/// `arg: &'a mut path::Ty` on a line.
fn declares_with(line: &str, arg: &str, ty: &str) -> bool {
    let mut from = 0;
    while let Some(i) = super::uses::find_word(&line[from..], arg).map(|i| from + i) {
        from = i + 1;
        let rest = line[i + arg.len()..].trim_start();
        let Some(rest) = rest.strip_prefix(':') else { continue };
        if rest.starts_with(':') {
            continue;
        }
        let t = strip_amp(rest.trim_start());
        if strip_path_to(t, ty) {
            return true;
        }
    }
    false
}

/// `let [mut] arg = &path::Ty…` on a line.
fn builds_with(line: &str, arg: &str, ty: &str) -> bool {
    let Some(i) = super::uses::find_word(line, "let") else { return false };
    let rest = line[i + 3..].trim_start();
    let rest = rest.strip_prefix("mut ").map_or(rest, str::trim_start);
    let Some(rest) = rest.strip_prefix(arg) else { return false };
    if rest.starts_with(|c: char| c.is_alphanumeric() || c == '_') {
        return false;
    }
    let Some(rest) = rest.trim_start().strip_prefix('=') else { return false };
    let t = rest.trim_start().trim_start_matches('&').trim_start();
    strip_path_to(t, ty)
}

fn strip_path_to(mut t: &str, ty: &str) -> bool {
    loop {
        if t.starts_with(ty) && !t[ty.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            return true;
        }
        let Some(id) = super::scopes::ident_at(t, 0) else { return false };
        match t[id.len()..].strip_prefix("::") {
            Some(r) => t = r,
            None => return false,
        }
    }
}

fn find_call_open(text: &str, method: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = super::uses::find_word(&text[from..], method).map(|i| from + i) {
        from = i + 1;
        let rest = text[i + method.len()..].trim_start();
        if rest.starts_with('(') {
            return Some(text.len() - rest.len() + 1);
        }
    }
    None
}

#[derive(Debug, Clone, Default)]
pub struct Fields {
    pub tuple: Option<Vec<String>>,
    pub named: Option<HashMap<String, String>>,
    pub generics: String,
}

#[derive(Debug, Clone)]
pub struct Lookup {
    pub nodes: Vec<SymId>,
    pub outside: bool,
    pub undeclared: bool,
    pub name: String,
}

/// What the struct's own type parameters are known by inside the impl around `at`.
fn self_param_bounds(struct_generics: &str, sc: &Scopes, at: usize, file: &str) -> Option<HashMap<String, (Vec<String>, String, usize)>> {
    if struct_generics.trim().is_empty() {
        return None;
    }
    let blk = sc.impl_at(at)?;
    if blk.kind != super::scopes::BlockKind::Impl {
        return None;
    }
    let mut header = sc.code[blk.at..blk.from].trim_start().strip_prefix("impl")?.trim().to_string();
    if header.starts_with('<') {
        let e = angle_end(&header, 0)?;
        header = header[e + 1..].trim().to_string();
    }
    if let Some(f) = super::uses::find_word(&header, "for") {
        header = header[f + 3..].trim().to_string();
    }
    if let Some(w) = super::uses::find_word(&header, "where") {
        header.truncate(w);
    }
    let (_, _, args) = path_head(header.trim())?;
    let args: Vec<String> = split_top_level(&args, ',');
    let params: Vec<String> = split_top_level(struct_generics, ',')
        .iter()
        .map(|p| {
            let p = p.trim_start().strip_prefix("const ").unwrap_or(p.trim_start());
            p.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '\'').collect()
        })
        .collect();
    if params.len() != args.len() {
        return None;
    }
    let own: HashSet<String> = split_top_level(&blk.generics, ',')
        .iter()
        .map(|p| {
            let p = p.trim_start().strip_prefix("const ").unwrap_or(p.trim_start());
            p.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect()
        })
        .collect();
    let mut bounds = HashMap::new();
    for (p, arg) in params.iter().zip(&args) {
        if p.is_empty() || p.starts_with('\'') || !own.contains(arg) {
            continue;
        }
        if let Some(traits) = bounds_of(arg, &[Generics { generics: &blk.generics, where_: &blk.where_ }]) {
            if !traits.is_empty() {
                bounds.insert(p.clone(), (traits, file.to_string(), at));
            }
        }
    }
    (!bounds.is_empty()).then_some(bounds)
}

/// The `index`th field of a Rust tuple type as written.
pub fn tuple_field(w: &Written, index: usize) -> Option<Written> {
    let t = strip_amp(w.text.trim());
    if !t.starts_with('(') || !group_is_whole(t) {
        return None;
    }
    let fields = split_top_level(&t[1..t.len() - 1], ',');
    let f = fields.get(index)?;
    Some(Written {
        text: f.clone(),
        wrappers: Vec::new(),
        ..w.clone()
    })
}

/// What `.await` hands out of a value of a Rust future type.
fn future_output(w: &Written) -> Option<Written> {
    let mut t = w.text.trim().to_string();
    for wrapper in ["Pin", "Box"] {
        if let Some((name, open)) = wrapper_head(&t) {
            if name == wrapper && angle_end(&t, open) == Some(t.len() - 1) {
                t = t[open + 1..t.len() - 1].trim().to_string();
            }
        }
    }
    if let Some((name, open)) = wrapper_head(&t) {
        if name == "BoxFuture" || name == "LocalBoxFuture" {
            if angle_end(&t, open) != Some(t.len() - 1) {
                return None;
            }
            let args: Vec<String> = split_top_level(&t[open + 1..t.len() - 1], ',').into_iter().filter(|a| !a.starts_with('\'')).collect();
            return (args.len() == 1).then(|| w.with_text(args[0].clone()));
        }
    }
    let rest = t.strip_prefix("impl ").or_else(|| t.strip_prefix("dyn "))?.trim_start();
    let (name, open) = wrapper_head(rest)?;
    if name != "Future" && name != "IntoFuture" {
        return None;
    }
    let end = angle_end(rest, open)?;
    let after = rest[end + 1..].trim();
    if !(after.is_empty() || after.starts_with('+')) {
        return None;
    }
    let output = split_top_level(&rest[open + 1..end], ',').into_iter().find_map(|a| {
        a.strip_prefix("Output").map(str::trim_start).and_then(|r| r.strip_prefix('=')).map(|r| r.trim().to_string())
    })?;
    Some(w.with_text(output))
}

/// `[::][std|core|alloc::…::]Name<` → (Name, offset of `<`).
pub fn wrapper_head(t: &str) -> Option<(String, usize)> {
    let lt = t.find('<')?;
    let path = t[..lt].trim().trim_start_matches("::").trim();
    let segs: Vec<&str> = path.split("::").map(str::trim).collect();
    if !segs.iter().all(|s| is_ident(s)) {
        return None;
    }
    // Only a std path: a project path names its own type.
    if segs.len() > 1 && !matches!(segs[0], "std" | "core" | "alloc") {
        return None;
    }
    Some((segs.last()?.to_string(), lt))
}

fn strip_lifetime_arg(inner: &str) -> &str {
    let t = inner.trim_start();
    if t.starts_with('\'') {
        if let Some(c) = t.find(',') {
            return &t[c + 1..];
        }
    }
    inner
}

fn is_fn_type(t: &str) -> bool {
    let mut s = t;
    loop {
        if let Some(r) = s.strip_prefix("unsafe ") {
            s = r.trim_start();
            continue;
        }
        if let Some(r) = s.strip_prefix("extern ") {
            let r = r.trim_start();
            s = if r.starts_with('"') { r[1..].find('"').map_or(r, |e| r[e + 2..].trim_start()) } else { r };
            continue;
        }
        break;
    }
    s.strip_prefix("fn").is_some_and(|r| r.trim_start().starts_with('('))
}

fn is_literal(v: &str) -> bool {
    let t = v.trim();
    if t.starts_with('"') || t.starts_with("r\"") || t.starts_with("r#") || t.starts_with("b\"") || t.starts_with("br") || t.starts_with("c\"") {
        return t.ends_with('"') || t.ends_with('#');
    }
    if (t.starts_with('\'') || t.starts_with("b'")) && t.ends_with('\'') {
        return true;
    }
    let num = t.strip_prefix('-').unwrap_or(t);
    if num.starts_with(|c: char| c.is_ascii_digit()) && num.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.') {
        return true;
    }
    t == "true" || t == "false" || t.starts_with('|') || t.starts_with("move |") || t.starts_with("move|")
}

fn is_screaming(s: &str) -> bool {
    s.len() > 1 && s.starts_with(|c: char| c.is_ascii_uppercase()) && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// `Box::new` / `std::rc::Rc::pin` / `Arc::from` → the wrapper.
fn box_like(callee: &str) -> Option<&'static str> {
    let c = ["std::boxed::", "std::rc::", "std::sync::", "alloc::boxed::", "alloc::rc::", "alloc::sync::"]
        .iter()
        .find_map(|p| callee.strip_prefix(p))
        .unwrap_or(callee);
    let (w, f) = c.split_once("::")?;
    if !matches!(f, "new" | "pin" | "from") {
        return None;
    }
    ["Box", "Rc", "Arc"].into_iter().find(|x| *x == w)
}

fn strip_suffix_ws<'s>(s: &'s str, suffix: &str) -> Option<&'s str> {
    let t = s.trim_end();
    let r = t.strip_suffix(suffix)?;
    if suffix.chars().all(|c| c.is_alphanumeric() || c == '_') && r.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some(r.trim_end())
}

/// `recv.method` / `recv.method::<T>` → parts, for a callee written `… . name`.
fn split_method_callee(callee: &str) -> Option<(String, String, Option<String>)> {
    let c = callee.trim_end();
    let (body, turbofish) = if c.ends_with('>') {
        let i = c.rfind("::<")?;
        (&c[..i], Some(c[i + 3..c.len() - 1].to_string()))
    } else {
        (c, None)
    };
    let body = body.trim_end();
    let end = body.len();
    let start = body.rfind(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')).map_or(0, |p| p + 1);
    let method = &body[start..end];
    if method.is_empty() || !method.starts_with(|ch: char| ch.is_ascii_lowercase() || ch == '_') {
        return None;
    }
    let recv = body[..start].trim_end().strip_suffix('.')?.trim_end();
    if recv.is_empty() {
        return None;
    }
    Some((recv.to_string(), method.to_string(), turbofish))
}

/// `recv.field` / `recv.0` → parts.
fn split_field_access(v: &str) -> Option<(String, String)> {
    let t = v.trim_end();
    let start = t.rfind(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '_')).map_or(0, |p| p + 1);
    let field = &t[start..];
    if field.is_empty() {
        return None;
    }
    let recv = t[..start].trim_end().strip_suffix('.')?;
    if recv.ends_with('.') || recv.trim().is_empty() {
        return None;
    }
    Some((recv.trim_end().to_string(), field.to_string()))
}

/// `Path::To::Type {` — a struct literal's path.
fn struct_literal_path(v: &str) -> Option<String> {
    let brace = v.find('{')?;
    let mut head = v[..brace].trim();
    if let Some(i) = head.rfind("::<") {
        if head.ends_with('>') {
            head = head[..i].trim();
        }
    }
    let segs: Vec<&str> = head.split("::").map(str::trim).collect();
    if !segs.iter().all(|s| is_ident(s)) || !segs.last()?.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    Some(segs.join("::"))
}

fn replace_idents(text: &str, bound: &HashMap<String, String>) -> String {
    let b = text.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if (c.is_ascii_alphabetic() || c == b'_') && (i == 0 || !(b[i - 1].is_ascii_alphanumeric() || b[i - 1] == b'_' || b[i - 1] == b'\'')) {
            let s = i;
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            let id = &text[s..i];
            out.push_str(bound.get(id).map_or(id, String::as_str));
            continue;
        }
        out.push(c as char);
        i += 1;
    }
    out
}

fn strip_attributes(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('#') {
        out.push_str(&rest[..i]);
        let after = rest[i + 1..].trim_start();
        if after.starts_with('[') {
            let mut depth = 0;
            let mut end = None;
            for (j, c) in after.char_indices() {
                match c {
                    '[' => depth += 1,
                    ']' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(j);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            match end {
                Some(e) => {
                    out.push(' ');
                    rest = &after[e + 1..];
                }
                None => {
                    rest = "";
                }
            }
        } else {
            out.push('#');
            rest = &rest[i + 1..];
        }
    }
    out.push_str(rest);
    out
}

fn collapse_ws(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

pub fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Path proximity: a longer common directory prefix is nearer.
pub fn proximity(a: &str, b: &str) -> usize {
    common_prefix_len(a, b)
}

fn item_projection(step: &str) -> Option<&'static str> {
    match step {
        "keys" | "into_keys" => Some("key"),
        "values" | "values_mut" | "into_values" => Some("value"),
        "enumerate" => Some("enumerate"),
        _ => None,
    }
}

fn std_iterator(name: &str) -> Option<&'static str> {
    match name {
        "Iter" | "IterMut" | "IntoIter" | "Drain" => Some("entry"),
        "Keys" | "IntoKeys" => Some("key"),
        "Values" | "ValuesMut" | "IntoValues" => Some("value"),
        _ => None,
    }
}

const ITEM_ADAPTERS: &[&str] = &["iter", "iter_mut", "into_iter", "drain", "rev", "copied", "cloned", "skip", "take", "step_by", "peekable", "by_ref", "fuse"];
const STD_SEQUENCES: &[&str] = &["Vec", "VecDeque", "HashSet", "BTreeSet", "BinaryHeap", "LinkedList", "Option", "IndexSet", "SmallVec"];
const STD_MAPS: &[&str] = &["HashMap", "BTreeMap", "IndexMap"];
const ITERATOR_TRAITS: &[&str] = &["Iterator", "IntoIterator", "DoubleEndedIterator", "ExactSizeIterator"];
