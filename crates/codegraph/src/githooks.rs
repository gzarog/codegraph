//! Opt-in git hooks that sync the index after commits, merges and checkouts,
//! for setups where the live file watcher is off. Each hook gets a marked
//! block, so existing hook scripts are kept and removal is exact.

use std::path::{Path, PathBuf};

const MARKER_BEGIN: &str = "# >>> codegraph sync hook >>>";
const MARKER_END: &str = "# <<< codegraph sync hook <<<";
pub const HOOKS: &[&str] = &["post-commit", "post-merge", "post-checkout"];

/// The repository's hooks directory (honoring worktrees and `core.hooksPath`).
pub fn hooks_dir(root: &Path) -> Option<PathBuf> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--git-path", "hooks"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|o| o.status.success());
    if let Some(o) = out {
        let p = PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
        return Some(if p.is_absolute() { p } else { root.join(p) });
    }
    let git = root.join(".git");
    git.is_dir().then(|| git.join("hooks"))
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

fn block(root: &Path) -> String {
    let root = root.display().to_string();
    [
        MARKER_BEGIN.to_string(),
        "# Keeps the CodeGraph index fresh. Runs in the background so it never blocks git."
            .to_string(),
        "# Managed by codegraph; remove with `codegraph git-hooks remove` or delete this block."
            .to_string(),
        "if command -v codegraph >/dev/null 2>&1; then".to_string(),
        format!(
            "  ( codegraph sync --quiet {} >/dev/null 2>&1 & ) >/dev/null 2>&1",
            shell_quote(&root)
        ),
        "fi".to_string(),
        MARKER_END.to_string(),
    ]
    .join("\n")
}

fn strip_block(text: &str) -> Option<String> {
    let s = text.find(MARKER_BEGIN)?;
    let e = text[s..].find(MARKER_END)? + s + MARKER_END.len();
    let mut out = format!("{}{}", text[..s].trim_end_matches('\n'), &text[e..]);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

#[cfg(unix)]
fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)?.permissions();
    perm.set_mode(perm.mode() | 0o755);
    std::fs::set_permissions(path, perm)
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Add (or refresh) the sync block in each hook. Returns the hooks written.
pub fn install(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let Some(dir) = hooks_dir(root) else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "not a git repository",
        ));
    };
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    for hook in HOOKS {
        let path = dir.join(hook);
        let existing = std::fs::read_to_string(&path).unwrap_or_default();
        let base = strip_block(&existing).unwrap_or(existing);
        let base = if base.trim().is_empty() {
            "#!/bin/sh\n".to_string()
        } else {
            base
        };
        let new = format!("{}\n{}\n", base.trim_end_matches('\n'), block(root));
        std::fs::write(&path, new)?;
        make_executable(&path)?;
        written.push(path);
    }
    Ok(written)
}

/// Remove the sync block from each hook (deleting hooks that only held it).
pub fn remove(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let Some(dir) = hooks_dir(root) else {
        return Ok(Vec::new());
    };
    let mut removed = Vec::new();
    for hook in HOOKS {
        let path = dir.join(hook);
        let Ok(existing) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Some(rest) = strip_block(&existing) else {
            continue;
        };
        if rest.trim().is_empty() || rest.trim() == "#!/bin/sh" {
            std::fs::remove_file(&path)?;
        } else {
            std::fs::write(&path, rest)?;
        }
        removed.push(path);
    }
    Ok(removed)
}

pub fn installed(root: &Path) -> bool {
    hooks_dir(root).is_some_and(|d| {
        HOOKS
            .iter()
            .any(|h| std::fs::read_to_string(d.join(h)).is_ok_and(|t| t.contains(MARKER_BEGIN)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_preserves_and_remove_restores() {
        let dir = tempfile::tempdir().unwrap();
        let hooks = dir.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        // `git rev-parse` fails outside a real repo with no HEAD; the fallback
        // still finds `.git/hooks`.
        let existing = "#!/bin/sh\necho mine\n";
        std::fs::write(hooks.join("post-commit"), existing).unwrap();
        install(dir.path()).unwrap();
        install(dir.path()).unwrap();
        let text = std::fs::read_to_string(hooks.join("post-commit")).unwrap();
        assert!(text.starts_with(existing) && text.matches(MARKER_BEGIN).count() == 1);
        assert!(installed(dir.path()));
        remove(dir.path()).unwrap();
        assert_eq!(
            std::fs::read_to_string(hooks.join("post-commit")).unwrap(),
            existing
        );
        assert!(!hooks.join("post-merge").exists());
        assert!(!installed(dir.path()));
    }
}
