//! Source languages and file → language detection.

use crate::string_enum;
use regex::Regex;
use std::sync::LazyLock;

string_enum!(
    /// A language CodeGraph can index.
    Language {
        TypeScript => "typescript",
        JavaScript => "javascript",
        Tsx => "tsx",
        Jsx => "jsx",
        ArkTs => "arkts",
        Python => "python",
        Go => "go",
        Rust => "rust",
        Java => "java",
        C => "c",
        Cpp => "cpp",
        CSharp => "csharp",
        Razor => "razor",
        Php => "php",
        Ruby => "ruby",
        Swift => "swift",
        Kotlin => "kotlin",
        Dart => "dart",
        Svelte => "svelte",
        Vue => "vue",
        Astro => "astro",
        Liquid => "liquid",
        Pascal => "pascal",
        Scala => "scala",
        Lua => "lua",
        Luau => "luau",
        ObjC => "objc",
        R => "r",
        Solidity => "solidity",
        Nix => "nix",
        Yaml => "yaml",
        Twig => "twig",
        Xml => "xml",
        Properties => "properties",
        Cfml => "cfml",
        CfScript => "cfscript",
        CfQuery => "cfquery",
        Cobol => "cobol",
        VbNet => "vbnet",
        Erlang => "erlang",
        Terraform => "terraform",
        Unknown => "unknown",
    }
);

impl Language {
    /// TypeScript/JavaScript family (shares import semantics).
    pub fn is_js_family(self) -> bool {
        matches!(
            self,
            Language::TypeScript
                | Language::JavaScript
                | Language::Tsx
                | Language::Jsx
                | Language::ArkTs
        )
    }
}

/// File extension (lowercase, with the dot) → language.
pub fn language_for_extension(ext: &str) -> Option<Language> {
    use Language::*;
    Some(match ext {
        ".ts" | ".mts" | ".cts" => TypeScript,
        ".tsx" => Tsx,
        ".ets" => ArkTs,
        ".js" | ".mjs" | ".cjs" | ".xsjs" | ".xsjslib" => JavaScript,
        ".jsx" => Jsx,
        ".py" | ".pyw" => Python,
        ".go" => Go,
        ".rs" => Rust,
        ".java" => Java,
        ".c" | ".h" => C,
        ".cpp" | ".cc" | ".cxx" | ".hpp" | ".hxx" | ".metal" | ".cu" | ".cuh" => Cpp,
        ".cs" => CSharp,
        ".cshtml" | ".razor" => Razor,
        ".php" | ".module" | ".install" | ".theme" | ".inc" => Php,
        ".yml" | ".yaml" => Yaml,
        ".twig" => Twig,
        ".rb" | ".rake" => Ruby,
        ".swift" => Swift,
        ".kt" | ".kts" => Kotlin,
        ".dart" => Dart,
        ".liquid" => Liquid,
        ".svelte" => Svelte,
        ".vue" => Vue,
        ".astro" => Astro,
        ".r" => R,
        ".pas" | ".dpr" | ".dpk" | ".lpr" | ".dfm" | ".fmx" => Pascal,
        ".scala" | ".sc" => Scala,
        ".lua" => Lua,
        ".luau" => Luau,
        ".m" | ".mm" => ObjC,
        ".sol" => Solidity,
        ".cfc" | ".cfm" => Cfml,
        ".cfs" => CfScript,
        ".nix" => Nix,
        ".xml" => Xml,
        ".cbl" | ".cob" | ".cobol" | ".cpy" => Cobol,
        ".vb" => VbNet,
        ".erl" | ".hrl" | ".escript" => Erlang,
        ".properties" => Properties,
        ".tf" | ".tfvars" | ".tofu" => Terraform,
        _ => return None,
    })
}

fn extension(path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    match base.rfind('.') {
        Some(i) => base[i..].to_ascii_lowercase(),
        None => String::new(),
    }
}

/// Play Framework `conf/routes` (and included `*.routes`).
pub fn is_play_routes_file(path: &str) -> bool {
    path == "conf/routes" || path.ends_with("/conf/routes") || path.ends_with(".routes")
}

/// OTP `.app` / `.app.src` resource files.
pub fn is_erlang_app_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with(".app") || lower.ends_with(".app.src")
}

/// Whether `path` (project-relative, `/`-separated) is a file CodeGraph indexes.
pub fn is_source_file(path: &str) -> bool {
    is_play_routes_file(path)
        || is_erlang_app_file(path)
        || language_for_extension(&extension(path)).is_some()
}

static FLOW_LEAD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\s*(?://[^\n]*|/\*(?s:.)*?\*/))*").unwrap());
static FLOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@flow\b").unwrap());
static NOFLOW: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"@noflow\b").unwrap());
static CPP_SIGNAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\bnamespace\b|\bclass\s+\w+\s*[:{]|\b(?:class|struct)\s+[A-Z][A-Z0-9_]+\s+\w+\s*(?:final\s*)?[:{]|\btemplate\s*<|\b(?:public|private|protected)\s*:|\bvirtual\b|\busing\s+(?:namespace\b|\w+\s*=)").unwrap()
});
static CPP_BASE_CLAUSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:class|struct)\s+\w+\s*(?:final\s*)?:\s*(?:(?:public|protected|private|virtual)\s+)*[A-Za-z_][\w:]*(?:\s*<[^{};]*>)?\s*[{,]").unwrap()
});
static C_COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)/\*.*?\*/|//[^\n]*").unwrap());
static OBJC_SIGNAL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@(?:interface|implementation|protocol|synthesize)\b").unwrap());
static PHP_OPEN_TAG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)<\?(?:php|=)").unwrap());
static PASCAL_INCLUDE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)^[ \t\x{FEFF}]*(?:\{\$[a-z]|\{%MainUnit\b|(?:class[ \t]+)?(?:procedure|constructor|destructor|function)[ \t]+[a-z_][\w.]*[^;\n]*;|unit[ \t]+[a-z_][\w.]*[ \t]*;|uses\s+[a-z_][\w.]*(?:\s*,\s*[a-z_][\w.]*)*\s*;)").unwrap()
});

fn sample(source: &str, max: usize) -> &str {
    if source.len() <= max {
        return source;
    }
    let mut end = max;
    while !source.is_char_boundary(end) {
        end -= 1;
    }
    &source[..end]
}

/// `// @flow` in a JS file's leading comments: parse it as TSX.
pub fn has_flow_pragma(source: &str) -> bool {
    let mut head = sample(source, 4096);
    if head.starts_with("#!") {
        head = head.find('\n').map(|i| &head[i + 1..]).unwrap_or("");
    }
    let lead = FLOW_LEAD.find(head).map(|m| m.as_str()).unwrap_or("");
    FLOW.is_match(lead) && !NOFLOW.is_match(lead)
}

fn looks_like_cpp(source: &str) -> bool {
    CPP_SIGNAL.is_match(sample(source, 8192))
        || CPP_BASE_CLAUSE.is_match(&C_COMMENT.replace_all(source, " "))
}

fn looks_like_objc(source: &str) -> bool {
    OBJC_SIGNAL.is_match(sample(source, 8192))
}

fn looks_like_pascal_include(source: &str) -> bool {
    !PHP_OPEN_TAG.is_match(source) && PASCAL_INCLUDE.is_match(source)
}

/// Decide a file's language from its path and (optionally) its content.
pub fn detect_language(path: &str, source: Option<&str>) -> Language {
    if is_play_routes_file(path) {
        return Language::Yaml;
    }
    if is_erlang_app_file(path) {
        return Language::Erlang;
    }
    let ext = extension(path);
    let lang = language_for_extension(&ext).unwrap_or(Language::Unknown);
    let Some(src) = source else { return lang };
    match lang {
        Language::JavaScript | Language::Jsx if has_flow_pragma(src) => Language::Tsx,
        Language::C if ext == ".h" => {
            if looks_like_cpp(src) {
                Language::Cpp
            } else if looks_like_objc(src) {
                Language::ObjC
            } else {
                Language::C
            }
        }
        Language::Php if ext == ".inc" && looks_like_pascal_include(src) => Language::Pascal,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_by_extension_and_content() {
        assert_eq!(detect_language("src/a.ts", None), Language::TypeScript);
        assert_eq!(detect_language("src/A.JAVA", None), Language::Java);
        assert_eq!(
            detect_language("x.h", Some("namespace foo {}")),
            Language::Cpp
        );
        assert_eq!(
            detect_language("x.h", Some("@interface Foo\n@end")),
            Language::ObjC
        );
        assert_eq!(detect_language("x.h", Some("int f(void);")), Language::C);
        assert_eq!(
            detect_language("a.js", Some("// @flow\nconst x = 1")),
            Language::Tsx
        );
        assert_eq!(detect_language("conf/routes", None), Language::Yaml);
        assert_eq!(
            detect_language("defs.inc", Some("unit Foo;\n")),
            Language::Pascal
        );
        assert_eq!(
            detect_language("defs.inc", Some("<?php echo 1;")),
            Language::Php
        );
        assert!(is_source_file("a/b.rs"));
        assert!(!is_source_file("README"));
    }
}
