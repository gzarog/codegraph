//! Row tables (buffers.rs) → typed [`ExtractionResult`].

use crate::buffers::{
    EmitOut, EDGE_KINDS, EDGE_ROW_SIZE, FUNCTION_REF_CODE, KERNEL_ABI_VERSION, META_SIZE, NODE_KINDS,
    NODE_ROW_SIZE, NONE, REF_FLAG_FILE_PATH, REF_ROW_SIZE,
};
use codegraph_core::{
    now_ms, Edge, EdgeKind, ExtractionError, ExtractionResult, Language, Node, NodeKind, Provenance,
    ReferenceKind, UnresolvedReference, Visibility,
};

fn u8_at(b: &[u8], off: usize) -> u8 {
    b[off]
}

fn u16_at(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn u32_at(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn opt_u32(b: &[u8], off: usize) -> Option<u32> {
    match u32_at(b, off) {
        NONE => None,
        v => Some(v),
    }
}

struct Arena<'a>(&'a [u8]);

impl Arena<'_> {
    fn str_at(&self, row: &[u8], off: usize) -> Option<String> {
        let start = u32_at(row, off);
        if start == NONE {
            return None;
        }
        let len = u32_at(row, off + 4) as usize;
        let start = start as usize;
        Some(String::from_utf8_lossy(&self.0[start..start + len]).into_owned())
    }

    fn list_at(&self, row: &[u8], off: usize) -> Vec<String> {
        self.str_at(row, off).map(|s| s.split('\0').map(str::to_string).collect()).unwrap_or_default()
    }
}

/// Tri-state flag pair → `Some(value)` when present.
fn flag(flags: u16, pair: u16) -> Option<bool> {
    if flags & (1 << (pair * 2)) == 0 {
        None
    } else {
        Some(flags & (1 << (pair * 2 + 1)) != 0)
    }
}

fn visibility(code: u8) -> Option<Visibility> {
    match code {
        1 => Some(Visibility::Public),
        2 => Some(Visibility::Private),
        3 => Some(Visibility::Protected),
        4 => Some(Visibility::Internal),
        _ => None,
    }
}

fn provenance(code: u8) -> Option<Provenance> {
    match code {
        1 => Some(Provenance::TreeSitter),
        2 => Some(Provenance::Scip),
        3 => Some(Provenance::Heuristic),
        _ => None,
    }
}

/// Apply the `extraJson` escape hatch: any Node attribute a walker set that
/// has no fixed column.
fn apply_extra(node: &mut Node, extra: &str) {
    let Ok(serde_json::Value::Object(map)) = serde_json::from_str::<serde_json::Value>(extra) else {
        return;
    };
    for (key, value) in map {
        match (key.as_str(), value) {
            ("isExported", serde_json::Value::Bool(b)) => node.is_exported = b,
            ("isAsync", serde_json::Value::Bool(b)) => node.is_async = b,
            ("isStatic", serde_json::Value::Bool(b)) => node.is_static = b,
            ("isAbstract", serde_json::Value::Bool(b)) => node.is_abstract = b,
            ("docstring", serde_json::Value::String(s)) => node.docstring = Some(s),
            ("signature", serde_json::Value::String(s)) => node.signature = Some(s),
            ("returnType", serde_json::Value::String(s)) => node.return_type = Some(s),
            ("qualifiedName", serde_json::Value::String(s)) => node.qualified_name = s,
            ("name", serde_json::Value::String(s)) => node.name = s,
            ("visibility", serde_json::Value::String(s)) => node.visibility = s.parse().ok(),
            ("endLine", serde_json::Value::Number(n)) => node.end_line = n.as_u64().unwrap_or(0) as u32,
            ("decorators", serde_json::Value::Array(a)) => {
                node.decorators = a.into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
            }
            ("typeParameters", serde_json::Value::Array(a)) => {
                node.type_parameters = a.into_iter().filter_map(|v| v.as_str().map(str::to_string)).collect()
            }
            _ => {}
        }
    }
}

/// Decode one file's row tables.
pub fn decode(out: &EmitOut, file_path: &str, language: Language) -> Result<ExtractionResult, String> {
    let meta = &out.meta;
    if meta.len() < META_SIZE {
        return Err(format!("extraction meta too short: {}", meta.len()));
    }
    if u8_at(meta, 0) != KERNEL_ABI_VERSION {
        return Err(format!("extraction table version {} != {}", u8_at(meta, 0), KERNEL_ABI_VERSION));
    }
    let node_count = u32_at(meta, 4) as usize;
    let edge_count = u32_at(meta, 8) as usize;
    let ref_count = u32_at(meta, 12) as usize;
    let arena = Arena(&out.arena);
    let now = now_ms();

    let mut nodes = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let row = &out.nodes[i * NODE_ROW_SIZE..(i + 1) * NODE_ROW_SIZE];
        let kind: NodeKind = NODE_KINDS[u8_at(row, 0) as usize].parse().map_err(|e| format!("{e}"))?;
        let flags = u16_at(row, 2);
        let name = arena.str_at(row, 20).unwrap_or_default();
        let mut node = Node {
            id: arena.str_at(row, 36).unwrap_or_default(),
            kind,
            qualified_name: arena.str_at(row, 28).unwrap_or_else(|| name.clone()),
            name,
            file_path: file_path.to_string(),
            language,
            start_line: u32_at(row, 4),
            end_line: u32_at(row, 8),
            start_column: u32_at(row, 12),
            end_column: u32_at(row, 16),
            docstring: arena.str_at(row, 44),
            signature: arena.str_at(row, 52),
            visibility: visibility(u8_at(row, 1)),
            is_exported: flag(flags, 0).unwrap_or(false),
            is_async: flag(flags, 1).unwrap_or(false),
            is_static: flag(flags, 2).unwrap_or(false),
            is_abstract: flag(flags, 3).unwrap_or(false),
            decorators: arena.list_at(row, 60),
            type_parameters: arena.list_at(row, 68),
            return_type: arena.str_at(row, 76),
            updated_at: now,
        };
        if let Some(extra) = arena.str_at(row, 84) {
            apply_extra(&mut node, &extra);
        }
        nodes.push(node);
    }

    let id_of = |idx: u32, fallback: Option<String>| -> String {
        if idx == NONE {
            fallback.unwrap_or_default()
        } else {
            nodes.get(idx as usize).map(|n| n.id.clone()).unwrap_or_default()
        }
    };

    let mut edges = Vec::with_capacity(edge_count);
    for i in 0..edge_count {
        let row = &out.edges[i * EDGE_ROW_SIZE..(i + 1) * EDGE_ROW_SIZE];
        let kind: EdgeKind = EDGE_KINDS[u8_at(row, 8) as usize].parse().map_err(|e| format!("{e}"))?;
        edges.push(Edge {
            source: id_of(u32_at(row, 0), arena.str_at(row, 28)),
            target: id_of(u32_at(row, 4), arena.str_at(row, 36)),
            kind,
            metadata: arena.str_at(row, 20).and_then(|s| serde_json::from_str(&s).ok()),
            line: opt_u32(row, 12),
            column: opt_u32(row, 16),
            provenance: provenance(u8_at(row, 9)),
        });
    }

    let mut refs = Vec::with_capacity(ref_count);
    for i in 0..ref_count {
        let row = &out.refs[i * REF_ROW_SIZE..(i + 1) * REF_ROW_SIZE];
        let code = u8_at(row, 4);
        let reference_kind = if code == FUNCTION_REF_CODE {
            ReferenceKind::FunctionRef
        } else {
            EDGE_KINDS[code as usize].parse().map_err(|e| format!("{e}"))?
        };
        refs.push(UnresolvedReference {
            from_node_id: id_of(u32_at(row, 0), arena.str_at(row, 32)),
            reference_name: arena.str_at(row, 16).unwrap_or_default(),
            reference_kind,
            line: u32_at(row, 8),
            column: u32_at(row, 12),
            file_path: (u8_at(row, 5) & REF_FLAG_FILE_PATH != 0).then(|| file_path.to_string()),
            language: None,
            candidates: arena.list_at(row, 24),
            row_id: None,
        });
    }

    let mut errors = Vec::new();
    let errors_off = u32_at(meta, 20);
    if errors_off != NONE {
        let len = u32_at(meta, 24) as usize;
        let start = errors_off as usize;
        let json = String::from_utf8_lossy(&out.arena[start..start + len]);
        if let Ok(list) = serde_json::from_str::<Vec<ExtractionError>>(&json) {
            errors = list;
        }
    }

    Ok(ExtractionResult { nodes, edges, unresolved_references: refs, errors, duration_ms: 0.0 })
}
