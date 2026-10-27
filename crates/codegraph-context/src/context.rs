//! Task context: the symbols, relationships and code an agent needs for a task,
//! as markdown or JSON.

use crate::relevance::{parse_parts, rank};
use crate::source::{numbered, read_source};
use codegraph_core::{Edge, Node, NodeKind};
use codegraph_graph::{Graph, FLOW_EDGES};
use codegraph_store::{Result, Store};
use serde::Serialize;
use std::collections::HashSet;
use std::fmt::Write;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextFormat {
    #[default]
    Markdown,
    Json,
}

#[derive(Debug, Clone)]
pub struct ContextOptions {
    pub max_nodes: usize,
    pub max_code_blocks: usize,
    pub max_code_block_lines: u32,
    pub include_code: bool,
    pub format: ContextFormat,
}

impl Default for ContextOptions {
    fn default() -> Self {
        ContextOptions {
            max_nodes: 30,
            max_code_blocks: 8,
            max_code_block_lines: 60,
            include_code: true,
            format: ContextFormat::Markdown,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeBlock {
    pub file_path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub symbol: String,
    pub content: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskContext {
    pub query: String,
    pub entry_points: Vec<Node>,
    pub related: Vec<Node>,
    pub edges: Vec<Edge>,
    pub code_blocks: Vec<CodeBlock>,
    pub related_files: Vec<String>,
}

pub fn build_context(
    store: &Store,
    root: &Path,
    query: &str,
    opts: &ContextOptions,
) -> Result<TaskContext> {
    let parts = parse_parts(store, query)?;
    let ranked = rank(store, query, &parts, opts.max_nodes)?;
    let entry_points: Vec<Node> = ranked.iter().take(5).map(|s| s.node.clone()).collect();
    let related: Vec<Node> = ranked.iter().skip(5).map(|s| s.node.clone()).collect();
    let ids: Vec<String> = ranked.iter().map(|s| s.node.id.clone()).collect();
    let graph = Graph::new(store);
    let edges = graph.edges_among(&ids)?;
    let mut code_blocks = Vec::new();
    if opts.include_code {
        for s in ranked
            .iter()
            .filter(|s| s.node.kind != NodeKind::File)
            .take(opts.max_code_blocks)
        {
            let n = &s.node;
            let hash = store.file(&n.file_path)?.map(|f| f.content_hash);
            if let Some(src) = read_source(root, &n.file_path, hash.as_deref()) {
                let end = n.end_line.min(n.start_line + opts.max_code_block_lines);
                code_blocks.push(CodeBlock {
                    file_path: n.file_path.clone(),
                    start_line: n.start_line,
                    end_line: end,
                    symbol: n.qualified_name.clone(),
                    content: numbered(&src.lines, n.start_line, end),
                });
            }
        }
    }
    let mut files: Vec<String> = ranked
        .iter()
        .map(|s| s.node.file_path.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    files.sort();
    Ok(TaskContext {
        query: query.to_string(),
        entry_points,
        related,
        edges,
        code_blocks,
        related_files: files,
    })
}

/// Render a task context.
pub fn format_context(store: &Store, ctx: &TaskContext, format: ContextFormat) -> Result<String> {
    if format == ContextFormat::Json {
        return Ok(serde_json::to_string_pretty(ctx).unwrap_or_default());
    }
    let mut out = String::new();
    let _ = writeln!(out, "# Context: {}\n", ctx.query);
    if ctx.entry_points.is_empty() {
        let _ = writeln!(out, "No relevant symbols found.");
        return Ok(out);
    }
    let _ = writeln!(out, "## Entry points");
    for n in &ctx.entry_points {
        let _ = writeln!(
            out,
            "- {} `{}` — {}:{}{}",
            n.kind,
            n.qualified_name,
            n.file_path,
            n.start_line,
            n.signature
                .as_ref()
                .map(|s| format!(" `{s}`"))
                .unwrap_or_default()
        );
    }
    if !ctx.related.is_empty() {
        let _ = writeln!(out, "\n## Related symbols");
        for n in &ctx.related {
            let _ = writeln!(
                out,
                "- {} `{}` — {}:{}",
                n.kind, n.qualified_name, n.file_path, n.start_line
            );
        }
    }
    if !ctx.edges.is_empty() {
        let ids: Vec<String> = ctx
            .edges
            .iter()
            .flat_map(|e| [e.source.clone(), e.target.clone()])
            .collect();
        let nodes = store.nodes_by_ids(&ids)?;
        let _ = writeln!(out, "\n## Relationships");
        for e in ctx
            .edges
            .iter()
            .filter(|e| {
                FLOW_EDGES.contains(&e.kind) || e.kind != codegraph_core::EdgeKind::Contains
            })
            .take(40)
        {
            if let (Some(a), Some(b)) = (nodes.get(&e.source), nodes.get(&e.target)) {
                let _ = writeln!(out, "- `{}` {} `{}`", a.name, e.kind, b.name);
            }
        }
    }
    if !ctx.code_blocks.is_empty() {
        let _ = writeln!(out, "\n## Code");
        for b in &ctx.code_blocks {
            let _ = writeln!(
                out,
                "\n### {} ({}:{}-{})\n```\n{}```",
                b.symbol, b.file_path, b.start_line, b.end_line, b.content
            );
        }
    }
    let _ = writeln!(
        out,
        "\n## Files\n{}",
        ctx.related_files
            .iter()
            .map(|f| format!("- {f}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    Ok(out)
}
