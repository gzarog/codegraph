//! A Rust file's `use`s and inline modules, the module tree they walk, and
//! whether a name can mean a candidate where it is written.

use super::ctx::{Ctx, Ref};
use super::{is_associated_item, MODULE_ITEM_KINDS, PRELUDE, STD_ROOTS};
use crate::symbols::SymId;
use codegraph_core::{NodeKind, ReferenceKind, Visibility};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

/// A path a `use` binds: as written, less any `as` alias.
#[derive(Debug, Clone)]
pub struct UsePath {
    pub segs: Vec<String>,
    /// The offset of its `use` in the file, which decides the module the path starts from.
    pub at: usize,
    /// A `pub use` (of any reach), which a glob over its module passes on.
    pub is_pub: bool,
}

/// An inline `mod name { … }`, by the offsets of its braces.
#[derive(Debug, Clone)]
pub struct ModBlock {
    pub name: String,
    pub open: usize,
    pub close: usize,
    /// The index of the inline module it is declared in.
    pub parent: Option<usize>,
}

#[derive(Debug, Default)]
pub struct Uses {
    /// Every identifier in the file's project `use` trees — not std's.
    pub names: HashSet<String>,
    /// `X` of each `use …::X::*` (`super` for `use super::*`).
    pub globs: HashSet<String>,
    /// Names the file imports from outside the project.
    pub external: HashSet<String>,
    /// The items the file's project `use`s bind — their leaves.
    pub bound: HashSet<String>,
    /// Each bound item → the first segment of every path that binds it.
    pub bound_from: HashMap<String, HashSet<String>>,
    /// Each name a `use` binds → the paths that bind it.
    pub paths: HashMap<String, Vec<UsePath>>,
    /// The modules its `use … ::*` globs bring in, as paths.
    pub glob_paths: Vec<UsePath>,
    /// The modules the file declares inline, in source order.
    pub blocks: Vec<ModBlock>,
    /// Modules declared with a `#[path = "…"]` of their own.
    pub path_mods: HashSet<String>,
    /// Whether every brace closes, so `blocks` can be trusted.
    pub balanced: bool,
}

/// Each `use … ;` statement of blanked text: its offset and its tree.
pub fn use_statements(code: &str) -> Vec<(usize, &str)> {
    let b = code.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = super::scopes::find(b, b"use", from) {
        from = i + 3;
        let ok_before = i == 0 || matches!(b[i - 1], b';' | b'{' | b'}') || b[i - 1].is_ascii_whitespace();
        if !ok_before || !b.get(i + 3).is_some_and(|c| c.is_ascii_whitespace()) {
            continue;
        }
        let Some(semi) = code[i..].find(';') else { continue };
        let tree = &code[i + 3..i + semi];
        if tree.len() > 2000 || tree.trim().is_empty() {
            continue;
        }
        out.push((i, tree));
    }
    out
}

/// What one `use` tree binds, by the path each binding names: `crate::sync::{
/// watch::{self, Receiver as Rx}, *}` binds `watch` and `Rx`, and globs
/// `crate::sync` (local none).
pub fn use_tree_bindings(tree: &str) -> Vec<(Vec<String>, Option<String>)> {
    let mut tokens: Vec<String> = Vec::new();
    let b = tree.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        if c == b':' && b.get(i + 1) == Some(&b':') {
            tokens.push("::".into());
            i += 2;
        } else if matches!(c, b'{' | b'}' | b',' | b'*') {
            tokens.push((c as char).to_string());
            i += 1;
        } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' || (c == b'r' && b.get(i + 1) == Some(&b'#')) {
            let start = i;
            if c == b'r' && b.get(i + 1) == Some(&b'#') {
                i += 2;
            } else if c == b'$' {
                i += 1;
            }
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_') {
                i += 1;
            }
            tokens.push(tree[start..i].to_string());
        } else {
            i += 1;
        }
    }
    let mut out = Vec::new();
    let mut i = 0;
    fn read(tokens: &[String], i: &mut usize, prefix: &[String], out: &mut Vec<(Vec<String>, Option<String>)>) {
        let mut segs: Vec<String> = prefix.to_vec();
        while *i < tokens.len() {
            let t = tokens[*i].as_str();
            if t == "::" {
                *i += 1;
                continue;
            }
            if t == "*" {
                *i += 1;
                out.push((segs, None));
                return;
            }
            if t == "{" {
                *i += 1;
                while *i < tokens.len() && tokens[*i] != "}" {
                    let before = *i;
                    read(tokens, i, &segs, out);
                    if tokens.get(*i).is_some_and(|t| t == ",") || *i == before {
                        *i += 1;
                    }
                }
                *i += 1;
                return;
            }
            if t == "," || t == "}" || (t == "as" && segs.len() > prefix.len()) {
                break;
            }
            segs.push(t.strip_prefix("r#").unwrap_or(t).to_string());
            *i += 1;
            if tokens.get(*i).is_none_or(|t| t != "::") {
                break;
            }
        }
        if segs.len() == prefix.len() {
            return;
        }
        let mut local = None;
        if tokens.get(*i).is_some_and(|t| t == "as") {
            local = tokens.get(*i + 1).cloned();
            *i += 2;
        }
        if segs.last().is_some_and(|s| s == "self") {
            segs.pop();
        }
        let local = local.or_else(|| segs.last().cloned());
        if let Some(l) = local {
            if !segs.is_empty() && l != "_" && is_ident(&l) {
                out.push((segs, Some(l)));
            }
        }
    }
    while i < tokens.len() {
        let before = i;
        read(&tokens, &mut i, &[], &mut out);
        if i == before {
            i += 1;
        }
    }
    out
}

pub fn is_ident(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The leaves a `use` tree binds (`as` aliases by their alias), and `io` of `io::{self, …}`.
fn leaves(tree: &str) -> Vec<String> {
    let mut out = Vec::new();
    for (segs, local) in use_tree_bindings(tree) {
        let Some(l) = local else { continue };
        // `use a::b as c` binds `c`, and upstream counts the `b` it renames too.
        if let Some(last) = segs.last() {
            if *last != l && last != "self" {
                out.push(last.clone());
            }
        }
        out.push(l);
    }
    out
}

/// The inline `mod name { … }` blocks of blanked text, in source order.
fn mod_blocks(code: &str) -> (Vec<ModBlock>, bool) {
    let b = code.as_bytes();
    let mut heads: HashMap<usize, String> = HashMap::new();
    let mut from = 0;
    while let Some(i) = super::scopes::find(b, b"mod", from) {
        from = i + 3;
        if !super::scopes::word_at(b, i, "mod") {
            continue;
        }
        let mut p = super::scopes::skip_space(b, i + 3);
        if b[p..].starts_with(b"r#") {
            p += 2;
        }
        let Some(name) = super::scopes::ident_at(code, p) else { continue };
        let q = super::scopes::skip_space(b, p + name.len());
        if b.get(q) == Some(&b'{') {
            heads.insert(q, name.to_string());
        }
    }
    let mut blocks: Vec<ModBlock> = Vec::new();
    if heads.is_empty() {
        return (blocks, true);
    }
    let mut open: Vec<Option<usize>> = Vec::new();
    let mut inner: Option<usize> = None;
    let mut balanced = true;
    for (at, &c) in b.iter().enumerate() {
        if c == b'{' {
            let block = heads.get(&at).map(|name| {
                blocks.push(ModBlock {
                    name: name.clone(),
                    open: at,
                    close: b.len(),
                    parent: inner,
                });
                blocks.len() - 1
            });
            if block.is_some() {
                inner = block;
            }
            open.push(block);
        } else if c == b'}' {
            match open.pop() {
                None => balanced = false,
                Some(Some(k)) => {
                    blocks[k].close = at;
                    inner = blocks[k].parent;
                }
                Some(None) => {}
            }
        }
    }
    (blocks, balanced && open.is_empty())
}

/// The innermost inline module whose braces hold `offset`.
pub fn block_at(blocks: &[ModBlock], offset: usize) -> Option<usize> {
    let mut at = None;
    for (i, b) in blocks.iter().enumerate() {
        if b.open > offset {
            break;
        }
        if offset < b.close {
            at = Some(i);
        }
    }
    at
}

impl Uses {
    pub fn read(ctx: &Ctx<'_>, file: &str) -> Uses {
        let mut uses = Uses {
            balanced: true,
            ..Default::default()
        };
        let Some(scopes) = ctx.scopes(file) else { return uses };
        let code = scopes.code.as_str();
        let deps = &ctx.graph().dependencies;
        for (at, tree) in use_statements(code) {
            let t = tree.trim_start().trim_start_matches("::");
            let root = super::scopes::ident_at(t, 0).unwrap_or("");
            let outside = STD_ROOTS.contains(&root) || (!root.is_empty() && deps.contains(root));
            let before = code[at.saturating_sub(48)..at].trim_end();
            let is_pub = ends_with_pub(before);
            for (segs, local) in use_tree_bindings(tree) {
                if segs.is_empty() || segs[0].starts_with('$') || STD_ROOTS.contains(&segs[0].as_str()) {
                    continue;
                }
                let path = UsePath { segs, at, is_pub };
                match local {
                    None => uses.glob_paths.push(path),
                    Some(l) => uses.paths.entry(l).or_default().push(path),
                }
            }
            let leaves = leaves(tree);
            if outside {
                uses.external.extend(leaves);
                continue;
            }
            for id in leaves {
                uses.bound.insert(id.clone());
                uses.bound_from.entry(id).or_default().insert(root.to_string());
            }
            for id in idents(tree) {
                uses.names.insert(id.to_string());
            }
            // `(\w+)\s*::\s*(?:\{[^}]*)?\*`
            let tb = tree.as_bytes();
            for (i, &c) in tb.iter().enumerate() {
                if c != b'*' {
                    continue;
                }
                let mut j = i;
                // back over `{ … ` (no `}` between)
                let head = &tree[..j];
                let brace = head.rfind('{');
                let close = head.rfind('}');
                if let Some(bpos) = brace.filter(|&bp| close.is_none_or(|c| c < bp)) {
                    let between = &tree[bpos + 1..i];
                    if !between.contains('}') {
                        j = bpos;
                    }
                }
                let h = tree[..j].trim_end();
                let Some(h) = h.strip_suffix("::") else { continue };
                let h = h.trim_end();
                let start = h.rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).map_or(0, |p| p + 1);
                if start < h.len() {
                    uses.globs.insert(h[start..].to_string());
                }
            }
        }
        let (blocks, balanced) = mod_blocks(code);
        uses.blocks = blocks;
        uses.balanced = balanced;
        // `#[path = "…"] mod imp;`, `#[cfg_attr(docsrs, path = "…")] pub mod de;`
        let source = ctx.source(file).unwrap_or_default();
        for name in path_mods(&source) {
            uses.path_mods.insert(name);
        }
        uses
    }
}

fn ends_with_pub(text: &str) -> bool {
    let t = text.trim_end();
    let t = if t.ends_with(')') {
        match t.rfind('(') {
            Some(o) => t[..o].trim_end(),
            None => t,
        }
    } else {
        t
    };
    t.ends_with("pub") && !t[..t.len() - 3].ends_with(|c: char| c.is_alphanumeric() || c == '_')
}

fn idents(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|s| s.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
}

/// Modules a file declares with a `#[path = …]` attribute of their own.
fn path_mods(source: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(i) = source[from..].find("#[") {
        let at = from + i;
        from = at + 2;
        let Some(end) = source[at..].find(']') else { break };
        let attr = &source[at + 2..at + end];
        let a = attr.trim_start();
        let is_path = (a.starts_with("path") && a[4..].trim_start().starts_with('='))
            || (a.starts_with("cfg_attr") && attr.contains("path") && attr.contains('='));
        if !is_path {
            continue;
        }
        // Past further attributes and `pub(…)`, to `mod name`.
        let mut rest = source[at + end + 1..].trim_start();
        while rest.starts_with("#[") || rest.starts_with("#![") {
            match rest.find(']') {
                Some(e) => rest = rest[e + 1..].trim_start(),
                None => break,
            }
        }
        if let Some(r) = rest.strip_prefix("pub") {
            rest = r.trim_start();
            if rest.starts_with('(') {
                rest = rest.find(')').map_or(rest, |e| rest[e + 1..].trim_start());
            }
        }
        if let Some(r) = rest.strip_prefix("mod") {
            let r = r.trim_start().trim_start_matches("r#");
            if let Some(name) = super::scopes::ident_at(r, 0) {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// A Rust module, by where its items are written.
#[derive(Debug, Clone, PartialEq)]
pub struct Module {
    /// The file its items are written in.
    pub file: String,
    /// Where the files of the modules it declares live.
    pub dir: String,
    /// Its block (index into its file's `Uses::blocks`), for a module declared inline.
    pub block: Option<usize>,
    /// A crate's root, which nothing is above.
    pub root: bool,
    /// The package's other root, which holds crate-root items too.
    pub alt: Option<String>,
}

pub fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

pub fn parent_dir(p: &str) -> &str {
    p.rfind('/').map_or("", |i| &p[..i])
}

/// The module directory a Rust file's private items are visible from.
pub fn module_dir(file: &str) -> String {
    let base = file.rsplit('/').next().unwrap_or(file);
    let dir = parent_dir(file);
    if matches!(base, "mod.rs" | "lib.rs" | "main.rs") {
        dir.to_string()
    } else {
        join(dir, base.trim_end_matches(".rs"))
    }
}

/// The module a Rust file is: `src/glob.rs` → `glob`, `src/walk/mod.rs` → `walk`.
pub fn module_name(file: &str) -> &str {
    let mut parts = file.rsplit('/');
    let base = parts.next().unwrap_or(file).trim_end_matches(".rs");
    if matches!(base, "mod" | "lib" | "main") {
        parts.next().unwrap_or(base)
    } else {
        base
    }
}

/// The module path of a Rust file in its crate: `crates/cli/src/net/config.rs` → `net`, `config`.
pub fn module_path(file: &str) -> Vec<String> {
    let parts: Vec<&str> = file.split('/').collect();
    let mut at: isize = -1;
    for i in (0..parts.len().saturating_sub(1)).rev() {
        if matches!(parts[i], "src" | "tests" | "examples" | "benches") {
            at = i as isize;
            break;
        }
    }
    let mut modules: Vec<String> = parts[(at + 1) as usize..].iter().map(|s| s.to_string()).collect();
    let last = modules.pop().unwrap_or_default();
    let last = last.trim_end_matches(".rs");
    if !matches!(last, "mod" | "lib" | "main") {
        modules.push(last.to_string());
    }
    modules
}

/// Whether the modules a path walks are, in order, among those of the file declaring `n`.
pub fn modules_lead(modules: &[String], file: &str) -> bool {
    let mut k = 0;
    for m in module_path(file) {
        if k < modules.len() && m == modules[k] {
            k += 1;
        }
    }
    k == modules.len()
}

impl<'a> Ctx<'a> {
    /// Whether a Rust file is a Cargo target's root.
    pub fn is_target_root(&self, file: &str) -> bool {
        let g = self.graph();
        let Some(dir) = g.crate_dir_of(file) else {
            return file.ends_with("/lib.rs") || file.ends_with("/main.rs") || file == "lib.rs" || file == "main.rs";
        };
        let rel = if dir.is_empty() { file } else { &file[dir.len() + 1..] };
        if rel == "src/lib.rs" || rel == "src/main.rs" || rel == "build.rs" {
            return true;
        }
        let parts: Vec<&str> = rel.split('/').collect();
        match parts.as_slice() {
            ["src", "bin", f] => f.ends_with(".rs"),
            ["src", "bin", _, "main.rs"] => true,
            [t, f] if matches!(*t, "tests" | "examples" | "benches") => f.ends_with(".rs"),
            [t, _, "main.rs"] if matches!(*t, "tests" | "examples" | "benches") => true,
            _ => false,
        }
    }

    /// The module a Rust file is, at its own top level.
    pub fn file_module(&self, file: &str) -> Module {
        let root = self.is_target_root(file);
        let dir = if root || file.ends_with("/mod.rs") || file == "mod.rs" {
            parent_dir(file).to_string()
        } else {
            file.trim_end_matches(".rs").to_string()
        };
        Module {
            file: file.to_string(),
            dir,
            block: None,
            root,
            alt: None,
        }
    }

    /// The module code at `offset` in `file` is written in: the file's, or the inline one around it.
    pub fn module_at(&self, file: &str, offset: usize) -> Module {
        let own = self.file_module(file);
        let uses = self.uses(file);
        let Some(block) = block_at(&uses.blocks, offset) else { return own };
        let mut names = Vec::new();
        let mut b = Some(block);
        while let Some(k) = b {
            names.push(uses.blocks[k].name.clone());
            b = uses.blocks[k].parent;
        }
        let mut dir = own.dir;
        for n in names.iter().rev() {
            dir = join(&dir, n);
        }
        Module {
            file: file.to_string(),
            dir,
            block: Some(block),
            root: false,
            alt: None,
        }
    }

    /// The root of the crate `file` is in — what its `crate::` names.
    pub fn crate_root_module(&self, file: &str) -> Option<Module> {
        let g = self.graph();
        let dir = g.crate_dir_of(file)?;
        let base = if dir.is_empty() { String::new() } else { format!("{dir}/") };
        let rel = &file[base.len()..];
        let parts: Vec<&str> = rel.split('/').collect();
        let target = match parts.as_slice() {
            ["src", "bin", rest @ ..] if !rest.is_empty() => Some(("src/bin", rest)),
            [t, rest @ ..] if matches!(*t, "tests" | "examples" | "benches") && !rest.is_empty() => Some((*t, rest)),
            _ => None,
        };
        if let Some((kind, rest)) = target {
            let file = if rest.len() == 1 && rest[0].ends_with(".rs") {
                file.to_string()
            } else {
                format!("{base}{kind}/{}/main.rs", rest[0])
            };
            return self.file_exists(&file).then(|| Module {
                dir: parent_dir(&file).to_string(),
                file,
                block: None,
                root: true,
                alt: None,
            });
        }
        if !rel.starts_with("src/") {
            return None;
        }
        let lib = format!("{base}src/lib.rs");
        let main = format!("{base}src/main.rs");
        let (has_lib, has_main) = (self.file_exists(&lib), self.file_exists(&main));
        if !has_lib && !has_main {
            return None;
        }
        let f = if has_main && (file == main || !has_lib) { main.clone() } else { lib.clone() };
        let alt = (has_lib && has_main).then(|| if f == lib { main } else { lib });
        Some(Module {
            file: f,
            dir: format!("{base}src"),
            block: None,
            root: true,
            alt,
        })
    }

    /// The root of a project crate's library.
    pub fn lib_module(&self, crate_dir: &str) -> Option<Module> {
        let file = if crate_dir.is_empty() { "src/lib.rs".to_string() } else { format!("{crate_dir}/src/lib.rs") };
        self.file_exists(&file).then(|| Module {
            dir: parent_dir(&file).to_string(),
            file,
            block: None,
            root: true,
            alt: None,
        })
    }

    /// The module `name` that `m` declares: inline, else `<name>.rs` or
    /// `<name>/mod.rs` in its directory, else one it passes on under that name.
    pub fn submodule(&self, m: &Module, name: &str, depth: u8) -> Option<Module> {
        let dir = join(&m.dir, name);
        let layout = self.uses(&m.file);
        if layout.path_mods.contains(name) {
            return None;
        }
        if let Some(k) = layout.blocks.iter().position(|b| b.name == name && b.parent == m.block) {
            return Some(Module {
                file: m.file.clone(),
                dir,
                block: Some(k),
                root: false,
                alt: None,
            });
        }
        for file in [format!("{dir}.rs"), format!("{dir}/mod.rs")] {
            if self.file_exists(&file) {
                return Some(Module {
                    file,
                    dir,
                    block: None,
                    root: false,
                    alt: None,
                });
            }
        }
        if depth >= 8 {
            return None;
        }
        for p in layout.paths.get(name).map(Vec::as_slice).unwrap_or(&[]) {
            if block_at(&layout.blocks, p.at) != m.block {
                continue;
            }
            if let Walk::Module(passed) = self.walk_modules(&p.segs, m, depth + 1) {
                return Some(passed);
            }
        }
        None
    }

    /// The module `m` is declared in — what `super::` names.
    pub fn super_module(&self, m: &Module) -> Option<Module> {
        if let Some(k) = m.block {
            let parent = self.uses(&m.file).blocks[k].parent;
            return Some(match parent {
                Some(p) => Module {
                    file: m.file.clone(),
                    dir: parent_dir(&m.dir).to_string(),
                    block: Some(p),
                    root: false,
                    alt: None,
                },
                None => self.file_module(&m.file),
            });
        }
        if m.root {
            return None;
        }
        let dir = parent_dir(&m.dir);
        let mut files = vec![join(dir, "mod.rs")];
        if !dir.is_empty() {
            files.push(format!("{dir}.rs"));
        }
        files.push(join(dir, "lib.rs"));
        files.push(join(dir, "main.rs"));
        files.into_iter().find(|f| self.file_exists(f)).map(|f| self.file_module(&f))
    }

    /// The module the segments of a `use` path walk to from `from`.
    pub fn walk_modules(&self, segs: &[String], from: &Module, depth: u8) -> Walk {
        let Some((first, rest)) = segs.split_first() else {
            return Walk::Module(from.clone());
        };
        let mut at = match first.as_str() {
            "crate" => self.crate_root_module(&from.file),
            "self" => Some(from.clone()),
            "super" => self.super_module(from),
            f if STD_ROOTS.contains(&f) => return Walk::Outside,
            f => {
                let sub = self.submodule(from, f, depth);
                if sub.is_some() {
                    sub
                } else {
                    let g = self.graph();
                    let own = g.crate_dir_of(&from.file);
                    let krate = own.and_then(|o| g.crates.get(o));
                    let dep = krate.and_then(|c| {
                        if c.name == *f {
                            own.map(str::to_string)
                        } else {
                            c.keys.get(f).cloned()
                        }
                    });
                    match dep {
                        Some(d) => self.lib_module(&d),
                        None if g.dependencies.contains(f) => return Walk::Outside,
                        None => None,
                    }
                }
            }
        };
        for seg in rest {
            let Some(m) = at else { return Walk::Unknown };
            at = match seg.as_str() {
                "self" => Some(m),
                "super" => self.super_module(&m),
                s => self.submodule(&m, s, depth),
            };
        }
        match at {
            Some(m) => Walk::Module(m),
            None => Walk::Unknown,
        }
    }

    /// Where a symbol sits, as an offset its file's `Uses::blocks` read.
    fn item_offset(&self, id: SymId) -> usize {
        self.node_offset(id)
    }

    /// The items `m` declares as `name` at its own level.
    pub fn declared_items(&self, m: &Module, name: &str, public_only: bool) -> Vec<SymId> {
        let mut out = Vec::new();
        let mut files = vec![m.file.clone()];
        if let Some(a) = &m.alt {
            files.push(a.clone());
        }
        for file in files {
            let layout = self.uses(&file);
            let block = if file == m.file { m.block } else { None };
            for &i in self.syms.in_file(&file) {
                let s = self.syms.get(i);
                if s.name != name
                    || s.language != codegraph_core::Language::Rust
                    || s.qualified_name != name
                    || !MODULE_ITEM_KINDS.contains(&s.kind)
                {
                    continue;
                }
                if public_only && s.visibility == Some(Visibility::Private) {
                    continue;
                }
                if block_at(&layout.blocks, self.item_offset(i)) == block {
                    out.push(i);
                }
            }
        }
        out
    }

    /// The items named `name` in module `m`, as Rust looks a name up in a
    /// module. None when one of them can't be followed.
    pub fn module_items(&self, m: &Module, name: &str, seen: &mut HashSet<String>, public_only: bool) -> Option<Vec<SymId>> {
        let key = format!("{}\0{:?}\0{name}\0{public_only}", m.file, m.block);
        if seen.contains(&key) {
            return Some(Vec::new());
        }
        if seen.len() >= 32 {
            return None;
        }
        let uses = self.uses(&m.file);
        if !uses.balanced {
            return None;
        }
        seen.insert(key.clone());
        let out = (|| {
            let declared = self.declared_items(m, name, public_only);
            if !declared.is_empty() {
                return Some(declared);
            }
            let here = |p: &UsePath| (p.is_pub || !public_only) && block_at(&uses.blocks, p.at) == m.block;
            let mut found = Vec::new();
            for p in uses.paths.get(name).map(Vec::as_slice).unwrap_or(&[]).iter().filter(|p| here(p)) {
                found.extend(self.use_path_items(&p.segs, m, seen)?);
            }
            // A `use` of the name shadows the globs — in its own namespace.
            if !found.is_empty() {
                return Some(found);
            }
            let globs: Vec<UsePath> = uses.glob_paths.iter().filter(|p| here(p)).cloned().collect();
            self.globbed_items(&globs, &|_| m.clone(), name, seen)
        })();
        seen.remove(&key);
        out
    }

    /// The items named `name` that globs bring in, each walked from the module `from_of` gives for it.
    pub fn globbed_items(
        &self,
        globs: &[UsePath],
        from_of: &dyn Fn(&UsePath) -> Module,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> Option<Vec<SymId>> {
        let mut found = Vec::new();
        let mut unknown = false;
        for g in globs {
            let from = from_of(g);
            match self.walk_modules(&g.segs, &from, 0) {
                Walk::Outside => continue,
                Walk::Unknown => unknown = true,
                Walk::Module(m) => {
                    let within = self.module_within(&from, &m);
                    match self.module_items(&m, name, seen, !within) {
                        Some(items) => found.extend(items),
                        None => unknown = true,
                    }
                }
            }
        }
        (!found.is_empty() || !unknown).then_some(found)
    }

    /// Whether module `inner` is `outer` or one declared inside it.
    pub fn module_within(&self, inner: &Module, outer: &Module) -> bool {
        if inner.file == outer.file {
            let blocks = &self.uses(&inner.file).blocks;
            let mut b = inner.block;
            loop {
                if b == outer.block {
                    return true;
                }
                match b {
                    None => return false,
                    Some(k) => b = blocks[k].parent,
                }
            }
        }
        let prefix = if outer.dir.is_empty() { String::new() } else { format!("{}/", outer.dir) };
        inner.file.starts_with(&prefix) && !(outer.dir.is_empty() && outer.block.is_some())
    }

    /// The items a `use` path names from `from`: its last segment's, in the module the rest walks to.
    pub fn use_path_items(&self, segs: &[String], from: &Module, seen: &mut HashSet<String>) -> Option<Vec<SymId>> {
        if segs.len() < 2 {
            return None;
        }
        match self.walk_modules(&segs[..segs.len() - 1], from, 0) {
            Walk::Outside => Some(Vec::new()),
            Walk::Unknown => None,
            Walk::Module(m) => self.module_items(&m, &segs[segs.len() - 1], seen, false),
        }
    }

    /// The ids of what the `use`s that bind `name` in `file` name, through the project's modules.
    pub fn use_items(&self, name: &str, file: &str) -> Option<Rc<HashSet<SymId>>> {
        self.memo(1, format!("{file}\0{name}"), || {
            let uses = self.uses(file);
            let paths = if uses.balanced { uses.paths.get(name)? } else { return None };
            let mut ids = HashSet::new();
            for p in paths {
                let items = self.use_path_items(&p.segs, &self.module_at(file, p.at), &mut HashSet::new())?;
                ids.extend(items);
            }
            Some(Rc::new(ids))
        })
    }

    /// The ids of the items named `name` that the globs of `file` bring in.
    pub fn glob_items(&self, name: &str, file: &str) -> Option<Rc<HashSet<SymId>>> {
        self.memo(2, format!("{file}\0{name}"), || {
            let uses = self.uses(file);
            if !uses.balanced || uses.glob_paths.is_empty() {
                return None;
            }
            let items = self.globbed_items(&uses.glob_paths, &|g| self.module_at(file, g.at), name, &mut HashSet::new())?;
            Some(Rc::new(items.into_iter().collect()))
        })
    }

    /// Whether one of the file's globs brings this candidate in.
    pub fn glob_scope(&self, name: &str, cand: SymId, uses: &Uses, file: &str) -> bool {
        let ids = if MODULE_ITEM_KINDS.contains(&self.syms.get(cand).kind) {
            self.glob_items(name, file)
        } else {
            None
        };
        match ids {
            None => glob_covers(uses, &self.syms.get(cand).file, file),
            Some(ids) => ids.contains(&cand),
        }
    }

    /// The ids of the items a path names from the module around `at` in `file`.
    pub fn walked_items(&self, segs: &[String], file: &str, at: Option<usize>) -> Option<Rc<HashSet<SymId>>> {
        let from = self.module_at(file, at.unwrap_or(0));
        self.memo(3, format!("{file}\0{:?}\0{}", from.block, segs.join("::")), || {
            self.use_path_items(segs, &from, &mut HashSet::new())
                .map(|v| Rc::new(v.into_iter().collect()))
        })
    }

    /// Whether a symbol is declared directly in an inline `mod <name> { … }` of its own file.
    pub fn is_in_inline_module(&self, cand: SymId, name: &str) -> bool {
        let file = self.syms.get(cand).file.clone();
        let layout = self.uses(&file);
        layout.balanced && block_at(&layout.blocks, self.item_offset(cand)).is_some_and(|k| layout.blocks[k].name == name)
    }

    /// Whether `ref`'s file can name this Rust item by its visibility.
    pub fn item_visible_from(&self, cand: SymId, file: &str) -> bool {
        let s = self.syms.get(cand);
        if s.language != codegraph_core::Language::Rust
            || s.visibility != Some(Visibility::Private)
            || !MODULE_ITEM_KINDS.contains(&s.kind)
        {
            return true;
        }
        s.file == file || file.starts_with(&format!("{}/", module_dir(&s.file)))
    }

    /// Whether a bare (or path-written) Rust name can mean this candidate: upstream's `isRustNameInScope`.
    pub fn name_in_scope(&self, cand: SymId, r: &Ref<'_>) -> bool {
        if !self.item_visible_from(cand, r.file) {
            return false;
        }
        let c = self.syms.get(cand);
        let name = r.name;
        let line = self.line(r.file, r.line);
        let col = r.column as usize;
        if r.kind == ReferenceKind::References {
            if let Some(l) = &line {
                if is_assoc_binding(l, col, name) {
                    return false;
                }
            }
        }
        let at = line.as_deref().map_or("", |l| l.get(col..).unwrap_or(""));
        let type_read = r.kind == ReferenceKind::References && is_ident(name);
        let via_self = type_read && name != "Self" && strip_ws_prefix(at, "Self").is_some_and(|r| r.trim_start().starts_with("::"));
        if type_read && (via_self || (at.starts_with(name) && !at[name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_'))) {
            if let Some(variant) = variant_after(at) {
                if !self.declares_variant(cand, &variant) {
                    return false;
                }
            }
        }
        let pathed = match &line {
            None => None,
            Some(_) if via_self => None,
            Some(l) => written_path(l, col, name),
        };
        if let Some((prefix, seg)) = pathed {
            let root = prefix
                .split("::")
                .map(str::trim)
                .find(|s| !s.is_empty())
                .unwrap_or(seg.as_str())
                .to_string();
            if r.kind == ReferenceKind::References && prefix.trim().is_empty() && line.is_some() {
                if seg == "Self" {
                    return c.kind == NodeKind::TypeAlias && self.is_in_enclosing_impl(cand, r);
                }
                if self.is_generic_param(&seg, r) {
                    return false;
                }
            }
            if is_associated_item(c) {
                if seg == "Self" {
                    return self.is_in_enclosing_impl(cand, r);
                }
                if !seg.is_empty() || line.is_none() {
                    return false;
                }
                let l = line.as_deref().unwrap_or("");
                let Some((qself, trait_)) = qualified_self(&l[..col.min(l.len())]) else {
                    return true;
                };
                if qself == "Self" {
                    return self.is_in_enclosing_impl(cand, r);
                }
                if self.is_generic_param(&qself, r) || c.qualified_name != format!("{qself}::{}", c.name) {
                    return false;
                }
                return trait_.is_none_or(|t| self.trait_impl_header(cand).is_some_and(|h| h.trait_ == t));
            }
            let g = self.graph();
            let path_uses = self.uses(r.file);
            if (STD_ROOTS[..3].contains(&root.as_str()) || g.dependencies.contains(&root))
                && !g.project.contains(&root)
                && !path_uses.bound.contains(&root)
            {
                return false;
            }
            if seg.is_empty() || matches!(seg.as_str(), "crate" | "self" | "super" | "Self") || g.project.contains(&seg) {
                return true;
            }
            if path_uses.external.contains(&seg) && !path_uses.bound.contains(&seg) {
                return false;
            }
            if let Some(from) = path_uses.bound_from.get(&root) {
                if !g.use_can_reach(from.iter().map(String::as_str), &c.file, r.file) {
                    return false;
                }
            }
            return module_name(&c.file) == seg
                || c.file.contains(&format!("/{seg}/"))
                || c.qualified_name.split("::").any(|s| s == seg)
                || (c.file == r.file && self.is_in_inline_module(cand, &seg));
        }
        if is_associated_item(c) {
            return false;
        }
        if c.kind == NodeKind::EnumMember {
            if r.kind == ReferenceKind::References {
                return false;
            }
            let uses = self.uses(r.file);
            let owner = c.owner().and_then(|o| o.rsplit("::").next()).unwrap_or("");
            return (!owner.is_empty() && uses.globs.contains(owner)) || (uses.names.contains(name) && uses.names.contains(owner));
        }
        if c.file == r.file {
            return true;
        }
        let uses = self.uses(r.file);
        let from = if MODULE_ITEM_KINDS.contains(&c.kind) { uses.bound_from.get(name) } else { None };
        if let Some(from) = from {
            if !self.graph().use_can_reach(from.iter().map(String::as_str), &c.file, r.file) {
                return false;
            }
            if self.use_items(name, r.file).is_some_and(|ids| !ids.contains(&cand)) {
                return false;
            }
        }
        if uses.external.contains(name) && !uses.bound.contains(name) {
            if let Some(l) = &line {
                let tail = l.get(col..).unwrap_or("");
                if let Some(p) = find_word(tail, name) {
                    if !l[..col + p].trim_end().ends_with('.') {
                        return false;
                    }
                }
            }
        }
        if !PRELUDE.contains(&name) {
            if matches!(c.kind, NodeKind::Method | NodeKind::Property | NodeKind::Field | NodeKind::EnumMember)
                || r.kind == ReferenceKind::Imports
                || matches!(c.kind, NodeKind::File | NodeKind::Module | NodeKind::Namespace)
            {
                return true;
            }
            return uses.bound.contains(name) || self.glob_scope(name, cand, &uses, r.file);
        }
        uses.names.contains(name) || self.glob_scope(name, cand, &uses, r.file)
    }

    /// Whether `ty` is a Rust enum that declares `variant`.
    pub fn declares_variant(&self, ty: SymId, variant: &str) -> bool {
        let t = self.syms.get(ty);
        if t.kind != NodeKind::Enum {
            return false;
        }
        let q = format!("{}::{variant}", t.qualified_name);
        self.syms.in_file(&t.file).iter().any(|&i| {
            let s = self.syms.get(i);
            s.kind == NodeKind::EnumMember && s.qualified_name == q
        })
    }

    /// The line of the `impl` / `trait` header at or above `line` of `file` (0 for none).
    pub fn impl_line_at(&self, file: &str, line: u32) -> u32 {
        let Some(src) = self.source(file) else { return 0 };
        let lines: Vec<&str> = src.lines().collect();
        let mut i = (line as usize).min(lines.len());
        while i > 0 {
            i -= 1;
            if is_impl_or_trait_line(lines[i]) {
                return i as u32 + 1;
            }
        }
        0
    }

    pub fn enclosing_impl_line(&self, r: &Ref<'_>) -> u32 {
        self.impl_line_at(r.file, r.line)
    }

    /// Whether a symbol is declared in the same `impl` / `trait` block as `r`, above it.
    pub fn is_in_enclosing_impl(&self, cand: SymId, r: &Ref<'_>) -> bool {
        let header = self.enclosing_impl_line(r);
        let c = self.syms.get(cand);
        header > 0 && c.file == r.file && c.start_line >= header && c.start_line <= r.line
    }

    /// Whether `name` is a generic type parameter of the function or impl around `r`.
    pub fn is_generic_param(&self, name: &str, r: &Ref<'_>) -> bool {
        if !name.starts_with(|c: char| c.is_ascii_uppercase()) || !is_ident(name) {
            return false;
        }
        let Some(src) = self.source(r.file) else { return false };
        let lines: Vec<&str> = src.lines().collect();
        let from = (self.enclosing_impl_line(r) as usize).saturating_sub(1);
        let to = (r.line as usize).min(lines.len());
        if from >= to {
            return false;
        }
        let text = lines[from..to].join("\n");
        generic_list_declares(&text, name)
    }
}

/// The result of walking a module path.
#[derive(Debug, Clone)]
pub enum Walk {
    Module(Module),
    /// The standard library or a crate from outside the project.
    Outside,
    /// A step found no module the project holds.
    Unknown,
}

/// Does one of the file's globs bring in this candidate's module (or, for `use super::*`, its parent's)?
pub fn glob_covers(uses: &Uses, cand_file: &str, ref_file: &str) -> bool {
    if uses.globs.contains(module_name(cand_file)) {
        return true;
    }
    if !uses.globs.contains("super") {
        return false;
    }
    let dir = |p: &str| parent_dir(p).to_string();
    dir(cand_file) == dir(ref_file) || dir(cand_file) == dir(&dir(ref_file))
}

pub fn is_impl_or_trait_line(line: &str) -> bool {
    let mut t = line.trim_start();
    if let Some(r) = t.strip_prefix("pub") {
        let r2 = r.trim_start();
        if r2.starts_with('(') {
            if let Some(e) = r2.find(')') {
                t = r2[e + 1..].trim_start();
            }
        } else if r.starts_with(char::is_whitespace) {
            t = r2;
        }
    }
    if let Some(r) = t.strip_prefix("unsafe") {
        if r.starts_with(char::is_whitespace) {
            t = r.trim_start();
        }
    }
    starts_with_word(t, "impl") || starts_with_word(t, "trait")
}

pub fn starts_with_word(t: &str, w: &str) -> bool {
    t.starts_with(w) && !t[w.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
}

fn strip_ws_prefix<'s>(t: &'s str, p: &str) -> Option<&'s str> {
    t.strip_prefix(p)
}

/// `^\w+\s*::\s*([A-Z]\w*)` of the text at a reference.
fn variant_after(at: &str) -> Option<String> {
    let end = at.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    if end == 0 {
        return None;
    }
    let rest = at[end..].trim_start().strip_prefix("::")?.trim_start();
    if !rest.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    let e = rest.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len());
    Some(rest[..e].to_string())
}

/// The first whole-word (not after `$`/word char) match of `name` in `text`.
pub fn find_word(text: &str, name: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = text[from..].find(name) {
        let at = from + i;
        from = at + 1;
        let before_ok = at == 0 || !text[..at].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '$');
        let after_ok = !text[at + name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
        if before_ok && after_ok {
            return Some(at);
        }
    }
    None
}

/// The path a line writes before `name` (prefix segments, last segment), if it
/// is written through one: at the reference's column, or wherever the line
/// writes `seg::name` when it never writes `name` bare.
fn written_path(line: &str, col: usize, name: &str) -> Option<(String, String)> {
    let before_col = line.get(..col)?;
    if line.get(col..).is_some_and(|t| t.starts_with(name)) && before_col.trim_end().ends_with("::") {
        return path_before(before_col);
    }
    // A bare occurrence (not after a word char, `$` or `:`) means no path.
    let mut from = 0;
    let mut bare = false;
    while let Some(i) = line[from..].find(name) {
        let at = from + i;
        from = at + 1;
        let before = &line[..at];
        let after_ok = !line[at + name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_');
        if after_ok && !before.ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '$' || c == ':') {
            bare = true;
            break;
        }
    }
    if bare {
        return None;
    }
    let mut from = 0;
    while let Some(i) = line[from..].find(name) {
        let at = from + i;
        from = at + 1;
        if line[at + name.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_') {
            continue;
        }
        let before = &line[..at];
        if before.trim_end().ends_with("::") {
            if let Some(p) = path_before(before) {
                if !p.1.is_empty() {
                    return Some(p);
                }
            }
        }
    }
    None
}

/// `((?:[A-Za-z_]\w*\s*::\s*)*)([A-Za-z_]\w*)?\s*::\s*$` of `text`.
fn path_before(text: &str) -> Option<(String, String)> {
    let t = text.trim_end().strip_suffix("::")?.trim_end();
    let mut segs: Vec<&str> = Vec::new();
    let mut rest = t;
    loop {
        let end = rest.len();
        let start = rest
            .char_indices()
            .rev()
            .take_while(|(_, c)| c.is_ascii_alphanumeric() || *c == '_')
            .last()
            .map(|(i, _)| i);
        let Some(start) = start else { break };
        let id = &rest[start..end];
        if !id.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
            break;
        }
        segs.push(id);
        let r = rest[..start].trim_end();
        match r.strip_suffix("::") {
            Some(r) => rest = r.trim_end(),
            None => break,
        }
    }
    if segs.is_empty() {
        return Some((String::new(), String::new()));
    }
    let seg = segs[0].to_string();
    let prefix: Vec<&str> = segs[1..].iter().rev().copied().collect();
    Some((prefix.join("::"), seg))
}

/// Whether `name` at `column` of `line` is a trait's associated type bound in its arguments.
fn is_assoc_binding(line: &str, column: usize, name: &str) -> bool {
    let Some(after) = line.get(column..).and_then(|t| t.strip_prefix(name)) else { return false };
    let a = after.trim_start();
    let binds = (a.starts_with('=') && !a.starts_with("==") && !a.starts_with("=>")) || (a.starts_with(':') && !a.starts_with("::"));
    if !binds {
        return false;
    }
    let b = line.as_bytes();
    let (mut angles, mut parens) = (0, 0);
    let mut i = column;
    while i > 0 {
        i -= 1;
        match b[i] {
            b'>' if i == 0 || b[i - 1] != b'-' => angles += 1,
            b'<' => {
                if angles == 0 {
                    return parens == 0;
                }
                angles -= 1;
            }
            b')' => parens += 1,
            b'(' => {
                if parens == 0 {
                    return false;
                }
                parens -= 1;
            }
            b'{' | b'}' | b';' => return false,
            _ => {}
        }
    }
    false
}

/// The qualified self a path ends in, written right before `::X`: `<T as Trait<A>>::X` → (`T`, `Trait`).
pub fn qualified_self(before: &str) -> Option<(String, Option<String>)> {
    let text = before.trim_end();
    let text = text.strip_suffix("::").unwrap_or(text).trim_end();
    if !text.ends_with('>') {
        return None;
    }
    let b = text.as_bytes();
    let mut depth = 0;
    let mut open = None;
    let mut i = b.len();
    while i > 0 && open.is_none() {
        i -= 1;
        if b[i] == b'>' && (i == 0 || b[i - 1] != b'-') {
            depth += 1;
        } else if b[i] == b'<' {
            depth -= 1;
            if depth == 0 {
                open = Some(i);
            }
        }
    }
    let open = open?;
    if text[..open].trim_end().ends_with("::") {
        return None;
    }
    let inner = text[open + 1..text.len() - 1].trim();
    let head = |part: &str| -> String {
        let mut p = part.trim();
        loop {
            let before = p;
            if let Some(r) = p.strip_prefix('&') {
                p = r.trim_start();
                if p.starts_with('\'') {
                    p = p.split_once(char::is_whitespace).map_or("", |(_, r)| r).trim_start();
                }
                if let Some(r) = p.strip_prefix("mut ") {
                    p = r.trim_start();
                }
            }
            if let Some(r) = p.strip_prefix("dyn ") {
                p = r.trim_start();
            }
            if p == before {
                break;
            }
        }
        let p = p.split('<').next().unwrap_or(p).trim();
        p.rsplit("::").next().unwrap_or(p).trim().to_string()
    };
    let (s, t) = match find_word(inner, "as") {
        Some(i) => (head(&inner[..i]), Some(head(&inner[i + 2..])).filter(|t| !t.is_empty())),
        None => (head(inner), None),
    };
    (!s.is_empty() && is_ident(&s)).then_some((s, t))
}

/// Whether some `<…>` list in `text` declares `name` (`<…, Name: …>`, `<Name>`, `<Name = …>`).
fn generic_list_declares(text: &str, name: &str) -> bool {
    let b = text.as_bytes();
    let mut from = 0;
    while let Some(i) = find_word(&text[from..], name).map(|i| from + i) {
        from = i + 1;
        let after = text[i + name.len()..].trim_start();
        if !after.starts_with([':', ',', '>', '=']) {
            continue;
        }
        // An unclosed `<` before it (allowing one nesting level).
        let mut depth = 0;
        let mut k = i;
        let mut inside = false;
        while k > 0 {
            k -= 1;
            match b[k] {
                b'>' if k == 0 || b[k - 1] != b'-' => depth += 1,
                b'<' => {
                    if depth == 0 {
                        inside = true;
                        break;
                    }
                    depth -= 1;
                }
                b'\n' if false => {}
                _ => {}
            }
        }
        if inside {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn use_trees_bind_their_leaves() {
        let b = use_tree_bindings(" crate::sync::{watch::{self, Receiver as Rx}, *}");
        let s: Vec<String> = b
            .iter()
            .map(|(p, l)| format!("{}={}", p.join("::"), l.clone().unwrap_or("*".into())))
            .collect();
        assert_eq!(s, vec!["crate::sync::watch=watch", "crate::sync::watch::Receiver=Rx", "crate::sync=*"]);
        let b = use_tree_bindings(" std::io::{self, Read}");
        assert_eq!(b.len(), 2);
    }

    #[test]
    fn reads_paths_and_qualified_selves() {
        assert_eq!(written_path("pub fn f(p: &ast::Pattern)", 18, "Pattern"), Some(("".into(), "ast".into())));
        assert_eq!(
            written_path("x: std::io::Error", 12, "Error"),
            Some(("std".into(), "io".into()))
        );
        assert_eq!(written_path("fn f() -> Pattern", 10, "Pattern"), None);
        assert_eq!(
            qualified_self("fn call(self) -> <Self as Handler>::"),
            Some(("Self".into(), Some("Handler".into())))
        );
        assert_eq!(qualified_self("Vec::<T>::"), None);
        assert!(is_assoc_binding("impl Iterator<Item = &str>", 14, "Item"));
        assert!(!is_assoc_binding("enum Item<T> {", 5, "Item"));
        assert!(generic_list_declares("pub fn handle<L: Listener>(io: <L as Listener>::Io) {}", "L"));
        assert!(!generic_list_declares("pub fn ready(io: <Ready as Listener>::Io) {}", "Ready"));
    }

    #[test]
    fn module_paths() {
        assert_eq!(module_path("crates/cli/src/net/config.rs"), vec!["net", "config"]);
        assert_eq!(module_path("src/lib.rs"), Vec::<String>::new());
        assert_eq!(module_name("src/walk/mod.rs"), "walk");
        assert_eq!(module_dir("src/net.rs"), "src/net");
        assert_eq!(module_dir("src/lib.rs"), "src");
    }
}
