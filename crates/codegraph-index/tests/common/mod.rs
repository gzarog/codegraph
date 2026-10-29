//! A small graph harness for resolution tests ported from upstream's vitest
//! suite: write a project into a temp dir, index it, query nodes and edges.

#![allow(dead_code)]

use codegraph_core::{Edge, EdgeKind, Node};
use codegraph_index::{index_project, IndexOptions, ProjectConfig};
use codegraph_store::Store;
use std::fs;
use std::path::Path;

pub struct Graph {
    pub dir: tempfile::TempDir,
    pub store: Store,
}

pub fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

impl Graph {
    /// Index a project made of `files` (relative path, content).
    pub fn new(files: &[(&str, &str)]) -> Graph {
        let dir = tempfile::tempdir().unwrap();
        for (rel, text) in files {
            write(dir.path(), rel, text);
        }
        let mut g = Graph {
            dir,
            store: Store::open_in_memory().unwrap(),
        };
        g.sync();
        g
    }

    /// Overwrite one file (without re-indexing).
    pub fn write(&self, rel: &str, text: &str) {
        write(self.dir.path(), rel, text);
    }

    /// Re-index what changed since the last run.
    pub fn sync(&mut self) {
        let root = self.dir.path().to_path_buf();
        index_project(
            &mut self.store,
            &root,
            &ProjectConfig::load(&root),
            &IndexOptions::default(),
        )
        .unwrap();
    }

    pub fn node(&self, id: &str) -> Node {
        self.store.node(id).unwrap().expect("edge target exists")
    }

    pub fn in_file(&self, file: &str) -> Vec<Node> {
        self.store.nodes_in_file(file).unwrap()
    }

    pub fn named(&self, name: &str) -> Vec<Node> {
        self.store.nodes_named(name).unwrap()
    }

    /// Nodes named `name` of kind `kind` (`"function"`, `"method"`…).
    pub fn named_kind(&self, name: &str, kind: &str) -> Vec<Node> {
        self.named(name)
            .into_iter()
            .filter(|n| n.kind.as_str() == kind)
            .collect()
    }

    /// The one node named `name` of kind `kind`, panicking otherwise.
    pub fn one(&self, name: &str, kind: &str) -> Node {
        let mut v = self.named_kind(name, kind);
        assert!(!v.is_empty(), "no {kind} named {name}");
        v.remove(0)
    }

    pub fn by_qualified(&self, qname: &str) -> Vec<Node> {
        self.store.nodes_by_qualified_name(qname).unwrap()
    }

    /// Outgoing edges of `sources` whose kind is one of `kinds` (all when empty).
    pub fn out(&self, sources: &[&Node], kinds: &[&str]) -> Vec<(Edge, Node)> {
        let ids: Vec<String> = sources.iter().map(|n| n.id.clone()).collect();
        let kinds: Vec<EdgeKind> = kinds.iter().map(|k| k.parse().unwrap()).collect();
        self.store
            .outgoing_edges_from(&ids, &kinds)
            .unwrap()
            .into_iter()
            .map(|e| {
                let t = self.node(&e.target);
                (e, t)
            })
            .collect()
    }

    /// Incoming edges of `target` whose kind is one of `kinds`, with sources.
    pub fn incoming(&self, target: &Node, kinds: &[&str]) -> Vec<(Edge, Node)> {
        let kinds: Vec<EdgeKind> = kinds.iter().map(|k| k.parse().unwrap()).collect();
        self.store
            .incoming_edges(&target.id, &kinds)
            .unwrap()
            .into_iter()
            .map(|e| {
                let s = self.node(&e.source);
                (e, s)
            })
            .collect()
    }

    /// Every non-`contains` edge out of `file`'s nodes.
    pub fn out_of_file(&self, file: &str) -> Vec<(Edge, Node)> {
        let nodes = self.in_file(file);
        let refs: Vec<&Node> = nodes.iter().collect();
        self.out(&refs, &[])
            .into_iter()
            .filter(|(e, _)| e.kind != EdgeKind::Contains)
            .collect()
    }

    /// Qualified names `node` calls.
    pub fn calls(&self, node: &Node) -> Vec<String> {
        self.out(&[node], &["calls"])
            .into_iter()
            .map(|(_, t)| t.qualified_name)
            .collect()
    }

    /// Qualified names the function or method called `name` calls (every one so named).
    pub fn calls_of(&self, name: &str) -> Vec<String> {
        let froms: Vec<Node> = self
            .named(name)
            .into_iter()
            .filter(|n| matches!(n.kind.as_str(), "function" | "method"))
            .collect();
        assert!(!froms.is_empty(), "no function or method named {name}");
        let refs: Vec<&Node> = froms.iter().collect();
        self.out(&refs, &["calls"])
            .into_iter()
            .map(|(_, t)| t.qualified_name)
            .collect()
    }
}

impl Graph {
    /// What `file` links to `name` on `line`, as `kind qualifiedName file@line`, sorted, deduped.
    pub fn targets_on(&self, file: &str, line: u32, name: &str) -> Vec<String> {
        let mut out: Vec<String> = self
            .out_of_file(file)
            .into_iter()
            .filter(|(e, t)| e.line == Some(line) && t.name == name)
            .map(|(_, t)| {
                format!(
                    "{} {} {}@{}",
                    t.kind.as_str(),
                    t.qualified_name,
                    t.file_path,
                    t.start_line
                )
            })
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// What `name`, written in `file` as an edge of one of `kinds`, links to,
    /// as `file@line` of each target, sorted, deduped.
    pub fn targets_of(&self, file: &str, name: &str, kinds: &[&str]) -> Vec<String> {
        let nodes = self.in_file(file);
        let refs: Vec<&Node> = nodes.iter().collect();
        let mut out: Vec<String> = self
            .out(&refs, kinds)
            .into_iter()
            .filter(|(_, t)| t.name == name)
            .map(|(_, t)| format!("{}@{}", t.file_path, t.start_line))
            .collect();
        out.sort();
        out.dedup();
        out
    }

    /// The unresolved (pending or failed) references made from `file`'s symbols.
    pub fn open_refs_in_file(&self, file: &str) -> Vec<codegraph_core::UnresolvedReference> {
        self.in_file(file)
            .iter()
            .flat_map(|n| self.store.open_refs_from(&n.id).unwrap())
            .collect()
    }

    /// Sorted qualified names the first function or method named `name` calls.
    pub fn callees(&self, name: &str) -> Vec<String> {
        let f = self
            .named(name)
            .into_iter()
            .find(|n| matches!(n.kind.as_str(), "function" | "method"))
            .unwrap_or_else(|| panic!("no function or method named {name}"));
        let mut v = self.calls(&f);
        v.sort();
        v
    }
}

/// `file:qualifiedName` of a node.
pub fn fq(n: &Node) -> String {
    format!("{}:{}", n.file_path, n.qualified_name)
}

/// `kind file:line` of a node.
pub fn at(n: &Node) -> String {
    format!("{} {}:{}", n.kind.as_str(), n.file_path, n.start_line)
}
