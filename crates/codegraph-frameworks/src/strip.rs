//! Blank out comments so route regexes don't match commented-out code.
//! String contents, line breaks and byte offsets are preserved.

use codegraph_core::Language;

#[derive(Clone, Copy)]
struct Syntax {
    line: &'static [&'static str],
    block: Option<(&'static str, &'static str)>,
    /// Quote characters that open strings.
    quotes: &'static [u8],
    triple: bool,
}

fn syntax(lang: Language) -> Syntax {
    use Language::*;
    const C_LIKE: Syntax = Syntax {
        line: &["//"],
        block: Some(("/*", "*/")),
        quotes: b"\"'`",
        triple: false,
    };
    match lang {
        Python => Syntax {
            line: &["#"],
            block: None,
            quotes: b"\"'",
            triple: true,
        },
        Ruby | Yaml | Nix | R | Terraform | Properties => Syntax {
            line: &["#"],
            block: None,
            quotes: b"\"'",
            triple: false,
        },
        Php => Syntax {
            line: &["//", "#"],
            block: Some(("/*", "*/")),
            quotes: b"\"'",
            triple: false,
        },
        Lua | Luau => Syntax {
            line: &["--"],
            block: None,
            quotes: b"\"'",
            triple: false,
        },
        Rust | Go | Java | Kotlin | Scala | CSharp | Swift | Dart | C | Cpp | ObjC => Syntax {
            line: &["//"],
            block: Some(("/*", "*/")),
            quotes: b"\"",
            triple: false,
        },
        _ => C_LIKE,
    }
}

/// `content` with comments replaced by spaces (newlines kept).
pub fn strip_comments(content: &str, lang: Language) -> String {
    let syn = syntax(lang);
    let b = content.as_bytes();
    let mut out = b.to_vec();
    let mut i = 0;
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for byte in &mut out[from..to] {
            if *byte != b'\n' {
                *byte = b' ';
            }
        }
    };
    while i < b.len() {
        let c = b[i];
        if syn.quotes.contains(&c) {
            // Triple-quoted (Python) strings.
            if syn.triple && b[i..].starts_with(&[c, c, c]) {
                let close = [c, c, c];
                let mut j = i + 3;
                while j < b.len() && !b[j..].starts_with(&close) {
                    j += 1;
                }
                i = (j + 3).min(b.len());
                continue;
            }
            let mut j = i + 1;
            while j < b.len() && b[j] != c {
                if b[j] == b'\\' {
                    j += 1;
                } else if b[j] == b'\n' && c != b'`' {
                    break;
                }
                j += 1;
            }
            i = (j + 1).min(b.len());
            continue;
        }
        if let Some(lc) = syn.line.iter().find(|l| b[i..].starts_with(l.as_bytes())) {
            // `://` in URLs inside unquoted contexts is rare; `#` in PHP 8 attributes is not a comment.
            if *lc == "#" && lang == Language::Php && b.get(i + 1) == Some(&b'[') {
                i += 1;
                continue;
            }
            let end = b[i..]
                .iter()
                .position(|&x| x == b'\n')
                .map(|p| i + p)
                .unwrap_or(b.len());
            blank(&mut out, i, end);
            i = end;
            continue;
        }
        if let Some((open, close)) = syn.block {
            if b[i..].starts_with(open.as_bytes()) {
                let end = b[i + open.len()..]
                    .windows(close.len())
                    .position(|w| w == close.as_bytes())
                    .map(|p| i + open.len() + p + close.len())
                    .unwrap_or(b.len());
                blank(&mut out, i, end);
                i = end;
                continue;
            }
        }
        i += 1;
    }
    // Only ASCII bytes inside comments were replaced, so this stays valid UTF-8
    // except where a comment held multi-byte text; blank those bytes too.
    String::from_utf8(out).unwrap_or_else(|e| {
        let mut bytes = e.into_bytes();
        for x in &mut bytes {
            if *x >= 0x80 {
                *x = b' ';
            }
        }
        String::from_utf8(bytes).unwrap_or_default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_but_keeps_strings_and_lines() {
        let s = strip_comments(
            "a // app.get('/x')\nb = \"//not\" /* c\nd */ e",
            Language::TypeScript,
        );
        assert_eq!(s.lines().count(), 3);
        assert!(!s.contains("app.get"));
        assert!(s.contains("\"//not\""));
        assert!(s.ends_with(" e"));
        let p = strip_comments("x = 1  # @app.route('/a')\ns = '#keep'", Language::Python);
        assert!(!p.contains("@app"));
        assert!(p.contains("'#keep'"));
    }
}
