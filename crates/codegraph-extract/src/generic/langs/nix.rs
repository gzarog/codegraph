//! Nix (tree-sitter-nix): each binding is a variable, or a function when
//! its value is a lambda; an application calls its head; `import ./x.nix`,
//! `callPackage ./pkg { }` and a module's `imports = [ ./a.nix ]` import
//! files.

use crate::generic::{Hooks, Spec, Walker};
use codegraph_core::NodeKind;
use tree_sitter::Node;

pub static SPEC: Spec = Spec {
    language: || tree_sitter_nix::LANGUAGE.into(),
    functions: &[],
    methods: &[],
    classes: &[],
    interfaces: &[],
    structs: &[],
    enums: &[],
    enum_members: &[],
    type_aliases: &[],
    modules: &[],
    imports: &[],
    calls: &[],
    variables: &[],
    fields: &[],
    name_field: "name",
    body_field: "body",
    hooks: &Nix,
};

struct Nix;

fn kids<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut cur = n.walk();
    n.named_children(&mut cur).collect()
}

fn unwrap_variable(n: Node<'_>) -> Node<'_> {
    if n.kind() == "variable_expression" {
        n.named_child(0).unwrap_or(n)
    } else {
        n
    }
}

fn function_of(n: Node<'_>) -> Option<Node<'_>> {
    n.child_by_field_name("function")
        .or_else(|| n.named_child(0))
}

/// The head a curried application calls: `lib.mkIf a b` → `lib.mkIf`.
fn callee(w: &Walker<'_>, n: Node<'_>) -> Option<String> {
    let mut cur = n;
    while cur.kind() == "apply_expression" {
        cur = function_of(cur)?;
    }
    let cur = unwrap_variable(cur);
    matches!(cur.kind(), "identifier" | "select_expression").then(|| w.text(cur).trim().to_string())
}

fn is_static_path(v: &str) -> bool {
    (v.starts_with("./") || v.starts_with("../"))
        && !v.contains(|c: char| c.is_whitespace() || "{}()[];\"'<>$".contains(c))
}

fn static_path(w: &Walker<'_>, mut n: Node<'_>) -> Option<String> {
    while n.kind() == "parenthesized_expression" {
        n = n.named_child(0)?;
    }
    let t = w.text(n).trim();
    let t = t
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(t);
    is_static_path(t).then(|| t.to_string())
}

/// `f a b` → `a`, the first argument of a curried chain.
fn first_argument(n: Node<'_>) -> Option<Node<'_>> {
    let mut inner = n;
    while let Some(f) = function_of(inner).filter(|f| f.kind() == "apply_expression") {
        inner = f;
    }
    inner
        .child_by_field_name("argument")
        .or_else(|| inner.named_child(1))
}

/// Whether a binding is a member of the attribute set the file returns.
fn exported(n: Node<'_>) -> bool {
    let mut cur = n;
    let mut in_attrset = false;
    while let Some(p) = cur.parent() {
        match p.kind() {
            "let_expression" => {
                if p.child_by_field_name("body") != Some(cur) {
                    return false;
                }
            }
            "binding" if cur != n => return false,
            "formals" | "formal_parameters" => return false,
            "attrset_expression" | "rec_attrset_expression" | "attrset" | "rec_attrset" => {
                in_attrset = true
            }
            _ => {}
        }
        cur = p;
    }
    in_attrset
}

/// A lambda's parameters and its innermost body: `{ pkgs, ... }: x: body`.
fn curried<'t>(w: &Walker<'_>, n: Node<'t>) -> (Vec<String>, Option<Node<'t>>) {
    let mut params = Vec::new();
    let mut cur = n;
    loop {
        let Some(body) = cur
            .child_by_field_name("body")
            .or_else(|| kids(cur).last().copied())
        else {
            return (params, None);
        };
        let head = w.src[cur.start_byte()..body.start_byte()].trim();
        let head = head.strip_suffix(':').unwrap_or(head).trim();
        if !head.is_empty() {
            params.push(head.to_string());
        }
        if body.kind() == "function_expression" {
            cur = body;
        } else {
            return (params, Some(body));
        }
    }
}

impl Hooks for Nix {
    fn visit(&self, w: &mut Walker<'_>, n: Node<'_>) -> bool {
        match n.kind() {
            "binding" => {
                let Some(path) = n
                    .child_by_field_name("attrpath")
                    .or_else(|| n.named_child(0))
                else {
                    return false;
                };
                let name = w.text(path).trim().to_string();
                let Some(value) = n
                    .child_by_field_name("expression")
                    .or_else(|| n.named_child(1))
                else {
                    return false;
                };
                if value.kind() == "function_expression" {
                    let (params, body) = curried(w, value);
                    let id = w.create(NodeKind::Function, &name, n);
                    let exp = exported(n);
                    if let Some(last) = w.last_node() {
                        last.signature = Some(match params.as_slice() {
                            [] => "()".to_string(),
                            [one]
                                if !(one.starts_with('(')
                                    || one.contains('{')
                                    || one.contains('@')) =>
                            {
                                format!("({one})")
                            }
                            _ => params.join(" : "),
                        });
                        last.is_exported = exp;
                    }
                    w.push_scope(&id, NodeKind::Function, &name);
                    if let Some(b) = body {
                        w.visit(b);
                    }
                    w.pop_scope();
                } else {
                    w.create(NodeKind::Variable, &name, n);
                    let init: String = w
                        .text(value)
                        .lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(100)
                        .collect();
                    let exp = exported(n);
                    if let Some(last) = w.last_node() {
                        last.signature = Some(format!("= {init}"));
                        last.is_exported = exp;
                    }
                    // `imports = [ ./hardware.nix ]` in a module.
                    let last_seg = name.rsplit('.').next().unwrap_or(&name);
                    if matches!(last_seg, "imports" | "modules")
                        && value.kind() == "list_expression"
                    {
                        for c in kids(value) {
                            if c.kind() == "path_expression" {
                                let p = w.text(c).trim().to_string();
                                if is_static_path(&p) {
                                    w.import(&p, c);
                                }
                            }
                        }
                    }
                    w.visit(value);
                }
                true
            }
            "function_expression" => {
                if let Some(b) = kids(n).last().copied() {
                    w.visit(b);
                }
                true
            }
            "inherit" | "inherit_from" => {
                for c in kids(n) {
                    if c.kind() == "inherited_attrs" {
                        for a in kids(c) {
                            let name = w.text(a).trim().to_string();
                            if !name.is_empty() {
                                w.create(NodeKind::Variable, &name, a);
                                let exp = exported(a);
                                if let Some(last) = w.last_node() {
                                    last.is_exported = exp;
                                }
                            }
                        }
                    } else {
                        w.visit(c);
                    }
                }
                true
            }
            "apply_expression" => {
                let direct = function_of(n).map(|f| w.text(unwrap_variable(f)).trim().to_string());
                let is_import = matches!(direct.as_deref(), Some("import" | "builtins.import"));
                // The inner levels of a curried chain are the same call.
                let inner = n
                    .parent()
                    .filter(|p| p.kind() == "apply_expression")
                    .and_then(function_of)
                    == Some(n);
                if is_import {
                    let arg = n
                        .child_by_field_name("argument")
                        .or_else(|| n.named_child(1));
                    if let Some(p) = arg.and_then(|a| static_path(w, a)) {
                        w.import(&p, n);
                    }
                } else if !inner {
                    if let Some(name) =
                        callee(w, n).filter(|c| c != "import" && c != "builtins.import")
                    {
                        w.call(&name, n);
                        let tail = name.rsplit('.').next().unwrap_or(&name);
                        if matches!(tail, "callPackage" | "callPackages") {
                            if let Some(p) = first_argument(n).and_then(|a| static_path(w, a)) {
                                w.import(&p, n);
                            }
                        }
                    }
                }
                w.children(n);
                true
            }
            _ => false,
        }
    }
}
