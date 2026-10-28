//! Terraform / OpenTofu / HCL (tree-sitter-hcl). Every construct is a
//! `block` whose first identifier is its type (`resource`, `variable`, …)
//! and whose string labels name it. Each declaration's qualified name is the
//! address Terraform code refers to it by, with `::` for `.`
//! (`aws_s3_bucket::logs`, `var::region`, `module::vpc`), so a reference
//! `aws_s3_bucket.logs` resolves by its qualified name.

use crate::generic::{Hooks, Spec, Walker};
use codegraph_core::{NodeKind, ReferenceKind};
use tree_sitter::Node;

pub static SPEC: Spec = Spec {
    language: || tree_sitter_hcl::LANGUAGE.into(),
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
    hooks: &Terraform,
};

struct Terraform;

/// Heads of references that never name a project declaration.
const BUILTIN_HEADS: &[&str] = &[
    "each",
    "count",
    "self",
    "path",
    "terraform",
    "null",
    "true",
    "false",
];

fn named_kids<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut cur = n.walk();
    n.named_children(&mut cur).collect()
}

/// A `string_lit`'s value.
fn string_value(w: &Walker<'_>, n: Node<'_>) -> String {
    named_kids(n)
        .into_iter()
        .find(|c| c.kind() == "template_literal")
        .map(|c| w.text(c).to_string())
        .unwrap_or_default()
}

/// A block's type and labels.
fn header(w: &Walker<'_>, block: Node<'_>) -> Option<(String, Vec<String>)> {
    let kids = named_kids(block);
    let first = kids.first().filter(|c| c.kind() == "identifier")?;
    let mut labels = Vec::new();
    for c in &kids[1..] {
        match c.kind() {
            "string_lit" => labels.push(string_value(w, *c)),
            "identifier" => labels.push(w.text(*c).to_string()),
            _ => break,
        }
    }
    Some((w.text(*first).to_string(), labels))
}

fn body_of(block: Node<'_>) -> Option<Node<'_>> {
    named_kids(block).into_iter().find(|c| c.kind() == "body")
}

/// References in an expression subtree: `var.X`, `local.X`, `module.M`,
/// `data.T.N` and `T.N` (a managed resource).
fn references(w: &mut Walker<'_>, from: &str, n: Node<'_>) {
    let mut stack = vec![n];
    while let Some(x) = stack.pop() {
        if x.kind() == "variable_expr" {
            reference_at(w, from, x);
        }
        stack.extend(named_kids(x).into_iter().rev());
    }
}

fn reference_at(w: &mut Walker<'_>, from: &str, var: Node<'_>) {
    let Some(id) = named_kids(var)
        .into_iter()
        .find(|c| c.kind() == "identifier")
    else {
        return;
    };
    let head = w.text(id).to_string();
    if BUILTIN_HEADS.contains(&head.as_str()) {
        return;
    }
    let mut attrs = Vec::new();
    let mut cur = var.next_named_sibling();
    while let Some(c) = cur {
        match c.kind() {
            "get_attr" => {
                let Some(a) = named_kids(c).into_iter().find(|k| k.kind() == "identifier") else {
                    break;
                };
                attrs.push(w.text(a).to_string());
            }
            "index" | "new_index" | "legacy_index" | "splat" | "attr_splat" | "full_splat" => {}
            _ => break,
        }
        cur = c.next_named_sibling();
    }
    let name = match (head.as_str(), attrs.as_slice()) {
        ("var" | "local" | "module", [a, ..]) => format!("{head}.{a}"),
        ("data", [t, n, ..]) => format!("data.{t}.{n}"),
        ("var" | "local" | "module" | "data", _) => return,
        (_, [a, ..]) => format!("{head}.{a}"),
        _ => return,
    };
    w.reference(Some(from), &name, ReferenceKind::References, var);
}

impl Terraform {
    fn block(&self, w: &mut Walker<'_>, n: Node<'_>) -> bool {
        let Some((ty, labels)) = header(w, n) else {
            return false;
        };
        let body = body_of(n);
        let first = labels.first().cloned();
        let decl: Option<(NodeKind, String, String)> = match (ty.as_str(), labels.as_slice()) {
            ("resource", [t, name, ..]) => {
                Some((NodeKind::Struct, name.clone(), format!("{t}::{name}")))
            }
            ("data", [t, name, ..]) => {
                Some((NodeKind::Struct, name.clone(), format!("data::{t}::{name}")))
            }
            ("module", [name, ..]) => {
                Some((NodeKind::Module, name.clone(), format!("module::{name}")))
            }
            ("variable", [name, ..]) => {
                Some((NodeKind::Variable, name.clone(), format!("var::{name}")))
            }
            ("output", [name, ..]) => {
                Some((NodeKind::Constant, name.clone(), format!("output::{name}")))
            }
            ("provider", [name, ..]) => Some((
                NodeKind::Namespace,
                name.clone(),
                format!("provider::{name}"),
            )),
            _ => None,
        };
        if ty == "locals" && labels.is_empty() {
            // Each attribute is a `local.<name>`.
            for attr in body.map(named_kids).unwrap_or_default() {
                if attr.kind() != "attribute" {
                    continue;
                }
                let Some(id) = named_kids(attr)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
                else {
                    continue;
                };
                let name = w.text(id).to_string();
                let node = w.create(NodeKind::Constant, &name, attr);
                if let Some(last) = w.last_node() {
                    last.qualified_name = format!("local::{name}");
                }
                references(w, &node, attr);
            }
            return true;
        }
        let Some((kind, name, qualified)) = decl else {
            // `terraform {}`, `moved {}`, `import {}`: their references are
            // the file's.
            if let Some(b) = body {
                let file = w.scope_id().to_string();
                references(w, &file, b);
            }
            return true;
        };
        let id = w.create(kind, &name, n);
        if let Some(last) = w.last_node() {
            last.qualified_name = qualified;
            last.signature = Some(format!(
                "{ty} {}",
                labels
                    .iter()
                    .map(|l| format!("\"{l}\""))
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        if let Some(b) = body {
            references(w, &id, b);
            if ty == "module" {
                self.module_source(w, b, first.as_deref().unwrap_or(""));
            }
        }
        true
    }

    /// A module with a local `source` imports that directory's `main.tf`.
    fn module_source(&self, w: &mut Walker<'_>, body: Node<'_>, _name: &str) {
        for attr in named_kids(body) {
            if attr.kind() != "attribute" {
                continue;
            }
            let kids = named_kids(attr);
            if kids.first().map(|i| w.text(*i)) != Some("source") {
                continue;
            }
            let mut stack: Vec<Node<'_>> = kids;
            while let Some(x) = stack.pop() {
                if x.kind() == "string_lit" {
                    let s = string_value(w, x);
                    if s.starts_with("./") || s.starts_with("../") {
                        w.import(&format!("{}/main.tf", s.trim_end_matches('/')), x);
                    }
                    break;
                }
                stack.extend(named_kids(x));
            }
        }
    }
}

impl Hooks for Terraform {
    fn visit(&self, w: &mut Walker<'_>, n: Node<'_>) -> bool {
        match n.kind() {
            "block" => self.block(w, n),
            // A `.tfvars` assignment sets the root module's variable.
            "attribute" if w.path.ends_with(".tfvars") => {
                if let Some(id) = named_kids(n).into_iter().find(|c| c.kind() == "identifier") {
                    let name = format!("var.{}", w.text(id));
                    let file = w.scope_id().to_string();
                    w.reference(Some(&file), &name, ReferenceKind::References, n);
                }
                true
            }
            _ => false,
        }
    }
}
