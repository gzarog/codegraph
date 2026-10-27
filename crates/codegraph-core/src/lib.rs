//! Shared types for CodeGraph: the node/edge vocabulary every crate agrees on,
//! the records extraction produces and the store persists, and language
//! detection.

use serde::{Deserialize, Serialize};
use std::fmt;

pub mod language;
pub mod search;
pub use language::{detect_language, is_source_file, Language};

macro_rules! string_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident => $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, ::serde::Serialize, ::serde::Deserialize)]
        pub enum $name {
            $(#[serde(rename = $s)] $variant),+
        }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            pub fn as_str(self) -> &'static str {
                match self { $($name::$variant => $s),+ }
            }
        }

        impl ::std::str::FromStr for $name {
            type Err = $crate::UnknownVariant;
            fn from_str(s: &str) -> ::std::result::Result<Self, Self::Err> {
                match s {
                    $($s => Ok($name::$variant),)+
                    _ => Err($crate::UnknownVariant(format!("{}: {s}", stringify!($name)))),
                }
            }
        }

        impl ::std::fmt::Display for $name {
            fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
pub(crate) use string_enum;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownVariant(pub String);

impl fmt::Display for UnknownVariant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "unknown {}", self.0)
    }
}

impl std::error::Error for UnknownVariant {}

string_enum!(
    /// What a node in the graph is.
    NodeKind {
        File => "file",
        Module => "module",
        Class => "class",
        Struct => "struct",
        Interface => "interface",
        Trait => "trait",
        Protocol => "protocol",
        Function => "function",
        Method => "method",
        Property => "property",
        Field => "field",
        Variable => "variable",
        Constant => "constant",
        Enum => "enum",
        EnumMember => "enum_member",
        TypeAlias => "type_alias",
        Namespace => "namespace",
        Parameter => "parameter",
        Import => "import",
        Export => "export",
        Route => "route",
        Component => "component",
        Union => "union",
    }
);

impl NodeKind {
    /// Kinds that can be called or contain executable code.
    pub fn is_callable(self) -> bool {
        matches!(
            self,
            NodeKind::Function | NodeKind::Method | NodeKind::Route | NodeKind::Component
        )
    }

    /// Kinds that declare a type.
    pub fn is_type(self) -> bool {
        matches!(
            self,
            NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Interface
                | NodeKind::Trait
                | NodeKind::Protocol
                | NodeKind::Enum
                | NodeKind::TypeAlias
                | NodeKind::Union
        )
    }
}

string_enum!(
    /// How two nodes relate.
    EdgeKind {
        Contains => "contains",
        Calls => "calls",
        Imports => "imports",
        Exports => "exports",
        Extends => "extends",
        Implements => "implements",
        References => "references",
        TypeOf => "type_of",
        Returns => "returns",
        Instantiates => "instantiates",
        Overrides => "overrides",
        Decorates => "decorates",
        Navigates => "navigates",
    }
);

string_enum!(
    /// The kind of an unresolved reference: any edge kind, plus the
    /// internal-only `function_ref` (a function passed as a value).
    ReferenceKind {
        Contains => "contains",
        Calls => "calls",
        Imports => "imports",
        Exports => "exports",
        Extends => "extends",
        Implements => "implements",
        References => "references",
        TypeOf => "type_of",
        Returns => "returns",
        Instantiates => "instantiates",
        Overrides => "overrides",
        Decorates => "decorates",
        Navigates => "navigates",
        FunctionRef => "function_ref",
    }
);

impl ReferenceKind {
    /// The edge a resolved reference becomes. A function passed as a value
    /// is a `references` edge.
    pub fn edge_kind(self) -> EdgeKind {
        match self {
            ReferenceKind::FunctionRef => EdgeKind::References,
            other => other
                .as_str()
                .parse()
                .expect("reference kinds mirror edge kinds"),
        }
    }
}

string_enum!(
    Visibility {
        Public => "public",
        Private => "private",
        Protected => "protected",
        Internal => "internal",
    }
);

string_enum!(
    /// Where an edge came from.
    Provenance {
        TreeSitter => "tree-sitter",
        Scip => "scip",
        Heuristic => "heuristic",
    }
);

/// A code symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    pub id: String,
    pub kind: NodeKind,
    pub name: String,
    pub qualified_name: String,
    pub file_path: String,
    pub language: Language,
    pub start_line: u32,
    pub end_line: u32,
    pub start_column: u32,
    pub end_column: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    #[serde(default)]
    pub is_exported: bool,
    #[serde(default)]
    pub is_async: bool,
    #[serde(default)]
    pub is_static: bool,
    #[serde(default)]
    pub is_abstract: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decorators: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub type_parameters: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    #[serde(default)]
    pub updated_at: i64,
}

impl Node {
    /// A minimal node; optional attributes default to absent.
    pub fn new(
        id: impl Into<String>,
        kind: NodeKind,
        name: impl Into<String>,
        file_path: impl Into<String>,
        language: Language,
        start_line: u32,
        end_line: u32,
    ) -> Self {
        let name = name.into();
        Node {
            id: id.into(),
            kind,
            qualified_name: name.clone(),
            name,
            file_path: file_path.into(),
            language,
            start_line,
            end_line,
            start_column: 0,
            end_column: 0,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: false,
            is_async: false,
            is_static: false,
            is_abstract: false,
            decorators: Vec::new(),
            type_parameters: Vec::new(),
            return_type: None,
            updated_at: 0,
        }
    }
}

/// A relationship between two nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    pub source: String,
    pub target: String,
    pub kind: EdgeKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

impl Edge {
    pub fn new(source: impl Into<String>, target: impl Into<String>, kind: EdgeKind) -> Self {
        Edge {
            source: source.into(),
            target: target.into(),
            kind,
            metadata: None,
            line: None,
            column: None,
            provenance: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionError {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    pub severity: Severity,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl ExtractionError {
    pub fn error(message: impl Into<String>) -> Self {
        ExtractionError {
            message: message.into(),
            file_path: None,
            line: None,
            column: None,
            severity: Severity::Error,
            code: None,
        }
    }
}

/// A reference found during extraction that names a symbol not yet known to
/// be in the graph; resolution turns it into an edge or marks it failed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedReference {
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: ReferenceKind,
    pub line: u32,
    pub column: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub row_id: Option<i64>,
}

/// Everything extraction produced for one file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionResult {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub unresolved_references: Vec<UnresolvedReference>,
    pub errors: Vec<ExtractionError>,
    pub duration_ms: f64,
}

/// A tracked source file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRecord {
    pub path: String,
    pub content_hash: String,
    pub language: Language,
    pub size: u64,
    pub modified_at: i64,
    pub indexed_at: i64,
    pub node_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<ExtractionError>,
    #[serde(default)]
    pub generated: bool,
}

/// Direction of a traversal relative to the start node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Outgoing,
    Incoming,
    Both,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraversalOptions {
    pub max_depth: Option<u32>,
    #[serde(default)]
    pub edge_kinds: Vec<EdgeKind>,
    #[serde(default)]
    pub node_kinds: Vec<NodeKind>,
    #[serde(default)]
    pub direction: Direction,
    pub limit: Option<usize>,
    #[serde(default)]
    pub include_start: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOptions {
    #[serde(default)]
    pub kinds: Vec<NodeKind>,
    #[serde(default)]
    pub languages: Vec<Language>,
    #[serde(default)]
    pub include_patterns: Vec<String>,
    #[serde(default)]
    pub exclude_patterns: Vec<String>,
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    #[serde(default)]
    pub case_sensitive: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    pub node: Node,
    pub score: f64,
}

/// A set of nodes and the edges among them.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subgraph {
    pub nodes: indexmap_lite::OrderedNodes,
    pub edges: Vec<Edge>,
    pub roots: Vec<String>,
}

/// Insertion-ordered node map without pulling in an extra dependency.
pub mod indexmap_lite {
    use super::Node;
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    #[derive(Debug, Clone, Default, Serialize, Deserialize)]
    pub struct OrderedNodes {
        nodes: Vec<Node>,
        #[serde(skip)]
        index: HashMap<String, usize>,
    }

    impl OrderedNodes {
        pub fn insert(&mut self, node: Node) -> bool {
            if self.index.contains_key(&node.id) {
                return false;
            }
            self.index.insert(node.id.clone(), self.nodes.len());
            self.nodes.push(node);
            true
        }

        pub fn get(&self, id: &str) -> Option<&Node> {
            self.index.get(id).map(|&i| &self.nodes[i])
        }

        pub fn contains(&self, id: &str) -> bool {
            self.index.contains_key(id)
        }

        pub fn len(&self) -> usize {
            self.nodes.len()
        }

        pub fn is_empty(&self) -> bool {
            self.nodes.is_empty()
        }

        pub fn iter(&self) -> std::slice::Iter<'_, Node> {
            self.nodes.iter()
        }

        pub fn into_vec(self) -> Vec<Node> {
            self.nodes
        }
    }
}

/// Counts over the whole graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphStats {
    pub node_count: u64,
    pub edge_count: u64,
    pub file_count: u64,
    pub nodes_by_kind: Vec<(String, u64)>,
    pub edges_by_kind: Vec<(String, u64)>,
    pub files_by_language: Vec<(String, u64)>,
    pub db_size_bytes: u64,
    pub wal_size_bytes: u64,
    pub last_updated: i64,
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kinds_round_trip() {
        for k in NodeKind::ALL {
            assert_eq!(k.as_str().parse::<NodeKind>().unwrap(), *k);
        }
        for k in EdgeKind::ALL {
            assert_eq!(k.as_str().parse::<EdgeKind>().unwrap(), *k);
        }
        assert_eq!(ReferenceKind::FunctionRef.edge_kind(), EdgeKind::References);
        assert_eq!(ReferenceKind::Calls.edge_kind(), EdgeKind::Calls);
    }

    #[test]
    fn node_serializes_camel_case() {
        let n = Node::new(
            "function:x",
            NodeKind::Function,
            "x",
            "a.ts",
            Language::TypeScript,
            1,
            2,
        );
        let v = serde_json::to_value(&n).unwrap();
        assert_eq!(v["qualifiedName"], "x");
        assert_eq!(v["kind"], "function");
        assert_eq!(v["language"], "typescript");
    }
}
