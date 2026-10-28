//! Pascal and Delphi (tree-sitter-pascal). A class's methods are declared
//! in its body and defined in the implementation section as
//! `procedure TShape.Draw;`: the definition, which has the body, is the
//! method node, and a body declaration only stands in for a method the unit
//! doesn't define (an abstract one, or one of an interface).

use crate::generic::{Hooks, Spec, Walker};
use codegraph_core::{NodeKind, ReferenceKind};
use tree_sitter::Node;

pub static SPEC: Spec = Spec {
    language: || tree_sitter_pascal::LANGUAGE.into(),
    functions: &[],
    methods: &[],
    classes: &[],
    interfaces: &[],
    structs: &[],
    enums: &[],
    enum_members: &["declEnumValue"],
    type_aliases: &[],
    modules: &[],
    imports: &[],
    calls: &["exprCall"],
    variables: &["declVar", "declConst"],
    fields: &["declField", "declProp"],
    name_field: "name",
    body_field: "body",
    hooks: &Pascal,
};

struct Pascal;

fn named<'t>(n: Node<'t>, kind: &str) -> Option<Node<'t>> {
    let mut cur = n.walk();
    let found = n.named_children(&mut cur).find(|c| c.kind() == kind);
    found
}

fn has_child(n: Node<'_>, kind: &str) -> bool {
    let mut cur = n.walk();
    let found = n.children(&mut cur).any(|c| c.kind() == kind);
    found
}

/// A dotted name as `Owner.Name` parts: `TShape.Draw` → (Some("TShape"), "Draw").
fn split_name(w: &Walker<'_>, name: Node<'_>) -> (Option<String>, String) {
    let text: String = w
        .text(name)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    // Generic arguments (`TList<T>.Add`) are not part of the names.
    let mut plain = String::new();
    let mut depth = 0;
    for c in text.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            _ if depth == 0 => plain.push(c),
            _ => {}
        }
    }
    match plain.rsplit_once('.') {
        Some((owner, name)) => (
            Some(owner.rsplit('.').next().unwrap_or(owner).to_string()),
            name.to_string(),
        ),
        None => (None, plain),
    }
}

impl Pascal {
    /// Record which `Class.Method`s the file defines, before the class
    /// bodies are read.
    fn note_definitions(&self, w: &mut Walker<'_>, n: Node<'_>) {
        let mut stack = vec![n];
        while let Some(x) = stack.pop() {
            if x.kind() == "defProc" {
                if let Some(name) = x
                    .child_by_field_name("header")
                    .and_then(|h| h.child_by_field_name("name"))
                {
                    if let (Some(owner), method) = split_name(w, name) {
                        w.notes.insert(format!(
                            "{}.{}",
                            owner.to_lowercase(),
                            method.to_lowercase()
                        ));
                    }
                }
            }
            let mut cur = x.walk();
            stack.extend(x.named_children(&mut cur));
        }
    }

    fn type_decl(&self, w: &mut Walker<'_>, n: Node<'_>) {
        let Some(name) = n
            .child_by_field_name("name")
            .map(|x| w.text(x).trim().to_string())
        else {
            return;
        };
        let ty = n.child_by_field_name("type");
        let class = ty.filter(|t| t.kind() == "declClass");
        let intf = ty.filter(|t| t.kind() == "declIntf");
        let enumeration = ty.and_then(|t| {
            if t.kind() == "declEnum" {
                Some(t)
            } else {
                named(t, "declEnum")
            }
        });
        if let Some(body) = class.or(intf) {
            let kind = if intf.is_some() {
                NodeKind::Interface
            } else if has_child(body, "kRecord") {
                NodeKind::Struct
            } else {
                NodeKind::Class
            };
            let id = w.create(kind, &name, n);
            let mut cur = body.walk();
            let parents: Vec<Node<'_>> = body
                .children_by_field_name("parent", &mut cur)
                .filter(|p| p.is_named())
                .collect();
            for (i, p) in parents.into_iter().enumerate() {
                // The first parent of a class is its base; the rest are
                // interfaces. An interface's parents are interfaces.
                let k = if i == 0 || kind == NodeKind::Interface {
                    ReferenceKind::Extends
                } else {
                    ReferenceKind::Implements
                };
                let text = w.text(p);
                let base = text.split('<').next().unwrap_or(text).trim();
                let base = base.rsplit('.').next().unwrap_or(base).to_string();
                w.reference(Some(&id), &base, k, p);
            }
            w.push_scope(&id, kind, &name);
            self.members(w, body, &name);
            w.pop_scope();
        } else if let Some(e) = enumeration {
            let id = w.create(NodeKind::Enum, &name, n);
            w.push_scope(&id, NodeKind::Enum, &name);
            w.children(e);
            w.pop_scope();
        } else {
            w.create(NodeKind::TypeAlias, &name, n);
            if let Some(t) = ty {
                w.children(t);
            }
        }
    }

    /// A class or interface body: sections, fields, properties and method
    /// declarations.
    fn members(&self, w: &mut Walker<'_>, body: Node<'_>, owner: &str) {
        let mut cur = body.walk();
        let kids: Vec<Node<'_>> = body.named_children(&mut cur).collect();
        for c in kids {
            match c.kind() {
                "declSection" => self.members(w, c, owner),
                "declProc" => {
                    let Some(name) = c.child_by_field_name("name") else {
                        continue;
                    };
                    let (_, method) = split_name(w, name);
                    let key = format!("{}.{}", owner.to_lowercase(), method.to_lowercase());
                    if !w.notes.contains(&key) {
                        w.create(NodeKind::Method, &method, c);
                    }
                }
                _ => w.visit(c),
            }
        }
    }

    fn routine(&self, w: &mut Walker<'_>, n: Node<'_>) {
        let Some(header) = n.child_by_field_name("header") else {
            w.children(n);
            return;
        };
        let Some(name) = header.child_by_field_name("name") else {
            w.children(n);
            return;
        };
        let (owner, routine) = split_name(w, name);
        let kind = if owner.is_some() {
            NodeKind::Method
        } else {
            NodeKind::Function
        };
        let id = w.create(kind, &routine, n);
        let sig = signature(w, header);
        if let Some(last) = w.last_node() {
            if sig.is_some() {
                last.signature = sig;
            }
            // `TShape.Draw` is TShape's, wherever the class is declared.
            if let Some(o) = &owner {
                last.qualified_name = format!("{o}::{routine}");
            }
        }
        w.push_scope(&id, kind, &routine);
        let mut cur = n.walk();
        let kids: Vec<Node<'_>> = n
            .named_children(&mut cur)
            .filter(|c| *c != header)
            .collect();
        for c in kids {
            w.visit(c);
        }
        w.pop_scope();
    }
}

/// `(const AName: string): Double` from a routine header.
fn signature(w: &Walker<'_>, header: Node<'_>) -> Option<String> {
    let args = header.child_by_field_name("args");
    let ret = header.child_by_field_name("type");
    if args.is_none() && ret.is_none() {
        return None;
    }
    let mut sig = args.map(|a| w.text(a).to_string()).unwrap_or_default();
    if let Some(r) = ret {
        sig.push_str(": ");
        sig.push_str(w.text(r));
    }
    Some(sig)
}

/// Blank `[…]` and the bytes of each match's `blank` group with spaces.
fn blank(out: &mut [u8], from: usize, to: usize) {
    for b in &mut out[from..to] {
        if *b != b'\n' {
            *b = b' ';
        }
    }
}

static CLASS_MODIFIER: std::sync::LazyLock<regex::Regex> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"(?i)\bclass\s+(sealed|abstract)\b").unwrap());
static FOR_VAR: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)\bfor\s+(var)\s+\w+(\s*:\s*[\w.<>, ]+?)?\s+in\b").unwrap()
});
static PARAM_ATTRIBUTE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)[(;]\s*(?:(?:const|var|out|constref)\s+)?(\[[^\]\n]{1,200}\])").unwrap()
});

impl Hooks for Pascal {
    /// Delphi syntax the grammar has no rule for, blanked: `class sealed`,
    /// a `for var X in` loop variable, an attribute on a parameter.
    fn preparse(&self, src: &str) -> Option<String> {
        let mut out = src.as_bytes().to_vec();
        let mut changed = false;
        for c in CLASS_MODIFIER.captures_iter(src) {
            let m = c.get(1).unwrap();
            blank(&mut out, m.start(), m.end());
            changed = true;
        }
        for c in FOR_VAR.captures_iter(src) {
            for g in [c.get(1), c.get(2)].into_iter().flatten() {
                blank(&mut out, g.start(), g.end());
                changed = true;
            }
        }
        for c in PARAM_ATTRIBUTE.captures_iter(src) {
            let m = c.get(1).unwrap();
            blank(&mut out, m.start(), m.end());
            changed = true;
        }
        changed.then(|| String::from_utf8(out).unwrap_or_else(|_| src.to_string()))
    }

    fn visit(&self, w: &mut Walker<'_>, n: Node<'_>) -> bool {
        match n.kind() {
            "root" => {
                self.note_definitions(w, n);
                w.children(n);
                true
            }
            "declUses" => {
                let mut cur = n.walk();
                let mods: Vec<Node<'_>> = n
                    .named_children(&mut cur)
                    .filter(|c| c.kind() == "moduleName")
                    .collect();
                for m in mods {
                    let name: String = w.text(m).chars().filter(|c| !c.is_whitespace()).collect();
                    w.import(&name, m);
                }
                true
            }
            "declType" => {
                self.type_decl(w, n);
                true
            }
            "defProc" => {
                self.routine(w, n);
                true
            }
            // A routine declared outside a class (a forward or external
            // declaration) is a function.
            "declProc" => {
                if let Some(name) = n.child_by_field_name("name") {
                    let (_, routine) = split_name(w, name);
                    w.create(NodeKind::Function, &routine, n);
                }
                true
            }
            // `Area;` — a parameterless call is a bare name statement.
            "statement" => {
                if n.named_child_count() == 1 {
                    let c = n.named_child(0).unwrap();
                    if matches!(c.kind(), "identifier" | "exprDot") {
                        let name = crate::generic::callee_name(w.text(c));
                        w.call(&name, c);
                        return true;
                    }
                }
                false
            }
            // `inherited Create` calls the base's Create.
            "inherited" => {
                if let Some(id) = named(n, "identifier") {
                    let name = w.text(id).to_string();
                    w.call(&name, id);
                }
                w.children(n);
                true
            }
            _ => false,
        }
    }

    fn call(&self, w: &Walker<'_>, n: Node<'_>) -> Option<String> {
        let entity = n.child_by_field_name("entity")?;
        Some(crate::generic::callee_name(w.text(entity)))
    }

    fn exported(&self, _w: &Walker<'_>, n: Node<'_>) -> bool {
        // Declared in a unit's interface section.
        let mut p = n.parent();
        while let Some(x) = p {
            match x.kind() {
                "interface" => return true,
                "implementation" => return false,
                _ => p = x.parent(),
            }
        }
        true
    }
}
