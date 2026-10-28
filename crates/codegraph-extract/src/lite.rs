//! Lightweight extractors for languages without a usable tree-sitter
//! grammar crate: COBOL and VB.NET. Each finds declarations with line-anchored
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
    matches!(lang, Cobol | VbNet)
}

pub fn extract(path: &str, content: &str, lang: Language) -> ExtractionResult {
    let mut b = Builder::new(path, content, lang);
    match lang {
        Language::Cobol => cobol(&mut b),
        Language::VbNet => vbnet(&mut b),
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
}
