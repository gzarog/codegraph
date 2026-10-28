//! Solidity (tree-sitter-solidity): contracts and libraries are classes,
//! constructors and `fallback`/`receive` are named methods, events and
//! errors are fields so `emit X` and `revert X` resolve to them.

use crate::generic::{callee_name, unquote, Hooks, Spec, Walker};
use codegraph_core::ReferenceKind;
use tree_sitter::Node;

pub static SPEC: Spec = Spec {
    language: || tree_sitter_solidity::LANGUAGE.into(),
    functions: &["function_definition", "modifier_definition"],
    methods: &[
        "function_definition",
        "modifier_definition",
        "constructor_definition",
        "fallback_receive_definition",
    ],
    classes: &["contract_declaration", "library_declaration"],
    interfaces: &["interface_declaration"],
    structs: &["struct_declaration"],
    enums: &["enum_declaration"],
    enum_members: &["enum_value"],
    type_aliases: &["user_defined_type_definition"],
    modules: &[],
    imports: &["import_directive"],
    calls: &[
        "call_expression",
        "emit_statement",
        "revert_statement",
        "modifier_invocation",
    ],
    variables: &[
        "constant_variable_declaration",
        "state_variable_declaration",
    ],
    fields: &[
        "state_variable_declaration",
        "struct_member",
        "event_definition",
        "error_declaration",
    ],
    name_field: "name",
    body_field: "body",
    hooks: &Solidity,
};

struct Solidity;

impl Hooks for Solidity {
    fn name(&self, w: &Walker<'_>, n: Node<'_>) -> Option<String> {
        match n.kind() {
            "constructor_definition" => Some("constructor".into()),
            "fallback_receive_definition" => {
                let mut cur = n.walk();
                let kw = n
                    .children(&mut cur)
                    .map(|c| w.text(c))
                    .find(|t| *t == "fallback" || *t == "receive");
                Some(kw.unwrap_or("fallback").to_string())
            }
            "enum_value" => Some(w.text(n).trim().to_string()),
            _ => None,
        }
    }

    fn signature(&self, w: &Walker<'_>, n: Node<'_>) -> Option<String> {
        if !n.kind().ends_with("_definition") || n.kind() == "event_definition" {
            return None;
        }
        let mut params = Vec::new();
        let mut rest = Vec::new();
        let mut cur = n.walk();
        for (i, c) in n.named_children(&mut cur).enumerate() {
            let _ = i;
            match c.kind() {
                "parameter" => params.push(w.text(c).to_string()),
                "visibility" | "state_mutability" | "return_type_definition" => {
                    rest.push(w.text(c).to_string())
                }
                _ => {}
            }
        }
        let mut sig = format!("({})", params.join(", "));
        for r in rest {
            sig.push(' ');
            sig.push_str(&r);
        }
        Some(sig)
    }

    fn import(&self, w: &Walker<'_>, n: Node<'_>) -> Vec<String> {
        n.child_by_field_name("source")
            .map(|s| vec![unquote(w.text(s)).to_string()])
            .unwrap_or_default()
    }

    fn call(&self, w: &Walker<'_>, n: Node<'_>) -> Option<String> {
        let callee = match n.kind() {
            "emit_statement" => n.child_by_field_name("name"),
            "revert_statement" => n.child_by_field_name("error"),
            "modifier_invocation" => n.named_child(0),
            _ => n.child_by_field_name("function"),
        }?;
        Some(callee_name(w.text(callee)))
    }

    fn supertypes(&self, w: &Walker<'_>, n: Node<'_>) -> Vec<(String, ReferenceKind)> {
        let mut cur = n.walk();
        n.named_children(&mut cur)
            .filter(|c| c.kind() == "inheritance_specifier")
            .filter_map(|c| c.child_by_field_name("ancestor"))
            .map(|a| {
                // `A.B` names B; one keyword (`is`) covers base contracts
                // and interfaces alike.
                let t = w.text(a);
                (
                    t.rsplit('.').next().unwrap_or(t).trim().to_string(),
                    ReferenceKind::Extends,
                )
            })
            .collect()
    }
}
