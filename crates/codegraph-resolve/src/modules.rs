//! Module specifier → project files, per language convention.

use crate::symbols::Symbols;
use codegraph_core::Language;
use std::collections::HashMap;
use std::path::Path;

/// Lexically normalize a `/`-separated relative path (`a/./b/../c` → `a/c`).
/// Returns `None` when it climbs above the project root.
pub fn normalize(path: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    Some(parts.join("/"))
}

pub fn dir_of(file: &str) -> &str {
    file.rfind('/').map(|i| &file[..i]).unwrap_or("")
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

const JS_EXTENSIONS: &[&str] = &[
    ".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts", ".ets", ".vue",
    ".svelte", ".astro",
];

/// tsconfig `paths` alias: `@app/*` → [`src/app/*`].
#[derive(Debug, Clone)]
struct PathAlias {
    pattern: String,
    targets: Vec<String>,
}

/// Project-wide facts module resolution needs, read once per run.
pub struct Modules {
    /// Go module path → directory (relative, "" for root).
    go_modules: Vec<(String, String)>,
    ts_aliases: Vec<PathAlias>,
    ts_base_url: Option<String>,
    /// package.json `name` → package directory, for monorepo workspaces.
    js_packages: HashMap<String, String>,
    /// Rust crate name → directory of its `src/lib.rs`/`main.rs` (crate root module dir).
    rust_crates: Vec<(String, String)>,
    /// Dart package name (pubspec.yaml) → package dir.
    dart_packages: HashMap<String, String>,
    /// directory → files directly in it.
    dir_files: HashMap<String, Vec<String>>,
    python: crate::python_modules::PythonModules,
}

fn read(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

/// Strip `//` and `/* */` comments and trailing commas so tsconfig parses as JSON.
fn jsonc_to_json(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let b = text.as_bytes();
    let (mut i, mut in_str) = (0, false);
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c as char);
            if c == b'\\' && i + 1 < b.len() {
                out.push(b[i + 1] as char);
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        if c == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else {
            out.push(c as char);
            i += 1;
        }
    }
    let re = regex::Regex::new(r",(\s*[}\]])").unwrap();
    re.replace_all(&out, "$1").into_owned()
}

impl Modules {
    pub fn load(root: &Path, syms: &Symbols) -> Self {
        let mut m = Modules {
            go_modules: Vec::new(),
            ts_aliases: Vec::new(),
            ts_base_url: None,
            js_packages: HashMap::new(),
            rust_crates: Vec::new(),
            dart_packages: HashMap::new(),
            dir_files: HashMap::new(),
            python: crate::python_modules::PythonModules::build(root, syms.files()),
        };
        for f in syms.files() {
            m.dir_files
                .entry(dir_of(f).to_string())
                .or_default()
                .push(f.clone());
        }
        for files in m.dir_files.values_mut() {
            files.sort();
        }
        m.scan_manifests(root);
        m.load_tsconfig(root);
        m
    }

    fn scan_manifests(&mut self, root: &Path) {
        let walker = ignore::WalkBuilder::new(root)
            .hidden(true)
            .max_depth(Some(6))
            .build();
        for entry in walker.flatten() {
            let name = entry.file_name().to_string_lossy();
            if !matches!(
                name.as_ref(),
                "go.mod" | "package.json" | "Cargo.toml" | "pubspec.yaml"
            ) {
                continue;
            }
            let Ok(rel) = entry.path().strip_prefix(root) else {
                continue;
            };
            let rel = rel.to_string_lossy().replace('\\', "/");
            if rel
                .split('/')
                .any(|seg| seg == "node_modules" || seg == "target" || seg == "vendor")
            {
                continue;
            }
            let dir = dir_of(&rel).to_string();
            let Some(text) = read(root, &rel) else {
                continue;
            };
            match name.as_ref() {
                "go.mod" => {
                    if let Some(line) = text.lines().find(|l| l.trim_start().starts_with("module "))
                    {
                        let module = line.trim_start()["module ".len()..]
                            .trim()
                            .trim_matches('"')
                            .to_string();
                        self.go_modules.push((module, dir));
                    }
                }
                "package.json" => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) {
                        if let Some(n) = v.get("name").and_then(|n| n.as_str()) {
                            self.js_packages.insert(n.to_string(), dir);
                        }
                    }
                }
                "Cargo.toml" => {
                    let crate_name = text
                        .lines()
                        .skip_while(|l| l.trim() != "[package]")
                        .find_map(|l| {
                            l.trim().strip_prefix("name").map(|r| {
                                r.trim_start_matches([' ', '='])
                                    .trim()
                                    .trim_matches('"')
                                    .to_string()
                            })
                        });
                    if let Some(n) = crate_name {
                        self.rust_crates
                            .push((n.replace('-', "_"), join(&dir, "src")));
                    }
                }
                "pubspec.yaml" => {
                    if let Some(n) = text.lines().find_map(|l| l.strip_prefix("name:")) {
                        self.dart_packages.insert(n.trim().to_string(), dir);
                    }
                }
                _ => {}
            }
        }
        // Longest module path first so nested modules win.
        self.go_modules
            .sort_by_key(|(m, _)| std::cmp::Reverse(m.len()));
    }

    fn load_tsconfig(&mut self, root: &Path) {
        for name in ["tsconfig.json", "jsconfig.json", "tsconfig.base.json"] {
            let Some(text) = read(root, name) else {
                continue;
            };
            let Ok(v) = serde_json::from_str::<serde_json::Value>(&jsonc_to_json(&text)) else {
                continue;
            };
            let Some(opts) = v.get("compilerOptions") else {
                continue;
            };
            let base = opts
                .get("baseUrl")
                .and_then(|b| b.as_str())
                .and_then(normalize);
            if let Some(paths) = opts.get("paths").and_then(|p| p.as_object()) {
                for (pattern, targets) in paths {
                    let targets = targets
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|t| t.as_str())
                                .filter_map(|t| normalize(&join(base.as_deref().unwrap_or(""), t)))
                                .collect()
                        })
                        .unwrap_or_default();
                    self.ts_aliases.push(PathAlias {
                        pattern: pattern.clone(),
                        targets,
                    });
                }
            }
            self.ts_base_url = base;
            break;
        }
    }

    pub fn files_in_dir(&self, dir: &str) -> &[String] {
        self.dir_files.get(dir).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Files a module specifier names, nearest/most specific first. Empty
    /// for external (dependency/stdlib) modules.
    pub fn resolve(&self, syms: &Symbols, from: &str, lang: Language, spec: &str) -> Vec<String> {
        use Language::*;
        match lang {
            TypeScript | JavaScript | Tsx | Jsx | ArkTs | Svelte | Vue | Astro => {
                self.resolve_js(syms, from, spec).into_iter().collect()
            }
            Python => self.resolve_python(from, spec).into_iter().collect(),
            Go => self.resolve_go(spec),
            Rust => self.resolve_rust(syms, from, spec).into_iter().collect(),
            C | Cpp | ObjC => self.resolve_relative_or_suffix(syms, from, spec, &[""]),
            Ruby => self.resolve_relative_or_suffix(syms, from, spec, &["", ".rb"]),
            Lua | Luau => {
                let p = spec.replace('.', "/");
                let mut out = self.resolve_relative_or_suffix(
                    syms,
                    from,
                    &p,
                    &[".lua", ".luau", "/init.lua", "/init.luau"],
                );
                out.truncate(1);
                out
            }
            Dart => self.resolve_dart(syms, from, spec).into_iter().collect(),
            Php => self.resolve_relative_or_suffix(syms, from, spec, &[""]),
            _ => self.resolve_relative_or_suffix(syms, from, spec, &[""]),
        }
    }

    fn try_js_file(&self, syms: &Symbols, base: &str) -> Option<String> {
        if syms.file_exists(base) && !base.ends_with(".js") {
            return Some(base.to_string());
        }
        // TS ESM imports name the emitted `.js`: try the source extensions.
        let stem = [".js", ".jsx", ".mjs", ".cjs"]
            .iter()
            .find_map(|e| base.strip_suffix(e))
            .unwrap_or(base);
        for ext in JS_EXTENSIONS {
            let p = format!("{stem}{ext}");
            if syms.file_exists(&p) {
                return Some(p);
            }
        }
        if syms.file_exists(base) {
            return Some(base.to_string());
        }
        for ext in JS_EXTENSIONS {
            let p = format!("{base}/index{ext}");
            if syms.file_exists(&p) {
                return Some(p);
            }
        }
        None
    }

    fn resolve_js(&self, syms: &Symbols, from: &str, spec: &str) -> Option<String> {
        if spec.starts_with('.') {
            return self.try_js_file(syms, &normalize(&join(dir_of(from), spec))?);
        }
        if let Some(abs) = spec.strip_prefix('/') {
            return self.try_js_file(syms, &normalize(abs)?);
        }
        for alias in &self.ts_aliases {
            let captured = match alias.pattern.strip_suffix('*') {
                Some(prefix) => spec.strip_prefix(prefix),
                None => (spec == alias.pattern).then_some(""),
            };
            let Some(rest) = captured else { continue };
            for t in &alias.targets {
                if let Some(hit) =
                    normalize(&t.replace('*', rest)).and_then(|p| self.try_js_file(syms, &p))
                {
                    return Some(hit);
                }
            }
        }
        if let Some(base) = &self.ts_base_url {
            if let Some(hit) = normalize(&join(base, spec)).and_then(|p| self.try_js_file(syms, &p))
            {
                return Some(hit);
            }
        }
        // Monorepo workspace package (`@scope/pkg` or `@scope/pkg/sub`).
        let mut best: Option<(&String, &String)> = None;
        for (name, dir) in &self.js_packages {
            if (spec == name || spec.starts_with(&format!("{name}/")))
                && best.is_none_or(|(n, _)| name.len() > n.len())
            {
                best = Some((name, dir));
            }
        }
        if let Some((name, dir)) = best {
            let sub = spec[name.len()..].trim_start_matches('/');
            let candidates = if sub.is_empty() {
                vec![
                    join(dir, "src/index"),
                    join(dir, "index"),
                    join(dir, "src/main"),
                    join(dir, "lib/index"),
                ]
            } else {
                vec![join(dir, &format!("src/{sub}")), join(dir, sub)]
            };
            for c in candidates {
                if let Some(hit) = normalize(&c).and_then(|p| self.try_js_file(syms, &p)) {
                    return Some(hit);
                }
            }
        }
        None
    }

    /// The file the Python module `a.b.c` may be from some import root:
    /// `a/b/c.py`, else `b/c.py`, else `c.py` (or a package of that path),
    /// when one file is.
    pub fn python_module_any_root(&self, syms: &Symbols, module: &str) -> Option<String> {
        let parts: Vec<&str> = module.split('.').collect();
        for i in 0..parts.len() {
            let rel = parts[i..].join("/");
            let (file, pkg) = (format!("{rel}.py"), format!("{rel}/__init__.py"));
            let hits: Vec<&String> = syms
                .files()
                .filter(|f| {
                    *f == &file
                        || *f == &pkg
                        || f.ends_with(&format!("/{file}"))
                        || f.ends_with(&format!("/{pkg}"))
                })
                .collect();
            if !hits.is_empty() {
                return (hits.len() == 1).then(|| hits[0].clone());
            }
        }
        None
    }

    /// Whether the Python module `a.b.c` may be the project's from some
    /// import root. Erring toward "project" only leaves a call to the name
    /// strategies.
    pub fn python_project_module_any_root(&self, syms: &Symbols, module: &str) -> bool {
        if module.starts_with('.') {
            return true;
        }
        let parts: Vec<&str> = module.split('.').collect();
        (0..parts.len()).any(|i| {
            let rel = parts[i..].join("/");
            let (file, pkg) = (format!("{rel}.py"), format!("{rel}/__init__.py"));
            syms.files().any(|f| {
                f == &file
                    || f == &pkg
                    || f.ends_with(&format!("/{file}"))
                    || f.ends_with(&format!("/{pkg}"))
            })
        })
    }

    fn resolve_python(&self, from: &str, spec: &str) -> Option<String> {
        self.python.resolve(spec, from)
    }

    fn resolve_go(&self, spec: &str) -> Vec<String> {
        for (module, dir) in &self.go_modules {
            let sub = if spec == module {
                ""
            } else if let Some(rest) = spec.strip_prefix(&format!("{module}/")) {
                rest
            } else {
                continue;
            };
            let target = normalize(&join(dir, sub)).unwrap_or_default();
            return self
                .files_in_dir(&target)
                .iter()
                .filter(|f| f.ends_with(".go") && !f.ends_with("_test.go"))
                .cloned()
                .collect();
        }
        Vec::new()
    }

    /// The directory of the Rust crate root (`src/`) a file belongs to.
    fn rust_crate_dir(&self, from: &str) -> String {
        self.rust_crates
            .iter()
            .filter(|(_, dir)| dir.is_empty() || from.starts_with(&format!("{dir}/")))
            .max_by_key(|(_, dir)| dir.len())
            .map(|(_, d)| d.clone())
            .unwrap_or_else(|| {
                from.find("src/")
                    .map(|i| from[..i + 3].to_string())
                    .unwrap_or_else(|| dir_of(from).to_string())
            })
    }

    /// Directory holding the child modules of the module defined in `file`.
    fn rust_module_dir(file: &str) -> String {
        let name = file.rsplit('/').next().unwrap_or(file);
        if matches!(name, "mod.rs" | "lib.rs" | "main.rs") {
            dir_of(file).to_string()
        } else {
            file.strip_suffix(".rs").unwrap_or(file).to_string()
        }
    }

    fn rust_module_file(syms: &Symbols, dir: &str) -> Option<String> {
        for cand in [format!("{dir}.rs"), format!("{dir}/mod.rs")] {
            if syms.file_exists(&cand) {
                return Some(cand);
            }
        }
        if syms.file_exists(&format!("{dir}/lib.rs")) {
            return Some(format!("{dir}/lib.rs"));
        }
        None
    }

    fn resolve_rust(&self, syms: &Symbols, from: &str, spec: &str) -> Option<String> {
        let mut segs: Vec<&str> = spec.split("::").filter(|s| !s.is_empty()).collect();
        if segs.is_empty() {
            return None;
        }
        let mut dir = match segs[0] {
            "crate" => {
                segs.remove(0);
                self.rust_crate_dir(from)
            }
            "self" => {
                segs.remove(0);
                Self::rust_module_dir(from)
            }
            "super" => {
                let mut d = Self::rust_module_dir(from);
                while segs.first() == Some(&"super") {
                    segs.remove(0);
                    d = dir_of(&d).to_string();
                }
                d
            }
            first => match self.rust_crates.iter().find(|(n, _)| n == first) {
                Some((_, d)) => {
                    segs.remove(0);
                    d.clone()
                }
                // A bare path is relative to the current module (`mod x;` / 2015 paths).
                None => Self::rust_module_dir(from),
            },
        };
        if segs.is_empty() {
            for root in ["lib.rs", "main.rs", "mod.rs"] {
                let p = join(&dir, root);
                if syms.file_exists(&p) {
                    return Some(p);
                }
            }
            return Self::rust_module_file(syms, &dir);
        }
        let mut found = None;
        for seg in segs {
            dir = join(&dir, seg);
            match Self::rust_module_file(syms, &dir) {
                Some(f) => found = Some(f),
                // The rest names items inside the last module file found.
                None => break,
            }
        }
        found
    }

    fn resolve_dart(&self, syms: &Symbols, from: &str, spec: &str) -> Option<String> {
        if let Some(rest) = spec.strip_prefix("package:") {
            let (pkg, path) = rest.split_once('/')?;
            let dir = self.dart_packages.get(pkg)?;
            let p = normalize(&join(dir, &format!("lib/{path}")))?;
            return syms.file_exists(&p).then_some(p);
        }
        if spec.starts_with("dart:") {
            return None;
        }
        let p = normalize(&join(dir_of(from), spec))?;
        syms.file_exists(&p).then_some(p)
    }

    /// Relative to the importing file, then the project root, then the
    /// closest file whose path ends with the specifier.
    fn resolve_relative_or_suffix(
        &self,
        syms: &Symbols,
        from: &str,
        spec: &str,
        exts: &[&str],
    ) -> Vec<String> {
        let spec = spec.trim_start_matches("./");
        for ext in exts {
            for base in [join(dir_of(from), spec), spec.to_string()] {
                if let Some(p) = normalize(&format!("{base}{ext}")) {
                    if let Some(f) = syms.find_file(&p) {
                        return vec![f.to_string()];
                    }
                }
            }
        }
        for ext in exts {
            let suffix = format!("/{}{ext}", spec.trim_start_matches("../"));
            let mut found: Vec<&String> = syms.files().filter(|f| f.ends_with(&suffix)).collect();
            if !found.is_empty() {
                found.sort_by_key(|f| (std::cmp::Reverse(common_prefix_len(f, from)), f.len()));
                return vec![found[0].clone()];
            }
        }
        Vec::new()
    }
}

/// Length of the shared leading directory path of two files.
pub fn common_prefix_len(a: &str, b: &str) -> usize {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(x, y)| x == y)
        .map(|(x, _)| x.len() + 1)
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("a/./b/../c").as_deref(), Some("a/c"));
        assert_eq!(normalize("../x"), None);
        assert_eq!(common_prefix_len("src/a/b.ts", "src/a/c.ts"), 6);
    }

    #[test]
    fn strips_jsonc() {
        let j = jsonc_to_json("{\n // c\n \"a\": \"http://x\", /* b */ \"b\": [1,],\n}");
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["a"], "http://x");
    }
}
