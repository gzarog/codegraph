//! C/C++ `#include`s: the file an include names (import-resolver.ts's C/C++
//! branches — resolveViaImport, resolveImportPath, the include directories,
//! isExternalImport) and name-matcher's matchByFilePath.

use super::ctx::{memo, Ctx, Ref};
use crate::matcher::Match;
use crate::symbols::{same_family, SymId};
use codegraph_core::{Language, NodeKind, ReferenceKind};
use std::rc::Rc;

/// C and C++ standard library header names, without delimiters.
const C_CPP_STDLIB_HEADERS: &[&str] = &[
    "assert.h",
    "complex.h",
    "ctype.h",
    "errno.h",
    "fenv.h",
    "float.h",
    "inttypes.h",
    "iso646.h",
    "limits.h",
    "locale.h",
    "math.h",
    "setjmp.h",
    "signal.h",
    "stdalign.h",
    "stdarg.h",
    "stdatomic.h",
    "stdbool.h",
    "stddef.h",
    "stdint.h",
    "stdio.h",
    "stdlib.h",
    "stdnoreturn.h",
    "string.h",
    "tgmath.h",
    "threads.h",
    "time.h",
    "uchar.h",
    "wchar.h",
    "wctype.h",
    "cassert",
    "ccomplex",
    "cctype",
    "cerrno",
    "cfenv",
    "cfloat",
    "cinttypes",
    "ciso646",
    "climits",
    "clocale",
    "cmath",
    "csetjmp",
    "csignal",
    "cstdalign",
    "cstdarg",
    "cstdbool",
    "cstddef",
    "cstdint",
    "cstdio",
    "cstdlib",
    "cstring",
    "ctgmath",
    "ctime",
    "cuchar",
    "cwchar",
    "cwctype",
    "algorithm",
    "any",
    "array",
    "atomic",
    "barrier",
    "bit",
    "bitset",
    "charconv",
    "chrono",
    "codecvt",
    "compare",
    "complex",
    "concepts",
    "condition_variable",
    "coroutine",
    "deque",
    "exception",
    "execution",
    "expected",
    "filesystem",
    "format",
    "forward_list",
    "fstream",
    "functional",
    "future",
    "generator",
    "initializer_list",
    "iomanip",
    "ios",
    "iosfwd",
    "iostream",
    "istream",
    "iterator",
    "latch",
    "limits",
    "list",
    "locale",
    "map",
    "mdspan",
    "memory",
    "memory_resource",
    "mutex",
    "new",
    "numbers",
    "numeric",
    "optional",
    "ostream",
    "print",
    "queue",
    "random",
    "ranges",
    "ratio",
    "regex",
    "scoped_allocator",
    "semaphore",
    "set",
    "shared_mutex",
    "source_location",
    "span",
    "spanstream",
    "sstream",
    "stack",
    "stacktrace",
    "stdexcept",
    "stdfloat",
    "stop_token",
    "streambuf",
    "string",
    "string_view",
    "strstream",
    "syncstream",
    "system_error",
    "thread",
    "tuple",
    "type_traits",
    "typeindex",
    "typeinfo",
    "unordered_map",
    "unordered_set",
    "utility",
    "valarray",
    "variant",
    "vector",
    "version",
];

/// `EXTENSION_RESOLUTION` for C and C++.
fn extensions(lang: Language) -> &'static [&'static str] {
    match lang {
        Language::C => &[".h", ".c"],
        Language::Cpp => &[".h", ".hpp", ".hxx", ".cpp", ".cc", ".cxx"],
        Language::ObjC => &[".h", ".m", ".mm"],
        _ => &[],
    }
}

/// `isExternalImport` for a C or C++ include: a standard header.
pub fn is_external_import(spec: &str) -> bool {
    if spec.starts_with('.') {
        return false;
    }
    if C_CPP_STDLIB_HEADERS.contains(&spec) {
        return true;
    }
    let without_ext = spec.strip_suffix(".h").unwrap_or(spec);
    C_CPP_STDLIB_HEADERS.contains(&without_ext)
}

/// `path.posix.normalize`: `.` and empty segments dropped, `..` folded.
pub fn normalize_posix(p: &str) -> String {
    let absolute = p.starts_with('/');
    let trailing = p.ends_with('/') && p.len() > 1;
    let mut out: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|l| *l != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            s => out.push(s),
        }
    }
    let mut s = out.join("/");
    if absolute {
        s = format!("/{s}");
    }
    if s.is_empty() {
        s = if absolute { "/".into() } else { ".".into() };
    }
    if trailing && !s.ends_with('/') {
        s.push('/');
    }
    s
}

/// The directory part of a project path (`a/b.h` → `a`, `b.h` → ``).
pub fn dir_of(p: &str) -> &str {
    p.rfind('/').map_or("", |i| &p[..i])
}

/// `resolveImportPath` for a C or C++ include, memoised.
pub fn resolve_import_path(
    ctx: &Ctx,
    spec: &str,
    from_file: &str,
    lang: Language,
) -> Option<String> {
    let key = format!("{}\0{from_file}\0{spec}", lang.as_str());
    ctx.memo(memo::IMPORT_PATH, key, || {
        resolve_import_path_uncached(ctx, spec, from_file, lang)
    })
}

fn resolve_import_path_uncached(
    ctx: &Ctx,
    spec: &str,
    from_file: &str,
    lang: Language,
) -> Option<String> {
    // An external header resolves through the importing app's own config only,
    // which reads ES module specifiers.
    if is_external_import(spec) {
        return None;
    }
    if spec.starts_with('.') {
        return resolve_relative_import(ctx, spec, dir_of(from_file), lang);
    }
    if let Some(hit) = resolve_aliased_import(ctx, spec, lang) {
        return Some(hit);
    }
    resolve_cpp_include_path(ctx, spec, lang)
}

/// The project-relative path `path.resolve(fromDir, spec)` names, or None
/// outside the project.
fn project_relative(from_dir: &str, spec: &str) -> Option<String> {
    let joined = if from_dir.is_empty() {
        spec.to_string()
    } else {
        format!("{from_dir}/{spec}")
    };
    let n = normalize_posix(&joined);
    let n = n.trim_end_matches('/').to_string();
    if n == "." {
        return Some(String::new());
    }
    if n.starts_with("..") || n.starts_with('/') {
        // `../x` past the root: fileExists refuses it.
        return Some(n);
    }
    Some(n)
}

fn resolve_relative_import(
    ctx: &Ctx,
    spec: &str,
    from_dir: &str,
    lang: Language,
) -> Option<String> {
    let relative = project_relative(from_dir, spec)?;
    for ext in extensions(lang) {
        if relative.is_empty() && !ext.starts_with('/') {
            continue;
        }
        let candidate = format!("{relative}{ext}");
        if ctx.file_exists(&candidate) {
            return Some(candidate);
        }
    }
    if ctx.file_exists(&relative) {
        return Some(relative);
    }
    None
}

fn try_with_extensions(ctx: &Ctx, base: &str, lang: Language) -> Option<String> {
    for ext in extensions(lang) {
        let candidate = format!("{base}{ext}");
        if ctx.file_exists(&candidate) {
            return Some(candidate);
        }
    }
    if ctx.file_exists(base) {
        return Some(base.to_string());
    }
    None
}

/// resolveAliasedImport's conventional prefixes and the direct path. (A C
/// project has no tsconfig `paths` or package workspaces to consult.)
fn resolve_aliased_import(ctx: &Ctx, spec: &str, lang: Language) -> Option<String> {
    const FALLBACK: &[(&str, &str)] = &[
        ("@/", "src/"),
        ("~/", "src/"),
        ("@src/", "src/"),
        ("src/", "src/"),
        ("@app/", "app/"),
        ("app/", "app/"),
    ];
    for (alias, replacement) in FALLBACK {
        if let Some(rest) = spec.strip_prefix(alias) {
            if let Some(hit) = try_with_extensions(ctx, &format!("{replacement}{rest}"), lang) {
                return Some(hit);
            }
        }
    }
    try_with_extensions(ctx, spec, lang)
}

/// `loadCppIncludeDirs`: `-I` directories from a compilation database, else
/// the conventional and header-holding top-level directories.
pub fn include_dirs(ctx: &Ctx) -> Rc<Vec<String>> {
    ctx.memo(memo::INCLUDE_DIRS, String::new(), || {
        Rc::new(
            include_dirs_from_compile_db(ctx.root)
                .unwrap_or_else(|| include_dirs_heuristic(ctx.root)),
        )
    })
}

fn include_dirs_from_compile_db(root: &std::path::Path) -> Option<Vec<String>> {
    let candidates = [
        "compile_commands.json",
        "build/compile_commands.json",
        "cmake-build-debug/compile_commands.json",
        "cmake-build-release/compile_commands.json",
        "out/compile_commands.json",
    ];
    let db = candidates
        .iter()
        .map(|c| root.join(c))
        .find(|p| p.exists())?;
    let content = std::fs::read_to_string(db).ok()?;
    let entries: serde_json::Value = serde_json::from_str(&content).ok()?;
    let entries = entries.as_array()?;
    let mut dirs: Vec<String> = Vec::new();
    for entry in entries {
        let dir = entry
            .get("directory")
            .and_then(|d| d.as_str())
            .filter(|d| !d.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| root.to_path_buf());
        let args: Vec<String> = match entry.get("arguments").and_then(|a| a.as_array()) {
            Some(a) => a
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect(),
            None => entry
                .get("command")
                .and_then(|c| c.as_str())
                .map(shlex_split)
                .unwrap_or_default(),
        };
        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            let mut include: Option<String> = None;
            if arg.starts_with("-I") && arg.len() > 2 {
                include = Some(arg[2..].to_string());
            } else if (arg == "-isystem" || arg == "-I") && i + 1 < args.len() {
                include = Some(args[i + 1].clone());
                i += 1;
            }
            if let Some(inc) = include {
                let abs = if std::path::Path::new(&inc).is_absolute() {
                    std::path::PathBuf::from(&inc)
                } else {
                    dir.join(&inc)
                };
                if let Some(rel) = relative_to(root, &abs) {
                    if !rel.is_empty() && !rel.starts_with("..") && !dirs.contains(&rel) {
                        dirs.push(rel);
                    }
                }
            }
            i += 1;
        }
    }
    Some(dirs)
}

/// `path.relative(root, abs)`, lexically, or None when it leaves the root.
fn relative_to(root: &std::path::Path, abs: &std::path::Path) -> Option<String> {
    let root = normalize_posix(&root.to_string_lossy().replace('\\', "/"));
    let abs = normalize_posix(&abs.to_string_lossy().replace('\\', "/"));
    if abs == root {
        return Some(String::new());
    }
    abs.strip_prefix(&format!("{}/", root.trim_end_matches('/')))
        .map(str::to_string)
}

fn shlex_split(cmd: &str) -> Vec<String> {
    let c: Vec<char> = cmd.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < c.len() {
        while i < c.len() && c[i].is_whitespace() {
            i += 1;
        }
        if i >= c.len() {
            break;
        }
        let mut arg = String::new();
        if c[i] == '"' {
            i += 1;
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' && i + 1 < c.len() {
                    i += 1;
                }
                arg.push(c[i]);
                i += 1;
            }
            i += 1;
        } else if c[i] == '\'' {
            i += 1;
            while i < c.len() && c[i] != '\'' {
                arg.push(c[i]);
                i += 1;
            }
            i += 1;
        } else {
            while i < c.len() && !c[i].is_whitespace() {
                arg.push(c[i]);
                i += 1;
            }
        }
        out.push(arg);
    }
    out
}

fn include_dirs_heuristic(root: &std::path::Path) -> Vec<String> {
    const CONVENTION: &[&str] = &["include", "src", "lib", "api", "inc"];
    let mut entries: Vec<(String, std::path::PathBuf)> = match std::fs::read_dir(root) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect(),
        Err(_) => return Vec::new(),
    };
    entries.sort();
    let mut dirs = Vec::new();
    for (name, path) in entries {
        if CONVENTION.contains(&name.to_lowercase().as_str()) {
            dirs.push(name);
            continue;
        }
        let has_header = std::fs::read_dir(&path).is_ok_and(|rd| {
            rd.filter_map(|e| e.ok()).any(|e| {
                let f = e.file_name().to_string_lossy().to_lowercase();
                [".h", ".hpp", ".hxx", ".hh"].iter().any(|x| f.ends_with(x))
            })
        });
        if has_header {
            dirs.push(name);
        }
    }
    dirs
}

fn resolve_cpp_include_path(ctx: &Ctx, spec: &str, lang: Language) -> Option<String> {
    for dir in include_dirs(ctx).iter() {
        let dir = dir.replace('\\', "/");
        for ext in extensions(lang) {
            let candidate = format!("{dir}/{spec}{ext}");
            if ctx.file_exists(&candidate) {
                return Some(candidate);
            }
        }
        let candidate = format!("{dir}/{spec}");
        if ctx.file_exists(&candidate) {
            return Some(candidate);
        }
    }
    None
}

/// The `file` node of a path, found the way upstream finds it: by the
/// file's basename, then its path.
fn file_node_at(ctx: &Ctx, path: &str) -> Option<SymId> {
    let base = path.rsplit('/').next().unwrap_or(path);
    ctx.named(base)
        .iter()
        .copied()
        .find(|&n| ctx.kind(n) == NodeKind::File && ctx.sym(n).file == path)
}

/// resolveViaImport's C/C++ `#include` branch: the same directory first, then
/// the include path search.
pub fn resolve_via_import(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if r.kind != ReferenceKind::Imports {
        return None;
    }
    let from_dir = dir_of(&r.file);
    let sibling = normalize_posix(&if from_dir.is_empty() {
        r.name.clone()
    } else {
        format!("{from_dir}/{}", r.name)
    });
    if let Some(n) = file_node_at(ctx, &sibling) {
        return Some(Match::new(n, "import", 0.92));
    }
    let resolved = resolve_import_path(ctx, &r.name, &r.file, r.lang)?;
    file_node_at(ctx, &resolved).map(|n| Match::new(n, "import", 0.9))
}

/// `context.resolveImport`: the import resolver, else (an include it can't
/// place that names no standard header) the end of the include's path.
pub fn resolve_import(ctx: &Ctx, r: &Ref) -> Option<Match> {
    resolve_via_import(ctx, r).or_else(|| {
        if r.kind == ReferenceKind::Imports && !is_external_import(&r.name) {
            match_by_file_path(ctx, r)
        } else {
            None
        }
    })
}

fn has_short_extension(name: &str) -> bool {
    match name.rfind('.') {
        Some(dot) => {
            let ext = &name[dot + 1..];
            (1..=4).contains(&ext.len())
                && ext.as_bytes()[0].is_ascii_alphabetic()
                && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// name-matcher's matchByFilePath: a path-like reference, by the file's name.
pub fn match_by_file_path(ctx: &Ctx, r: &Ref) -> Option<Match> {
    if !r.name.contains('/') && !has_short_extension(&r.name) {
        return None;
    }
    let file_name = r.name.rsplit('/').next()?;
    if file_name.is_empty() {
        return None;
    }
    let files: Vec<SymId> = ctx
        .named(file_name)
        .iter()
        .copied()
        .filter(|&n| ctx.kind(n) == NodeKind::File)
        .collect();
    if files.is_empty() {
        return None;
    }
    if let Some(&exact) = files.iter().find(|&&n| {
        let s = ctx.sym(n);
        s.qualified_name == r.name || s.file == r.name
    }) {
        return Some(Match::new(exact, "file-path", 0.95));
    }
    let suffix: Vec<SymId> = files
        .iter()
        .copied()
        .filter(|&n| {
            let s = ctx.sym(n);
            s.qualified_name.ends_with(&r.name) || s.file.ends_with(&r.name)
        })
        .collect();
    if !suffix.is_empty() {
        return Some(Match::new(
            pick_closest_file_node(ctx, &suffix, r),
            "file-path",
            0.85,
        ));
    }
    if files.len() == 1 {
        return Some(Match::new(files[0], "file-path", 0.7));
    }
    None
}

fn pick_closest_file_node(ctx: &Ctx, candidates: &[SymId], r: &Ref) -> SymId {
    let ref_dir = dir_of(&r.file);
    let same_dir: Vec<SymId> = candidates
        .iter()
        .copied()
        .filter(|&c| dir_of(&ctx.sym(c).file) == ref_dir)
        .collect();
    let pool = if same_dir.is_empty() {
        candidates.to_vec()
    } else {
        same_dir
    };
    let mut best = pool[0];
    let mut best_score = i64::MIN;
    for c in pool {
        let s = ctx.sym(c);
        let score = path_proximity(&r.file, &s.file)
            + if same_family(s.language, r.lang) {
                5
            } else {
                0
            };
        if score > best_score {
            best_score = score;
            best = c;
        }
    }
    best
}

/// computePathProximity: 15 per shared leading directory, at most 80.
pub fn path_proximity(a: &str, b: &str) -> i64 {
    let mut da: Vec<&str> = a.split('/').collect();
    da.pop();
    let mut db: Vec<&str> = b.split('/').collect();
    db.pop();
    let shared = da.iter().zip(db.iter()).take_while(|(x, y)| x == y).count() as i64;
    (shared * 15).min(80)
}
