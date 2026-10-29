//! The project's Cargo crates: the root package, the workspace's members and
//! the path dependencies they name, which of them each depends on, and the
//! crates from outside the project their manifests name.

use std::collections::{HashMap, HashSet};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DepKind {
    Normal,
    Dev,
    Build,
    Workspace,
}

#[derive(Debug, Clone)]
struct ManifestDep {
    /// The name code writes for it (`futures_util`).
    key: String,
    /// The package it is: its key, unless `package = "…"` renames it.
    pkg: String,
    /// Its `path = "…"`, from the manifest's directory.
    path: Option<String>,
    /// `workspace = true`: the workspace's `[workspace.dependencies]` declares it.
    workspace: bool,
    kind: DepKind,
}

/// A Cargo manifest's dependencies, however each is written: `foo = "1"`, an
/// inline table, dotted keys (`foo.workspace = true`) or a `[dependencies.foo]`
/// table of its own.
fn manifest_dependencies(text: &str) -> Vec<ManifestDep> {
    let mut deps: Vec<ManifestDep> = Vec::new();
    let mut index: HashMap<(u8, String), usize> = HashMap::new();
    let mut entry = |deps: &mut Vec<ManifestDep>, kind: DepKind, name: &str| -> usize {
        let key = name.replace('-', "_");
        *index.entry((kind as u8, key.clone())).or_insert_with(|| {
            deps.push(ManifestDep {
                key: key.clone(),
                pkg: key,
                path: None,
                workspace: false,
                kind,
            });
            deps.len() - 1
        })
    };
    fn quoted_after<'a>(fields: &'a str, key: &str) -> Option<&'a str> {
        let mut from = 0;
        while let Some(i) = fields[from..].find(key) {
            let at = from + i;
            from = at + key.len();
            if at > 0 && fields.as_bytes()[at - 1].is_ascii_alphanumeric() {
                continue;
            }
            let rest = fields[at + key.len()..].trim_start();
            let Some(rest) = rest.strip_prefix('=') else {
                continue;
            };
            let rest = rest.trim_start().strip_prefix('"')?;
            return rest.split('"').next();
        }
        None
    }
    let read = |dep: &mut ManifestDep, fields: &str| {
        if let Some(p) = quoted_after(fields, "package") {
            dep.pkg = p.replace('-', "_");
        }
        if let Some(p) = quoted_after(fields, "path") {
            dep.path = Some(p.replace('\\', "/"));
        }
        let compact: String = fields.chars().filter(|c| !c.is_whitespace()).collect();
        if compact.contains("workspace=true") {
            dep.workspace = true;
        }
    };
    let mut kind: Option<DepKind> = None;
    let mut table: Option<usize> = None;
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let h = header.trim();
            let (ws, rest) = match h.strip_prefix("workspace.") {
                Some(r) => (true, r),
                None => (false, h),
            };
            let rest = if !ws && rest.starts_with("target.") {
                // `target.'cfg(unix)'.dependencies`
                rest.rfind(".dependencies")
                    .or_else(|| rest.rfind(".dev-dependencies"))
                    .or_else(|| rest.rfind(".build-dependencies"))
                    .map_or("", |i| &rest[i + 1..])
            } else {
                rest
            };
            let (of, sub) = if let Some(r) = rest.strip_prefix("dev-dependencies") {
                (Some(DepKind::Dev), r)
            } else if let Some(r) = rest.strip_prefix("build-dependencies") {
                (Some(DepKind::Build), r)
            } else if let Some(r) = rest.strip_prefix("dependencies") {
                (Some(DepKind::Normal), r)
            } else {
                (None, "")
            };
            let of = of
                .filter(|_| sub.is_empty() || sub.starts_with('.'))
                .map(|k| if ws { DepKind::Workspace } else { k });
            let sub = sub.strip_prefix('.').unwrap_or("");
            kind = of.filter(|_| sub.is_empty());
            table = match of {
                Some(k) if !sub.is_empty() => Some(entry(&mut deps, k, sub)),
                _ => None,
            };
            continue;
        }
        if let Some(t) = table {
            read(&mut deps[t], line);
            continue;
        }
        let Some(k) = kind else { continue };
        // `"name" = …`, `name.workspace = true`, `name = { … }`
        let Some((lhs, rhs)) = line.split_once('=') else {
            continue;
        };
        let lhs = lhs.trim().trim_matches('"');
        let (name, dotted) = match lhs.split_once('.') {
            Some((n, d)) => (n.trim(), Some(d.trim())),
            None => (lhs, None),
        };
        if name.is_empty()
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            continue;
        }
        let i = entry(&mut deps, k, name);
        let fields = match dotted {
            Some(d) => format!("{d} = {}", rhs.trim()),
            None => rhs.trim().to_string(),
        };
        read(&mut deps[i], &fields);
    }
    deps
}

/// `[package] name = "…"`, as code writes it (`-` → `_`).
fn package_name(text: &str) -> Option<String> {
    section(text, "package").and_then(|s| {
        s.lines().find_map(|l| {
            let l = l.trim();
            let rest = l
                .strip_prefix("name")?
                .trim_start()
                .strip_prefix('=')?
                .trim();
            let q = rest.chars().next().filter(|c| *c == '"' || *c == '\'')?;
            rest[1..].split(q).next().map(|n| n.trim().to_string())
        })
    })
}

fn section<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let want = format!("[{name}]");
    let mut start = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let t = line.trim();
        match start {
            None if t == want => start = Some(offset + line.len()),
            Some(s) if t.starts_with('[') && t.ends_with(']') => return Some(&text[s..offset]),
            _ => {}
        }
        offset += line.len();
    }
    start.map(|s| &text[s..])
}

/// `[workspace] members = [ … ]`, globs expanded against the directories on disk.
fn workspace_members(root: &Path, text: &str) -> Vec<String> {
    let Some(ws) = section(text, "workspace") else {
        return Vec::new();
    };
    let Some(at) = ws.find("members") else {
        return Vec::new();
    };
    let rest = &ws[at + "members".len()..];
    let Some(rest) = rest.trim_start().strip_prefix('=') else {
        return Vec::new();
    };
    let Some(rest) = rest.trim_start().strip_prefix('[') else {
        return Vec::new();
    };
    // The quoted values up to the closing `]`, comments aside.
    let mut values = Vec::new();
    let mut quote: Option<char> = None;
    let mut cur = String::new();
    let mut comment = false;
    for ch in rest.chars() {
        match quote {
            Some(q) if ch == q => {
                values.push(std::mem::take(&mut cur));
                quote = None;
            }
            Some(_) => cur.push(ch),
            None if comment => comment = ch != '\n',
            None if ch == '#' => comment = true,
            None if ch == '"' || ch == '\'' => quote = Some(ch),
            None if ch == ']' => break,
            None => {}
        }
    }
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for raw in values {
        let m = raw.trim();
        if m.is_empty() {
            continue;
        }
        let m = m.replace('\\', "/").trim_end_matches('/').to_string();
        let found = if m.contains(['*', '?', '[', '{']) {
            expand_glob(root, &m)
        } else {
            vec![m]
        };
        for f in found {
            if seen.insert(f.clone()) {
                out.push(f);
            }
        }
    }
    out
}

fn expand_glob(root: &Path, pattern: &str) -> Vec<String> {
    let Ok(glob) = globset::Glob::new(pattern) else {
        return Vec::new();
    };
    let matcher = glob.compile_matcher();
    let prefix: String = {
        let first = pattern.find(['*', '?', '[', '{']).unwrap_or(pattern.len());
        pattern[..first]
            .rsplit_once('/')
            .map_or(String::new(), |(p, _)| p.to_string())
    };
    let mut out = Vec::new();
    fn walk(root: &Path, dir: &str, depth: u8, m: &globset::GlobMatcher, out: &mut Vec<String>) {
        if depth > 5 {
            return;
        }
        let Ok(rd) = std::fs::read_dir(root.join(dir)) else {
            return;
        };
        let mut names: Vec<String> = rd
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for name in names {
            if name.starts_with('.')
                || matches!(name.as_str(), "target" | "node_modules" | "dist" | "build")
            {
                continue;
            }
            let rel = if dir.is_empty() {
                name
            } else {
                format!("{dir}/{name}")
            };
            if m.is_match(&rel) {
                out.push(rel.clone());
            }
            walk(root, &rel, depth + 1, m, out);
        }
    }
    walk(root, &prefix, 0, &matcher, &mut out);
    out
}

#[derive(Debug, Clone)]
pub struct Crate {
    /// Its package name as code writes it (`tokio_test`).
    pub name: String,
    /// Each dependency of any kind that is a project crate: the name code writes → that crate's directory.
    pub keys: HashMap<String, String>,
    /// The project crates its `[dependencies]` name, by directory: the ones it can re-export from.
    deps: HashSet<String>,
}

#[derive(Debug, Default)]
pub struct CrateGraph {
    /// The project's crates by directory: the root package's is ''.
    pub crates: HashMap<String, Crate>,
    /// A crate's directory → itself and every project crate it depends on, transitively.
    reach: HashMap<String, HashSet<String>>,
    /// The project's own crate names: the root package and the workspace's members.
    pub project: HashSet<String>,
    /// The crates the manifests depend on, by the name code writes them, less the project's own.
    pub dependencies: HashSet<String>,
}

fn read(root: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(root.join(rel)).ok()
}

/// `dir` joined with `rel`, as a project directory; none when it leaves the project.
fn join(dir: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = if dir.is_empty() {
        Vec::new()
    } else {
        dir.split('/').collect()
    };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            s => parts.push(s),
        }
    }
    if rel.starts_with('/') {
        return None;
    }
    Some(parts.join("/"))
}

impl CrateGraph {
    pub fn load(root: &Path) -> CrateGraph {
        let mut g = CrateGraph::default();
        let root_toml = read(root, "Cargo.toml").unwrap_or_default();
        let members = workspace_members(root, &root_toml);
        let mut manifests: HashMap<String, String> = HashMap::new();
        let mut by_name: HashMap<String, String> = HashMap::new();
        let add = |g: &mut CrateGraph,
                   manifests: &mut HashMap<String, String>,
                   by_name: &mut HashMap<String, String>,
                   dir: &str|
         -> bool {
            if g.crates.contains_key(dir) {
                return true;
            }
            let path = if dir.is_empty() {
                "Cargo.toml".to_string()
            } else {
                format!("{dir}/Cargo.toml")
            };
            let Some(text) = read(root, &path) else {
                return false;
            };
            let Some(name) = package_name(&text).map(|n| n.replace('-', "_")) else {
                return false;
            };
            by_name
                .entry(name.clone())
                .or_insert_with(|| dir.to_string());
            g.crates.insert(
                dir.to_string(),
                Crate {
                    name,
                    keys: HashMap::new(),
                    deps: HashSet::new(),
                },
            );
            manifests.insert(dir.to_string(), text);
            true
        };
        if add(&mut g, &mut manifests, &mut by_name, "") {
            g.project.insert(g.crates[""].name.clone());
        }
        for m in &members {
            let dir = m.trim_start_matches("./").to_string();
            let dir = if dir == "." { String::new() } else { dir };
            if add(&mut g, &mut manifests, &mut by_name, &dir) {
                g.project.insert(g.crates[&dir].name.clone());
            }
        }
        let inherited: HashMap<String, ManifestDep> = manifest_dependencies(&root_toml)
            .into_iter()
            .filter(|d| d.kind == DepKind::Workspace)
            .map(|d| (d.key.clone(), d))
            .collect();
        let mut queue: Vec<String> = g.crates.keys().cloned().collect();
        queue.sort();
        let mut i = 0;
        while i < queue.len() {
            let dir = queue[i].clone();
            i += 1;
            let deps = manifest_dependencies(&manifests[&dir]);
            for dep in deps {
                if dep.kind == DepKind::Workspace {
                    continue;
                }
                let shared = dep.workspace.then(|| inherited.get(&dep.key)).flatten();
                let at = match shared {
                    Some(s) => s.path.as_deref().and_then(|p| join("", p)),
                    None => dep.path.as_deref().and_then(|p| join(&dir, p)),
                };
                if let Some(at) = &at {
                    if !g.crates.contains_key(at) && add(&mut g, &mut manifests, &mut by_name, at) {
                        queue.push(at.clone());
                    }
                }
                let to = match &at {
                    Some(at) if g.crates.contains_key(at) => Some(at.clone()),
                    _ => by_name.get(shared.map_or(&dep.pkg, |s| &s.pkg)).cloned(),
                };
                let Some(to) = to.filter(|t| *t != dir) else {
                    continue;
                };
                let c = g.crates.get_mut(&dir).unwrap();
                c.keys.insert(dep.key.clone(), to.clone());
                if dep.kind == DepKind::Normal {
                    c.deps.insert(to);
                }
            }
        }
        let dirs: Vec<String> = g.crates.keys().cloned().collect();
        for dir in dirs {
            let mut seen: HashSet<String> = [dir.clone()].into();
            let mut pending = vec![dir.clone()];
            while let Some(next) = pending.pop() {
                for to in &g.crates[&next].deps {
                    if seen.insert(to.clone()) {
                        pending.push(to.clone());
                    }
                }
            }
            g.reach.insert(dir, seen);
        }
        // The crates the manifests depend on: the root's and the workspace members'.
        let mut manifest_dirs = vec![String::new()];
        manifest_dirs.extend(members.iter().cloned());
        for dir in manifest_dirs {
            let path = if dir.is_empty() {
                "Cargo.toml".to_string()
            } else {
                format!("{dir}/Cargo.toml")
            };
            for d in manifest_dependencies(&read(root, &path).unwrap_or_default()) {
                g.dependencies.insert(d.key);
            }
        }
        for own in &g.project {
            g.dependencies.remove(own);
        }
        g
    }

    /// The directory of the crate `file` is in: the deepest one that holds it, the root package's (''), or none.
    pub fn crate_dir_of(&self, file: &str) -> Option<&str> {
        let mut best: Option<&str> = self.crates.contains_key("").then_some("");
        for dir in self.crates.keys() {
            if !dir.is_empty()
                && file.starts_with(dir.as_str())
                && file.as_bytes().get(dir.len()) == Some(&b'/')
                && best.is_none_or(|b| dir.len() > b.len())
            {
                best = Some(dir);
            }
        }
        best
    }

    pub fn crate_of(&self, file: &str) -> Option<&Crate> {
        self.crate_dir_of(file).and_then(|d| self.crates.get(d))
    }

    /// Whether a `use` path from one of `roots` in `from_file` can name an item in `target_file`, by its crate.
    pub fn use_can_reach<'r>(
        &self,
        roots: impl IntoIterator<Item = &'r str>,
        target_file: &str,
        from_file: &str,
    ) -> bool {
        let (Some(at), Some(own)) = (self.crate_dir_of(target_file), self.crate_dir_of(from_file))
        else {
            return true;
        };
        let krate = &self.crates[own];
        for root in roots {
            let from = if matches!(root, "crate" | "self" | "super") || root == krate.name {
                Some(own)
            } else {
                krate.keys.get(root).map(String::as_str)
            };
            match from {
                None => return true,
                Some(f) if self.reach[f].contains(at) => return true,
                _ => {}
            }
        }
        false
    }

    /// Whether a path's first segment names std, or a crate the manifests depend on that the project doesn't hold.
    pub fn is_outside_crate(&self, root: &str, from_file: &str) -> bool {
        if super::STD_ROOTS.contains(&root) {
            return true;
        }
        if matches!(root, "crate" | "self" | "super") || self.project.contains(root) {
            return false;
        }
        if !self.dependencies.contains(root) {
            return false;
        }
        if self
            .crate_of(from_file)
            .is_some_and(|c| c.keys.contains_key(root))
        {
            return false;
        }
        !self.crates.values().any(|c| c.name == root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_dependencies_however_written() {
        let deps = manifest_dependencies(
            "[package]\nname = \"a\"\n\n[dependencies]\nfoo-bar = \"1\"\nshared = { package = \"shared-types\", path = \"../types\" }\nhelper.workspace = true\n\n[dependencies.baz]\npath = \"../baz\"\n\n[dev-dependencies]\nt = \"1\"\n[workspace.dependencies]\nhelper = { path = \"helper\" }\n",
        );
        let get =
            |k: &str, kind: DepKind| deps.iter().find(|d| d.key == k && d.kind == kind).unwrap();
        assert_eq!(get("foo_bar", DepKind::Normal).pkg, "foo_bar");
        let s = get("shared", DepKind::Normal);
        assert_eq!(s.pkg, "shared_types");
        assert_eq!(s.path.as_deref(), Some("../types"));
        assert!(get("helper", DepKind::Normal).workspace);
        assert_eq!(get("baz", DepKind::Normal).path.as_deref(), Some("../baz"));
        assert_eq!(get("t", DepKind::Dev).kind, DepKind::Dev);
        assert_eq!(
            get("helper", DepKind::Workspace).path.as_deref(),
            Some("helper")
        );
    }

    #[test]
    fn reads_workspace_members_past_comments() {
        let m = workspace_members(
            Path::new("/nonexistent"),
            "[workspace]\nresolver = \"2\"\nmembers = [\n  \"tokio\",\n\n  # Internal\n  \"benches\",\n]\n\n[patch.crates-io]\n",
        );
        assert_eq!(m, vec!["tokio", "benches"]);
    }

    #[test]
    fn joins_paths() {
        assert_eq!(join("app", "../types").as_deref(), Some("types"));
        assert_eq!(join("", "helper").as_deref(), Some("helper"));
        assert_eq!(join("", "../x"), None);
    }
}
