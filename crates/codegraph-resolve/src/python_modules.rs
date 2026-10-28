//! Python module resolution. Python imports name modules (`pkg.sub.mod`,
//! `..sibling`), which the interpreter finds by searching `sys.path` roots,
//! so the answer depends on where packages start, not on any file that
//! merely ends in `pkg/sub/mod.py`.
//!
//! Two kinds of root, both derived from evidence:
//! - project roots, on the path of any code in the repo: the repo root; each
//!   service root (a directory with its own `pyproject.toml`, `setup.cfg` or
//!   `setup.py`); the `src/` of any of those; and the package directories
//!   their build config declares. These answer an importer anywhere, but
//!   only unambiguously.
//! - local roots: the parent of every top-level regular package, a script's
//!   own directory, a namespace package's parent seen from inside it. These
//!   answer only importers within their reach: `tests/fixtures/requests.py`
//!   is never the application's `requests`.

use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::LazyLock;

/// Directory names that hold code that is not the project's importable API.
/// A service root under one answers only importers inside that directory.
const NON_PROJECT_DIRS: &[&str] = &[
    "tests",
    "test",
    "testing",
    "fixtures",
    "testdata",
    "examples",
    "example",
    "samples",
    "vendor",
    "third_party",
];

fn parent(dir: &str) -> &str {
    dir.rfind('/').map_or("", |i| &dir[..i])
}

fn join(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

/// Whether `dir` is `scope` or inside it.
fn within(dir: &str, scope: &str) -> bool {
    scope.is_empty() || dir == scope || dir.starts_with(&format!("{scope}/"))
}

#[derive(Default)]
pub struct PythonModules {
    files: HashSet<String>,
    /// Directories holding an `__init__.py`.
    packages: HashSet<String>,
    /// Every directory holding a `.py` file at any depth.
    dirs: HashSet<String>,
    project_roots: HashSet<String>,
    /// Local root → the directory whose importers it may answer.
    local_roots: HashMap<String, String>,
}

#[derive(PartialEq)]
enum Head {
    Concrete,
    Namespace,
}

impl PythonModules {
    pub fn build<'a>(root: &Path, all_files: impl Iterator<Item = &'a String>) -> Self {
        let mut m = PythonModules::default();
        m.dirs.insert(String::new());
        for f in all_files {
            if !f.ends_with(".py") {
                continue;
            }
            m.files.insert(f.clone());
            let dir = parent(f);
            if f == "__init__.py" || f.ends_with("/__init__.py") {
                m.packages.insert(dir.to_string());
            }
            let mut d = dir;
            while !d.is_empty() && m.dirs.insert(d.to_string()) {
                d = parent(d);
            }
        }
        if m.files.is_empty() {
            return m;
        }
        // Where a service's roots may answer from: everywhere (None), only
        // itself when a package encloses it, or the nearest enclosing
        // non-project directory.
        let reach = |m: &PythonModules, service: &str| -> Option<String> {
            if m.packages.contains(service) {
                return Some(service.to_string());
            }
            let mut a = parent(service);
            while !a.is_empty() {
                if m.packages.contains(a) {
                    return Some(service.to_string());
                }
                if NON_PROJECT_DIRS.contains(&a.rsplit('/').next().unwrap_or(a)) {
                    return Some(a.to_string());
                }
                a = parent(a);
            }
            None
        };
        let mut local: HashMap<String, String> = HashMap::new();
        let add_local = |local: &mut HashMap<String, String>, root: &str, reach: &str| {
            // Two claims on one root: the wider reach wins.
            match local.get(root) {
                Some(prior) if !within(prior, reach) => {}
                _ => {
                    local.insert(root.to_string(), reach.to_string());
                }
            }
        };
        for dir in &m.packages {
            let mut top = dir.as_str();
            while !top.is_empty() && m.packages.contains(parent(top)) {
                top = parent(top);
            }
            if !top.is_empty() {
                add_local(&mut local, parent(top), parent(top));
            }
        }
        m.project_roots.insert(String::new());
        let mut dirs: Vec<String> = m.dirs.iter().cloned().collect();
        dirs.sort();
        for service in dirs {
            let has_config = m.files.contains(&join(&service, "setup.py"))
                || root.join(join(&service, "pyproject.toml")).is_file()
                || root.join(join(&service, "setup.cfg")).is_file();
            if !has_config && !service.is_empty() {
                continue;
            }
            let r = if service.is_empty() {
                None
            } else {
                reach(&m, &service)
            };
            let mut roots = vec![service.clone()];
            let src = join(&service, "src");
            if m.dirs.contains(&src) && !m.packages.contains(&src) {
                roots.push(src);
            }
            for declared in declared_roots(root, &service) {
                if m.dirs.contains(&declared) {
                    roots.push(declared);
                }
            }
            for x in roots {
                match &r {
                    None => {
                        m.project_roots.insert(x);
                    }
                    Some(reach) => add_local(&mut local, &x, reach),
                }
            }
        }
        for r in &m.project_roots {
            local.remove(r);
        }
        m.local_roots = local;
        m
    }

    /// The module file at `base`: a regular package's `__init__.py` beats
    /// a module `base.py`; a bare directory has no file of its own.
    fn module_at(&self, base: &str) -> Option<String> {
        let init = join(base, "__init__.py");
        if self.files.contains(&init) {
            return Some(init);
        }
        let file = format!("{base}.py");
        self.files.contains(&file).then_some(file)
    }

    /// The module for the dotted path `rel` (slash-joined) under `root`:
    /// every prefix must be a package (regular or namespace).
    fn module_under(&self, root: &str, rel: &str) -> Option<String> {
        let parts: Vec<&str> = rel.split('/').collect();
        let mut base = root.to_string();
        for p in &parts[..parts.len() - 1] {
            base = join(&base, p);
            if self.files.contains(&join(&base, "__init__.py")) {
                continue;
            }
            if self.files.contains(&format!("{base}.py")) || !self.dirs.contains(&base) {
                return None;
            }
        }
        self.module_at(&join(root, rel))
    }

    fn head_kind(&self, root: &str, head: &str) -> Option<Head> {
        let base = join(root, head);
        if self.module_at(&base).is_some() {
            return Some(Head::Concrete);
        }
        self.dirs.contains(&base).then_some(Head::Namespace)
    }

    /// The file of the module `spec` names, imported from `from`. None when
    /// the module is not in the index, a relative import climbs above its
    /// top-level package, or two roots that don't contain the importer both
    /// provide it.
    pub fn resolve(&self, spec: &str, from: &str) -> Option<String> {
        static SPEC: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\.*[\w.]*$").unwrap());
        if spec.is_empty() || !SPEC.is_match(spec) {
            return None;
        }
        let importer = parent(from);
        let dots = spec.len() - spec.trim_start_matches('.').len();
        if dots > 0 {
            let mut base = importer.to_string();
            for _ in 1..dots {
                // Python refuses to climb out of the top-level package.
                if self.packages.contains(&base) && !self.packages.contains(parent(&base)) {
                    return None;
                }
                if base.is_empty() {
                    return None;
                }
                base = parent(&base).to_string();
            }
            let rest = &spec[dots..];
            if !rest.is_empty() {
                return self.module_under(&base, &rest.replace('.', "/"));
            }
            let init = join(&base, "__init__.py");
            return self.files.contains(&init).then_some(init);
        }
        let rel = spec.replace('.', "/");
        let head = spec.split('.').next().unwrap_or(spec);
        // Roots containing the importer, nearest first.
        let mut containing: Vec<String> = Vec::new();
        let mut dir = Some(importer);
        while let Some(d) = dir {
            let head_dir = join(d, head);
            let is_root = self.project_roots.contains(d)
                || self.local_roots.contains_key(d)
                || (!self.packages.contains(d)
                    && (d == importer
                        || importer == head_dir
                        || importer.starts_with(&format!("{head_dir}/"))));
            if is_root {
                containing.push(d.to_string());
            }
            dir = (!d.is_empty()).then(|| parent(d));
        }
        let mut head_is_local = false;
        for root in &containing {
            if let Some(hit) = self.module_under(root, &rel) {
                return Some(hit);
            }
            if self.head_kind(root, head) == Some(Head::Concrete) {
                head_is_local = true;
            }
        }
        if head_is_local {
            return None;
        }
        // A root elsewhere whose reach includes the importer: one holding
        // `head` concretely shadows any namespace portion; either way the
        // answer must be unambiguous.
        let reaches = |root: &str| {
            !containing.iter().any(|c| c == root)
                && (self.project_roots.contains(root)
                    || self
                        .local_roots
                        .get(root)
                        .is_some_and(|r| within(importer, r)))
        };
        let mut roots: Vec<&String> = self
            .project_roots
            .iter()
            .chain(self.local_roots.keys())
            .collect();
        roots.sort();
        roots.dedup();
        let mut concrete = Vec::new();
        let mut namespace = Vec::new();
        for r in roots {
            match self.head_kind(r, head) {
                Some(Head::Concrete) if reaches(r) => concrete.push(r),
                Some(Head::Namespace) if reaches(r) => namespace.push(r),
                _ => {}
            }
        }
        if !concrete.is_empty() {
            return match concrete.as_slice() {
                [one] => self.module_under(one, &rel),
                _ => None,
            };
        }
        let mut found: Option<String> = None;
        for r in namespace {
            let Some(hit) = self.module_under(r, &rel) else {
                continue;
            };
            if found.as_ref().is_some_and(|f| *f != hit) {
                return None;
            }
            found = Some(hit);
        }
        found
    }
}

/// `[table]` sections of a TOML / INI file, with the body under each.
fn ini_tables(content: &str) -> Vec<(String, String)> {
    static HEADER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^[ \t]*\[([^\[\]\n]+)\][ \t]*(?:#.*)?$").unwrap());
    let heads: Vec<_> = HEADER.captures_iter(content).collect();
    heads
        .iter()
        .enumerate()
        .map(|(i, h)| {
            let whole = h.get(0).unwrap();
            let end = heads
                .get(i + 1)
                .map_or(content.len(), |n| n.get(0).unwrap().start());
            (
                h[1].trim().replace(['"', '\''], ""),
                content[whole.end()..end].to_string(),
            )
        })
        .collect()
}

/// Package roots a service's build config declares, relative to the repo
/// root: setuptools `packages.find where` and `package-dir`, poetry
/// `packages = [{ from = … }]`, hatch wheel `packages`, and `setup.cfg`'s
/// `package_dir` and `packages.find where`.
fn declared_roots(root: &Path, service: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut add = |raw: &str| {
        let d = raw.trim().trim_matches(['"', '\'']);
        let d = d.strip_prefix("./").unwrap_or(d);
        let d = if d == "." { "" } else { d }.trim_end_matches('/');
        if d.is_empty() || d.starts_with("..") || d.starts_with('/') {
            return;
        }
        out.push(join(service, d));
    };
    let re = |p: &str| Regex::new(p).unwrap();
    if let Ok(toml) = std::fs::read_to_string(root.join(join(service, "pyproject.toml"))) {
        for (table, body) in ini_tables(&toml) {
            match table.as_str() {
                "tool.setuptools.packages.find" => {
                    if let Some(c) = re(r"(?m)^\s*where\s*=\s*\[([^\]]*)\]").captures(&body) {
                        c[1].split(',').for_each(&mut add);
                    }
                }
                "tool.setuptools.package-dir" => {
                    if let Some(c) = re(r#"(?m)^\s*["']{2}\s*=\s*["']([^"']+)["']"#).captures(&body)
                    {
                        add(&c[1]);
                    }
                }
                "tool.setuptools" => {
                    if let Some(c) =
                        re(r#"package-dir\s*=\s*\{[^}]*?["']{2}\s*=\s*["']([^"']+)["']"#)
                            .captures(&body)
                    {
                        add(&c[1]);
                    }
                }
                "tool.poetry" => {
                    if let Some(c) = re(r"(?ms)^\s*packages\s*=\s*\[(.*?)\]\s*$").captures(&body) {
                        for m in re(r#"\bfrom\s*=\s*["']([^"']+)["']"#).captures_iter(&c[1]) {
                            add(&m[1]);
                        }
                    }
                }
                "tool.hatch.build.targets.wheel" => {
                    if let Some(c) = re(r"(?m)^\s*packages\s*=\s*\[([^\]]*)\]").captures(&body) {
                        for item in c[1].split(',') {
                            let p = item.trim().trim_matches(['"', '\'']);
                            if let Some((dir, _)) = p.rsplit_once('/') {
                                add(dir);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    if let Ok(cfg) = std::fs::read_to_string(root.join(join(service, "setup.cfg"))) {
        for (table, body) in ini_tables(&cfg) {
            match table.as_str() {
                "options" => {
                    if let Some(c) =
                        re(r"(?m)^\s*package_dir\s*=\s*(?:\n\s+)?=\s*(\S+)\s*$").captures(&body)
                    {
                        add(&c[1]);
                    }
                }
                "options.packages.find" => {
                    if let Some(c) = re(r"(?m)^\s*where\s*=\s*(\S+)\s*$").captures(&body) {
                        add(&c[1]);
                    }
                }
                _ => {}
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::PythonModules;
    use std::path::Path;

    fn index(files: &[&str]) -> PythonModules {
        let files: Vec<String> = files.iter().map(|s| s.to_string()).collect();
        PythonModules::build(Path::new("/nonexistent"), files.iter())
    }

    #[test]
    fn roots_and_reach() {
        let m = index(&[
            "app/__init__.py",
            "app/email.py",
            "tests/fixtures/requests.py",
            "tests/test_x.py",
            "pkg/a/__init__.py",
            "pkg/a/b.py",
        ]);
        assert_eq!(
            m.resolve("app.email", "tests/test_x.py").as_deref(),
            Some("app/email.py")
        );
        assert_eq!(m.resolve("requests", "client.py"), None);
        assert_eq!(
            m.resolve(".b", "pkg/a/__init__.py").as_deref(),
            Some("pkg/a/b.py")
        );
        assert_eq!(m.resolve("...x", "pkg/a/b.py"), None);
        assert_eq!(m.resolve("a.b", "pkg/x.py").as_deref(), Some("pkg/a/b.py"));
    }
}
