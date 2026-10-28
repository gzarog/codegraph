//! codegraph-extract — tree-sitter parse + extraction.
//!
//! Input `(file_path, content, language)` per file; output an
//! [`ExtractionResult`]: the file's symbols, the edges between them, and the
//! references that resolution still has to match against the rest of the
//! project. Calls are synchronous; the indexer parallelizes per file.
//!
//! Each language has a dedicated walker module (tsjs/ for
//! typescript/tsx/javascript/jsx). Walkers write compact row tables
//! (buffers.rs) that [`decode`] turns into typed records.

#![deny(clippy::all)]

/// First statement of every recursive walker function (see stack.rs, #1581):
/// once the stack pointer is inside the red zone, stop descending — the
/// latched flag makes `stack::run_guarded` discard the walk and defer the
/// file to wasm. `Default::default()` covers every walker return type in use
/// (`()`, `bool`, `Option<_>`, `String`); a hook returning `false` just sends
/// its caller down the generic child walk, whose own guard returns at once.
macro_rules! stack_guard {
    () => {
        if $crate::stack::exhausted() {
            return ::core::default::Default::default();
        }
    };
}

mod buffers;
mod ccpp;
mod cfnptr;
mod csharp;
mod dart;
mod decode;
mod docstring;
mod generic;
mod go;
mod ids;
mod java;
mod kotlin;
mod langs;
mod lite;
mod lua;
mod php;
mod python;
mod rlang;
mod ruby;
mod rustlang;
mod scala;
mod sfc;
mod stack;
mod swift;
mod textutil;
mod tsjs;

use codegraph_core::{ExtractionError, ExtractionResult, Language, Severity};
use std::cell::Cell;

thread_local! {
    /// Set by a walker whose parse tree contained ERROR/MISSING nodes.
    static PARSE_ERRORS: Cell<bool> = const { Cell::new(false) };
}

pub(crate) fn note_parse_errors() {
    PARSE_ERRORS.with(|f| f.set(true));
}

pub use decode::decode;
pub use generic::debug_tree;
pub use langs::grammar_for;

/// The languages this crate has a walker for.
pub fn supported_languages() -> &'static [&'static str] {
    &langs::LANGUAGES
}

/// Whether `language` has a dedicated extraction walker.
pub fn supports(language: Language) -> bool {
    langs::LANGUAGES.contains(&language.as_str())
        || matches!(
            language,
            Language::Svelte
                | Language::Vue
                | Language::Astro
                | Language::ArkTs
                | Language::Liquid
                | Language::Razor
        )
        || generic::spec_for(language).is_some()
        || lite::supports(language)
}

/// The C function-pointer facts for one file (see cfnptr.rs).
pub use cfnptr::{
    scan_file as cfnptr_scan_file, FileFacts as CfnptrFacts, StructExtent as CfnptrStructExtent,
};

/// Extract one file. A file the walker can't finish (a stack-guard trip on a
/// pathologically nested file) comes back with no symbols and one error.
pub fn extract_file(file_path: &str, content: &str, language: Language) -> ExtractionResult {
    match language {
        Language::Svelte | Language::Vue | Language::Astro => {
            return sfc::extract_component(file_path, content, language)
        }
        Language::ArkTs => return sfc::extract_arkts(file_path, content),
        Language::Liquid => return sfc::extract_liquid(file_path, content),
        Language::Razor => return sfc::extract_razor(file_path, content),
        l if generic::spec_for(l).is_some() => {
            let spec = generic::spec_for(l).unwrap();
            return generic::extract(file_path, content, l, spec);
        }
        l if lite::supports(l) => return lite::extract(file_path, content, l),
        _ => {}
    }
    let started = std::time::Instant::now();
    let lang = language.as_str();
    PARSE_ERRORS.with(|f| f.set(false));
    let out = stack::run_guarded(|| match lang {
        "java" => java::extract(file_path, content),
        "python" => python::extract(file_path, content),
        "go" => go::extract(file_path, content),
        "c" | "cpp" => ccpp::extract(file_path, content, lang),
        "rust" => rustlang::extract(file_path, content),
        "csharp" => csharp::extract(file_path, content),
        "ruby" => ruby::extract(file_path, content),
        "php" => php::extract(file_path, content),
        "swift" => swift::extract(file_path, content),
        "kotlin" => kotlin::extract(file_path, content),
        "r" => rlang::extract(file_path, content),
        "lua" | "luau" => lua::extract(file_path, content, lang),
        "scala" => scala::extract(file_path, content),
        "dart" => dart::extract(file_path, content),
        "typescript" | "tsx" | "javascript" | "jsx" => tsjs::extract(file_path, content, lang),
        other => Err(format!("unsupported language: {other}")),
    });
    let mut result = match out.and_then(|o| decode(&o, file_path, language)) {
        Ok(r) => r,
        Err(message) => {
            let mut err =
                ExtractionError::error(message.trim_start_matches("defer:").trim().to_string());
            err.file_path = Some(file_path.to_string());
            ExtractionResult {
                errors: vec![err],
                ..Default::default()
            }
        }
    };
    if PARSE_ERRORS.with(|f| f.replace(false)) {
        result.errors.push(ExtractionError {
            message: "parse tree contains syntax errors; extraction is partial".to_string(),
            file_path: Some(file_path.to_string()),
            line: None,
            column: None,
            severity: Severity::Warning,
            code: Some("parse_error".to_string()),
        });
    }
    result.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
    result
}

/// Node-kind and field tables of a grammar, for diagnostics.
pub struct GrammarInfo {
    pub abi_version: u32,
    pub node_kinds: Vec<String>,
    pub field_names: Vec<String>,
}

pub fn grammar_info(language: &str) -> Option<GrammarInfo> {
    let lang = langs::grammar_for(language)?;
    let node_kinds = (0..lang.node_kind_count())
        .map(|i| lang.node_kind_for_id(i as u16).unwrap_or("").to_string())
        .collect();
    // Field ids are 1-based in tree-sitter.
    let field_names = (1..=lang.field_count())
        .map(|i| lang.field_name_for_id(i as u16).unwrap_or("").to_string())
        .collect();
    Some(GrammarInfo {
        abi_version: lang.abi_version() as u32,
        node_kinds,
        field_names,
    })
}

/// Strip C comments and string contents the way the cFnPtr scan does.
pub fn cfnptr_strip_c(text: &str) -> String {
    String::from_utf8_lossy(&cfnptr::strip_c(text.as_bytes())).into_owned()
}
