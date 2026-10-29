//! Cross-file reference resolution: turns the references extraction found
//! into edges between symbols.

pub mod dispatch;
pub mod imports;
pub mod matcher;
pub mod modules;
pub mod rust;
pub mod symbols;
pub mod synth;

use codegraph_core::{Edge, Provenance, ReferenceKind, UnresolvedReference};
use codegraph_store::Store;
use matcher::Matcher;
use modules::Modules;
use std::path::Path;
use symbols::Symbols;

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error(transparent)]
    Store(#[from] codegraph_store::StoreError),
}

#[derive(Debug, Clone, Default)]
pub struct ResolveOutcome {
    pub attempted: usize,
    pub resolved: usize,
    pub failed: usize,
}

const PAGE: usize = 5000;

/// An `f32` as the `f64` its shortest decimal spells, so `0.9` is stored as
/// `0.9` and not `0.8999999761581421`.
fn as_decimal(x: f32) -> f64 {
    x.to_string().parse().unwrap_or(f64::from(x))
}

fn to_edge(r: &UnresolvedReference, syms: &Symbols, m: &matcher::Match) -> (Edge, String) {
    let target = syms.get(m.target);
    let mut edge = Edge::new(&r.from_node_id, &target.id, r.reference_kind.edge_kind());
    edge.line = Some(r.line);
    edge.column = Some(r.column);
    let mut meta = serde_json::json!({
        "resolvedBy": m.strategy,
        "confidence": as_decimal(m.confidence),
        "refName": r.reference_name,
    });
    if r.reference_kind == ReferenceKind::FunctionRef {
        meta["functionRef"] = serde_json::Value::Bool(true);
    }
    edge.metadata = Some(meta);
    edge.provenance = Some(if m.is_heuristic() {
        Provenance::Heuristic
    } else {
        Provenance::TreeSitter
    });
    (edge, target.file.clone())
}

/// Resolve every pending reference in the store.
pub fn resolve_pending(
    store: &mut Store,
    root: &Path,
    progress: &dyn Fn(usize, usize),
) -> Result<ResolveOutcome, ResolveError> {
    let timings = std::env::var_os("CODEGRAPH_TIMINGS").is_some();
    let t = std::time::Instant::now();
    let lap = |what: &str| {
        if timings {
            eprintln!("[resolve] {what}: {:?}", t.elapsed());
        }
    };
    // Joins depend on binds and routes anywhere in the project: redo them.
    store.reopen_dispatch_refs()?;
    lap("reopen");
    let total = store.count_refs(codegraph_store::RefStatus::Pending)? as usize;
    let mut outcome = ResolveOutcome::default();
    if total == 0 {
        return Ok(outcome);
    }
    let mut syms = Symbols::load(store)?;
    lap("symbols");
    let modules = Modules::load(root, &syms);
    lap("modules");

    // Pass 1: supertypes, so member lookups can walk inheritance in pass 2.
    let mut pending = Vec::new();
    let mut after = 0;
    loop {
        let page = store.pending_refs_after(after, PAGE)?;
        let Some(last) = page.last() else { break };
        after = last.row_id.unwrap_or(after);
        pending.extend(page);
    }
    let (joins, pending): (Vec<_>, Vec<_>) = pending
        .into_iter()
        .partition(|r| dispatch::is_join(&r.reference_name));
    let (supers, rest): (Vec<_>, Vec<_>) = pending.into_iter().partition(|r| {
        matches!(
            r.reference_kind,
            ReferenceKind::Extends | ReferenceKind::Implements
        )
    });

    let mut resolved = Vec::new();
    let mut failed = Vec::new();
    {
        let mut new_supers = Vec::new();
        {
            let mut matcher = Matcher::new(&syms, &modules, root);
            let rust = rust::Ctx::new(&syms, root);
            for r in &supers {
                match resolve_one(&mut matcher, &rust, &syms, r) {
                    Some(m) => {
                        if let Some(from) = syms.by_id(&r.from_node_id) {
                            new_supers.push((from, m.target));
                        }
                        resolved.push((r.row_id.unwrap_or(0), to_edge(r, &syms, &m)));
                    }
                    None => failed.push(r.row_id.unwrap_or(0)),
                }
            }
        }
        for (a, b) in new_supers {
            syms.add_super(a, b);
        }
    }
    outcome.attempted += supers.len();
    progress(supers.len(), total);

    let mut matcher = Matcher::new(&syms, &modules, root);
    let rust = rust::Ctx::new(&syms, root);
    for (i, r) in rest.iter().enumerate() {
        let handler = dispatch::bind_handler(r);
        let m = match handler {
            Some(h) => resolve_named(&mut matcher, &rust, &syms, r, ReferenceKind::References, h),
            None => resolve_one(&mut matcher, &rust, &syms, r),
        };
        match m {
            Some(m) => {
                let (mut edge, file) = to_edge(r, &syms, &m);
                if handler.is_some() {
                    dispatch::mark_bind(&mut edge, &r.reference_name);
                }
                resolved.push((r.row_id.unwrap_or(0), (edge, file)));
            }
            None => failed.push(r.row_id.unwrap_or(0)),
        }
        if (i + 1) % 2000 == 0 {
            progress(supers.len() + i + 1, total);
        }
    }
    outcome.attempted += rest.len();
    lap("matched");
    let rows: Vec<(i64, Edge, String)> = resolved
        .into_iter()
        .map(|(id, (e, f))| (id, e, f))
        .collect();
    store.apply_resolutions(&rows, &failed)?;
    lap("applied");

    // Pass 3: join fires to binds and requests to routes; one site may reach
    // several targets.
    let binds = dispatch::Binds::new(store.resolved_binds()?);
    let routes = dispatch::Routes::new(&syms);
    let mut rows = Vec::new();
    let mut extra = Vec::new();
    let mut join_failed = Vec::new();
    for r in &joins {
        let targets = dispatch::targets(r, &binds, &routes, &syms);
        let mut edges = targets.into_iter().map(|t| dispatch::edge(r, &syms, t));
        match edges.next() {
            Some(first) => {
                rows.push((r.row_id.unwrap_or(0), first.0, first.1));
                extra.extend(edges.map(|(e, _)| e));
            }
            None => join_failed.push(r.row_id.unwrap_or(0)),
        }
    }
    outcome.attempted += joins.len();
    store.apply_resolutions(&rows, &join_failed)?;
    store.insert_edges(&extra)?;
    outcome.resolved = outcome.attempted - failed.len() - join_failed.len();
    outcome.failed = failed.len() + join_failed.len();
    progress(total, total);
    Ok(outcome)
}

fn resolve_one(
    matcher: &mut Matcher<'_>,
    rust: &rust::Ctx<'_>,
    syms: &Symbols,
    r: &UnresolvedReference,
) -> Option<matcher::Match> {
    resolve_named(matcher, rust, syms, r, r.reference_kind, &r.reference_name)
}

fn resolve_named(
    matcher: &mut Matcher<'_>,
    rust: &rust::Ctx<'_>,
    syms: &Symbols,
    r: &UnresolvedReference,
    kind: ReferenceKind,
    name: &str,
) -> Option<matcher::Match> {
    let from = syms.by_id(&r.from_node_id)?;
    let file = r
        .file_path
        .clone()
        .unwrap_or_else(|| syms.get(from).file.clone());
    let lang = r.language.unwrap_or(syms.get(from).language);
    let m = if lang == codegraph_core::Language::Rust {
        let rr = rust::Ref {
            from,
            file: &file,
            line: r.line,
            column: r.column,
            name,
            kind,
        };
        rust.resolve(&rr, &mut |n| matcher.resolve(from, &file, lang, kind, n))?
    } else {
        matcher.resolve(from, &file, lang, kind, name)?
    };
    // Import refs come from the file node; everything else must not point at a file.
    if r.reference_kind != ReferenceKind::Imports
        && syms.get(m.target).kind == codegraph_core::NodeKind::File
    {
        return None;
    }
    Some(m)
}
