//! One writer per index: a lock file in the data directory holding the
//! writer's pid. A lock whose process is gone is stale and taken over.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const LOCK_FILE: &str = "writer.lock";

/// Held while indexing; removes the lock file when dropped.
pub struct WriterLock {
    path: PathBuf,
}

impl Drop for WriterLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn lock_path(root: &Path) -> PathBuf {
    crate::project::data_dir(root).join(LOCK_FILE)
}

/// The pid recorded in an existing lock.
pub fn holder(root: &Path) -> Option<u32> {
    std::fs::read_to_string(lock_path(root))
        .ok()?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // Signal 0 checks existence; EPERM means it exists but isn't ours.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    true
}

/// A lock left behind by a dead process (or, where liveness can't be
/// checked, one older than an hour).
fn is_stale(path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return true;
    };
    let Some(pid) = text
        .split_whitespace()
        .next()
        .and_then(|p| p.parse::<u32>().ok())
    else {
        return true;
    };
    if pid == std::process::id() {
        return false;
    }
    if cfg!(unix) {
        return !alive(pid);
    }
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|m| m.elapsed().ok())
        .is_some_and(|age| age > Duration::from_secs(3600))
}

/// Take the lock if it is free (or stale).
pub fn try_acquire(root: &Path) -> std::io::Result<Option<WriterLock>> {
    let path = lock_path(root);
    for _ in 0..2 {
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                writeln!(f, "{} {}", std::process::id(), codegraph_core::now_ms())?;
                return Ok(Some(WriterLock { path }));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_stale(&path) {
                    let _ = std::fs::remove_file(&path);
                    continue;
                }
                return Ok(None);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(None)
}

/// Take the lock, waiting up to `timeout` for another writer to finish.
pub fn acquire(root: &Path, timeout: Duration) -> std::io::Result<Option<WriterLock>> {
    let start = Instant::now();
    loop {
        if let Some(lock) = try_acquire(root)? {
            return Ok(Some(lock));
        }
        if start.elapsed() >= timeout {
            return Ok(None);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Remove the lock regardless of who holds it. Returns the holder's pid, if any.
pub fn force_unlock(root: &Path) -> std::io::Result<Option<u32>> {
    let pid = holder(root);
    match std::fs::remove_file(lock_path(root)) {
        Ok(()) => Ok(pid.or(Some(0))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exclusive_and_stale_takeover() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(crate::project::data_dir(dir.path())).unwrap();
        let held = try_acquire(dir.path()).unwrap().expect("free lock");
        // Our own pid is never stale, and a second acquire must wait.
        assert!(acquire(dir.path(), Duration::from_millis(150))
            .unwrap()
            .is_none());
        drop(held);
        assert!(!lock_path(dir.path()).exists());
        // A lock from a pid that doesn't exist is taken over.
        std::fs::write(lock_path(dir.path()), "999999999 0\n").unwrap();
        if cfg!(unix) {
            assert!(try_acquire(dir.path()).unwrap().is_some());
        }
        std::fs::write(lock_path(dir.path()), "123 0\n").unwrap();
        assert_eq!(force_unlock(dir.path()).unwrap(), Some(123));
        assert_eq!(force_unlock(dir.path()).unwrap(), None);
    }
}
