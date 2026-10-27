//! Machine-wide state shared by every project: `~/.codegraph/` (or
//! `$CODEGRAPH_HOME`), holding the update-check cache and telemetry consent.

use std::path::PathBuf;

pub fn dir() -> PathBuf {
    if let Some(d) = std::env::var_os("CODEGRAPH_HOME").filter(|d| !d.is_empty()) {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    home.join(".codegraph")
}

/// `1`, `true`, `yes` (anything set but `0`/`false`/empty).
pub fn env_truthy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| !v.is_empty() && v != "0" && !v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

/// Explicitly false: `0` or `false`.
pub fn env_falsy(name: &str) -> bool {
    std::env::var(name)
        .map(|v| v == "0" || v.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Write `contents` via a temporary file and rename, so readers never see a
/// torn file.
pub fn write_atomic(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}
