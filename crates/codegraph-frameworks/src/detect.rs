//! Which frameworks a project uses, from its dependency manifests and layout.

use std::collections::HashSet;
use std::path::Path;

/// Everything detection looks at, read once per index run.
#[derive(Debug, Default)]
pub struct ProjectInfo {
    /// Lowercased dependency names from every manifest found.
    pub deps: HashSet<String>,
    /// Lowercased raw manifest text, for ecosystems without a parsed dependency list.
    pub manifest_text: String,
    /// Project-relative paths of indexed files.
    pub files: Vec<String>,
}

const MANIFESTS: &[&str] = &[
    "package.json",
    "requirements.txt",
    "requirements-dev.txt",
    "pyproject.toml",
    "Pipfile",
    "setup.py",
    "setup.cfg",
    "Gemfile",
    "composer.json",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "go.mod",
    "Cargo.toml",
    "Package.swift",
    "build.sbt",
    "pubspec.yaml",
];

fn is_manifest(name: &str) -> bool {
    MANIFESTS.contains(&name) || name.ends_with(".csproj")
}

impl ProjectInfo {
    /// Scan manifests under `root` (bounded depth, skipping dependency dirs).
    pub fn scan(root: &Path, files: &[String]) -> Self {
        let mut info = ProjectInfo {
            files: files.to_vec(),
            ..Default::default()
        };
        let mut stack = vec![(root.to_path_buf(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                let Ok(ft) = e.file_type() else { continue };
                if ft.is_dir() {
                    if depth < 5
                        && !name.starts_with('.')
                        && !matches!(
                            name.as_str(),
                            "node_modules"
                                | "target"
                                | "vendor"
                                | "dist"
                                | "build"
                                | "venv"
                                | "__pycache__"
                        )
                    {
                        stack.push((e.path(), depth + 1));
                    }
                } else if is_manifest(&name) {
                    if let Ok(text) = std::fs::read_to_string(e.path()) {
                        info.add_manifest(&name, &text);
                    }
                }
            }
        }
        info
    }

    pub fn add_manifest(&mut self, name: &str, text: &str) {
        let lower = text.to_lowercase();
        if name == "package.json" || name == "composer.json" {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
                for key in [
                    "dependencies",
                    "devDependencies",
                    "peerDependencies",
                    "require",
                    "require-dev",
                ] {
                    if let Some(obj) = v.get(key).and_then(|d| d.as_object()) {
                        self.deps.extend(obj.keys().map(|k| k.to_lowercase()));
                    }
                }
            }
        }
        self.manifest_text.push_str(&lower);
        self.manifest_text.push('\n');
    }

    /// A JS/PHP dependency by exact name, or a word in any other manifest.
    pub fn depends_on(&self, names: &[&str]) -> bool {
        names
            .iter()
            .any(|n| self.deps.contains(*n) || self.mentions(n))
    }

    /// `word` appears as a whole token in some manifest.
    pub fn mentions(&self, word: &str) -> bool {
        let t = &self.manifest_text;
        let mut from = 0;
        while let Some(p) = t[from..].find(word) {
            let at = from + p;
            let before = t[..at].chars().next_back();
            let after = t[at + word.len()..].chars().next();
            let boundary =
                |c: Option<char>| c.is_none_or(|c| !(c.is_alphanumeric() || c == '_' || c == '-'));
            if boundary(before) && boundary(after) {
                return true;
            }
            from = at + word.len();
        }
        false
    }

    pub fn has_file(&self, pred: impl Fn(&str) -> bool) -> bool {
        self.files.iter().any(|f| pred(f))
    }
}
