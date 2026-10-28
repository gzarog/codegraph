//! CFML (tree-sitter-cfml): tag-based `.cfm`/`.cfc` files with the `cfml`
//! grammar, script components (`component { … }`) with `cfscript`. A
//! component is a class named after its file; `<cffunction>` and `function`
//! declare its methods.

use crate::generic::{callee_name, Hooks, Spec, Walker};
use codegraph_core::{NodeKind, ReferenceKind};
use tree_sitter::{Node, Parser};

pub static SPEC: Spec = Spec {
    language: || tree_sitter_cfml::LANGUAGE_CFML.into(),
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
    hooks: &Cfml,
};

pub static SCRIPT: Spec = Spec {
    language: || tree_sitter_cfml::LANGUAGE_CFSCRIPT.into(),
    ..SPEC_FIELDS
};

/// The shared parts of both specs (the grammar differs).
const SPEC_FIELDS: Spec = Spec {
    language: || tree_sitter_cfml::LANGUAGE_CFSCRIPT.into(),
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
    hooks: &Cfml,
};

struct Cfml;

fn kids<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut cur = n.walk();
    n.named_children(&mut cur).collect()
}

/// The component's name: its file's stem.
fn stem(path: &str) -> String {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .split('.')
        .next()
        .unwrap_or("")
        .to_string()
}

/// `name="x"` attributes of a tag or a script component.
fn attributes(w: &Walker<'_>, n: Node<'_>) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        match x.kind() {
            "cf_attribute" | "component_attribute" => {
                let k = kids(x);
                let name = k
                    .iter()
                    .find(|c| matches!(c.kind(), "cf_attribute_name" | "identifier"))
                    .map(|c| w.text(*c).to_lowercase());
                let value = k
                    .iter()
                    .find(|c| {
                        matches!(
                            c.kind(),
                            "quoted_cf_attribute_value" | "string" | "attribute_value"
                        )
                    })
                    .map(|c| w.text(*c).trim_matches(['"', '\'']).to_string());
                if let (Some(n), Some(v)) = (name, value) {
                    out.push((n, v));
                }
            }
            // A nested tag's attributes are its own.
            "cf_selfclose_tag" | "cf_function_tag" if x != n => {}
            _ => stack.extend(kids(x)),
        }
    }
    out
}

/// Whether `n` follows a component open tag already walked (before its
/// close tag).
fn after_open_tag(w: &Walker<'_>, n: Node<'_>) -> bool {
    let mut p = n.prev_named_sibling();
    while let Some(x) = p {
        match x.kind() {
            "cf_component_close_tag" => return false,
            "cf_component_open_tag" => {
                return w
                    .notes
                    .contains(&format!("component-body-from:{}", x.end_byte()))
            }
            _ => p = x.prev_named_sibling(),
        }
    }
    false
}

fn last_segment(t: &str) -> &str {
    t.trim().rsplit('.').next().unwrap_or(t).trim()
}

impl Cfml {
    fn component(
        &self,
        w: &mut Walker<'_>,
        n: Node<'_>,
        attrs: &[(String, String)],
        body: Vec<Node<'_>>,
    ) {
        let name = stem(w.path);
        let id = w.create(NodeKind::Class, &name, n);
        for (k, v) in attrs {
            let kind = match k.as_str() {
                "extends" => ReferenceKind::Extends,
                "implements" => ReferenceKind::Implements,
                _ => continue,
            };
            for t in v.split(',') {
                let t = last_segment(t).to_string();
                if !t.is_empty() {
                    w.reference(Some(&id), &t, kind, n);
                }
            }
        }
        w.push_scope(&id, NodeKind::Class, &name);
        for c in body {
            w.visit(c);
        }
        w.pop_scope();
    }

    fn function(&self, w: &mut Walker<'_>, n: Node<'_>, name: &str, body: Vec<Node<'_>>) {
        let kind = if w.scope_kind() == Some(NodeKind::Class) {
            NodeKind::Method
        } else {
            NodeKind::Function
        };
        let id = w.create(kind, name, n);
        w.push_scope(&id, kind, name);
        for c in body {
            w.visit(c);
        }
        w.pop_scope();
    }

    /// `createObject("component", "models.UserDAO")` instantiates UserDAO.
    fn call(&self, w: &mut Walker<'_>, n: Node<'_>) {
        let Some(f) = n.child_by_field_name("function") else {
            w.children(n);
            return;
        };
        let name = callee_name(w.text(f));
        if name.eq_ignore_ascii_case("createObject") {
            let args: Vec<String> = n
                .child_by_field_name("arguments")
                .map(kids)
                .unwrap_or_default()
                .into_iter()
                .map(|a| w.text(a).trim_matches(['"', '\'']).to_string())
                .collect();
            if args
                .first()
                .is_some_and(|k| k.eq_ignore_ascii_case("component"))
            {
                if let Some(c) = args.get(1) {
                    let t = last_segment(c).to_string();
                    w.reference(None, &t, ReferenceKind::Instantiates, n);
                }
            }
        } else {
            w.call(&name, n);
        }
        w.children(n);
    }
}

impl Hooks for Cfml {
    fn visit(&self, w: &mut Walker<'_>, n: Node<'_>) -> bool {
        // The tags after a component's open tag were walked as its body.
        if w.scope_kind().is_none() && after_open_tag(w, n) {
            return true;
        }
        match n.kind() {
            // A script component in a `.cfc`: the cfml grammar keeps it
            // opaque; read it with the script grammar.
            "component_file" => {
                let mut p = Parser::new();
                if p.set_language(&tree_sitter_cfml::LANGUAGE_CFSCRIPT.into())
                    .is_ok()
                {
                    if let Some(tree) = p.parse(w.src, None) {
                        w.visit(tree.root_node());
                    }
                }
                true
            }
            "component" | "interface" => {
                let attrs = attributes(w, n);
                let body: Vec<Node<'_>> =
                    n.child_by_field_name("body").map(kids).unwrap_or_default();
                self.component(w, n, &attrs, body);
                true
            }
            "cf_component_open_tag" => {
                let attrs = attributes(w, n);
                // The tags after it, up to the close tag, are its body.
                let mut body = Vec::new();
                let mut cur = n.next_named_sibling();
                while let Some(c) = cur {
                    if c.kind() == "cf_component_close_tag" {
                        break;
                    }
                    body.push(c);
                    cur = c.next_named_sibling();
                }
                w.notes
                    .insert(format!("component-body-from:{}", n.end_byte()));
                self.component(w, n.parent().unwrap_or(n), &attrs, body);
                true
            }
            "function_declaration" => {
                let Some(name) = n.child_by_field_name("name").map(|x| w.text(x).to_string())
                else {
                    return true;
                };
                let body = n.child_by_field_name("body").map(kids).unwrap_or_default();
                self.function(w, n, &name, body);
                true
            }
            "cf_function_tag" => {
                let name = attributes(w, n)
                    .into_iter()
                    .find(|(k, _)| k == "name")
                    .map(|(_, v)| v);
                let Some(name) = name else { return true };
                let body: Vec<Node<'_>> = kids(n)
                    .into_iter()
                    .filter(|c| c.kind() != "cf_attribute")
                    .collect();
                self.function(w, n, &name, body);
                true
            }
            "property_declaration" => {
                if let Some((_, v)) = attributes(w, n).into_iter().find(|(k, _)| k == "name") {
                    w.create(NodeKind::Property, &v, n);
                }
                true
            }
            "cf_selfclose_tag" | "cf_tag" => {
                let head = w.text(n).trim_start().to_lowercase();
                let attrs = attributes(w, n);
                let get = |k: &str| attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.clone());
                if head.starts_with("<cfinclude") || head.starts_with("<cfmodule") {
                    if let Some(t) = get("template") {
                        w.import(&t, n);
                    }
                } else if head.starts_with("<cfinvoke") {
                    if let Some(m) = get("method") {
                        let name = match get("component") {
                            Some(c) => format!("{}.{m}", last_segment(&c)),
                            None => m,
                        };
                        w.call(&name, n);
                    }
                } else if head.starts_with("<cfobject") {
                    if let Some(c) = get("component") {
                        let t = last_segment(&c).to_string();
                        w.reference(None, &t, ReferenceKind::Instantiates, n);
                    }
                }
                w.children(n);
                true
            }
            // `include "x.cfm";` in script.
            "include_statement" => {
                if let Some(s) = kids(n).into_iter().find(|c| c.kind() == "string") {
                    let t = w.text(s).trim_matches(['"', '\'']).to_string();
                    w.import(&t, n);
                }
                true
            }
            "call_expression" => {
                self.call(w, n);
                true
            }
            "new_expression" => {
                if let Some(c) = n.child_by_field_name("constructor") {
                    let t = last_segment(w.text(c)).to_string();
                    w.reference(None, &t, ReferenceKind::Instantiates, n);
                }
                w.children(n);
                true
            }
            _ => false,
        }
    }
}
