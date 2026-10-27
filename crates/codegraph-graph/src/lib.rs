//! Queries over the stored graph: callers/callees, impact radius, paths
//! between symbols, type hierarchies and unreferenced code.

use codegraph_core::{Direction, Edge, EdgeKind, Node, NodeKind, Subgraph, TraversalOptions};
use codegraph_store::{Result, Store};
use serde::Serialize;
use std::collections::{HashMap, HashSet, VecDeque};

/// Edges that mean "uses": the dependency relations impact analysis follows.
pub const DEPENDENCY_EDGES: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::References,
    EdgeKind::Instantiates,
    EdgeKind::Extends,
    EdgeKind::Implements,
    EdgeKind::TypeOf,
    EdgeKind::Returns,
    EdgeKind::Decorates,
    EdgeKind::Overrides,
    EdgeKind::Imports,
    EdgeKind::Navigates,
];

/// Edges a call path follows.
pub const FLOW_EDGES: &[EdgeKind] = &[
    EdgeKind::Calls,
    EdgeKind::Instantiates,
    EdgeKind::References,
    EdgeKind::Navigates,
    EdgeKind::Decorates,
];

/// A neighbouring node and the edge that connects it.
#[derive(Debug, Clone, Serialize)]
pub struct Neighbor {
    pub node: Node,
    pub edge: Edge,
}

/// Symbols affected by changing one symbol, by distance.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Impact {
    pub root: Option<Node>,
    /// `levels[0]` are direct dependents, `levels[1]` their dependents, …
    pub levels: Vec<Vec<Neighbor>>,
    pub files: Vec<String>,
    pub truncated: bool,
}

impl Impact {
    pub fn total(&self) -> usize {
        self.levels.iter().map(Vec::len).sum()
    }
}

/// One step of a path: the node, and the edge that led to it (none for the start).
#[derive(Debug, Clone, Serialize)]
pub struct PathStep {
    pub node: Node,
    pub via: Option<Edge>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TypeHierarchy {
    pub ancestors: Vec<Node>,
    pub descendants: Vec<Node>,
}

pub struct Graph<'a> {
    store: &'a Store,
}

/// Wrapping a symbol definition in its file: `contains` edges from the file
/// and enclosing types don't count as uses.
fn is_structural(e: &Edge) -> bool {
    e.kind == EdgeKind::Contains
}

impl<'a> Graph<'a> {
    pub fn new(store: &'a Store) -> Self {
        Graph { store }
    }

    fn neighbors(&self, edges: Vec<Edge>, pick_target: bool) -> Result<Vec<Neighbor>> {
        let ids: Vec<String> = edges
            .iter()
            .map(|e| {
                if pick_target {
                    e.target.clone()
                } else {
                    e.source.clone()
                }
            })
            .collect();
        let nodes = self.store.nodes_by_ids(&ids)?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for e in edges {
            let id = if pick_target { &e.target } else { &e.source };
            if let Some(n) = nodes.get(id) {
                if seen.insert((n.id.clone(), e.kind, e.line)) {
                    out.push(Neighbor {
                        node: n.clone(),
                        edge: e,
                    });
                }
            }
        }
        Ok(out)
    }

    /// Who calls/uses `id` (call-like edges by default).
    pub fn callers(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<Neighbor>> {
        let kinds = if kinds.is_empty() { FLOW_EDGES } else { kinds };
        let edges = self.store.incoming_edges(id, kinds)?;
        let mut out = self.neighbors(edges, false)?;
        // A file node "calls" through module-level code; keep those last.
        out.sort_by_key(|n| {
            (
                n.node.kind == NodeKind::File,
                n.node.file_path.clone(),
                n.node.start_line,
            )
        });
        Ok(out)
    }

    /// What `id` calls/uses.
    pub fn callees(&self, id: &str, kinds: &[EdgeKind]) -> Result<Vec<Neighbor>> {
        let kinds = if kinds.is_empty() { FLOW_EDGES } else { kinds };
        let mut edges = self.store.outgoing_edges(id, kinds)?;
        edges.sort_by_key(|e| (e.line, e.column));
        self.neighbors(edges, true)
    }

    /// Everything that (transitively) depends on `id`, up to `depth` levels.
    /// Members count as their container: changing a class affects users of its methods.
    pub fn impact(&self, id: &str, depth: u32, limit: usize) -> Result<Impact> {
        let mut impact = Impact {
            root: self.store.node(id)?,
            ..Default::default()
        };
        let mut seen: HashSet<String> = [id.to_string()].into();
        let mut frontier: Vec<String> = vec![id.to_string()];
        // A type's members are part of it.
        for member in self.store.outgoing_edges(id, &[EdgeKind::Contains])? {
            if seen.insert(member.target.clone()) {
                frontier.push(member.target);
            }
        }
        let mut files: HashSet<String> = HashSet::new();
        for _ in 0..depth.max(1) {
            let edges = self.store.incoming_edges_to(&frontier, DEPENDENCY_EDGES)?;
            let level: Vec<Neighbor> = self
                .neighbors(edges, false)?
                .into_iter()
                .filter(|n| seen.insert(n.node.id.clone()))
                .collect();
            if level.is_empty() {
                break;
            }
            frontier = level.iter().map(|n| n.node.id.clone()).collect();
            for n in &level {
                files.insert(n.node.file_path.clone());
            }
            let total = impact.total() + level.len();
            impact.levels.push(level);
            if total >= limit {
                impact.truncated = true;
                break;
            }
        }
        let mut files: Vec<String> = files.into_iter().collect();
        files.sort();
        impact.files = files;
        Ok(impact)
    }

    /// Breadth-first traversal from `start`.
    pub fn traverse(&self, start: &str, opts: &TraversalOptions) -> Result<Subgraph> {
        let mut sub = Subgraph {
            roots: vec![start.to_string()],
            ..Default::default()
        };
        let max_depth = opts.max_depth.unwrap_or(2);
        let limit = opts.limit.unwrap_or(200);
        let Some(root) = self.store.node(start)? else {
            return Ok(sub);
        };
        if opts.include_start {
            sub.nodes.insert(root);
        }
        let mut seen: HashSet<String> = [start.to_string()].into();
        let mut queue: VecDeque<(String, u32)> = [(start.to_string(), 0)].into();
        while let Some((id, d)) = queue.pop_front() {
            if d >= max_depth || sub.nodes.len() >= limit {
                continue;
            }
            let mut edges = Vec::new();
            if matches!(opts.direction, Direction::Outgoing | Direction::Both) {
                edges.extend(
                    self.store
                        .outgoing_edges(&id, &opts.edge_kinds)?
                        .into_iter()
                        .map(|e| (e, true)),
                );
            }
            if matches!(opts.direction, Direction::Incoming | Direction::Both) {
                edges.extend(
                    self.store
                        .incoming_edges(&id, &opts.edge_kinds)?
                        .into_iter()
                        .map(|e| (e, false)),
                );
            }
            let other_ids: Vec<String> = edges
                .iter()
                .map(|(e, out)| {
                    if *out {
                        e.target.clone()
                    } else {
                        e.source.clone()
                    }
                })
                .collect();
            let nodes = self.store.nodes_by_ids(&other_ids)?;
            for (e, out) in edges {
                let other = if out { &e.target } else { &e.source };
                let Some(n) = nodes.get(other) else { continue };
                if !opts.node_kinds.is_empty() && !opts.node_kinds.contains(&n.kind) {
                    continue;
                }
                if seen.insert(other.clone()) {
                    sub.nodes.insert(n.clone());
                    queue.push_back((other.clone(), d + 1));
                }
                sub.edges.push(e);
            }
        }
        Ok(sub)
    }

    /// Shortest path from `from` to `to` along `kinds` edges (either
    /// direction when `undirected`), at most `max_depth` hops.
    pub fn find_path(
        &self,
        from: &str,
        to: &str,
        kinds: &[EdgeKind],
        max_depth: usize,
        undirected: bool,
    ) -> Result<Option<Vec<PathStep>>> {
        if from == to {
            return Ok(self
                .store
                .node(from)?
                .map(|n| vec![PathStep { node: n, via: None }]));
        }
        let kinds = if kinds.is_empty() { FLOW_EDGES } else { kinds };
        let mut prev: HashMap<String, (String, Edge)> = HashMap::new();
        let mut frontier = vec![from.to_string()];
        let mut seen: HashSet<String> = [from.to_string()].into();
        let mut found = false;
        'outer: for _ in 0..max_depth {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            let mut edges: Vec<(Edge, bool)> = self
                .store
                .outgoing_edges_from(&frontier, kinds)?
                .into_iter()
                .map(|e| (e, true))
                .collect();
            if undirected {
                edges.extend(
                    self.store
                        .incoming_edges_to(&frontier, kinds)?
                        .into_iter()
                        .map(|e| (e, false)),
                );
            }
            for (e, forward) in edges {
                let (here, there) = if forward {
                    (e.source.clone(), e.target.clone())
                } else {
                    (e.target.clone(), e.source.clone())
                };
                if seen.insert(there.clone()) {
                    prev.insert(there.clone(), (here, e));
                    if there == to {
                        found = true;
                        break 'outer;
                    }
                    next.push(there);
                }
            }
            frontier = next;
            if frontier.len() > 20_000 {
                break;
            }
        }
        if !found {
            return Ok(None);
        }
        let mut chain: Vec<(String, Option<Edge>)> = Vec::new();
        let mut cur = to.to_string();
        while let Some((p, e)) = prev.get(&cur) {
            chain.push((cur.clone(), Some(e.clone())));
            cur = p.clone();
        }
        chain.push((from.to_string(), None));
        chain.reverse();
        let ids: Vec<String> = chain.iter().map(|(id, _)| id.clone()).collect();
        let nodes = self.store.nodes_by_ids(&ids)?;
        Ok(Some(
            chain
                .into_iter()
                .filter_map(|(id, via)| {
                    nodes.get(&id).map(|n| PathStep {
                        node: n.clone(),
                        via,
                    })
                })
                .collect(),
        ))
    }

    /// Supertypes (transitively) and subtypes (transitively) of a type.
    pub fn type_hierarchy(&self, id: &str, max: usize) -> Result<TypeHierarchy> {
        let kinds = [EdgeKind::Extends, EdgeKind::Implements];
        let walk = |up: bool| -> Result<Vec<Node>> {
            let mut out = Vec::new();
            let mut seen: HashSet<String> = [id.to_string()].into();
            let mut frontier = vec![id.to_string()];
            while !frontier.is_empty() && out.len() < max {
                let edges = if up {
                    self.store.outgoing_edges_from(&frontier, &kinds)?
                } else {
                    self.store.incoming_edges_to(&frontier, &kinds)?
                };
                let ids: Vec<String> = edges
                    .iter()
                    .map(|e| {
                        if up {
                            e.target.clone()
                        } else {
                            e.source.clone()
                        }
                    })
                    .filter(|i| seen.insert(i.clone()))
                    .collect();
                let nodes = self.store.nodes_by_ids(&ids)?;
                frontier = Vec::new();
                for i in ids {
                    if let Some(n) = nodes.get(&i) {
                        out.push(n.clone());
                        frontier.push(i);
                    }
                }
            }
            Ok(out)
        };
        Ok(TypeHierarchy {
            ancestors: walk(true)?,
            descendants: walk(false)?,
        })
    }

    /// The chain of containers of a node (file → class → method …), outermost first.
    pub fn ancestors(&self, id: &str) -> Result<Vec<Node>> {
        let mut out = Vec::new();
        let mut cur = id.to_string();
        for _ in 0..16 {
            let parents = self.store.incoming_edges(&cur, &[EdgeKind::Contains])?;
            let Some(p) = parents.first() else { break };
            let Some(n) = self.store.node(&p.source)? else {
                break;
            };
            cur = n.id.clone();
            out.push(n);
        }
        out.reverse();
        Ok(out)
    }

    /// Direct children (`contains`) of a node, in source order.
    pub fn children(&self, id: &str) -> Result<Vec<Node>> {
        let mut out: Vec<Node> = self
            .callees(id, &[EdgeKind::Contains])?
            .into_iter()
            .map(|n| n.node)
            .collect();
        out.sort_by_key(|n| (n.start_line, n.start_column));
        Ok(out)
    }

    /// Symbols nothing references, excluding entry points that are expected
    /// to be unreferenced (exported API, `main`, tests, constructors).
    pub fn dead_code(&self, limit: usize) -> Result<Vec<Node>> {
        let kinds = [
            NodeKind::Function,
            NodeKind::Method,
            NodeKind::Class,
            NodeKind::Struct,
            NodeKind::Interface,
        ];
        let candidates = self.store.unreferenced_nodes(&kinds, limit * 4)?;
        Ok(candidates
            .into_iter()
            .filter(|n| {
                !n.is_exported
                    && !matches!(
                        n.name.as_str(),
                        "main" | "constructor" | "__init__" | "init" | "new" | "setUp" | "tearDown"
                    )
                    && !n.name.starts_with("test")
                    && !n.name.starts_with("Test")
                    && !n.name.starts_with("__")
                    && !codegraph_core::search::is_test_path(&n.file_path)
                    && n.decorators.is_empty()
            })
            .take(limit)
            .collect())
    }

    /// How many distinct symbols use each of `ids` (excluding containment).
    pub fn dependent_counts(&self, ids: &[String]) -> Result<HashMap<String, u64>> {
        self.store.incoming_counts(ids, DEPENDENCY_EDGES)
    }

    /// Edges among a set of nodes, minus structural ones.
    pub fn edges_among(&self, ids: &[String]) -> Result<Vec<Edge>> {
        Ok(self
            .store
            .edges_among(ids, &[])?
            .into_iter()
            .filter(|e| !is_structural(e))
            .collect())
    }
}

#[cfg(test)]
mod tests;
