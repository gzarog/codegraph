//! `codegraph.json`: per-project indexing configuration.

use codegraph_core::Language;
use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

pub const PROJECT_CONFIG_FILENAME: &str = "codegraph.json";

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawConfig {
    #[serde(default)]
    extensions: HashMap<String, String>,
    #[serde(default)]
    include_ignored: Vec<String>,
    #[serde(default)]
    exclude: Vec<String>,
    #[serde(default)]
    include: Vec<String>,
    #[serde(default)]
    deprioritize: Vec<String>,
}

/// Validated project configuration. Invalid entries are dropped with a warning.
#[derive(Debug, Default, Clone)]
pub struct ProjectConfig {
    /// Extra extension → language mappings (`.foo` → `typescript`).
    pub extensions: HashMap<String, Language>,
    /// Gitignored paths to index anyway (gitignore-style patterns).
    pub include_ignored: Vec<String>,
    /// Paths never to index (gitignore-style patterns).
    pub exclude: Vec<String>,
    /// When non-empty, only these paths are indexed (gitignore-style patterns).
    pub include: Vec<String>,
    /// Paths indexed but ranked lower in search.
    pub deprioritize: Vec<String>,
    pub warnings: Vec<String>,
}

fn normalize_ext(raw: &str) -> Option<String> {
    let mut ext = raw.trim().to_ascii_lowercase();
    if ext.is_empty() {
        return None;
    }
    if !ext.starts_with('.') {
        ext.insert(0, '.');
    }
    let body = &ext[1..];
    if body.is_empty() || body.contains(['.', '/', '\\']) {
        return None;
    }
    Some(ext)
}

fn clean_patterns(list: Vec<String>, key: &str, warnings: &mut Vec<String>) -> Vec<String> {
    list.into_iter()
        .filter_map(|p| {
            let t = p.trim().to_string();
            if t.is_empty() {
                warnings.push(format!(
                    "ignoring an empty \"{key}\" entry in {PROJECT_CONFIG_FILENAME}"
                ));
                None
            } else {
                Some(t)
            }
        })
        .collect()
}

impl ProjectConfig {
    /// Load `<root>/codegraph.json`; a missing or invalid file is an empty config.
    pub fn load(root: &Path) -> ProjectConfig {
        let path = root.join(PROJECT_CONFIG_FILENAME);
        let Ok(text) = std::fs::read_to_string(&path) else {
            return ProjectConfig::default();
        };
        Self::parse(&text)
    }

    pub fn parse(text: &str) -> ProjectConfig {
        let mut warnings = Vec::new();
        let raw: RawConfig = match serde_json::from_str(text) {
            Ok(r) => r,
            Err(e) => {
                return ProjectConfig {
                    warnings: vec![format!("ignoring {PROJECT_CONFIG_FILENAME}: {e}")],
                    ..Default::default()
                }
            }
        };
        let mut extensions = HashMap::new();
        for (k, v) in raw.extensions {
            match (normalize_ext(&k), v.parse::<Language>()) {
                (Some(ext), Ok(lang)) if lang != Language::Unknown => {
                    extensions.insert(ext, lang);
                }
                (None, _) => warnings.push(format!(
                    "ignoring extension mapping \"{k}\": not a file extension"
                )),
                _ => warnings.push(format!(
                    "ignoring extension \"{k}\": \"{v}\" is not a supported language"
                )),
            }
        }
        ProjectConfig {
            extensions,
            include_ignored: clean_patterns(raw.include_ignored, "includeIgnored", &mut warnings),
            exclude: clean_patterns(raw.exclude, "exclude", &mut warnings),
            include: clean_patterns(raw.include, "include", &mut warnings),
            deprioritize: clean_patterns(raw.deprioritize, "deprioritize", &mut warnings),
            warnings,
        }
    }

    /// Language for `path` honoring custom extension mappings.
    pub fn language_override(&self, path: &str) -> Option<Language> {
        if self.extensions.is_empty() {
            return None;
        }
        let base = path.rsplit('/').next().unwrap_or(path);
        let ext = base.rfind('.').map(|i| base[i..].to_ascii_lowercase())?;
        self.extensions.get(&ext).copied()
    }
}

/// gitignore-style patterns → a glob matcher over project-relative paths.
/// A pattern matches the path and everything below it; a pattern without `/`
/// matches at any depth, a leading `/` anchors it at the project root.
pub fn pattern_set(patterns: &[String]) -> GlobSet {
    let mut b = GlobSetBuilder::new();
    for p in patterns {
        let p = p.trim_start_matches("./");
        let anchored = p.starts_with('/');
        let p = p.trim_start_matches('/');
        let p = p.trim_end_matches('/');
        let base = if anchored || p.contains('/') {
            p.to_string()
        } else {
            format!("**/{p}")
        };
        // The path itself, and everything below it when it names a directory.
        for g in [base.clone(), format!("{base}/**")] {
            if let Ok(glob) = Glob::new(&g) {
                b.add(glob);
            }
        }
    }
    b.build().unwrap_or_else(|_| GlobSet::empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_validates() {
        let c = ProjectConfig::parse(
            r#"{"extensions": {"foo": "typescript", ".a.b": "go", ".x": "klingon"}, "exclude": ["gen/", " "]}"#,
        );
        assert_eq!(c.extensions.get(".foo"), Some(&Language::TypeScript));
        assert_eq!(c.extensions.len(), 1);
        assert_eq!(c.exclude, vec!["gen/"]);
        assert_eq!(c.warnings.len(), 3);
        assert_eq!(c.language_override("src/x.FOO"), Some(Language::TypeScript));
    }

    #[test]
    fn patterns_match_like_gitignore() {
        let s = pattern_set(&["gen/".into(), "*.pb.go".into(), "/top.ts".into()]);
        assert!(s.is_match("gen/a.ts"));
        assert!(s.is_match("pkg/gen/a.ts"));
        assert!(s.is_match("a/b/x.pb.go"));
        assert!(s.is_match("top.ts"));
        assert!(!s.is_match("src/top.ts"));
        assert!(!s.is_match("src/a.ts"));
    }
}
