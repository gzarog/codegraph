//! Erlang (tree-sitter-erlang, the ELP grammar). A function's identity is
//! its name and arity: consecutive clauses of `f/2` are one function node,
//! qualified `mod::f/2`; a remote call is recorded as `mod::f/2` and a local
//! one as `f/2`, so `f/1` and `f/2` never stand in for each other.
//! `-spec`/`-type` bodies parse as calls and are skipped.

use crate::generic::{Hooks, Spec, Walker};
use codegraph_core::{NodeKind, ReferenceKind};
use tree_sitter::Node;

pub static SPEC: Spec = Spec {
    language: || tree_sitter_erlang::LANGUAGE.into(),
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
    hooks: &Erlang,
};

struct Erlang;

/// Calls naming their real callee as (Module, Function, Args) arguments.
const MFA_CALLS: &[&str] = &[
    "spawn",
    "spawn_link",
    "spawn_monitor",
    "spawn_opt",
    "apply",
    "erlang:spawn",
    "erlang:spawn_link",
    "erlang:spawn_monitor",
    "erlang:spawn_opt",
    "erlang:apply",
    "proc_lib:spawn",
    "proc_lib:spawn_link",
    "proc_lib:spawn_opt",
    "proc_lib:start",
    "proc_lib:start_link",
    "timer:apply_after",
    "timer:apply_interval",
    "rpc:call",
    "rpc:cast",
    "rpc:async_call",
    "erpc:call",
    "erpc:cast",
];

/// Macros the compiler defines; no `-define` exists to link a use to.
const PREDEFINED_MACROS: &[&str] = &[
    "MODULE",
    "MODULE_STRING",
    "FILE",
    "LINE",
    "MACHINE",
    "FUNCTION_NAME",
    "FUNCTION_ARITY",
    "OTP_RELEASE",
    "FEATURE_AVAILABLE",
    "FEATURE_ENABLED",
];

fn kids<'t>(n: Node<'t>) -> Vec<Node<'t>> {
    let mut cur = n.walk();
    n.named_children(&mut cur).collect()
}

fn field<'t>(n: Node<'t>, f: &str) -> Option<Node<'t>> {
    n.child_by_field_name(f)
}

/// An atom's text without quoted-atom quotes.
fn atom(w: &Walker<'_>, n: Node<'_>) -> String {
    let t = w.text(n);
    t.strip_prefix('\'')
        .and_then(|x| x.strip_suffix('\''))
        .unwrap_or(t)
        .to_string()
}

/// Arguments in an `args` node.
fn arity(n: Option<Node<'_>>) -> usize {
    n.map_or(0, |a| a.named_child_count())
}

fn collapse(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Erlang {
    fn module_name(&self, w: &Walker<'_>, root: Node<'_>) -> Option<String> {
        kids(root)
            .into_iter()
            .find(|f| f.kind() == "module_attribute")
            .and_then(|m| field(m, "name"))
            .map(|n| atom(w, n))
    }

    /// `-export([f/1])` keys, or everything with `export_all`.
    fn note_exports(&self, w: &mut Walker<'_>, root: Node<'_>) {
        for form in kids(root) {
            match form.kind() {
                "compile_options_attribute" if w.text(form).contains("export_all") => {
                    w.notes.insert("export:*".into());
                }
                "export_attribute" => {
                    for fa in kids(form).into_iter().filter(|c| c.kind() == "fa") {
                        let Some(fun) = field(fa, "fun") else {
                            continue;
                        };
                        let name = atom(w, fun);
                        let ar = field(fa, "arity").and_then(|a| field(a, "value"));
                        let key = match ar {
                            Some(v) => format!("export:{name}/{}", w.text(v)),
                            None => format!("export:{name}"),
                        };
                        w.notes.insert(key);
                    }
                }
                "pp_define" => {
                    // Macros naming this module or an atom, for gen_server targets.
                    let name = field(form, "lhs").and_then(|l| field(l, "name"));
                    let repl = field(form, "replacement");
                    if let (Some(name), Some(r)) = (name, repl) {
                        let name = w.text(name).to_string();
                        if r.kind() == "macro_call_expr"
                            && field(r, "name").map(|x| w.text(x)) == Some("MODULE")
                        {
                            w.notes.insert(format!("selfmacro:{name}"));
                        } else if r.kind() == "atom" {
                            let a = atom(w, r);
                            w.notes.insert(format!("atommacro:{name}={a}"));
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn fun_decl(&self, w: &mut Walker<'_>, n: Node<'_>) {
        let clauses: Vec<Node<'_>> = kids(n)
            .into_iter()
            .filter(|c| c.kind() == "function_clause")
            .collect();
        let Some(first) = clauses.first().copied() else {
            return;
        };
        let Some(name_node) = field(first, "name") else {
            return;
        };
        let name = atom(w, name_node);
        let ar = arity(field(first, "args"));
        let key = format!("{name}/{ar}");
        // A continuation clause of the same function.
        let last = w.notes.iter().find(|k| k.starts_with("lastfn:")).cloned();
        let id = match last
            .as_deref()
            .and_then(|k| k.strip_prefix("lastfn:"))
            .and_then(|r| r.split_once('\0'))
        {
            Some((k, id)) if k == key => {
                let id = id.to_string();
                let end = n.end_position().row as u32 + 1;
                if let Some(node) = w.node_mut(&id) {
                    node.end_line = node.end_line.max(end);
                }
                id
            }
            _ => {
                let exported = w.notes.contains("export:*")
                    || w.notes.contains(&format!("export:{key}"))
                    || w.notes.contains(&format!("export:{name}"));
                let spec = {
                    let mut prev = n.prev_named_sibling();
                    while prev.is_some_and(|p| p.kind() == "comment") {
                        prev = prev.and_then(|p| p.prev_named_sibling());
                    }
                    prev.filter(|p| {
                        p.kind() == "spec"
                            && field(*p, "fun").map(|f| atom(w, f)).as_deref() == Some(&name)
                    })
                };
                let header = {
                    let end = field(first, "body").map_or(first.end_byte(), |b| b.start_byte());
                    collapse(&w.src[first.start_byte()..end])
                };
                let id = w.create(NodeKind::Function, &name, n);
                let sig = spec
                    .map(|s| collapse(w.text(s)).chars().take(300).collect())
                    .unwrap_or(header);
                let doc = spec.and_then(|s| crate::docstring::preceding_docstring(s, w.src));
                if let Some(node) = w.last_node() {
                    node.qualified_name = format!("{}/{ar}", node.qualified_name);
                    node.signature = Some(sig);
                    node.is_exported = exported;
                    if doc.is_some() {
                        node.docstring = doc;
                    }
                }
                if let Some(k) = last {
                    w.notes.remove(&k);
                }
                w.notes.insert(format!("lastfn:{key}\0{id}"));
                id
            }
        };
        w.push_scope(&id, NodeKind::Function, &name);
        for c in clauses {
            w.children(c);
        }
        w.pop_scope();
    }

    /// The module a `gen_server:call(Target, …)` target names.
    fn server_target(&self, w: &Walker<'_>, t: Node<'_>) -> Option<String> {
        let own = w
            .path
            .rsplit('/')
            .next()
            .unwrap_or("")
            .trim_end_matches(".erl")
            .to_string();
        match t.kind() {
            "atom" => Some(atom(w, t)),
            "macro_call_expr" => {
                let m = w.text(field(t, "name")?).to_string();
                if m == "MODULE" || w.notes.contains(&format!("selfmacro:{m}")) {
                    return Some(own);
                }
                let prefix = format!("atommacro:{m}=");
                w.notes
                    .iter()
                    .find_map(|k| k.strip_prefix(&prefix).map(str::to_string))
            }
            _ => None,
        }
    }

    fn call(&self, w: &mut Walker<'_>, n: Node<'_>) {
        let mut callee = field(n, "expr");
        let mut module_node = None;
        if n.parent().is_some_and(|p| p.kind() == "remote") {
            module_node = n.parent().and_then(|p| field(p, "module"));
        } else if callee.is_some_and(|c| c.kind() == "remote") {
            let r = callee.unwrap();
            module_node = field(r, "module");
            callee = field(r, "fun");
        }
        if let Some(c) = callee.filter(|c| c.kind() == "atom") {
            let bare = atom(w, c);
            let module_expr = module_node.and_then(|m| field(m, "module"));
            let mut name = bare.clone();
            match module_expr {
                Some(m) if m.kind() == "atom" => name = format!("{}::{bare}", atom(w, m)),
                // `?MODULE:f(X)` is this module's f; `Mod:f(X)` is dynamic.
                Some(m) => {
                    let is_self = m.kind() == "macro_call_expr"
                        && field(m, "name").map(|x| w.text(x)) == Some("MODULE");
                    if !is_self {
                        w.children(n);
                        return;
                    }
                }
                None => {}
            }
            let args = field(n, "args");
            w.call(&format!("{name}/{}", arity(args)), n);
            let module = module_expr
                .filter(|m| m.kind() == "atom")
                .map(|m| atom(w, m));
            // gen_server:call(Target, Msg) reaches Target's handle_call/3.
            if module.as_deref() == Some("gen_server")
                && matches!(bare.as_str(), "call" | "cast" | "send_request")
            {
                if let Some(t) = args
                    .and_then(|a| a.named_child(0))
                    .and_then(|t| self.server_target(w, t))
                {
                    let handler = if bare == "cast" {
                        "handle_cast/2"
                    } else {
                        "handle_call/3"
                    };
                    w.call(&format!("{t}::{handler}"), n);
                }
            }
            // spawn(M, F, Args) and friends call M:F.
            let family = match &module {
                Some(m) => format!("{m}:{bare}"),
                None => bare.clone(),
            };
            if MFA_CALLS.contains(&family.as_str()) {
                let args: Vec<Node<'_>> = args.map(kids).unwrap_or_default();
                let is_module = |m: Node<'_>| {
                    m.kind() == "atom"
                        || m.kind() == "macro_call_expr"
                            && field(m, "name").map(|x| w.text(x)) == Some("MODULE")
                };
                let pairs: Vec<usize> = (0..args.len().saturating_sub(1))
                    .filter(|&i| args[i + 1].kind() == "atom" && is_module(args[i]))
                    .collect();
                // The pair right before the argument list, so a node name
                // (`rpc:call('n@h', M, F, A)`) is never taken for the module.
                let chosen = pairs
                    .iter()
                    .copied()
                    .find(|&i| args.get(i + 2).is_some_and(|l| l.kind() == "list"))
                    .or_else(|| pairs.first().copied());
                if let Some(i) = chosen {
                    let (m, f) = (args[i], args[i + 1]);
                    let tail = args
                        .get(i + 2)
                        .filter(|l| l.kind() == "list")
                        .map(|l| format!("/{}", l.named_child_count()))
                        .unwrap_or_default();
                    let target = if m.kind() == "atom" {
                        format!("{}::{}{tail}", atom(w, m), atom(w, f))
                    } else {
                        format!("{}{tail}", atom(w, f))
                    };
                    w.call(&target, f);
                }
            }
        }
        w.children(n);
    }
}

impl Hooks for Erlang {
    fn visit(&self, w: &mut Walker<'_>, n: Node<'_>) -> bool {
        match n.kind() {
            "source_file" => {
                self.note_exports(w, n);
                match self.module_name(w, n) {
                    Some(m) => {
                        let id = w.create(NodeKind::Namespace, &m, n);
                        w.push_scope(&id, NodeKind::Namespace, &m);
                        w.children(n);
                        w.pop_scope();
                    }
                    None => w.children(n),
                }
                true
            }
            "fun_decl" => {
                self.fun_decl(w, n);
                true
            }
            "record_decl" => {
                if let Some(name) = field(n, "name") {
                    let name = atom(w, name);
                    let id = w.create(NodeKind::Struct, &name, n);
                    w.push_scope(&id, NodeKind::Struct, &name);
                    for f in kids(n).into_iter().filter(|c| c.kind() == "record_field") {
                        if let Some(fname) = field(f, "name") {
                            let fname = atom(w, fname);
                            w.create(NodeKind::Field, &fname, f);
                        }
                    }
                    w.pop_scope();
                }
                true
            }
            "type_alias" | "opaque" => {
                if let Some(name) = field(n, "name").and_then(|t| field(t, "name")) {
                    let name = atom(w, name);
                    w.create(NodeKind::TypeAlias, &name, n);
                }
                true
            }
            "pp_define" => {
                if let Some(name) = field(n, "lhs").and_then(|l| field(l, "name")) {
                    let name = w.text(name).to_string();
                    let id = w.create(NodeKind::Constant, &name, n);
                    if let Some(r) = field(n, "replacement") {
                        w.push_scope(&id, NodeKind::Function, &name);
                        w.visit(r);
                        w.pop_scope();
                    }
                }
                true
            }
            "behaviour_attribute" => {
                if let Some(name) = field(n, "name") {
                    let name = atom(w, name);
                    w.reference(None, &name, ReferenceKind::Implements, n);
                }
                true
            }
            "import_attribute" => {
                if let Some(m) = field(n, "module") {
                    let m = atom(w, m);
                    w.import(&m, n);
                }
                true
            }
            "pp_include" | "pp_include_lib" => {
                if let Some(f) = field(n, "file") {
                    let f = w.text(f).trim_matches('"').to_string();
                    w.import(&f, n);
                }
                true
            }
            "spec" | "callback" | "module_attribute" | "export_attribute" => true,
            "call" => {
                self.call(w, n);
                true
            }
            "internal_fun" | "external_fun" => {
                let Some(fun) = field(n, "fun").filter(|f| f.kind() == "atom") else {
                    return true;
                };
                let mut name = atom(w, fun);
                if n.kind() == "external_fun" {
                    match field(n, "module")
                        .and_then(|m| field(m, "name"))
                        .filter(|a| a.kind() == "atom")
                    {
                        Some(m) => name = format!("{}::{name}", atom(w, m)),
                        None => return true,
                    }
                }
                if let Some(v) = field(n, "arity").and_then(|a| field(a, "value")) {
                    name = format!("{name}/{}", w.text(v));
                }
                w.reference(None, &name, ReferenceKind::References, n);
                true
            }
            "macro_call_expr" => {
                if let Some(m) = field(n, "name") {
                    let m = w.text(m).to_string();
                    if !PREDEFINED_MACROS.contains(&m.as_str()) {
                        if field(n, "args").is_some() {
                            w.call(&m, n);
                        } else {
                            w.reference(None, &m, ReferenceKind::References, n);
                        }
                    }
                }
                w.children(n);
                true
            }
            "record_expr" | "record_update_expr" | "record_index_expr" | "record_field_expr" => {
                if let Some(a) = field(n, "name")
                    .filter(|r| r.kind() == "record_name")
                    .and_then(|r| field(r, "name"))
                    .filter(|a| a.kind() == "atom")
                {
                    let name = atom(w, a);
                    w.reference(None, &name, ReferenceKind::References, n);
                }
                w.children(n);
                true
            }
            _ => false,
        }
    }
}
