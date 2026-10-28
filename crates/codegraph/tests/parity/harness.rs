//! A project in a temp directory, indexed, with the queries upstream's
//! tests make (`calls from this file`, `callers of that symbol`).

use codegraph::project::CodeGraph;
use codegraph_core::{Edge, EdgeKind, Node};
use std::path::Path;

pub struct Project {
    pub dir: tempfile::TempDir,
    pub cg: CodeGraph,
}

impl Project {
    /// Write `files` and index them.
    pub fn new(files: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in files {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, text).unwrap();
        }
        let mut cg = CodeGraph::init(dir.path()).unwrap();
        cg.index_all(None).unwrap();
        Project { dir, cg }
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    pub fn nodes_in(&self, file: &str) -> Vec<Node> {
        self.cg.store().nodes_in_file(file).unwrap()
    }

    pub fn node(&self, id: &str) -> Option<Node> {
        self.cg.store().node(id).unwrap()
    }

    /// Symbols named `name`.
    pub fn named(&self, name: &str) -> Vec<Node> {
        self.cg.store().nodes_named(name).unwrap()
    }

    /// The one symbol named `name` (in `file` when given).
    pub fn one(&self, name: &str, file: Option<&str>) -> Node {
        let hits: Vec<Node> = self
            .named(name)
            .into_iter()
            .filter(|n| file.is_none_or(|f| n.file_path == f))
            .collect();
        assert_eq!(hits.len(), 1, "one `{name}` in {file:?}: {hits:#?}");
        hits.into_iter().next().unwrap()
    }

    /// Edges of `kind` out of every node in `file`.
    pub fn edges_from_file(&self, file: &str, kind: EdgeKind) -> Vec<Edge> {
        let ids: Vec<String> = self.nodes_in(file).into_iter().map(|n| n.id).collect();
        self.cg.store().outgoing_edges_from(&ids, &[kind]).unwrap()
    }

    /// `file: qualified` of each target of a `kind` edge out of `file`, sorted.
    pub fn targets_from_file(&self, file: &str, kind: EdgeKind) -> Vec<String> {
        let mut out: Vec<String> = self
            .edges_from_file(file, kind)
            .iter()
            .filter_map(|e| self.node(&e.target))
            .map(|t| format!("{}: {}", t.file_path, t.qualified_name))
            .collect();
        out.sort();
        out
    }

    pub fn calls_from(&self, file: &str) -> Vec<String> {
        self.targets_from_file(file, EdgeKind::Calls)
    }

    /// `file: qualified` of each target of `node`'s `kind` edges, sorted.
    pub fn targets_of(&self, node: &Node, kinds: &[EdgeKind]) -> Vec<String> {
        let mut out: Vec<String> = self
            .cg
            .store()
            .outgoing_edges(&node.id, kinds)
            .unwrap()
            .iter()
            .filter_map(|e| self.node(&e.target))
            .map(|t| format!("{}: {}", t.file_path, t.qualified_name))
            .collect();
        out.sort();
        out
    }

    /// `file: qualified` of each source of an edge of `kinds` into `node`, sorted.
    pub fn sources_of(&self, node: &Node, kinds: &[EdgeKind]) -> Vec<String> {
        let mut out: Vec<String> = self
            .cg
            .store()
            .incoming_edges(&node.id, kinds)
            .unwrap()
            .iter()
            .filter_map(|e| self.node(&e.source))
            .map(|t| format!("{}: {}", t.file_path, t.qualified_name))
            .collect();
        out.sort();
        out
    }
}

/// Whether `list` holds an entry equal to `item`.
#[track_caller]
pub fn assert_has(list: &[String], item: &str) {
    assert!(
        list.iter().any(|x| x == item),
        "expected {item:?} in {list:#?}"
    );
}

#[track_caller]
pub fn assert_lacks(list: &[String], item: &str) {
    assert!(
        !list.iter().any(|x| x == item),
        "did not expect {item:?} in {list:#?}"
    );
}

impl Project {
    /// Qualified names of what `node` calls, sorted (upstream `getCallees`).
    pub fn callees(&self, node: &Node) -> Vec<String> {
        let mut out: Vec<String> = self
            .cg
            .store()
            .outgoing_edges(&node.id, &[EdgeKind::Calls])
            .unwrap()
            .iter()
            .filter_map(|e| self.node(&e.target))
            .map(|t| t.qualified_name)
            .collect();
        out.sort();
        out
    }

    /// The function named `name` (upstream `getNodesByName(fn).find(kind === 'function')`).
    pub fn function(&self, name: &str) -> Node {
        self.named(name)
            .into_iter()
            .find(|n| n.kind == codegraph_core::NodeKind::Function)
            .unwrap_or_else(|| panic!("no function {name}"))
    }

    /// `kind:name@path` of each import edge's target out of `file`'s file node.
    pub fn imports_of_file(&self, file: &str) -> Vec<String> {
        let f = self
            .cg
            .store()
            .file_node(file)
            .unwrap()
            .unwrap_or_else(|| panic!("no file {file}"));
        let mut out: Vec<String> = self
            .cg
            .store()
            .outgoing_edges(&f.id, &[EdgeKind::Imports])
            .unwrap()
            .iter()
            .filter_map(|e| self.node(&e.target))
            .map(|n| format!("{}:{}@{}", n.kind.as_str(), n.name, n.file_path))
            .collect();
        out.sort();
        out
    }
}

impl Project {
    /// Targets of `node`'s `kind` edges.
    pub fn targets(&self, node: &Node, kind: EdgeKind) -> Vec<Node> {
        self.cg
            .store()
            .outgoing_edges(&node.id, &[kind])
            .unwrap()
            .iter()
            .filter_map(|e| self.node(&e.target))
            .collect()
    }

    /// Targets of `kind` edges out of every node in `file`.
    pub fn file_targets(&self, file: &str, kind: EdgeKind) -> Vec<Node> {
        self.edges_from_file(file, kind)
            .iter()
            .filter_map(|e| self.node(&e.target))
            .collect()
    }

    /// The function or method named `name`.
    pub fn callable(&self, name: &str) -> Node {
        self.named(name)
            .into_iter()
            .find(|n| n.kind.is_callable())
            .unwrap_or_else(|| panic!("no callable {name}"))
    }

    /// `name@path` of what the callable `name` calls, in edge order.
    pub fn calls_of(&self, name: &str) -> Vec<String> {
        self.targets(&self.callable(name), EdgeKind::Calls)
            .iter()
            .map(|n| format!("{}@{}", n.name, n.file_path))
            .collect()
    }
}

impl Project {
    /// `source -> target (target file)` of every call edge, sorted.
    pub fn all_calls(&self) -> Vec<String> {
        let mut out: Vec<String> = self
            .cg
            .store()
            .edges_of_kind(EdgeKind::Calls)
            .unwrap()
            .iter()
            .filter_map(|e| Some((self.node(&e.source)?, self.node(&e.target)?)))
            .map(|(s, t)| {
                format!(
                    "{} -> {} ({})",
                    s.qualified_name, t.qualified_name, t.file_path
                )
            })
            .collect();
        out.sort();
        out
    }

    /// `kind:qualified` of what the function `name` calls, sorted.
    pub fn call_kinds(&self, name: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .targets(&self.function(name), EdgeKind::Calls)
            .iter()
            .map(|n| format!("{}:{}", n.kind.as_str(), n.qualified_name))
            .collect();
        out.sort();
        out
    }
}

impl Project {
    /// `qualified@file` of the function `name`'s call targets, with
    /// `instantiates ` before a constructed class, sorted.
    pub fn calls_and_news(&self, name: &str) -> Vec<String> {
        let f = self
            .cg
            .store()
            .all_nodes()
            .unwrap()
            .into_iter()
            .find(|n| n.kind == codegraph_core::NodeKind::Function && n.name == name)
            .unwrap_or_else(|| panic!("no function {name}"));
        let mut out: Vec<String> = self
            .cg
            .store()
            .outgoing_edges(&f.id, &[EdgeKind::Calls, EdgeKind::Instantiates])
            .unwrap()
            .iter()
            .filter_map(|e| Some((e.kind, self.node(&e.target)?)))
            .map(|(k, n)| {
                let pre = if k == EdgeKind::Calls {
                    ""
                } else {
                    "instantiates "
                };
                format!("{pre}{}@{}", n.qualified_name, n.file_path)
            })
            .collect();
        out.sort();
        out
    }

    /// `file:qualified` of each references/calls target out of `file`'s
    /// nodes (only `qualified`'s, when given).
    pub fn refs_from(&self, file: &str, qualified: Option<&str>) -> Vec<String> {
        let ids: Vec<String> = self
            .nodes_in(file)
            .into_iter()
            .filter(|n| qualified.is_none_or(|q| n.qualified_name == q))
            .map(|n| n.id)
            .collect();
        self.cg
            .store()
            .outgoing_edges_from(&ids, &[EdgeKind::References, EdgeKind::Calls])
            .unwrap()
            .iter()
            .filter_map(|e| self.node(&e.target))
            .map(|t| format!("{}:{}", t.file_path, t.qualified_name))
            .collect()
    }

    /// Ids of what `node` calls.
    pub fn callee_ids(&self, node: &Node) -> Vec<String> {
        self.targets(node, EdgeKind::Calls)
            .into_iter()
            .map(|n| n.id)
            .collect()
    }
}
