//! `codegraph upgrade` and the background update check.
//!
//! Releases are GitHub release assets named `codegraph-<os>-<arch>.tar.gz`
//! (`.zip` on Windows) with a `SHA256SUMS` file beside them. Network access
//! goes through the system `curl` (present on macOS, Linux and Windows 10+),
//! so the binary carries no TLS stack of its own. A source install (the
//! binary lives in cargo's bin dir) upgrades with `cargo install` instead.

use crate::global;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub const REPO: &str = "gzarog/codegraph";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Re-check the release feed after this long (successful checks).
const CHECK_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// Back off this long after a failed check.
const CHECK_BACKOFF_MS: u64 = 60 * 60 * 1000;

// ---------------------------------------------------------------- versions

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: Option<String>,
}

/// `v1.2.3`, `1.2.3-rc.1` → parts; anything else → None.
pub fn parse_semver(v: &str) -> Option<Semver> {
    let v = v.trim().trim_start_matches('v');
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let core = core.split('+').next()?;
    let mut it = core.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    let patch = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None;
    }
    let pre = match pre {
        Some(p) if !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '.') => {
            Some(p.to_string())
        }
        Some(_) => return None,
        None => None,
    };
    Some(Semver {
        major,
        minor,
        patch,
        pre,
    })
}

/// The canonical `vX.Y.Z[-pre]` tag, rebuilt from parsed parts so nothing
/// from the network is ever echoed verbatim.
pub fn canonical_tag(v: &str) -> Option<String> {
    let s = parse_semver(v)?;
    Some(match &s.pre {
        Some(p) => format!("v{}.{}.{}-{p}", s.major, s.minor, s.patch),
        None => format!("v{}.{}.{}", s.major, s.minor, s.patch),
    })
}

/// Semver ordering: a release outranks its pre-releases.
pub fn compare(a: &Semver, b: &Semver) -> std::cmp::Ordering {
    (a.major, a.minor, a.patch)
        .cmp(&(b.major, b.minor, b.patch))
        .then_with(|| match (&a.pre, &b.pre) {
            (None, None) => std::cmp::Ordering::Equal,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (Some(_), None) => std::cmp::Ordering::Less,
            (Some(x), Some(y)) => compare_pre(x, y),
        })
}

fn compare_pre(a: &str, b: &str) -> std::cmp::Ordering {
    for (x, y) in a.split('.').zip(b.split('.')) {
        let o = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(m), Ok(n)) => m.cmp(&n),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            _ => x.cmp(y),
        };
        if o.is_ne() {
            return o;
        }
    }
    a.split('.').count().cmp(&b.split('.').count())
}

pub fn is_update_available(current: &str, latest: &str) -> bool {
    match (parse_semver(current), parse_semver(latest)) {
        (Some(c), Some(l)) => compare(&l, &c).is_gt(),
        _ => false,
    }
}

// ----------------------------------------------------------------- network

fn curl() -> Command {
    let mut c = Command::new(if cfg!(windows) { "curl.exe" } else { "curl" });
    c.stdin(Stdio::null()).stderr(Stdio::null());
    c
}

fn curl_text(args: &[&str], timeout: Duration) -> Option<String> {
    let out = curl()
        .arg("--max-time")
        .arg(timeout.as_secs().max(1).to_string())
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The tag in a `…/releases/tag/<tag>` URL.
pub fn tag_from_location(url: &str) -> Option<String> {
    let rest = url.split("/releases/tag/").nth(1)?;
    let tag = rest.split(['/', '?', '#']).next()?;
    (!tag.is_empty()).then(|| tag.to_string())
}

/// The latest release tag: the `releases/latest` redirect, then the API.
pub fn resolve_latest(timeout: Duration) -> Option<String> {
    let url = format!("https://github.com/{REPO}/releases/latest");
    if let Some(effective) = curl_text(
        &["-fsIL", "-o", null_device(), "-w", "%{url_effective}", &url],
        timeout,
    ) {
        if let Some(t) = tag_from_location(&effective).and_then(|t| canonical_tag(&t)) {
            return Some(t);
        }
    }
    let api = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = curl_text(
        &["-fsSL", "-H", "Accept: application/vnd.github+json", &api],
        timeout,
    )?;
    let v: serde_json::Value = serde_json::from_str(&body).ok()?;
    canonical_tag(v.get("tag_name")?.as_str()?)
}

fn null_device() -> &'static str {
    if cfg!(windows) {
        "NUL"
    } else {
        "/dev/null"
    }
}

fn download(url: &str, to: &Path) -> std::io::Result<()> {
    let status = curl()
        .args(["-fsSL", "--retry", "2", "-o"])
        .arg(to)
        .arg(url)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!("download failed: {url}")))
    }
}

// ------------------------------------------------------------ update check

#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckCache {
    last_attempt_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_success_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    latest: Option<String>,
}

/// `CODEGRAPH_NO_UPDATE_CHECK` or `DO_NOT_TRACK` turn the check off entirely.
pub fn check_disabled() -> bool {
    global::env_truthy("CODEGRAPH_NO_UPDATE_CHECK") || global::env_truthy("DO_NOT_TRACK")
}

fn cache_path(dir: &Path) -> PathBuf {
    dir.join("update-check.json")
}

fn read_cache(dir: &Path) -> Option<CheckCache> {
    serde_json::from_str(&std::fs::read_to_string(cache_path(dir)).ok()?).ok()
}

fn fresh(cache: &CheckCache, now: u64) -> bool {
    if cache
        .last_success_at
        .is_some_and(|t| now.saturating_sub(t) < CHECK_TTL_MS)
    {
        return true;
    }
    now.saturating_sub(cache.last_attempt_at) < CHECK_BACKOFF_MS
}

pub fn format_notice(current: &str, latest: &str) -> String {
    format!("CodeGraph {latest} is available (this is {current}). Update with `codegraph upgrade`.")
}

fn notice_from(cache: Option<&CheckCache>) -> Option<String> {
    let latest = canonical_tag(cache?.latest.as_deref()?)?;
    let current = canonical_tag(VERSION)?;
    is_update_available(&current, &latest).then(|| format_notice(&current, &latest))
}

/// The update notice from the cached check, without touching the network.
pub fn cached_notice() -> Option<String> {
    if check_disabled() {
        return None;
    }
    notice_from(read_cache(&global::dir()).as_ref())
}

/// Refresh the cached check when it is stale and return the notice.
/// Never fails: offline or rate-limited degrade to the previous answer.
pub fn refresh_check(dir: &Path, resolve: impl FnOnce() -> Option<String>) -> Option<String> {
    if check_disabled() {
        return None;
    }
    let cached = read_cache(dir);
    let now = global::now_ms();
    if cached.as_ref().is_some_and(|c| fresh(c, now)) {
        return notice_from(cached.as_ref());
    }
    let next = match resolve().and_then(|t| canonical_tag(&t)) {
        Some(latest) => CheckCache {
            last_attempt_at: now,
            last_success_at: Some(now),
            latest: Some(latest),
        },
        // Keep the last known tag: an outage must not hide a known update.
        None => CheckCache {
            last_attempt_at: now,
            last_success_at: cached.as_ref().and_then(|c| c.last_success_at),
            latest: cached.and_then(|c| c.latest),
        },
    };
    if let Ok(json) = serde_json::to_vec(&next) {
        let _ = global::write_atomic(&cache_path(dir), &json);
    }
    notice_from(Some(&next))
}

/// Check in the background; report an available update once on stderr
/// (never stdout, which is the MCP channel).
pub fn check_in_background() {
    if check_disabled() {
        return;
    }
    std::thread::spawn(|| {
        let dir = global::dir();
        if let Some(n) = refresh_check(&dir, || resolve_latest(Duration::from_secs(5))) {
            eprintln!("[CodeGraph] {}", n.trim_start_matches("CodeGraph "));
        }
    });
}

// ----------------------------------------------------------------- upgrade

/// How this binary was installed, which decides how it upgrades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallMethod {
    /// `cargo install` (lives in cargo's bin dir).
    Cargo,
    /// A release binary at this path.
    Binary(PathBuf),
}

pub fn detect_install_method(exe: &Path) -> InstallMethod {
    let cargo_bin = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".cargo"))
        })
        .map(|c| c.join("bin"));
    match (cargo_bin, exe.parent()) {
        (Some(bin), Some(parent)) if same_dir(&bin, parent) => InstallMethod::Cargo,
        _ => InstallMethod::Binary(exe.to_path_buf()),
    }
}

fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(x), Ok(y)) => x == y,
        _ => a == b,
    }
}

/// The release asset for this platform.
pub fn asset_name() -> String {
    let os = std::env::consts::OS;
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    format!("codegraph-{os}-{}.{ext}", std::env::consts::ARCH)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// The checksum `SHA256SUMS` lists for `name`.
pub fn checksum_for<'a>(sums: &'a str, name: &str) -> Option<&'a str> {
    sums.lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let hash = parts.next()?;
        let file = parts.next()?.trim_start_matches('*');
        (file == name && hash.len() == 64).then_some(hash)
    })
}

pub struct UpgradeOptions {
    /// Only report whether an update exists.
    pub check: bool,
    /// Install this tag instead of the latest.
    pub version: Option<String>,
    /// Reinstall even when already current.
    pub force: bool,
}

/// Run `codegraph upgrade`; returns the process exit code.
pub fn run(opts: &UpgradeOptions, log: &mut dyn FnMut(&str)) -> anyhow::Result<i32> {
    let current = canonical_tag(VERSION).unwrap_or_else(|| VERSION.to_string());
    let target = match &opts.version {
        Some(v) => canonical_tag(v).ok_or_else(|| anyhow::anyhow!("not a version: {v}"))?,
        None => match resolve_latest(Duration::from_secs(15)) {
            Some(t) => t,
            None => {
                log("Could not find a CodeGraph release on GitHub (none published yet, or curl or the network is unavailable).");
                return Ok(1);
            }
        },
    };
    let dir = global::dir();
    let _ = refresh_check(&dir, || Some(target.clone()));
    if !opts.force && opts.version.is_none() && !is_update_available(&current, &target) {
        log(&format!("CodeGraph {current} is up to date."));
        return Ok(0);
    }
    if opts.check {
        log(&format_notice(&current, &target));
        return Ok(0);
    }
    let exe = std::env::current_exe()?;
    match detect_install_method(&exe) {
        InstallMethod::Cargo => {
            log(&format!(
                "Upgrading {current} → {target} with cargo install…"
            ));
            let status = Command::new("cargo")
                .args(["install", "--locked", "--force", "--git"])
                .arg(format!("https://github.com/{REPO}"))
                .args(["--tag", &target, "codegraph"])
                .status()?;
            if !status.success() {
                log("cargo install failed.");
                return Ok(1);
            }
        }
        InstallMethod::Binary(path) => {
            log(&format!("Upgrading {current} → {target}…"));
            replace_binary(&path, &target)?;
        }
    }
    log(&format!("CodeGraph {target} installed."));
    log("Indexes stay valid; run `codegraph index --force` in a project to pick up extraction improvements.");
    Ok(0)
}

/// Download, verify and swap in the release binary for `tag`.
fn replace_binary(exe: &Path, tag: &str) -> anyhow::Result<()> {
    let asset = asset_name();
    let base = format!("https://github.com/{REPO}/releases/download/{tag}");
    let stage = std::env::temp_dir().join(format!("codegraph-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&stage)?;
    let result = (|| -> anyhow::Result<()> {
        let archive = stage.join(&asset);
        let sums = stage.join("SHA256SUMS");
        download(&format!("{base}/{asset}"), &archive)?;
        download(&format!("{base}/SHA256SUMS"), &sums)?;
        let sums = std::fs::read_to_string(&sums)?;
        let expected = checksum_for(&sums, &asset)
            .ok_or_else(|| anyhow::anyhow!("{asset} is not listed in SHA256SUMS"))?;
        let actual = sha256_hex(&std::fs::read(&archive)?);
        if !actual.eq_ignore_ascii_case(expected) {
            anyhow::bail!("checksum mismatch for {asset}; not installing");
        }
        // `tar` reads both .tar.gz and (on Windows 10+) .zip.
        let status = Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&stage)
            .status()?;
        if !status.success() {
            anyhow::bail!("could not unpack {asset}");
        }
        let bin_name = if cfg!(windows) {
            "codegraph.exe"
        } else {
            "codegraph"
        };
        let new_bin = find_file(&stage, bin_name)
            .ok_or_else(|| anyhow::anyhow!("{bin_name} missing from {asset}"))?;
        swap_in(&new_bin, exe)?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&stage);
    result
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(f) = find_file(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

/// Put `new_bin` where `exe` is. The running binary is moved aside first
/// (Windows can't overwrite a running executable) and restored on failure.
fn swap_in(new_bin: &Path, exe: &Path) -> anyhow::Result<()> {
    let staged = exe.with_extension("new");
    std::fs::copy(new_bin, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    let old = exe.with_extension("old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old)?;
    if let Err(e) = std::fs::rename(&staged, exe) {
        let _ = std::fs::rename(&old, exe);
        let _ = std::fs::remove_file(&staged);
        return Err(e.into());
    }
    // Windows keeps the old image locked until this process exits.
    let _ = std::fs::remove_file(&old);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(is_update_available("0.9.9", "v1.0.0"));
        assert!(is_update_available("1.0.0-rc.1", "1.0.0"));
        assert!(is_update_available("1.0.0-rc.2", "1.0.0-rc.10"));
        assert!(!is_update_available("1.0.0", "1.0.0-rc.1"));
        assert!(!is_update_available("1.2.0", "1.1.9"));
        assert_eq!(canonical_tag("1.2.3 <evil>"), None);
        assert_eq!(
            canonical_tag("v1.2.3-beta.1").as_deref(),
            Some("v1.2.3-beta.1")
        );
        assert_eq!(
            tag_from_location("https://github.com/a/b/releases/tag/v1.4.1").as_deref(),
            Some("v1.4.1")
        );
    }

    #[test]
    fn checksums() {
        let sums = "abc  other.tar.gz\n".to_string()
            + &"f".repeat(64)
            + "  codegraph-linux-x86_64.tar.gz\n";
        assert_eq!(
            checksum_for(&sums, "codegraph-linux-x86_64.tar.gz"),
            Some("f".repeat(64).as_str())
        );
        assert_eq!(checksum_for(&sums, "other.tar.gz"), None);
    }

    #[test]
    fn check_cache_keeps_known_update_through_outages() {
        let dir = tempfile::tempdir().unwrap();
        let n = refresh_check(dir.path(), || Some("v999.0.0".into()));
        assert!(n.unwrap().contains("v999.0.0"));
        // Fresh: no network call.
        let n = refresh_check(dir.path(), || panic!("should use the cache"));
        assert!(n.is_some());
        // Stale and offline: the known tag survives.
        let mut cache = read_cache(dir.path()).unwrap();
        cache.last_attempt_at = 0;
        cache.last_success_at = Some(0);
        std::fs::write(cache_path(dir.path()), serde_json::to_vec(&cache).unwrap()).unwrap();
        let n = refresh_check(dir.path(), || None);
        assert!(n.unwrap().contains("v999.0.0"));
    }
}
