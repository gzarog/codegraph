//! Lightweight extractors for languages without a bundled tree-sitter
//! grammar: Objective-C, Pascal/Delphi, Solidity, Terraform/HCL, Nix, Erlang,
//! COBOL, VB.NET and CFML. Each finds declarations with line-anchored
//! patterns over comment-masked source, sizes them by the language's block
//! rules (braces, `end` keywords, terminating periods), and records imports,
//! inheritance and call sites as references from the innermost declaration.

use crate::ids::{file_node_id, node_id};
use codegraph_core::{
    now_ms, Edge, EdgeKind, ExtractionResult, Language, Node, NodeKind, ReferenceKind,
    UnresolvedReference,
};
use regex::{Captures, Regex};
use std::sync::LazyLock;

/// Whether `lang` has an extractor here.
pub fn supports(lang: Language) -> bool {
    use Language::*;
    matches!(
        lang,
        ObjC | Pascal | Solidity | Terraform | Nix | Erlang | Cobol | VbNet | Cfml | CfScript
    )
}

pub fn extract(path: &str, content: &str, lang: Language) -> ExtractionResult {
    let mut b = Builder::new(path, content, lang);
    match lang {
        Language::ObjC => objc(&mut b),
        Language::Pascal => pascal(&mut b),
        Language::Solidity => solidity(&mut b),
        Language::Terraform => terraform(&mut b),
        Language::Nix => nix(&mut b),
        Language::Erlang => erlang(&mut b),
        Language::Cobol => cobol(&mut b),
        Language::VbNet => vbnet(&mut b),
        Language::Cfml | Language::CfScript => cfml(&mut b),
        _ => {}
    }
    b.finish()
}

fn re(p: &str) -> Regex {
    Regex::new(p).unwrap()
}

// ------------------------------------------------------------------ masking

struct Syntax {
    line: &'static [&'static str],
    block: &'static [(&'static str, &'static str)],
    quotes: &'static [u8],
}

/// Blank comments (and, with `strings`, string bodies) to spaces, keeping
/// byte offsets and newlines so positions map back to the source.
fn mask(text: &str, syn: &Syntax, strings: bool) -> String {
    let b = text.as_bytes();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for c in &mut out[from..to] {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let mut i = 0;
    'scan: while i < b.len() {
        for l in syn.line {
            if b[i..].starts_with(l.as_bytes()) {
                let end = b[i..]
                    .iter()
                    .position(|&c| c == b'\n')
                    .map_or(b.len(), |p| i + p);
                blank(&mut out, i, end);
                i = end;
                continue 'scan;
            }
        }
        for (open, close) in syn.block {
            if b[i..].starts_with(open.as_bytes()) {
                let body = i + open.len();
                let end = find(b, close.as_bytes(), body).map_or(b.len(), |p| p + close.len());
                blank(&mut out, i, end);
                i = end;
                continue 'scan;
            }
        }
        if syn.quotes.contains(&b[i]) {
            let q = b[i];
            let mut j = i + 1;
            while j < b.len() && b[j] != q && b[j] != b'\n' {
                if b[j] == b'\\' && q != b'\'' || b[j] == b'\\' && j + 1 < b.len() && b[j + 1] == q
                {
                    j += 1;
                }
                j += 1;
            }
            if strings {
                blank(&mut out, i + 1, j.min(b.len()));
            }
            i = j + 1;
            continue;
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|_| text.to_string())
}

fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from >= hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| from + p)
}

/// With `from` at or before an opening `{`: the offset of its matching `}`.
/// A `;` met first means a declaration without a body: its offset.
fn brace_end(code: &str, from: usize) -> usize {
    let b = code.as_bytes();
    let mut i = from;
    while i < b.len() && b[i] != b'{' {
        if b[i] == b';' {
            return i;
        }
        i += 1;
    }
    let mut depth = 0i32;
    while i < b.len() {
        match b[i] {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return i;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len().saturating_sub(1)
}

// ------------------------------------------------------------------ builder

struct Def {
    id: String,
    kind: NodeKind,
    name: String,
    start: u32,
    end: u32,
    signature: String,
    /// Owner name for members declared outside their type (`TFoo.Bar`).
    owner: Option<String>,
    exported: bool,
}

struct Pending {
    line: u32,
    name: String,
    kind: ReferenceKind,
    /// Attribute to this declaration instead of the innermost one.
    from: Option<usize>,
}

struct Builder<'a> {
    path: &'a str,
    text: &'a str,
    lang: Language,
    line_starts: Vec<usize>,
    defs: Vec<Def>,
    imports: Vec<(String, String, u32)>,
    refs: Vec<Pending>,
}

impl<'a> Builder<'a> {
    fn new(path: &'a str, text: &'a str, lang: Language) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Builder {
            path,
            text,
            lang,
            line_starts,
            defs: Vec::new(),
            imports: Vec::new(),
            refs: Vec::new(),
        }
    }

    fn line(&self, offset: usize) -> u32 {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i as u32 + 1,
            Err(i) => i as u32,
        }
    }

    fn lines(&self) -> u32 {
        self.line_starts.len() as u32
    }

    fn signature_at(&self, offset: usize) -> String {
        let start = self.line_starts[(self.line(offset) - 1) as usize];
        let end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |p| start + p);
        let s = self.text[start..end].trim();
        s.chars().take(200).collect()
    }

    fn def(&mut self, kind: NodeKind, name: &str, start_off: usize, end_off: usize) -> usize {
        let start = self.line(start_off);
        let end = self.line(end_off).max(start);
        let signature = self.signature_at(start_off);
        self.def_lines(kind, name, start, end, signature)
    }

    fn def_lines(
        &mut self,
        kind: NodeKind,
        name: &str,
        start: u32,
        end: u32,
        signature: String,
    ) -> usize {
        let name = name.trim().trim_matches('\'').to_string();
        self.defs.push(Def {
            id: node_id(self.path, kind.as_str(), &name, start),
            kind,
            name,
            start,
            end: end.max(start),
            signature,
            owner: None,
            exported: true,
        });
        self.defs.len() - 1
    }

    fn import(&mut self, module: &str, offset: usize) {
        let line = self.line(offset);
        let statement = self.signature_at(offset);
        self.imports
            .push((module.trim().to_string(), statement, line));
    }

    fn reference(&mut self, name: &str, kind: ReferenceKind, offset: usize) {
        let line = self.line(offset);
        self.refs.push(Pending {
            line,
            name: name.trim().to_string(),
            kind,
            from: None,
        });
    }

    fn reference_from(&mut self, def: usize, name: &str, kind: ReferenceKind) {
        let line = self.defs[def].start;
        self.refs.push(Pending {
            line,
            name: name.trim().to_string(),
            kind,
            from: Some(def),
        });
    }

    /// Innermost declaration spanning `line`.
    fn enclosing(&self, line: u32, callable_only: bool) -> Option<usize> {
        self.defs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.start <= line && line <= d.end)
            .filter(|(_, d)| !callable_only || d.kind.is_callable())
            .min_by_key(|(_, d)| (d.end - d.start, std::cmp::Reverse(d.start)))
            .map(|(i, _)| i)
    }

    fn finish(mut self) -> ExtractionResult {
        let path = self.path;
        let file_id = file_node_id(path);
        let mut r = ExtractionResult::default();
        let mut f = Node::new(
            &file_id,
            NodeKind::File,
            path.rsplit('/').next().unwrap_or(path),
            path,
            self.lang,
            1,
            self.lines(),
        );
        f.qualified_name = path.to_string();
        f.updated_at = now_ms();
        r.nodes.push(f);

        // Parents: the innermost strictly larger declaration around each one.
        let parents: Vec<Option<usize>> = (0..self.defs.len())
            .map(|i| {
                let d = &self.defs[i];
                self.defs
                    .iter()
                    .enumerate()
                    .filter(|(j, p)| {
                        *j != i
                            && p.start <= d.start
                            && d.end <= p.end
                            && (p.end - p.start > d.end - d.start || p.start < d.start || *j < i)
                            && (p.kind.is_type()
                                || matches!(p.kind, NodeKind::Module | NodeKind::Namespace)
                                || p.kind.is_callable())
                    })
                    .min_by_key(|(_, p)| p.end - p.start)
                    .map(|(j, _)| j)
            })
            .collect();
        // Members of a type become methods; functions in functions stay local.
        let methods: Vec<usize> = parents
            .iter()
            .enumerate()
            .filter(|&(i, p)| {
                self.defs[i].kind == NodeKind::Function
                    && (p.is_some_and(|p| self.defs[p].kind.is_type())
                        || self.defs[i].owner.is_some())
            })
            .map(|(i, _)| i)
            .collect();
        for i in methods {
            self.defs[i].kind = NodeKind::Method;
        }
        let prefix = path.to_string();
        let qualified = |i: usize| -> String {
            let mut chain = vec![self.defs[i].name.clone()];
            if let Some(o) = &self.defs[i].owner {
                chain.push(o.clone());
            }
            let mut p = parents[i];
            while let Some(j) = p {
                if matches!(self.defs[j].kind, NodeKind::File) {
                    break;
                }
                chain.push(self.defs[j].name.clone());
                p = parents[j];
            }
            chain.reverse();
            format!("{prefix}::{}", chain.join("::"))
        };
        for (i, d) in self.defs.iter().enumerate() {
            let mut n = Node::new(&d.id, d.kind, &d.name, path, self.lang, d.start, d.end);
            n.qualified_name = qualified(i);
            n.signature = Some(d.signature.clone());
            n.is_exported = d.exported;
            n.updated_at = now_ms();
            r.nodes.push(n);
            let parent = match parents[i] {
                Some(p) => self.defs[p].id.clone(),
                None => file_id.clone(),
            };
            r.edges.push(Edge::new(parent, &d.id, EdgeKind::Contains));
        }
        for (module, statement, line) in &self.imports {
            let id = node_id(path, "import", module, *line);
            let mut n = Node::new(&id, NodeKind::Import, module, path, self.lang, *line, *line);
            n.signature = Some(statement.clone());
            n.updated_at = now_ms();
            r.nodes.push(n);
            r.edges.push(Edge::new(&file_id, &id, EdgeKind::Contains));
            r.unresolved_references.push(UnresolvedReference {
                from_node_id: file_id.clone(),
                reference_name: module.clone(),
                reference_kind: ReferenceKind::Imports,
                line: *line,
                column: 0,
                file_path: Some(path.to_string()),
                language: Some(self.lang),
                candidates: Vec::new(),
                row_id: None,
            });
        }
        let mut seen = std::collections::HashSet::new();
        for p in &self.refs {
            if p.name.is_empty() {
                continue;
            }
            let from = p.from.or_else(|| {
                // Calls belong to code; elsewhere the innermost declaration.
                if matches!(p.kind, ReferenceKind::Calls | ReferenceKind::Instantiates) {
                    self.enclosing(p.line, true)
                } else {
                    self.enclosing(p.line, false)
                }
            });
            // A declaration's own header is not a call of itself.
            if let Some(i) = from {
                let d = &self.defs[i];
                if p.from.is_none()
                    && d.start == p.line
                    && d.name.eq_ignore_ascii_case(tail(&p.name))
                {
                    continue;
                }
            }
            let from_id = from.map_or_else(|| file_id.clone(), |i| self.defs[i].id.clone());
            if !seen.insert((from_id.clone(), p.name.clone(), p.kind, p.line)) {
                continue;
            }
            r.unresolved_references.push(UnresolvedReference {
                from_node_id: from_id,
                reference_name: p.name.clone(),
                reference_kind: p.kind,
                line: p.line,
                column: 0,
                file_path: Some(path.to_string()),
                language: Some(self.lang),
                candidates: Vec::new(),
                row_id: None,
            });
        }
        r
    }
}

fn tail(name: &str) -> &str {
    name.rsplit(['.', ':']).next().unwrap_or(name)
}

/// Call sites `name(` / `a.b(` in masked code, minus keywords and builtins.
fn calls(b: &mut Builder<'_>, code: &str, keywords: &[&str], case_insensitive: bool) {
    static CALL: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?:^|[^\w$.@#])([A-Za-z_][\w]*(?:\.[A-Za-z_]\w*)*)\s*\("));
    let mut found = Vec::new();
    for c in CALL.captures_iter(code) {
        let m = c.get(1).unwrap();
        let name = m.as_str();
        let first = name.split('.').next().unwrap_or(name);
        let hit = |k: &&str| {
            if case_insensitive {
                k.eq_ignore_ascii_case(first)
            } else {
                *k == first
            }
        };
        if keywords.iter().any(hit) {
            continue;
        }
        found.push((name.to_string(), m.start()));
    }
    for (name, off) in found {
        b.reference(&name, ReferenceKind::Calls, off);
    }
}

fn each<'t>(r: &Regex, text: &'t str) -> Vec<Captures<'t>> {
    r.captures_iter(text).collect()
}

// --------------------------------------------------------------- Objective-C

const C_SYNTAX: Syntax = Syntax {
    line: &["//"],
    block: &[("/*", "*/")],
    quotes: b"\"'",
};

const C_KEYWORDS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "return",
    "sizeof",
    "do",
    "else",
    "case",
    "typeof",
    "__typeof__",
    "defined",
    "alignof",
    "_Alignof",
    "offsetof",
    "va_arg",
    "NSLog",
    "@selector",
    "@encode",
    "assert",
];

fn objc(b: &mut Builder<'_>) {
    static TYPE: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"(?m)^\s*@(interface|implementation|protocol)\s+([A-Za-z_]\w*)\s*(?::\s*([A-Za-z_]\w*))?\s*(\(\s*\w*\s*\))?\s*(?:<([^>]*)>)?",
        )
    });
    static END: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s*@end\b"));
    static METHOD: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?m)^\s*[-+]\s*\([^)]*\)\s*([A-Za-z_]\w*)"));
    static FUNC: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"(?m)^[A-Za-z_][\w \t\*]*?\b([A-Za-z_]\w*)\s*\([^;{}()]*(?:\([^()]*\)[^;{}()]*)*\)\s*\{",
        )
    });
    static IMPORT: LazyLock<Regex> = LazyLock::new(|| {
        re(r#"(?m)^\s*(?:#\s*(?:import|include)\s*["<]([^">]+)[">]|@import\s+([\w.]+)\s*;)"#)
    });
    static SEND: LazyLock<Regex> =
        LazyLock::new(|| re(r"\[\s*([A-Za-z_][\w.]*)\s+([A-Za-z_]\w*)\s*[:\]]"));
    let code = mask(b.text, &C_SYNTAX, false);
    let safe = mask(b.text, &C_SYNTAX, true);
    for c in each(&IMPORT, &code) {
        let m = c.get(1).or(c.get(2)).unwrap();
        b.import(m.as_str(), c.get(0).unwrap().start());
    }
    let ends: Vec<usize> = END.find_iter(&safe).map(|m| m.start()).collect();
    for c in each(&TYPE, &safe) {
        let start = c.get(0).unwrap().start();
        let rest = &safe[c.get(0).unwrap().end()..];
        if c[1].eq("protocol") && rest.trim_start().starts_with(';') {
            continue; // forward declaration
        }
        let end = ends
            .iter()
            .copied()
            .find(|&e| e > start)
            .unwrap_or(safe.len() - 1);
        let kind = if &c[1] == "protocol" {
            NodeKind::Protocol
        } else {
            NodeKind::Class
        };
        let d = b.def(kind, &c[2], start, end);
        if let Some(sup) = c.get(3) {
            b.reference_from(d, sup.as_str(), ReferenceKind::Extends);
        }
        if let Some(protos) = c.get(5) {
            for p in protos.as_str().split(',') {
                let kind = if kind == NodeKind::Protocol {
                    ReferenceKind::Extends
                } else {
                    ReferenceKind::Implements
                };
                b.reference_from(d, p, kind);
            }
        }
    }
    for c in each(&METHOD, &safe) {
        let start = c.get(0).unwrap().start();
        let end = brace_end(&safe, c.get(0).unwrap().end());
        b.def(NodeKind::Method, &c[1], start, end);
    }
    for c in each(&FUNC, &safe) {
        let name = &c[1];
        if C_KEYWORDS.contains(&name) {
            continue;
        }
        let start = c.get(0).unwrap().start();
        let end = brace_end(&safe, c.get(1).unwrap().end());
        b.def(NodeKind::Function, name, start, end);
    }
    for c in each(&SEND, &safe) {
        let (recv, sel) = (&c[1], &c[2]);
        let name = match recv {
            "self" | "super" => format!("{recv}.{sel}"),
            r if r.starts_with(|ch: char| ch.is_ascii_uppercase()) => format!("{r}.{sel}"),
            _ => sel.to_string(),
        };
        b.reference(&name, ReferenceKind::Calls, c.get(2).unwrap().start());
    }
    calls(b, &safe, C_KEYWORDS, false);
}

// ------------------------------------------------------------------- Pascal

const PASCAL_SYNTAX: Syntax = Syntax {
    line: &["//"],
    block: &[("{", "}"), ("(*", "*)")],
    quotes: b"'",
};

const PASCAL_KEYWORDS: &[&str] = &[
    "if",
    "then",
    "else",
    "while",
    "for",
    "do",
    "repeat",
    "until",
    "case",
    "of",
    "begin",
    "end",
    "with",
    "try",
    "except",
    "finally",
    "raise",
    "exit",
    "inherited",
    "not",
    "and",
    "or",
    "procedure",
    "function",
    "constructor",
    "destructor",
    "array",
    "set",
    "string",
    "record",
    "class",
    "object",
    "inc",
    "dec",
    "length",
    "high",
    "low",
    "ord",
    "chr",
    "assigned",
    "sizeof",
    "setlength",
    "writeln",
    "write",
    "readln",
    "read",
    "copy",
    "pos",
    "trim",
    "format",
    "inttostr",
    "strtoint",
    "freeandnil",
    "integer",
    "boolean",
    "cardinal",
    "byte",
    "word",
    "pointer",
    "uses",
    "type",
    "var",
    "const",
    "result",
    "self",
    "nil",
    "true",
    "false",
];

fn pascal(b: &mut Builder<'_>) {
    static UNIT: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?im)^\s*(?:unit|program|library|package)\s+([\w.]+)\s*;"));
    static USES: LazyLock<Regex> = LazyLock::new(|| re(r"(?is)\buses\s+([^;]+);"));
    static TYPE: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"(?im)^\s*([A-Za-z_]\w*)\s*(?:<[^>]*>)?\s*=\s*(?:packed\s+)?(class|record|interface|dispinterface|object)\b(?:\s*\(([^)]*)\))?",
        )
    });
    static ROUTINE: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"(?im)^\s*(?:class\s+)?(procedure|function|constructor|destructor|operator)\s+([A-Za-z_][\w.]*)",
        )
    });
    static IMPLEMENTATION: LazyLock<Regex> = LazyLock::new(|| re(r"(?im)^\s*implementation\b"));
    static BARE_CALL: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s*([A-Za-z_][\w.]*)\s*;"));
    let code = mask(b.text, &PASCAL_SYNTAX, true);
    let lower = code.to_ascii_lowercase();
    if let Some(c) = UNIT.captures(&code) {
        b.def(
            NodeKind::Module,
            &c[1],
            c.get(0).unwrap().start(),
            code.len().saturating_sub(1),
        );
    }
    let implementation = IMPLEMENTATION.find(&code).map(|m| m.start());
    for c in each(&USES, &code) {
        let off = c.get(0).unwrap().start();
        for unit in c[1].split(',') {
            let unit = unit.split_whitespace().next().unwrap_or("");
            if !unit.is_empty() {
                b.import(unit, off);
            }
        }
    }
    let mut type_ranges = Vec::new();
    for c in each(&TYPE, &code) {
        let kw_end = c.get(2).unwrap().end();
        let after = code[c.get(0).unwrap().end()..].trim_start();
        // Forward declarations (`TFoo = class;`) and class references (`class of`).
        if after.starts_with(';') || after.to_ascii_lowercase().starts_with("of ") {
            continue;
        }
        let end = pascal_type_end(&lower, kw_end);
        let kind = match c[2].to_ascii_lowercase().as_str() {
            "record" => NodeKind::Struct,
            "interface" | "dispinterface" => NodeKind::Interface,
            _ => NodeKind::Class,
        };
        let d = b.def(kind, &c[1], c.get(0).unwrap().start(), end);
        type_ranges.push((c.get(0).unwrap().start(), end));
        if let Some(sup) = c.get(3) {
            for (i, s) in sup.as_str().split(',').enumerate() {
                // A class's first base is its superclass, the rest interfaces.
                let kind = if i == 0 || kind == NodeKind::Interface {
                    ReferenceKind::Extends
                } else {
                    ReferenceKind::Implements
                };
                b.reference_from(d, s, kind);
            }
        }
    }
    for c in each(&ROUTINE, &code) {
        let start = c.get(0).unwrap().start();
        let full = &c[2];
        let (owner, name) = match full.rsplit_once('.') {
            Some((o, n)) => (Some(o.rsplit('.').next().unwrap_or(o).to_string()), n),
            None => (None, full),
        };
        let header_end = pascal_header_end(&code, c.get(2).unwrap().end());
        let tail = lower[header_end..].trim_start();
        let declaration = type_ranges.iter().any(|&(s, e)| s < start && start <= e)
            || implementation.is_some_and(|i| start < i)
            || tail.starts_with("forward")
            || tail.starts_with("external");
        let end = if declaration {
            header_end
        } else {
            pascal_body_end(&lower, header_end)
        };
        let kind = if owner.is_some() {
            NodeKind::Method
        } else {
            NodeKind::Function
        };
        let d = b.def(kind, name, start, end);
        b.defs[d].owner = owner;
    }
    calls(b, &code, PASCAL_KEYWORDS, true);
    let bare: Vec<(String, usize)> = BARE_CALL
        .captures_iter(&code)
        .map(|c| (c[1].to_string(), c.get(1).unwrap().start()))
        .filter(|(n, _)| {
            !PASCAL_KEYWORDS
                .iter()
                .any(|k| k.eq_ignore_ascii_case(n.split('.').next().unwrap_or(n)))
        })
        .collect();
    for (name, off) in bare {
        b.reference(&name, ReferenceKind::Calls, off);
    }
}

/// The `;` ending a routine header (after its parameter list).
fn pascal_header_end(code: &str, from: usize) -> usize {
    let b = code.as_bytes();
    let mut depth = 0;
    for (i, &c) in b.iter().enumerate().skip(from) {
        match c {
            b'(' => depth += 1,
            b')' => depth -= 1,
            b';' if depth <= 0 => return i,
            _ => {}
        }
    }
    b.len().saturating_sub(1)
}

static WORD: LazyLock<Regex> = LazyLock::new(|| re(r"[a-z_][a-z0-9_]*"));

/// The `end` closing a routine body, skipping nested routines' bodies.
fn pascal_body_end(lower: &str, from: usize) -> usize {
    let mut depth = 0i32;
    let mut nested = 0;
    let mut seen_body = false;
    let mut prev_sig = b';';
    for m in WORD.find_iter(&lower[from..]) {
        let at = from + m.start();
        match m.as_str() {
            "procedure" | "function" | "constructor" | "destructor"
                if depth == 0 && !matches!(prev_sig, b'=' | b':') =>
            {
                nested += 1
            }
            "begin" | "try" | "asm" => {
                depth += 1;
                if depth == 1 && nested == 0 {
                    seen_body = true;
                }
            }
            "case" | "record" if depth > 0 || prev_sig == b'=' => depth += 1,
            "end" => {
                depth -= 1;
                if depth <= 0 {
                    depth = 0;
                    if nested > 0 {
                        nested -= 1;
                    } else if seen_body {
                        return at + 2;
                    }
                }
            }
            _ => {}
        }
        prev_sig = lower[..at]
            .bytes()
            .rev()
            .find(|c| !c.is_ascii_whitespace())
            .unwrap_or(b';');
    }
    lower.len().saturating_sub(1)
}

/// The `end` closing a type declaration started by its keyword.
fn pascal_type_end(lower: &str, from: usize) -> usize {
    static NESTED: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"=\s*(?:packed\s+)?(?:class|record|interface|object)\b\s*(?:\([^)]*\))?\s*[^;\s]|\bend\b",
        )
    });
    let mut depth = 1;
    for m in NESTED.find_iter(&lower[from..]) {
        if m.as_str() == "end" {
            depth -= 1;
            if depth == 0 {
                return from + m.end();
            }
        } else {
            depth += 1;
        }
    }
    lower.len().saturating_sub(1)
}

// ----------------------------------------------------------------- Solidity

const SOLIDITY_KEYWORDS: &[&str] = &[
    "if",
    "for",
    "while",
    "return",
    "returns",
    "require",
    "assert",
    "function",
    "mapping",
    "address",
    "bool",
    "string",
    "bytes",
    "payable",
    "keccak256",
    "sha256",
    "ecrecover",
    "abi",
    "type",
    "emit",
    "modifier",
    "event",
    "constructor",
    "catch",
    "try",
    "unchecked",
    "assembly",
    "selfdestruct",
    "blockhash",
    "gasleft",
    "super",
];

fn solidity(b: &mut Builder<'_>) {
    static TYPE: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"\b(abstract\s+contract|contract|interface|library)\s+([A-Za-z_]\w*)(?:\s+is\s+([^{]+))?\s*\{",
        )
    });
    static MEMBER: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"\b(function|modifier|event|error)\s+([A-Za-z_]\w*)\s*\(|\b(constructor|fallback|receive)\s*\(|\b(struct|enum)\s+([A-Za-z_]\w*)\s*\{",
        )
    });
    static IMPORT: LazyLock<Regex> =
        LazyLock::new(|| re(r#"\bimport\s+(?:[^;'"]*?\bfrom\s+)?["']([^"']+)["']"#));
    static NUMERIC: LazyLock<Regex> =
        LazyLock::new(|| re(r"^(u?int\d*|bytes\d+|fixed\w*|ufixed\w*)$"));
    let code = mask(b.text, &C_SYNTAX, false);
    let safe = mask(b.text, &C_SYNTAX, true);
    for c in each(&IMPORT, &code) {
        b.import(&c[1], c.get(0).unwrap().start());
    }
    for c in each(&TYPE, &safe) {
        let start = c.get(0).unwrap().start();
        let end = brace_end(&safe, c.get(0).unwrap().end() - 1);
        let kind = if &c[1] == "interface" {
            NodeKind::Interface
        } else {
            NodeKind::Class
        };
        let d = b.def(kind, &c[2], start, end);
        if let Some(bases) = c.get(3) {
            for base in bases.as_str().split(',') {
                let base = base.split('(').next().unwrap_or("").trim();
                if !base.is_empty() {
                    b.reference_from(d, base, ReferenceKind::Extends);
                }
            }
        }
    }
    for c in each(&MEMBER, &safe) {
        let start = c.get(0).unwrap().start();
        let (kind, name) = if c.get(1).is_some() {
            // Events and errors are invoked like functions (`emit`, `revert`).
            (NodeKind::Function, c[2].to_string())
        } else if let Some(special) = c.get(3) {
            (NodeKind::Function, special.as_str().to_string())
        } else {
            let kind = if &c[4] == "struct" {
                NodeKind::Struct
            } else {
                NodeKind::Enum
            };
            (kind, c[5].to_string())
        };
        let end = if matches!(c.get(1).map(|m| m.as_str()), Some("event" | "error")) {
            safe[start..].find(';').map_or(start, |p| start + p)
        } else {
            brace_end(&safe, c.get(0).unwrap().end() - 1)
        };
        b.def(kind, &name, start, end);
    }
    let mut kw: Vec<&str> = SOLIDITY_KEYWORDS.to_vec();
    kw.extend(["new"]);
    let before = b.refs.len();
    calls(b, &safe, &kw, false);
    // Type casts (`uint256(x)`) are not calls.
    let mut i = before;
    while i < b.refs.len() {
        if NUMERIC.is_match(&b.refs[i].name) {
            b.refs.remove(i);
        } else {
            i += 1;
        }
    }
}

// ---------------------------------------------------------------- Terraform

const HCL_SYNTAX: Syntax = Syntax {
    line: &["#", "//"],
    block: &[("/*", "*/")],
    quotes: b"\"",
};

fn terraform(b: &mut Builder<'_>) {
    static BLOCK: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r#"(?m)^\s*(resource|data|module|variable|output|provider|locals)\b\s*(?:"([\w.-]+)"\s*)?(?:"([\w.-]+)"\s*)?\{"#,
        )
    });
    static ATTR: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^\s*([A-Za-z_][\w-]*)\s*="));
    static SOURCE: LazyLock<Regex> =
        LazyLock::new(|| re(r#"(?m)^\s*source\s*=\s*"(\.{1,2}/[^"]+)""#));
    static REF: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"\b(?:(var|local|module)\.([A-Za-z_][\w-]*)|data\.([A-Za-z_][\w-]*)\.([A-Za-z_][\w-]*)|([a-z][a-z0-9]*_[a-z0-9_]+)\.([A-Za-z_][\w-]*))",
        )
    });
    let code = mask(b.text, &HCL_SYNTAX, false);
    let mut blocks = Vec::new();
    for c in each(&BLOCK, &code) {
        let start = c.get(0).unwrap().start();
        let end = brace_end(&code, c.get(0).unwrap().end() - 1);
        blocks.push((start, end));
        let (kind, name, owner): (NodeKind, Option<&str>, Option<String>) = match &c[1] {
            "resource" => (
                NodeKind::Struct,
                c.get(3).map(|m| m.as_str()),
                c.get(2).map(|m| m.as_str().to_string()),
            ),
            "data" => (
                NodeKind::Struct,
                c.get(3).map(|m| m.as_str()),
                c.get(2).map(|m| format!("data::{}", m.as_str())),
            ),
            "module" => (
                NodeKind::Module,
                c.get(2).map(|m| m.as_str()),
                Some("module".into()),
            ),
            "variable" => (
                NodeKind::Variable,
                c.get(2).map(|m| m.as_str()),
                Some("var".into()),
            ),
            "output" => (
                NodeKind::Constant,
                c.get(2).map(|m| m.as_str()),
                Some("output".into()),
            ),
            "locals" => {
                // Each attribute is a `local.<name>`.
                let body_start = c.get(0).unwrap().end();
                let body = &code[body_start..end.max(body_start)];
                let mut depth = 0i32;
                let mut last = 0;
                let mut attrs = Vec::new();
                for m in ATTR.captures_iter(body) {
                    let at = m.get(0).unwrap().start();
                    depth += body[last..at].matches(['{', '[', '(']).count() as i32
                        - body[last..at].matches(['}', ']', ')']).count() as i32;
                    last = at;
                    if depth == 0 {
                        attrs.push((m[1].to_string(), body_start + m.get(1).unwrap().start()));
                    }
                }
                for (name, off) in attrs {
                    let d = b.def(NodeKind::Constant, &name, off, off);
                    b.defs[d].owner = Some("local".into());
                }
                continue;
            }
            _ => continue,
        };
        let Some(name) = name else { continue };
        let d = b.def(kind, name, start, end);
        b.defs[d].owner = owner;
        if &c[1] == "module" {
            if let Some(s) = SOURCE.captures(&code[start..end.max(start)]) {
                let dir = s[1].trim_end_matches('/');
                b.import(&format!("{dir}/main.tf"), start + s.get(0).unwrap().start());
            }
        }
    }
    // References read the raw text: interpolations live inside strings.
    let comments_only = mask(b.text, &HCL_SYNTAX, false);
    let found: Vec<(String, usize)> = REF
        .captures_iter(&comments_only)
        .filter_map(|c| {
            let off = c.get(0).unwrap().start();
            let name = if let (Some(k), Some(n)) = (c.get(1), c.get(2)) {
                format!("{}.{}", k.as_str(), n.as_str())
            } else if let (Some(t), Some(n)) = (c.get(3), c.get(4)) {
                format!("data.{}.{}", t.as_str(), n.as_str())
            } else {
                let (t, n) = (c.get(5)?.as_str(), c.get(6)?.as_str());
                format!("{t}.{n}")
            };
            Some((name, off))
        })
        .collect();
    for (name, off) in found {
        b.reference(&name, ReferenceKind::References, off);
    }
}

// ---------------------------------------------------------------------- Nix

const NIX_SYNTAX: Syntax = Syntax {
    line: &["#"],
    block: &[("/*", "*/"), ("''", "''")],
    quotes: b"\"",
};

fn nix(b: &mut Builder<'_>) {
    static BIND: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?m)^\s*([A-Za-z_][\w'-]*(?:\.[A-Za-z_][\w'-]*)*)\s*=[^=]"));
    static LAMBDA: LazyLock<Regex> = LazyLock::new(|| {
        re(r"^\s*(?:[A-Za-z_][\w'-]*\s*:|\{[^}]*\}\s*(?:@\s*\w+\s*)?:|[A-Za-z_]\w*\s*@\s*\{)")
    });
    static IMPORT: LazyLock<Regex> =
        LazyLock::new(|| re(r"\b(?:import|callPackage)\s+(\.{1,2}/[\w./+-]*)"));
    let code = mask(b.text, &NIX_SYNTAX, true);
    for c in each(&IMPORT, &code) {
        let p = c[1].trim_end_matches('/');
        let module = if p.ends_with(".nix") {
            p.to_string()
        } else {
            format!("{p}/default.nix")
        };
        b.import(&module, c.get(0).unwrap().start());
    }
    let bytes = code.as_bytes();
    let mut depth = 0i32;
    let mut last = 0;
    for c in each(&BIND, &code) {
        let at = c.get(0).unwrap().start();
        for &ch in &bytes[last..at] {
            match ch {
                b'{' | b'[' | b'(' => depth += 1,
                b'}' | b']' | b')' => depth -= 1,
                _ => {}
            }
        }
        last = at;
        if depth > 2 {
            continue;
        }
        let value_at = c.get(0).unwrap().end() - 1;
        let lambda = LAMBDA.is_match(&code[value_at..]);
        // The binding runs to its `;` at the same nesting.
        let mut d = 0i32;
        let mut end = code.len().saturating_sub(1);
        for (i, &ch) in bytes.iter().enumerate().skip(value_at) {
            match ch {
                b'{' | b'[' | b'(' => d += 1,
                b'}' | b']' | b')' => {
                    d -= 1;
                    if d < 0 {
                        end = i;
                        break;
                    }
                }
                b';' if d == 0 => {
                    end = i;
                    break;
                }
                _ => {}
            }
        }
        let full = &c[1];
        let name = full.rsplit('.').next().unwrap_or(full);
        let kind = if lambda {
            NodeKind::Function
        } else {
            NodeKind::Variable
        };
        b.def(kind, name, c.get(1).unwrap().start(), end);
    }
}

// ------------------------------------------------------------------- Erlang

const ERLANG_SYNTAX: Syntax = Syntax {
    line: &["%"],
    block: &[],
    quotes: b"\"",
};

const ERLANG_KEYWORDS: &[&str] = &[
    "if",
    "case",
    "receive",
    "fun",
    "when",
    "try",
    "catch",
    "begin",
    "end",
    "of",
    "after",
    "not",
    "and",
    "or",
    "andalso",
    "orelse",
    "throw",
    "error",
    "exit",
    "element",
    "length",
    "hd",
    "tl",
    "is_atom",
    "is_list",
    "is_integer",
    "is_binary",
    "is_tuple",
    "is_map",
    "is_pid",
    "is_function",
    "is_number",
    "is_float",
    "is_boolean",
    "self",
    "spawn",
    "spawn_link",
    "atom_to_list",
    "list_to_atom",
    "integer_to_list",
    "list_to_integer",
    "binary_to_list",
    "list_to_binary",
    "tuple_to_list",
    "list_to_tuple",
    "size",
    "byte_size",
    "map_size",
];

fn erlang(b: &mut Builder<'_>) {
    static MODULE: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^-module\(\s*'?(\w+)'?\s*\)"));
    static CLAUSE: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^([a-z]\w*|'[^'\n]+')\s*\("));
    static RECORD: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^-record\(\s*(\w+)"));
    static INCLUDE: LazyLock<Regex> =
        LazyLock::new(|| re(r#"(?m)^-include(?:_lib)?\(\s*"([^"]+)""#));
    static BEHAVIOUR: LazyLock<Regex> = LazyLock::new(|| re(r"(?m)^-behaviou?r\(\s*(\w+)\s*\)"));
    static REMOTE: LazyLock<Regex> = LazyLock::new(|| re(r"\b([a-z]\w*)\s*:\s*([a-z]\w*)\s*\("));
    static LOCAL: LazyLock<Regex> = LazyLock::new(|| re(r"(?:^|[^\w:?#'.-])([a-z]\w*)\s*\("));
    let code = mask(b.text, &ERLANG_SYNTAX, true);
    let raw = mask(b.text, &ERLANG_SYNTAX, false);
    if let Some(c) = MODULE.captures(&code) {
        b.def(
            NodeKind::Module,
            &c[1],
            c.get(0).unwrap().start(),
            code.len().saturating_sub(1),
        );
    }
    for c in each(&INCLUDE, &raw) {
        b.import(&c[1], c.get(0).unwrap().start());
    }
    for c in each(&RECORD, &code) {
        let start = c.get(0).unwrap().start();
        b.def(
            NodeKind::Struct,
            &c[1],
            start,
            erlang_form_end(&code, start),
        );
    }
    // Consecutive clauses of one function are one symbol.
    let mut prev: Option<(String, usize)> = None;
    for c in each(&CLAUSE, &code) {
        let start = c.get(0).unwrap().start();
        let end = erlang_form_end(&code, start);
        let name = c[1].to_string();
        if let Some((pname, idx)) = &prev {
            if *pname == name {
                let e = b.line(end);
                b.defs[*idx].end = b.defs[*idx].end.max(e);
                continue;
            }
        }
        let d = b.def(NodeKind::Function, &name, start, end);
        prev = Some((name, d));
    }
    let module_def = b.defs.iter().position(|d| d.kind == NodeKind::Module);
    if let Some(m) = module_def {
        for c in each(&BEHAVIOUR, &code) {
            b.reference_from(m, &c[1], ReferenceKind::Implements);
        }
    }
    let remote: Vec<(String, usize)> = REMOTE
        .captures_iter(&code)
        .map(|c| (format!("{}.{}", &c[1], &c[2]), c.get(1).unwrap().start()))
        .collect();
    for (name, off) in remote {
        b.reference(&name, ReferenceKind::Calls, off);
    }
    let local: Vec<(String, usize)> = LOCAL
        .captures_iter(&code)
        .map(|c| (c[1].to_string(), c.get(1).unwrap().start()))
        .filter(|(n, _)| !ERLANG_KEYWORDS.contains(&n.as_str()))
        .collect();
    for (name, off) in local {
        // A clause head at column 0 is a declaration, not a call.
        if off == 0 || code.as_bytes()[off - 1] == b'\n' {
            continue;
        }
        b.reference(&name, ReferenceKind::Calls, off);
    }
}

/// The `.` ending an Erlang form: outside brackets, followed by whitespace.
fn erlang_form_end(code: &str, from: usize) -> usize {
    let b = code.as_bytes();
    let mut depth = 0i32;
    for i in from..b.len() {
        match b[i] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'.' if depth <= 0
                && b.get(i + 1).is_none_or(|c| c.is_ascii_whitespace())
                && !(i > 0
                    && b[i - 1].is_ascii_digit()
                    && b.get(i + 1).is_some_and(u8::is_ascii_digit)) =>
            {
                return i
            }
            _ => {}
        }
    }
    b.len().saturating_sub(1)
}

// -------------------------------------------------------------------- COBOL

fn cobol_mask(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.split_inclusive('\n') {
        let body = line.trim_end_matches(['\n', '\r']);
        let nl = &line[body.len()..];
        let fixed = body.len() > 6
            && body.as_bytes()[..6]
                .iter()
                .all(|c| c.is_ascii_digit() || *c == b' ');
        let mut chars: Vec<u8> = body.bytes().collect();
        if fixed {
            for c in chars.iter_mut().take(6) {
                *c = b' ';
            }
            if matches!(chars.get(6), Some(b'*' | b'/')) {
                chars.iter_mut().for_each(|c| *c = b' ');
            }
        }
        if let Some(p) = find(&chars, b"*>", 0) {
            for c in &mut chars[p..] {
                *c = b' ';
            }
        }
        out.push_str(&String::from_utf8(chars).unwrap_or_else(|_| body.to_string()));
        out.push_str(nl);
    }
    out
}

fn cobol(b: &mut Builder<'_>) {
    static PROGRAM: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?im)\bPROGRAM-ID\s*\.\s*'?([\w-]+)'?"));
    static END_PROGRAM: LazyLock<Regex> = LazyLock::new(|| re(r"(?im)\bEND\s+PROGRAM\b"));
    static PROCEDURE: LazyLock<Regex> = LazyLock::new(|| re(r"(?im)\bPROCEDURE\s+DIVISION\b"));
    static PARAGRAPH: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?im)^( {0,11})([A-Z0-9][\w-]*)(\s+SECTION)?\s*\.\s*$"));
    static PERFORM: LazyLock<Regex> = LazyLock::new(|| {
        re(r"(?i)\bPERFORM\s+([A-Z0-9][\w-]*)(?:\s+(?:THRU|THROUGH)\s+([A-Z0-9][\w-]*))?")
    });
    static CALL: LazyLock<Regex> = LazyLock::new(|| re(r#"(?i)\bCALL\s+['"]([\w-]+)['"]"#));
    static COPY: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bCOPY\s+([\w-]+)"));
    const NOT_PARAGRAPHS: &[&str] = &[
        "EXIT",
        "GOBACK",
        "CONTINUE",
        "END-IF",
        "END-PERFORM",
        "END-EVALUATE",
        "END-READ",
        "ELSE",
        "STOP",
        "END-CALL",
        "END-STRING",
        "END-SEARCH",
        "END-COMPUTE",
    ];
    const NOT_TARGETS: &[&str] = &["UNTIL", "VARYING", "WITH", "TEST", "FOREVER"];
    let code = cobol_mask(b.text);
    let ends: Vec<usize> = END_PROGRAM.find_iter(&code).map(|m| m.start()).collect();
    let programs: Vec<(String, usize)> = PROGRAM
        .captures_iter(&code)
        .map(|c| (c[1].to_string(), c.get(0).unwrap().start()))
        .collect();
    for (name, start) in &programs {
        let end = ends
            .iter()
            .copied()
            .find(|e| e > start)
            .unwrap_or(code.len().saturating_sub(1));
        b.def(NodeKind::Function, name, *start, end);
    }
    for c in each(&COPY, &code) {
        b.import(&c[1], c.get(0).unwrap().start());
    }
    if let Some(proc_start) = PROCEDURE.find(&code).map(|m| m.end()) {
        let heads: Vec<(String, usize, bool)> = PARAGRAPH
            .captures_iter(&code[proc_start..])
            .filter(|c| !NOT_PARAGRAPHS.contains(&c[2].to_ascii_uppercase().as_str()))
            .map(|c| {
                (
                    c[2].to_string(),
                    proc_start + c.get(2).unwrap().start(),
                    c.get(3).is_some(),
                )
            })
            .collect();
        for (i, (name, start, _)) in heads.iter().enumerate() {
            let end = heads
                .get(i + 1)
                .map(|n| code[..n.1].rfind('\n').unwrap_or(n.1).saturating_sub(1))
                .or_else(|| ends.iter().copied().find(|e| e > start))
                .unwrap_or(code.len().saturating_sub(1));
            b.def(NodeKind::Function, name, *start, end);
        }
    }
    let mut found = Vec::new();
    for c in each(&PERFORM, &code) {
        for m in [c.get(1), c.get(2)].into_iter().flatten() {
            if !NOT_TARGETS.contains(&m.as_str().to_ascii_uppercase().as_str()) {
                found.push((m.as_str().to_string(), m.start()));
            }
        }
    }
    for c in each(&CALL, &code) {
        found.push((c[1].to_string(), c.get(1).unwrap().start()));
    }
    for (name, off) in found {
        b.reference(&name, ReferenceKind::Calls, off);
    }
}

// ------------------------------------------------------------------- VB.NET

const VB_SYNTAX: Syntax = Syntax {
    line: &["'"],
    block: &[],
    quotes: b"\"",
};

const VB_KEYWORDS: &[&str] = &[
    "If",
    "ElseIf",
    "While",
    "For",
    "Select",
    "Case",
    "Return",
    "Not",
    "And",
    "Or",
    "AndAlso",
    "OrElse",
    "CType",
    "DirectCast",
    "TryCast",
    "GetType",
    "NameOf",
    "CInt",
    "CStr",
    "CDbl",
    "CBool",
    "CLng",
    "CDate",
    "CDec",
    "CObj",
    "CSng",
    "CShort",
    "CByte",
    "CChar",
    "Sub",
    "Function",
    "New",
    "Is",
    "IsNot",
    "TypeOf",
    "Using",
    "SyncLock",
    "Me",
    "MyBase",
    "Call",
    "Handles",
    "Of",
    "As",
    "In",
    "Await",
];

fn vbnet(b: &mut Builder<'_>) {
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"(?im)^[ \t]*(?:<[^>\n]*>\s*)?((?:(?:Public|Private|Protected|Friend|Shared|Partial|MustInherit|NotInheritable|Overrides|Overridable|Overloads|MustOverride|Shadows|Async|Iterator|ReadOnly|WriteOnly|Default|Static|NotOverridable|Widening|Narrowing|Declare(?:\s+(?:Ansi|Unicode|Auto))?)\s+)*)(Class|Module|Structure|Interface|Enum|Namespace|Sub|Function|Property|Operator)\s+([A-Za-z_][\w.]*)",
        )
    });
    static END: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r"(?im)^[ \t]*End\s+(Class|Module|Structure|Interface|Enum|Namespace|Sub|Function|Property|Operator)\b",
        )
    });
    static IMPORTS: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?im)^\s*Imports\s+(?:\w+\s*=\s*)?([\w.]+)"));
    static INHERITS: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?im)^\s*(Inherits|Implements)\s+([\w.,\s(Of)]+?)\s*$"));
    static NEW: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bNew\s+([A-Za-z_][\w.]*)"));
    let code = mask(b.text, &VB_SYNTAX, true);
    for c in each(&IMPORTS, &code) {
        b.import(&c[1], c.get(0).unwrap().start());
    }
    let headers = each(&HEADER, &code);
    let ends: Vec<(String, usize)> = END
        .captures_iter(&code)
        .map(|c| (c[1].to_ascii_lowercase(), c.get(0).unwrap().start()))
        .collect();
    let mut used = vec![false; ends.len()];
    // Innermost first: match each header (from the last) to the first free
    // `End <Kind>` after it.
    let mut spans: Vec<Option<usize>> = vec![None; headers.len()];
    let mut interface_ranges = Vec::new();
    for (hi, c) in headers.iter().enumerate().rev() {
        let kw = c[2].to_ascii_lowercase();
        let mods = c[1].to_ascii_lowercase();
        let start = c.get(0).unwrap().start();
        let bodyless = mods.contains("mustoverride") || mods.contains("declare");
        if bodyless {
            continue;
        }
        // Members before the next header without their own End are bodyless.
        let next_header = headers.get(hi + 1).map(|n| n.get(0).unwrap().start());
        let found = ends
            .iter()
            .enumerate()
            .find(|(i, (k, at))| !used[*i] && *k == kw && *at > start)
            .map(|(i, (_, at))| (i, *at));
        if let Some((i, at)) = found {
            let member = matches!(kw.as_str(), "sub" | "function" | "property" | "operator");
            if member
                && next_header.is_some_and(|n| n < at)
                && headers[hi + 1][2].eq_ignore_ascii_case(&c[2])
            {
                continue;
            }
            used[i] = true;
            spans[hi] = Some(at);
        }
    }
    for (hi, c) in headers.iter().enumerate() {
        let start = c.get(0).unwrap().start();
        let in_interface = interface_ranges
            .iter()
            .any(|&(s, e)| s < start && start < e);
        let end = if in_interface { None } else { spans[hi] };
        let kind = match c[2].to_ascii_lowercase().as_str() {
            "class" => NodeKind::Class,
            "module" => NodeKind::Module,
            "structure" => NodeKind::Struct,
            "interface" => NodeKind::Interface,
            "enum" => NodeKind::Enum,
            "namespace" => NodeKind::Namespace,
            "property" => NodeKind::Property,
            _ => NodeKind::Function,
        };
        let end_off = end.unwrap_or(start);
        if kind == NodeKind::Interface {
            interface_ranges.push((start, end_off));
        }
        let name = c[3].rsplit('.').next().unwrap_or(&c[3]).to_string();
        let d = b.def(kind, &name, start, end_off);
        b.defs[d].exported = !c[1].to_ascii_lowercase().contains("private");
    }
    for c in each(&INHERITS, &code) {
        let off = c.get(0).unwrap().start();
        let line = b.line(off);
        let Some(owner) = b
            .defs
            .iter()
            .enumerate()
            .filter(|(_, d)| d.kind.is_type() && d.start < line && line <= d.end)
            .min_by_key(|(_, d)| d.end - d.start)
            .map(|(i, _)| i)
        else {
            continue;
        };
        let kind = if c[1].eq_ignore_ascii_case("inherits") {
            ReferenceKind::Extends
        } else {
            ReferenceKind::Implements
        };
        for t in c[2].split(',') {
            let t = t.split('(').next().unwrap_or("").trim();
            if !t.is_empty() {
                b.reference_from(owner, t, kind);
            }
        }
    }
    let news: Vec<(String, usize)> = NEW
        .captures_iter(&code)
        .map(|c| (c[1].to_string(), c.get(1).unwrap().start()))
        .collect();
    for (name, off) in news {
        b.reference(&name, ReferenceKind::Instantiates, off);
    }
    calls(b, &code, VB_KEYWORDS, true);
}

// --------------------------------------------------------------------- CFML

const CFML_SYNTAX: Syntax = Syntax {
    line: &["//"],
    block: &[("<!---", "--->"), ("/*", "*/")],
    quotes: b"\"'",
};

const CFML_KEYWORDS: &[&str] = &[
    "if",
    "for",
    "while",
    "switch",
    "return",
    "function",
    "catch",
    "try",
    "var",
    "new",
    "isDefined",
    "len",
    "arrayLen",
    "structKeyExists",
    "isNull",
    "writeOutput",
    "dump",
    "createObject",
    "local",
    "arguments",
    "this",
    "variables",
];

fn cfml(b: &mut Builder<'_>) {
    static COMPONENT: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?i)(?:^|\n)\s*(?:@\w+\s*)*component\b([^{]*)\{"));
    static TAG_COMPONENT: LazyLock<Regex> =
        LazyLock::new(|| re(r"(?i)<cf(?:component|interface)\b([^>]*)>"));
    static ATTR_EXTENDS: LazyLock<Regex> =
        LazyLock::new(|| re(r#"(?i)\b(extends|implements)\s*=\s*["']([\w.,\s]+)["']"#));
    static FUNC: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)\bfunction\s+([A-Za-z_]\w*)\s*\("));
    static TAG_FUNC: LazyLock<Regex> =
        LazyLock::new(|| re(r#"(?i)<cffunction\b[^>]*?\bname\s*=\s*["'](\w+)["'][^>]*>"#));
    static TAG_FUNC_END: LazyLock<Regex> = LazyLock::new(|| re(r"(?i)</cffunction>"));
    static NEW: LazyLock<Regex> = LazyLock::new(|| {
        re(
            r#"(?i)\bnew\s+([A-Za-z_][\w.]*)\s*\(|createObject\(\s*["']component["']\s*,\s*["']([\w.]+)["']"#,
        )
    });
    static INCLUDE: LazyLock<Regex> = LazyLock::new(|| {
        re(r#"(?i)<cfinclude\s+template\s*=\s*["']([^"']+)["']|\binclude\s+["']([^"']+)["']"#)
    });
    let code = mask(b.text, &CFML_SYNTAX, false);
    let safe = mask(b.text, &CFML_SYNTAX, true);
    let stem = b
        .path
        .rsplit('/')
        .next()
        .unwrap_or(b.path)
        .split('.')
        .next()
        .unwrap_or("")
        .to_string();
    for c in each(&INCLUDE, &code) {
        let m = c.get(1).or(c.get(2)).unwrap();
        b.import(m.as_str(), c.get(0).unwrap().start());
    }
    let comp = COMPONENT
        .captures(&code)
        .map(|c| {
            (
                c.get(0).unwrap().start(),
                brace_end(&safe, c.get(0).unwrap().end() - 1),
                c[1].to_string(),
            )
        })
        .or_else(|| {
            TAG_COMPONENT.captures(&code).map(|c| {
                (
                    c.get(0).unwrap().start(),
                    code.len().saturating_sub(1),
                    c[1].to_string(),
                )
            })
        });
    if let Some((start, end, attrs)) = comp {
        let d = b.def(NodeKind::Class, &stem, start, end);
        for a in ATTR_EXTENDS.captures_iter(&attrs) {
            let kind = if a[1].eq_ignore_ascii_case("extends") {
                ReferenceKind::Extends
            } else {
                ReferenceKind::Implements
            };
            for t in a[2].split(',') {
                let t = t.trim();
                let t = t.rsplit('.').next().unwrap_or(t);
                if !t.is_empty() {
                    b.reference_from(d, t, kind);
                }
            }
        }
    }
    for c in each(&FUNC, &safe) {
        let start = c.get(0).unwrap().start();
        // Include return type and access modifiers on the same line.
        let end = brace_end(&safe, c.get(0).unwrap().end());
        b.def(NodeKind::Function, &c[1], start, end);
    }
    let tag_ends: Vec<usize> = TAG_FUNC_END.find_iter(&code).map(|m| m.start()).collect();
    for c in each(&TAG_FUNC, &code) {
        let start = c.get(0).unwrap().start();
        let end = tag_ends
            .iter()
            .copied()
            .find(|&e| e > start)
            .unwrap_or(start);
        b.def(NodeKind::Function, &c[1], start, end);
    }
    let news: Vec<(String, usize)> = NEW
        .captures_iter(&code)
        .filter_map(|c| {
            let m = c.get(1).or(c.get(2))?;
            let t = m
                .as_str()
                .rsplit('.')
                .next()
                .unwrap_or(m.as_str())
                .to_string();
            Some((t, m.start()))
        })
        .collect();
    for (name, off) in news {
        b.reference(&name, ReferenceKind::Instantiates, off);
    }
    calls(b, &safe, CFML_KEYWORDS, true);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(r: &ExtractionResult, kind: NodeKind) -> Vec<String> {
        r.nodes
            .iter()
            .filter(|n| n.kind == kind)
            .map(|n| n.name.clone())
            .collect()
    }

    fn span(r: &ExtractionResult, name: &str) -> (u32, u32) {
        let n = r.nodes.iter().find(|n| n.name == name).unwrap();
        (n.start_line, n.end_line)
    }

    fn refs(r: &ExtractionResult, kind: ReferenceKind) -> Vec<String> {
        r.unresolved_references
            .iter()
            .filter(|x| x.reference_kind == kind)
            .map(|x| x.reference_name.clone())
            .collect()
    }

    #[test]
    fn solidity_contracts() {
        let src = "import \"./Base.sol\";\n// function fake() {}\ncontract Token is Base, IERC20 {\n  event Moved(address to);\n  function transfer(address to) public returns (bool) {\n    _move(to);\n    emit Moved(to);\n    return uint256(1) > 0;\n  }\n  function _move(address to) internal {}\n}\n";
        let r = extract("t.sol", src, Language::Solidity);
        assert_eq!(names(&r, NodeKind::Class), vec!["Token"]);
        assert_eq!(
            names(&r, NodeKind::Method),
            vec!["Moved", "transfer", "_move"]
        );
        assert_eq!(span(&r, "transfer"), (5, 9));
        assert_eq!(refs(&r, ReferenceKind::Extends), vec!["Base", "IERC20"]);
        let calls = refs(&r, ReferenceKind::Calls);
        assert_eq!(calls, vec!["_move", "Moved"], "{calls:?}");
        assert_eq!(refs(&r, ReferenceKind::Imports), vec!["./Base.sol"]);
        let q = r.nodes.iter().find(|n| n.name == "_move").unwrap();
        assert_eq!(q.qualified_name, "t.sol::Token::_move");
    }

    #[test]
    fn pascal_units() {
        let src = "unit Shapes;\ninterface\nuses SysUtils, Classes;\ntype\n  TShape = class(TObject)\n  public\n    procedure Draw; virtual;\n  end;\nfunction Area(x: Integer): Integer;\nimplementation\nfunction Area(x: Integer): Integer;\n  procedure Helper;\n  begin\n    Inc(x);\n  end;\nbegin\n  Helper;\n  case x of 1: x := 2; end;\n  Result := x;\nend;\nprocedure TShape.Draw;\nbegin\n  Area(1);\nend;\nend.\n";
        let r = extract("Shapes.pas", src, Language::Pascal);
        assert_eq!(names(&r, NodeKind::Class), vec!["TShape"]);
        assert_eq!(span(&r, "TShape"), (5, 8));
        let areas: Vec<(u32, u32)> = r
            .nodes
            .iter()
            .filter(|n| n.name == "Area")
            .map(|n| (n.start_line, n.end_line))
            .collect();
        assert_eq!(areas, vec![(9, 9), (11, 20)]);
        let draw = r
            .nodes
            .iter()
            .filter(|n| n.name == "Draw")
            .map(|n| (n.start_line, n.end_line, n.qualified_name.clone()))
            .collect::<Vec<_>>();
        assert_eq!(draw[1], (21, 24, "Shapes.pas::Shapes::TShape::Draw".into()));
        assert_eq!(
            refs(&r, ReferenceKind::Imports),
            vec!["SysUtils", "Classes"]
        );
        let calls = refs(&r, ReferenceKind::Calls);
        assert!(
            calls.contains(&"Helper".to_string()) && calls.contains(&"Area".to_string()),
            "{calls:?}"
        );
    }

    #[test]
    fn terraform_blocks() {
        let src = "variable \"region\" {}\nlocals {\n  name = \"x-${var.region}\"\n  tags = { a = 1 }\n}\nresource \"aws_s3_bucket\" \"logs\" {\n  bucket = local.name\n}\nmodule \"vpc\" {\n  source = \"./modules/vpc\"\n  arn = aws_s3_bucket.logs.arn\n}\n";
        let r = extract("main.tf", src, Language::Terraform);
        assert_eq!(names(&r, NodeKind::Variable), vec!["region"]);
        assert_eq!(names(&r, NodeKind::Constant), vec!["name", "tags"]);
        assert_eq!(names(&r, NodeKind::Struct), vec!["logs"]);
        let logs = r.nodes.iter().find(|n| n.name == "logs").unwrap();
        assert_eq!(logs.qualified_name, "main.tf::aws_s3_bucket::logs");
        assert_eq!(
            refs(&r, ReferenceKind::References),
            vec!["var.region", "local.name", "aws_s3_bucket.logs"]
        );
        assert_eq!(
            refs(&r, ReferenceKind::Imports),
            vec!["./modules/vpc/main.tf"]
        );
    }

    #[test]
    fn erlang_modules() {
        let src = "-module(shop).\n-behaviour(gen_server).\n-include(\"shop.hrl\").\n-record(item, {name, price}).\n\ntotal([]) -> 0;\ntotal([H|T]) ->\n    price(H) + total(T).\n\nprice(I) -> lists:sum([I]).\n";
        let r = extract("shop.erl", src, Language::Erlang);
        assert_eq!(names(&r, NodeKind::Function), vec!["total", "price"]);
        assert_eq!(span(&r, "total"), (6, 8));
        assert_eq!(names(&r, NodeKind::Struct), vec!["item"]);
        let calls = refs(&r, ReferenceKind::Calls);
        assert_eq!(calls, vec!["lists.sum", "price", "total"], "{calls:?}");
        let total = r.nodes.iter().find(|n| n.name == "total").unwrap();
        assert_eq!(total.qualified_name, "shop.erl::shop::total");
    }

    #[test]
    fn cobol_programs() {
        let src = "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. PAYROLL.\n       PROCEDURE DIVISION.\n       MAIN-PARA.\n           PERFORM CALC-PARA\n           CALL 'TAXLIB'\n           STOP RUN.\n       CALC-PARA.\n      * a comment PERFORM NOPE\n           EXIT.\n";
        let r = extract("PAYROLL.cbl", src, Language::Cobol);
        assert_eq!(
            names(&r, NodeKind::Function),
            vec!["PAYROLL", "MAIN-PARA", "CALC-PARA"]
        );
        assert_eq!(span(&r, "MAIN-PARA"), (4, 7));
        assert_eq!(refs(&r, ReferenceKind::Calls), vec!["CALC-PARA", "TAXLIB"]);
    }

    #[test]
    fn vbnet_classes() {
        let src = "Imports System.IO\nPublic Class Report\n    Inherits BaseReport\n    Implements IDisposable\n    Public Property Title As String\n    Public Sub Render()\n        ' Render(nope)\n        Dim w = New Writer()\n        Format(Title)\n    End Sub\n    Public MustOverride Function Total() As Integer\nEnd Class\n";
        let r = extract("Report.vb", src, Language::VbNet);
        assert_eq!(names(&r, NodeKind::Class), vec!["Report"]);
        assert_eq!(span(&r, "Report"), (2, 12));
        assert_eq!(span(&r, "Render"), (6, 10));
        assert_eq!(span(&r, "Title"), (5, 5));
        assert_eq!(refs(&r, ReferenceKind::Extends), vec!["BaseReport"]);
        assert_eq!(refs(&r, ReferenceKind::Implements), vec!["IDisposable"]);
        assert_eq!(refs(&r, ReferenceKind::Instantiates), vec!["Writer"]);
        assert_eq!(refs(&r, ReferenceKind::Calls), vec!["Writer", "Format"]);
    }

    #[test]
    fn objc_classes() {
        let src = "#import \"Base.h\"\n@interface Dog : Animal <Pet>\n- (void)bark;\n@end\n@implementation Dog\n- (void)bark {\n  [self wag:1];\n  NSLog(@\"woof\");\n}\n@end\nstatic int helper(int x) {\n  return x;\n}\n";
        let r = extract("Dog.m", src, Language::ObjC);
        assert_eq!(names(&r, NodeKind::Class), vec!["Dog", "Dog"]);
        assert_eq!(span(&r, "helper"), (11, 13));
        let barks: Vec<(u32, u32)> = r
            .nodes
            .iter()
            .filter(|n| n.name == "bark")
            .map(|n| (n.start_line, n.end_line))
            .collect();
        assert_eq!(barks, vec![(3, 3), (6, 9)]);
        assert_eq!(refs(&r, ReferenceKind::Extends), vec!["Animal"]);
        assert_eq!(refs(&r, ReferenceKind::Implements), vec!["Pet"]);
        assert_eq!(refs(&r, ReferenceKind::Calls), vec!["self.wag"]);
        assert_eq!(refs(&r, ReferenceKind::Imports), vec!["Base.h"]);
    }

    #[test]
    fn nix_bindings() {
        let src = "{ pkgs, ... }:\nlet\n  helper = x: x + 1;\n  version = \"1.0\";\nin {\n  app = pkgs.callPackage ./app { };\n  lib = import ./lib.nix;\n}\n";
        let r = extract("default.nix", src, Language::Nix);
        assert_eq!(names(&r, NodeKind::Function), vec!["helper"]);
        assert_eq!(names(&r, NodeKind::Variable), vec!["version", "app", "lib"]);
        assert_eq!(
            refs(&r, ReferenceKind::Imports),
            vec!["./app/default.nix", "./lib.nix"]
        );
    }

    #[test]
    fn cfml_components() {
        let src = "component extends=\"models.Base\" {\n  public string function greet(name) {\n    return format(name);\n  }\n  private function format(s) { return s; }\n}\n";
        let r = extract("User.cfc", src, Language::Cfml);
        assert_eq!(names(&r, NodeKind::Class), vec!["User"]);
        assert_eq!(names(&r, NodeKind::Method), vec!["greet", "format"]);
        assert_eq!(span(&r, "greet"), (2, 4));
        assert_eq!(refs(&r, ReferenceKind::Extends), vec!["Base"]);
        assert_eq!(refs(&r, ReferenceKind::Calls), vec!["format"]);
    }
}
