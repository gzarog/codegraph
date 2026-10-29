//! C and C++ string literals and block comments as text, not code, for the
//! scans that read declarations off source lines (upstream `cpp-raw-strings.ts`).

use super::ctx::{memo, Ctx};
use std::collections::HashMap;
use std::rc::Rc;

/// Column ranges `[start, end)` a raw string or block comment takes on a line.
pub type Ranges = Vec<(usize, usize)>;
pub type NonCode = Rc<HashMap<usize, Ranges>>;

fn is_word_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

/// The 1-based lines of a C or C++ file that raw string literals or block
/// comments cover, each with the columns they take.
pub fn non_code_lines(ctx: &Ctx, file: &str) -> NonCode {
    ctx.memo(memo::NON_CODE_LINES, file.to_string(), || {
        let mut lines: HashMap<usize, Ranges> = HashMap::new();
        if let Some(src) = ctx.source(file) {
            let text = src.text.as_bytes();
            if src.text.contains("R\"") || src.text.contains("/*") {
                scan(text, &mut lines);
            }
        }
        Rc::new(lines)
    })
}

fn scan(text: &[u8], lines: &mut HashMap<usize, Ranges>) {
    let n = text.len();
    let mut line = 1usize;
    let mut column = 0usize;
    let mut i = 0usize;
    let mut mark = |from: usize, from_line: usize, from_column: usize, to: usize| {
        let mut l = from_line;
        let mut start = from_column;
        let mut c = from_column;
        for &b in &text[from..to.min(n)] {
            if b == b'\n' {
                if c > start {
                    lines.entry(l).or_default().push((start, c));
                }
                l += 1;
                start = 0;
                c = 0;
            } else if b != b'\r' {
                c += 1;
            }
        }
        if c > start {
            lines.entry(l).or_default().push((start, c));
        }
    };
    let advance = |i: &mut usize, line: &mut usize, column: &mut usize, to: usize| {
        while *i < to {
            if text[*i] == b'\n' {
                *line += 1;
                *column = 0;
            } else if text[*i] != b'\r' {
                *column += 1;
            }
            *i += 1;
        }
    };
    let find = |from: usize, pat: &[u8]| -> Option<usize> {
        if from >= n {
            return None;
        }
        text[from..]
            .windows(pat.len())
            .position(|w| w == pat)
            .map(|k| from + k)
    };
    while i < n {
        let c = text[i];
        let next = text.get(i + 1).copied().unwrap_or(0);
        if c == b'/' && next == b'/' {
            let end = find(i, b"\n").unwrap_or(n);
            advance(&mut i, &mut line, &mut column, end);
        } else if c == b'/' && next == b'*' {
            let to = find(i + 2, b"*/").map_or(n, |e| e + 2);
            mark(i, line, column, to);
            advance(&mut i, &mut line, &mut column, to);
        } else if c == b'"' {
            // `R"`, `u8R"`, `uR"`, `UR"`, `LR"`, not part of a longer name.
            let lo = i.saturating_sub(3);
            let before = &text[lo..i];
            let prefix_len = if before.ends_with(b"u8R") {
                3
            } else if before.ends_with(b"uR") || before.ends_with(b"UR") || before.ends_with(b"LR")
            {
                2
            } else if before.ends_with(b"R") {
                1
            } else {
                0
            };
            let mut raw_end = None;
            if prefix_len > 0 {
                let pre = i - prefix_len;
                let standalone = pre == 0 || !is_word_char(text[pre - 1]);
                if standalone {
                    if let Some(open) = find(i + 1, b"(") {
                        let delimiter = &text[i + 1..open];
                        if delimiter.len() <= 16
                            && delimiter.iter().all(|&b| {
                                !b.is_ascii_whitespace() && !matches!(b, b'(' | b')' | b'\\')
                            })
                        {
                            let mut closer = vec![b')'];
                            closer.extend_from_slice(delimiter);
                            closer.push(b'"');
                            raw_end = Some(
                                find(open + 1, &closer).map_or(n, |e| e + delimiter.len() + 2),
                            );
                        }
                    }
                }
            }
            match raw_end {
                Some(end) => {
                    mark(i, line, column, end);
                    advance(&mut i, &mut line, &mut column, end);
                }
                None => {
                    let mut k = i + 1;
                    while k < n && text[k] != b'"' && text[k] != b'\n' {
                        k += if text[k] == b'\\' { 2 } else { 1 };
                    }
                    let to = if k < n && text[k] == b'"' { k + 1 } else { k };
                    advance(&mut i, &mut line, &mut column, to.min(n));
                }
            }
        } else if c == b'\'' && !(i > 0 && is_word_char(text[i - 1])) {
            let mut k = i + 1;
            while k < n && text[k] != b'\'' && text[k] != b'\n' {
                k += if text[k] == b'\\' { 2 } else { 1 };
            }
            let to = if k < n && text[k] == b'\'' { k + 1 } else { k };
            advance(&mut i, &mut line, &mut column, to.min(n));
        } else {
            let to = i + 1;
            advance(&mut i, &mut line, &mut column, to);
        }
    }
}

/// Line `line` (1-based) of a C or C++ file with the contents of its string
/// and character literals blanked, raw strings included, columns kept.
pub fn literals_blanked(text: &str, line: usize, raw_lines: &HashMap<usize, Ranges>) -> String {
    let raw = raw_lines.get(&line);
    let mut out = text.as_bytes().to_vec();
    if let Some(raw) = raw {
        for &(start, end) in raw {
            for b in out.iter_mut().take(end).skip(start) {
                *b = b' ';
            }
        }
    }
    if !text.contains('"') && !text.contains('\'') {
        return bytes_to_string(out);
    }
    let t = text.as_bytes();
    let in_raw = |k: usize| raw.is_some_and(|r| r.iter().any(|&(s, e)| k >= s && k < e));
    let mut k = 0;
    while k < t.len() {
        let c = t[k];
        if in_raw(k) {
            k += 1;
            continue;
        }
        if c == b'/' && t.get(k + 1) == Some(&b'/') {
            break;
        }
        if c == b'/' && t.get(k + 1) == Some(&b'*') {
            match text[k + 2..].find("*/") {
                Some(close) => {
                    k = k + 2 + close + 2;
                    continue;
                }
                None => break,
            }
        }
        if c != b'"' && !(c == b'\'' && !(k > 0 && is_word_char(t[k - 1]))) {
            k += 1;
            continue;
        }
        let mut end = k + 1;
        while end < t.len() && t[end] != c {
            end += if t[end] == b'\\' { 2 } else { 1 };
        }
        for b in out.iter_mut().take(end.min(t.len())).skip(k + 1) {
            *b = b' ';
        }
        k = end + 1;
    }
    bytes_to_string(out)
}

fn bytes_to_string(out: Vec<u8>) -> String {
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}
