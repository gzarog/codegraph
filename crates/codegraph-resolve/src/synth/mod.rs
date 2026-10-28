//! Synthesized edges for dispatch that no single reference names: a queued
//! task onto its body, a published event onto its listeners, a store action
//! called through its factory, a registry entry dispatched by a runtime key.
//!
//! Each pass reads the resolved graph plus the source of the files it
//! concerns and proposes `calls` edges. Passes run after resolution, every
//! time the index changes; their edges carry `synthesizedBy: "synth"` so a
//! re-run replaces them wholesale.

mod backend;
mod callbacks;
mod go;
mod js;
mod modules;
mod native;
mod tier;
mod types;

use codegraph_core::{Edge, EdgeKind, Language, Node, NodeKind, Provenance};
use codegraph_store::Store;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::rc::Rc;

/// The `synthesizedBy` value on every edge a pass produces.
pub const PRODUCER: &str = "synth";

/// What one pass proposes: `source` calls `target` at `line`, through `via`.
#[derive(Debug, Clone)]
pub struct Proposal {
    pub source: usize,
    pub target: usize,
    pub line: u32,
    pub via: String,
    pub kind: EdgeKind,
}

pub struct Ctx<'a> {
    root: &'a Path,
    /// The graph as resolution left it, for edge lookups.
    pub store: Option<&'a Store>,
    pub nodes: Vec<Node>,
    by_name: HashMap<String, Vec<usize>>,
    by_qname: HashMap<String, Vec<usize>>,
    by_file: HashMap<String, Vec<usize>>,
    pub files: Vec<(String, Language)>,
    generated: HashSet<String>,
    by_id: HashMap<String, usize>,
    /// Type → the methods it contains (passes may add cross-file ones).
    children: RefCell<HashMap<usize, Vec<usize>>>,
    /// Type → its supertypes, and whether a pass synthesized the link.
    supers: RefCell<HashMap<usize, Vec<(usize, bool)>>>,
    sources: RefCell<HashMap<String, Option<Rc<str>>>>,
    stripped: RefCell<HashMap<String, Rc<str>>>,
}

impl<'a> Ctx<'a> {
    pub fn new(root: &'a Path, nodes: Vec<Node>, files: Vec<(String, Language)>) -> Self {
        let mut by_id = HashMap::with_capacity(nodes.len());
        let mut by_name: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_qname: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_file: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, n) in nodes.iter().enumerate() {
            by_id.insert(n.id.clone(), i);
            by_name.entry(n.name.clone()).or_default().push(i);
            by_qname
                .entry(n.qualified_name.clone())
                .or_default()
                .push(i);
            by_file.entry(n.file_path.clone()).or_default().push(i);
        }
        Ctx {
            root,
            store: None,
            nodes,
            by_name,
            by_qname,
            by_file,
            files,
            generated: HashSet::new(),
            by_id,
            children: RefCell::default(),
            supers: RefCell::default(),
            sources: RefCell::new(HashMap::new()),
            stripped: RefCell::new(HashMap::new()),
        }
    }

    pub fn node(&self, i: usize) -> &Node {
        &self.nodes[i]
    }

    pub fn index_of(&self, id: &str) -> Option<usize> {
        self.by_id.get(id).copied()
    }

    pub fn is_generated(&self, file: &str) -> bool {
        self.generated.contains(file)
    }

    /// Record the graph's `contains` (type → method) and supertype edges.
    pub fn add_structure(&mut self, contains: &[Edge], supertypes: &[Edge]) {
        let children = self.children.get_mut();
        for e in contains {
            if let (Some(a), Some(b)) = (self.by_id.get(&e.source), self.by_id.get(&e.target)) {
                if self.nodes[*b].kind == NodeKind::Method {
                    children.entry(*a).or_default().push(*b);
                }
            }
        }
        let supers = self.supers.get_mut();
        for e in supertypes {
            if let (Some(&a), Some(&b)) = (self.by_id.get(&e.source), self.by_id.get(&e.target)) {
                let v = supers.entry(a).or_default();
                if a != b && !v.iter().any(|(x, _)| *x == b) {
                    v.push((b, false));
                }
            }
        }
    }

    /// The methods a type contains.
    pub fn methods_of(&self, ty: usize) -> Vec<usize> {
        self.children.borrow().get(&ty).cloned().unwrap_or_default()
    }

    pub fn add_method(&self, ty: usize, method: usize) {
        let mut c = self.children.borrow_mut();
        let v = c.entry(ty).or_default();
        if !v.contains(&method) {
            v.push(method);
        }
    }

    /// A type's supertypes, with whether each link was synthesized.
    pub fn supers_of(&self, ty: usize) -> Vec<(usize, bool)> {
        self.supers.borrow().get(&ty).cloned().unwrap_or_default()
    }

    pub fn add_super(&self, ty: usize, sup: usize) {
        let mut s = self.supers.borrow_mut();
        let v = s.entry(ty).or_default();
        if !v.iter().any(|(x, _)| *x == sup) {
            v.push((sup, true));
        }
    }

    pub fn named(&self, name: &str) -> impl Iterator<Item = usize> + '_ {
        self.by_name.get(name).into_iter().flatten().copied()
    }

    pub fn qualified(&self, qname: &str) -> impl Iterator<Item = usize> + '_ {
        self.by_qname.get(qname).into_iter().flatten().copied()
    }

    pub fn in_file(&self, file: &str) -> &[usize] {
        self.by_file.get(file).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn has_language(&self, langs: &[Language]) -> bool {
        self.files.iter().any(|(_, l)| langs.contains(l))
    }

    /// Files whose language is one of `langs`.
    pub fn files_of<'s>(&'s self, langs: &'s [Language]) -> impl Iterator<Item = &'s str> + 's {
        self.files
            .iter()
            .filter(move |(_, l)| langs.contains(l))
            .map(|(p, _)| p.as_str())
    }

    /// A file's source, read once.
    pub fn source(&self, file: &str) -> Option<Rc<str>> {
        if let Some(s) = self.sources.borrow().get(file) {
            return s.clone();
        }
        let text = std::fs::read(self.root.join(file))
            .ok()
            .map(|b| Rc::<str>::from(String::from_utf8_lossy(&b).as_ref()));
        self.sources
            .borrow_mut()
            .insert(file.to_string(), text.clone());
        text
    }

    /// A file's source with comments blanked (newlines kept), read once.
    pub fn stripped(&self, file: &str, lang: Language) -> Option<Rc<str>> {
        if let Some(s) = self.stripped.borrow().get(file) {
            return Some(s.clone());
        }
        let src = self.source(file)?;
        let s: Rc<str> = codegraph_frameworks::strip::strip_comments(&src, lang).into();
        self.stripped
            .borrow_mut()
            .insert(file.to_string(), s.clone());
        Some(s)
    }

    /// The innermost function, method or component in `file` around `line`.
    pub fn enclosing_fn(&self, file: &str, line: u32) -> Option<usize> {
        let mut best: Option<usize> = None;
        for &i in self.in_file(file) {
            let n = &self.nodes[i];
            if !matches!(
                n.kind,
                NodeKind::Function | NodeKind::Method | NodeKind::Component
            ) {
                continue;
            }
            if n.start_line <= line
                && line <= n.end_line
                && best.is_none_or(|b| n.start_line >= self.nodes[b].start_line)
            {
                best = Some(i);
            }
        }
        best
    }

    /// The first `kind` node in `file` named `name` within `owner`'s lines.
    pub fn member_of(&self, owner: usize, kind: NodeKind, name: &str) -> Option<usize> {
        let o = &self.nodes[owner];
        self.in_file(&o.file_path).iter().copied().find(|&i| {
            let n = &self.nodes[i];
            n.kind == kind
                && n.name == name
                && n.start_line >= o.start_line
                && n.start_line <= o.end_line
                && i != owner
        })
    }

    /// The lines `from..=to` (1-based) of a file's source.
    pub fn lines(&self, file: &str, from: u32, to: u32) -> Option<String> {
        let src = self.source(file)?;
        Some(
            src.lines()
                .skip(from.saturating_sub(1) as usize)
                .take((to + 1).saturating_sub(from.max(1)) as usize)
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

/// 1-based line of each byte offset in a text, by binary search.
pub struct LineIndex(Vec<usize>);

impl LineIndex {
    pub fn new(text: &str) -> Self {
        LineIndex(text.match_indices('\n').map(|(i, _)| i).collect())
    }

    pub fn line(&self, offset: usize) -> u32 {
        self.0.partition_point(|&nl| nl < offset) as u32 + 1
    }
}

/// Collects one pass's proposals, skipping self-loops and repeats and
/// stopping a site at `cap` targets.
pub struct Sink<'p> {
    out: &'p mut Vec<Proposal>,
    seen: HashSet<(usize, usize)>,
}

impl<'p> Sink<'p> {
    pub fn new(out: &'p mut Vec<Proposal>) -> Self {
        Sink {
            out,
            seen: HashSet::new(),
        }
    }

    /// Record `source` calls `target`; false when it was a repeat or a self-loop.
    pub fn push(&mut self, source: usize, target: usize, line: u32, via: &str) -> bool {
        self.push_kind(source, target, line, via, EdgeKind::Calls)
    }

    pub fn push_kind(
        &mut self,
        source: usize,
        target: usize,
        line: u32,
        via: &str,
        kind: EdgeKind,
    ) -> bool {
        if source == target || !self.seen.insert((source, target)) {
            return false;
        }
        self.out.push(Proposal {
            source,
            target,
            line,
            via: via.to_string(),
            kind,
        });
        true
    }
}

type Pass = fn(&Ctx<'_>, &mut Vec<Proposal>);

/// The passes, in merge order: when two propose the same pair, the first wins.
const PASSES: &[(&str, Pass)] = &[
    // Structure first: later passes read the methods and supertypes these add.
    ("go-method-contains", types::go_method_contains),
    ("go-implements", types::go_implements),
    ("override", types::overrides),
    ("kotlin-expect-actual", types::kotlin_expect_actual),
    ("go-grpc", types::go_grpc_stubs),
    ("queue", tier::queues),
    ("bus", tier::events),
    ("callback", callbacks::field_channels),
    ("closure-collection", callbacks::closure_collections),
    ("react-render", callbacks::react_render),
    ("flutter-build", callbacks::flutter_build),
    ("gin-middleware", callbacks::gin_middleware),
    ("go-func-field", go::func_fields),
    ("sveltekit-load", callbacks::sveltekit_load),
    ("redux-thunk", js::redux_thunks),
    ("object-registry", js::object_registries),
    ("rtk-query", js::rtk_query),
    ("pinia", js::pinia),
    ("vuex", js::vuex),
    ("celery", backend::celery),
    ("spring-event", backend::spring_events),
    ("mediatr", backend::mediatr),
    ("sidekiq", backend::sidekiq),
    ("laravel-event", backend::laravel_events),
    ("erlang-behaviour", modules::erlang_behaviours),
    ("nix-option", modules::nix_options),
    ("rn-event", native::rn_events),
    ("fabric-native", native::fabric_native),
];

/// Run every pass over the store's graph and replace the synthesized edges.
pub fn synthesize(store: &mut Store, root: &Path) -> Result<usize, codegraph_store::StoreError> {
    store.delete_synthesized_edges(PRODUCER)?;
    let records = store.all_files()?;
    let generated = records
        .iter()
        .filter(|f| f.generated)
        .map(|f| f.path.clone())
        .collect();
    let files = records.into_iter().map(|f| (f.path, f.language)).collect();
    let edges = {
        let mut ctx = Ctx::new(root, store.all_nodes()?, files);
        ctx.generated = generated;
        let mut supertypes = store.edges_of_kind(EdgeKind::Extends)?;
        supertypes.extend(store.edges_of_kind(EdgeKind::Implements)?);
        ctx.add_structure(&store.edges_of_kind(EdgeKind::Contains)?, &supertypes);
        ctx.store = Some(&*store);
        edges(&ctx)
    };
    store.insert_edges(&edges)
}

/// Every pass's proposals as edges, deduplicated in pass order.
pub fn edges(ctx: &Ctx<'_>) -> Vec<Edge> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for (family, pass) in PASSES {
        let mut proposals = Vec::new();
        let t0 = std::time::Instant::now();
        pass(ctx, &mut proposals);
        if std::env::var_os("CODEGRAPH_SYNTH_TIMINGS").is_some() {
            eprintln!(
                "[synth] {family}: {:?}, {} proposed",
                t0.elapsed(),
                proposals.len()
            );
        }
        for p in proposals {
            if !seen.insert((p.source, p.target, p.kind)) {
                continue;
            }
            let (s, t) = (ctx.node(p.source), ctx.node(p.target));
            let mut e = Edge::new(&s.id, &t.id, p.kind);
            e.line = Some(p.line);
            e.provenance = Some(Provenance::Heuristic);
            e.metadata = Some(serde_json::json!({
                "synthesizedBy": PRODUCER,
                "dispatch": family,
                "key": p.via,
                "registeredAt": format!("{}:{}", s.file_path, p.line),
                "resolvedBy": "dispatch",
                "confidence": 0.6,
            }));
            out.push(e);
        }
    }
    out
}
