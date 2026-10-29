//! Reading C++ declarations off source lines: declarators, listed
//! declarators, range-based fors, the types they write (name-matcher.ts,
//! from `CPP_NON_TYPE_TOKENS` to `cppWrittenClass`).

use super::*;

/// C++ keywords/control-flow tokens that can appear right before a receiver
/// and must NOT be treated as a type.
pub const CPP_NON_TYPE_TOKENS: &[&str] = &[
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "goto",
    "throw",
    "new",
    "delete",
    "co_await",
    "co_yield",
    "co_return",
    "static_cast",
    "const_cast",
    "dynamic_cast",
    "reinterpret_cast",
    "sizeof",
    "alignof",
    "typeid",
    "and",
    "or",
    "not",
    "xor",
];

/// Does a `>` in a declared C++ type close no `<`?
pub fn cpp_type_began_above(type_name: &str) -> bool {
    let mut depth = 0i32;
    for c in type_name.bytes() {
        if c == b'<' {
            depth += 1;
        } else if c == b'>' {
            depth -= 1;
            if depth < 0 {
                return true;
            }
        }
    }
    false
}

/// Does `line` end a declaration of the receiver begun on an earlier line?
pub fn cpp_ends_declaration_begun_above(ctx: &Ctx, line: &str, escaped_receiver: &str) -> bool {
    let re = ctx.re(&format!(
        r"(?:^|[^\w:])([A-Za-z_][\w:]*\s*<[^;=(){{}}]+>)(?:\s*(?:const\b|volatile\b|[*&]+))*\s*{escaped_receiver}\b"
    ));
    js::caps(&re, line).is_some_and(|m| cpp_type_began_above(js::group(&m, 1)))
}

/// The type a declaration of the receiver on `lines[index]` declares when its
/// type is written on lines above, read with them.
pub fn cpp_declaration_begun_above(
    ctx: &Ctx,
    lines: &[String],
    index: usize,
    line: &str,
    receiver_pattern: &Regex,
    declarator_regex: &Regex,
    raw_lines: &NonCode,
) -> Option<String> {
    let at = js::search(receiver_pattern, line)?;
    if index == 0 {
        return None;
    }
    let code_at = |j: usize| -> String {
        cpp_code_of(
            ctx,
            &raw_strings::literals_blanked(&lines[j], j + 1, raw_lines),
        )
        .unwrap_or_default()
    };
    let head = &line[..at];
    if js::test(&ctx.re(r"^\s*[*&]*\s*$"), head) {
        let above = code_at(index - 1);
        return if js::test(&CPP_TYPE_LINE.with(ctx), &above) {
            match_cpp_declarator(ctx, &format!("{above} {line}"), declarator_regex).map(|m| m.ty)
        } else {
            None
        };
    }
    if !cpp_type_began_above(head) {
        return None;
    }
    let mut text = line.to_string();
    let mut before = head.to_string();
    let lo = index.saturating_sub(6);
    let mut j = index;
    while j > lo {
        j -= 1;
        let above = code_at(j);
        text = format!("{above} {text}");
        before = format!("{above} {before}");
        if !cpp_type_began_above(&before) {
            return match_cpp_declarator(ctx, &text, declarator_regex).map(|m| m.ty);
        }
    }
    None
}

/// The last name of a declared C++ type: `const std::vector<std::pair<int, Foo>>&` → `vector`.
pub fn normalize_cpp_type_name(ctx: &Ctx, type_name: &str) -> Option<String> {
    if cpp_type_began_above(type_name) {
        return None;
    }
    let kw = ctx.re(r"\b(const|volatile|mutable|typename|class|struct)\b");
    let ptr = ctx.re(r"[&*]+");
    let normalized = aliases::strip_template_arguments(&js::replace_with(
        &ptr,
        &js::replace_with(&kw, type_name, " "),
        " ",
    ));
    let normalized = js::collapse_ws(&normalized).trim().to_string();
    if normalized.is_empty() {
        return None;
    }
    let last = normalized.split("::").filter(|p| !p.is_empty()).last()?;
    if CPP_NON_TYPE_TOKENS.contains(&last) {
        return None;
    }
    Some(last.to_string())
}

/// A pattern composed once from parts, compiled per context.
pub struct Pattern(pub LazyLock<String>);

impl Pattern {
    pub fn with(&self, ctx: &Ctx) -> Rc<Regex> {
        ctx.re(&self.0)
    }
}

pub const CPP_DECLARATOR_CV: &str = r"\b(?:const|volatile|__restrict__|__restrict)\b";
pub const CPP_DECLARATOR_ANNOTATION: &str = r"\b(?:absl_nonnull|absl_nullable|absl_nullability_unknown|_Nonnull|_Nullable|_Null_unspecified|[A-Z][A-Z0-9]*_[A-Z0-9_]*)\b";
pub const CPP_FUNCTION_WRAPPER_TYPE: &str = r"(?:::\s*)?(?:std\s*::\s*(?:function|move_only_function|copyable_function)|absl\s*::\s*(?:FunctionRef|AnyInvocable))\s*<[^;={}]*>";
pub const CPP_TEMPLATE_ARGUMENTS: &str = r"<(?:[^<>;=(){}]|<(?:[^<>;=(){}]|<[^<>;=(){}]*>)*>)*>";
pub static CPP_DECLARATOR_TYPE: LazyLock<String> = LazyLock::new(|| {
    format!(
        r"(?:{w}|(?:(?<![\w>):])::\s*)?[A-Za-z_][\w:]*(?:\s*{t}(?:\s*::\s*[A-Za-z_][\w:]*)?)?)(?:\s*(?:{cv}|[*&]+(?:\s*{a})*))*",
        w = CPP_FUNCTION_WRAPPER_TYPE,
        t = CPP_TEMPLATE_ARGUMENTS,
        cv = CPP_DECLARATOR_CV,
        a = CPP_DECLARATOR_ANNOTATION
    )
});
pub static CPP_TYPE_LINE: Pattern = Pattern(LazyLock::new(|| {
    format!(
        r"^\s*(?:(?:const|volatile|static|mutable|constexpr|inline)\s+)*{}\s*$",
        *CPP_DECLARATOR_TYPE
    )
}));
pub const CPP_TRAILING_ANNOTATION: &str = r"\b[A-Z][A-Z0-9]*_[A-Z0-9_]*\s*\([^()]*\)";

/// `Type receiver`, `Type* receiver`, … requiring a declarator terminator after the name.
pub fn build_declarator_regex(ctx: &Ctx, escaped_receiver: &str) -> Rc<Regex> {
    ctx.re(&format!(
        r"({})\s*\b{escaped_receiver}\b(?:\s*{CPP_TRAILING_ANNOTATION})*\s*(?=[;=,)\[{{(]|$)",
        *CPP_DECLARATOR_TYPE
    ))
}

/// A declarator match: where it starts and ends in the line, and its type as
/// cppDeclaratorType gives it.
#[derive(Clone, Debug)]
pub struct DeclMatch {
    pub index: usize,
    pub end: usize,
    pub ty: String,
}

pub fn match_cpp_declarator(ctx: &Ctx, line: &str, regex: &Regex) -> Option<DeclMatch> {
    match_cpp_declarator_from(ctx, line, regex, 0)
}

pub fn match_cpp_declarator_from(
    ctx: &Ctx,
    line: &str,
    regex: &Regex,
    from: usize,
) -> Option<DeclMatch> {
    let m = js::caps_at(regex, line, from)?;
    let whole = m.get(0).unwrap();
    Some(DeclMatch {
        index: whole.start(),
        end: whole.end(),
        ty: cpp_declarator_type(ctx, js::group(&m, 1)),
    })
}

/// A type the declarator regex read, without the annotations it reads past after a `*` or `&`.
pub fn cpp_declarator_type(ctx: &Ctx, text: &str) -> String {
    let annotation = ctx.re(&format!("^{CPP_DECLARATOR_ANNOTATION}$"));
    let mut out = String::with_capacity(text.len());
    let mut depth = 0i32;
    let mut after_pointer = false;
    let mut dropped = false;
    let b = text.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let prev = if i > 0 { b[i - 1] } else { b' ' };
        let word = if depth == 0 && !(js::is_word(prev) || prev == b':') {
            js::word_at(text, i)
        } else {
            None
        };
        if let Some(w) = word {
            if after_pointer && js::test(&annotation, w) {
                dropped = true;
            } else {
                out.push_str(w);
                after_pointer = false;
            }
            i += w.len();
            continue;
        }
        let c = b[i];
        if c == b'<' {
            depth += 1;
        } else if c == b'>' {
            depth -= 1;
        } else if depth == 0 && (c == b'*' || c == b'&') {
            after_pointer = true;
        } else if depth == 0 && !js::is_space(c) {
            after_pointer = false;
        }
        let ch = text[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    if dropped {
        js::collapse_ws(&out).trim().to_string()
    } else {
        text.to_string()
    }
}

/// The receiver after a comma, as a name later in a declaration's list is.
pub fn build_listed_declarator_regex(ctx: &Ctx, escaped_receiver: &str) -> Rc<Regex> {
    ctx.re(&format!(
        r",(?:\s|[*&]|{CPP_DECLARATOR_CV}|{CPP_DECLARATOR_ANNOTATION})*\b{escaped_receiver}\b"
    ))
}

pub const CPP_DECL_SPECIFIERS: &[&str] = &[
    "static",
    "extern",
    "inline",
    "constexpr",
    "constinit",
    "thread_local",
    "register",
    "mutable",
    "const",
    "volatile",
    "typename",
    "struct",
    "class",
    "union",
    "enum",
];

const CPP_NOT_A_DECLARED_TYPE_EXTRA: &[&str] = &[
    "typedef",
    "using",
    "namespace",
    "template",
    "friend",
    "operator",
    "public",
    "private",
    "protected",
    "virtual",
    "explicit",
    "static_assert",
    "asm",
    "try",
    "catch",
    "this",
    "true",
    "false",
    "nullptr",
];

pub fn is_not_a_declared_type(w: &str) -> bool {
    CPP_NON_TYPE_TOKENS.contains(&w) || CPP_NOT_A_DECLARED_TYPE_EXTRA.contains(&w)
}

const BUILTIN_TYPE_WORDS: &[&str] = &[
    "auto", "void", "bool", "char", "wchar_t", "char8_t", "char16_t", "char32_t", "short", "int",
    "long", "signed", "unsigned", "float", "double",
];

pub fn is_not_a_declared_name(w: &str) -> bool {
    is_not_a_declared_type(w) || CPP_DECL_SPECIFIERS.contains(&w) || BUILTIN_TYPE_WORDS.contains(&w)
}

/// The word that ends where the spaces before `code[i]` begin.
pub fn cpp_word_before(code: &str, i: usize) -> &str {
    let b = code.as_bytes();
    let mut end = i.min(b.len());
    while end > 0 && js::is_space(b[end - 1]) {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && js::is_word(b[start - 1]) {
        start -= 1;
    }
    &code[start..end]
}

/// Does a string or character literal open at `code[i]`?
pub fn cpp_opens_literal(code: &str, i: usize) -> bool {
    let c = js::at(code, i);
    c == b'"' || (c == b'\'' && !js::before(code, i).is_ascii_digit())
}

/// Past the bracket that closes the `(`, `[` or `{` at `code[i]`: the line's
/// length when the line ends first, None when a bracket closes out of turn.
pub fn cpp_past_brackets(code: &str, mut i: usize) -> Option<usize> {
    let b = code.as_bytes();
    let mut closers: Vec<u8> = Vec::new();
    while i < b.len() {
        let c = b[i];
        match c {
            b'(' => closers.push(b')'),
            b'[' => closers.push(b']'),
            b'{' => closers.push(b'}'),
            b')' | b']' | b'}' => {
                if closers.pop() != Some(c) {
                    return None;
                }
                if closers.is_empty() {
                    return Some(i + 1);
                }
            }
            _ => {
                if cpp_opens_literal(code, i) {
                    i = cpp_literal_end(code, i);
                }
            }
        }
        i += 1;
    }
    Some(b.len())
}

/// Where the initializer starting at `code[i]` ends; None when its brackets don't match.
pub fn cpp_past_initializer(code: &str, mut i: usize) -> Option<usize> {
    let b = code.as_bytes();
    while i < b.len() {
        let c = b[i];
        if matches!(c, b',' | b';' | b')' | b']' | b'}') {
            return Some(i);
        }
        if matches!(c, b'(' | b'[' | b'{') {
            i = cpp_past_brackets(code, i)?;
        } else if cpp_opens_literal(code, i) {
            i = cpp_literal_end(code, i) + 1;
        } else {
            i += 1;
        }
    }
    Some(i)
}

/// Past the template argument list opening at `code[i]`, or None when it doesn't close on the line.
pub fn cpp_past_template_arguments(code: &str, mut i: usize) -> Option<usize> {
    let b = code.as_bytes();
    let mut angles = 0i32;
    let mut parens = 0i32;
    while i < b.len() {
        let c = b[i];
        if c == b'(' {
            parens += 1;
        } else if c == b')' {
            parens -= 1;
            if parens < 0 {
                return None;
            }
        } else if parens > 0 {
        } else if c == b'<' {
            angles += 1;
        } else if c == b'>' {
            angles -= 1;
            if angles == 0 {
                return Some(i + 1);
            }
        } else if matches!(c, b';' | b'{' | b'}') {
            return None;
        }
        i += 1;
    }
    None
}

/// Past the type a C++ declaration opens with at `code[i]`, or None.
pub fn cpp_past_declared_type(code: &str, mut i: usize) -> Option<usize> {
    if js::starts_at(code, i, "::") {
        i = js::skip_space(code, i + 2);
    }
    let mut segment = 0;
    loop {
        let mut word = js::word_at(code, i);
        if word == Some("template") && segment > 0 {
            i = js::skip_space(code, i + 8);
            word = js::word_at(code, i);
        }
        let w = word?;
        if is_not_a_declared_type(w) {
            return None;
        }
        i += w.len();
        let mut next = js::skip_space(code, i);
        if js::at(code, next) == b'<' {
            i = cpp_past_template_arguments(code, next)?;
            next = js::skip_space(code, i);
        }
        if !js::starts_at(code, next, "::") {
            return Some(i);
        }
        i = js::skip_space(code, next + 2);
        segment += 1;
    }
}

/// A name a C++ declaration lists after its first.
#[derive(Clone, Debug)]
pub struct ListedName {
    pub name: String,
    pub ty: String,
    pub type_at: usize,
    pub end: usize,
}

/// The names after the first that the C++ declaration starting at `code[start]` lists.
pub fn cpp_declarators_after_first(
    ctx: &Ctx,
    code: &str,
    start: usize,
    in_header: bool,
) -> Option<Vec<ListedName>> {
    let cv_word = ctx.re(&format!("^{CPP_DECLARATOR_CV}$"));
    let annotation_word = ctx.re(&format!("^{CPP_DECLARATOR_ANNOTATION}$"));
    let trailing = ctx.re(&format!("^{CPP_TRAILING_ANNOTATION}"));
    let b = code.as_bytes();
    let mut i = js::skip_space(code, start);
    while let Some(w) = js::word_at(code, i) {
        if !CPP_DECL_SPECIFIERS.contains(&w) {
            break;
        }
        i = js::skip_space(code, i + w.len());
    }
    let type_at = i;
    i = cpp_past_declared_type(code, i)?;
    let ty = code[type_at..i].to_string();
    let mut listed = Vec::new();
    let mut first = true;
    loop {
        let mut pointer = String::new();
        i = js::skip_space(code, i);
        loop {
            let c = js::at(code, i);
            if c == b'*' || c == b'&' {
                pointer.push(c as char);
                i += 1;
                i = js::skip_space(code, i);
                continue;
            }
            let Some(word) = js::word_at(code, i) else {
                break;
            };
            let after = js::skip_space(code, i + word.len());
            let is_annotation = !pointer.is_empty()
                && js::test(&annotation_word, word)
                && (matches!(js::at(code, after), b'*' | b'&')
                    || js::word_at(code, after).is_some());
            if !js::test(&cv_word, word) && !is_annotation {
                break;
            }
            i += word.len();
            i = js::skip_space(code, i);
        }
        if i >= b.len() && !first && pointer.is_empty() {
            return Some(listed);
        }
        let name = js::word_at(code, i)?;
        if is_not_a_declared_name(name) {
            return None;
        }
        let name = name.to_string();
        i = js::skip_space(code, i + name.len());
        if js::starts_at(code, i, "::") {
            return None;
        }
        while let Some(m) = trailing.find(&code[i..]).ok().flatten() {
            i = js::skip_space(code, i + m.end());
        }
        if !first {
            listed.push(ListedName {
                name,
                ty: format!("{ty}{pointer}"),
                type_at,
                end: i,
            });
        }
        while js::at(code, i) == b'[' {
            i = cpp_past_brackets(code, i)?;
            i = js::skip_space(code, i);
        }
        let c = js::at(code, i);
        if c == b'(' || c == b'{' {
            i = cpp_past_brackets(code, i)?;
        } else if c == b'=' && js::at(code, i + 1) != b'=' {
            i = cpp_past_initializer(code, i + 1)?;
        }
        i = js::skip_space(code, i);
        let c = js::at(code, i);
        if i >= b.len() || c == b';' || (in_header && c == b')') {
            return Some(listed);
        }
        if c != b',' {
            return None;
        }
        i += 1;
        first = false;
    }
}

/// The declaration of `receiver` on `line` when the name follows another in the declaration's list.
pub fn match_cpp_listed_declarator(
    ctx: &Ctx,
    line: &str,
    receiver: &str,
    listed_regex: &Regex,
) -> Option<DeclMatch> {
    if js::test(listed_regex, line) {
        cpp_listed_declarator_on(ctx, line, receiver)
    } else {
        None
    }
}

pub fn cpp_listed_declarator_on(ctx: &Ctx, line: &str, receiver: &str) -> Option<DeclMatch> {
    let b = line.as_bytes();
    let mut i: isize = -1;
    while i < b.len() as isize {
        let header = i >= 0
            && b[i as usize] == b'('
            && matches!(cpp_word_before(line, i as usize), "for" | "if" | "switch");
        if i >= 0 && !matches!(b[i as usize], b';' | b'{' | b'}') && !header {
            i += 1;
            continue;
        }
        let start = (i + 1) as usize;
        if let Some(named) = cpp_declarators_after_first(ctx, line, start, header)
            .and_then(|list| list.into_iter().find(|d| d.name == receiver))
        {
            return Some(DeclMatch {
                index: named.type_at,
                end: named.end,
                ty: named.ty,
            });
        }
        i += 1;
    }
    None
}

static CPP_RANGE_FOR_DECLARATION: &str = r#"\bfor\s*\(([^;=(){}?."']*?)\b([A-Za-z_]\w*)\s*:(?!:)"#;
pub static CPP_DECLARED_TYPE_TAIL: Pattern =
    Pattern(LazyLock::new(|| format!(r"({})\s*$", *CPP_DECLARATOR_TYPE)));

/// A range-based for's declaration of `receiver` on `line`.
#[derive(Clone, Debug)]
pub struct RangeFor {
    pub ty: String,
    pub paren: usize,
    pub colon: usize,
}

pub fn cpp_range_for_declaration(ctx: &Ctx, line: &str, receiver: &str) -> Option<RangeFor> {
    if !line.contains("for") {
        return None;
    }
    for m in js::all(&ctx.re(CPP_RANGE_FOR_DECLARATION), line) {
        if js::group(&m, 2) != receiver {
            continue;
        }
        let whole = m.get(0).unwrap();
        let tail = js::caps(&CPP_DECLARED_TYPE_TAIL.with(ctx), js::group(&m, 1))?;
        return Some(RangeFor {
            ty: cpp_declarator_type(ctx, js::group(&tail, 1)),
            paren: whole.start() + whole.as_str().find('(').unwrap_or(0),
            colon: whole.end(),
        });
    }
    None
}

/// The range a range-based for iterates, as written, from the declaration's `:`.
pub fn cpp_range_expression(
    ctx: &Ctx,
    lines: &[String],
    line: usize,
    column: usize,
) -> Option<String> {
    let block = ctx.re(r"/\*.*?\*/");
    let tail = ctx.re(r"//.*$");
    let mut depth = 1i32;
    let mut text = String::new();
    for (l, raw) in lines.iter().enumerate().take(line + 4).skip(line) {
        let code = js::replace_with(&tail, &js::replace_with(&block, raw, " "), "");
        let from = if l == line { column } else { 0 };
        for ch in js::slice_from(&code, from).chars() {
            if ch == '"' || ch == '\'' {
                return None;
            }
            if ch == '(' {
                depth += 1;
            } else if ch == ')' {
                depth -= 1;
                if depth == 0 {
                    let t = text.trim();
                    return (!t.is_empty()).then(|| t.to_string());
                }
            }
            text.push(ch);
        }
        text.push(' ');
    }
    None
}

/// A C++ type as written, with where it is written.
#[derive(Clone, Debug)]
pub struct WrittenType {
    pub text: String,
    pub scope: String,
    pub file: String,
    pub line: u32,
    pub parameters: Rc<HashSet<String>>,
    /// The array dimensions a declarator gives a variable of `text`.
    pub array: Option<u32>,
    /// The fixture class a gtest body is looked up from.
    pub class_scope: Option<String>,
}

impl WrittenType {
    pub fn with_text(&self, text: impl Into<String>) -> WrittenType {
        WrittenType {
            text: text.into(),
            ..self.clone()
        }
    }

    pub fn site(&self) -> supertypes::NameSite {
        supertypes::NameSite {
            scope: self.scope.clone(),
            file: self.file.clone(),
            line: self.line,
            parameters: self.parameters.clone(),
            self_id: None,
            class_scope: self.class_scope.clone(),
        }
    }
}

pub const CPP_LIBRARY_SEQUENCES: &[&str] = &[
    "vector",
    "list",
    "forward_list",
    "deque",
    "set",
    "multiset",
    "unordered_set",
    "unordered_multiset",
    "array",
    "span",
    "initializer_list",
    "Span",
    "InlinedVector",
    "FixedArray",
    "flat_hash_set",
    "node_hash_set",
    "btree_set",
    "btree_multiset",
];
pub const CPP_PROJECT_SEQUENCES: &[&str] = &["RepeatedField", "RepeatedPtrField", "autovector"];
pub const CPP_LIBRARY_MAPS: &[&str] = &[
    "map",
    "multimap",
    "unordered_map",
    "unordered_multimap",
    "flat_hash_map",
    "node_hash_map",
    "btree_map",
    "btree_multimap",
];

/// A written type without its nullability annotations.
pub fn cpp_without_nullability(ctx: &Ctx, text: &str) -> String {
    let re = ctx.re(r"\b(?:absl_nonnull|absl_nullable|absl_nullability_unknown|_Nonnull|_Nullable|_Null_unspecified)\b");
    js::collapse_ws(&js::replace_with(&re, text, " "))
        .trim()
        .to_string()
}

pub fn is_cpp_function_node(ctx: &Ctx, n: SymId) -> bool {
    let s = ctx.sym(n);
    matches!(s.kind, NodeKind::Method | NodeKind::Function) && is_c_family(s.language)
}

pub fn cpp_opening_paren(text: &str, close: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut i = close.min(b.len().saturating_sub(1)) as isize;
    while i >= 0 {
        match b[i as usize] {
            b')' => depth += 1,
            b'(' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i as usize);
                }
            }
            _ => {}
        }
        i -= 1;
    }
    None
}

pub fn cpp_opening_bracket(text: &str, close: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0i32;
    let mut i = close.min(b.len().saturating_sub(1)) as isize;
    while i >= 0 {
        match b[i as usize] {
            b']' => depth += 1,
            b'[' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i as usize);
                }
            }
            _ => {}
        }
        i -= 1;
    }
    None
}

pub fn cpp_closing_bracket(text: &str, open: usize) -> Option<usize> {
    let b = text.as_bytes();
    let mut depth = 0i32;
    for (i, &c) in b.iter().enumerate().skip(open) {
        if c == b'[' {
            depth += 1;
        } else if c == b']' {
            depth -= 1;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// How many array dimensions the declarator that `rest` follows declares.
pub fn cpp_array_dimensions(ctx: &Ctx, rest: &str) -> u32 {
    let open_re = ctx.re(r"^\s*\[(?!\[)");
    let mut rest = rest.to_string();
    let mut dimensions = 0;
    while let Some(m) = js::caps(&open_re, &rest) {
        dimensions += 1;
        let open = m.get(0).unwrap().end() - 1;
        let Some(close) = cpp_closing_bracket(&rest, open) else {
            break;
        };
        rest = rest[close + 1..].to_string();
    }
    dimensions
}

/// An element of the array `ty`: the type it is declared with, one dimension less.
pub fn cpp_array_element(ty: &WrittenType) -> WrittenType {
    let mut element = ty.clone();
    element.array = match ty.array {
        Some(a) if a > 1 => Some(a - 1),
        _ => None,
    };
    element
}

/// The element a subscript of a value of type `ty` reads.
pub fn cpp_subscript_element(ctx: &Ctx, ty: &WrittenType, at: &Ref) -> Option<WrittenType> {
    if ty.array.is_some_and(|a| a > 0) {
        return Some(cpp_array_element(ty));
    }
    if aliases::is_pointer_type(&ty.text) {
        return cpp_pointee(ctx, ty);
    }
    let segments = aliases::type_segments(ctx, &ty.text)?;
    let last = segments.last()?.as_str();
    let library = segments[0] == "std" || segments[0] == "absl";
    if library && CPP_LIBRARY_MAPS.contains(&last) {
        let value = aliases::template_arguments(&ty.text)?.get(1).cloned()?;
        return Some(ty.with_text(cpp_without_nullability(ctx, &value)));
    }
    if (library && CPP_LIBRARY_SEQUENCES.contains(&last)) || CPP_PROJECT_SEQUENCES.contains(&last) {
        return cpp_range_element_of(ctx, ty, at);
    }
    if library {
        return None;
    }
    let cls = cpp_written_class(ctx, ty, at)?;
    let element = cpp_return_type_of(ctx, &cpp_methods_of(ctx, cls, "operator[]", at))?;
    let trimmed = js::replace_with(&ctx.re(r"&+\s*$"), &element.text, "");
    Some(element.with_text(trimmed.trim()))
}

/// The expanded definition a macro-defined C++ method carries as its signature.
pub fn cpp_macro_method_head(ctx: &Ctx, f: SymId) -> Option<String> {
    let s = ctx.sym(f);
    if s.kind != NodeKind::Method {
        return None;
    }
    let sig = s.signature.as_deref().filter(|g| !g.is_empty())?;
    js::test(&ctx.re(&format!(r"::\s*{}\s*\(", js::escape(&s.name))), sig).then(|| sig.to_string())
}

/// The return type of a C or C++ function, as its declaration writes it.
pub fn cpp_written_return_type(ctx: &Ctx, f: SymId) -> Option<String> {
    let s = ctx.sym(f);
    let macro_head = cpp_macro_method_head(ctx, f);
    let head = match macro_head {
        Some(h) => h,
        None => {
            let src = ctx.source(&s.file)?;
            let from = (s.start_line as usize)
                .saturating_sub(1)
                .min(src.lines.len());
            let to = (s.start_line as usize + 2).min(src.lines.len());
            js::slice_from(&src.lines[from..to].join(" "), s.start_column as usize).to_string()
        }
    };
    let name = js::escape(&s.name);
    let name = match name.strip_prefix("operator") {
        Some(rest) => format!(r"operator\s*{rest}"),
        None => name,
    };
    let declared = js::caps(
        &ctx.re(&format!(
            r"^(.*?)(?:\b[A-Za-z_]\w*\s*(?:<[^<>;{{}}]*>)?\s*::\s*)*\b{name}\s*\("
        )),
        &head,
    )?;
    let mut written = js::group(&declared, 1).to_string();
    if js::test(&ctx.re(r"^\s*template\b"), &written) {
        let open = written.find('<');
        let mut depth = 0i32;
        let mut end = None;
        if let Some(open) = open {
            for (i, c) in written.bytes().enumerate().skip(open) {
                if c == b'<' {
                    depth += 1;
                } else if c == b'>' {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i);
                        break;
                    }
                }
            }
        }
        written = written[end? + 1..].to_string();
    }
    let w = js::replace_with(&ctx.re(r"\[\[[^\]]*\]\]"), &written, " ");
    let w = js::replace_with(
        &ctx.re(r"\b(?:inline|static|virtual|constexpr|consteval|explicit|friend|extern)\b"),
        &w,
        " ",
    );
    let w = js::replace_first(
        &ctx.re(r"^\s*(?:[A-Z][A-Z0-9]*_[A-Z0-9_]*(?:\([^()]*\))?\s+)+"),
        &w,
        |_| String::new(),
    );
    let w = js::replace_first(
        &ctx.re(r"(?:\s+[A-Z][A-Z0-9]*_[A-Z0-9_]*(?:\([^()]*\))?)+\s*$"),
        &w,
        |_| String::new(),
    );
    let written = cpp_without_nullability(ctx, &w);
    if written.is_empty()
        || written == "void"
        || written == "auto"
        || js::test(&ctx.re(r"\bdecltype\b"), &written)
    {
        return None;
    }
    Some(written)
}

/// The template parameter names a C++ function template's own header declares.
pub fn cpp_own_template_parameters(ctx: &Ctx, f: SymId) -> Vec<String> {
    let s = ctx.sym(f);
    let Some(src) = ctx.source(&s.file) else {
        return Vec::new();
    };
    let line = s.start_line as usize;
    let at = src
        .lines
        .get(line.wrapping_sub(1))
        .cloned()
        .unwrap_or_default();
    let from = line.saturating_sub(9).min(src.lines.len());
    let to = line.saturating_sub(1).min(src.lines.len());
    let mut parts: Vec<String> = src.lines[from..to.max(from)].to_vec();
    parts.push(js::slice(&at, 0, s.start_column as usize).to_string());
    let before = parts
        .iter()
        .map(|l| cpp_code_of(ctx, l).unwrap_or_default())
        .collect::<Vec<_>>()
        .join(" ");
    let before = before.trim_end();
    let b = before.as_bytes();
    if !before.ends_with('>') {
        return Vec::new();
    }
    let mut depth = 0i32;
    let mut i = b.len() as isize - 1;
    while i >= 0 {
        let c = b[i as usize];
        if c == b'>' {
            depth += 1;
        } else if c == b'<' {
            depth -= 1;
            if depth == 0 {
                let iu = i as usize;
                if !js::test(&ctx.re(r"\btemplate\s*$"), &before[..iu]) {
                    return Vec::new();
                }
                let tail = ctx.re(r"([A-Za-z_]\w*)$");
                return split_cpp_top_level(ctx, &before[iu + 1..before.len() - 1])
                    .iter()
                    .map(|p| {
                        let head = p.split('=').next().unwrap_or("").trim().to_string();
                        js::caps(&tail, &head)
                            .map(|c| js::group(&c, 1).to_string())
                            .unwrap_or_default()
                    })
                    .collect();
            }
        }
        i -= 1;
    }
    Vec::new()
}

/// What a call of the C++ member template `methods` with explicit template arguments `args` returns.
pub fn cpp_member_template_return(
    ctx: &Ctx,
    methods: &[SymId],
    args: &str,
    site: &WrittenType,
) -> Option<WrittenType> {
    let mut slot: Option<(usize, String, String)> = None;
    let cvp = ctx.re(r"\b(?:const|volatile)\b|[*&]");
    for &f in methods {
        let Some(written) = cpp_written_return_type(ctx, f) else {
            continue;
        };
        let param = js::replace_with(&cvp, &written, " ").trim().to_string();
        let index = if param.is_empty() {
            None
        } else {
            cpp_own_template_parameters(ctx, f)
                .iter()
                .position(|p| *p == param)
        };
        let index = index?;
        if slot.as_ref().is_some_and(|s| s.0 != index) {
            return None;
        }
        if slot.is_none() {
            slot = Some((index, written, param));
        }
    }
    let (index, written, param) = slot?;
    let inner = js::slice(args, 1, args.len().saturating_sub(1));
    let arg = split_cpp_top_level(ctx, inner).get(index).cloned()?;
    if arg.is_empty() {
        return None;
    }
    let re = ctx.re(&format!(r"\b{}\b", js::escape(&param)));
    Some(site.with_text(js::replace_first(&re, &written, |_| arg.clone())))
}

/// What one of `fns` returns, as written where it is declared.
pub fn cpp_return_type_of(ctx: &Ctx, fns: &[SymId]) -> Option<WrittenType> {
    for &f in fns {
        if let Some(text) = cpp_written_return_type(ctx, f) {
            let s = ctx.sym(f);
            return Some(WrittenType {
                text,
                scope: aliases::parent_scope(&s.qualified_name).to_string(),
                file: s.file.clone(),
                line: s.start_line,
                parameters: aliases::template_parameters(ctx, f),
                array: None,
                class_scope: None,
            });
        }
    }
    None
}

/// The C++ class `cls`'s methods named `name`: its own, else the nearest a base has.
pub fn cpp_methods_of(ctx: &Ctx, cls: SymId, name: &str, at: &Ref) -> Vec<SymId> {
    let own: Vec<SymId> = ctx
        .by_qname(&format!("{}::{name}", ctx.sym(cls).qualified_name))
        .iter()
        .copied()
        .filter(|&n| is_cpp_function_node(ctx, n))
        .collect();
    if !own.is_empty() {
        return own;
    }
    cpp_method_of(ctx, cls, name, at).into_iter().collect()
}

/// `ty` once the aliases it is written with are followed, each in its own scope.
pub fn cpp_follow_written_aliases(ctx: &Ctx, ty: &WrittenType, at: &Ref) -> Option<WrittenType> {
    let mut current = ty.clone();
    for _ in 0..4 {
        match aliases::aliased_type_written(
            ctx,
            &current.text,
            &aliases::scopes_within(&current.scope),
            at,
        ) {
            Tri::Undefined => return Some(current),
            Tri::Null => return None,
            Tri::Found((text, alias)) => {
                let a = ctx.sym(alias);
                current = WrittenType {
                    text: if aliases::is_pointer_type(&current.text) {
                        format!("{text}*")
                    } else {
                        text
                    },
                    scope: aliases::parent_scope(&a.qualified_name).to_string(),
                    file: a.file.clone(),
                    line: a.start_line,
                    parameters: aliases::template_parameters(ctx, alias),
                    array: current.array,
                    class_scope: None,
                };
            }
        }
    }
    None
}

/// Is the type `ty` names a template parameter's?
pub fn is_cpp_template_parameter_type(ctx: &Ctx, ty: &WrittenType) -> bool {
    aliases::type_segments(ctx, &ty.text)
        .and_then(|s| s.into_iter().next())
        .is_some_and(|h| ty.parameters.contains(&h))
}

/// The project class `ty` names, looked up as C++ looks it up where it is written.
pub fn cpp_written_class(ctx: &Ctx, ty: &WrittenType, at: &Ref) -> Option<SymId> {
    match supertypes::class_written(ctx, &ty.text, &ty.site(), at) {
        Tri::Found(f) if CPP_CLASS_KINDS.contains(&ctx.kind(f.node)) => Some(f.node),
        _ => None,
    }
}
