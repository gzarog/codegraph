//! Which symbols and files a query is about.

use codegraph_core::search::{
    extract_search_terms, is_distinctive_identifier, is_test_file, STOP_WORDS,
};
use codegraph_core::{Node, NodeKind, SearchOptions};
use codegraph_graph::{Graph, FLOW_EDGES};
use codegraph_store::{Result, Store};
use std::collections::{HashMap, HashSet};

/// A symbol judged relevant to a query.
#[derive(Debug, Clone)]
pub struct Scored {
    pub node: Node,
    pub score: f64,
    /// Named exactly in the query.
    pub named: bool,
}

/// The query broken into what it names.
#[derive(Debug, Default)]
pub struct QueryParts {
    /// Indexed files named by path or basename.
    pub files: Vec<String>,
    /// Identifier-like tokens (possibly qualified: `A.b`, `A::b`).
    pub identifiers: Vec<String>,
    pub mentions_tests: bool,
}

fn looks_like_path(tok: &str) -> bool {
    tok.contains('/')
        || tok.rsplit_once('.').is_some_and(|(stem, ext)| {
            !stem.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
                && codegraph_core::language::is_source_file(tok)
        })
}

/// Split a query into file names and identifiers.
pub fn parse_parts(store: &Store, query: &str) -> Result<QueryParts> {
    let mut parts = QueryParts {
        mentions_tests: query.to_lowercase().contains("test")
            || query.to_lowercase().contains("spec"),
        ..Default::default()
    };
    let tokens: Vec<&str> = query
        .split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ',' | ';' | '(' | ')' | '`' | '"' | '\'' | '?' | '[' | ']' | '{' | '}'
                )
        })
        .map(|t| t.trim_end_matches(['.', ':']))
        .filter(|t| !t.is_empty())
        .collect();
    let mut files: Vec<String> = Vec::new();
    let all_files = if tokens.iter().any(|t| looks_like_path(t)) {
        store.all_files()?
    } else {
        Vec::new()
    };
    for tok in &tokens {
        if looks_like_path(tok) {
            let t = tok.trim_start_matches("./");
            // `path:line` anchors.
            let t = t.split(':').next().unwrap_or(t);
            let hits: Vec<&String> = all_files
                .iter()
                .map(|f| &f.path)
                .filter(|p| *p == t || p.ends_with(&format!("/{t}")))
                .collect();
            if !hits.is_empty() {
                for h in hits.into_iter().take(3) {
                    if !files.contains(h) {
                        files.push(h.clone());
                    }
                }
                continue;
            }
        }
        let ident = tok.trim_start_matches(['@', '$', '#']);
        if ident
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
            && ident
                .chars()
                .all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | ':' | '$'))
            && !STOP_WORDS.contains(&ident.to_lowercase().as_str())
            && ident.len() >= 2
        {
            parts.identifiers.push(ident.to_string());
        }
    }
    parts.files = files;
    Ok(parts)
}

fn kind_weight(kind: NodeKind) -> f64 {
    match kind {
        NodeKind::File | NodeKind::Import | NodeKind::Parameter | NodeKind::Export => 0.0,
        NodeKind::Variable | NodeKind::Field | NodeKind::Property | NodeKind::EnumMember => 0.5,
        _ => 1.0,
    }
}

/// Exact definitions for an identifier, qualified or not.
pub fn exact_matches(store: &Store, ident: &str) -> Result<Vec<Node>> {
    let normalized = ident.replace("::", ".").replace('#', ".");
    let (owner, name) = match normalized.rsplit_once('.') {
        Some((o, n)) if !n.is_empty() => (Some(o.to_string()), n.to_string()),
        _ => (None, normalized.clone()),
    };
    let mut nodes: Vec<Node> = store
        .nodes_named(&name)?
        .into_iter()
        .filter(|n| kind_weight(n.kind) > 0.0)
        .collect();
    if let Some(owner) = owner {
        let owner_tail = owner.rsplit('.').next().unwrap_or(&owner).to_string();
        let qualified: Vec<Node> = nodes
            .iter()
            .filter(|n| {
                let q = n.qualified_name.replace("::", ".");
                q.ends_with(&format!("{owner}.{name}"))
                    || q.ends_with(&format!("{owner_tail}.{name}"))
            })
            .cloned()
            .collect();
        if !qualified.is_empty() {
            return Ok(qualified);
        }
        // `module.func` where the owner is a file/module name.
        let by_file: Vec<Node> = nodes
            .iter()
            .filter(|n| {
                let base = n.file_path.rsplit('/').next().unwrap_or(&n.file_path);
                base.split('.').next() == Some(owner_tail.as_str())
            })
            .cloned()
            .collect();
        if !by_file.is_empty() {
            return Ok(by_file);
        }
        if nodes.is_empty() {
            return Ok(Vec::new());
        }
    }
    // A lowercase word may name a symbol in another case (`usermanager` → `UserManager`).
    if nodes.is_empty() && !ident.chars().any(char::is_uppercase) && !ident.contains('_') {
        nodes = store
            .nodes_named_case_insensitive(&name)?
            .into_iter()
            .filter(|n| kind_weight(n.kind) > 0.0)
            .collect();
    }
    Ok(nodes)
}

fn owner_mentioned(node: &Node, mentioned: &HashSet<String>) -> bool {
    let Some((owner, _)) = node.qualified_name.rsplit_once("::") else {
        return false;
    };
    owner
        .split("::")
        .any(|seg| mentioned.contains(&seg.to_lowercase()))
}

/// Declared inside a function or method body.
fn is_local(store: &Store, node: &Node) -> Result<bool> {
    let Some((owner, _)) = node.qualified_name.rsplit_once("::") else {
        return Ok(false);
    };
    Ok(store
        .nodes_by_qualified_name(owner)?
        .iter()
        .any(|o| o.file_path == node.file_path && o.kind.is_callable()))
}

/// Rank symbols for a query: exact names first, then full-text hits, then
/// their immediate call neighbours.
pub fn rank(store: &Store, query: &str, parts: &QueryParts, limit: usize) -> Result<Vec<Scored>> {
    let mut scores: HashMap<String, Scored> = HashMap::new();
    let bump = |node: Node, score: f64, named: bool, scores: &mut HashMap<String, Scored>| {
        let w = kind_weight(node.kind);
        if w == 0.0 {
            return;
        }
        let penalty = if !parts.mentions_tests && is_test_file(&node.file_path) {
            0.3
        } else {
            1.0
        };
        let s = score * w * penalty;
        let e = scores.entry(node.id.clone()).or_insert(Scored {
            node,
            score: 0.0,
            named: false,
        });
        e.score += s;
        e.named |= named;
    };

    let mentioned: HashSet<String> = parts.identifiers.iter().map(|i| i.to_lowercase()).collect();
    for ident in &parts.identifiers {
        let hits = exact_matches(store, ident)?;
        let n = hits.len().max(1) as f64;
        let distinctive = is_distinctive_identifier(ident) || ident.contains(['.', ':']);
        for node in hits.into_iter().take(12) {
            // A unique, distinctive name is almost certainly what was meant.
            let mut base = if distinctive { 100.0 } else { 60.0 };
            // The query also names its owner (`ReferenceResolver resolveOne`).
            if owner_mentioned(&node, &mentioned) {
                base *= 2.0;
            }
            // A helper declared inside a function body is rarely the subject.
            if is_local(store, &node)? {
                base *= 0.3;
            }
            bump(node, base / n.sqrt(), true, &mut scores);
        }
    }
    for file in &parts.files {
        for node in store
            .nodes_in_file(file)?
            .into_iter()
            .filter(|n| n.kind != NodeKind::File)
            .take(30)
        {
            let top = node.qualified_name.matches("::").count() == 0;
            bump(node, if top { 30.0 } else { 10.0 }, false, &mut scores);
        }
    }

    let terms = extract_search_terms(query, true);
    let mut searches: Vec<String> = vec![query.to_string()];
    searches.extend(terms.into_iter().take(10));
    let opts = SearchOptions {
        limit: Some(15),
        ..Default::default()
    };
    for (i, q) in searches.iter().enumerate() {
        let weight = if i == 0 { 1.0 } else { 0.5 };
        for r in store.search_nodes(q, &opts)? {
            bump(
                r.node,
                (r.score.max(0.0) / 10.0).min(20.0) * weight,
                false,
                &mut scores,
            );
        }
    }

    // Pull in direct neighbours of the strongest seeds: the code around them.
    let mut ranked: Vec<Scored> = scores.values().cloned().collect();
    ranked.sort_by(|a, b| b.score.total_cmp(&a.score));
    let graph = Graph::new(store);
    let seeds: Vec<Scored> = ranked.iter().take(5).cloned().collect();
    let seed_ids: HashSet<String> = seeds.iter().map(|s| s.node.id.clone()).collect();
    for seed in &seeds {
        let neighbors = graph
            .callees(&seed.node.id, FLOW_EDGES)?
            .into_iter()
            .chain(graph.callers(&seed.node.id, FLOW_EDGES)?);
        for n in neighbors.take(12) {
            if seed_ids.contains(&n.node.id) || n.node.kind == NodeKind::File {
                continue;
            }
            bump(n.node, seed.score * 0.15, false, &mut scores);
        }
    }

    let mut ranked: Vec<Scored> = scores.into_values().collect();
    ranked.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.node.id.cmp(&b.node.id))
    });
    ranked.truncate(limit);
    Ok(ranked)
}
