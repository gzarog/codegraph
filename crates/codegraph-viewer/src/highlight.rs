//! Syntax highlighting for the source blocks: a single-pass lexer per
//! language family (comments, strings, numbers, keywords, type-like and
//! called identifiers), producing escaped HTML spans. Block comments and
//! multi-line strings carry over between lines.

use crate::vdom::escape_html;

struct Lang {
    line: &'static [&'static str],
    block: &'static [(&'static str, &'static str)],
    /// String delimiters; the bool marks ones that may span lines.
    strings: &'static [(&'static str, bool)],
    keywords: &'static [&'static str],
    case_insensitive: bool,
}

const C_LIKE_BLOCK: &[(&str, &str)] = &[("/*", "*/")];

const JS: Lang = Lang {
    line: &["//"],
    block: C_LIKE_BLOCK,
    strings: &[("`", true), ("\"", false), ("'", false)],
    keywords: &[
        "abstract",
        "as",
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "debugger",
        "declare",
        "default",
        "delete",
        "do",
        "else",
        "enum",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "from",
        "function",
        "get",
        "if",
        "implements",
        "import",
        "in",
        "instanceof",
        "interface",
        "is",
        "keyof",
        "let",
        "namespace",
        "new",
        "null",
        "of",
        "private",
        "protected",
        "public",
        "readonly",
        "return",
        "set",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "type",
        "typeof",
        "undefined",
        "var",
        "void",
        "while",
        "with",
        "yield",
    ],
    case_insensitive: false,
};

const PY: Lang = Lang {
    line: &["#"],
    block: &[],
    strings: &[("\"\"\"", true), ("'''", true), ("\"", false), ("'", false)],
    keywords: &[
        "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class",
        "continue", "def", "del", "elif", "else", "except", "finally", "for", "from", "global",
        "if", "import", "in", "is", "lambda", "match", "case", "nonlocal", "not", "or", "pass",
        "raise", "return", "self", "try", "while", "with", "yield",
    ],
    case_insensitive: false,
};

const RUST: Lang = Lang {
    line: &["//"],
    block: C_LIKE_BLOCK,
    strings: &[("\"", true)],
    keywords: &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ],
    case_insensitive: false,
};

const GO: Lang = Lang {
    line: &["//"],
    block: C_LIKE_BLOCK,
    strings: &[("`", true), ("\"", false), ("'", false)],
    keywords: &[
        "break",
        "case",
        "chan",
        "const",
        "continue",
        "default",
        "defer",
        "else",
        "fallthrough",
        "false",
        "for",
        "func",
        "go",
        "goto",
        "if",
        "import",
        "interface",
        "map",
        "nil",
        "package",
        "range",
        "return",
        "select",
        "struct",
        "switch",
        "true",
        "type",
        "var",
    ],
    case_insensitive: false,
};

const JVM: Lang = Lang {
    line: &["//"],
    block: C_LIKE_BLOCK,
    strings: &[("\"\"\"", true), ("\"", false), ("'", false)],
    keywords: &[
        "abstract",
        "async",
        "await",
        "base",
        "boolean",
        "break",
        "case",
        "catch",
        "class",
        "companion",
        "const",
        "continue",
        "data",
        "def",
        "default",
        "do",
        "else",
        "enum",
        "extends",
        "extension",
        "false",
        "final",
        "finally",
        "for",
        "fun",
        "func",
        "guard",
        "if",
        "implements",
        "import",
        "in",
        "init",
        "interface",
        "internal",
        "is",
        "let",
        "namespace",
        "new",
        "nil",
        "null",
        "object",
        "open",
        "out",
        "override",
        "package",
        "private",
        "protected",
        "public",
        "readonly",
        "record",
        "return",
        "sealed",
        "static",
        "struct",
        "super",
        "switch",
        "this",
        "throw",
        "throws",
        "trait",
        "true",
        "try",
        "using",
        "val",
        "var",
        "virtual",
        "void",
        "when",
        "where",
        "while",
        "with",
        "yield",
        "late",
        "required",
    ],
    case_insensitive: false,
};

const C: Lang = Lang {
    line: &["//"],
    block: C_LIKE_BLOCK,
    strings: &[("\"", false), ("'", false)],
    keywords: &[
        "auto",
        "bool",
        "break",
        "case",
        "char",
        "class",
        "const",
        "constexpr",
        "continue",
        "default",
        "delete",
        "do",
        "double",
        "else",
        "enum",
        "extern",
        "false",
        "float",
        "for",
        "goto",
        "if",
        "inline",
        "int",
        "long",
        "namespace",
        "new",
        "nullptr",
        "private",
        "protected",
        "public",
        "return",
        "short",
        "signed",
        "sizeof",
        "static",
        "struct",
        "switch",
        "template",
        "this",
        "true",
        "typedef",
        "typename",
        "union",
        "unsigned",
        "using",
        "virtual",
        "void",
        "volatile",
        "while",
        "#include",
        "#define",
        "#if",
        "#ifdef",
        "#endif",
        "@interface",
        "@implementation",
        "@end",
        "@property",
        "self",
    ],
    case_insensitive: false,
};

const RUBY: Lang = Lang {
    line: &["#"],
    block: &[("=begin", "=end")],
    strings: &[("\"", false), ("'", false)],
    keywords: &[
        "alias",
        "and",
        "begin",
        "break",
        "case",
        "class",
        "def",
        "defined?",
        "do",
        "else",
        "elsif",
        "end",
        "ensure",
        "false",
        "for",
        "if",
        "in",
        "module",
        "next",
        "nil",
        "not",
        "or",
        "redo",
        "rescue",
        "retry",
        "return",
        "self",
        "super",
        "then",
        "true",
        "undef",
        "unless",
        "until",
        "when",
        "while",
        "yield",
        "require",
        "attr_reader",
        "attr_accessor",
    ],
    case_insensitive: false,
};

const PHP: Lang = Lang {
    line: &["//", "#"],
    block: C_LIKE_BLOCK,
    strings: &[("\"", false), ("'", false)],
    keywords: &[
        "abstract",
        "array",
        "as",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "echo",
        "else",
        "elseif",
        "extends",
        "false",
        "final",
        "finally",
        "fn",
        "for",
        "foreach",
        "function",
        "if",
        "implements",
        "interface",
        "namespace",
        "new",
        "null",
        "private",
        "protected",
        "public",
        "return",
        "static",
        "switch",
        "throw",
        "trait",
        "true",
        "try",
        "use",
        "while",
    ],
    case_insensitive: true,
};

const LUA: Lang = Lang {
    line: &["--"],
    block: &[("--[[", "]]")],
    strings: &[("\"", false), ("'", false)],
    keywords: &[
        "and", "break", "do", "else", "elseif", "end", "false", "for", "function", "if", "in",
        "local", "nil", "not", "or", "repeat", "return", "then", "true", "until", "while",
    ],
    case_insensitive: false,
};

const SQLISH: Lang = Lang {
    line: &["--", "#", "%", "'"],
    block: &[("{", "}"), ("(*", "*)")],
    strings: &[("\"", false)],
    keywords: &[
        "begin",
        "end",
        "procedure",
        "function",
        "program",
        "unit",
        "uses",
        "var",
        "const",
        "type",
        "class",
        "record",
        "if",
        "then",
        "else",
        "while",
        "do",
        "for",
        "to",
        "return",
        "module",
        "sub",
        "public",
        "private",
        "dim",
        "as",
        "new",
        "imports",
        "inherits",
        "perform",
        "call",
        "move",
        "display",
        "division",
        "section",
        "resource",
        "variable",
        "output",
        "data",
        "locals",
        "let",
        "in",
        "with",
        "import",
        "true",
        "false",
        "null",
    ],
    case_insensitive: true,
};

const SHELL: Lang = Lang {
    line: &["#"],
    block: &[],
    strings: &[("\"", false), ("'", false)],
    keywords: &[
        "if",
        "then",
        "else",
        "fi",
        "for",
        "in",
        "do",
        "done",
        "case",
        "esac",
        "export",
        "echo",
        "cd",
        "set",
        "irm",
        "iex",
        "curl",
        "sh",
        "cargo",
        "codegraph",
        "npm",
        "rustup",
        "git",
    ],
    case_insensitive: false,
};

const DATA: Lang = Lang {
    line: &["#"],
    block: &[],
    strings: &[("\"", false), ("'", false)],
    keywords: &["true", "false", "null", "yes", "no"],
    case_insensitive: false,
};

fn lang_for(language: &str) -> &'static Lang {
    match language {
        "bash" | "sh" | "shell" | "console" | "powershell" | "ps1" => &SHELL,
        "json" | "jsonc" | "toml" | "yaml" | "yml" | "text" | "txt" | "" => &DATA,
        "typescript" | "tsx" | "javascript" | "jsx" | "svelte" | "vue" | "astro" | "arkts" => &JS,
        "python" => &PY,
        "rust" => &RUST,
        "go" => &GO,
        "java" | "kotlin" | "scala" | "csharp" | "swift" | "dart" | "solidity" | "cfml" => &JVM,
        "c" | "cpp" | "objc" => &C,
        "ruby" => &RUBY,
        "php" => &PHP,
        "lua" | "luau" => &LUA,
        _ => &SQLISH,
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Code,
    Block(usize),
    Str(usize),
}

fn span(out: &mut String, class: &str, text: &str) {
    if text.is_empty() {
        return;
    }
    if class.is_empty() {
        out.push_str(&escape_html(text));
    } else {
        out.push_str("<span class=\"");
        out.push_str(class);
        out.push_str("\">");
        out.push_str(&escape_html(text));
        out.push_str("</span>");
    }
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// One line as highlighted HTML; `state` carries over to the next line.
fn line_html(line: &str, lang: &Lang, state: &mut State) -> String {
    let mut out = String::with_capacity(line.len() * 2);
    let mut rest = line;
    loop {
        match *state {
            State::Block(i) => {
                let close = lang.block[i].1;
                match rest.find(close) {
                    Some(p) => {
                        span(&mut out, "c", &rest[..p + close.len()]);
                        rest = &rest[p + close.len()..];
                        *state = State::Code;
                    }
                    None => {
                        span(&mut out, "c", rest);
                        return out;
                    }
                }
            }
            State::Str(i) => {
                let delim = lang.strings[i].0;
                let mut end = None;
                let mut chars = rest.char_indices();
                while let Some((p, c)) = chars.next() {
                    if c == '\\' {
                        chars.next();
                    } else if rest[p..].starts_with(delim) {
                        end = Some(p + delim.len());
                        break;
                    }
                }
                match end {
                    Some(e) => {
                        span(&mut out, "s", &rest[..e]);
                        rest = &rest[e..];
                        *state = State::Code;
                    }
                    None => {
                        span(&mut out, "s", rest);
                        // Single-line strings end with the line.
                        if !lang.strings[i].1 {
                            *state = State::Code;
                        }
                        return out;
                    }
                }
            }
            State::Code => {
                if rest.is_empty() {
                    return out;
                }
                if lang.line.iter().any(|l| rest.starts_with(l)) {
                    span(&mut out, "c", rest);
                    return out;
                }
                if let Some(i) = lang.block.iter().position(|(o, _)| rest.starts_with(o)) {
                    let open = lang.block[i].0;
                    span(&mut out, "c", open);
                    rest = &rest[open.len()..];
                    *state = State::Block(i);
                    continue;
                }
                if let Some(i) = lang.strings.iter().position(|(d, _)| rest.starts_with(d)) {
                    let d = lang.strings[i].0;
                    span(&mut out, "s", d);
                    rest = &rest[d.len()..];
                    *state = State::Str(i);
                    continue;
                }
                let c = rest.chars().next().unwrap_or(' ');
                if c.is_ascii_digit() {
                    let end = rest
                        .find(|ch: char| !(ch.is_ascii_alphanumeric() || ch == '.' || ch == '_'))
                        .unwrap_or(rest.len());
                    span(&mut out, "n", &rest[..end]);
                    rest = &rest[end..];
                } else if is_ident(c) || matches!(c, '#' | '@') {
                    let end = rest
                        .char_indices()
                        .skip(1)
                        .find(|(_, ch)| !is_ident(*ch) && *ch != '?')
                        .map_or(rest.len(), |(p, _)| p);
                    let word = &rest[..end];
                    let after = rest[end..].trim_start();
                    let keyword = lang.keywords.iter().any(|k| {
                        if lang.case_insensitive {
                            k.eq_ignore_ascii_case(word)
                        } else {
                            *k == word
                        }
                    });
                    let class = if keyword {
                        "k"
                    } else if after.starts_with('(') {
                        "f"
                    } else if word.starts_with(|ch: char| ch.is_uppercase())
                        && word.chars().any(|ch| ch.is_lowercase())
                    {
                        "t"
                    } else {
                        ""
                    };
                    span(&mut out, class, word);
                    rest = &rest[end..];
                } else {
                    let end = c.len_utf8();
                    span(
                        &mut out,
                        if c.is_ascii_punctuation() { "p" } else { "" },
                        &rest[..end],
                    );
                    rest = &rest[end..];
                }
            }
        }
    }
}

/// A code block: each line a row with its number, `mark` lines highlighted.
pub fn block_html(lines: &[String], first_line: usize, language: &str, mark: &[usize]) -> String {
    let lang = lang_for(language);
    let mut state = State::Code;
    let mut out = String::new();
    for (i, line) in lines.iter().enumerate() {
        let n = first_line + i;
        let body = line_html(line, lang, &mut state);
        let class = if mark.contains(&n) { "ln mark" } else { "ln" };
        out.push_str(&format!(
            "<div class=\"{class}\" id=\"L{n}\"><span class=\"no\">{n}</span><span class=\"tx\">{body}</span></div>"
        ));
    }
    out
}

/// Highlighted HTML for a snippet, one string per line (no numbers).
pub fn snippet_html(code: &str, language: &str) -> Vec<String> {
    let lang = lang_for(language);
    let mut state = State::Code;
    code.lines()
        .map(|l| line_html(l, lang, &mut state))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn highlights_and_carries_state() {
        let mut st = State::Code;
        let h = line_html("const x = foo(\"a<b\"); // done", &JS, &mut st);
        assert_eq!(
            h,
            "<span class=\"k\">const</span> x <span class=\"p\">=</span> <span class=\"f\">foo</span><span class=\"p\">(</span><span class=\"s\">&quot;</span><span class=\"s\">a&lt;b&quot;</span><span class=\"p\">)</span><span class=\"p\">;</span> <span class=\"c\">// done</span>"
        );
        let mut st = State::Code;
        line_html("/* open", &JS, &mut st);
        assert_eq!(st, State::Block(0));
        let h = line_html("still */ let", &JS, &mut st);
        assert!(h.starts_with("<span class=\"c\">still */</span>"));
        assert_eq!(st, State::Code);
        let mut st = State::Code;
        line_html("s = \"\"\"doc", &PY, &mut st);
        assert_eq!(st, State::Str(0));
    }
}
