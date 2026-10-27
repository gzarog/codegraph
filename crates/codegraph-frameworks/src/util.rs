//! Shared helpers for route extraction.

use codegraph_core::{
    now_ms, Edge, EdgeKind, Language, Node, NodeKind, Provenance, ReferenceKind,
    UnresolvedReference,
};

/// Byte offset → 1-based line.
pub struct Lines {
    starts: Vec<usize>,
}

impl Lines {
    pub fn new(text: &str) -> Self {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Lines { starts }
    }

    pub fn line(&self, offset: usize) -> u32 {
        (self.starts.partition_point(|&s| s <= offset)) as u32
    }

    pub fn column(&self, offset: usize) -> u32 {
        let l = self.line(offset) as usize;
        (offset - self.starts[l - 1]) as u32
    }
}

/// Routes and references a framework found in one file.
#[derive(Debug, Default, Clone)]
pub struct Routes {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub refs: Vec<UnresolvedReference>,
}

impl Routes {
    /// Add a route node `name` (e.g. `GET /users`) at `line`, contained by the file.
    pub fn route(
        &mut self,
        file: &str,
        lang: Language,
        framework: &str,
        name: &str,
        line: u32,
    ) -> String {
        let id = format!("route:{file}:{line}:{name}");
        if self.nodes.iter().any(|n| n.id == id) {
            return id;
        }
        let mut node = Node::new(&id, NodeKind::Route, name, file, lang, line, line);
        node.qualified_name = format!("{file}::route:{name}");
        node.signature = Some(format!("{framework} route"));
        node.decorators = vec![framework.to_string()];
        node.updated_at = now_ms();
        self.nodes.push(node);
        let mut edge = Edge::new(format!("file:{file}"), &id, EdgeKind::Contains);
        edge.provenance = Some(Provenance::Heuristic);
        self.edges.push(edge);
        id
    }

    /// A reference from a route to the code that handles it.
    pub fn handler(&mut self, from: &str, name: &str, kind: ReferenceKind, line: u32) {
        let name = name.trim();
        if name.is_empty() {
            return;
        }
        if self
            .refs
            .iter()
            .any(|r| r.from_node_id == from && r.reference_name == name && r.reference_kind == kind)
        {
            return;
        }
        self.refs.push(UnresolvedReference {
            from_node_id: from.to_string(),
            reference_name: name.to_string(),
            reference_kind: kind,
            line,
            column: 0,
            file_path: None,
            language: None,
            candidates: Vec::new(),
            row_id: None,
        });
    }

    pub fn extend(&mut self, other: Routes) {
        for n in other.nodes {
            if !self.nodes.iter().any(|m| m.id == n.id) {
                self.nodes.push(n);
            }
        }
        self.edges.extend(other.edges);
        self.refs.extend(other.refs);
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

/// Join URL path segments, normalizing slashes (`/api/` + `users` → `/api/users`).
pub fn join_path(prefix: &str, path: &str) -> String {
    let p = prefix.trim_matches('/');
    let s = path.trim_matches('/');
    let joined = match (p.is_empty(), s.is_empty()) {
        (true, true) => String::new(),
        (true, false) => s.to_string(),
        (false, true) => p.to_string(),
        (false, false) => format!("{p}/{s}"),
    };
    format!("/{joined}")
}

/// Index of the delimiter closing the one at `open` (skipping strings).
pub fn matching(s: &[u8], open: usize) -> Option<usize> {
    let (oc, cc) = match s.get(open)? {
        b'(' => (b'(', b')'),
        b'[' => (b'[', b']'),
        b'{' => (b'{', b'}'),
        _ => return None,
    };
    let mut depth = 0;
    let mut i = open;
    while i < s.len() {
        let c = s[i];
        if c == b'"' || c == b'\'' || c == b'`' {
            i += 1;
            while i < s.len() && s[i] != c {
                if s[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
        } else if c == oc {
            depth += 1;
        } else if c == cc {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Split call arguments at top-level commas.
pub fn split_args(args: &str) -> Vec<&str> {
    let b = args.as_bytes();
    let mut out = Vec::new();
    let (mut depth, mut start, mut i) = (0i32, 0, 0);
    while i < b.len() {
        match b[i] {
            b'"' | b'\'' | b'`' => {
                let q = b[i];
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
            }
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b',' if depth == 0 => {
                out.push(args[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    let last = args[start..].trim();
    if !last.is_empty() {
        out.push(last);
    }
    out
}

/// A plain (possibly dotted) identifier expression, e.g. `users.list` or `ctrl::show`.
pub fn is_ident_path(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '$' | '.' | ':'))
}

/// The first `def`/`fn`/`function` declared after `offset`, within a few lines.
pub fn next_declared_name(text: &str, offset: usize, keywords: &[&str]) -> Option<(String, usize)> {
    let rest = &text[offset..];
    let window_end = rest
        .match_indices('\n')
        .nth(12)
        .map(|(i, _)| i)
        .unwrap_or(rest.len());
    let window = &rest[..window_end];
    let mut best: Option<(usize, String)> = None;
    for kw in keywords {
        let mut from = 0;
        while let Some(pos) = window[from..].find(kw) {
            let at = from + pos;
            from = at + kw.len();
            let before_ok = at == 0
                || !window.as_bytes()[at - 1].is_ascii_alphanumeric()
                    && window.as_bytes()[at - 1] != b'_';
            let after = &window[at + kw.len()..];
            if !before_ok || !after.starts_with([' ', '\t']) {
                continue;
            }
            let name: String = after
                .trim_start()
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() && best.as_ref().is_none_or(|(b, _)| at < *b) {
                best = Some((at, name));
            }
            break;
        }
    }
    best.map(|(at, name)| (name, offset + at))
}

/// HTTP methods, uppercase.
pub const HTTP_METHODS: &[&str] = &["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        let l = Lines::new("a\nbc\nd");
        assert_eq!((l.line(0), l.line(2), l.line(5)), (1, 2, 3));
        assert_eq!(l.column(3), 1);
        assert_eq!(join_path("/api/", "/users/"), "/api/users");
        assert_eq!(join_path("", ""), "/");
        assert_eq!(
            split_args("'/x', auth, (a, b) => { f(a, b) }"),
            vec!["'/x'", "auth", "(a, b) => { f(a, b) }"]
        );
        let s = b"f(a, \")\", g(b))";
        assert_eq!(matching(s, 1), Some(s.len() - 1));
        assert_eq!(
            next_declared_name("@x\nasync def handler(r):", 0, &["def"])
                .unwrap()
                .0,
            "handler"
        );
    }
}
