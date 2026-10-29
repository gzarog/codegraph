//! C++ methods a function-like macro defines, minted where the file invokes
//! the macro (upstream `cpp-macro-methods.ts`).
//!
//! protobuf defines most of its descriptor accessors this way:
//! `PROTOBUF_DEFINE_ACCESSOR(FieldDescriptor, number, int)`, the macro being
//! `inline TYPE CLASS::FIELD() const { return FIELD##_; }`. The parser never
//! sees that definition (the preParse blanks the invocation), so this reads
//! the file's own `#define`s whose body defines `Class::method(…) … {` at its
//! top level, expands each invocation at namespace scope, and mints a method
//! node per definition as the walker mints an out-of-line definition. The node
//! spans the invocation; its signature is the expanded head. Constructors and
//! destructors are left out.

use super::brace_scopes::{self, BraceScopes};
use super::{normalize_cpp_return_type, strip_cpp_template_args};
use crate::{ids, textutil};
use codegraph_core::{Edge, EdgeKind, ExtractionResult, Language, Node, NodeKind};
use regex::Regex;
use std::collections::HashSet;
use std::sync::OnceLock;

/// A function-like macro a file defines (not a variadic one).
struct CppMacro {
    name: String,
    params: Vec<String>,
    /// Its replacement text: continuations spliced, comments blanked.
    body: String,
    /// Where its `#` is.
    at: usize,
}

/// A method definition at the top level of a macro's expansion.
struct DefinitionHead {
    /// The receiver as written, template arguments dropped.
    receiver: String,
    name: String,
    /// The text before the receiver: specifiers and the return type.
    return_text: String,
    /// Everything up to the body's `{`, whitespace collapsed.
    head: String,
}

fn is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_'
}

fn is_word_char(c: u8) -> bool {
    is_word_start(c) || c.is_ascii_digit()
}

fn at(s: &[u8], i: usize) -> u8 {
    s.get(i).copied().unwrap_or(0)
}

fn starts_with_at(text: &str, i: usize, pat: &str) -> bool {
    text.get(i..).is_some_and(|r| r.starts_with(pat))
}

fn find_from(text: &str, from: usize, pat: &str) -> Option<usize> {
    text.get(from..).and_then(|r| r.find(pat)).map(|k| from + k)
}

fn method_macro_candidate_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(
            r"(?m)^[ \t]*#[ \t]*define[ \t]+[A-Za-z_]\w*\([^)\n]*\)((?:[^\n]*\\\r?\n)*[^\n]*)",
        )
        .unwrap()
    })
}

/// Whether the C++ `source` may define a method with a macro of its own.
pub(crate) fn may_define_cpp_macro_methods(source: &str) -> bool {
    if !source.contains("define") {
        return false;
    }
    method_macro_candidate_re().captures_iter(source).any(|m| {
        let body = m.get(1).map_or("", |g| g.as_str());
        body.contains("::") && body.contains('{')
    })
}

/// Add the methods the file's own macros define to an extraction result.
pub(crate) fn append_cpp_macro_methods(
    result: &mut ExtractionResult,
    file_path: &str,
    source: &str,
    pre_parse: impl Fn(&str) -> String,
) {
    if !may_define_cpp_macro_methods(source) {
        return;
    }
    let (nodes, edges) = cpp_macro_methods(file_path, source, pre_parse, &result.nodes);
    result.nodes.extend(nodes);
    result.edges.extend(edges);
}

fn hash_hash_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\s*##\s*").unwrap())
}

fn cpp_macro_methods(
    file_path: &str,
    source: &str,
    pre_parse: impl Fn(&str) -> String,
    nodes: &[Node],
) -> (Vec<Node>, Vec<Edge>) {
    let mut out_nodes = Vec::new();
    let mut out_edges = Vec::new();
    let (macros, undefs) = cpp_macro_directives(source);
    let defining: Vec<&CppMacro> = macros
        .iter()
        .filter(|m| !cpp_definition_heads(&hash_hash_re().replace_all(&m.body, "")).is_empty())
        .collect();
    if defining.is_empty() {
        return (out_nodes, out_edges);
    }
    // Scopes as the parser saw them (after preParse, offsets unchanged).
    let parsed = pre_parse(source);
    let Some(scopes) = brace_scopes::scan(&parsed) else {
        return (out_nodes, out_edges);
    };
    let line_starts = textutil::line_starts(&parsed);
    let position = |offset: usize| BraceScopes::position_of(&parsed, &line_starts, offset);

    let names: HashSet<&str> = defining.iter().map(|m| m.name.as_str()).collect();
    let mut taken: HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();
    let file_id = ids::file_node_id(file_path);
    let now = nodes.first().map_or(0, |n| n.updated_at);
    for call in cpp_macro_invocations(source, &names) {
        if !scopes.at_namespace_scope(call.at) {
            continue;
        }
        let Some(mac) = live_macro(&defining, &undefs, &call.name, call.at) else {
            continue;
        };
        let Some(expansion) = expand_cpp_macro(mac, &call.args) else {
            continue;
        };
        let (start_line, start_column) = position(call.at);
        let (end_line, end_column) = position(call.end);
        let prefix = scopes.namespaces_at(call.at);
        for def in cpp_definition_heads(&expansion) {
            let receiver_last = def.receiver.rsplit("::").next().unwrap_or("");
            if def.name == receiver_last || def.name.starts_with('~') {
                continue;
            }
            let base = ids::node_id(file_path, "method", &def.name, start_line);
            let mut id = base.clone();
            if taken.contains(&id) {
                id = format!("{base}:{start_column}");
            }
            let mut k = 1;
            while taken.contains(&id) {
                id = format!("{base}:{start_column}:{k}");
                k += 1;
            }
            taken.insert(id.clone());
            let mut node = Node::new(
                id.clone(),
                NodeKind::Method,
                def.name.clone(),
                file_path,
                Language::Cpp,
                start_line,
                end_line,
            );
            node.qualified_name = compose_qualified_name(&prefix, &def.receiver, &def.name);
            node.start_column = start_column;
            node.end_column = end_column;
            node.signature = Some(def.head.clone());
            node.return_type = normalize_cpp_return_type(&cpp_type_text(&def.return_text));
            node.updated_at = now;
            out_nodes.push(node);
            out_edges.push(Edge::new(file_id.clone(), id.clone(), EdgeKind::Contains));
            let owner = nodes.iter().find(|n| {
                n.name == def.receiver
                    && n.file_path == file_path
                    && matches!(
                        n.kind,
                        NodeKind::Struct
                            | NodeKind::Union
                            | NodeKind::Class
                            | NodeKind::Enum
                            | NodeKind::Trait
                    )
            });
            if let Some(owner) = owner {
                out_edges.push(Edge::new(owner.id.clone(), id, EdgeKind::Contains));
            }
        }
    }
    (out_nodes, out_edges)
}

/// The qualified name of `receiver::name` inside the namespaces `prefix`,
/// anchored at the first namespace the receiver re-spells.
fn compose_qualified_name(prefix: &[String], receiver: &str, name: &str) -> String {
    let base = format!("{receiver}::{name}");
    if prefix.is_empty() {
        return base;
    }
    let head = receiver.split("::").next().unwrap_or("");
    let kept = match prefix.iter().position(|p| p == head) {
        Some(anchor) => &prefix[..anchor],
        None => prefix,
    };
    if kept.is_empty() {
        base
    } else {
        format!("{}::{base}", kept.join("::"))
    }
}

fn type_text_res() -> &'static [Regex; 3] {
    static R: OnceLock<[Regex; 3]> = OnceLock::new();
    R.get_or_init(|| {
        [
            Regex::new(r"^\[\[[^\]]*\]\]\s*").unwrap(),
            Regex::new(
                r"^(?:inline|static|virtual|constexpr|consteval|explicit|extern|friend|__forceinline)\b\s*",
            )
            .unwrap(),
            Regex::new(r"^[A-Z][A-Z0-9]*_[A-Z0-9_]*(?:\s*\([^()]*\))?\s+").unwrap(),
        ]
    })
}

/// The type a definition's leading text writes, without specifiers, template header and attributes.
fn cpp_type_text(return_text: &str) -> String {
    let mut text = return_text.trim().to_string();
    loop {
        let before = text.clone();
        if text.starts_with("template") {
            let close = text
                .find('<')
                .and_then(|open| matching(&text, open, b'<', b'>'));
            let Some(close) = close else {
                return String::new();
            };
            text = text[close + 1..].trim().to_string();
        }
        for re in type_text_res() {
            text = re.replace(&text, "").into_owned();
        }
        if text == before {
            return text;
        }
    }
}

/// Where the `close` matching the `open` at `text[from]` is.
fn matching(text: &str, from: usize, open: u8, close: u8) -> Option<usize> {
    let s = text.as_bytes();
    let mut depth = 0i32;
    let mut i = from;
    while i < s.len() {
        let c = s[i];
        if c == b'"' || c == b'\'' {
            i = quoted_end(s, i);
            continue;
        }
        if c == open {
            depth += 1;
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
        i += 1;
    }
    None
}

/// Past the string or character literal whose quote is at `i`.
fn quoted_end(s: &[u8], i: usize) -> usize {
    let quote = s[i];
    let mut j = i + 1;
    while j < s.len() && s[j] != quote && s[j] != b'\n' {
        j += if s[j] == b'\\' { 2 } else { 1 };
    }
    s.len().min(j + 1)
}

fn skip_blank(text: &str, mut i: usize) -> usize {
    let s = text.as_bytes();
    while i < s.len() && s[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

fn word_end(text: &str, i: usize) -> Option<usize> {
    let s = text.as_bytes();
    if !is_word_start(at(s, i)) {
        return None;
    }
    let mut j = i + 1;
    while j < s.len() && is_word_char(s[j]) {
        j += 1;
    }
    Some(j)
}

fn operator_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"^operator\s*(\(\s*\)|\[\s*\]|[^\s\w(]+|\w+)\s*\(").unwrap())
}

/// The qualified name starting at `i` that an argument list follows: its
/// segments and the offset of the `(`. None without a `::`.
fn qualified_callable_at(text: &str, i: usize) -> Option<(Vec<String>, usize)> {
    let s = text.as_bytes();
    let mut segments: Vec<String> = Vec::new();
    let mut j = i;
    if starts_with_at(text, j, "::") {
        j = skip_blank(text, j + 2);
    }
    loop {
        if starts_with_at(text, j, "operator") && !is_word_char(at(s, j + 8)) {
            let op = operator_re().captures(&text[j..])?;
            if segments.is_empty() {
                return None;
            }
            let sym: String = op[1].chars().filter(|c| !c.is_whitespace()).collect();
            segments.push(format!("operator{sym}"));
            return Some((segments, j + op[0].len() - 1));
        }
        let tilde = at(s, j) == b'~';
        let word_at = if tilde { skip_blank(text, j + 1) } else { j };
        let end = word_end(text, word_at)?;
        let word = &text[word_at..end];
        segments.push(if tilde {
            format!("~{word}")
        } else {
            word.to_string()
        });
        let mut k = skip_blank(text, end);
        if at(s, k) == b'<' && !tilde {
            let close = matching(text, k, b'<', b'>')?;
            segments.last_mut().unwrap().push_str(&text[k..=close]);
            k = skip_blank(text, close + 1);
        }
        if starts_with_at(text, k, "::") {
            if tilde {
                return None;
            }
            j = skip_blank(text, k + 2);
            continue;
        }
        return if at(s, k) == b'(' && segments.len() > 1 {
            Some((segments, k))
        } else {
            None
        };
    }
}

fn qualifier_word_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        Regex::new(r"^(?:const|volatile|override|final|noexcept|throw|[A-Z][A-Z0-9]*_[A-Z0-9_]*)$")
            .unwrap()
    })
}

/// After a definition's parameter list (from `i`): the offset of its body's `{`.
fn body_open_after(text: &str, i: usize) -> Option<usize> {
    let s = text.as_bytes();
    let mut j = skip_blank(text, i);
    loop {
        let c = at(s, j);
        if c == b'{' {
            return Some(j);
        }
        if c == b'&' {
            j = skip_blank(text, j + if at(s, j + 1) == b'&' { 2 } else { 1 });
            continue;
        }
        if starts_with_at(text, j, "[[") {
            let close = find_from(text, j + 2, "]]")?;
            j = skip_blank(text, close + 2);
            continue;
        }
        if starts_with_at(text, j, "->") {
            let mut depth = 0i32;
            for (k, &d) in s.iter().enumerate().skip(j + 2) {
                match d {
                    b'(' | b'<' | b'[' => depth += 1,
                    b')' | b'>' | b']' => depth -= 1,
                    b'{' if depth <= 0 => return Some(k),
                    b';' if depth <= 0 => return None,
                    _ => {}
                }
            }
            return None;
        }
        let end = word_end(text, j)?;
        let word = &text[j..end];
        if !qualifier_word_re().is_match(word) {
            return None;
        }
        j = skip_blank(text, end);
        if at(s, j) == b'(' && !matches!(word, "const" | "volatile" | "override" | "final") {
            let close = matching(text, j, b'(', b')')?;
            j = skip_blank(text, close + 1);
        }
    }
}

fn ws_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"\s+").unwrap())
}

/// The method definitions at the top level of a macro's expansion `text`, in order.
fn cpp_definition_heads(text: &str) -> Vec<DefinitionHead> {
    let s = text.as_bytes();
    let mut heads = Vec::new();
    let mut segment = 0;
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c == b'"' || c == b'\'' {
            i = quoted_end(s, i);
            continue;
        }
        if c == b';' || c == b'}' {
            i += 1;
            segment = i;
            continue;
        }
        if c == b'{' || c == b'(' {
            let Some(close) = matching(text, i, c, if c == b'{' { b'}' } else { b')' }) else {
                return heads;
            };
            if c == b'{' {
                segment = close + 1;
            }
            i = close + 1;
            continue;
        }
        let callable = if is_word_start(c) || (c == b':' && at(s, i + 1) == b':') {
            qualified_callable_at(text, i)
        } else {
            None
        };
        let Some((segments, open_paren)) = callable else {
            i = word_end(text, i).unwrap_or(i + 1);
            continue;
        };
        let Some(close) = matching(text, open_paren, b'(', b')') else {
            return heads;
        };
        let Some(open) = body_open_after(text, close + 1) else {
            i = close + 1;
            continue;
        };
        let last = segments.last().unwrap();
        let name = if last.starts_with("operator") {
            last.clone()
        } else {
            strip_cpp_template_args(last)
        };
        let receiver = strip_cpp_template_args(&segments[..segments.len() - 1].join("::"));
        if !receiver.is_empty() {
            heads.push(DefinitionHead {
                receiver,
                name,
                return_text: text[segment..i].trim().to_string(),
                head: ws_re()
                    .replace_all(&text[segment..open], " ")
                    .trim()
                    .to_string(),
            });
        }
        let Some(body_close) = matching(text, open, b'{', b'}') else {
            return heads;
        };
        i = body_close + 1;
        segment = i;
    }
    heads
}

/// The macro of `name` in effect at `offset`: its last `#define` before it, unless an `#undef` came between.
fn live_macro<'a>(
    macros: &[&'a CppMacro],
    undefs: &[(String, usize)],
    name: &str,
    offset: usize,
) -> Option<&'a CppMacro> {
    let mut found: Option<&CppMacro> = None;
    for m in macros {
        if m.name == name && m.at < offset && found.is_none_or(|f| m.at > f.at) {
            found = Some(m);
        }
    }
    let found = found?;
    if undefs
        .iter()
        .any(|(u, at)| u == name && *at > found.at && *at < offset)
    {
        return None;
    }
    Some(found)
}

fn ident_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| Regex::new(r"[A-Za-z_]\w*").unwrap())
}

/// A macro's expansion for the arguments `args`, or None when they don't fit its parameters.
fn expand_cpp_macro(mac: &CppMacro, args: &[String]) -> Option<String> {
    let values: Vec<String> =
        if mac.params.is_empty() && args.len() == 1 && args[0].trim().is_empty() {
            Vec::new()
        } else {
            args.iter().map(|a| a.trim().to_string()).collect()
        };
    if values.len() != mac.params.len() {
        return None;
    }
    let expanded = ident_re().replace_all(&mac.body, |c: &regex::Captures| {
        let word = &c[0];
        match mac.params.iter().position(|p| p == word) {
            Some(k) => values[k].clone(),
            None => word.to_string(),
        }
    });
    Some(hash_hash_re().replace_all(&expanded, "").into_owned())
}

fn directive_res() -> &'static [Regex; 4] {
    static R: OnceLock<[Regex; 4]> = OnceLock::new();
    R.get_or_init(|| {
        [
            Regex::new(r"\\\r?\n").unwrap(),
            Regex::new(r"/\*[\s\S]*?\*/").unwrap(),
            Regex::new(r"^#[ \t]*define[ \t]+([A-Za-z_]\w*)\(([^)]*)\)").unwrap(),
            Regex::new(r"^#[ \t]*undef[ \t]+([A-Za-z_]\w*)").unwrap(),
        ]
    })
}

/// The function-like `#define`s of a C++ source and its `#undef`s, in order.
fn cpp_macro_directives(source: &str) -> (Vec<CppMacro>, Vec<(String, usize)>) {
    let [splice, block, define, undef] = directive_res();
    let mut macros = Vec::new();
    let mut undefs = Vec::new();
    for (at, raw) in cpp_code_scan(source, None).0 {
        let spliced = splice.replace_all(raw, " ");
        let text = without_line_comment(&block.replace_all(&spliced, " ")).to_string();
        if let Some(d) = define.captures(&text) {
            let params: Vec<String> = d[2]
                .split(',')
                .map(|p| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .collect();
            if !params.iter().any(|p| p.ends_with("...")) {
                macros.push(CppMacro {
                    name: d[1].to_string(),
                    params,
                    body: text[d[0].len()..].to_string(),
                    at,
                });
            }
            continue;
        }
        if let Some(u) = undef.captures(&text) {
            undefs.push((u[1].to_string(), at));
        }
    }
    (macros, undefs)
}

struct Invocation {
    name: String,
    at: usize,
    end: usize,
    args: Vec<String>,
}

fn comment_res() -> &'static [Regex; 2] {
    static R: OnceLock<[Regex; 2]> = OnceLock::new();
    R.get_or_init(|| {
        [
            Regex::new(r"/\*[\s\S]*?\*/").unwrap(),
            Regex::new(r"//[^\n]*").unwrap(),
        ]
    })
}

fn without_comments(text: &str) -> String {
    let [block, line] = comment_res();
    line.replace_all(&block.replace_all(text, " "), " ")
        .into_owned()
}

/// The invocations of the macros `names` in code, with their arguments.
fn cpp_macro_invocations(source: &str, names: &HashSet<&str>) -> Vec<Invocation> {
    let s = source.as_bytes();
    let mut calls = Vec::new();
    for (name, word_at, word_end_at) in cpp_code_scan(source, Some(names)).1 {
        let open = skip_blank_and_comments(source, word_end_at);
        if at(s, open) != b'(' {
            continue;
        }
        let mut args = Vec::new();
        let mut depth = 0i32;
        let mut from = open + 1;
        let mut close = None;
        let mut i = open;
        while i < s.len() {
            let c = s[i];
            if c == b'"' || c == b'\'' {
                i = quoted_end(s, i);
                continue;
            } else if c == b'/' && at(s, i + 1) == b'*' {
                i = find_from(source, i + 2, "*/").map_or(s.len(), |e| e + 2);
                continue;
            } else if c == b'/' && at(s, i + 1) == b'/' {
                i = find_from(source, i, "\n").unwrap_or(s.len());
                continue;
            } else if c == b'(' {
                depth += 1;
            } else if c == b')' {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                    break;
                }
            } else if c == b',' && depth == 1 {
                args.push(source[from..i].to_string());
                from = i + 1;
            }
            i += 1;
        }
        let Some(close) = close else {
            continue;
        };
        args.push(source[from..close].to_string());
        calls.push(Invocation {
            name: name.to_string(),
            at: word_at,
            end: close + 1,
            args: args.iter().map(|a| without_comments(a)).collect(),
        });
    }
    calls
}

/// A spliced directive without the `//` comment that ends it.
fn without_line_comment(text: &str) -> &str {
    let s = text.as_bytes();
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c == b'"' || c == b'\'' {
            i = quoted_end(s, i);
            continue;
        }
        if c == b'/' && at(s, i + 1) == b'/' {
            return &text[..i];
        }
        i += 1;
    }
    text
}

fn skip_blank_and_comments(source: &str, mut i: usize) -> usize {
    loop {
        i = skip_blank(source, i);
        if starts_with_at(source, i, "//") {
            i = find_from(source, i, "\n").unwrap_or(source.len());
        } else if starts_with_at(source, i, "/*") {
            i = find_from(source, i + 2, "*/").map_or(source.len(), |e| e + 2);
        } else {
            return i;
        }
    }
}

type Directives<'a> = Vec<(usize, &'a str)>;
type Words<'a> = Vec<(&'a str, usize, usize)>;

/// One pass over C/C++ source, skipping comments and literals: the
/// directives (with their continuations), and the code's identifiers among `names`.
fn cpp_code_scan<'a>(
    source: &'a str,
    names: Option<&HashSet<&str>>,
) -> (Directives<'a>, Words<'a>) {
    let s = source.as_bytes();
    let n = s.len();
    let mut directives = Vec::new();
    let mut words = Vec::new();
    let mut line_start = true;
    let mut last_word_start = 0usize;
    let mut last_word_end: Option<usize> = None;
    let mut i = 0;
    while i < n {
        let c = s[i];
        if c == b'\n' {
            line_start = true;
            i += 1;
            continue;
        }
        if matches!(c, b' ' | b'\t' | b'\r' | 0x0b | 0x0c) {
            i += 1;
            continue;
        }
        if c == b'/' && at(s, i + 1) == b'/' {
            let mut end = i;
            loop {
                match find_from(source, end, "\n") {
                    None => {
                        end = n;
                        break;
                    }
                    Some(nl) => {
                        end = nl;
                        let mut k = nl as isize - 1;
                        if k >= 0 && s[k as usize] == b'\r' {
                            k -= 1;
                        }
                        if k < 0 || s[k as usize] != b'\\' {
                            break;
                        }
                        end += 1;
                    }
                }
            }
            i = end;
            continue;
        }
        if c == b'/' && at(s, i + 1) == b'*' {
            i = find_from(source, i + 2, "*/").map_or(n, |e| e + 2);
            continue;
        }
        if c == b'#' && line_start {
            let mut j = i + 1;
            while j < n {
                let d = s[j];
                if d == b'/' && at(s, j + 1) == b'*' {
                    j = find_from(source, j + 2, "*/").map_or(n - 1, |e| e + 1);
                } else if d == b'\n' {
                    let mut k = j as isize - 1;
                    if k >= 0 && s[k as usize] == b'\r' {
                        k -= 1;
                    }
                    if k < 0 || s[k as usize] != b'\\' {
                        break;
                    }
                }
                j += 1;
            }
            let j = j.min(n);
            directives.push((i, &source[i..j]));
            i = j;
            continue;
        }
        line_start = false;
        if c == b'"' {
            if last_word_end == Some(i) && is_raw_prefix(&s[last_word_start..i]) {
                if let Some(paren) = find_from(source, i + 1, "(") {
                    let delimiter = &source[i + 1..paren];
                    if delimiter.len() <= 16
                        && delimiter.bytes().all(|b| {
                            !b.is_ascii_whitespace() && !matches!(b, b'(' | b')' | b'\\' | b'"')
                        })
                    {
                        let closer = format!("){delimiter}\"");
                        i = find_from(source, paren + 1, &closer)
                            .map_or(n, |e| e + delimiter.len() + 2);
                        continue;
                    }
                }
            }
            i = quoted_end(s, i);
            continue;
        }
        if c == b'\'' {
            if i > 0 && is_word_char(s[i - 1]) && last_word_end != Some(i) {
                i += 1;
                continue;
            }
            i = quoted_end(s, i);
            continue;
        }
        if is_word_char(c) {
            let mut j = i + 1;
            while j < n && is_word_char(s[j]) {
                j += 1;
            }
            if is_word_start(c) {
                last_word_start = i;
                last_word_end = Some(j);
                let word = &source[i..j];
                if names.is_some_and(|ns| ns.contains(word)) {
                    words.push((word, i, j));
                }
            }
            i = j;
            continue;
        }
        i += 1;
    }
    (directives, words)
}

fn is_raw_prefix(w: &[u8]) -> bool {
    matches!(w, b"R" | b"uR" | b"UR" | b"LR" | b"u8R")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mints_accessor_methods() {
        let src = "namespace google {\nnamespace protobuf {\n#define PROTOBUF_DEFINE_ACCESSOR(CLASS, FIELD, TYPE) \\\n  inline TYPE CLASS::FIELD() const { return FIELD##_; }\nclass FieldDescriptor {\n public:\n  int number() const;\n};\nPROTOBUF_DEFINE_ACCESSOR(FieldDescriptor, number, int)\n}\n}\n";
        let mut result = ExtractionResult::default();
        append_cpp_macro_methods(&mut result, "d.h", src, |s| s.to_string());
        assert_eq!(result.nodes.len(), 1);
        let n = &result.nodes[0];
        assert_eq!(
            n.qualified_name,
            "google::protobuf::FieldDescriptor::number"
        );
        assert_eq!(
            n.signature.as_deref(),
            Some("inline int FieldDescriptor::number() const")
        );
        assert_eq!(n.start_line, 9);
    }
}
