//! A Rust file's local bindings, read from its text, for typing the receiver
//! of a method call: every parameter of a function or a closure, `let`,
//! `if let` / `while let`, `let … else`, `for` and match-arm binding, where it
//! is visible, and what the source writes of its type there.
//!
//! A reader, not a parser. Comments and the contents of strings and
//! characters are blanked first (offsets and newlines kept), brackets are
//! paired, and each form is read from its keyword. A name a pattern may bind
//! is bound, typed or not, so that it hides the declarations around it rather
//! than taking their type: `fn f(srv: Server) { if let Some(srv) = spare() {
//! srv.run() } }` calls `run` on what `spare()` holds, not on a `Server`.
//!
//! Offsets are byte offsets into the blanked text, which keeps the source's
//! length and line breaks.

use std::cell::OnceCell;
use std::collections::HashMap;

/// Where a name sits in a tuple-struct, variant or struct pattern.
#[derive(Debug, Clone, PartialEq)]
pub struct PatternSite {
    /// The pattern's path as written: `Commands::Build`, `Self::Text`, `Some`; '' for a tuple's own `(a, b)`.
    pub path: String,
    /// The name's position among a tuple pattern's fields.
    pub index: Option<usize>,
    /// The field a struct pattern binds the name from.
    pub field: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BindingKind {
    Param,
    Closure,
    Let,
    Cond,
    For,
    Arm,
}

#[derive(Debug, Clone)]
pub struct Binding {
    /// Offset of the bound name.
    pub at: usize,
    pub kind: BindingKind,
    /// Where the name is visible: from `from` up to, not including, `to`.
    pub from: usize,
    pub to: usize,
    /// The type written for it — a parameter's or a `let`'s — when its pattern is the name alone.
    pub ty: Option<String>,
    /// A `let`'s value, when its pattern is the name alone and it writes no type.
    pub init: Option<String>,
    /// The pattern around the name, when it is bound inside a tuple-struct, variant or struct pattern.
    pub site: Option<PatternSite>,
    /// What an `if let`, `while let`, `let … else` or `match` pattern is matched against, or what a `for` iterates.
    pub scrutinee: Option<String>,
}

/// A function with a body: its generic parameters and `where` clause, and the span its parameters are bound in.
#[derive(Debug, Clone)]
pub struct RustFn {
    /// Offset of the `fn` keyword.
    pub at: usize,
    pub generics: String,
    pub where_: String,
    /// From its parameter list's `(` to its body's `}`.
    pub from: usize,
    pub to: usize,
    pub params_end: usize,
    pub body: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Impl,
    Trait,
}

/// An `impl` or `trait` block: its generic parameters and `where` clause, and its body.
#[derive(Debug, Clone)]
pub struct ImplBlock {
    pub at: usize,
    pub kind: BlockKind,
    pub generics: String,
    pub where_: String,
    /// Its body, from `{` to `}`.
    pub from: usize,
    pub to: usize,
}

/// Generics and a `where` clause, as `bounds_of` reads them.
pub struct Generics<'a> {
    pub generics: &'a str,
    pub where_: &'a str,
}

pub struct Scopes {
    /// The source with its comments and the contents of its strings and characters blanked.
    pub code: String,
    pub line_starts: Vec<usize>,
    pub fns: Vec<RustFn>,
    pub impls: Vec<ImplBlock>,
    /// Each `(`, `[` and `{` → the bracket closing it.
    pub close: HashMap<usize, usize>,
    /// Offsets of every `{` and `}`, with the innermost block open after each.
    events: Vec<usize>,
    open_after: Vec<isize>,
    bindings: OnceCell<HashMap<String, Vec<Binding>>>,
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c >= 0x80
}

fn ident_at_opt(code: &[u8], i: Option<usize>) -> bool {
    i.and_then(|i| code.get(i)).is_some_and(|&c| is_ident_byte(c))
}

/// Rust source with its comments, and the contents of its strings, raw
/// strings and character literals, blanked: offsets and newlines stay. A
/// lifetime (`'a`) is no character literal.
pub fn blank_rust_code(src: &str) -> String {
    let s = src.as_bytes();
    let n = s.len();
    let mut out = s.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for b in out.iter_mut().take(to.min(n)).skip(from) {
            if *b != b'\n' {
                *b = b' ';
            }
        }
    };
    let mut i = 0;
    while i < n {
        let c = s[i];
        let d = s.get(i + 1).copied();
        match c {
            b'/' if d == Some(b'/') => {
                let to = s[i..].iter().position(|&b| b == b'\n').map_or(n, |p| i + p);
                blank(&mut out, i, to);
                i = to;
                continue;
            }
            b'/' if d == Some(b'*') => {
                let mut depth = 1;
                let mut j = i + 2;
                while j < n && depth > 0 {
                    if s[j] == b'/' && s.get(j + 1) == Some(&b'*') {
                        depth += 1;
                        j += 2;
                    } else if s[j] == b'*' && s.get(j + 1) == Some(&b'/') {
                        depth -= 1;
                        j += 2;
                    } else {
                        j += 1;
                    }
                }
                blank(&mut out, i, j);
                i = j;
                continue;
            }
            b'"' => {
                // A raw string: `r"…"`, `r#"…"#`, also after a `b` or `c` prefix.
                let mut k = i as isize - 1;
                while k >= 0 && s[k as usize] == b'#' {
                    k -= 1;
                }
                let mut p = k - 1;
                if p >= 0 && (s[p as usize] == b'b' || s[p as usize] == b'c') {
                    p -= 1;
                }
                if k >= 0 && s[k as usize] == b'r' && (p < 0 || !is_ident_byte(s[p as usize])) {
                    let hashes = i - 1 - k as usize;
                    let mut close = vec![b'"'];
                    close.extend(std::iter::repeat_n(b'#', hashes));
                    let to = find(s, &close, i + 1).unwrap_or(n);
                    blank(&mut out, i + 1, to);
                    i = (to + close.len()).min(n);
                    continue;
                }
                let mut j = i + 1;
                while j < n && s[j] != b'"' {
                    j += if s[j] == b'\\' { 2 } else { 1 };
                }
                blank(&mut out, i + 1, j.min(n));
                i = (j + 1).min(n);
                continue;
            }
            b'\'' => {
                if d == Some(b'\\') {
                    if let Some(end) = s.get(i + 3..).and_then(|r| r.iter().position(|&b| b == b'\'')) {
                        let end = i + 3 + end;
                        if end - i <= 12 {
                            blank(&mut out, i + 1, end);
                            i = end + 1;
                            continue;
                        }
                    }
                } else if let Some(d) = d {
                    // `'x'`, or a multi-byte character: the quote right after it.
                    let len = utf8_len(d);
                    if d != b'\n' && s.get(i + 1 + len) == Some(&b'\'') {
                        blank(&mut out, i + 1, i + 1 + len);
                        i += 2 + len;
                        continue;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    // Blanking only rewrites whole UTF-8 sequences (strings, comments, chars) to spaces.
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn utf8_len(first: u8) -> usize {
    match first {
        0xF0..=0xFF => 4,
        0xE0..=0xEF => 3,
        0xC0..=0xDF => 2,
        _ => 1,
    }
}

pub fn find(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if needle.is_empty() || from > hay.len() {
        return None;
    }
    hay[from..]
        .windows(needle.len())
        .position(|w| w == needle)
        .map(|p| p + from)
}

/// Each `(`, `[` and `{` → the offset of the bracket closing it.
fn pair_brackets(code: &[u8]) -> HashMap<usize, usize> {
    let mut close = HashMap::new();
    let mut stack: Vec<usize> = Vec::new();
    for (i, &c) in code.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' => stack.push(i),
            b')' | b']' | b'}' => {
                let want = match c {
                    b')' => b'(',
                    b']' => b'[',
                    _ => b'{',
                };
                // A stray closer (a macro's unbalanced tokens) closes nothing.
                if stack.last().is_some_and(|&o| code[o] == want) {
                    close.insert(stack.pop().unwrap(), i);
                }
            }
            _ => {}
        }
    }
    close
}

pub fn skip_space(code: &[u8], mut at: usize) -> usize {
    while at < code.len() && matches!(code[at], b' ' | b'\n' | b'\r' | b'\t' | 0x0b | 0x0c) {
        at += 1;
    }
    at
}

/// The identifier starting at `at`, if one does.
pub fn ident_at(code: &str, at: usize) -> Option<&str> {
    let b = code.as_bytes();
    let first = *b.get(at)?;
    if !(first.is_ascii_alphabetic() || first == b'_') {
        return None;
    }
    let mut end = at + 1;
    while end < b.len() && (b[end].is_ascii_alphanumeric() || b[end] == b'_') {
        end += 1;
    }
    Some(&code[at..end])
}

/// The offset of the `>` closing the `<` at `at`, or none. `->` and `=>` close nothing.
pub fn angle_close(code: &[u8], at: usize, limit: usize) -> Option<usize> {
    let mut depth = 0i32;
    for i in at..limit.min(code.len()) {
        match code[i] {
            b'<' => depth += 1,
            b'>' if i == 0 || (code[i - 1] != b'-' && code[i - 1] != b'=') => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b';' | b'{' | b'}' => return None,
            _ => {}
        }
    }
    None
}

/// Walk `code` from `from` to `to` over what lies outside brackets: `stop`
/// sees each such offset and a true return ends the walk there; a `(`, `[` or
/// `{` group is jumped over whole, and with `angles` so is a `<…>` one.
pub fn walk_top(
    code: &[u8],
    from: usize,
    to: usize,
    close: &HashMap<usize, usize>,
    angles: bool,
    mut stop: impl FnMut(usize) -> bool,
) -> usize {
    let mut depth = 0i32;
    let mut i = from;
    while i < to {
        let c = code[i];
        if depth == 0 && stop(i) {
            return i;
        }
        if matches!(c, b'(' | b'[' | b'{') {
            match close.get(&i) {
                Some(&end) if end < to => {
                    i = end + 1;
                    continue;
                }
                _ => return to,
            }
        }
        if angles && c == b'<' {
            depth += 1;
        } else if angles && c == b'>' && i > 0 && code[i - 1] != b'-' && code[i - 1] != b'=' && depth > 0 {
            depth -= 1;
        }
        i += 1;
    }
    to
}

/// The spans of `code` between `from` and `to` that top-level commas part.
pub fn split_top(
    code: &[u8],
    from: usize,
    to: usize,
    close: &HashMap<usize, usize>,
    angles: bool,
) -> Vec<(usize, usize)> {
    let mut parts = Vec::new();
    let mut start = from;
    loop {
        let comma = walk_top(code, start, to, close, angles, |i| code[i] == b',');
        if comma >= to {
            break;
        }
        parts.push((start, comma));
        start = comma + 1;
    }
    if start < to && code[start..to].iter().any(|c| !c.is_ascii_whitespace()) {
        parts.push((start, to));
    }
    parts
}

/// A top-level `:` that is no part of a `::`, between `from` and `to`.
fn top_colon(code: &[u8], from: usize, to: usize, close: &HashMap<usize, usize>) -> Option<usize> {
    let at = walk_top(code, from, to, close, true, |i| {
        code[i] == b':' && code.get(i + 1) != Some(&b':') && (i == 0 || code[i - 1] != b':')
    });
    (at < to).then_some(at)
}

/// Whether the keyword `word` starts at `i`, a whole word.
pub fn word_at(code: &[u8], i: usize, word: &str) -> bool {
    code[i..].starts_with(word.as_bytes())
        && !(i > 0 && is_ident_byte(code[i - 1]))
        && !ident_at_opt(code, Some(i + word.len()))
}

/// Words a pattern writes that bind nothing.
const PATTERN_WORDS: &[&str] = &["ref", "mut", "box", "true", "false", "self", "Self", "crate", "super", "_"];

fn is_binding_name(id: &str) -> bool {
    id.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && !PATTERN_WORDS.contains(&id)
}

/// `^\s*(?:ref\s+)?(?:mut\s+)?([A-Za-z_]\w*)\s*$` — the name a simple pattern binds.
fn simple_name(text: &str, allow_ref: bool) -> Option<&str> {
    let mut t = text.trim();
    if allow_ref {
        if let Some(r) = t.strip_prefix("ref") {
            if r.starts_with(char::is_whitespace) {
                t = r.trim_start();
            }
        }
    }
    if let Some(r) = t.strip_prefix("mut") {
        if r.starts_with(char::is_whitespace) {
            t = r.trim_start();
        }
    }
    let ok = t.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    ok.then_some(t)
}

/// A path at `at` — `Commands::Build`, `Self::A` — as its segments and the offset after it.
fn read_path(code: &str, at: usize) -> (Vec<String>, usize) {
    let b = code.as_bytes();
    let mut segments = Vec::new();
    let mut p = at;
    loop {
        let Some(id) = ident_at(code, p) else { break };
        segments.push(id.to_string());
        p += id.len();
        let mut q = skip_space(b, p);
        if !b[q..].starts_with(b"::") {
            break;
        }
        q = skip_space(b, q + 2);
        // A turbofish: `Foo::<T>::Bar`, `Vec::<u8>`.
        if b.get(q) == Some(&b'<') {
            let Some(end) = angle_close(b, q, b.len()) else { break };
            p = end + 1;
            q = skip_space(b, p);
            if !b[q..].starts_with(b"::") {
                break;
            }
            q = skip_space(b, q + 2);
        }
        p = q;
    }
    (segments, p)
}

/// The names a pattern between `from` and `to` binds, each with the
/// tuple-struct, variant or struct pattern it sits in.
fn walk_pattern(
    code: &str,
    from: usize,
    to: usize,
    close: &HashMap<usize, usize>,
    site: Option<&PatternSite>,
    emit: &mut dyn FnMut(&str, usize, Option<PatternSite>),
) {
    let b = code.as_bytes();
    let mut p = from;
    while p < to {
        let c = b[p];
        if c == b'(' || c == b'[' {
            // A tuple or a slice: its fields' types are not read here.
            let end = close.get(&p).copied().unwrap_or(to).min(to);
            for (a, z) in split_top(b, p + 1, end, close, false) {
                walk_pattern(code, a, z, close, None, emit);
            }
            p = end + 1;
            continue;
        }
        if !(c.is_ascii_alphabetic() || c == b'_') || (p > 0 && is_ident_byte(b[p - 1])) {
            p += 1;
            continue;
        }
        let (segments, end_path) = read_path(code, p);
        if segments.is_empty() {
            p += 1;
            continue;
        }
        let after = skip_space(b, end_path);
        let written = segments.join("::");
        if b.get(after) == Some(&b'(') {
            let end = close.get(&after).copied().unwrap_or(to).min(to);
            for (index, (a, z)) in split_top(b, after + 1, end, close, false).into_iter().enumerate() {
                let s = PatternSite {
                    path: written.clone(),
                    index: Some(index),
                    field: None,
                };
                walk_pattern(code, a, z, close, Some(&s), emit);
            }
            p = end + 1;
            continue;
        }
        if b.get(after) == Some(&b'{') {
            let end = close.get(&after).copied().unwrap_or(to).min(to);
            for (a, z) in split_top(b, after + 1, end, close, false) {
                if let Some(colon) = top_colon(b, a, z, close) {
                    let field = code[a..colon].trim();
                    if !field.is_empty() && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                        let s = PatternSite {
                            path: written.clone(),
                            index: None,
                            field: Some(field.to_string()),
                        };
                        walk_pattern(code, colon + 1, z, close, Some(&s), emit);
                    }
                    continue;
                }
                // `name`, `ref name`, `mut name`, `ref mut name` bind the field of that name.
                let text = &code[a..z];
                if let Some(name) = simple_name(text, true) {
                    if is_binding_name(name) {
                        let at = a + text.rfind(name).unwrap_or(0);
                        emit(
                            name,
                            at,
                            Some(PatternSite {
                                path: written.clone(),
                                index: None,
                                field: Some(name.to_string()),
                            }),
                        );
                    }
                }
            }
            p = end + 1;
            continue;
        }
        if segments.len() == 1 && is_binding_name(&segments[0]) {
            emit(&segments[0], p, site.cloned());
        }
        // `name @ pattern`: the subpattern binds in its own right.
        p = if b.get(after) == Some(&b'@') { after + 1 } else { end_path };
    }
}

/// Whether a `|` at `i` opens a closure's parameter list.
fn opens_closure(code: &[u8], i: usize) -> bool {
    let mut k = i as isize - 1;
    while k >= 0 && code[k as usize].is_ascii_whitespace() {
        k -= 1;
    }
    if k < 0 {
        return true;
    }
    let k = k as usize;
    let c = code[k];
    if c == b':' {
        return k == 0 || code[k - 1] != b':';
    }
    if b"([{,=;".contains(&c) {
        return true;
    }
    if c == b'>' {
        return k > 0 && code[k - 1] == b'=';
    }
    let mut s = k + 1;
    while s > 0 && (code[s - 1].is_ascii_alphanumeric() || code[s - 1] == b'_') && k + 1 - (s - 1) <= 8 {
        s -= 1;
    }
    let word = &code[s..=k];
    matches!(word, b"move" | b"return" | b"async" | b"static")
}

/// Where an expression starting at `from` ends: its first top-level `,` or `;`, or the bracket closing around it.
fn expression_end(code: &[u8], from: usize, close: &HashMap<usize, usize>) -> usize {
    let mut i = from;
    while i < code.len() {
        let c = code[i];
        if matches!(c, b'(' | b'[' | b'{') {
            match close.get(&i) {
                Some(&e) => {
                    i = e + 1;
                    continue;
                }
                None => return code.len(),
            }
        }
        if matches!(c, b',' | b';' | b')' | b']' | b'}') {
            return i;
        }
        i += 1;
    }
    code.len()
}

/// Offsets of each match of a whole word `word` in `code`.
fn word_hits<'a>(code: &'a [u8], word: &'a str) -> impl Iterator<Item = usize> + 'a {
    let w = word.as_bytes();
    let mut from = 0;
    std::iter::from_fn(move || {
        while let Some(i) = find(code, w, from) {
            from = i + 1;
            if word_at(code, i, word) {
                return Some(i);
            }
        }
        None
    })
}

impl Scopes {
    /// A Rust file read for its bindings: its blanked text, its functions with
    /// bodies, its `impl` and `trait` blocks.
    pub fn read(source: &str) -> Scopes {
        let code = blank_rust_code(source);
        let b = code.as_bytes();
        let n = b.len();
        let mut line_starts = vec![0];
        for (i, &c) in b.iter().enumerate() {
            if c == b'\n' {
                line_starts.push(i + 1);
            }
        }
        let close = pair_brackets(b);

        let mut fns = Vec::new();
        for at in word_hits(b, "fn") {
            let mut p = skip_space(b, at + 2);
            let Some(name) = ident_at(&code, p) else { continue };
            p = skip_space(b, p + name.len());
            let mut generics = String::new();
            if b.get(p) == Some(&b'<') {
                let Some(end) = angle_close(b, p, n) else { continue };
                generics = code[p + 1..end].to_string();
                p = skip_space(b, end + 1);
            }
            if b.get(p) != Some(&b'(') {
                continue;
            }
            let Some(&params_end) = close.get(&p) else { continue };
            // The body: the first `{` past the return type and `where` clause.
            let mut body = None;
            let mut where_ = None;
            let mut q = params_end + 1;
            while q < n {
                let c = b[q];
                if c == b'{' {
                    body = Some(q);
                    break;
                }
                if c == b';' || c == b'}' {
                    break;
                }
                if c == b'(' || c == b'[' {
                    q = close.get(&q).copied().unwrap_or(n) + 1;
                    continue;
                }
                if where_.is_none() && c == b'w' && word_at(b, q, "where") {
                    where_ = Some(q + 5);
                }
                q += 1;
            }
            let Some(body) = body else { continue };
            fns.push(RustFn {
                at,
                generics,
                where_: where_.map(|w| code[w..body].to_string()).unwrap_or_default(),
                from: p,
                to: close.get(&body).copied().unwrap_or(n),
                params_end,
                body,
            });
        }

        // `impl<T: Bound> … {` and `trait Name<T>: Super where … {` — an item,
        // not the `impl Trait` of a type.
        let mut impls = Vec::new();
        let mut hits: Vec<(usize, BlockKind)> = word_hits(b, "impl").map(|i| (i, BlockKind::Impl)).collect();
        hits.extend(word_hits(b, "trait").map(|i| (i, BlockKind::Trait)));
        hits.sort_by_key(|h| h.0);
        for (at, kind) in hits {
            if kind == BlockKind::Impl {
                let mut k = at as isize - 1;
                while k >= 0 && b[k as usize].is_ascii_whitespace() {
                    k -= 1;
                }
                if k >= 0 {
                    let ku = k as usize;
                    let before = &code[ku.saturating_sub(7)..=ku];
                    if !b"{};]".contains(&b[ku]) && !before.ends_with("unsafe") && !before.ends_with("default") {
                        continue;
                    }
                }
            }
            let kw = if kind == BlockKind::Impl { 4 } else { 5 };
            let mut p = skip_space(b, at + kw);
            if kind == BlockKind::Trait {
                let Some(name) = ident_at(&code, p) else { continue };
                p = skip_space(b, p + name.len());
            }
            let mut generics = String::new();
            if b.get(p) == Some(&b'<') {
                let Some(end) = angle_close(b, p, n) else { continue };
                generics = code[p + 1..end].to_string();
                p = end + 1;
            }
            let mut where_ = None;
            let mut open = None;
            let mut q = p;
            while q < n {
                let c = b[q];
                if c == b'{' {
                    open = Some(q);
                    break;
                }
                if c == b';' || c == b'}' {
                    break;
                }
                if c == b'(' || c == b'[' {
                    q = close.get(&q).copied().unwrap_or(n) + 1;
                    continue;
                }
                if where_.is_none() && c == b'w' && word_at(b, q, "where") {
                    where_ = Some(q + 5);
                }
                q += 1;
            }
            let Some(open) = open else { continue };
            impls.push(ImplBlock {
                at,
                kind,
                generics,
                where_: where_.map(|w| code[w..open].to_string()).unwrap_or_default(),
                from: open,
                to: close.get(&open).copied().unwrap_or(n),
            });
        }

        let mut events = Vec::new();
        let mut open_after = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for (i, &c) in b.iter().enumerate() {
            if c == b'{' {
                stack.push(i);
            } else if c == b'}' {
                stack.pop();
            } else {
                continue;
            }
            events.push(i);
            open_after.push(stack.last().map_or(-1, |&o| o as isize));
        }

        Scopes {
            code,
            line_starts,
            fns,
            impls,
            close,
            events,
            open_after,
            bindings: OnceCell::new(),
        }
    }

    /// The offset of a 1-based line and 0-based column.
    pub fn offset(&self, line: u32, column: u32) -> usize {
        let start = self
            .line_starts
            .get((line as usize).saturating_sub(1))
            .copied()
            .unwrap_or(self.code.len());
        (start + column as usize).min(self.code.len())
    }

    /// The 1-based line of an offset.
    pub fn line_at(&self, offset: usize) -> u32 {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i as u32 + 1,
            Err(i) => i as u32,
        }
    }

    /// The innermost `{…}` holding `offset`, as (open, close).
    pub fn inner_block_at(&self, offset: usize) -> Option<(usize, usize)> {
        let at = self.events.partition_point(|&e| e < offset);
        if at == 0 {
            return None;
        }
        let o = self.open_after[at - 1];
        (o >= 0).then(|| {
            let o = o as usize;
            (o, self.close.get(&o).copied().unwrap_or(self.code.len()))
        })
    }

    /// The innermost `impl` or `trait` body around `offset`.
    pub fn impl_at(&self, offset: usize) -> Option<&ImplBlock> {
        self.impls
            .iter()
            .filter(|b| offset > b.from && offset < b.to)
            .max_by_key(|b| b.from)
    }

    /// The innermost function with a body around `offset`.
    pub fn fn_at(&self, offset: usize) -> Option<&RustFn> {
        self.fns
            .iter()
            .filter(|f| offset >= f.from && offset <= f.to)
            .max_by_key(|f| f.from)
    }

    /// The binding of `name` in scope at `offset`: the innermost, latest one,
    /// and whether an alternative pattern binds it more than once there.
    pub fn binding_at(&self, name: &str, offset: usize) -> Option<(&Binding, bool)> {
        let list = self.bindings.get_or_init(|| self.read_bindings()).get(name)?;
        let best = list
            .iter()
            .filter(|b| offset >= b.from && offset < b.to)
            .max_by_key(|b| b.at)?;
        let ambiguous = list.iter().any(|b| {
            !std::ptr::eq(b, best)
                && b.from == best.from
                && b.to == best.to
                && b.kind == best.kind
                && matches!(b.kind, BindingKind::Arm | BindingKind::Cond | BindingKind::Let)
        });
        Some((best, ambiguous))
    }

    fn block_at(&self, offset: usize) -> Option<(usize, usize)> {
        self.inner_block_at(offset)
    }

    fn read_bindings(&self) -> HashMap<String, Vec<Binding>> {
        let code = self.code.as_str();
        let b = code.as_bytes();
        let close = &self.close;
        let n = b.len();
        let mut bindings: HashMap<String, Vec<Binding>> = HashMap::new();
        let mut add = |name: &str, binding: Binding| bindings.entry(name.to_string()).or_default().push(binding);

        fn bind_pattern(
            code: &str,
            close: &HashMap<usize, usize>,
            from: usize,
            to: usize,
            kind: BindingKind,
            scope: (usize, usize),
            scrutinee: Option<&str>,
            add: &mut dyn FnMut(&str, Binding),
        ) {
            let b = code.as_bytes();
            let mut emit = |name: &str, at: usize, site: Option<PatternSite>| {
                add(
                    name,
                    Binding {
                        at,
                        kind,
                        from: scope.0,
                        to: scope.1,
                        ty: None,
                        init: None,
                        site,
                        scrutinee: scrutinee.map(str::to_string),
                    },
                )
            };
            // A tuple pattern, `(k, v)`: a name alone at its top is the tuple's field at its place.
            let open = skip_space(b, from);
            if b.get(open) == Some(&b'(') {
                if let Some(&end) = close.get(&open) {
                    if end < to && skip_space(b, end + 1) >= to {
                        for (index, (a, z)) in split_top(b, open + 1, end, close, false).into_iter().enumerate() {
                            let text = &code[a..z];
                            match simple_name(text, true) {
                                Some(name) if is_binding_name(name) => emit(
                                    name,
                                    a + text.rfind(name).unwrap_or(0),
                                    Some(PatternSite {
                                        path: String::new(),
                                        index: Some(index),
                                        field: None,
                                    }),
                                ),
                                _ => walk_pattern(code, a, z, close, None, &mut emit),
                            }
                        }
                        return;
                    }
                }
            }
            walk_pattern(code, from, to, close, None, &mut emit);
        }

        // A parameter `pattern: Type`, or a closure's `pattern`.
        let param = |from: usize, to: usize, kind: BindingKind, scope: (usize, usize), add: &mut dyn FnMut(&str, Binding)| {
            let colon = top_colon(b, from, to, close);
            let pattern = &code[from..colon.unwrap_or(to)];
            if let Some(name) = simple_name(pattern, false) {
                if is_binding_name(name) {
                    add(
                        name,
                        Binding {
                            at: from + pattern.rfind(name).unwrap_or(0),
                            kind,
                            from: scope.0,
                            to: scope.1,
                            ty: colon.map(|c| code[c + 1..to].trim().to_string()),
                            init: None,
                            site: None,
                            scrutinee: None,
                        },
                    );
                }
                return;
            }
            bind_pattern(code, close, from, colon.unwrap_or(to), kind, scope, None, add);
        };

        for f in &self.fns {
            for (a, z) in split_top(b, f.from + 1, f.params_end, close, true) {
                param(a, z, BindingKind::Param, (f.body, f.to), &mut add);
            }
        }

        let mut i = 0;
        while let Some(at) = find(b, b"|", i) {
            i = at + 1;
            if b.get(at + 1) == Some(&b'|') {
                i = at + 2;
                continue;
            }
            if b.get(at + 1) == Some(&b'=') || !opens_closure(b, at) {
                continue;
            }
            let mut end = None;
            let mut depth = 0i32;
            let mut j = at + 1;
            while j < n && j < at + 2000 {
                let c = b[j];
                if matches!(c, b'(' | b'[' | b'{') {
                    match close.get(&j) {
                        Some(&e) => {
                            j = e + 1;
                            continue;
                        }
                        None => break,
                    }
                }
                if c == b'<' {
                    depth += 1;
                } else if c == b'>' && b[j - 1] != b'-' && b[j - 1] != b'=' {
                    depth -= 1;
                } else if c == b'|' && depth <= 0 {
                    end = Some(j);
                    break;
                } else if matches!(c, b';' | b')' | b']' | b'}') {
                    break;
                }
                j += 1;
            }
            let Some(end) = end else { continue };
            let bs = skip_space(b, end + 1);
            let body_end = if b[bs..].starts_with(b"->") {
                find(b, b"{", bs).map_or(n, |br| close.get(&br).copied().unwrap_or(n))
            } else if b.get(bs) == Some(&b'{') {
                close.get(&bs).copied().unwrap_or(n)
            } else {
                expression_end(b, bs, close)
            };
            for (s, e) in split_top(b, at + 1, end, close, true) {
                param(s, e, BindingKind::Closure, (end + 1, body_end), &mut add);
            }
            // The body is read on: closures nest.
            i = end;
        }

        let is_assign = |i: usize| {
            b[i] == b'='
                && b.get(i + 1) != Some(&b'=')
                && b.get(i + 1) != Some(&b'>')
                && !(i > 0 && b"=!<>".contains(&b[i - 1]))
        };
        for at in word_hits(b, "let") {
            let before = code[at.saturating_sub(16)..at].trim_end();
            let cond = before.ends_with("&&") || ends_with_word(before, "if") || ends_with_word(before, "while");
            let pat_from = at + 3;
            let pat_end = walk_top(b, pat_from, n, close, false, |i| match b[i] {
                b';' | b'}' => true,
                b':' => b.get(i + 1) != Some(&b':') && b[i - 1] != b':',
                _ => is_assign(i),
            });
            if pat_end >= n {
                continue;
            }
            let mut ty = None;
            let mut eq = (b[pat_end] == b'=').then_some(pat_end);
            if b[pat_end] == b':' {
                let type_end = walk_top(b, pat_end + 1, n, close, true, |i| {
                    b[i] == b';' || b[i] == b'}' || is_assign(i)
                });
                if type_end >= n {
                    continue;
                }
                ty = Some(code[pat_end + 1..type_end].trim().to_string());
                if b[type_end] == b'=' {
                    eq = Some(type_end);
                }
            }
            let simple = simple_name(&code[pat_from..pat_end], true);
            if cond {
                let Some(eq) = eq else { continue };
                let value_end = walk_top(b, eq + 1, n, close, false, |i| {
                    b[i] == b'{' || b[i] == b';' || (b[i] == b'&' && b.get(i + 1) == Some(&b'&'))
                });
                let scrutinee = code[eq + 1..value_end].trim().to_string();
                let open = if b.get(value_end) == Some(&b'{') {
                    value_end
                } else {
                    walk_top(b, value_end, n, close, false, |i| b[i] == b'{' || b[i] == b';')
                };
                if b.get(open) != Some(&b'{') {
                    continue;
                }
                bind_pattern(
                    code,
                    close,
                    pat_from,
                    pat_end,
                    BindingKind::Cond,
                    (value_end, close.get(&open).copied().unwrap_or(n)),
                    Some(&scrutinee),
                    &mut add,
                );
                continue;
            }
            // A statement: up to its `;`, past a `let … else { … }`'s block.
            let mut init_end = n;
            let stmt_end;
            if let Some(eq) = eq {
                let stop = walk_top(b, eq + 1, n, close, false, |i| {
                    b[i] == b';'
                        || b[i] == b'}'
                        || (b[i] == b'e'
                            && word_at(b, i, "else")
                            && !code[i.saturating_sub(64)..i].trim_end().ends_with('}'))
                });
                init_end = stop;
                if b.get(stop) == Some(&b'e') {
                    let open = skip_space(b, stop + 4);
                    let else_end = if b.get(open) == Some(&b'{') {
                        close.get(&open).copied().unwrap_or(n)
                    } else {
                        n
                    };
                    let semi = skip_space(b, else_end + 1);
                    stmt_end = if b.get(semi) == Some(&b';') { semi } else { else_end };
                } else {
                    stmt_end = stop;
                }
            } else {
                stmt_end = walk_top(b, pat_end, n, close, false, |i| b[i] == b';' || b[i] == b'}');
            }
            let Some(block) = self.block_at(at) else { continue };
            let scope = (stmt_end + 1, block.1);
            let init = eq.map(|e| code[e + 1..init_end.min(n)].trim().to_string());
            if let Some(name) = simple {
                if is_binding_name(name) {
                    let pat = &code[pat_from..pat_end];
                    add(
                        name,
                        Binding {
                            at: pat_from + pat.rfind(name).unwrap_or(0),
                            kind: BindingKind::Let,
                            from: scope.0,
                            to: scope.1,
                            init: if ty.is_none() { init } else { None },
                            ty,
                            site: None,
                            scrutinee: None,
                        },
                    );
                }
                continue;
            }
            bind_pattern(code, close, pat_from, pat_end, BindingKind::Let, scope, init.as_deref(), &mut add);
        }

        for at in word_hits(b, "for") {
            let p = skip_space(b, at + 3);
            // `for<'a>` binds lifetimes; `impl Trait for Type {` no names.
            if b.get(p) == Some(&b'<') {
                continue;
            }
            let in_at = walk_top(b, p, n, close, false, |i| {
                b[i] == b'{' || b[i] == b';' || b[i] == b'=' || (b[i] == b'i' && word_at(b, i, "in"))
            });
            if b.get(in_at) != Some(&b'i') {
                continue;
            }
            let open = walk_top(b, in_at + 2, n, close, false, |i| b[i] == b'{' || b[i] == b';');
            if b.get(open) != Some(&b'{') {
                continue;
            }
            let scrutinee = code[in_at + 2..open].trim().to_string();
            bind_pattern(
                code,
                close,
                p,
                in_at,
                BindingKind::For,
                (open, close.get(&open).copied().unwrap_or(n)),
                Some(&scrutinee),
                &mut add,
            );
        }

        for at in word_hits(b, "match") {
            let from = at + 5;
            let open = walk_top(b, from, n, close, false, |i| matches!(b[i], b'{' | b';' | b'}'));
            if b.get(open) != Some(&b'{') {
                continue;
            }
            let scrutinee = code[from..open].trim().to_string();
            if scrutinee.is_empty() {
                continue;
            }
            let end = close.get(&open).copied().unwrap_or(n);
            let mut arm_start = open + 1;
            let mut p = open + 1;
            while p < end {
                let c = b[p];
                if matches!(c, b'(' | b'[' | b'{') {
                    p = close.get(&p).copied().unwrap_or(end) + 1;
                    continue;
                }
                if c != b'=' || b.get(p + 1) != Some(&b'>') {
                    p += 1;
                    continue;
                }
                // The pattern, without its guard; the guard sees what it binds.
                let guard = walk_top(b, arm_start, p, close, false, |i| b[i] == b'i' && word_at(b, i, "if"));
                let mut q = skip_space(b, p + 2);
                let arm_end;
                if b.get(q) == Some(&b'{') {
                    arm_end = close.get(&q).copied().unwrap_or(end);
                    q = skip_space(b, arm_end + 1);
                    if b.get(q) == Some(&b',') {
                        q += 1;
                    } else {
                        q = arm_end + 1;
                    }
                } else {
                    arm_end = walk_top(b, q, end, close, false, |i| b[i] == b',');
                    q = arm_end + 1;
                }
                bind_pattern(
                    code,
                    close,
                    arm_start,
                    guard,
                    BindingKind::Arm,
                    (guard, arm_end),
                    Some(&scrutinee),
                    &mut add,
                );
                arm_start = q;
                p = q;
            }
        }
        bindings
    }
}

fn ends_with_word(text: &str, word: &str) -> bool {
    text.ends_with(word)
        && !text[..text.len() - word.len()]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// `text` parted at its top-level `sep`s, outside `()`, `[]`, `{}` and `<>` (`->` closes nothing).
pub fn split_top_level(text: &str, sep: char) -> Vec<String> {
    let b = text.as_bytes();
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut start = 0;
    for (i, &c) in b.iter().enumerate() {
        match c {
            b'(' | b'[' | b'{' | b'<' => depth += 1,
            b')' | b']' | b'}' => depth -= 1,
            b'>' if i == 0 || (b[i - 1] != b'-' && b[i - 1] != b'=') => depth -= 1,
            _ if c == sep as u8 && depth == 0 => {
                parts.push(text[start..i].trim().to_string());
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(text[start..].trim().to_string());
    parts.retain(|p| !p.is_empty());
    parts
}

/// The bounds a generic parameter `name` is declared with, read from the
/// generics lists and `where` clauses of the items around it: each bound's
/// text, `?Sized` and lifetimes left out. None when no list declares `name`.
pub fn bounds_of(name: &str, items: &[Generics<'_>]) -> Option<Vec<String>> {
    let mut declared = false;
    let mut bounds = Vec::new();
    let take = |text: &str, bounds: &mut Vec<String>| {
        for b in split_top_level(text, '+') {
            let mut bound = b.as_str();
            if let Some(rest) = bound.strip_prefix("for") {
                let rest = rest.trim_start();
                if rest.starts_with('<') {
                    if let Some(end) = rest.find('>') {
                        bound = rest[end + 1..].trim();
                    }
                }
            }
            let bound = bound.trim();
            if bound.is_empty() || bound.starts_with('?') || bound.starts_with('\'') {
                continue;
            }
            bounds.push(bound.to_string());
        }
    };
    for item in items {
        for part in split_top_level(item.generics, ',') {
            let (head, rest) = split_ident(&part);
            if head != name {
                continue;
            }
            let rest = rest.trim_start();
            declared = true;
            if let Some(after) = rest.strip_prefix(':') {
                let bound = after.split_once('=').map_or(after, |(b, _)| b);
                take(bound, &mut bounds);
            }
        }
    }
    if !declared {
        return None;
    }
    for item in items {
        for part in split_top_level(item.where_, ',') {
            let (head, rest) = split_ident(&part);
            if head == name {
                if let Some(after) = rest.trim_start().strip_prefix(':') {
                    take(after, &mut bounds);
                }
            }
        }
    }
    Some(bounds)
}

/// The identifier a text starts with, and the rest.
fn split_ident(text: &str) -> (&str, &str) {
    let end = text
        .char_indices()
        .find(|&(i, c)| !(c.is_ascii_alphanumeric() || c == '_') || (i == 0 && c.is_ascii_digit()))
        .map_or(text.len(), |(i, _)| i);
    (&text[..end], &text[end..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blanks_comments_strings_and_chars() {
        let src = "let a = \"x{\"; // }\nlet c = '{'; let l: &'a str = r#\"}\"#; /* { */";
        let code = blank_rust_code(src);
        assert_eq!(code.len(), src.len());
        assert!(!code.contains('{') && !code.contains('}'), "{code}");
        assert!(code.contains("'a str"));
    }

    #[test]
    fn reads_bindings_in_scope() {
        let src = "fn f(srv: Server, mut n: u8) {\n  if let Some(srv) = spare() { srv.run(); }\n  srv.go();\n  for (k, v) in map { k.x(); }\n  let t: Thing = make();\n  match m { Cmd::Build(opts) => opts.build(), Pt { x, .. } => x.y() }\n  let c = |a: A| a.z();\n}\n";
        let s = Scopes::read(src);
        let at = |needle: &str| src.find(needle).unwrap();
        let (b, _) = s.binding_at("srv", at("srv.run")).unwrap();
        assert_eq!(b.kind, BindingKind::Cond);
        assert_eq!(b.site.as_ref().unwrap().path, "Some");
        let (b, _) = s.binding_at("srv", at("srv.go")).unwrap();
        assert_eq!(b.ty.as_deref(), Some("Server"));
        let (b, _) = s.binding_at("k", at("k.x")).unwrap();
        assert_eq!(b.kind, BindingKind::For);
        assert_eq!(b.site.as_ref().unwrap().index, Some(0));
        assert_eq!(b.scrutinee.as_deref(), Some("map"));
        let (b, _) = s.binding_at("t", at("match")).unwrap();
        assert_eq!(b.ty.as_deref(), Some("Thing"));
        let (b, _) = s.binding_at("opts", at("opts.build")).unwrap();
        assert_eq!(b.site.as_ref().unwrap().path, "Cmd::Build");
        let (b, _) = s.binding_at("x", at("x.y")).unwrap();
        assert_eq!(b.site.as_ref().unwrap().field.as_deref(), Some("x"));
        let (b, _) = s.binding_at("a", at("a.z")).unwrap();
        assert_eq!(b.kind, BindingKind::Closure);
        assert_eq!(b.ty.as_deref(), Some("A"));
        assert!(s.binding_at("t", at("let t")).is_none());
    }

    #[test]
    fn reads_fns_impls_and_bounds() {
        let src = "impl<S: Store> Worker<S> where S: Send {\n  fn run<K: Key + ?Sized>(&self) where K: Clone { }\n}\ntrait T: Sup { fn x(); }\n";
        let s = Scopes::read(src);
        assert_eq!(s.fns.len(), 1);
        assert_eq!(s.fns[0].generics, "K: Key + ?Sized");
        assert_eq!(s.impls.len(), 2);
        let f = &s.fns[0];
        let blk = s.impl_at(f.at).unwrap();
        assert_eq!(blk.generics, "S: Store");
        let items = [
            Generics { generics: &f.generics, where_: &f.where_ },
            Generics { generics: &blk.generics, where_: &blk.where_ },
        ];
        assert_eq!(bounds_of("K", &items), Some(vec!["Key".to_string(), "Clone".to_string()]));
        assert_eq!(bounds_of("S", &items), Some(vec!["Store".to_string(), "Send".to_string()]));
        assert_eq!(bounds_of("Z", &items), None);
    }
}
