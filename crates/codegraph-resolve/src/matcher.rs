//! Reference → target symbol.
//!
//! Strategies are tried most-specific first: the enclosing type's members,
//! the same file, import bindings, the same package/namespace, and finally a
//! project-wide name match ranked by path proximity.

use crate::imports::{parse_import, Binding};
use crate::modules::{common_prefix_len, dir_of, Modules};
use crate::symbols::{same_family, SymId, Symbols};
use codegraph_core::search::is_test_path;
use codegraph_core::{Language, NodeKind, ReferenceKind};
use regex::Regex;
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;
use std::sync::LazyLock;

/// How a reference was matched.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Match {
    pub target: SymId,
    pub strategy: &'static str,
    pub confidence: f32,
}

impl Match {
    fn new(target: SymId, strategy: &'static str, confidence: f32) -> Self {
        Match {
            target,
            strategy,
            confidence,
        }
    }

    /// Whether the match came from name similarity rather than scope/imports.
    pub fn is_heuristic(&self) -> bool {
        self.strategy.starts_with("name-match")
    }
}

/// What a reference of `kind` may point at.
fn kind_accepts(kind: ReferenceKind, target: NodeKind) -> bool {
    use NodeKind as N;
    use ReferenceKind as R;
    if matches!(target, N::File | N::Import | N::Parameter | N::Export) {
        return false;
    }
    match kind {
        R::Calls => matches!(
            target,
            N::Function
                | N::Method
                | N::Class
                | N::Struct
                | N::Component
                | N::Variable
                | N::Constant
                | N::Property
                | N::Field
                | N::Enum
        ),
        R::Instantiates => target.is_type() || matches!(target, N::Component | N::Function),
        R::Extends => matches!(
            target,
            N::Class
                | N::Interface
                | N::Struct
                | N::Trait
                | N::Protocol
                | N::TypeAlias
                | N::Module
                | N::Enum
        ),
        R::Implements => matches!(
            target,
            N::Interface | N::Trait | N::Protocol | N::Class | N::Module | N::Struct
        ),
        R::TypeOf | R::Returns => {
            target.is_type() || matches!(target, N::Enum | N::Namespace | N::Module)
        }
        R::Decorates => matches!(
            target,
            N::Function | N::Class | N::Method | N::Variable | N::Constant
        ),
        R::Overrides => matches!(target, N::Method | N::Function | N::Property),
        R::Navigates => matches!(target, N::Route | N::Component | N::Function),
        _ => true,
    }
}

/// Preference among acceptable targets (higher first).
fn kind_rank(kind: ReferenceKind, target: NodeKind) -> u8 {
    use NodeKind as N;
    match kind {
        ReferenceKind::Calls | ReferenceKind::FunctionRef => match target {
            N::Function | N::Method => 3,
            N::Class | N::Struct | N::Component => 2,
            _ => 1,
        },
        _ => {
            if target.is_type() {
                3
            } else {
                1
            }
        }
    }
}

const SELF_WORDS: &[&str] = &[
    "this", "self", "$this", "Self", "static", "cls", "me", "Me", "@",
];
const SUPER_WORDS: &[&str] = &["super", "parent", "base", "MyBase"];

/// Split `a.b.c` / `A::b` / `$x->y` / `a#b` at the last member separator.
fn split_member(name: &str) -> Option<(&str, &str)> {
    // The separator ending last wins; at the same end the longer one (`::` over `:`).
    let mut best: Option<(usize, usize)> = None;
    for sep in ["::", "->", ".", "#", "\\", ":"] {
        if let Some(i) = name.rfind(sep) {
            let key = (i + sep.len(), sep.len());
            if best.is_none_or(|(b, l)| key > (b + l, l)) {
                best = Some((i, sep.len()));
            }
        }
    }
    let (i, len) = best?;
    let (head, tail) = (&name[..i], &name[i + len..]);
    (!head.is_empty() && !tail.is_empty()).then_some((head, tail))
}

/// First segment of a member head (`a.b` → `a`, `Foo()` → `Foo`).
fn head_root(head: &str) -> &str {
    let end = head
        .find(['.', ':', '-', '#', '\\', '(', '[', '<'])
        .unwrap_or(head.len());
    &head[..end]
}

fn clean(name: &str) -> String {
    let mut s = name
        .trim()
        .trim_start_matches("::")
        .trim_start_matches("new ")
        .to_string();
    s = s.replace("?.", ".").replace("!.", ".");
    while s.ends_with("()") {
        s.truncate(s.len() - 2);
    }
    s.trim_end_matches(['!', '?', ';']).to_string()
}

/// Normalize a namespace path for comparison (`App\Models`, `App::Models`, `app.models`).
fn norm_ns(s: &str) -> String {
    s.replace(['\\', '/', '.'], "::")
        .trim_matches(':')
        .to_lowercase()
}

static JS_DEFAULT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"export\s+default\s+(?:abstract\s+)?(?:async\s+)?(?:function\s*\*?|class)?\s*([A-Za-z_$][\w$]*)").unwrap()
});
static CJS_DEFAULT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"module\.exports\s*=\s*([A-Za-z_$][\w$]*)\s*;?\s*$").unwrap());

pub struct Matcher<'a> {
    pub syms: &'a Symbols,
    pub modules: &'a Modules,
    root: &'a Path,
    bindings: HashMap<String, Rc<Vec<Binding>>>,
    defaults: HashMap<String, Option<SymId>>,
}

impl<'a> Matcher<'a> {
    pub fn new(syms: &'a Symbols, modules: &'a Modules, root: &'a Path) -> Self {
        Matcher {
            syms,
            modules,
            root,
            bindings: HashMap::new(),
            defaults: HashMap::new(),
        }
    }

    /// Import bindings declared in `file`.
    pub fn bindings(&mut self, file: &str) -> Rc<Vec<Binding>> {
        if let Some(b) = self.bindings.get(file) {
            return b.clone();
        }
        let mut out = Vec::new();
        for &i in self.syms.in_file(file) {
            let s = self.syms.get(i);
            if s.kind == NodeKind::Import {
                out.extend(parse_import(
                    s.language,
                    s.statement.as_deref().unwrap_or(""),
                    &s.name,
                ));
            }
        }
        let rc = Rc::new(out);
        self.bindings.insert(file.to_string(), rc.clone());
        rc
    }

    /// Resolve one reference made from `from` in `file`.
    pub fn resolve(
        &mut self,
        from: SymId,
        file: &str,
        lang: Language,
        kind: ReferenceKind,
        name: &str,
    ) -> Option<Match> {
        if kind == ReferenceKind::Imports {
            return self.resolve_import(file, lang, name);
        }
        let name = clean(name);
        if name.is_empty() {
            return None;
        }
        match split_member(&name) {
            Some((head, tail)) if !head.ends_with('(') => {
                self.resolve_member(from, file, lang, kind, &name, head, tail)
            }
            _ => self.resolve_simple(from, file, lang, kind, &name),
        }
    }

    // ------------------------------------------------------------- imports

    fn file_target(&self, files: &[String]) -> Option<SymId> {
        files.first().and_then(|f| self.syms.file_node(f))
    }

    fn resolve_import(&mut self, file: &str, lang: Language, name: &str) -> Option<Match> {
        let bindings = self.bindings(file);
        // A module specifier: the import edge points at the module's file.
        if bindings.iter().any(|b| b.module == name)
            || !bindings
                .iter()
                .any(|b| b.local == name || qualified_binding_name(b) == name)
        {
            let files = self.modules.resolve(self.syms, file, lang, name);
            if let Some(t) = self.file_target(&files) {
                return Some(Match::new(t, "import-module", 0.95));
            }
            if let Some(t) = self.qualified_type(lang, name) {
                return Some(Match::new(t, "import-qualified", 0.9));
            }
            if !bindings
                .iter()
                .any(|b| b.local == name || qualified_binding_name(b) == name)
            {
                return None;
            }
        }
        let binding = bindings
            .iter()
            .find(|b| b.local == name || qualified_binding_name(b) == name)?
            .clone();
        let target = self.resolve_binding(file, lang, &binding, 0)?;
        Some(Match::new(target, "import-binding", 0.95))
    }

    /// A type named by a fully qualified path (`com.y.Util`, `App\Models\User`).
    fn qualified_type(&self, lang: Language, path: &str) -> Option<SymId> {
        let (ns, name) = path
            .rsplit_once(['.', '\\'])
            .or_else(|| path.rsplit_once("::"))?;
        let want = norm_ns(ns);
        self.syms.named_for(name, lang).iter().copied().find(|&i| {
            let s = self.syms.get(i);
            same_family(s.language, lang)
                && (s.kind.is_type() || matches!(s.kind, NodeKind::Module | NodeKind::Namespace))
                && s.owner().is_some_and(|o| {
                    let o = norm_ns(o);
                    o == want || o.ends_with(&format!("::{want}"))
                })
        })
    }

    /// What an import binding refers to.
    fn resolve_binding(
        &mut self,
        file: &str,
        lang: Language,
        b: &Binding,
        depth: u8,
    ) -> Option<SymId> {
        if depth > 4 {
            return None;
        }
        if uses_namespaces(lang) {
            if let Some(imported) = &b.imported {
                return self
                    .qualified_type(lang, &format!("{}.{}", b.module, imported))
                    .or_else(|| {
                        let files = self.modules.resolve(self.syms, file, lang, &b.module);
                        files
                            .iter()
                            .find_map(|f| self.top_level_named(f, imported, lang))
                    });
            }
        }
        let files = self.modules.resolve(self.syms, file, lang, &b.module);
        if files.is_empty() {
            return None;
        }
        match &b.imported {
            None => self.file_target(&files),
            Some(_) if b.is_default => self.default_export(&files[0], lang),
            Some(imported) => files
                .iter()
                .find_map(|f| self.find_export(f, imported, lang, depth + 1)),
        }
    }

    fn top_level_named(&self, file: &str, name: &str, lang: Language) -> Option<SymId> {
        let candidates: Vec<SymId> = self
            .syms
            .in_file(file)
            .iter()
            .copied()
            .filter(|&i| {
                let s = self.syms.get(i);
                !matches!(
                    s.kind,
                    NodeKind::File | NodeKind::Import | NodeKind::Parameter
                ) && (s.name == name
                    || crate::symbols::case_insensitive(lang) && s.name.eq_ignore_ascii_case(name))
            })
            .collect();
        candidates.iter().copied().max_by_key(|&i| {
            let s = self.syms.get(i);
            let top = s.owner().is_none_or(|o| !self.is_type_owner(file, o));
            (top, s.exported, std::cmp::Reverse(s.start_line))
        })
    }

    fn is_type_owner(&self, file: &str, owner: &str) -> bool {
        self.syms.in_file(file).iter().any(|&i| {
            let s = self.syms.get(i);
            s.qualified_name == owner && (s.kind.is_type() || s.kind.is_callable())
        })
    }

    /// The symbol `file` exports as `name`, following re-exports.
    fn find_export(&mut self, file: &str, name: &str, lang: Language, depth: u8) -> Option<SymId> {
        if let Some(hit) = self.top_level_named(file, name, lang) {
            return Some(hit);
        }
        if depth > 4 {
            return None;
        }
        let bindings = self.bindings(file);
        // Python packages and Rust modules re-export whatever they import.
        let implicit = matches!(lang, Language::Python | Language::Rust);
        if let Some(b) = bindings
            .iter()
            .find(|b| b.local == name && (b.is_reexport || implicit))
            .cloned()
        {
            if let Some(hit) = self.resolve_binding(file, lang, &b, depth + 1) {
                return Some(hit);
            }
        }
        for b in bindings
            .iter()
            .filter(|b| b.local == "*" && (b.is_reexport || implicit))
            .cloned()
            .collect::<Vec<_>>()
        {
            for f in self.modules.resolve(self.syms, file, lang, &b.module) {
                if let Some(hit) = self.find_export(&f, name, lang, depth + 1) {
                    return Some(hit);
                }
            }
        }
        None
    }

    fn default_export(&mut self, file: &str, lang: Language) -> Option<SymId> {
        if let Some(hit) = self.defaults.get(file) {
            return *hit;
        }
        let text = std::fs::read_to_string(self.root.join(file)).unwrap_or_default();
        let named = JS_DEFAULT
            .captures(&text)
            .or_else(|| CJS_DEFAULT.captures(&text))
            .and_then(|c| self.top_level_named(file, &c[1], lang));
        let hit = named.or_else(|| {
            let exported: Vec<SymId> = self
                .syms
                .in_file(file)
                .iter()
                .copied()
                .filter(|&i| self.syms.get(i).exported && self.syms.get(i).owner().is_none())
                .collect();
            if exported.len() == 1 {
                Some(exported[0])
            } else {
                self.syms.file_node(file)
            }
        });
        self.defaults.insert(file.to_string(), hit);
        hit
    }

    // ------------------------------------------------------------- symbols

    /// The type whose body `from` is in (or `from` itself when it is a type).
    fn enclosing_type(&self, from: SymId) -> Option<SymId> {
        let mut cur = from;
        for _ in 0..8 {
            let s = self.syms.get(cur);
            if s.kind.is_type() || matches!(s.kind, NodeKind::Module) && s.owner().is_some() {
                return Some(cur);
            }
            cur = self.syms.owner_type(cur).or_else(|| {
                // Methods of a Go struct, Rust impl, Swift extension: owner by qualified name.
                let owner = s.owner()?;
                let name = owner.rsplit("::").next()?;
                self.syms.named(name).iter().copied().find(|&i| {
                    let o = self.syms.get(i);
                    o.qualified_name == owner && o.kind.is_type()
                })
            })?;
        }
        None
    }

    fn resolve_simple(
        &mut self,
        from: SymId,
        file: &str,
        lang: Language,
        kind: ReferenceKind,
        name: &str,
    ) -> Option<Match> {
        // 1. A member of the enclosing type. Extraction drops `this.`/`self.`
        //    receivers, so this covers explicit-receiver languages too.
        if !matches!(
            lang,
            Language::Go | Language::C | Language::Lua | Language::Luau | Language::R
        ) {
            if let Some(ty) = self.enclosing_type(from) {
                if let Some(m) = self.syms.member_named(ty, name, lang) {
                    if kind_accepts(kind, self.syms.get(m).kind) {
                        return Some(Match::new(m, "member", 0.9));
                    }
                }
            }
        }
        // 2. The same file.
        if let Some(t) = self.best_in_file(from, file, name, lang, kind) {
            return Some(Match::new(t, "same-file", 0.9));
        }
        // 3. An import binding.
        let bindings = self.bindings(file);
        if let Some(b) = bindings.iter().find(|b| b.local == name).cloned() {
            if let Some(t) = self.resolve_binding(file, lang, &b, 0) {
                if kind_accepts(kind, self.syms.get(t).kind)
                    || self.syms.get(t).kind == NodeKind::File
                {
                    return Some(Match::new(t, "import", 0.95));
                }
            }
        }
        // 4. The same package / namespace, or a wildcard import.
        if let Some(t) = self.same_scope(file, lang, kind, name, &bindings, from) {
            return Some(Match::new(t, "package", 0.85));
        }
        // 5. Project-wide by name.
        self.global(from, file, lang, kind, name)
    }

    fn best_in_file(
        &self,
        from: SymId,
        file: &str,
        name: &str,
        lang: Language,
        kind: ReferenceKind,
    ) -> Option<SymId> {
        self.syms
            .in_file(file)
            .iter()
            .copied()
            .filter(|&i| {
                let s = self.syms.get(i);
                (s.name == name
                    || crate::symbols::case_insensitive(lang) && s.name.eq_ignore_ascii_case(name))
                    && kind_accepts(kind, s.kind)
                    && self.syms.visible_from(i, from)
            })
            .max_by_key(|&i| {
                let s = self.syms.get(i);
                (
                    kind_rank(kind, s.kind),
                    s.owner().is_none(),
                    std::cmp::Reverse(s.start_line),
                )
            })
    }

    fn same_scope(
        &self,
        file: &str,
        lang: Language,
        kind: ReferenceKind,
        name: &str,
        bindings: &[Binding],
        from: SymId,
    ) -> Option<SymId> {
        let candidates = self.syms.named_for(name, lang);
        let accept = |i: &SymId| {
            let s = self.syms.get(*i);
            same_family(s.language, lang) && kind_accepts(kind, s.kind) && !s.local
        };
        match lang {
            Language::Go => {
                let dir = dir_of(file);
                candidates
                    .iter()
                    .copied()
                    .filter(accept)
                    .find(|&i| dir_of(&self.syms.get(i).file) == dir)
            }
            Language::Java
            | Language::Kotlin
            | Language::Scala
            | Language::CSharp
            | Language::VbNet
            | Language::Php
            | Language::Cpp
            | Language::Swift
            | Language::Dart => {
                let mut scopes: Vec<String> = Vec::new();
                if let Some(ns) = self.namespace_of(from) {
                    scopes.push(norm_ns(&ns));
                }
                scopes.extend(
                    bindings
                        .iter()
                        .filter(|b| b.local == "*")
                        .map(|b| norm_ns(&b.module)),
                );
                let in_scope = |i: SymId| {
                    let s = self.syms.get(i);
                    let owner = s.owner().map(norm_ns).unwrap_or_default();
                    scopes.iter().any(|sc| {
                        !sc.is_empty() && (owner == *sc || owner.ends_with(&format!("::{sc}")))
                    }) || dir_of(&s.file) == dir_of(file)
                        && matches!(
                            lang,
                            Language::Java | Language::Kotlin | Language::Swift | Language::Dart
                        )
                };
                candidates
                    .iter()
                    .copied()
                    .filter(accept)
                    .find(|&i| in_scope(i))
            }
            _ => None,
        }
    }

    /// The namespace/package a symbol is declared in (`com.x::A::f` → `com.x`).
    fn namespace_of(&self, from: SymId) -> Option<String> {
        let s = self.syms.get(from);
        for &i in self.syms.in_file(&s.file) {
            let n = self.syms.get(i);
            if matches!(n.kind, NodeKind::Namespace | NodeKind::Module)
                && n.start_line <= s.start_line
                && s.end_line <= n.end_line.max(s.end_line)
            {
                return Some(n.qualified_name.clone());
            }
        }
        None
    }

    fn global(
        &self,
        from: SymId,
        file: &str,
        lang: Language,
        kind: ReferenceKind,
        name: &str,
    ) -> Option<Match> {
        let mut candidates: Vec<SymId> = self
            .syms
            .named_for(name, lang)
            .iter()
            .copied()
            .filter(|&i| {
                let s = self.syms.get(i);
                same_family(s.language, lang)
                    && kind_accepts(kind, s.kind)
                    && i != from
                    && self.syms.visible_from(i, from)
            })
            .collect();
        // Methods are reached through a receiver; a bare call names a function.
        if kind == ReferenceKind::Calls
            && candidates
                .iter()
                .any(|&i| self.syms.get(i).kind != NodeKind::Method)
        {
            candidates.retain(|&i| self.syms.get(i).kind != NodeKind::Method);
        }
        // Variables/fields elsewhere are rarely what a bare call means.
        if matches!(kind, ReferenceKind::Calls) {
            let callable: Vec<SymId> = candidates
                .iter()
                .copied()
                .filter(|&i| {
                    !matches!(
                        self.syms.get(i).kind,
                        NodeKind::Variable
                            | NodeKind::Property
                            | NodeKind::Field
                            | NodeKind::Constant
                    )
                })
                .collect();
            if !callable.is_empty() {
                candidates = callable;
            }
        }
        self.pick_closest(
            &candidates,
            file,
            kind,
            "name-match",
            "name-match-ambiguous",
        )
    }

    fn pick_closest(
        &self,
        candidates: &[SymId],
        file: &str,
        kind: ReferenceKind,
        unique: &'static str,
        ambiguous: &'static str,
    ) -> Option<Match> {
        match candidates.len() {
            0 => None,
            1 => Some(Match::new(candidates[0], unique, 0.7)),
            n if n <= 8 => {
                let from_test = is_test_path(file);
                let score = |i: SymId| {
                    let s = self.syms.get(i);
                    (
                        kind_rank(kind, s.kind),
                        is_test_path(&s.file) == from_test,
                        common_prefix_len(&s.file, file),
                        s.exported,
                    )
                };
                let mut ranked: Vec<SymId> = candidates.to_vec();
                ranked.sort_by_key(|&i| std::cmp::Reverse(score(i)));
                let clear = score(ranked[0]) > score(ranked[1]);
                Some(Match::new(
                    ranked[0],
                    ambiguous,
                    if clear { 0.5 } else { 0.35 },
                ))
            }
            _ => None,
        }
    }

    fn resolve_member(
        &mut self,
        from: SymId,
        file: &str,
        lang: Language,
        kind: ReferenceKind,
        full: &str,
        head: &str,
        tail: &str,
    ) -> Option<Match> {
        let root = head_root(head);
        let single = root.len() == head.len()
            || head.ends_with(')') && !head[..head.len() - 1].contains(['.', ':']);

        // `this.x` / `self.x` / `super.x`
        if single && (SELF_WORDS.contains(&root) || SUPER_WORDS.contains(&root)) {
            let ty = self.enclosing_type(from)?;
            let start = if SUPER_WORDS.contains(&root) {
                self.syms.supers.get(&ty).and_then(|s| s.first().copied())?
            } else {
                ty
            };
            return self
                .syms
                .member_named(start, tail, lang)
                .map(|m| Match::new(m, "member", 0.9));
        }

        // A qualified name defined as such (`Foo::bar` with qualified_name `Foo::bar`).
        let qualified = full.replace(['.', '\\'], "::").replace("->", "::");
        let qhits: Vec<SymId> = self
            .syms
            .named_for(tail, lang)
            .iter()
            .copied()
            .filter(|&i| {
                let s = self.syms.get(i);
                same_family(s.language, lang)
                    && (s.qualified_name == qualified
                        || s.qualified_name.ends_with(&format!("::{qualified}")))
            })
            .collect();
        if let Some(m) = self.pick_closest(&qhits, file, kind, "qualified", "qualified") {
            return Some(Match {
                confidence: 0.9,
                ..m
            });
        }

        // An import binding: a module namespace or an imported type.
        let bindings = self.bindings(file);
        if single {
            if let Some(b) = bindings
                .iter()
                .find(|b| b.local == root || b.local == head)
                .cloned()
            {
                match &b.imported {
                    None => {
                        let files = self.modules.resolve(self.syms, file, lang, &b.module);
                        for f in &files {
                            if let Some(t) = self.find_export(f, tail, lang, 1) {
                                return Some(Match::new(t, "import-namespace", 0.9));
                            }
                        }
                        if !files.is_empty() || lang == Language::Go {
                            // A known module without that member, or an external package.
                            return None;
                        }
                    }
                    Some(_) => {
                        if let Some(t) = self.resolve_binding(file, lang, &b, 0) {
                            if let Some(m) = self.syms.member_named(t, tail, lang) {
                                return Some(Match::new(m, "import-member", 0.85));
                            }
                        }
                    }
                }
            }
        }

        // `Type.member` / `Type::member` / `Type().member`
        let type_name = head
            .rsplit(['.', ':'])
            .next()
            .unwrap_or(head)
            .trim_end_matches("()");
        let type_name = if head.ends_with(')') {
            head_root(head)
        } else {
            type_name
        };
        if type_name.chars().next().is_some_and(|c| c.is_uppercase())
            || matches!(
                lang,
                Language::Rust | Language::Cpp | Language::Ruby | Language::Php
            )
        {
            let types: Vec<SymId> = self
                .syms
                .named_for(type_name, lang)
                .iter()
                .copied()
                .filter(|&i| {
                    let s = self.syms.get(i);
                    same_family(s.language, lang)
                        && (s.kind.is_type()
                            || matches!(
                                s.kind,
                                NodeKind::Module | NodeKind::Namespace | NodeKind::Enum
                            ))
                })
                .collect();
            let mut ranked = types.clone();
            ranked.sort_by_key(|&i| {
                std::cmp::Reverse((
                    self.syms.get(i).file == file,
                    common_prefix_len(&self.syms.get(i).file, file),
                ))
            });
            for t in ranked {
                if let Some(m) = self.syms.member_named(t, tail, lang) {
                    return Some(Match::new(m, "type-member", 0.8));
                }
            }
        }

        // An unknown receiver (`obj.method()`): a method of that name, if it is
        // distinctive enough to pick one.
        if matches!(
            kind,
            ReferenceKind::Calls | ReferenceKind::FunctionRef | ReferenceKind::References
        ) {
            // The receiver might be the enclosing type's own instance (Go `s.N()`).
            if let Some(ty) = self.enclosing_type(from) {
                if let Some(m) = self.syms.member_named(ty, tail, lang) {
                    if lang == Language::Go {
                        return Some(Match::new(m, "receiver", 0.7));
                    }
                }
            }
            let methods: Vec<SymId> = self
                .syms
                .named_for(tail, lang)
                .iter()
                .copied()
                .filter(|&i| {
                    let s = self.syms.get(i);
                    same_family(s.language, lang)
                        && matches!(
                            s.kind,
                            NodeKind::Method | NodeKind::Function | NodeKind::Property
                        )
                        && s.owner().is_some()
                        && !s.local
                })
                .collect();
            // Without the receiver's type, only a name no other type shares is safe.
            let owners: std::collections::HashSet<&str> = methods
                .iter()
                .filter_map(|&i| self.syms.get(i).owner())
                .collect();
            if owners.len() == 1 {
                return self
                    .pick_closest(
                        &methods,
                        file,
                        kind,
                        "name-match-member",
                        "name-match-member",
                    )
                    .map(|m| Match {
                        confidence: 0.5,
                        ..m
                    });
            }
            return None;
        }
        None
    }
}

/// `com.y` + `Util` → `com.y.Util`; Rust `crate::util` + `helper` → `crate::util::helper`.
fn qualified_binding_name(b: &Binding) -> String {
    match &b.imported {
        Some(i)
            if b.module.contains("::")
                || b.module.starts_with("crate")
                || b.module.starts_with("super")
                || b.module.starts_with("self") =>
        {
            format!("{}::{i}", b.module)
        }
        Some(i) if b.module.contains('\\') => format!("{}\\{i}", b.module),
        Some(i) if !b.module.is_empty() => format!("{}.{i}", b.module),
        _ => b.module.clone(),
    }
}

/// Languages whose imports name namespaces rather than files.
fn uses_namespaces(lang: Language) -> bool {
    matches!(
        lang,
        Language::Java
            | Language::Kotlin
            | Language::Scala
            | Language::CSharp
            | Language::VbNet
            | Language::Php
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_members() {
        assert_eq!(split_member("a.b.c"), Some(("a.b", "c")));
        assert_eq!(split_member("Foo::bar"), Some(("Foo", "bar")));
        assert_eq!(split_member("$this->save"), Some(("$this", "save")));
        assert_eq!(split_member("plain"), None);
        assert_eq!(head_root("Thing()"), "Thing");
        assert_eq!(clean("new Foo()"), "Foo");
        assert_eq!(norm_ns("App\\Models"), "app::models");
    }
}
