//! Objective-C (tree-sitter-objc). A class is declared by its `@interface`
//! and its methods are defined in its `@implementation`, which reuses the
//! class node. Methods are named by selector (`storeImage:forKey:`) and a
//! message send is recorded as `receiver.selector`.

use crate::generic::{callee_name, Hooks, Spec, Walker};
use codegraph_core::{NodeKind, ReferenceKind};
use regex::Regex;
use std::sync::LazyLock;
use tree_sitter::Node;

pub static SPEC: Spec = Spec {
    language: || tree_sitter_objc::LANGUAGE.into(),
    functions: &[],
    methods: &[],
    classes: &[],
    interfaces: &[],
    structs: &["struct_specifier"],
    enums: &["enum_specifier"],
    enum_members: &["enumerator"],
    type_aliases: &[],
    modules: &[],
    imports: &[],
    calls: &["call_expression"],
    variables: &[],
    fields: &[],
    name_field: "name",
    body_field: "body",
    hooks: &ObjC,
};

struct ObjC;

fn kids<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut cur = n.walk();
    n.named_children(&mut cur).collect()
}

fn first_identifier<'t>(n: Node<'t>) -> Option<Node<'t>> {
    kids(n).into_iter().find(|c| c.kind() == "identifier")
}

/// A macro alone on its line (`NS_ASSUME_NONNULL_BEGIN`).
static LONE_MACRO: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^[ \t]*([A-Z][A-Z0-9_]{2,})[ \t]*$").unwrap());
/// `typedef NS_ENUM(NSInteger, Mode) {`.
static FOUNDATION_ENUM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"typedef\s+(?:NS_ENUM|NS_OPTIONS|NS_CLOSED_ENUM|NS_ERROR_ENUM)\s*\(\s*\w+\s*,\s*(\w+)\s*\)").unwrap()
});

/// The innermost name a declarator chain ends in.
fn declarator_name(w: &Walker<'_>, mut n: Node<'_>) -> Option<String> {
    loop {
        if matches!(
            n.kind(),
            "identifier" | "type_identifier" | "field_identifier"
        ) {
            return Some(w.text(n).to_string());
        }
        n = n.child_by_field_name("declarator").or_else(|| {
            kids(n)
                .into_iter()
                .find(|c| c.kind().ends_with("declarator") || c.kind() == "identifier")
        })?;
    }
}

/// A method's selector: `greet`, `doThing:`, `doThing:with:`.
fn selector(w: &Walker<'_>, n: Node<'_>) -> Option<String> {
    let parts: Vec<&str> = kids(n)
        .into_iter()
        .filter(|c| c.kind() == "identifier")
        .map(|c| w.text(c))
        .collect();
    let first = *parts.first()?;
    if kids(n).iter().any(|c| c.kind() == "method_parameter") {
        Some(parts.iter().map(|p| format!("{p}:")).collect())
    } else {
        Some(first.to_string())
    }
}

impl ObjC {
    /// The class node for `name` in this file, creating it at `n` if needed.
    fn class(&self, w: &mut Walker<'_>, name: &str, n: Node<'_>) -> String {
        let key = format!("class:{name}=");
        if let Some(id) = w
            .notes
            .iter()
            .find_map(|k| k.strip_prefix(&key).map(str::to_string))
        {
            return id;
        }
        let id = w.create(NodeKind::Class, name, n);
        w.notes.insert(format!("{key}{id}"));
        id
    }

    fn interface(&self, w: &mut Walker<'_>, n: Node<'_>) {
        let Some(name) = first_identifier(n).map(|c| w.text(c).to_string()) else {
            return;
        };
        let id = self.class(w, &name, n);
        if let Some(sup) = n.child_by_field_name("superclass") {
            let s = w.text(sup).to_string();
            w.reference(Some(&id), &s, ReferenceKind::Extends, sup);
        }
        for c in kids(n) {
            if matches!(
                c.kind(),
                "protocol_reference_list" | "parameterized_arguments"
            ) {
                let mut stack = vec![c];
                while let Some(x) = stack.pop() {
                    if matches!(x.kind(), "identifier" | "type_identifier") {
                        let p = w.text(x).to_string();
                        w.reference(Some(&id), &p, ReferenceKind::Implements, x);
                    }
                    stack.extend(kids(x));
                }
            }
        }
        w.push_scope(&id, NodeKind::Class, &name);
        for c in kids(n) {
            if c.kind() == "property_declaration" {
                self.property(w, c);
            }
        }
        w.pop_scope();
    }

    fn property(&self, w: &mut Walker<'_>, n: Node<'_>) {
        let decl = kids(n)
            .into_iter()
            .find(|c| c.kind() == "struct_declaration")
            .and_then(|s| {
                kids(s)
                    .into_iter()
                    .find(|c| c.kind() == "struct_declarator")
            });
        if let Some(name) = decl.and_then(|d| declarator_name(w, d)) {
            w.create(NodeKind::Property, &name, n);
        }
    }

    fn message(&self, w: &mut Walker<'_>, n: Node<'_>) {
        let mut cur = n.walk();
        let mut keywords = Vec::new();
        let mut has_colon = false;
        if cur.goto_first_child() {
            loop {
                if cur.field_name() == Some("method") {
                    keywords.push(w.text(cur.node()).to_string());
                }
                if cur.node().kind() == ":" {
                    has_colon = true;
                }
                if !cur.goto_next_sibling() {
                    break;
                }
            }
        }
        if !keywords.is_empty() {
            let method = if has_colon {
                keywords.iter().map(|k| format!("{k}:")).collect()
            } else {
                keywords[0].clone()
            };
            let receiver = n.child_by_field_name("receiver");
            let name = match receiver {
                Some(r) if r.kind() != "message_expression" => {
                    let rn = w.text(r).to_string();
                    if rn == "self" || rn == "super" {
                        method
                    } else {
                        // A class message also depends on the class.
                        if rn.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                            && rn.chars().all(|c| c.is_alphanumeric() || c == '_')
                        {
                            w.reference(None, &rn, ReferenceKind::References, r);
                        }
                        format!("{rn}.{method}")
                    }
                }
                _ => method,
            };
            w.call(&name, n);
        }
        w.children(n);
    }
}

impl Hooks for ObjC {
    fn preparse(&self, src: &str) -> Option<String> {
        if !LONE_MACRO.is_match(src) && !FOUNDATION_ENUM.is_match(src) {
            return None;
        }
        let mut out = src.as_bytes().to_vec();
        for c in LONE_MACRO.captures_iter(src) {
            let m = c.get(1).unwrap();
            for b in &mut out[m.start()..m.end()] {
                *b = b' ';
            }
        }
        // `typedef NS_ENUM(NSInteger, Mode)` → `enum Mode` padded to length.
        for c in FOUNDATION_ENUM.captures_iter(src) {
            let m = c.get(0).unwrap();
            let replacement = format!("enum {}", &c[1]);
            let mut bytes = replacement.into_bytes();
            bytes.resize(m.len(), b' ');
            out[m.start()..m.end()].copy_from_slice(&bytes);
        }
        String::from_utf8(out).ok()
    }

    fn visit(&self, w: &mut Walker<'_>, n: Node<'_>) -> bool {
        match n.kind() {
            "class_interface" => {
                self.interface(w, n);
                true
            }
            "protocol_declaration" => {
                let Some(name) = first_identifier(n).map(|c| w.text(c).to_string()) else {
                    return true;
                };
                let id = w.create(NodeKind::Protocol, &name, n);
                for c in kids(n)
                    .into_iter()
                    .filter(|c| c.kind() == "protocol_reference_list")
                {
                    for p in kids(c) {
                        let pn = w.text(p).to_string();
                        w.reference(Some(&id), &pn, ReferenceKind::Extends, p);
                    }
                }
                true
            }
            "class_implementation" => {
                let Some(name) = first_identifier(n).map(|c| w.text(c).to_string()) else {
                    return true;
                };
                let id = self.class(w, &name, n);
                w.push_scope(&id, NodeKind::Class, &name);
                for c in kids(n)
                    .into_iter()
                    .filter(|c| c.kind() == "implementation_definition")
                {
                    w.children(c);
                }
                w.pop_scope();
                true
            }
            "method_definition" => {
                let Some(name) = selector(w, n) else {
                    return true;
                };
                let in_class = w.scope_kind().is_some_and(|k| k.is_type());
                let kind = if in_class {
                    NodeKind::Method
                } else {
                    NodeKind::Function
                };
                let id = w.create(kind, &name, n);
                let is_static = w.text(n).trim_start().starts_with('+');
                if let Some(last) = w.last_node() {
                    last.is_static = is_static;
                }
                w.push_scope(&id, kind, &name);
                for c in kids(n)
                    .into_iter()
                    .filter(|c| c.kind() == "compound_statement")
                {
                    w.children(c);
                }
                w.pop_scope();
                true
            }
            "function_definition" => {
                let Some(name) = n
                    .child_by_field_name("declarator")
                    .and_then(|d| declarator_name(w, d))
                else {
                    return true;
                };
                let id = w.create(NodeKind::Function, &name, n);
                w.push_scope(&id, NodeKind::Function, &name);
                if let Some(b) = n.child_by_field_name("body") {
                    w.children(b);
                }
                w.pop_scope();
                true
            }
            "type_definition" => {
                let named = n
                    .child_by_field_name("declarator")
                    .and_then(|d| declarator_name(w, d));
                if let Some(name) = named {
                    w.create(NodeKind::TypeAlias, &name, n);
                }
                if let Some(t) = n.child_by_field_name("type") {
                    w.visit(t);
                }
                true
            }
            "declaration" if w.scope_kind().is_none() => {
                if let Some(name) = n
                    .child_by_field_name("declarator")
                    .and_then(|d| declarator_name(w, d))
                {
                    let constant = w.text(n).contains("const ");
                    w.create(
                        if constant {
                            NodeKind::Constant
                        } else {
                            NodeKind::Variable
                        },
                        &name,
                        n,
                    );
                }
                w.children(n);
                true
            }
            "preproc_include" => {
                let path = n.child_by_field_name("path");
                if let Some(p) = path {
                    let m = if p.kind() == "system_lib_string" {
                        w.text(p)
                            .trim_start_matches('<')
                            .trim_end_matches('>')
                            .to_string()
                    } else {
                        kids(p)
                            .into_iter()
                            .find(|c| c.kind() == "string_content")
                            .map(|c| w.text(c).to_string())
                            .unwrap_or_default()
                    };
                    w.import(&m, n);
                }
                true
            }
            "message_expression" => {
                self.message(w, n);
                true
            }
            _ => false,
        }
    }

    fn call(&self, w: &Walker<'_>, n: Node<'_>) -> Option<String> {
        let f = n.child_by_field_name("function")?;
        Some(callee_name(w.text(f)))
    }
}
