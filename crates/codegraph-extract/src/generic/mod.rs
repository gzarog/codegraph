//! A config-driven tree-sitter walker for the languages upstream extracts
//! with its generic walker (`tree-sitter.ts` + `languages/*.ts`): Pascal,
//! Solidity, Terraform/HCL, Nix, Erlang, Objective-C, CFML and VB.NET.
//!
//! A [`Spec`] names the grammar's node types for each kind of declaration;
//! its [`Hooks`] read what the grammar spells its own way (names, imports,
//! supertypes, call targets) and may take over a node entirely.

mod langs;

use crate::docstring::preceding_docstring;
use crate::ids::{file_node_id, node_id};
use codegraph_core::{
    now_ms, Edge, EdgeKind, ExtractionError, ExtractionResult, Language, Node as GNode, NodeKind,
    ReferenceKind, Severity, UnresolvedReference,
};
use tree_sitter::{Node, Parser};

pub use langs::spec_for;

/// The node types a grammar uses for each kind of declaration.
pub struct Spec {
    pub language: fn() -> tree_sitter::Language,
    pub functions: &'static [&'static str],
    /// Function-shaped nodes that are methods when inside a type.
    pub methods: &'static [&'static str],
    pub classes: &'static [&'static str],
    pub interfaces: &'static [&'static str],
    pub structs: &'static [&'static str],
    pub enums: &'static [&'static str],
    pub enum_members: &'static [&'static str],
    pub type_aliases: &'static [&'static str],
    pub modules: &'static [&'static str],
    pub imports: &'static [&'static str],
    pub calls: &'static [&'static str],
    pub variables: &'static [&'static str],
    pub fields: &'static [&'static str],
    pub name_field: &'static str,
    pub body_field: &'static str,
    pub hooks: &'static (dyn Hooks + Sync),
}

/// What a language reads its own way. Every method has a generic default.
pub trait Hooks {
    /// Handle `node` entirely; return true when done with it.
    fn visit(&self, _w: &mut Walker<'_>, _node: Node<'_>) -> bool {
        false
    }
    /// A declaration's name when the name field doesn't give it.
    fn name(&self, _w: &Walker<'_>, _node: Node<'_>) -> Option<String> {
        None
    }
    /// A declaration's signature; default: its text up to the body.
    fn signature(&self, _w: &Walker<'_>, _node: Node<'_>) -> Option<String> {
        None
    }
    /// The module an import names.
    fn import(&self, w: &Walker<'_>, node: Node<'_>) -> Vec<String> {
        node.child_by_field_name("source")
            .or_else(|| node.named_child(0))
            .map(|n| vec![unquote(w.text(n)).to_string()])
            .unwrap_or_default()
    }
    /// The name a call site calls; default: its callee's text.
    fn call(&self, w: &Walker<'_>, node: Node<'_>) -> Option<String> {
        let callee = node
            .child_by_field_name("function")
            .or_else(|| node.child_by_field_name("name"))
            .or_else(|| node.named_child(0))?;
        Some(callee_name(w.text(callee)))
    }
    /// The supertypes a type declaration names.
    fn supertypes(&self, _w: &Walker<'_>, _node: Node<'_>) -> Vec<(String, ReferenceKind)> {
        Vec::new()
    }
    /// Rewrite syntax the grammar can't read, keeping every byte offset.
    fn preparse(&self, _src: &str) -> Option<String> {
        None
    }
    /// Whether a declaration is visible outside its file.
    fn exported(&self, _w: &Walker<'_>, _node: Node<'_>) -> bool {
        true
    }
}

/// `"x"` → `x`.
pub fn unquote(s: &str) -> &str {
    s.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`')
}

/// A callee as a reference name: `a.b.c(` → `b.c`, `f<T>(` → `f`, no spaces.
pub fn callee_name(text: &str) -> String {
    let t: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let t = t.split(['(', '<', '[']).next().unwrap_or("").to_string();
    let t = t.replace("::", ".").replace("->", ".").replace(':', ".");
    let parts: Vec<&str> = t.split('.').filter(|p| !p.is_empty()).collect();
    match parts.as_slice() {
        [] => String::new(),
        [one] => one.to_string(),
        [.., a, b] => format!("{a}.{b}"),
    }
}

struct Scope {
    id: String,
    kind: NodeKind,
    qualified: String,
}

/// Extraction state for one file.
pub struct Walker<'a> {
    pub path: &'a str,
    pub src: &'a str,
    pub lang: Language,
    pub spec: &'static Spec,
    nodes: Vec<GNode>,
    edges: Vec<Edge>,
    refs: Vec<UnresolvedReference>,
    scopes: Vec<Scope>,
    file_id: String,
    /// Facts a language's hooks record for later nodes of the same file.
    pub notes: std::collections::HashSet<String>,
}

impl<'a> Walker<'a> {
    pub fn text(&self, n: Node<'_>) -> &'a str {
        &self.src[n.byte_range()]
    }

    fn is(&self, list: &[&str], n: Node<'_>) -> bool {
        list.contains(&n.kind())
    }

    /// The innermost scope's node id (the file at top level).
    pub fn scope_id(&self) -> &str {
        self.scopes.last().map_or(&self.file_id, |s| &s.id)
    }

    /// The innermost scope's kind.
    pub fn scope_kind(&self) -> Option<NodeKind> {
        self.scopes.last().map(|s| s.kind)
    }

    fn in_type(&self) -> bool {
        self.scopes.last().is_some_and(|s| {
            s.kind.is_type() && s.kind != NodeKind::Enum
                || s.kind == NodeKind::Module && self.lang == Language::Erlang
        })
    }

    /// The innermost callable scope, for attributing calls.
    fn caller(&self) -> String {
        self.scopes
            .iter()
            .rev()
            .find(|s| s.kind.is_callable())
            .or(self.scopes.last())
            .map_or_else(|| self.file_id.clone(), |s| s.id.clone())
    }

    /// The declaration's name: the spec's name field, then the hook, then
    /// the first identifier-like child.
    pub fn name_of(&self, n: Node<'_>) -> Option<String> {
        if let Some(name) = self.spec.hooks.name(self, n) {
            return Some(name);
        }
        if let Some(c) = n.child_by_field_name(self.spec.name_field) {
            return Some(self.text(c).trim().to_string());
        }
        let mut cur = n.walk();
        let found = n
            .named_children(&mut cur)
            .find(|c| c.kind().contains("identifier") || c.kind() == "name");
        found.map(|c| self.text(c).trim().to_string())
    }

    fn default_signature(&self, n: Node<'_>) -> String {
        let end = n
            .child_by_field_name(self.spec.body_field)
            .map_or(n.end_byte(), |b| b.start_byte());
        let text = &self.src[n.start_byte()..end.max(n.start_byte())];
        let first: String = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .unwrap_or("")
            .to_string();
        first.chars().take(200).collect()
    }

    /// Create a node for `n` and record it in the current scope.
    pub fn create(&mut self, kind: NodeKind, name: &str, n: Node<'_>) -> String {
        let start = n.start_position().row as u32 + 1;
        let end = n.end_position().row as u32 + 1;
        let id = node_id(self.path, kind.as_str(), name, start);
        let mut node = GNode::new(&id, kind, name, self.path, self.lang, start, end);
        node.start_column = n.start_position().column as u32;
        node.end_column = n.end_position().column as u32;
        node.qualified_name = match self.scopes.last() {
            Some(s) => format!("{}::{name}", s.qualified),
            None => name.to_string(),
        };
        if kind != NodeKind::Import {
            node.signature = self
                .spec
                .hooks
                .signature(self, n)
                .or_else(|| Some(self.default_signature(n)));
            node.docstring = preceding_docstring(n, self.src);
        }
        node.is_exported = self.spec.hooks.exported(self, n);
        node.updated_at = now_ms();
        let parent = self.scope_id().to_string();
        self.edges.push(Edge::new(parent, &id, EdgeKind::Contains));
        self.nodes.push(node);
        id
    }

    /// A node created earlier in this file.
    pub fn node_mut(&mut self, id: &str) -> Option<&mut GNode> {
        self.nodes.iter_mut().rev().find(|n| n.id == id)
    }

    /// The last node created, to adjust fields a hook knows better.
    pub fn last_node(&mut self) -> Option<&mut GNode> {
        self.nodes.last_mut()
    }

    pub fn push_scope(&mut self, id: &str, kind: NodeKind, name: &str) {
        let qualified = match self.scopes.last() {
            Some(s) => format!("{}::{name}", s.qualified),
            None => name.to_string(),
        };
        self.scopes.push(Scope {
            id: id.to_string(),
            kind,
            qualified,
        });
    }

    pub fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// A reference from `from` (the innermost scope when None).
    pub fn reference(&mut self, from: Option<&str>, name: &str, kind: ReferenceKind, n: Node<'_>) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        let from = from.map_or_else(|| self.scope_id().to_string(), str::to_string);
        self.refs.push(UnresolvedReference {
            from_node_id: from,
            reference_name: name.to_string(),
            reference_kind: kind,
            line: n.start_position().row as u32 + 1,
            column: n.start_position().column as u32,
            file_path: Some(self.path.to_string()),
            language: Some(self.lang),
            candidates: Vec::new(),
            row_id: None,
        });
    }

    /// A call from the innermost callable.
    pub fn call(&mut self, name: &str, n: Node<'_>) {
        let from = self.caller();
        self.reference(Some(&from), name, ReferenceKind::Calls, n);
    }

    /// An import node for `module` and its reference from the file.
    pub fn import(&mut self, module: &str, n: Node<'_>) {
        let module = module.trim();
        if module.is_empty() {
            return;
        }
        let line = n.start_position().row as u32 + 1;
        let id = node_id(self.path, "import", module, line);
        let mut node = GNode::new(
            &id,
            NodeKind::Import,
            module,
            self.path,
            self.lang,
            line,
            line,
        );
        node.signature = Some(
            self.text(n)
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .chars()
                .take(200)
                .collect(),
        );
        node.updated_at = now_ms();
        self.edges
            .push(Edge::new(&self.file_id, &id, EdgeKind::Contains));
        self.nodes.push(node);
        let file = self.file_id.clone();
        self.reference(Some(&file), module, ReferenceKind::Imports, n);
    }

    /// Visit every named child.
    pub fn children(&mut self, n: Node<'_>) {
        let mut cur = n.walk();
        let kids: Vec<Node<'_>> = n.named_children(&mut cur).collect();
        for c in kids {
            self.visit(c);
        }
    }

    /// Create a declaration of `kind`, then visit its body (or all its
    /// children) inside it.
    pub fn declare(&mut self, kind: NodeKind, n: Node<'_>) -> Option<String> {
        let name = self.name_of(n)?;
        if name.is_empty() {
            return None;
        }
        let id = self.create(kind, &name, n);
        for (sup, k) in self.spec.hooks.supertypes(self, n) {
            self.reference(Some(&id), &sup, k, n);
        }
        self.push_scope(&id, kind, &name);
        match n.child_by_field_name(self.spec.body_field) {
            Some(body) => {
                // Calls in the header (a Solidity modifier, a default value).
                let mut cur = n.walk();
                let header: Vec<Node<'_>> = n
                    .named_children(&mut cur)
                    .filter(|c| *c != body && self.is(self.spec.calls, *c))
                    .collect();
                for c in header {
                    self.visit(c);
                }
                self.children(body)
            }
            None => {
                let mut cur = n.walk();
                let kids: Vec<Node<'_>> = n.named_children(&mut cur).collect();
                for c in kids {
                    if Some(c) != n.child_by_field_name(self.spec.name_field) {
                        self.visit(c);
                    }
                }
            }
        }
        self.pop_scope();
        Some(id)
    }

    pub fn visit(&mut self, n: Node<'_>) {
        if self.spec.hooks.visit(self, n) {
            return;
        }
        let spec = self.spec;
        if self.is(spec.classes, n) {
            self.declare(NodeKind::Class, n);
        } else if self.is(spec.interfaces, n) {
            self.declare(NodeKind::Interface, n);
        } else if self.is(spec.structs, n) {
            self.declare(NodeKind::Struct, n);
        } else if self.is(spec.enums, n) {
            self.declare(NodeKind::Enum, n);
        } else if self.is(spec.modules, n) {
            self.declare(NodeKind::Module, n);
        } else if self.is(spec.methods, n) && self.in_type() {
            self.declare(NodeKind::Method, n);
        } else if self.is(spec.functions, n) || self.is(spec.methods, n) {
            self.declare(NodeKind::Function, n);
        } else if self.is(spec.enum_members, n) {
            if let Some(name) = self.name_of(n) {
                self.create(NodeKind::EnumMember, &name, n);
            }
        } else if self.is(spec.type_aliases, n) {
            if let Some(name) = self.name_of(n) {
                self.create(NodeKind::TypeAlias, &name, n);
            }
        } else if self.is(spec.imports, n) {
            for m in self.spec.hooks.import(self, n) {
                self.import(&m, n);
            }
        } else if self.is(spec.fields, n) && self.in_type() {
            if let Some(name) = self.name_of(n) {
                self.create(NodeKind::Field, &name, n);
            }
            self.children(n);
        } else if self.is(spec.variables, n)
            && !self.scopes.last().is_some_and(|s| s.kind.is_callable())
        {
            if let Some(name) = self.name_of(n) {
                self.create(NodeKind::Variable, &name, n);
            }
            self.children(n);
        } else if self.is(spec.calls, n) {
            if let Some(name) = self.spec.hooks.call(self, n) {
                self.call(&name, n);
            }
            self.children(n);
        } else {
            self.children(n);
        }
    }
}

/// Extract one file with its language's spec.
pub fn extract(path: &str, content: &str, lang: Language, spec: &'static Spec) -> ExtractionResult {
    let mut r = ExtractionResult::default();
    let lines = content.lines().count().max(1) as u32;
    let file_id = file_node_id(path);
    let mut f = GNode::new(
        &file_id,
        NodeKind::File,
        path.rsplit('/').next().unwrap_or(path),
        path,
        lang,
        1,
        lines,
    );
    f.qualified_name = path.to_string();
    f.updated_at = now_ms();
    r.nodes.push(f);
    let mut parser = Parser::new();
    if parser.set_language(&(spec.language)()).is_err() {
        return r;
    }
    let fixed = spec.hooks.preparse(content);
    let parsed = fixed.as_deref().unwrap_or(content);
    let Some(tree) = parser.parse(parsed, None) else {
        return r;
    };
    let root = tree.root_node();
    let mut w = Walker {
        path,
        src: parsed,
        lang,
        spec,
        nodes: Vec::new(),
        edges: Vec::new(),
        refs: Vec::new(),
        scopes: Vec::new(),
        file_id,
        notes: Default::default(),
    };
    w.visit(root);
    if root.has_error() {
        r.errors.push(ExtractionError {
            message: "parse tree contains syntax errors; extraction is partial".to_string(),
            file_path: Some(path.to_string()),
            line: None,
            column: None,
            severity: Severity::Warning,
            code: Some("parse_error".to_string()),
        });
    }
    r.nodes.extend(w.nodes);
    r.edges.extend(w.edges);
    r.unresolved_references.extend(w.refs);
    r
}

/// A file's parse tree as an S-expression, for writing a spec.
pub fn debug_tree(lang: Language, content: &str) -> Option<String> {
    let spec = spec_for(lang)?;
    let mut parser = Parser::new();
    parser.set_language(&(spec.language)()).ok()?;
    let tree = parser.parse(content, None)?;
    let mut out = String::new();
    dump(tree.root_node(), content, 0, None, &mut out);
    Some(out)
}

fn dump(n: Node<'_>, src: &str, depth: usize, field: Option<&str>, out: &mut String) {
    use std::fmt::Write;
    let text = &src[n.byte_range()];
    let leaf = if n.named_child_count() == 0 {
        format!(" {:?}", text.chars().take(60).collect::<String>())
    } else {
        String::new()
    };
    let _ = writeln!(
        out,
        "{}{}{}{}",
        "  ".repeat(depth),
        field.map(|f| format!("{f}: ")).unwrap_or_default(),
        n.kind(),
        leaf
    );
    let mut cur = n.walk();
    for (i, c) in n.named_children(&mut cur).enumerate() {
        let _ = i;
        let f = field_of(n, c);
        dump(c, src, depth + 1, f, out);
    }
}

fn field_of<'t>(parent: Node<'t>, child: Node<'t>) -> Option<&'static str> {
    let mut cur = parent.walk();
    if cur.goto_first_child() {
        loop {
            if cur.node() == child {
                return cur.field_name();
            }
            if !cur.goto_next_sibling() {
                break;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests;
