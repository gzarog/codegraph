//! Import statements → local bindings.
//!
//! Extraction records each import statement as an `import` node whose
//! signature is the statement text. Parsing it per language yields what each
//! local name refers to: a module, and optionally a symbol inside it.

use codegraph_core::Language;
use regex::Regex;
use std::sync::LazyLock;

/// One name an import statement brings into scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// The name code in the importing file uses.
    pub local: String,
    /// The module specifier as written (`./util`, `os.path`, `com.y`).
    pub module: String,
    /// The symbol imported from the module; `None` binds the module itself
    /// (a namespace import, `import os`, a Go package).
    pub imported: Option<String>,
    /// `import X from` / `export default`.
    pub is_default: bool,
    /// `export ... from`: a re-export, visible to importers of this file.
    pub is_reexport: bool,
}

impl Binding {
    fn module(local: &str, module: &str) -> Self {
        Binding {
            local: local.into(),
            module: module.into(),
            imported: None,
            is_default: false,
            is_reexport: false,
        }
    }

    fn symbol(local: &str, module: &str, imported: &str) -> Self {
        Binding {
            local: local.into(),
            module: module.into(),
            imported: Some(imported.into()),
            is_default: false,
            is_reexport: false,
        }
    }
}

static JS_FROM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?s)^\s*(import|export)\s+(type\s+)?(.*?)\s*from\s*['"]([^'"]+)['"]"#).unwrap()
});
static JS_SIDE_EFFECT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s*import\s*['"]([^'"]+)['"]"#).unwrap());
static JS_REQUIRE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?s)(?:const|let|var)\s+(\{.*?\}|[\w$]+)\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)"#,
    )
    .unwrap()
});
static JS_IMPORT_EQUALS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"import\s+([\w$]+)\s*=\s*require\(\s*['"]([^'"]+)['"]\s*\)"#).unwrap()
});

fn strip_type_prefix(s: &str) -> &str {
    s.trim()
        .strip_prefix("type ")
        .map(str::trim)
        .unwrap_or(s.trim())
}

/// `{ a, b as c, type d }` → [(imported, local)].
fn js_specifiers(list: &str) -> Vec<(String, String)> {
    list.trim()
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(',')
        .filter_map(|part| {
            let part = strip_type_prefix(part);
            if part.is_empty() {
                return None;
            }
            let (imported, local) = match part.split_once(" as ") {
                Some((i, l)) => (i.trim(), l.trim()),
                None => (part, part),
            };
            let unquote = |s: &str| s.trim_matches(|c| c == '"' || c == '\'').to_string();
            Some((unquote(imported), unquote(local)))
        })
        .collect()
}

fn parse_js(stmt: &str) -> Vec<Binding> {
    let mut out = Vec::new();
    if let Some(c) = JS_FROM.captures(stmt) {
        let is_export = &c[1] == "export";
        let clause = c[3].trim();
        let module = &c[4];
        // `default, { a }` / `default, * as ns`
        let (default_part, rest) = if !clause.starts_with('{') && !clause.starts_with('*') {
            match clause.split_once(',') {
                Some((d, r)) => (Some(d.trim()), r.trim()),
                None => (Some(clause), ""),
            }
        } else {
            (None, clause)
        };
        if let Some(d) = default_part.filter(|d| !d.is_empty()) {
            out.push(Binding {
                is_default: true,
                ..Binding::symbol(d, module, "default")
            });
        }
        if let Some(ns) = rest.strip_prefix('*') {
            let ns = ns.trim();
            match ns.strip_prefix("as ") {
                Some(local) => out.push(Binding::module(local.trim(), module)),
                // `export * from`: re-exports every name.
                None => out.push(Binding {
                    is_reexport: true,
                    ..Binding::module("*", module)
                }),
            }
        } else if rest.starts_with('{') {
            for (imported, local) in js_specifiers(rest) {
                out.push(Binding {
                    is_default: imported == "default",
                    ..Binding::symbol(&local, module, &imported)
                });
            }
        }
        if is_export {
            for b in &mut out {
                b.is_reexport = true;
            }
        }
        return out;
    }
    if let Some(c) = JS_IMPORT_EQUALS.captures(stmt) {
        out.push(Binding::module(&c[1], &c[2]));
        return out;
    }
    if let Some(c) = JS_REQUIRE.captures(stmt) {
        let target = c[1].trim();
        if target.starts_with('{') {
            for (imported, local) in js_specifiers(&target.replace(':', " as ")) {
                out.push(Binding::symbol(&local, &c[2], &imported));
            }
        } else {
            out.push(Binding::module(target, &c[2]));
        }
        return out;
    }
    if let Some(c) = JS_SIDE_EFFECT.captures(stmt) {
        out.push(Binding::module("", &c[1]));
    }
    out
}

static PY_FROM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^\s*from\s+([.\w]+)\s+import\s+\(?(.*?)\)?\s*$").unwrap());
static PY_IMPORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*import\s+(.+)$").unwrap());

fn parse_python(stmt: &str) -> Vec<Binding> {
    let stmt = stmt.trim();
    if let Some(c) = PY_FROM.captures(stmt) {
        let module = &c[1];
        return c[2]
            .split(',')
            .filter_map(|p| {
                let p = p
                    .trim()
                    .trim_matches(|ch| ch == '(' || ch == ')' || ch == '\\')
                    .trim();
                if p.is_empty() {
                    return None;
                }
                if p == "*" {
                    return Some(Binding {
                        is_reexport: true,
                        ..Binding::module("*", module)
                    });
                }
                let (name, local) = p
                    .split_once(" as ")
                    .map(|(a, b)| (a.trim(), b.trim()))
                    .unwrap_or((p, p));
                Some(Binding::symbol(local, module, name))
            })
            .collect();
    }
    if let Some(c) = PY_IMPORT.captures(stmt) {
        return c[1]
            .split(',')
            .filter_map(|p| {
                let p = p.trim();
                if p.is_empty() {
                    return None;
                }
                Some(match p.split_once(" as ") {
                    Some((m, l)) => Binding::module(l.trim(), m.trim()),
                    // `import a.b` binds `a`, but code writes `a.b.f()`; bind the full path.
                    None => Binding::module(p, p),
                })
            })
            .collect();
    }
    Vec::new()
}

static GO_SPEC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?:([\w.]+)\s+)?"([^"]+)""#).unwrap());

fn parse_go(stmt: &str) -> Vec<Binding> {
    GO_SPEC
        .captures_iter(stmt)
        .map(|c| {
            let path = &c[2];
            let local = match c.get(1).map(|m| m.as_str()) {
                Some(alias) => alias.to_string(),
                None => go_package_name(path),
            };
            Binding::module(&local, path)
        })
        .collect()
}

/// The package name Go assumes for an import path: its last segment, minus a
/// `/vN` major-version suffix and a `go-` prefix / `.go` suffix.
pub fn go_package_name(path: &str) -> String {
    let mut parts: Vec<&str> = path.split('/').collect();
    if parts.len() > 1
        && parts.last().is_some_and(|l| {
            l.len() > 1 && l.starts_with('v') && l[1..].chars().all(|c| c.is_ascii_digit())
        })
    {
        parts.pop();
    }
    let last = parts.last().copied().unwrap_or(path);
    let last = last.strip_prefix("go-").unwrap_or(last);
    let last = last.strip_suffix(".go").unwrap_or(last);
    last.replace(['-', '.'], "_")
}

static JVM_IMPORT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*import\s+(static\s+)?([\w.]+?)(\.\*)?(?:\s+as\s+(\w+))?\s*;?\s*$").unwrap()
});

fn parse_jvm(stmt: &str) -> Vec<Binding> {
    let Some(c) = JVM_IMPORT.captures(stmt.trim()) else {
        return Vec::new();
    };
    let path = &c[2];
    if c.get(3).is_some() {
        return vec![Binding::module("*", path)];
    }
    let (pkg, name) = path.rsplit_once('.').unwrap_or(("", path));
    let local = c.get(4).map(|m| m.as_str()).unwrap_or(name);
    vec![Binding::symbol(local, pkg, name)]
}

static CS_USING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*(?:global\s+)?using\s+(static\s+)?(?:(\w+)\s*=\s*)?([\w.]+)\s*;").unwrap()
});

fn parse_csharp(stmt: &str) -> Vec<Binding> {
    let Some(c) = CS_USING.captures(stmt) else {
        return Vec::new();
    };
    let path = &c[3];
    match c.get(2) {
        Some(alias) => {
            let (ns, name) = path.rsplit_once('.').unwrap_or(("", path));
            vec![Binding::symbol(alias.as_str(), ns, name)]
        }
        None => vec![Binding::module("*", path)],
    }
}

fn parse_php(stmt: &str) -> Vec<Binding> {
    static USE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s)^\s*use\s+(?:function\s+|const\s+)?(.+?);?\s*$").unwrap()
    });
    static INCLUDE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?:require|include)(?:_once)?\s*\(?\s*['"]([^'"]+)['"]"#).unwrap()
    });
    if let Some(c) = INCLUDE.captures(stmt) {
        return vec![Binding::module("", &c[1])];
    }
    let Some(c) = USE.captures(stmt) else {
        return Vec::new();
    };
    let body = c[1].trim().trim_start_matches('\\');
    let mut out = Vec::new();
    // Group use: `use A\B\{C, D as E};`
    if let Some((prefix, group)) = body.split_once('{') {
        let prefix = prefix.trim_end_matches('\\');
        for item in group.trim_end_matches('}').split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let (path, alias) = item
                .split_once(" as ")
                .map(|(p, a)| (p.trim(), Some(a.trim())))
                .unwrap_or((item, None));
            let full = format!("{prefix}\\{path}");
            let (ns, name) = full.rsplit_once('\\').unwrap_or(("", &full));
            out.push(Binding::symbol(alias.unwrap_or(name), ns, name));
        }
        return out;
    }
    for item in body.split(',') {
        let item = item.trim().trim_start_matches('\\');
        let (path, alias) = item
            .split_once(" as ")
            .map(|(p, a)| (p.trim(), Some(a.trim())))
            .unwrap_or((item, None));
        let (ns, name) = path.rsplit_once('\\').unwrap_or(("", path));
        out.push(Binding::symbol(alias.unwrap_or(name), ns, name));
    }
    out
}

/// `use a::b::{c, d as e, self};` → bindings with `::`-joined module paths.
fn parse_rust(stmt: &str) -> Vec<Binding> {
    let s = stmt.trim();
    let s = s.strip_prefix("pub ").unwrap_or(s);
    let s = s.strip_prefix("pub(crate) ").unwrap_or(s);
    let Some(body) = s.strip_prefix("use ") else {
        if let Some(m) = s
            .strip_prefix("mod ")
            .or_else(|| s.strip_prefix("pub mod "))
        {
            let name = m.trim_end_matches(';').trim();
            return vec![Binding::module(name, &format!("self::{name}"))];
        }
        return Vec::new();
    };
    let mut out = Vec::new();
    rust_tree(body.trim_end_matches(';').trim(), "", &mut out);
    out
}

fn split_top_level(s: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut depth, mut start) = (0, 0);
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&s[start..]);
    parts
}

fn rust_tree(tree: &str, prefix: &str, out: &mut Vec<Binding>) {
    let tree = tree.trim();
    if tree.is_empty() {
        return;
    }
    let join = |a: &str, b: &str| {
        if a.is_empty() {
            b.to_string()
        } else {
            format!("{a}::{b}")
        }
    };
    if let Some(open) = tree.find('{') {
        let head = tree[..open].trim().trim_end_matches("::");
        let inner = tree[open + 1..].trim_end().trim_end_matches('}');
        let base = join(prefix, head);
        for part in split_top_level(inner) {
            rust_tree(part, &base, out);
        }
        return;
    }
    let (path, alias) = match tree.split_once(" as ") {
        Some((p, a)) => (p.trim(), Some(a.trim())),
        None => (tree, None),
    };
    let full = join(prefix, path);
    let (module, name) = full.rsplit_once("::").unwrap_or(("", &full));
    if name == "*" {
        out.push(Binding::module("*", module));
    } else if name == "self" {
        let local = module.rsplit("::").next().unwrap_or(module);
        out.push(Binding::module(alias.unwrap_or(local), module));
    } else {
        out.push(Binding::symbol(alias.unwrap_or(name), module, name));
    }
}

fn quoted(stmt: &str) -> Option<&str> {
    static Q: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"['"<]([^'">]+)['">]"#).unwrap());
    Q.captures(stmt).map(|c| c.get(1).unwrap().as_str())
}

/// The bindings an import statement creates. `name` is the import node's
/// name (the module as extraction saw it), used when the statement text
/// doesn't parse.
pub fn parse_import(language: Language, statement: &str, name: &str) -> Vec<Binding> {
    use Language::*;
    let mut out = match language {
        TypeScript | JavaScript | Tsx | Jsx | ArkTs | Svelte | Vue | Astro => parse_js(statement),
        Python => {
            // `from \<newline> loader import X`: a continuation is a space.
            let joined = statement.replace("\\\r\n", " ").replace("\\\n", " ");
            parse_python(&joined)
        }
        Go => parse_go(statement),
        Java | Kotlin | Scala => parse_jvm(statement),
        CSharp => parse_csharp(statement),
        Php => parse_php(statement),
        Rust => parse_rust(statement),
        C | Cpp | ObjC | Ruby | Lua | Luau | Dart | Swift => {
            let module = quoted(statement).unwrap_or(name);
            let local = if matches!(language, Swift) { name } else { "" };
            vec![Binding::module(local, module)]
        }
        _ => Vec::new(),
    };
    if out.is_empty() && !name.is_empty() {
        out.push(Binding::module("", name));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_forms() {
        let b = parse_import(
            Language::TypeScript,
            "import Def, { helper as h, type T } from './util';",
            "./util",
        );
        assert_eq!(b.len(), 3);
        assert!(b[0].is_default && b[0].local == "Def");
        assert_eq!(b[1], Binding::symbol("h", "./util", "helper"));
        assert_eq!(b[2].local, "T");
        let ns = parse_import(Language::TypeScript, "import * as ns from 'pkg'", "pkg");
        assert_eq!(ns, vec![Binding::module("ns", "pkg")]);
        let re = parse_import(
            Language::TypeScript,
            "export { a as b } from \"./x\";",
            "./x",
        );
        assert!(re[0].is_reexport && re[0].local == "b");
        let star = parse_import(Language::TypeScript, "export * from './z'", "./z");
        assert!(star[0].is_reexport && star[0].local == "*");
        let req = parse_import(
            Language::JavaScript,
            "const { a, b: c } = require('./m')",
            "./m",
        );
        assert_eq!(req[1], Binding::symbol("c", "./m", "b"));
    }

    #[test]
    fn python_forms() {
        let b = parse_import(
            Language::Python,
            "from .util import (helper, other as o)",
            ".util",
        );
        assert_eq!(
            b,
            vec![
                Binding::symbol("helper", ".util", "helper"),
                Binding::symbol("o", ".util", "other")
            ]
        );
        let m = parse_import(Language::Python, "import os.path as p, sys", "os.path");
        assert_eq!(
            m,
            vec![
                Binding::module("p", "os.path"),
                Binding::module("sys", "sys")
            ]
        );
    }

    #[test]
    fn go_jvm_cs_php_rust() {
        assert_eq!(
            parse_import(Language::Go, "u \"example.com/m/util\"", ""),
            vec![Binding::module("u", "example.com/m/util")]
        );
        assert_eq!(
            parse_import(Language::Go, "\"github.com/x/go-yaml/v3\"", "")[0].local,
            "yaml"
        );
        assert_eq!(
            parse_import(Language::Java, "import com.y.Util;", ""),
            vec![Binding::symbol("Util", "com.y", "Util")]
        );
        assert_eq!(
            parse_import(Language::Java, "import com.z.*;", "")[0].local,
            "*"
        );
        assert_eq!(
            parse_import(Language::CSharp, "using M = App.Models.User;", ""),
            vec![Binding::symbol("M", "App.Models", "User")]
        );
        assert_eq!(
            parse_import(Language::Php, "use Foo\\Bar as Baz;", ""),
            vec![Binding::symbol("Baz", "Foo", "Bar")]
        );
        let g = parse_import(
            Language::Php,
            "use App\\{Models\\User, Http\\Kernel as K};",
            "",
        );
        assert_eq!(g[1], Binding::symbol("K", "App\\Http", "Kernel"));
        let r = parse_import(
            Language::Rust,
            "use crate::util::{helper, Thing as T, sub::{self, x}};",
            "crate",
        );
        assert_eq!(r[0], Binding::symbol("helper", "crate::util", "helper"));
        assert_eq!(r[1], Binding::symbol("T", "crate::util", "Thing"));
        assert_eq!(r[2], Binding::module("sub", "crate::util::sub"));
        assert_eq!(r[3], Binding::symbol("x", "crate::util::sub", "x"));
        assert_eq!(
            parse_import(Language::C, "#include \"util.h\"", "util.h")[0].module,
            "util.h"
        );
    }
}
