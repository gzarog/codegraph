//! C/C++ source pre-processing before tree-sitter (upstream `c-cpp.ts`'s
//! `preParseCppSource` / `preParseCSource`).
//!
//! Each pass blanks macro shapes the grammars can't read — export and
//! attribute macros, Unreal reflection markup, a macro alone on its line,
//! iterator macros, compiler annotations — with spaces, so every byte offset,
//! line and column survives and the declarations around them parse cleanly.
//! Offsets are bytes: a blanked character becomes as many spaces as it has
//! bytes. Directive lines are restored at the end (`restore_directive_lines`),
//! and raw string literals are masked throughout (`mask_cpp_raw_strings`).

use fancy_regex::Regex;

type Captures<'t> = fancy_regex::Captures<'t, str>;
use std::cell::RefCell;
use std::sync::OnceLock;

macro_rules! re {
    ($name:ident, $pat:expr) => {
        fn $name() -> &'static Regex {
            static R: OnceLock<Regex> = OnceLock::new();
            R.get_or_init(|| Regex::new($pat).unwrap())
        }
    };
}

/// The first match at or after `pos` (lookbehind sees what lies before).
fn caps_at<'t>(re: &Regex, text: &'t str, pos: usize) -> Option<Captures<'t>> {
    if pos > text.len() {
        return None;
    }
    re.captures_from_pos(text, pos).ok().flatten()
}

fn is_match(re: &Regex, text: &str) -> bool {
    re.is_match(text).unwrap_or(false)
}

/// Where the next search starts after a match `[start, end)`: past it, or
/// past one character for an empty match.
fn next_pos(text: &str, start: usize, end: usize) -> usize {
    if end > start {
        end
    } else {
        end + text[end..].chars().next().map_or(1, char::len_utf8)
    }
}

/// `text.replace(re, f)` for a global regex.
fn replace_all(re: &Regex, text: &str, mut f: impl FnMut(&Captures) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    let mut pos = 0;
    let mut matched = false;
    while let Some(c) = caps_at(re, text, pos) {
        let m = c.get(0).unwrap();
        out.push_str(&text[last..m.start()]);
        out.push_str(&f(&c));
        last = m.end();
        matched = true;
        pos = next_pos(text, m.start(), m.end());
    }
    if !matched {
        return text.to_string();
    }
    out.push_str(&text[last..]);
    out
}

/// `s` with every byte but line endings turned to a space.
fn spaces_keeping_lines(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b == b'\n' || b == b'\r' {
                b as char
            } else {
                ' '
            }
        })
        .collect()
}

/// `s` with every byte but `\r` turned to a space (a line from `split('\n')`).
fn spaces_keeping_cr(s: &str) -> String {
    s.bytes()
        .map(|b| if b == b'\r' { '\r' } else { ' ' })
        .collect()
}

fn blank_bytes(buf: &mut [u8], start: usize, end: usize) {
    let end = end.min(buf.len());
    for b in &mut buf[start..end] {
        if *b != b'\n' && *b != b'\r' {
            *b = b' ';
        }
    }
}

fn into_string(buf: Vec<u8>) -> String {
    String::from_utf8(buf).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn floor(s: &str, mut i: usize) -> usize {
    i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn byte(s: &str, i: usize) -> u8 {
    s.as_bytes().get(i).copied().unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Raw strings
// ---------------------------------------------------------------------------

/// A source with its raw string literals masked, and how to put them back.
pub(crate) struct RawStrings {
    original: String,
    pub masked: String,
    spans: Vec<(usize, usize)>,
}

impl RawStrings {
    /// Restore the raw literals' bytes in a blanked copy of `masked`; a
    /// length-changing rewrite can't be restored, so the original comes back.
    pub fn restore(&self, blanked: String) -> String {
        if self.spans.is_empty() {
            return blanked;
        }
        if blanked == self.masked || blanked.len() != self.original.len() {
            return self.original.clone();
        }
        let mut out = blanked.into_bytes();
        let src = self.original.as_bytes();
        for &(start, end) in &self.spans {
            for i in start..end {
                if out[i] == 0 {
                    out[i] = src[i];
                }
            }
        }
        into_string(out)
    }
}

re!(
    raw_string_scan_re,
    r#"//[^\r\n]*|/\*[\s\S]*?(?:\*/|$)|\b(?:u8|[LuU])?R"([^ \t\x0b\x0c\r\n()\\]{0,16})\(|"(?:\\[\s\S]|[^"\\])*(?:"|$)|(?<!\w)(?:u8|[LuU])?'(?:\\[\s\S]|[^'\\])*(?:'|$)"#
);

/// Hide C++ raw literals from the offset-preserving scans (#1505): the whole
/// literal becomes `\0`s, line endings kept.
pub(crate) fn mask_cpp_raw_strings(source: &str) -> RawStrings {
    let unchanged = || RawStrings {
        original: source.to_string(),
        masked: source.to_string(),
        spans: Vec::new(),
    };
    if !source.contains("R\"") {
        return unchanged();
    }
    let re = raw_string_scan_re();
    let mut spans = Vec::new();
    let mut out = String::with_capacity(source.len());
    let mut last = 0;
    let mut pos = 0;
    while let Some(c) = caps_at(re, source, pos) {
        let m = c.get(0).unwrap();
        let Some(delim) = c.get(1) else {
            pos = next_pos(source, m.start(), m.end());
            continue;
        };
        let closer = format!("){}\"", delim.as_str());
        let end = source[m.end()..]
            .find(&closer)
            .map_or(source.len(), |i| m.end() + i + closer.len());
        spans.push((m.start(), end));
        out.push_str(&source[last..m.start()]);
        out.extend(source[m.start()..end].bytes().map(|b| {
            if b == b'\n' || b == b'\r' {
                b as char
            } else {
                '\0'
            }
        }));
        last = end;
        pos = end;
    }
    if spans.is_empty() {
        return unchanged();
    }
    out.push_str(&source[last..]);
    RawStrings {
        original: source.to_string(),
        masked: out,
        spans,
    }
}

// ---------------------------------------------------------------------------
// Masks
// ---------------------------------------------------------------------------

re!(
    comment_or_literal_re,
    r#"//(?:\\\r?\n|[^\r\n])*|/\*[\s\S]*?(?:\*/|$)|"(?:\\[\s\S]|[^"\\])*(?:"|$)|(?<!\w)(?:u8|[LuU])?'(?:\\[\s\S]|[^'\\])*(?:'|$)"#
);
re!(directive_line_re, r"(?m)^[ \t]*#(?:\\\r?\n|[^\r\n])*");

/// The source with comments and string/char literals turned to spaces.
fn mask_cpp_comments_and_literals(source: &str) -> String {
    replace_all(comment_or_literal_re(), source, |c| {
        spaces_keeping_lines(c.get(0).unwrap().as_str())
    })
}

thread_local! {
    /// The last source `mask_cpp_code` masked, and its mask.
    static MASK_MEMO: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

/// The code the macro scans match on, at the source's offsets: comments and
/// directives are spaces, a literal keeps its place as `\x01`s.
fn mask_cpp_code(source: &str) -> String {
    if let Some(hit) = MASK_MEMO.with(|m| {
        m.borrow()
            .as_ref()
            .filter(|(s, _)| s == source)
            .map(|(_, c)| c.clone())
    }) {
        return hit;
    }
    let literals = replace_all(comment_or_literal_re(), source, |c| {
        let m = c.get(0).unwrap().as_str();
        let fill = if m.starts_with('/') { ' ' } else { '\x01' };
        m.bytes()
            .map(|b| {
                if b == b'\n' || b == b'\r' {
                    b as char
                } else {
                    fill
                }
            })
            .collect()
    });
    let code = replace_all(directive_line_re(), &literals, |c| {
        spaces_keeping_lines(c.get(0).unwrap().as_str())
    });
    MASK_MEMO.with(|m| *m.borrow_mut() = Some((source.to_string(), code.clone())));
    code
}

re!(
    run_token_re,
    r"[A-Z][A-Z0-9_]*(?:\s*\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\))?"
);

/// A span for each macro of `run` (found at `start`).
fn push_run_tokens(spans: &mut Vec<(usize, usize)>, run: &str, start: usize) {
    let mut pos = 0;
    while let Some(c) = caps_at(run_token_re(), run, pos) {
        let m = c.get(0).unwrap();
        spans.push((start + m.start(), start + m.end()));
        pos = next_pos(run, m.start(), m.end());
    }
}

/// Blank each span of `source`; given the mask they were found on, blank it
/// the same way and keep it for the next pass.
fn blank_spans(source: &str, spans: &[(usize, usize)], code: Option<&str>) -> String {
    if spans.is_empty() {
        return source.to_string();
    }
    let blank = |text: &str| {
        let mut buf = text.as_bytes().to_vec();
        for &(s, e) in spans {
            blank_bytes(&mut buf, s, e);
        }
        into_string(buf)
    };
    let out = blank(source);
    if let Some(code) = code {
        let masked = blank(code);
        MASK_MEMO.with(|m| *m.borrow_mut() = Some((out.clone(), masked)));
    }
    out
}

fn skip_space(code: &str, mut i: usize) -> usize {
    while i < code.len() && is_space(byte(code, i)) {
        i += 1;
    }
    i
}

/// Just past the template argument list opening at `code[open]`, or None
/// when it doesn't close before a `;`, `{` or `}`.
fn skip_angle_args(code: &str, open: usize) -> Option<usize> {
    let b = code.as_bytes();
    let mut angles = 0i32;
    let mut parens = 0i32;
    let limit = code.len().min(open + 4000);
    for i in open..limit {
        match b[i] {
            b'(' => parens += 1,
            b')' => {
                parens -= 1;
                if parens < 0 {
                    return None;
                }
            }
            b';' | b'{' | b'}' => return None,
            b'<' if parens == 0 => angles += 1,
            b'>' if parens == 0 && (i == 0 || b[i - 1] != b'-') => {
                angles -= 1;
                if angles == 0 {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

// ---------------------------------------------------------------------------
// C++ passes
// ---------------------------------------------------------------------------

re!(
    class_head_macros_re,
    r"\b(?:class|struct)\s+((?:[A-Z][A-Z0-9_]+(?:\s*\((?:[^()]|\([^()]*\))*\))?\s+)+)(?!final\b)[A-Za-z_]\w*(?:\s*::\s*[A-Za-z_]\w*)*"
);
re!(class_head_hint_re, r"\b(?:class|struct)\s+[A-Z][A-Z0-9_]");

/// Blank export macros in a class head (`class PROTOBUF_EXPORT Any final :`).
pub(crate) fn blank_cpp_export_macros(source: &str) -> String {
    if !source.contains("class") && !source.contains("struct") {
        return source.to_string();
    }
    if !is_match(class_head_hint_re(), source) {
        return source.to_string();
    }
    let code = mask_cpp_code(source);
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(c) = caps_at(class_head_macros_re(), &code, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(&code, m.start(), m.end());
        let mut i = skip_space(&code, m.end());
        if byte(&code, i) == b'<' {
            match skip_angle_args(&code, i) {
                Some(j) => i = skip_space(&code, j),
                None => continue,
            }
        }
        if code[i..].starts_with("final") && !is_word(byte(&code, i + 5)) {
            i = skip_space(&code, i + 5);
        }
        let at = byte(&code, i);
        if at != b'{' && !(at == b':' && byte(&code, i + 1) != b':') {
            continue;
        }
        let run = c.get(1).unwrap();
        push_run_tokens(&mut spans, run.as_str(), run.start());
    }
    blank_spans(source, &spans, Some(&code))
}

re!(
    pointer_annotation_re,
    r"([*&])\s*((?:[A-Z][A-Z0-9]*_[A-Z0-9_]*(?:\s+|(?=\*)))+)(?=\*|::|(?!(?:and|and_eq|bitand|bitor|compl|not|not_eq|or|or_eq|xor|xor_eq)\b)[A-Za-z_~])"
);
re!(pointer_annotation_hint_re, r"[*&]\s*[A-Z][A-Z0-9]*_");
re!(product_before_re, r"(?:[)\]]|\b\d[\w.]*)\s*$");

/// Blank attribute macros between a pointer or reference and its name
/// (`const Descriptor* PROTOBUF_NONNULL descriptor()`).
pub(crate) fn blank_cpp_pointer_annotation_macros(source: &str) -> String {
    if !is_match(pointer_annotation_hint_re(), source) {
        return source.to_string();
    }
    let code = mask_cpp_code(source);
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(c) = caps_at(pointer_annotation_re(), &code, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(&code, m.start(), m.end());
        let run = c.get(2).unwrap();
        let start = m.end() - run.as_str().len();
        if c.get(1).unwrap().as_str() == "&" && byte(&code, start + run.as_str().len()) == b'*' {
            continue;
        }
        let before = &code[floor(&code, m.start().saturating_sub(40))..m.start()];
        if is_match(product_before_re(), before) {
            continue;
        }
        push_run_tokens(&mut spans, run.as_str(), start);
    }
    blank_spans(source, &spans, Some(&code))
}

const CPP_PAREN_KEYWORDS: &[&str] = &[
    "if",
    "while",
    "for",
    "switch",
    "catch",
    "return",
    "sizeof",
    "alignof",
    "alignas",
    "decltype",
    "typeid",
    "noexcept",
    "throw",
    "static_assert",
    "new",
    "delete",
    "case",
    "requires",
    "co_await",
    "co_yield",
    "co_return",
    "asm",
    "__asm__",
    "__attribute__",
    "__attribute",
    "__declspec",
    "_Pragma",
    "else",
    "do",
    "constexpr",
    "consteval",
    "explicit",
    "typeof",
    "__typeof__",
    "__typeof",
    "_Alignof",
    "__alignof__",
    "volatile",
    "__volatile__",
];

const CPP_TYPE_KEYWORDS: &[&str] = &[
    "void", "bool", "char", "char8_t", "char16_t", "char32_t", "wchar_t", "short", "int", "long",
    "float", "double", "signed", "unsigned", "auto",
];

const CPP_OTHER_KEYWORDS: &[&str] = &[
    "const",
    "volatile",
    "static",
    "extern",
    "inline",
    "virtual",
    "explicit",
    "constexpr",
    "consteval",
    "constinit",
    "friend",
    "mutable",
    "thread_local",
    "register",
    "typedef",
    "using",
    "template",
    "typename",
    "class",
    "struct",
    "union",
    "enum",
    "namespace",
    "operator",
    "goto",
    "break",
    "continue",
    "default",
    "public",
    "private",
    "protected",
    "this",
    "true",
    "false",
    "nullptr",
    "and",
    "or",
    "not",
    "xor",
    "bitand",
    "bitor",
    "compl",
    "and_eq",
    "or_eq",
    "xor_eq",
    "not_eq",
];

fn is_cpp_keyword(w: &str) -> bool {
    CPP_TYPE_KEYWORDS.contains(&w)
        || CPP_PAREN_KEYWORDS.contains(&w)
        || CPP_OTHER_KEYWORDS.contains(&w)
}

re!(
    trailing_attr_candidate_re,
    r"\)((?:\s*(?:const|volatile|noexcept|override|final|&&|&)(?![\w]))*)\s*(?=[A-Z][A-Z0-9_]{2,}\b)"
);
re!(
    trailing_attr_run_re,
    r"^(?:[A-Z][A-Z0-9_]{2,}\b(?:\s*\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\))?\s*)+"
);
re!(
    trailing_attr_follow_re,
    r"^(?:\{|;|=(?!=)|:(?!:)|->|(?:override|final|const|noexcept)\b)"
);
re!(trailing_attr_hint_re, r"\)[\s\w&]*?\b[A-Z][A-Z0-9_]{2,}\b");
re!(blank_line_in_re, r"\n[ \t]*\r?\n");

/// Blank attribute macros after a function's parameter list
/// (`void Wait() LOCKS_EXCLUDED(mu_) {`).
pub(crate) fn blank_cpp_trailing_attribute_macros(source: &str) -> String {
    if !is_match(trailing_attr_hint_re(), source) {
        return source.to_string();
    }
    let code = mask_cpp_code(source);
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(c) = caps_at(trailing_attr_candidate_re(), &code, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(&code, m.start(), m.end());
        let macro_start = m.end();
        if is_match(blank_line_in_re(), m.as_str()) {
            continue;
        }
        let Some(r) = trailing_attr_run_re()
            .find(&code[macro_start..])
            .ok()
            .flatten()
        else {
            continue;
        };
        let run = r.as_str();
        if !is_match(trailing_attr_follow_re(), &code[macro_start + run.len()..]) {
            continue;
        }
        if !closes_parameter_list(&code, m.start()) {
            continue;
        }
        push_run_tokens(&mut spans, run, macro_start);
        pos = macro_start + run.len();
    }
    blank_spans(source, &spans, Some(&code))
}

re!(
    declarator_attr_candidate_re,
    r"(?<![\w$])[A-Z][A-Z0-9]*_[A-Z0-9_]*\b"
);
re!(
    declarator_attr_run_re,
    r"^(?:[A-Z][A-Z0-9]*_[A-Z0-9_]*\b(?:\s*\((?:[^()]|\((?:[^()]|\([^()]*\))*\))*\))?\s*)+"
);
re!(
    declarator_attr_hint_re,
    r"[\w\]]\s+[A-Z][A-Z0-9]*_[A-Z0-9_]*\s*[(;=,)]"
);
re!(declarator_attr_follow_re, r"^[;,)]|^=(?!=)");

/// Blank attribute macros after a declared name (`port::CondVar cv GUARDED_BY(mu);`).
pub(crate) fn blank_cpp_declarator_attribute_macros(source: &str) -> String {
    if !is_match(declarator_attr_hint_re(), source) {
        return source.to_string();
    }
    let code = mask_cpp_code(source);
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(c) = caps_at(declarator_attr_candidate_re(), &code, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(&code, m.start(), m.end());
        let macro_start = m.start();
        let mut last = macro_start as isize - 1;
        while last >= 0 && is_space(byte(&code, last as usize)) {
            last -= 1;
        }
        if last == macro_start as isize - 1 || last < 0 {
            continue;
        }
        let last = last as usize;
        let lb = byte(&code, last);
        if !(is_word(lb) || lb == b']') {
            continue;
        }
        let Some(r) = declarator_attr_run_re()
            .find(&code[macro_start..])
            .ok()
            .flatten()
        else {
            continue;
        };
        let run = r.as_str();
        let after_at = macro_start + run.len();
        let after = &code[after_at..floor(&code, after_at + 2)];
        if !is_match(declarator_attr_follow_re(), after) {
            continue;
        }
        if lb != b']' && !follows_declared_name(&code, last) {
            continue;
        }
        push_run_tokens(&mut spans, run, macro_start);
        pos = macro_start + run.len();
    }
    blank_spans(source, &spans, Some(&code))
}

fn is_ident(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && (b[0].is_ascii_alphabetic() || b[0] == b'_') && b.iter().all(|&c| is_word(c))
}

/// Whether the identifier ending at `code[last]` is a name a declaration declares.
fn follows_declared_name(code: &str, last: usize) -> bool {
    let mut start = last;
    while start > 0 && is_word(byte(code, start - 1)) {
        start -= 1;
    }
    let name = &code[start..=last];
    if !is_ident(name) || !name.bytes().any(|b| b.is_ascii_lowercase()) || is_cpp_keyword(name) {
        return false;
    }
    let mut i = start as isize - 1;
    while i >= 0 && is_space(byte(code, i as usize)) {
        i -= 1;
    }
    if i < 0 {
        return false;
    }
    let i = i as usize;
    let c = byte(code, i);
    if c == b'*' || c == b'&' {
        return true;
    }
    if c == b'>' {
        return i == 0 || byte(code, i - 1) != b'-';
    }
    if !is_word(c) {
        return false;
    }
    let mut j = i;
    while j > 0 && is_word(byte(code, j - 1)) {
        j -= 1;
    }
    let before = &code[j..=i];
    if before == "const" || before == "volatile" {
        let mut k = j as isize - 1;
        while k >= 0 && is_space(byte(code, k as usize)) {
            k -= 1;
        }
        let kb = if k >= 0 { byte(code, k as usize) } else { 0 };
        return kb == b'*' || kb == b'&';
    }
    if CPP_TYPE_KEYWORDS.contains(&before) {
        return true;
    }
    is_ident(before) && !is_cpp_keyword(before)
}

re!(
    operator_before_re,
    r"\boperator\s*(?:\(\s*\)|\[\s*\]|[^\s\w()\[\]]{1,3}|\s+[A-Za-z_][\w\s:<>*&]*)\s*$"
);
re!(name_before_re, r"([A-Za-z_]\w*)\s*$");

/// Whether the `)` at `code[close]` closes a function declarator's parameter list.
fn closes_parameter_list(code: &str, close: usize) -> bool {
    let b = code.as_bytes();
    let mut depth = 0i32;
    let mut open = None;
    let mut i = close as isize;
    while i >= 0 && close - (i as usize) < 4000 {
        match b[i as usize] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    open = Some(i as usize);
                    break;
                }
            }
            b';' | b'{' | b'}' => return false,
            _ => {}
        }
        i -= 1;
    }
    let Some(open) = open else {
        return false;
    };
    let before = &code[floor(code, open.saturating_sub(200))..open];
    if is_match(operator_before_re(), before) {
        return true;
    }
    match name_before_re().captures(before).ok().flatten() {
        Some(c) => {
            let name = c.get(1).unwrap().as_str();
            !CPP_PAREN_KEYWORDS.contains(&name) && !is_all_caps_macro(name)
        }
        None => false,
    }
}

/// `^[A-Z][A-Z0-9_]*$`
fn is_all_caps_macro(name: &str) -> bool {
    let b = name.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_uppercase()
        && b.iter()
            .all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
}

const CPP_TYPE_ARGS: &str = r"<(?:[^<>;{}]|<(?:[^<>;{}]|<[^<>;{}]*>)*>)*>";

fn leading_attr_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let type_name = format!(
            r"(?:::\s*)?(?!(?:const|volatile|operator|return)\b)[A-Za-z_]\w*(?:\s*{a})?(?:\s*::\s*[A-Za-z_]\w*(?:\s*{a})?)*",
            a = CPP_TYPE_ARGS
        );
        let declared = r"(?:(?:::\s*)?~?[A-Za-z_]\w*(?:\s*::\s*~?[A-Za-z_]\w*)*\s*[(;=\[{]|operator\b)";
        let pat = format!(
            concat!(
                r"(?m)(^[ \t]*|\]\][ \t]*)((?:[A-Z][A-Z0-9]*_[A-Z0-9_]*\s+)+)(?=",
                r"(?:static|inline|virtual|explicit|constexpr|consteval|constinit|friend|extern|thread_local|mutable|",
                r"typedef|using|template|typename|class|struct|union|enum|void|bool|char|char8_t|char16_t|char32_t|",
                r"wchar_t|short|int|long|float|double|signed|unsigned|auto|decltype|return)\b|\[\[|",
                r"(?:[A-Za-z_]\w*\s*::\s*)*([A-Za-z_]\w*)\s*::\s*~?\3\s*\(|",
                r"(?:(?:const|volatile)\s+)*{t}(?:\s*(?:const|volatile)\b)?(?:\s*[*&]+\s*|\s+)(?:(?:const|volatile)\s+)?{d})"
            ),
            t = type_name,
            d = declared
        );
        Regex::new(&pat).unwrap()
    })
}
re!(
    leading_attr_hint_re,
    r"(?m)(?:^|\]\])[ \t]*[A-Z][A-Z0-9]*_[A-Z0-9_]*\s"
);

/// Blank attribute macros that open a declaration
/// (`PROTOBUF_FUTURE_ADD_EARLY_NODISCARD absl::string_view name() const {`).
pub(crate) fn blank_cpp_leading_attribute_macros(source: &str) -> String {
    if !is_match(leading_attr_hint_re(), source) {
        return source.to_string();
    }
    let code = mask_cpp_code(source);
    let mut spans = Vec::new();
    let mut pos = 0;
    while let Some(c) = caps_at(leading_attr_re(), &code, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(&code, m.start(), m.end());
        let run = c.get(2).unwrap();
        push_run_tokens(&mut spans, run.as_str(), run.start());
    }
    blank_spans(source, &spans, Some(&code))
}

const CPP_INLINE_MACROS: &[&str] = &[
    "FORCEINLINE_DEBUGGABLE",
    "FORCENOINLINE",
    "FORCEINLINE",
    "PUGI__FN_NO_INLINE",
    "PUGI__FN",
    "PUGIXML_FUNCTION",
    "_ALWAYS_INLINE_",
    "_FORCE_INLINE_",
    "BOOST_FORCEINLINE",
    "BOOST_NOINLINE",
    "Q_INVOKABLE",
    "Q_SCRIPTABLE",
    "Q_ALWAYS_INLINE",
    "Q_SLOT",
    "Q_SIGNAL",
    "FOLLY_ALWAYS_INLINE",
    "FOLLY_NOINLINE",
    "ABSL_ATTRIBUTE_ALWAYS_INLINE",
    "ABSL_ATTRIBUTE_NOINLINE",
    "LLVM_ATTRIBUTE_ALWAYS_INLINE",
    "LLVM_ATTRIBUTE_NOINLINE",
    "V8_INLINE",
    "V8_NOINLINE",
    "EIGEN_STRONG_INLINE",
    "EIGEN_ALWAYS_INLINE",
    "EIGEN_DEVICE_FUNC",
    "RAPIDJSON_FORCEINLINE",
    "MOZ_ALWAYS_INLINE",
    "MOZ_NEVER_INLINE",
    "PROTOBUF_ALWAYS_INLINE",
    "PROTOBUF_NOINLINE",
    "FMT_CONSTEXPR20",
    "FMT_CONSTEXPR",
    "FMT_INLINE",
    "JSON_HEDLEY_ALWAYS_INLINE",
    "JSON_HEDLEY_NEVER_INLINE",
    "HEDLEY_ALWAYS_INLINE",
    "HEDLEY_NEVER_INLINE",
    "GLM_FUNC_QUALIFIER",
    "GLM_FUNC_DECL",
    "GLM_CONSTEXPR",
    "GLM_INLINE",
    "SIMD_FORCE_INLINE",
    "SK_ALWAYS_INLINE",
    "CV_ALWAYS_INLINE",
    "CV_INLINE",
    "EA_FORCE_INLINE",
    "EA_NOINLINE",
    "CC_INLINE",
    "NEVER_INLINE",
    "G_INLINE_FUNC",
    "SQLITE_PRIVATE",
    "SQLITE_API",
    "STDMETHODCALLTYPE",
    "WINAPIV",
    "WINAPI",
    "APIENTRY",
    "ALWAYS_INLINE",
    "FORCE_INLINE",
    "NOINLINE",
];

/// One alternation of `words`, longest first, as `\b(…)\b` + `tail`.
fn word_list_re(words: &[&str], tail: &str) -> Regex {
    let mut sorted: Vec<&str> = words.to_vec();
    // A stable sort, as upstream's: equal lengths keep their order.
    sorted.sort_by_key(|w| std::cmp::Reverse(w.len()));
    Regex::new(&format!(r"\b({})\b{}", sorted.join("|"), tail)).unwrap()
}

fn inline_macro_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| word_list_re(CPP_INLINE_MACROS, r"(?=\s+[A-Za-z_])"))
}

/// Blank a known inline-specifier macro before a return type (`FORCEINLINE FString GetName(…)`).
pub(crate) fn blank_cpp_inline_macros(source: &str) -> String {
    if !CPP_INLINE_MACROS.iter().any(|m| source.contains(m)) {
        return source.to_string();
    }
    replace_all(inline_macro_re(), source, |c| {
        " ".repeat(c.get(0).unwrap().as_str().len())
    })
}

re!(
    metal_attribute_re,
    r"\[\[\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?(?:\s*,\s*[A-Za-z_]\w*(?:\s*\([^()\n]*\))?)*\s*\]\]"
);

/// Blank Metal `[[attribute]]` annotations (`.metal` files only).
pub(crate) fn blank_metal_attributes(source: &str) -> String {
    if !source.contains("[[") {
        return source.to_string();
    }
    replace_all(metal_attribute_re(), source, |c| {
        " ".repeat(c.get(0).unwrap().as_str().len())
    })
}

/// The index just past the balanced `( … )` whose `(` is at `open`, string
/// and char literals skipped; None when it never closes.
fn balanced_paren_end(src: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut i = open;
    while i < src.len() {
        let c = src[i];
        if c == b'"' || c == b'\'' {
            i += 1;
            while i < src.len() && src[i] != c {
                if src[i] == b'\\' {
                    i += 1;
                }
                i += 1;
            }
            i += 1;
            continue;
        }
        if c == b'(' {
            depth += 1;
        } else if c == b')' {
            depth -= 1;
            if depth == 0 {
                return Some(i + 1);
            }
        }
        i += 1;
    }
    None
}

re!(
    annotation_call_hint_re,
    r"(?m)^[ \t]*[A-Z][A-Z0-9_]{2,}\s*\("
);
re!(
    annotation_call_re,
    r"(?m)^([ \t]*)([A-Z][A-Z0-9_]{2,})(\s*)\("
);

/// Blank declaration-markup macro calls with no `;` (`UPROPERTY(...)`,
/// `GENERATED_BODY()`); C keeps a macro that is the next function's return type.
pub(crate) fn blank_cpp_annotation_macro_calls(source: &str, keep_c_return_types: bool) -> String {
    if !is_match(annotation_call_hint_re(), source) {
        return source.to_string();
    }
    let raw = mask_cpp_raw_strings(source);
    let src = raw.masked.as_str();
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();
    let mut pos = 0;
    while let Some(c) = caps_at(annotation_call_re(), src, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(src, m.start(), m.end());
        let macro_start = m.start() + c.get(1).unwrap().as_str().len();
        let open = m.end() - 1;
        let Some(end) = balanced_paren_end(bytes, open) else {
            continue;
        };
        let j = skip_space(src, end);
        let after = byte(src, j);
        if after == 0 || !(after.is_ascii_alphabetic() || matches!(after, b'_' | b'~' | b'#')) {
            continue;
        }
        if keep_c_return_types
            && macro_start == m.start()
            && is_c_return_type_macro(src, m.start(), open, end, j)
        {
            continue;
        }
        blank_bytes(&mut out, macro_start, end);
        pos = end;
    }
    raw.restore(into_string(out))
}

re!(c_return_type_arg_re, r"^\s*[A-Za-z_][\w\s*]*$");
re!(c_declarator_head_re, r"^[A-Za-z_]\w*\s*\(");
re!(
    c_declarator_tail_re,
    r"^\s*(?:(?:__attribute__\s*\(\((?:[^()]|\([^()]*\))*\)\)|[A-Z][A-Z0-9_]*(?![\w$])(?:\s*\((?:[^()]|\([^()]*\))*\))?)\s*)*[{;]"
);

/// Whether the column-0 `MACRO(…)` is the return type of the function declared after it.
fn is_c_return_type_macro(
    src: &str,
    line_start: usize,
    open: usize,
    end: usize,
    next: usize,
) -> bool {
    if !is_match(c_return_type_arg_re(), &src[open + 1..end - 1]) {
        return false;
    }
    let Some(head) = c_declarator_head_re().find(&src[next..]).ok().flatten() else {
        return false;
    };
    let b = src.as_bytes();
    let mut close = None;
    let mut depth = 0i32;
    let mut i = next + head.end() - 1;
    while i < b.len() && close.is_none() {
        match b[i] {
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    close = Some(i);
                }
            }
            b';' | b'{' | b'}' => return false,
            _ => {}
        }
        i += 1;
    }
    let Some(close) = close else {
        return false;
    };
    is_match(c_declarator_tail_re(), &src[close + 1..]) && !opens_c_declaration(src, line_start)
}

re!(line_comment_tail_re, r"//.*$");

/// Whether the line right above `line_start` leaves a declaration open.
fn opens_c_declaration(src: &str, line_start: usize) -> bool {
    if line_start == 0 {
        return false;
    }
    let from = if line_start >= 2 {
        src[..line_start - 1].rfind('\n').map_or(0, |i| i + 1)
    } else {
        0
    };
    let raw = &src[from..line_start - 1];
    let line = replace_all(line_comment_tail_re(), raw, |_| String::new());
    let line = line.trim();
    !line.is_empty()
        && !line.starts_with('#')
        && line.bytes().last().is_some_and(|b| is_word(b) || b == b'*')
}

/// How a language's code reads around a macro alone on its line.
#[derive(Clone, Copy)]
pub(crate) struct LoneMacroRules {
    /// The characters the code after the macro may start with.
    next_code: fn(char) -> bool,
    /// Whether a macro that is a body's only statement stays.
    keeps_sole_statement: bool,
}

pub(crate) const C_LONE_MACROS: LoneMacroRules = LoneMacroRules {
    next_code: |c| c.is_ascii_alphabetic() || matches!(c, '_' | '#' | '{' | '}' | '~'),
    keeps_sole_statement: false,
};

pub(crate) const OBJC_LONE_MACROS: LoneMacroRules = LoneMacroRules {
    next_code: |c| c.is_ascii_alphabetic() || matches!(c, '_' | '#' | '{' | '}' | '~' | '@'),
    keeps_sole_statement: true,
};

re!(
    lone_macro_hint_re,
    r"(?m)^[ \t]*[A-Z][A-Z0-9_]{3,}[ \t]*\r?$"
);
re!(
    lone_macro_line_re,
    r"^[ \t]*([A-Z][A-Z0-9_]{3,})[ \t]*(?://[^\n\r]*|/\*[^\n\r]*\*/[ \t]*)?\r?$"
);
re!(lone_macro_continuation_end_re, r"[=+\-*/%&|^<>?:,(\\]$");
re!(comment_lead_re, r"^(?://|/\*|\*)");
re!(inline_comments_re, r"/\*.*?\*/|//.*$");
re!(template_head_re, r"^template\s*<");

/// Blank a macro that is the only token on its line (`FMT_BEGIN_NAMESPACE`, `Q_OBJECT`).
pub(crate) fn blank_lone_macro_lines(source: &str, rules: LoneMacroRules) -> String {
    if !is_match(lone_macro_hint_re(), source) {
        return source.to_string();
    }
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    let content = |l: &str| l.strip_suffix('\r').unwrap_or(l).trim().to_string();
    let mut changed = false;
    for i in 0..lines.len() {
        let line = lines[i].clone();
        let Some(m) = lone_macro_line_re().captures(&line).ok().flatten() else {
            continue;
        };
        let name = m.get(1).unwrap().as_str();
        if !name.contains('_') {
            continue;
        }
        let mut prev = i as isize - 1;
        while prev >= 0 && content(&lines[prev as usize]).is_empty() {
            prev -= 1;
        }
        let prev_text = if prev >= 0 {
            content(&lines[prev as usize])
        } else {
            String::new()
        };
        let prev_code = if is_match(comment_lead_re(), &prev_text) {
            String::new()
        } else {
            replace_all(inline_comments_re(), &prev_text, |_| String::new())
                .trim()
                .to_string()
        };
        let directive_ends = prev_code.starts_with('#') && !prev_code.ends_with('\\');
        if !prev_code.is_empty()
            && !directive_ends
            && !is_match(template_head_re(), &prev_code)
            && is_match(lone_macro_continuation_end_re(), &prev_code)
        {
            continue;
        }
        let first = first_code_char_after(&lines, i + 1);
        if let Some(f) = first {
            if !(rules.next_code)(f) {
                continue;
            }
        }
        if rules.keeps_sole_statement && prev_code.ends_with('{') && first == Some('}') {
            continue;
        }
        let start = line.find(name).unwrap();
        lines[i] = format!(
            "{}{}{}",
            &line[..start],
            " ".repeat(name.len()),
            &line[start + name.len()..]
        );
        changed = true;
    }
    if changed {
        lines.join("\n")
    } else {
        source.to_string()
    }
}

/// The first character of code at or after line `from`, past blank lines and comments.
fn first_code_char_after(lines: &[String], from: usize) -> Option<char> {
    let mut in_block = false;
    for line in lines.iter().skip(from) {
        let mut text: String = line.trim().to_string();
        loop {
            if in_block {
                match text.find("*/") {
                    None => {
                        text.clear();
                        break;
                    }
                    Some(end) => {
                        text = text[end + 2..].trim().to_string();
                        in_block = false;
                    }
                }
            }
            if text.starts_with("//") {
                text.clear();
            } else if text.starts_with("/*") {
                in_block = true;
                text = text[2..].to_string();
                continue;
            }
            break;
        }
        if let Some(c) = text.chars().next() {
            return Some(c);
        }
    }
    None
}

re!(api_prefix_hint_re, r"_(?:API|EXPORT|ABI)\b");
re!(
    api_prefix_re,
    r"\b[A-Z][A-Z0-9_]*(?:_API|_EXPORT|_ABI)\b(?=\s+[A-Za-z_])"
);

/// Blank an export macro before a member (`ENGINE_API virtual void Tick(…)`).
pub(crate) fn blank_cpp_api_prefix_macros(source: &str) -> String {
    if !is_match(api_prefix_hint_re(), source) {
        return source.to_string();
    }
    replace_all(api_prefix_re(), source, |c| {
        " ".repeat(c.get(0).unwrap().as_str().len())
    })
}

re!(
    inline_annotation_hint_re,
    r"\b(?:UMETA|UPARAM|UE_DEPRECATED)"
);
re!(
    inline_annotation_re,
    r"\b(?:UMETA|UPARAM|UE_DEPRECATED\w*)\s*\("
);

/// Blank Unreal annotation macros mid-line (`UMETA(…)`, `UPARAM(ref)`, `UE_DEPRECATED(…)`).
pub(crate) fn blank_cpp_inline_annotation_macros(source: &str) -> String {
    if !is_match(inline_annotation_hint_re(), source) {
        return source.to_string();
    }
    let raw = mask_cpp_raw_strings(source);
    let src = raw.masked.as_str();
    let bytes = src.as_bytes();
    let mut out = bytes.to_vec();
    let mut pos = 0;
    while let Some(c) = caps_at(inline_annotation_re(), src, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(src, m.start(), m.end());
        let Some(end) = balanced_paren_end(bytes, m.end() - 1) else {
            continue;
        };
        blank_bytes(&mut out, m.start(), end);
        pos = end;
    }
    raw.restore(into_string(out))
}

re!(cuda_launch_bounds_re, r"\b__launch_bounds__\s*\([^()\n]*\)");
re!(
    cuda_specifier_re,
    r"\b__(?:global|device|host|constant|shared|managed|grid_constant|forceinline|noinline|launch_bounds)__\b"
);
re!(cuda_launch_config_re, r"<<<[^;]{0,400}?>>>");

/// Blank CUDA specifiers, `__launch_bounds__(…)` and `<<<…>>>` launch configs.
pub(crate) fn blank_cuda_constructs(source: &str) -> String {
    let mut out = source.to_string();
    if out.contains("__") {
        out = replace_all(cuda_launch_bounds_re(), &out, |c| {
            " ".repeat(c.get(0).unwrap().as_str().len())
        });
        out = replace_all(cuda_specifier_re(), &out, |c| {
            " ".repeat(c.get(0).unwrap().as_str().len())
        });
    }
    if out.contains("<<<") {
        out = replace_all(cuda_launch_config_re(), &out, |c| {
            let m = c.get(0).unwrap().as_str();
            let mut depth = 0i32;
            for b in m.bytes() {
                if b == b'{' {
                    depth += 1;
                } else if b == b'}' {
                    depth -= 1;
                    if depth < 0 {
                        return m.to_string();
                    }
                }
            }
            if depth == 0 {
                m.bytes()
                    .map(|b| if b == b'\n' { '\n' } else { ' ' })
                    .collect()
            } else {
                m.to_string()
            }
        });
    }
    out
}

fn looks_like_cuda_source(source: &str) -> bool {
    source.contains("__global__")
        || source.contains("__device__")
        || source.contains("__constant__")
        || source.contains("cudaStream_t")
}

re!(directive_start_re, r"^[ \t]*#");
re!(trailing_backslash_re, r"\\\s*$");

/// Copy the original directive lines (and their `\` continuations) back.
fn restore_directive_lines(original: &str, blanked: String) -> String {
    if blanked == original || !original.contains('#') {
        return blanked;
    }
    let o: Vec<&str> = original.split('\n').collect();
    let mut b: Vec<String> = blanked.split('\n').map(str::to_string).collect();
    let mut changed = false;
    let mut continuation = false;
    for i in 0..o.len().min(b.len()) {
        let line = o[i];
        let is_directive = continuation || is_match(directive_start_re(), line);
        if is_directive && b[i] != line {
            b[i] = line.to_string();
            changed = true;
        }
        continuation = is_directive
            && is_match(
                trailing_backslash_re(),
                line.strip_suffix('\r').unwrap_or(line),
            );
    }
    if changed {
        b.join("\n")
    } else {
        blanked
    }
}

re!(
    com_alias_re,
    r"(?m)^[ \t]*#[ \t]*define[ \t]+interface[ \t]+struct\b"
);
re!(
    com_declaration_re,
    r"(?m)(^|[;{}])\s*\binterface\s+[A-Za-z_]\w*\s*(:[^;{}]+)?\{"
);
re!(virtual_word_re, r"\bvirtual\b");

/// Recover MSVC's COM `interface` alias at type-definition sites.
fn normalize_cpp_com_interfaces(source: &str) -> String {
    if !source.contains("interface") {
        return source.to_string();
    }
    let mut code = mask_cpp_comments_and_literals(source);
    let has_alias = is_match(com_alias_re(), &code);
    code = replace_all(directive_line_re(), &code, |c| {
        c.get(0)
            .unwrap()
            .as_str()
            .bytes()
            .map(|b| {
                if b == b'\n' || b == b'\r' {
                    b as char
                } else {
                    '\0'
                }
            })
            .collect()
    });
    let mut offsets = Vec::new();
    let mut pos = 0;
    while let Some(c) = caps_at(com_declaration_re(), &code, pos) {
        let m = c.get(0).unwrap();
        let last = m.end();
        let body_end = code[last..].find('}').map_or(code.len(), |i| last + i);
        let has_virtual = is_match(virtual_word_re(), &code[last..body_end]);
        if !has_alias && c.get(2).is_none() && !has_virtual {
            pos = next_pos(&code, m.start(), m.end());
            continue;
        }
        offsets.push(m.start() + m.as_str().find("interface").unwrap());
        // The opening brace may start a nested declaration.
        pos = last - 1;
    }
    let mut out = source.to_string();
    for offset in offsets {
        out.replace_range(offset..offset + 9, "struct   ");
    }
    out
}

re!(access_macro_hint_re, r"_[A-Z0-9_]*[ \t]*:(?!:)");
re!(
    access_macro_line_re,
    r"(?m)^([ \t]*)([A-Z][A-Z0-9]*_[A-Z0-9_]*)([ \t]*):(?!:)(?=[ \t]*(?://[^\n]*)?\r?$)"
);

/// Rewrite an access specifier spelled as a macro (`JSON_PRIVATE_UNLESS_TESTED:`) to its keyword.
pub(crate) fn rewrite_cpp_access_macros(source: &str) -> String {
    if !is_match(access_macro_hint_re(), source) {
        return source.to_string();
    }
    replace_all(access_macro_line_re(), source, |c| {
        let indent = c.get(1).unwrap().as_str();
        let name = c.get(2).unwrap().as_str();
        let gap = c.get(3).unwrap().as_str();
        let keyword = if name.contains("PRIVATE") {
            Some("private")
        } else if name.contains("PROTECTED") {
            Some("protected")
        } else if name.contains("PUBLIC") || name == "Q_SIGNALS" || name == "Q_SLOTS" {
            Some("public")
        } else {
            None
        };
        match keyword {
            Some(k) if k.len() <= name.len() + gap.len() => {
                format!(
                    "{indent}{k}{}:",
                    " ".repeat(name.len() + gap.len() - k.len())
                )
            }
            _ => c.get(0).unwrap().as_str().to_string(),
        }
    })
}

re!(directive_name_re, r"^[ \t]*#[ \t]*(\w+)");
re!(block_comment_inline_re, r"/\*.*?\*/");
re!(open_block_comment_re, r"/\*(?!.*\*/)");
re!(statement_end_re, r"[;{}]$|^[\w\s]*:$|\*/$");
re!(access_label_re, r"^(?:public|private|protected)\s*:$");
re!(statement_macro_re, r"^[A-Z][A-Z0-9_]*(?:\s*\(.*\))?$");
re!(
    conditional_directive_re,
    r"^(?:if|ifdef|ifndef|else|elif|elifdef|elifndef|endif)$"
);

/// Blank a preprocessor conditional that sits mid-declaration, keeping its first branch.
pub(crate) fn flatten_mid_statement_conditionals(source: &str) -> String {
    if !source.contains('#') {
        return source.to_string();
    }
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    let directive = |l: &str| -> Option<String> {
        directive_name_re()
            .captures(l)
            .ok()
            .flatten()
            .map(|c| c.get(1).unwrap().as_str().to_string())
    };
    let code_of = |l: &str| -> String {
        let no_line = replace_all(line_comment_tail_re(), l, |_| String::new());
        replace_all(block_comment_inline_re(), &no_line, |_| String::new())
            .trim()
            .to_string()
    };
    let mut changed = false;
    let mut last_code = String::new();
    let mut in_block_comment = false;
    let mut region = 0i32;
    let mut skip_from = 0i32;
    let mut continued = false;
    #[allow(clippy::needless_range_loop)]
    for i in 0..lines.len() {
        if continued {
            continued = is_match(trailing_backslash_re(), &lines[i]);
            continue;
        }
        let d = directive(&lines[i]);
        if let (Some(d), false) = (&d, in_block_comment) {
            if is_match(trailing_backslash_re(), &lines[i]) {
                continued = true;
                if region == 0 {
                    continue;
                }
            }
            let opens = d.starts_with("if");
            if region == 0 {
                let mid = opens
                    && !last_code.is_empty()
                    && !is_match(statement_end_re(), &last_code)
                    && !is_match(access_label_re(), &last_code)
                    && !is_match(statement_macro_re(), &last_code);
                if !mid {
                    continue;
                }
                region = 1;
                lines[i] = spaces_keeping_cr(&lines[i]);
                changed = true;
                continue;
            }
            if opens {
                region += 1;
            } else if (d == "else" || d == "elif") && skip_from == 0 {
                skip_from = region;
            } else if d == "endif" {
                if skip_from == region {
                    skip_from = 0;
                }
                region -= 1;
            }
            if is_match(conditional_directive_re(), d) {
                lines[i] = spaces_keeping_cr(&lines[i]);
                changed = true;
            }
            continue;
        }
        let text = lines[i].clone();
        if region > 0 && skip_from > 0 {
            if !text.trim().is_empty() {
                lines[i] = spaces_keeping_cr(&lines[i]);
                changed = true;
            }
            continue;
        }
        let trimmed = code_of(&text);
        if in_block_comment {
            if text.contains("*/") {
                in_block_comment = false;
            }
            continue;
        }
        let no_line = replace_all(line_comment_tail_re(), &text, |_| String::new());
        if is_match(open_block_comment_re(), &no_line) {
            in_block_comment = true;
            let before = trimmed.split("/*").next().unwrap_or("").trim().to_string();
            if !before.is_empty() {
                last_code = before;
            }
            continue;
        }
        if !trimmed.is_empty() {
            last_code = trimmed;
        }
    }
    if changed {
        lines.join("\n")
    } else {
        source.to_string()
    }
}

re!(
    foundation_enum_hint_re,
    r"\b(?:NS|CF)_[A-Z_]*(?:ENUM|OPTIONS)\s*\("
);
re!(
    foundation_enum_re,
    r"\b(?:(typedef)(\s+))?(NS_ENUM|NS_OPTIONS|NS_CLOSED_ENUM|NS_ERROR_ENUM|CF_ENUM|CF_OPTIONS|CF_CLOSED_ENUM)(\s*)\(([^(),]*),(\s*)([A-Za-z_]\w*)(\s*)\)"
);

/// Rewrite a Foundation enum macro (`typedef NS_ENUM(NSInteger, T) {`) as the enum it declares.
pub(crate) fn rewrite_foundation_enum_macros(source: &str) -> String {
    if !is_match(foundation_enum_hint_re(), source) {
        return source.to_string();
    }
    let code = mask_cpp_code(source);
    let mut out: Option<Vec<u8>> = None;
    let mut pos = 0;
    while let Some(c) = caps_at(foundation_enum_re(), &code, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(&code, m.start(), m.end());
        let buf = out.get_or_insert_with(|| source.as_bytes().to_vec());
        let len = |k: usize| c.get(k).map_or(0, |g| g.as_str().len());
        let mut at = m.start();
        if c.get(1).is_some() {
            blank_bytes(buf, at, at + len(1));
            at += len(1) + len(2);
        }
        let macro_len = len(3);
        buf[at..at + 4].copy_from_slice(b"enum");
        blank_bytes(buf, at + 4, at + macro_len);
        at += macro_len + len(4);
        blank_bytes(buf, at, at + 1 + len(5) + 1);
        at += 1 + len(5) + 1 + len(6) + len(7) + len(8);
        blank_bytes(buf, at, at + 1);
    }
    match out {
        Some(buf) => into_string(buf),
        None => source.to_string(),
    }
}

/// C++ source pre-processing before tree-sitter.
pub(crate) fn pre_parse_cpp_source(source: &str, file_path: &str) -> String {
    let raw = mask_cpp_raw_strings(source);
    let source = flatten_mid_statement_conditionals(&rewrite_cpp_access_macros(
        &rewrite_foundation_enum_macros(&raw.masked),
    ));
    let mut blanked = normalize_cpp_com_interfaces(&source);
    blanked = blank_cpp_export_macros(&blanked);
    blanked = blank_cpp_inline_macros(&blanked);
    blanked = blank_cpp_api_prefix_macros(&blanked);
    blanked = blank_cpp_inline_annotation_macros(&blanked);
    blanked = blank_cpp_pointer_annotation_macros(&blanked);
    blanked = blank_cpp_trailing_attribute_macros(&blanked);
    blanked = blank_cpp_declarator_attribute_macros(&blanked);
    blanked = blank_cpp_annotation_macro_calls(&blanked, false);
    blanked = blank_c_leading_attr_macros(&blanked);
    blanked = blank_cpp_leading_attribute_macros(&blanked);
    blanked = blank_lone_macro_lines(&blanked, C_LONE_MACROS);
    let lower = file_path.to_ascii_lowercase();
    if lower.ends_with(".metal") {
        blanked = blank_metal_attributes(&blanked);
    } else if lower.ends_with(".cu") || lower.ends_with(".cuh") || looks_like_cuda_source(&source) {
        blanked = blank_cuda_constructs(&blanked);
    }
    raw.restore(restore_directive_lines(&source, blanked))
}

// ---------------------------------------------------------------------------
// C passes
// ---------------------------------------------------------------------------

re!(
    c_leading_attr_macro_re,
    r"(?m)^([ \t]*)([A-Z][A-Z0-9_]{2,})(?=\s+[A-Za-z_]\w*[\s*]+[A-Za-z_]\w*\s*\()"
);

/// Blank an attribute macro before a C function's return type (`SEC_ATTR UINT32 f(VOID)`).
pub(crate) fn blank_c_leading_attr_macros(source: &str) -> String {
    replace_all(c_leading_attr_macro_re(), source, |c| {
        format!(
            "{}{}",
            c.get(1).unwrap().as_str(),
            " ".repeat(c.get(2).unwrap().as_str().len())
        )
    })
}

re!(
    c_cplusplus_guard_open_re,
    r"^[ \t]*#[ \t]*(?:ifdef[ \t]+__cplusplus\b|if[ \t]+defined[ \t]*\(?[ \t]*__cplusplus[ \t]*\)?)"
);
re!(c_preproc_endif_re, r"^[ \t]*#[ \t]*endif\b");
const C_CPLUSPLUS_GUARD_MAX_BODY_LINES: usize = 40;

fn strip_cr(l: &str) -> &str {
    l.strip_suffix('\r').unwrap_or(l)
}

/// Blank the body of `#ifdef __cplusplus … #endif` guards in C sources.
pub(crate) fn blank_c_cplusplus_guard_bodies(source: &str) -> String {
    if !source.contains("__cplusplus") {
        return source.to_string();
    }
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        if !is_match(c_cplusplus_guard_open_re(), strip_cr(&lines[i])) {
            i += 1;
            continue;
        }
        let mut end = None;
        let mut j = i + 1;
        while j < lines.len() && j - i - 1 <= C_CPLUSPLUS_GUARD_MAX_BODY_LINES {
            let line = strip_cr(&lines[j]);
            if is_match(c_preproc_endif_re(), line) {
                end = Some(j);
                break;
            }
            if is_match(directive_start_re(), line) {
                break;
            }
            j += 1;
        }
        let Some(end) = end else {
            i += 1;
            continue;
        };
        for line in lines.iter_mut().take(end).skip(i + 1) {
            *line = spaces_keeping_cr(line);
        }
        changed = true;
        i = end + 1;
    }
    if changed {
        lines.join("\n")
    } else {
        source.to_string()
    }
}

const C_STMT_MACRO_KEYWORDS: &[&str] = &[
    "if", "while", "for", "switch", "return", "do", "else", "sizeof",
];
re!(
    c_stmt_macro_head_re,
    r"^[ \t]+([a-z_][a-z0-9_]*|[A-Z_][A-Z0-9_]*)[ \t]*\("
);

/// Skip a quoted literal starting at `k` (on `quote`); the index of its closer.
fn skip_quoted(b: &[u8], mut k: usize) -> usize {
    let quote = b[k];
    k += 1;
    while k < b.len() && b[k] != quote {
        if b[k] == b'\\' {
            k += 1;
        }
        k += 1;
    }
    k
}

/// Blank a C iterator-macro call in statement position (`list_for_each_entry(pos, head, m) {`).
pub(crate) fn blank_c_statement_macro_calls(source: &str) -> String {
    let raw = mask_cpp_raw_strings(source);
    let src = raw.masked.clone();
    let mut lines: Vec<String> = src.split('\n').map(str::to_string).collect();
    let mut changed = false;
    let content = |l: &str| strip_cr(l).trim().to_string();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i].clone();
        let Some(m) = c_stmt_macro_head_re().captures(&line).ok().flatten() else {
            i += 1;
            continue;
        };
        let ident = m.get(1).unwrap().as_str().to_string();
        if C_STMT_MACRO_KEYWORDS.contains(&ident.as_str()) {
            i += 1;
            continue;
        }
        let head_len = m.get(0).unwrap().end();
        let open = line[head_len - 1..]
            .find('(')
            .map(|k| k + head_len - 1)
            .unwrap();
        let lb = line.as_bytes();
        let mut depth = 0i32;
        let mut close: Option<usize> = None;
        let mut k = open;
        while k < lb.len() {
            let ch = lb[k];
            if ch == b'"' || ch == b'\'' {
                k = skip_quoted(lb, k) + 1;
                continue;
            }
            if ch == b'(' {
                depth += 1;
            } else if ch == b')' {
                depth -= 1;
                if depth == 0 {
                    close = Some(k);
                    break;
                }
            }
            k += 1;
        }
        let mut end_line = i;
        if close.is_none() {
            if line.contains(';') {
                i += 1;
                continue;
            }
            let mut j = i + 1;
            while j <= i + 5 && j < lines.len() && close.is_none() {
                let cont = lines[j].as_bytes();
                let mut bail = false;
                let mut k = 0;
                while k < cont.len() {
                    let ch = cont[k];
                    if ch == b'"' || ch == b'\'' {
                        k = skip_quoted(cont, k) + 1;
                        continue;
                    }
                    if ch == b';' || ch == b'{' || ch == b'}' {
                        bail = true;
                        break;
                    }
                    if ch == b'(' {
                        depth += 1;
                    } else if ch == b')' {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(k);
                            end_line = j;
                            break;
                        }
                    }
                    k += 1;
                }
                if bail {
                    break;
                }
                j += 1;
            }
            if close.is_none() {
                i += 1;
                continue;
            }
        }
        let close = close.unwrap();
        let end_str = lines[end_line].clone();
        let after = strip_cr(&end_str[(close + 1).min(end_str.len())..])
            .trim()
            .to_string();
        if after.is_empty() {
            let mut next = end_line + 1;
            while next < lines.len() && content(&lines[next]).is_empty() {
                next += 1;
            }
            if next >= lines.len() {
                i += 1;
                continue;
            }
            let first = content(&lines[next]).chars().next();
            if !first.is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '{') {
                i += 1;
                continue;
            }
        } else if after != "{" {
            i += 1;
            continue;
        }
        let ident_start = line.find(&ident).unwrap();
        if end_line == i {
            lines[i] = format!(
                "{}{}{}",
                &line[..ident_start],
                " ".repeat(close + 1 - ident_start),
                &line[close + 1..]
            );
        } else {
            lines[i] = format!(
                "{}{}",
                &line[..ident_start],
                spaces_keeping_cr(&line[ident_start..])
            );
            for l in lines.iter_mut().take(end_line).skip(i + 1) {
                *l = spaces_keeping_cr(l);
            }
            lines[end_line] = format!(
                "{}{}",
                spaces_keeping_cr(&end_str[..close + 1]),
                &end_str[close + 1..]
            );
            i = end_line;
        }
        changed = true;
        i += 1;
    }
    raw.restore(if changed { lines.join("\n") } else { src })
}

re!(
    c_sandwich_re,
    r"\b(static|extern|inline)([ \t]+)(noinline_for_stack|nokprobe_inline|noinline|notrace|noinstr)\b(?=[ \t]+[A-Za-z_])"
);

/// Blank a compiler-annotation word between a storage class and a declaration (`static notrace void f`).
pub(crate) fn blank_c_sandwiched_annotations(source: &str) -> String {
    if !is_match(c_sandwich_re(), source) {
        return source.to_string();
    }
    replace_all(c_sandwich_re(), source, |c| {
        format!(
            "{}{}{}",
            c.get(1).unwrap().as_str(),
            c.get(2).unwrap().as_str(),
            " ".repeat(c.get(3).unwrap().as_str().len())
        )
    })
}

re!(c_auto_infer_re, r"\bauto(?=[ \t]+[A-Za-z_]\w*[ \t]*=)");

/// Blank a C23 `auto` type-inference keyword (`auto hb = hbr.hb;`).
pub(crate) fn blank_c_auto_inference(source: &str) -> String {
    if !is_match(c_auto_infer_re(), source) {
        return source.to_string();
    }
    replace_all(c_auto_infer_re(), source, |_| "    ".to_string())
}

re!(
    c_trailing_param_attr_re,
    r"\b([A-Za-z_]\w*)([ \t]+)([A-Z][A-Z0-9_]{2,})(?=[ \t]*[,)])"
);

/// Blank a trailing parameter-attribute macro (`int argc UNUSED,`).
pub(crate) fn blank_c_trailing_param_attr_macros(source: &str) -> String {
    if !is_match(c_trailing_param_attr_re(), source) {
        return source.to_string();
    }
    replace_all(c_trailing_param_attr_re(), source, |c| {
        format!(
            "{}{}{}",
            c.get(1).unwrap().as_str(),
            c.get(2).unwrap().as_str(),
            " ".repeat(c.get(3).unwrap().as_str().len())
        )
    })
}

const C_KERNEL_ANNOTATIONS: &[&str] = &[
    "__init",
    "__exit",
    "__initdata",
    "__initconst",
    "__exitdata",
    "__devinit",
    "__devexit",
    "__cpuinit",
    "__meminit",
    "__meminitdata",
    "__net_init",
    "__net_exit",
    "__net_initdata",
    "__init_or_module",
    "__user",
    "__kernel",
    "__iomem",
    "__percpu",
    "__rcu",
    "__force",
    "__nocast",
    "__must_check",
    "__maybe_unused",
    "__always_unused",
    "__used",
    "__cold",
    "__hot",
    "__weak",
    "__pure",
    "__sched",
    "__malloc",
    "__visible",
    "__deprecated",
    "__ro_after_init",
    "__read_mostly",
    "__refdata",
    "__latent_entropy",
    "__randomize_layout",
    "__no_randomize_layout",
    "__bpf_kfunc",
    "__function_aligned",
    "__always_inline",
    "__noreturn",
    "__cacheline_aligned_in_smp",
    "__cacheline_aligned",
    "__cacheline_internodealigned_in_smp",
    "____cacheline_aligned_in_smp",
    "____cacheline_aligned",
    "____cacheline_internodealigned_in_smp",
    "__noclone",
    "__lockfunc",
    "__ref",
    "__private",
    "__bitwise",
    "__nosavedata",
    "__no_kcsan",
    "__cpuidle",
    "__ksym",
    "__initdata_memblock",
    "__initdata_or_meminfo",
];

fn c_kernel_annotation_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| word_list_re(C_KERNEL_ANNOTATIONS, r"(?!\s*\()"))
}
re!(
    container_of_re,
    r"(\bcontainer_of\s*\([^;()]*?,\s*)(struct|union)(\s+)"
);

/// Blank the Linux-kernel/sparse declaration annotations (`static int __init f(void)`).
pub(crate) fn blank_c_kernel_annotations(source: &str) -> String {
    if !source.contains("__") || !is_match(c_kernel_annotation_re(), source) {
        return source.to_string();
    }
    let mut out = replace_all(c_kernel_annotation_re(), source, |c| {
        " ".repeat(c.get(0).unwrap().as_str().len())
    });
    if out.contains("container_of") {
        out = replace_all(container_of_re(), &out, |c| {
            format!(
                "{}{}{}",
                c.get(1).unwrap().as_str(),
                " ".repeat(c.get(2).unwrap().as_str().len()),
                c.get(3).unwrap().as_str()
            )
        });
    }
    out
}

const C_PARAMETERIZED_ANNOTATIONS: &[&str] = &[
    "__free",
    "__printf",
    "__scanf",
    "__counted_by",
    "__counted_by_le",
    "__counted_by_be",
    "__guarded_by",
    "__pt_guarded_by",
    "__acquires",
    "__releases",
    "__must_hold",
    "__cleanup",
    "__aligned",
    "__section",
    "__bpf_md_ptr",
    "__assume_aligned",
];

fn c_param_annotation_re() -> &'static Regex {
    static R: OnceLock<Regex> = OnceLock::new();
    R.get_or_init(|| {
        let mut sorted: Vec<&str> = C_PARAMETERIZED_ANNOTATIONS.to_vec();
        sorted.sort_by_key(|w| std::cmp::Reverse(w.len()));
        Regex::new(&format!(
            r"\b({})[ \t]*\((?:[^()]|\([^()]*\))*\)",
            sorted.join("|")
        ))
        .unwrap()
    })
}
re!(blank_to_line_start_re, r"^[ \t]*$");
re!(semicolon_after_re, r"^[ \t]*;");

/// Blank parameterized sparse/compiler annotations whole (`__free(fput)`, `__printf(4, 0)`).
pub(crate) fn blank_c_parameterized_annotation_macros(source: &str) -> String {
    if !source.contains("__") {
        return source.to_string();
    }
    let raw = mask_cpp_raw_strings(source);
    let src = raw.masked.as_str();
    let mut result = String::with_capacity(src.len());
    let mut last = 0;
    let mut pos = 0;
    while let Some(c) = caps_at(c_param_annotation_re(), src, pos) {
        let m = c.get(0).unwrap();
        let start = m.start();
        let mut end = m.end();
        let line_start = if start == 0 {
            0
        } else {
            src[..start].rfind('\n').map_or(0, |i| i + 1)
        };
        if is_match(blank_to_line_start_re(), &src[line_start..start]) {
            if let Some(a) = semicolon_after_re().find(&src[end..]).ok().flatten() {
                end += a.end();
            }
        }
        result.push_str(&src[last..start]);
        result.push_str(&spaces_keeping_lines(&src[start..end]));
        last = end;
        pos = next_pos(src, start, end);
    }
    result.push_str(&src[last..]);
    raw.restore(result)
}

const C_TYPE_ARG_HEAD_EXCLUSIONS: &[&str] = &[
    "sizeof",
    "alignof",
    "_Alignof",
    "typeof",
    "__typeof__",
    "__typeof",
    "offsetof",
    "_Generic",
    "va_arg",
    "if",
    "while",
    "for",
    "switch",
    "case",
];
re!(c_type_arg_hint_re, r"\b(?:struct|union|enum)[ \t\r\n]");
re!(c_type_arg_head_re, r"\b([A-Za-z_]\w*)[ \t]*\(");
re!(
    c_type_arg_opener_re,
    r"^(struct|union|enum)([ \t\r\n]+)([A-Za-z_]\w*)([ \t\r\n]*\*+)?(?=[ \t\r\n]*[,)])"
);
const C_TYPE_ARG_SCAN_CAP: usize = 600;

/// Blank a bare `struct T` type-keyword argument in a macro call (`kzalloc_obj(struct x)`).
pub(crate) fn blank_c_type_keyword_args(source: &str) -> String {
    if !is_match(c_type_arg_hint_re(), source) {
        return source.to_string();
    }
    let raw = mask_cpp_raw_strings(source);
    let src = raw.masked.as_str();
    let b = src.as_bytes();
    let mut chars: Option<Vec<u8>> = None;
    let mut pos = 0;
    while let Some(c) = caps_at(c_type_arg_head_re(), src, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(src, m.start(), m.end());
        let head = c.get(1).unwrap().as_str();
        if C_TYPE_ARG_HEAD_EXCLUSIONS.contains(&head) {
            continue;
        }
        let mut j = m.start() as isize - 1;
        while j >= 0 && (b[j as usize] == b' ' || b[j as usize] == b'\t') {
            j -= 1;
        }
        let prev = if j >= 0 { b[j as usize] } else { 0 };
        if is_word(prev) || prev == b'*' {
            let mut w = j;
            while w >= 0 && is_word(b[w as usize]) {
                w -= 1;
            }
            if &src[(w + 1) as usize..(j + 1) as usize] != "return" {
                continue;
            }
        }
        let open = m.end() - 1;
        let mut depth = 1i32;
        let mut at_arg_start = true;
        let mut k = open + 1;
        while k < b.len() && k - open <= C_TYPE_ARG_SCAN_CAP {
            let ch = b[k];
            if ch == b'"' || ch == b'\'' {
                k = skip_quoted(b, k) + 1;
                at_arg_start = false;
                continue;
            }
            if matches!(ch, b' ' | b'\t' | b'\n' | b'\r') {
                k += 1;
                continue;
            }
            if ch == b'(' {
                depth += 1;
                at_arg_start = false;
                k += 1;
                continue;
            }
            if ch == b')' {
                depth -= 1;
                if depth == 0 {
                    break;
                }
                at_arg_start = false;
                k += 1;
                continue;
            }
            if ch == b';' || ch == b'{' || ch == b'}' {
                break;
            }
            if ch == b',' {
                if depth == 1 {
                    at_arg_start = true;
                }
                k += 1;
                continue;
            }
            if depth == 1 && at_arg_start {
                let window = &src[k..floor(src, k + 160)];
                if let Some(o) = c_type_arg_opener_re().captures(window).ok().flatten() {
                    let buf = chars.get_or_insert_with(|| b.to_vec());
                    let kw = o.get(1).unwrap().as_str().len();
                    for x in buf.iter_mut().skip(k).take(kw) {
                        *x = b' ';
                    }
                    if let Some(tail) = o.get(4) {
                        let tail = tail.as_str();
                        let stars = tail.len() - tail.trim_end_matches('*').len();
                        let stars_start = k
                            + kw
                            + o.get(2).unwrap().as_str().len()
                            + o.get(3).unwrap().as_str().len()
                            + (tail.len() - stars);
                        for x in buf.iter_mut().skip(stars_start).take(stars) {
                            *x = b' ';
                        }
                    }
                    k += o.get(0).unwrap().as_str().len() - 1;
                }
            }
            at_arg_start = false;
            k += 1;
        }
    }
    raw.restore(match chars {
        Some(buf) => into_string(buf),
        None => src.to_string(),
    })
}

re!(
    c_prefixed_decl_macro_re,
    r"^[ \t]*(?:static|extern)[ \t]+[A-Z][A-Z0-9_]{2,}[ \t]*\("
);
re!(
    c_prefixed_decl_hint_re,
    r"(?m)^[ \t]*(?:static|extern)[ \t]+[A-Z]"
);

/// Blank a `static`/`extern`-prefixed declaration macro line (`static DEFINE_PER_CPU(…);`).
pub(crate) fn blank_c_file_scope_prefixed_decl_macros(source: &str) -> String {
    if !is_match(c_prefixed_decl_hint_re(), source) {
        return source.to_string();
    }
    let raw = mask_cpp_raw_strings(source);
    let src = raw.masked.clone();
    let mut lines: Vec<String> = src.split('\n').map(str::to_string).collect();
    let mut changed = false;
    for line in lines.iter_mut() {
        let Some(m) = c_prefixed_decl_macro_re().find(line).ok().flatten() else {
            continue;
        };
        let open = line[m.end() - 1..].find('(').unwrap() + m.end() - 1;
        let lb = line.as_bytes();
        let mut depth = 0i32;
        let mut close = None;
        let mut k = open;
        while k < lb.len() {
            let ch = lb[k];
            if ch == b'"' || ch == b'\'' {
                k = skip_quoted(lb, k) + 1;
                continue;
            }
            if ch == b'(' {
                depth += 1;
            } else if ch == b')' {
                depth -= 1;
                if depth == 0 {
                    close = Some(k);
                    break;
                }
            }
            k += 1;
        }
        let Some(close) = close else {
            continue;
        };
        if strip_cr(&line[close + 1..]).trim() != ";" {
            continue;
        }
        *line = spaces_keeping_lines(line);
        changed = true;
    }
    raw.restore(if changed { lines.join("\n") } else { src })
}

re!(
    c_prefixed_decl_init_re,
    r"^([ \t]*)static[ \t]+[A-Z][A-Z0-9_]{2,}[ \t]*\(([^();'\x22]*?),[ \t]*([A-Za-z_]\w*)[ \t]*\)([ \t]*=[ \t]*\{?[ \t]*\r?)$"
);
re!(c_static_caps_hint_re, r"(?m)^[ \t]*static[ \t]+[A-Z]");
re!(ws_run_re, r"[ \t]+");
re!(type_token_run_re, r"^[A-Za-z_][\w \t*]*$");

/// Rewrite `static MACRO(type, name) = {` into the declaration it expands to.
pub(crate) fn rewrite_c_prefixed_decl_macro_initializers(source: &str) -> String {
    if !is_match(c_static_caps_hint_re(), source) {
        return source.to_string();
    }
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    let mut changed = false;
    for line in lines.iter_mut() {
        let Some(m) = c_prefixed_decl_init_re().captures(line).ok().flatten() else {
            continue;
        };
        let arg1 = replace_all(ws_run_re(), m.get(2).unwrap().as_str().trim(), |_| {
            " ".to_string()
        });
        if !is_match(type_token_run_re(), &arg1) {
            continue;
        }
        let name = m.get(3).unwrap();
        let name_start = name.start();
        let tail_start = m.get(4).unwrap().start();
        let prefix = format!("{}static {}", m.get(1).unwrap().as_str(), arg1);
        if prefix.len() + 1 > name_start {
            continue;
        }
        let rewritten = format!(
            "{}{}{}{}{}",
            prefix,
            " ".repeat(name_start - prefix.len()),
            name.as_str(),
            " ".repeat(tail_start - name_start - name.as_str().len()),
            &line[tail_start..]
        );
        *line = rewritten;
        changed = true;
    }
    if changed {
        lines.join("\n")
    } else {
        source.to_string()
    }
}

re!(c_va_arg_re, r"\bva_arg[ \t]*\(([^(),]+)(,[^()]*)\)");
re!(single_token_rest_re, r"^,[ \t]*[A-Za-z_]\w*[ \t]*$");

/// Blank a qualified/pointer type argument of `va_arg` (`va_arg(ap, const char *)`).
pub(crate) fn blank_c_va_arg_qualified_type_args(source: &str) -> String {
    if !source.contains("va_arg") {
        return source.to_string();
    }
    replace_all(c_va_arg_re(), source, |c| {
        let m = c.get(0).unwrap().as_str();
        let arg1 = c.get(1).unwrap().as_str();
        let rest = c.get(2).unwrap().as_str();
        if is_match(single_token_rest_re(), rest) {
            return m.to_string();
        }
        format!("va_arg({}{})", arg1, spaces_keeping_lines(rest))
    })
}

re!(
    c_named_variadic_define_re,
    r"^([ \t]*#[ \t]*define[ \t]+[A-Za-z_]\w*\([^()\n]*?\b\w+)\.\.\."
);

/// Blank the dots of a GNU named-variadic `#define` parameter (`args...`).
pub(crate) fn blank_c_named_variadic_define_dots(source: &str) -> String {
    if !source.contains("...") {
        return source.to_string();
    }
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    let mut changed = false;
    for line in lines.iter_mut() {
        let Some(m) = c_named_variadic_define_re().captures(line).ok().flatten() else {
            continue;
        };
        let keep = m.get(1).unwrap().as_str().len();
        *line = format!("{}   {}", &line[..keep], &line[keep + 3..]);
        changed = true;
    }
    if changed {
        lines.join("\n")
    } else {
        source.to_string()
    }
}

re!(c_designated_head_re, r"(?m)^[ \t]*([A-Z_][A-Z0-9_]*)\s*\(");
re!(
    c_designator_re,
    r"(^|[,{(\s])(\.[A-Za-z_]\w*|\[[^\]]+\])\s*=[^=]"
);
re!(semicolon_next_re, r"^\s*;");

/// Blank the argument list of a statement-level `MACRO(…);` holding designated initializers.
pub(crate) fn blank_c_designated_macro_args(source: &str) -> String {
    if !source.contains('=') {
        return source.to_string();
    }
    let raw = mask_cpp_raw_strings(source);
    let src = raw.masked.as_str();
    let b = src.as_bytes();
    let mut out = b.to_vec();
    let mut pos = 0;
    while let Some(c) = caps_at(c_designated_head_re(), src, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(src, m.start(), m.end());
        let open = m.end() - 1;
        let mut depth = 1i32;
        let mut i = open + 1;
        while i < b.len() && depth > 0 {
            if b[i] == b'(' {
                depth += 1;
            } else if b[i] == b')' {
                depth -= 1;
            }
            i += 1;
        }
        if depth != 0 {
            continue;
        }
        let close = i - 1;
        if !is_match(c_designator_re(), &src[open + 1..close]) {
            continue;
        }
        if !is_match(semicolon_next_re(), &src[close + 1..]) {
            continue;
        }
        for x in out.iter_mut().take(close).skip(open + 1) {
            if *x != b'\n' {
                *x = b' ';
            }
        }
        pos = close;
    }
    raw.restore(into_string(out))
}

re!(
    c_cond_open_re,
    r"^[ \t]*#[ \t]*(if|ifdef|ifndef)\b[ \t]*(.*)$"
);
re!(c_cond_next_re, r"^[ \t]*#[ \t]*(elif|else)\b");
re!(c_cond_else_re, r"^[ \t]*#[ \t]*else\b");
re!(starts_zero_re, r"^0\b");
re!(starts_one_re, r"^1\b");
re!(starts_else_re, r"^else\b");
re!(expr_fragment_start_re, r"^(\|\||&&|,|\)|:|\?)");
re!(expr_fragment_end_re, r"(\|\||&&|,|\(|\?|:)$");
re!(if_header_re, r"^(else[ \t]+)?if[ \t]*\(");

struct CondGroup {
    marks: Vec<usize>,
    dead: bool,
    literal: bool,
    positive: bool,
    else_at: isize,
}

/// Open up a `#if … #endif` group whose branches are not self-contained statements.
pub(crate) fn blank_c_unbalanced_conditional_branches(source: &str) -> String {
    if !source.contains('#') {
        return source.to_string();
    }
    let mut lines: Vec<String> = source.split('\n').map(str::to_string).collect();
    let mut masked: Vec<String> = mask_cpp_comments_and_literals(source)
        .split('\n')
        .map(str::to_string)
        .collect();
    let mut stack: Vec<CondGroup> = Vec::new();
    let mut directive = vec![false; lines.len()];
    let mut continued = vec![false; lines.len()];
    let mut changed = false;
    let mut continuation = false;
    for i in 0..lines.len() {
        let line = strip_cr(&masked[i]).to_string();
        let is_directive = continuation || is_match(directive_start_re(), &line);
        directive[i] = is_directive;
        continued[i] = continuation;
        let was_continuation = continuation;
        continuation = is_directive && is_match(trailing_backslash_re(), strip_cr(&lines[i]));
        if !is_directive || was_continuation {
            continue;
        }
        if let Some(m) = c_cond_open_re().captures(&line).ok().flatten() {
            let kind = m.get(1).unwrap().as_str();
            let cond = m.get(2).unwrap().as_str().trim();
            let dead = kind == "if" && is_match(starts_zero_re(), cond);
            let live = kind == "if" && is_match(starts_one_re(), cond);
            let positive =
                kind == "ifdef" || (kind == "if" && !dead && !live && !cond.starts_with('!'));
            stack.push(CondGroup {
                marks: vec![i],
                dead,
                literal: dead || live,
                positive,
                else_at: -1,
            });
            continue;
        }
        let Some(top) = stack.last_mut() else {
            continue;
        };
        if is_match(c_cond_next_re(), &line) {
            if is_match(c_cond_else_re(), &line) {
                top.else_at = top.marks.len() as isize;
            }
            top.marks.push(i);
            continue;
        }
        if !is_match(c_preproc_endif_re(), &line) {
            continue;
        }
        let mut top = stack.pop().unwrap();
        top.marks.push(i);
        let keep = if top.dead {
            1
        } else if top.positive && top.else_at > 0 {
            top.else_at as usize
        } else {
            0
        };
        if keep >= top.marks.len() - 1 {
            continue;
        }
        let mut damaged = false;
        let mut balanced = true;
        for b in 0..top.marks.len() - 1 {
            let mut body: Vec<String> = Vec::new();
            for k in top.marks[b] + 1..top.marks[b + 1] {
                if directive[k] {
                    continue;
                }
                let c = strip_cr(&masked[k]).trim().to_string();
                if !c.is_empty() {
                    body.push(c);
                }
            }
            if body.is_empty() {
                continue;
            }
            let first = body[0].clone();
            let last = body[body.len() - 1].clone();
            let mut depth = 0i32;
            let mut parens = 0i32;
            let mut dips = false;
            for l in &body {
                for ch in l.bytes() {
                    depth += match ch {
                        b'{' => 1,
                        b'}' => -1,
                        _ => 0,
                    };
                    parens += match ch {
                        b'(' => 1,
                        b')' => -1,
                        _ => 0,
                    };
                    if depth < 0 {
                        dips = true;
                    }
                }
            }
            if depth != 0 || parens != 0 {
                balanced = false;
            }
            damaged = damaged
                || depth != 0
                || dips
                || is_match(starts_else_re(), &first)
                || is_match(expr_fragment_start_re(), &first)
                || is_match(expr_fragment_end_re(), &last)
                || (is_match(if_header_re(), &last) && last.ends_with(')'));
        }
        if !damaged {
            continue;
        }
        let keep_all = balanced && !top.literal;
        let blank_line = |lines: &mut Vec<String>, masked: &mut Vec<String>, k: usize| {
            lines[k] = spaces_keeping_cr(&lines[k]);
            masked[k] = spaces_keeping_cr(&masked[k]);
        };
        for b in 0..top.marks.len() - 1 {
            let from = top.marks[b];
            let to = top.marks[b + 1];
            let mut k = from;
            while k < to && (k == from || continued[k]) {
                blank_line(&mut lines, &mut masked, k);
                k += 1;
            }
            if b == keep || keep_all {
                continue;
            }
            for k in from + 1..to {
                blank_line(&mut lines, &mut masked, k);
            }
        }
        blank_line(&mut lines, &mut masked, i);
        changed = true;
    }
    if changed {
        lines.join("\n")
    } else {
        source.to_string()
    }
}

/// C source pre-processing before tree-sitter.
pub(crate) fn pre_parse_c_source(source: &str) -> String {
    let raw = mask_cpp_raw_strings(source);
    let source = rewrite_foundation_enum_macros(&raw.masked);
    let inner = blank_c_designated_macro_args(&blank_c_kernel_annotations(
        &blank_c_cplusplus_guard_bodies(&source),
    ));
    let mut blanked = blank_c_sandwiched_annotations(&inner);
    blanked = blank_c_auto_inference(&blanked);
    blanked = blank_c_parameterized_annotation_macros(&blanked);
    blanked = blank_c_type_keyword_args(&blanked);
    blanked = blank_c_va_arg_qualified_type_args(&blanked);
    blanked = blank_c_file_scope_prefixed_decl_macros(&blanked);
    blanked = rewrite_c_prefixed_decl_macro_initializers(&blanked);
    blanked = blank_cpp_annotation_macro_calls(&blanked, true);
    blanked = blank_c_trailing_param_attr_macros(&blanked);
    blanked = blank_c_statement_macro_calls(&blanked);
    blanked = blank_lone_macro_lines(&blanked, C_LONE_MACROS);
    blanked = blank_c_leading_attr_macros(&blanked);
    if looks_like_cuda_source(&blanked) {
        blanked = blank_cuda_constructs(&blanked);
    }
    raw.restore(blank_c_unbalanced_conditional_branches(
        &blank_c_named_variadic_define_dots(&restore_directive_lines(&source, blanked)),
    ))
}

/// The bytes the C/C++ walker parses for `source` in `language`.
pub(crate) fn pre_parse(language: &str, file_path: &str, source: &str) -> String {
    match language {
        "cpp" => pre_parse_cpp_source(source, file_path),
        "c" => pre_parse_c_source(source),
        _ => source.to_string(),
    }
}

/// One pass by its upstream name, for the parity runner: `option` is
/// `blankLoneMacroLines`' Objective-C rules and `blankCppAnnotationMacroCalls`'
/// `keepCReturnTypes`. None for a name with no pass.
pub fn pass_by_name(name: &str, source: &str, file_path: &str, option: bool) -> Option<String> {
    Some(match name {
        "cppPreParse" => pre_parse_cpp_source(source, file_path),
        "cPreParse" => pre_parse_c_source(source),
        "blankLoneMacroLines" => blank_lone_macro_lines(
            source,
            if option {
                OBJC_LONE_MACROS
            } else {
                C_LONE_MACROS
            },
        ),
        "flattenMidStatementConditionals" => flatten_mid_statement_conditionals(source),
        "blankCppAnnotationMacroCalls" => blank_cpp_annotation_macro_calls(source, option),
        "rewriteFoundationEnumMacros" => rewrite_foundation_enum_macros(source),
        "rewriteCppAccessMacros" => rewrite_cpp_access_macros(source),
        "blankCppExportMacros" => blank_cpp_export_macros(source),
        "blankCppInlineMacros" => blank_cpp_inline_macros(source),
        "blankMetalAttributes" => blank_metal_attributes(source),
        "blankCudaConstructs" => blank_cuda_constructs(source),
        "blankCppApiPrefixMacros" => blank_cpp_api_prefix_macros(source),
        "blankCppInlineAnnotationMacros" => blank_cpp_inline_annotation_macros(source),
        "blankCppPointerAnnotationMacros" => blank_cpp_pointer_annotation_macros(source),
        "blankCppTrailingAttributeMacros" => blank_cpp_trailing_attribute_macros(source),
        "blankCppDeclaratorAttributeMacros" => blank_cpp_declarator_attribute_macros(source),
        "blankCppLeadingAttributeMacros" => blank_cpp_leading_attribute_macros(source),
        "blankCLeadingAttrMacros" => blank_c_leading_attr_macros(source),
        "blankCCplusplusGuardBodies" => blank_c_cplusplus_guard_bodies(source),
        "blankCStatementMacroCalls" => blank_c_statement_macro_calls(source),
        "blankCSandwichedAnnotations" => blank_c_sandwiched_annotations(source),
        "blankCAutoInference" => blank_c_auto_inference(source),
        "blankCTrailingParamAttrMacros" => blank_c_trailing_param_attr_macros(source),
        "blankCKernelAnnotations" => blank_c_kernel_annotations(source),
        "blankCParameterizedAnnotationMacros" => blank_c_parameterized_annotation_macros(source),
        "blankCTypeKeywordArgs" => blank_c_type_keyword_args(source),
        "blankCFileScopePrefixedDeclMacros" => blank_c_file_scope_prefixed_decl_macros(source),
        "rewriteCPrefixedDeclMacroInitializers" => {
            rewrite_c_prefixed_decl_macro_initializers(source)
        }
        "blankCVaArgQualifiedTypeArgs" => blank_c_va_arg_qualified_type_args(source),
        "blankCNamedVariadicDefineDots" => blank_c_named_variadic_define_dots(source),
        "blankCDesignatedMacroArgs" => blank_c_designated_macro_args(source),
        "blankCUnbalancedConditionalBranches" => blank_c_unbalanced_conditional_branches(source),
        "stripCppTemplateArgs" => super::strip_cpp_template_args(source),
        "recoverMangledCppName" => super::recover_mangled_cpp_name(source.to_string()),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lone_namespace_macro_blanks() {
        let src = "#define NS namespace a {\nFMT_BEGIN_NAMESPACE\nvoid f() {}\nFMT_END_NAMESPACE\n";
        let out = pre_parse_cpp_source(src, "a.cc");
        assert_eq!(out.len(), src.len());
        assert!(out.contains("\n                   \nvoid f() {}\n"));
    }

    #[test]
    fn export_macro_in_class_head_blanks() {
        let src = "class PROTOBUF_EXPORT Any final : public Message {};";
        assert_eq!(
            blank_cpp_export_macros(src),
            "class                 Any final : public Message {};"
        );
    }

    #[test]
    fn raw_strings_survive() {
        let src = "auto s = R\"x(FOO_BAR\n)x\";\nUPROPERTY(a)\nint b;\n";
        let out = pre_parse_cpp_source(src, "a.cc");
        assert!(out.starts_with("auto s = R\"x(FOO_BAR\n)x\";"));
        assert!(out.contains("            \nint b;"));
    }

    #[test]
    fn c_return_type_macro_stays() {
        let src = "CJSON_PUBLIC(char *) cJSON_Print(const cJSON *item);\nREDIS_NO_SANITIZE(\"bounds\")\nint f(void) {}\n";
        let out = pre_parse_c_source(src);
        assert!(out.starts_with("CJSON_PUBLIC(char *) cJSON_Print"));
        assert!(out.contains("\n                           \nint f"));
    }
}
