//! Which files in a project get indexed.

use crate::config::{pattern_set, ProjectConfig};
use codegraph_core::language::is_source_file;
use globset::GlobSet;
use ignore::WalkBuilder;
use std::collections::BTreeSet;
use std::path::Path;

/// Directories that never hold project source: dependency trees, build
/// output, caches and tool state, across ecosystems.
pub const DEFAULT_IGNORE_DIRS: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".codegraph",
    "node_modules",
    "bower_components",
    "jspm_packages",
    "web_modules",
    ".yarn",
    ".pnpm-store",
    ".next",
    ".nuxt",
    ".svelte-kit",
    ".turbo",
    ".vite",
    ".parcel-cache",
    ".angular",
    ".docusaurus",
    "storybook-static",
    ".vinxi",
    ".nitro",
    "out-tsc",
    ".vercel",
    ".netlify",
    ".wrangler",
    "dist",
    "build",
    "out",
    ".output",
    "coverage",
    ".nyc_output",
    "__pycache__",
    "__pypackages__",
    ".venv",
    "venv",
    ".pixi",
    ".pdm-build",
    ".mypy_cache",
    ".pytest_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    ".hypothesis",
    ".ipynb_checkpoints",
    ".eggs",
    "target",
    ".gradle",
    "obj",
    "vendor",
    ".build",
    "Pods",
    "Carthage",
    "DerivedData",
    ".swiftpm",
    ".dart_tool",
    ".pub-cache",
    ".cxx",
    ".externalNativeBuild",
    "vcpkg_installed",
    ".bloop",
    ".metals",
    "lua_modules",
    ".luarocks",
    "__history",
    "__recovery",
    ".cache",
];

/// Android resource folders (`res/values-es/`, `res/layout/`, …): no code.
const ANDROID_RES_TYPES: &[&str] = &[
    "anim",
    "animator",
    "color",
    "drawable",
    "font",
    "layout",
    "menu",
    "mipmap",
    "navigation",
    "transition",
    "values",
    "xml",
];

/// Files larger than this are tracked but not parsed.
pub const MAX_SOURCE_FILE_SIZE_BYTES: u64 = 1024 * 1024;

fn is_default_ignored_dir(rel_dir: &str, name: &str) -> bool {
    if name.ends_with(".egg-info") || name.starts_with("cmake-build-") || name.starts_with("bazel-")
    {
        return true;
    }
    let parent = rel_dir.rsplit_once('/').map(|(p, _)| p).unwrap_or("");
    let parent_name = parent.rsplit('/').next().unwrap_or(parent);
    if parent_name == "res" && ANDROID_RES_TYPES.iter().any(|t| name.starts_with(t)) {
        return true;
    }
    if !DEFAULT_IGNORE_DIRS.contains(&name) {
        return false;
    }
    // `build` is a legal JVM package segment, and a folder below `src/` can
    // be a feature folder rather than build output.
    if name == "build" {
        let parts: Vec<&str> = rel_dir.split('/').collect();
        if let Some(src) = parts.iter().position(|p| *p == "src") {
            if src + 1 < parts.len() - 1 {
                return false;
            }
        }
    }
    true
}

fn rel_path(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let s = rel.to_string_lossy().replace('\\', "/");
    (!s.is_empty()).then_some(s)
}

struct Filters {
    exclude: GlobSet,
    include: Option<GlobSet>,
    config: ProjectConfig,
}

impl Filters {
    fn wants(&self, rel: &str) -> bool {
        (is_source_file(rel) || self.config.language_override(rel).is_some())
            && !self.exclude.is_match(rel)
            && self.include.as_ref().is_none_or(|inc| inc.is_match(rel))
    }
}

fn walk(
    root: &Path,
    filters: &Filters,
    respect_gitignore: bool,
    only: Option<&GlobSet>,
    out: &mut BTreeSet<String>,
) {
    let root_buf = root.to_path_buf();
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(false)
        .git_ignore(respect_gitignore)
        .git_global(respect_gitignore)
        .git_exclude(respect_gitignore)
        .ignore(respect_gitignore)
        .parents(respect_gitignore)
        .require_git(false)
        .follow_links(false)
        .filter_entry(move |e| {
            if e.depth() == 0 || !e.file_type().is_some_and(|t| t.is_dir()) {
                return true;
            }
            let Some(rel) = rel_path(&root_buf, e.path()) else {
                return true;
            };
            !is_default_ignored_dir(&rel, &e.file_name().to_string_lossy())
        });
    for entry in builder.build().flatten() {
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let Some(rel) = rel_path(root, entry.path()) else {
            continue;
        };
        if only.is_some_and(|o| !o.is_match(&rel)) {
            continue;
        }
        if filters.wants(&rel) {
            out.insert(rel);
        }
    }
}

/// Project-relative (`/`-separated) paths of every file to index, sorted.
pub fn discover_files(root: &Path, config: &ProjectConfig) -> Vec<String> {
    let filters = Filters {
        exclude: pattern_set(&config.exclude),
        include: (!config.include.is_empty()).then(|| pattern_set(&config.include)),
        config: config.clone(),
    };
    let mut out = BTreeSet::new();
    walk(root, &filters, true, None, &mut out);
    if !config.include_ignored.is_empty() {
        let only = pattern_set(&config.include_ignored);
        walk(root, &filters, false, Some(&only), &mut out);
    }
    out.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn honors_gitignore_defaults_and_config() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        for f in [
            "src/a.ts",
            "src/b.md",
            "node_modules/x/index.js",
            "dist/out.js",
            "gen/skip.ts",
            "ignored/keep.ts",
            "ignored/drop.ts",
            "app/src/main/java/com/x/build/B.java",
            "res/values-es/strings.xml",
        ] {
            let p = root.join(f);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        fs::write(root.join(".gitignore"), "ignored/\n").unwrap();
        let config =
            ProjectConfig::parse(r#"{"exclude": ["gen/"], "includeIgnored": ["ignored/keep.ts"]}"#);
        let files = discover_files(root, &config);
        assert_eq!(
            files,
            vec![
                "app/src/main/java/com/x/build/B.java",
                "ignored/keep.ts",
                "src/a.ts"
            ]
        );
    }
}
