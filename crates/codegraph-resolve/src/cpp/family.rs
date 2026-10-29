//! Upstream's language families (name-matcher.ts `LANGUAGE_FAMILY` and
//! `CODE_FAMILY`), exactly: C, C++, Objective-C and Swift are one native family.

use codegraph_core::Language;

fn language_family(lang: Language) -> Option<&'static str> {
    use Language::*;
    Some(match lang {
        Java | Kotlin | Scala => "jvm",
        Swift | ObjC | C | Cpp => "native",
        TypeScript | Tsx | JavaScript | Jsx | ArkTs | Svelte | Vue | Astro => "web",
        CSharp | Razor | VbNet => "dotnet",
        Cfml | CfScript => "cfml",
        _ => return None,
    })
}

fn code_family(lang: Language) -> Option<&'static str> {
    use Language::*;
    language_family(lang).or(Some(match lang {
        Python => "python",
        Go => "go",
        Rust => "rust",
        Php => "php",
        Ruby => "ruby",
        Dart => "dart",
        Lua | Luau => "lua",
        R => "r",
        Erlang => "erlang",
        Pascal => "pascal",
        Solidity => "solidity",
        Nix => "nix",
        Cobol => "cobol",
        _ => return None,
    }))
}

/// `sameLanguageFamily`.
pub fn same_language_family(a: Language, b: Language) -> bool {
    a == b || language_family(a).is_some_and(|f| language_family(b) == Some(f))
}

/// `crossesCodeBoundary`.
pub fn crosses_code_boundary(a: Language, b: Language) -> bool {
    match (code_family(a), code_family(b)) {
        (Some(x), Some(y)) => x != y,
        _ => false,
    }
}

pub fn is_native(lang: Language) -> bool {
    code_family(lang) == Some("native")
}

pub fn is_c_family(lang: Language) -> bool {
    matches!(lang, Language::C | Language::Cpp)
}
