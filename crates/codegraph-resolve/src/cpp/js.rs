//! What upstream's TypeScript leans on from JavaScript, for the C/C++ port:
//! regexes with lookaround and backreferences (`fancy-regex`), searched from
//! an offset as `lastIndex` does, `String.replace` with a callback, and
//! JavaScript's ways of cutting text. Offsets are bytes.

use fancy_regex::Regex;

pub type Captures<'t> = fancy_regex::Captures<'t, str>;

/// The first match at or after `pos`; lookbehind sees what lies before.
pub fn caps_at<'t>(re: &Regex, text: &'t str, pos: usize) -> Option<Captures<'t>> {
    if pos > text.len() || !text.is_char_boundary(pos) {
        return None;
    }
    re.captures_from_pos(text, pos).ok().flatten()
}

pub fn caps<'t>(re: &Regex, text: &'t str) -> Option<Captures<'t>> {
    caps_at(re, text, 0)
}

/// `re.test(text)`.
pub fn test(re: &Regex, text: &str) -> bool {
    re.is_match(text).unwrap_or(false)
}

/// `text.search(re)`: where the first match starts.
pub fn search(re: &Regex, text: &str) -> Option<usize> {
    re.find(text).ok().flatten().map(|m| m.start())
}

/// Every match, in order, as JavaScript's `matchAll` finds them.
pub fn all<'t>(re: &Regex, text: &'t str) -> Vec<Captures<'t>> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(c) = caps_at(re, text, pos) {
        let m = c.get(0).unwrap();
        pos = next_pos(text, m.start(), m.end());
        out.push(c);
    }
    out
}

/// Where a global search resumes after the match `[start, end)`.
pub fn next_pos(text: &str, start: usize, end: usize) -> usize {
    if end > start {
        end
    } else {
        end + text[end..].chars().next().map_or(1, char::len_utf8)
    }
}

/// `text.replace(re, f)` for a global regex.
pub fn replace_all(re: &Regex, text: &str, mut f: impl FnMut(&Captures) -> String) -> String {
    let mut out = String::new();
    let mut last = 0;
    let mut pos = 0;
    let mut any = false;
    while let Some(c) = caps_at(re, text, pos) {
        let m = c.get(0).unwrap();
        out.push_str(&text[last..m.start()]);
        out.push_str(&f(&c));
        last = m.end();
        any = true;
        pos = next_pos(text, m.start(), m.end());
    }
    if !any {
        return text.to_string();
    }
    out.push_str(&text[last..]);
    out
}

/// `text.replace(re, with)` for a global regex and a literal replacement.
pub fn replace_with(re: &Regex, text: &str, with: &str) -> String {
    replace_all(re, text, |_| with.to_string())
}

/// `text.replace(re, f)` for a regex without the `g` flag: the first match only.
pub fn replace_first(re: &Regex, text: &str, f: impl FnOnce(&Captures) -> String) -> String {
    match caps(re, text) {
        Some(c) => {
            let m = c.get(0).unwrap();
            format!("{}{}{}", &text[..m.start()], f(&c), &text[m.end()..])
        }
        None => text.to_string(),
    }
}

/// A capture group's text, or "" when it took no part.
pub fn group<'t>(c: &Captures<'t>, i: usize) -> &'t str {
    c.get(i).map_or("", |m| m.as_str())
}

pub fn group_opt<'t>(c: &Captures<'t>, i: usize) -> Option<&'t str> {
    c.get(i).map(|m| m.as_str())
}

/// `s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')`.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if ".*+?^${}()|[]\\".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// `text.split(/\r?\n/)`.
pub fn split_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l).to_string())
        .collect()
}

/// The byte at `i`, or 0 past the end (`text[i]` reads `undefined`).
pub fn at(text: &str, i: usize) -> u8 {
    text.as_bytes().get(i).copied().unwrap_or(0)
}

/// The byte before `i`, or 0 at the start.
pub fn before(text: &str, i: usize) -> u8 {
    if i == 0 {
        0
    } else {
        at(text, i - 1)
    }
}

pub fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

pub fn is_space(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c)
}

/// The largest char boundary at or below `i`.
pub fn floor(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// `s.slice(from, to)` on byte offsets, clamped to the text and its char boundaries.
pub fn slice(s: &str, from: usize, to: usize) -> &str {
    let to = floor(s, to);
    let from = floor(s, from).min(to);
    &s[from..to]
}

/// `s.slice(from)`.
pub fn slice_from(s: &str, from: usize) -> &str {
    &s[floor(s, from)..]
}

/// `s.indexOf(pat, from)`.
pub fn index_of(s: &str, pat: &str, from: usize) -> Option<usize> {
    let from = floor(s, from);
    s.get(from..).and_then(|r| r.find(pat)).map(|k| from + k)
}

/// `s.startsWith(pat, at)`.
pub fn starts_at(s: &str, at: usize, pat: &str) -> bool {
    s.get(at..).is_some_and(|r| r.starts_with(pat))
}

/// `/^[A-Za-z_]\w*$/.test(s)`.
pub fn is_ident(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty() && (b[0].is_ascii_alphabetic() || b[0] == b'_') && b.iter().all(|&c| is_word(c))
}

/// `/^\w+$/.test(s)`.
pub fn is_word_str(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(is_word)
}

/// `s.replace(/\s+/g, '')`.
pub fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// `s.replace(/\s+/g, ' ')`.
pub fn collapse_ws(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_ws = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !in_ws {
                out.push(' ');
            }
            in_ws = true;
        } else {
            out.push(c);
            in_ws = false;
        }
    }
    out
}

/// The identifier starting at `code[i]` (`/[A-Za-z_]\w*/y`), or None.
pub fn word_at(code: &str, i: usize) -> Option<&str> {
    let b = code.as_bytes();
    if i >= b.len() || !(b[i].is_ascii_alphabetic() || b[i] == b'_') {
        return None;
    }
    let mut j = i + 1;
    while j < b.len() && is_word(b[j]) {
        j += 1;
    }
    Some(&code[i..j])
}

/// Past the whitespace at `code[i]`.
pub fn skip_space(code: &str, mut i: usize) -> usize {
    while i < code.len() && is_space(at(code, i)) {
        i += 1;
    }
    i
}

/// `s.lastIndexOf(pat)`.
pub fn last_index_of(s: &str, pat: &str) -> Option<usize> {
    s.rfind(pat)
}

/// The text after the last `::`, or all of it.
pub fn last_segment(s: &str) -> &str {
    s.rsplit("::").next().unwrap_or(s)
}

/// `name.split(/::|\.|->/).pop()`: the last member a reference name spells.
pub fn split_last_member(name: &str) -> &str {
    let mut start = 0;
    let b = name.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if (b[i] == b':' && b.get(i + 1) == Some(&b':'))
            || (b[i] == b'-' && b.get(i + 1) == Some(&b'>'))
        {
            start = i + 2;
            i += 2;
        } else if b[i] == b'.' {
            start = i + 1;
            i += 1;
        } else {
            i += 1;
        }
    }
    &name[start..]
}

/// `text.trim()` as JavaScript trims: Unicode whitespace.
pub fn trim(s: &str) -> &str {
    s.trim()
}

/// strip-comments.ts `stripCommentsForRegex(src, 'cpp')` (stripCStyle with no
/// single-quoted strings): comments become spaces, newlines kept; string
/// contents stay.
pub fn strip_comments_cpp(src: &str) -> String {
    let b = src.as_bytes();
    let n = b.len();
    let mut out = b.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for (k, byte) in out.iter_mut().enumerate().take(to).skip(from) {
            *byte = if b[k] == b'\n' { b'\n' } else { b' ' };
        }
    };
    let mut i = 0;
    while i < n {
        let c = b[i];
        let c2 = b.get(i + 1).copied().unwrap_or(0);
        if c == b'/' && c2 == b'*' {
            let start = i;
            i += 2;
            while i < n && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                i += 1;
            }
            if i < n {
                i += 2;
            }
            blank(&mut out, start, i.min(n));
            continue;
        }
        if c == b'/' && c2 == b'/' {
            let start = i;
            while i < n && b[i] != b'\n' {
                i += 1;
            }
            blank(&mut out, start, i);
            continue;
        }
        if c == b'"' || c == b'`' {
            let quote = c;
            i += 1;
            while i < n && b[i] != quote {
                if b[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if quote != b'`' && b[i] == b'\n' {
                    break;
                }
                i += 1;
            }
            if i < n && b[i] == quote {
                i += 1;
            }
            continue;
        }
        i += 1;
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}
