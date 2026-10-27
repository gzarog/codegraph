//! `explore`: the verbatim source of the symbols a query is about, grouped by
//! file and capped to fit an agent's tool-result budget, plus the call path
//! among the named symbols and what depends on them.

use crate::memory::{self, ExploreMemory};
use crate::relevance::{parse_parts, rank, Scored};
use crate::source::{merge_ranges, numbered, read_source};
use codegraph_core::{Node, NodeKind};
use codegraph_graph::{Graph, DEPENDENCY_EDGES, FLOW_EDGES};
use codegraph_store::{Result, Store};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;
use std::path::Path;

/// Output caps scaled to project size: small projects get fewer, fuller files.
#[derive(Debug, Clone, Copy)]
pub struct ExploreBudget {
    pub max_output_chars: usize,
    pub default_max_files: usize,
    pub max_chars_per_file: usize,
    pub gap_threshold: u32,
    pub include_relationships: bool,
}

pub fn budget_for(file_count: u64) -> ExploreBudget {
    match file_count {
        0..150 => ExploreBudget {
            max_output_chars: 13_000,
            default_max_files: 4,
            max_chars_per_file: 3_800,
            gap_threshold: 7,
            include_relationships: false,
        },
        150..500 => ExploreBudget {
            max_output_chars: 18_000,
            default_max_files: 5,
            max_chars_per_file: 3_800,
            gap_threshold: 8,
            include_relationships: false,
        },
        500..5000 => ExploreBudget {
            max_output_chars: 24_000,
            default_max_files: 8,
            max_chars_per_file: 6_500,
            gap_threshold: 12,
            include_relationships: true,
        },
        _ => ExploreBudget {
            max_output_chars: 24_000,
            default_max_files: 8,
            max_chars_per_file: 7_000,
            gap_threshold: 15,
            include_relationships: true,
        },
    }
}

#[derive(Debug, Clone, Default)]
pub struct ExploreOptions {
    pub max_files: Option<usize>,
}

/// Small files are shown whole: cheaper than describing which part to read.
const WHOLE_FILE_LINES: u32 = 60;
/// A stale file is re-read from disk and shown whole up to this size.
const STALE_WHOLE_FILE_MAX_LINES: usize = 300;

fn loc(n: &Node) -> String {
    format!("{}:{}", n.file_path, n.start_line)
}

fn kind_label(n: &Node) -> &'static str {
    n.kind.as_str()
}

struct FilePlan {
    path: String,
    score: f64,
    symbols: Vec<Scored>,
    pinned: bool,
}

/// Run an explore query.
pub fn explore(store: &Store, root: &Path, query: &str, opts: &ExploreOptions) -> Result<String> {
    explore_in_session(store, root, query, opts, None)
}

/// Run an explore query for a session that remembers what it was shown:
/// source it already has (unchanged) is replaced by a pointer.
pub fn explore_in_session(
    store: &Store,
    root: &Path,
    query: &str,
    opts: &ExploreOptions,
    mut memory: Option<&mut ExploreMemory>,
) -> Result<String> {
    let file_count = store.file_count()?;
    let budget = budget_for(file_count);
    let max_files = opts
        .max_files
        .unwrap_or(budget.default_max_files)
        .clamp(1, 20);
    let parts = parse_parts(store, query)?;
    let ranked = rank(store, query, &parts, 80)?;

    let mut out = String::new();
    if ranked.is_empty() && parts.files.is_empty() {
        let _ = writeln!(out, "No indexed symbols match \"{query}\".");
        let suggestions = suggestions(store, &parts.identifiers)?;
        if !suggestions.is_empty() {
            let _ = writeln!(out, "Did you mean: {}?", suggestions.join(", "));
        }
        let _ = writeln!(
            out,
            "Explore matches names and code words, not meaning; retry with symbol or file names."
        );
        return Ok(out);
    }

    // Group by file; files the query named come first.
    let mut plans: HashMap<String, FilePlan> = HashMap::new();
    for f in &parts.files {
        plans.insert(
            f.clone(),
            FilePlan {
                path: f.clone(),
                score: 1e6,
                symbols: Vec::new(),
                pinned: true,
            },
        );
    }
    for s in &ranked {
        let p = plans
            .entry(s.node.file_path.clone())
            .or_insert_with(|| FilePlan {
                path: s.node.file_path.clone(),
                score: 0.0,
                symbols: Vec::new(),
                pinned: false,
            });
        p.score += s.score;
        p.symbols.push(s.clone());
    }
    let mut plans: Vec<FilePlan> = plans.into_values().collect();
    plans.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.path.cmp(&b.path))
    });
    let dropped: Vec<String> = plans
        .iter()
        .skip(max_files)
        .map(|p| p.path.clone())
        .collect();
    plans.truncate(max_files);

    let named: Vec<&Scored> = ranked.iter().filter(|s| s.named).collect();
    let graph = Graph::new(store);

    // Flow among the named symbols.
    let flow = flow_section(&graph, &named)?;
    if !flow.is_empty() {
        out.push_str(&flow);
        out.push('\n');
    }

    let mut used = out.len();
    let mut shown_files = Vec::new();
    for plan in &plans {
        if used >= budget.max_output_chars {
            break;
        }
        let record = store.file(&plan.path)?;
        let Some(src) = read_source(
            root,
            &plan.path,
            record.as_ref().map(|r| r.content_hash.as_str()),
        ) else {
            continue;
        };
        let lang = record
            .as_ref()
            .map(|r| r.language.as_str())
            .unwrap_or("unknown");
        let total_lines = src.lines.len() as u32;
        let mut section = String::new();
        let anchored = parts.anchors.iter().any(|(f, ..)| *f == plan.path);
        let names: Vec<String> = plan
            .symbols
            .iter()
            .filter(|s| !anchored || s.named)
            .take(6)
            .map(|s| s.node.name.clone())
            .collect();
        let _ = writeln!(
            section,
            "## {} ({lang}, {total_lines} lines){}",
            plan.path,
            if names.is_empty() {
                String::new()
            } else {
                format!(" — {}", names.join(", "))
            }
        );
        if src.stale {
            if src.lines.len() <= STALE_WHOLE_FILE_MAX_LINES {
                let _ = writeln!(section, "⚠ changed on disk after the last index sync — showing the current file in full.");
                section.push_str(&numbered(&src.lines, 1, total_lines));
                if let Some(m) = memory.as_deref_mut() {
                    m.record(
                        &plan.path,
                        &memory::fingerprint(&src.lines),
                        &[(1, total_lines)],
                    );
                }
            } else {
                let _ = writeln!(section, "⚠ changed on disk after the last index sync — source omitted; Read this file directly.");
            }
        } else {
            let anchors: Vec<(u32, u32)> = parts
                .anchors
                .iter()
                .filter(|(f, ..)| *f == plan.path)
                .map(|(_, a, b)| (*a, *b))
                .collect();
            let ranges = if total_lines <= WHOLE_FILE_LINES
                || plan.pinned && plan.symbols.is_empty() && anchors.is_empty()
            {
                vec![(1, total_lines.min(400))]
            } else {
                let mut r = anchor_ranges(
                    store,
                    &plan.path,
                    &anchors,
                    total_lines,
                    budget.max_chars_per_file,
                )?;
                // An anchored file shows the anchors plus what the query named.
                r.extend(
                    plan.symbols
                        .iter()
                        .filter(|s| s.node.kind != NodeKind::File)
                        .filter(|s| anchors.is_empty() || s.named)
                        .map(|s| symbol_range(&s.node, budget.max_chars_per_file)),
                );
                if r.is_empty() {
                    r.push((1, total_lines.min(80)));
                }
                merge_ranges(r, budget.gap_threshold)
            };
            let fp = memory.as_ref().map(|_| memory::fingerprint(&src.lines));
            let ranges = match (memory.as_deref(), &fp) {
                (Some(m), Some(fp)) => {
                    let (emit, covered) = m.split(&plan.path, fp, &ranges);
                    if !covered.is_empty() {
                        let within =
                            |line: u32| covered.iter().any(|&(a, b)| a <= line && line <= b);
                        let syms: Vec<&str> = plan
                            .symbols
                            .iter()
                            .filter(|s| s.node.kind != NodeKind::File && within(s.node.start_line))
                            .map(|s| s.node.name.as_str())
                            .take(5)
                            .collect();
                        let _ = writeln!(
                            section,
                            "↩ Lines {}{} were already shown earlier in this conversation and are unchanged; not repeated.",
                            memory::spans(&covered),
                            if syms.is_empty() { String::new() } else { format!(" ({})", syms.join(", ")) }
                        );
                    }
                    emit
                }
                _ => ranges,
            };
            let mut served: Vec<(u32, u32)> = Vec::new();
            let cap = budget.max_chars_per_file.max(if plan.pinned {
                budget.max_chars_per_file * 2
            } else {
                0
            });
            let mut body = String::new();
            let mut prev_end = 0;
            for (s, e) in ranges {
                if s > prev_end + 1 && prev_end > 0 {
                    body.push_str("…\n");
                }
                let chunk = numbered(&src.lines, s, e);
                if body.len() + chunk.len() > cap {
                    let room = cap.saturating_sub(body.len());
                    let mut taken = 0;
                    let mut last = s;
                    for line in chunk.lines() {
                        if taken + line.len() + 1 > room {
                            break;
                        }
                        body.push_str(line);
                        body.push('\n');
                        taken += line.len() + 1;
                        last = line
                            .split('\t')
                            .next()
                            .and_then(|n| n.parse().ok())
                            .unwrap_or(last);
                    }
                    let _ = writeln!(
                        body,
                        "… (truncated at line {last}; ask for a specific symbol to see more)"
                    );
                    if taken > 0 {
                        served.push((s, last));
                    }
                    break;
                }
                body.push_str(&chunk);
                served.push((s, e));
                prev_end = e;
            }
            section.push_str(&body);
            if let (Some(m), Some(fp)) = (memory.as_deref_mut(), &fp) {
                m.record(&plan.path, fp, &served);
            }
        }
        section.push('\n');
        used += section.len();
        out.push_str(&section);
        shown_files.push(plan.path.clone());
    }

    let dynamic = dispatch_section(store, &named, &ranked)?;
    if !dynamic.is_empty() && out.len() + dynamic.len() <= budget.max_output_chars + 2000 {
        out.push_str(&dynamic);
    }
    let blast = blast_section(&graph, &named, &ranked)?;
    if !blast.is_empty() && out.len() + blast.len() <= budget.max_output_chars + 2000 {
        out.push_str(&blast);
    }
    if budget.include_relationships || !dropped.is_empty() {
        let more: Vec<String> = dropped.into_iter().take(8).collect();
        if !more.is_empty() {
            let _ = writeln!(out, "\nAlso relevant (not shown): {}", more.join(", "));
        }
    }
    Ok(out)
}

/// A symbol's line range, trimmed so one huge symbol can't eat the budget.
fn symbol_range(n: &Node, max_chars: usize) -> (u32, u32) {
    let max_lines = (max_chars / 40).max(20) as u32;
    let doc_lines = n
        .docstring
        .as_ref()
        .map(|d| d.lines().count() as u32 + 1)
        .unwrap_or(0);
    let start = n.start_line.saturating_sub(doc_lines).max(1);
    let end = n.end_line.max(n.start_line);
    // A large type: its declaration header; selected members bring their own ranges.
    if n.kind.is_type() && end - n.start_line > TYPE_HEADER_THRESHOLD {
        return (start, n.start_line + TYPE_HEADER_LINES);
    }
    (start, end.min(start + max_lines))
}

/// Ranges for `path:line` anchors: the innermost symbol around each line
/// (a large one trimmed to a window), or a window around the lines.
fn anchor_ranges(
    store: &Store,
    path: &str,
    anchors: &[(u32, u32)],
    total_lines: u32,
    max_chars: usize,
) -> Result<Vec<(u32, u32)>> {
    if anchors.is_empty() {
        return Ok(Vec::new());
    }
    let nodes = store.nodes_in_file(path)?;
    let mut out = Vec::new();
    for &(a, b) in anchors {
        let around = nodes
            .iter()
            .filter(|n| n.kind != NodeKind::File && n.start_line <= a && b <= n.end_line)
            .min_by_key(|n| n.end_line - n.start_line);
        let window = (
            a.saturating_sub(ANCHOR_CONTEXT).max(1),
            (b + ANCHOR_CONTEXT).min(total_lines),
        );
        out.push(match around {
            Some(n) if n.end_line - n.start_line <= (max_chars / 40) as u32 => {
                symbol_range(n, max_chars)
            }
            Some(n) => (n.start_line, n.start_line),
            None => window,
        });
        out.push(window);
    }
    Ok(out)
}

/// Lines shown on each side of a `path:line` anchor.
const ANCHOR_CONTEXT: u32 = 12;

/// Types longer than this show only their first `TYPE_HEADER_LINES` lines.
const TYPE_HEADER_THRESHOLD: u32 = 40;
const TYPE_HEADER_LINES: u32 = 6;

fn flow_section(graph: &Graph<'_>, named: &[&Scored]) -> Result<String> {
    let callable: Vec<&Node> = named
        .iter()
        .map(|s| &s.node)
        .filter(|n| n.kind.is_callable() || n.kind.is_type())
        .take(6)
        .collect();
    if callable.len() < 2 {
        return Ok(String::new());
    }
    let mut out = String::new();
    let mut seen_pairs = HashSet::new();
    for (i, a) in callable.iter().enumerate() {
        for b in callable.iter().skip(i + 1) {
            if a.name == b.name || !seen_pairs.insert((a.id.clone(), b.id.clone())) {
                continue;
            }
            let path = match graph.find_path(&a.id, &b.id, FLOW_EDGES, 6, false)? {
                Some(p) => Some(p),
                None => graph.find_path(&b.id, &a.id, FLOW_EDGES, 6, false)?,
            };
            if let Some(path) = path {
                if out.is_empty() {
                    out.push_str("## Flow\n");
                }
                let chain: Vec<String> = path
                    .iter()
                    .map(|step| match &step.via {
                        Some(e) => format!("—{}→ {} ({})", e.kind, step.node.name, loc(&step.node)),
                        None => format!("{} ({})", step.node.name, loc(&step.node)),
                    })
                    .collect();
                let _ = writeln!(out, "{}", chain.join(" "));
            }
        }
        if out.lines().count() > 6 {
            break;
        }
    }
    Ok(out)
}

fn blast_section(graph: &Graph<'_>, named: &[&Scored], ranked: &[Scored]) -> Result<String> {
    let focus: Vec<&Node> = if named.is_empty() {
        ranked.iter().take(2).map(|s| &s.node).collect()
    } else {
        named.iter().take(4).map(|s| &s.node).collect()
    };
    let mut out = String::new();
    for n in focus {
        if n.kind == NodeKind::File {
            continue;
        }
        let callers = graph.callers(&n.id, DEPENDENCY_EDGES)?;
        if callers.is_empty() {
            continue;
        }
        if out.is_empty() {
            out.push_str("## Blast radius\n");
        }
        let files: HashSet<&str> = callers.iter().map(|c| c.node.file_path.as_str()).collect();
        let top: Vec<String> = callers
            .iter()
            .take(5)
            .map(|c| format!("{} ({})", c.node.name, loc(&c.node)))
            .collect();
        let _ = writeln!(
            out,
            "- {} `{}` — used by {} symbol{} in {} file{}: {}{}",
            kind_label(n),
            n.name,
            callers.len(),
            if callers.len() == 1 { "" } else { "s" },
            files.len(),
            if files.len() == 1 { "" } else { "s" },
            top.join(", "),
            if callers.len() > 5 { ", …" } else { "" }
        );
    }
    Ok(out)
}

/// Links the call graph only has by heuristic: event emits joined to their
/// listeners, function-pointer calls to the functions in that slot, HTTP
/// requests to the routes serving them, and the reverse for handlers.
fn dispatch_section(store: &Store, named: &[&Scored], ranked: &[Scored]) -> Result<String> {
    let focus: Vec<&Node> = if named.is_empty() {
        ranked.iter().take(3).map(|s| &s.node).collect()
    } else {
        named.iter().take(6).map(|s| &s.node).collect()
    };
    let kinds = [
        codegraph_core::EdgeKind::Calls,
        codegraph_core::EdgeKind::Navigates,
    ];
    let mut out = String::new();
    let describe = |via: &str, key: &str| -> String {
        // Event and slot keys carry their channel: `event:saved`.
        let key = match via {
            "event" | "fnptr" => key.split_once(':').map_or(key, |(_, k)| k),
            _ => key,
        };
        match via {
            "http" => format!("requests `{key}`"),
            "nav" => format!("navigates to `{key}`"),
            "fnptr" => format!("calls function-pointer slot `{key}`"),
            "event" => format!("emits `{key}`"),
            family => format!("dispatches `{key}` ({family})"),
        }
    };
    let dispatch_of = |e: &codegraph_core::Edge| -> Option<(String, String)> {
        let m = e.metadata.as_ref()?;
        if !matches!(m.get("synthesizedBy")?.as_str()?, "dispatch" | "synth") {
            return None;
        }
        let via = m.get("dispatch")?.as_str()?.to_string();
        let key = m.get("key")?.as_str()?.to_string();
        Some((via, key))
    };
    for n in focus {
        if n.kind == NodeKind::File {
            continue;
        }
        // Outgoing: grouped by what is dispatched.
        let mut groups: Vec<((String, String), Vec<String>)> = Vec::new();
        for e in store.outgoing_edges(&n.id, &kinds)? {
            let Some(k) = dispatch_of(&e) else { continue };
            let Some(t) = store.node(&e.target)? else {
                continue;
            };
            let label = format!("{} ({})", t.name, loc(&t));
            match groups.iter_mut().find(|(g, _)| *g == k) {
                Some((_, v)) => v.push(label),
                None => groups.push((k, vec![label])),
            }
        }
        for ((via, key), targets) in groups {
            let _ = writeln!(
                out,
                "- `{}` {} → {}",
                n.name,
                describe(&via, &key),
                targets.join(", ")
            );
        }
        // Incoming: who reaches this handler dynamically.
        let mut callers = Vec::new();
        for e in store.incoming_edges(&n.id, &kinds)? {
            let Some((via, key)) = dispatch_of(&e) else {
                continue;
            };
            let Some(s) = store.node(&e.source)? else {
                continue;
            };
            callers.push(format!("{} ({}) {}", s.name, loc(&s), describe(&via, &key)));
        }
        if !callers.is_empty() {
            let more = callers.len() > 5;
            callers.truncate(5);
            let _ = writeln!(
                out,
                "- `{}` is reached dynamically: {}{}",
                n.name,
                callers.join("; "),
                if more { "; …" } else { "" }
            );
        }
    }
    if out.is_empty() {
        return Ok(out);
    }
    Ok(format!("## Dynamic dispatch (heuristic links)\n{out}\n"))
}

/// Close spellings of names that matched nothing.
pub fn suggestions(store: &Store, idents: &[String]) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for ident in idents.iter().take(4) {
        let hits = store.search_nodes(
            ident,
            &codegraph_core::SearchOptions {
                limit: Some(3),
                ..Default::default()
            },
        )?;
        for h in hits {
            if !out.contains(&h.node.name) {
                out.push(h.node.name);
            }
        }
    }
    out.truncate(6);
    Ok(out)
}
