//! Text renderings shared by the CLI and the MCP tools.

use crate::project::{CodeGraph, Result, Status};
use codegraph_context::{numbered, read_source};
use codegraph_core::{FileRecord, Node, NodeKind};
use codegraph_graph::{Impact, Neighbor, DEPENDENCY_EDGES, FLOW_EDGES};
use globset::Glob;
use std::collections::BTreeMap;
use std::fmt::Write;

pub fn loc(n: &Node) -> String {
    format!("{}:{}", n.file_path, n.start_line)
}

/// One line describing a symbol.
pub fn node_line(n: &Node) -> String {
    let sig = n
        .signature
        .as_deref()
        .filter(|s| !s.is_empty() && s.len() < 160)
        .map(|s| format!("  {s}"))
        .unwrap_or_default();
    format!("{} {} — {}{}", n.kind, n.qualified_name, loc(n), sig)
}

pub fn search_results(results: &[codegraph_core::SearchResult]) -> String {
    if results.is_empty() {
        return "No results.".into();
    }
    results
        .iter()
        .map(|r| node_line(&r.node))
        .collect::<Vec<_>>()
        .join("\n")
}

fn definition_heading(n: &Node) -> String {
    format!("### {} `{}` ({})", n.kind, n.qualified_name, loc(n))
}

pub fn neighbor_list(
    title: &str,
    defs: &[(Node, Vec<Neighbor>)],
    limit: usize,
    file_note: Option<&str>,
) -> String {
    let mut out = String::new();
    if let Some(note) = file_note {
        let _ = writeln!(out, "{note}");
    }
    for (def, list) in defs {
        if defs.len() > 1 {
            let _ = writeln!(out, "{}", definition_heading(def));
        }
        if list.is_empty() {
            let _ = writeln!(out, "No {title} found for `{}`.", def.name);
            continue;
        }
        let _ = writeln!(out, "{} {title} of `{}`:", list.len().min(limit), def.name);
        for n in list.iter().take(limit) {
            let line = n
                .edge
                .line
                .map(|l| format!(" (line {l})"))
                .unwrap_or_default();
            let _ = writeln!(
                out,
                "- {} `{}` — {}{} [{}]",
                n.node.kind,
                n.node.qualified_name,
                loc(&n.node),
                line,
                n.edge.kind
            );
        }
        if list.len() > limit {
            let _ = writeln!(out, "… {} more", list.len() - limit);
        }
    }
    out
}

pub fn impact(defs: &[(Node, Impact)]) -> String {
    let mut out = String::new();
    for (def, imp) in defs {
        let _ = writeln!(
            out,
            "Impact of changing {} `{}` ({}):",
            def.kind,
            def.qualified_name,
            loc(def)
        );
        if imp.total() == 0 {
            let _ = writeln!(out, "Nothing in the index depends on it.");
            continue;
        }
        for (i, level) in imp.levels.iter().enumerate() {
            let _ = writeln!(out, "Depth {} ({}):", i + 1, level.len());
            for n in level.iter().take(40) {
                let _ = writeln!(
                    out,
                    "- {} `{}` — {} [{}]",
                    n.node.kind,
                    n.node.qualified_name,
                    loc(&n.node),
                    n.edge.kind
                );
            }
            if level.len() > 40 {
                let _ = writeln!(out, "… {} more", level.len() - 40);
            }
        }
        let _ = writeln!(
            out,
            "{} symbol(s) across {} file(s){}.",
            imp.total(),
            imp.files.len(),
            if imp.truncated { " (truncated)" } else { "" }
        );
    }
    out
}

/// A symbol's details: signature, docs, optional body, and its call trail.
pub fn node_details(cg: &CodeGraph, n: &Node, include_code: bool) -> Result<String> {
    let mut out = String::new();
    let _ = writeln!(out, "{}", definition_heading(n));
    if let Some(sig) = n.signature.as_deref().filter(|s| !s.is_empty()) {
        let _ = writeln!(out, "Signature: `{sig}`");
    }
    if let Some(doc) = n.docstring.as_deref().filter(|s| !s.is_empty()) {
        let _ = writeln!(
            out,
            "Doc: {}",
            doc.lines().take(6).collect::<Vec<_>>().join(" ")
        );
    }
    let g = cg.graph();
    let callers = g.callers(&n.id, DEPENDENCY_EDGES)?;
    let callees = g.callees(&n.id, FLOW_EDGES)?;
    if include_code {
        let hash = cg.store().file(&n.file_path)?.map(|f| f.content_hash);
        if let Some(src) = read_source(cg.root(), &n.file_path, hash.as_deref()) {
            if src.stale {
                let _ = writeln!(
                    out,
                    "⚠ {} changed on disk after the last index sync; line numbers may be shifted.",
                    n.file_path
                );
            }
            let _ = write!(
                out,
                "```\n{}```\n",
                numbered(&src.lines, n.start_line, n.end_line.max(n.start_line))
            );
        }
    }
    if n.kind.is_type() {
        let children = g.children(&n.id)?;
        if !children.is_empty() {
            let names: Vec<String> = children
                .iter()
                .take(30)
                .map(|c| format!("{} {}", c.kind, c.name))
                .collect();
            let _ = writeln!(out, "Members: {}", names.join(", "));
        }
        let h = g.type_hierarchy(&n.id, 20)?;
        if !h.ancestors.is_empty() {
            let _ = writeln!(
                out,
                "Extends/implements: {}",
                h.ancestors
                    .iter()
                    .map(|a| a.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !h.descendants.is_empty() {
            let _ = writeln!(
                out,
                "Subtypes ({}): {}",
                h.descendants.len(),
                h.descendants
                    .iter()
                    .take(10)
                    .map(|a| a.name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    let trail = |list: &[Neighbor]| {
        list.iter()
            .take(8)
            .map(|c| format!("{} ({})", c.node.name, loc(&c.node)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !callers.is_empty() {
        let _ = writeln!(
            out,
            "Used by ({}): {}{}",
            callers.len(),
            trail(&callers),
            if callers.len() > 8 { ", …" } else { "" }
        );
    }
    if !callees.is_empty() {
        let _ = writeln!(
            out,
            "Uses ({}): {}{}",
            callees.len(),
            trail(&callees),
            if callees.len() > 8 { ", …" } else { "" }
        );
    }
    Ok(out)
}

/// File mode of `node`: the file's current source like the Read tool, plus dependents.
pub fn file_view(
    cg: &CodeGraph,
    rec: &FileRecord,
    offset: Option<u32>,
    limit: Option<u32>,
    symbols_only: bool,
) -> Result<String> {
    let mut out = String::new();
    let dependents = cg.store().dependent_files(&rec.path)?;
    let dep_note = if dependents.is_empty() {
        "No indexed file depends on it.".to_string()
    } else {
        format!(
            "Depended on by {} file(s): {}{}",
            dependents.len(),
            dependents
                .iter()
                .take(8)
                .cloned()
                .collect::<Vec<_>>()
                .join(", "),
            if dependents.len() > 8 { ", …" } else { "" }
        )
    };
    if symbols_only {
        let _ = writeln!(
            out,
            "{} ({}, {} symbols)",
            rec.path, rec.language, rec.node_count
        );
        for n in cg.store().nodes_in_file(&rec.path)? {
            if matches!(
                n.kind,
                NodeKind::File | NodeKind::Import | NodeKind::Parameter
            ) {
                continue;
            }
            let depth = n.qualified_name.matches("::").count();
            let _ = writeln!(
                out,
                "{}{} {} (line {})",
                "  ".repeat(depth.min(4)),
                n.kind,
                n.name,
                n.start_line
            );
        }
        let _ = writeln!(out, "{dep_note}");
        return Ok(out);
    }
    let Some(src) = read_source(cg.root(), &rec.path, Some(&rec.content_hash)) else {
        return Ok(format!("Cannot read {} from disk.", rec.path));
    };
    let start = offset.unwrap_or(1).max(1);
    let count = limit.unwrap_or(2000).min(2000);
    let end = (start + count - 1).min(src.lines.len() as u32);
    if src.stale {
        let _ = writeln!(
            out,
            "⚠ changed on disk after the last index sync (showing current content)."
        );
    }
    out.push_str(&numbered(&src.lines, start, end));
    if (end as usize) < src.lines.len() {
        let _ = writeln!(
            out,
            "… {} more lines (use offset {} to continue)",
            src.lines.len() - end as usize,
            end + 1
        );
    }
    let _ = writeln!(out, "\n{dep_note}");
    Ok(out)
}

pub fn status(s: &Status) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "CodeGraph {} — {}", s.version, s.root);
    let _ = writeln!(
        out,
        "Files: {}  Nodes: {}  Edges: {}",
        s.stats.file_count, s.stats.node_count, s.stats.edge_count
    );
    let mb = |b: u64| format!("{:.1} MB", b as f64 / 1_048_576.0);
    let _ = writeln!(
        out,
        "Database: {} ({} + {} WAL)",
        s.db_path,
        mb(s.stats.db_size_bytes),
        mb(s.stats.wal_size_bytes)
    );
    if s.stats.last_updated > 0 {
        let age = (codegraph_core::now_ms() - s.stats.last_updated) / 1000;
        let _ = writeln!(out, "Last indexed: {} ago", human_duration(age));
    }
    if !s.stats.files_by_language.is_empty() {
        let langs: Vec<String> = s
            .stats
            .files_by_language
            .iter()
            .map(|(l, n)| format!("{l} {n}"))
            .collect();
        let _ = writeln!(out, "Languages: {}", langs.join(", "));
    }
    if !s.stats.nodes_by_kind.is_empty() {
        let kinds: Vec<String> = s
            .stats
            .nodes_by_kind
            .iter()
            .take(10)
            .map(|(k, n)| format!("{k} {n}"))
            .collect();
        let _ = writeln!(out, "Symbols: {}", kinds.join(", "));
    }
    let _ = writeln!(
        out,
        "References: {} unresolved (external or unknown), {} pending",
        s.failed_refs, s.pending_refs
    );
    if s.pending.is_empty() {
        let _ = writeln!(out, "Index is up to date.");
    } else {
        let _ = writeln!(
            out,
            "### Pending sync: {} added, {} modified, {} removed",
            s.pending.added.len(),
            s.pending.modified.len(),
            s.pending.removed.len()
        );
        for (label, list) in [
            ("+", &s.pending.added),
            ("~", &s.pending.modified),
            ("-", &s.pending.removed),
        ] {
            for f in list.iter().take(10) {
                let _ = writeln!(out, "  {label} {f}");
            }
        }
        let _ = writeln!(out, "Run `codegraph sync` to update.");
    }
    out
}

pub fn human_duration(secs: i64) -> String {
    match secs {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilesFormat {
    Tree,
    Flat,
    Grouped,
}

impl std::str::FromStr for FilesFormat {
    type Err = String;
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "tree" => Ok(FilesFormat::Tree),
            "flat" => Ok(FilesFormat::Flat),
            "grouped" => Ok(FilesFormat::Grouped),
            other => Err(format!("unknown format {other} (tree, flat, grouped)")),
        }
    }
}

/// The indexed file listing.
pub fn files(
    all: &[FileRecord],
    dir: Option<&str>,
    pattern: Option<&str>,
    format: FilesFormat,
    metadata: bool,
    max_depth: Option<usize>,
) -> String {
    let dir = dir
        .map(|d| d.trim_matches('/').to_string())
        .filter(|d| !d.is_empty() && d != ".");
    let matcher = pattern.and_then(|p| {
        let p = if p.contains('/') {
            p.to_string()
        } else {
            format!("**/{p}")
        };
        Glob::new(&p).ok().map(|g| g.compile_matcher())
    });
    let selected: Vec<&FileRecord> = all
        .iter()
        .filter(|f| {
            dir.as_ref()
                .is_none_or(|d| f.path.starts_with(&format!("{d}/")))
        })
        .filter(|f| matcher.as_ref().is_none_or(|m| m.is_match(&f.path)))
        .collect();
    if selected.is_empty() {
        return "No indexed files match.".into();
    }
    let meta = |f: &FileRecord| {
        if metadata {
            format!(
                " ({}, {} symbols)",
                f.language,
                f.node_count.saturating_sub(1)
            )
        } else {
            String::new()
        }
    };
    let mut out = String::new();
    match format {
        FilesFormat::Flat => {
            for f in &selected {
                let _ = writeln!(out, "{}{}", f.path, meta(f));
            }
        }
        FilesFormat::Grouped => {
            let mut groups: BTreeMap<String, Vec<&FileRecord>> = BTreeMap::new();
            for f in &selected {
                groups.entry(f.language.to_string()).or_default().push(f);
            }
            for (lang, list) in groups {
                let _ = writeln!(out, "{lang} ({}):", list.len());
                for f in list {
                    let _ = writeln!(
                        out,
                        "  {}{}",
                        f.path,
                        if metadata {
                            format!(" ({} symbols)", f.node_count.saturating_sub(1))
                        } else {
                            String::new()
                        }
                    );
                }
            }
        }
        FilesFormat::Tree => {
            #[derive(Default)]
            struct Dir<'a> {
                dirs: BTreeMap<String, Dir<'a>>,
                files: Vec<&'a FileRecord>,
            }
            let mut tree = Dir::default();
            for f in &selected {
                let rel = match &dir {
                    Some(d) => f.path[d.len() + 1..].to_string(),
                    None => f.path.clone(),
                };
                let mut cur = &mut tree;
                let parts: Vec<&str> = rel.split('/').collect();
                for p in &parts[..parts.len() - 1] {
                    cur = cur.dirs.entry(p.to_string()).or_default();
                }
                cur.files.push(f);
            }
            fn render(
                d: &Dir<'_>,
                depth: usize,
                max: Option<usize>,
                meta: &dyn Fn(&FileRecord) -> String,
                out: &mut String,
            ) {
                let pad = "  ".repeat(depth);
                for (name, sub) in &d.dirs {
                    let _ = writeln!(out, "{pad}{name}/");
                    if max.is_none_or(|m| depth + 1 < m) {
                        render(sub, depth + 1, max, meta, out);
                    }
                }
                for f in &d.files {
                    let name = f.path.rsplit('/').next().unwrap_or(&f.path);
                    let _ = writeln!(out, "{pad}{name}{}", meta(f));
                }
            }
            if let Some(d) = &dir {
                let _ = writeln!(out, "{d}/");
            }
            render(
                &tree,
                usize::from(dir.is_some()),
                max_depth,
                &meta,
                &mut out,
            );
        }
    }
    let _ = writeln!(out, "{} file(s)", selected.len());
    out
}
