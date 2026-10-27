//! Cross-file reference resolution: turns the references extraction found
//! into edges between symbols.

pub mod imports;
pub mod matcher;
pub mod modules;
pub mod symbols;

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

fn to_edge(r: &UnresolvedReference, syms: &Symbols, m: &matcher::Match) -> (Edge, String) {
    let target = syms.get(m.target);
    let mut edge = Edge::new(&r.from_node_id, &target.id, r.reference_kind.edge_kind());
    edge.line = Some(r.line);
    edge.column = Some(r.column);
    let mut meta = serde_json::json!({ "resolvedBy": m.strategy, "confidence": m.confidence });
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
    let total = store.count_refs(codegraph_store::RefStatus::Pending)? as usize;
    let mut outcome = ResolveOutcome::default();
    if total == 0 {
        return Ok(outcome);
    }
    let mut syms = Symbols::load(store)?;
    let modules = Modules::load(root, &syms);

    // Pass 1: supertypes, so member lookups can walk inheritance in pass 2.
    let mut pending = Vec::new();
    let mut after = 0;
    loop {
        let page = store.pending_refs_after(after, PAGE)?;
        let Some(last) = page.last() else { break };
        after = last.row_id.unwrap_or(after);
        pending.extend(page);
    }
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
            for r in &supers {
                match resolve_one(&mut matcher, &syms, r) {
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
    for (i, r) in rest.iter().enumerate() {
        match resolve_one(&mut matcher, &syms, r) {
            Some(m) => resolved.push((r.row_id.unwrap_or(0), to_edge(r, &syms, &m))),
            None => failed.push(r.row_id.unwrap_or(0)),
        }
        if (i + 1) % 2000 == 0 {
            progress(supers.len() + i + 1, total);
        }
    }
    outcome.attempted += rest.len();
    outcome.resolved = resolved.len();
    outcome.failed = failed.len();
    let rows: Vec<(i64, Edge, String)> = resolved
        .into_iter()
        .map(|(id, (e, f))| (id, e, f))
        .collect();
    store.apply_resolutions(&rows, &failed)?;
    progress(total, total);
    Ok(outcome)
}

fn resolve_one(
    matcher: &mut Matcher<'_>,
    syms: &Symbols,
    r: &UnresolvedReference,
) -> Option<matcher::Match> {
    let from = syms.by_id(&r.from_node_id)?;
    let file = r
        .file_path
        .clone()
        .unwrap_or_else(|| syms.get(from).file.clone());
    let lang = r.language.unwrap_or(syms.get(from).language);
    let m = matcher.resolve(from, &file, lang, r.reference_kind, &r.reference_name)?;
    // Import refs come from the file node; everything else must not point at a file.
    if r.reference_kind != ReferenceKind::Imports
        && syms.get(m.target).kind == codegraph_core::NodeKind::File
    {
        return None;
    }
    Some(m)
}
