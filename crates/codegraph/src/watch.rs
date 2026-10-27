//! Keep an index in sync with the working tree: watch for file changes and
//! run an incremental sync once edits settle.

use crate::project::CodeGraph;
use codegraph_core::language::is_source_file;
use notify::{RecursiveMode, Watcher as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Quiet period after the last change before syncing.
pub const DEBOUNCE: Duration = Duration::from_millis(800);

pub struct Watcher {
    _inner: notify::RecommendedWatcher,
    stop: Arc<AtomicBool>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn relevant(root: &Path, path: &Path, data_dir: &str) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    let rel = rel.to_string_lossy().replace('\\', "/");
    if rel
        .split('/')
        .any(|seg| seg == data_dir || codegraph_index::discover::DEFAULT_IGNORE_DIRS.contains(&seg))
    {
        return false;
    }
    is_source_file(&rel)
        || rel.ends_with(".gitignore")
        || rel.ends_with(codegraph_index::config::PROJECT_CONFIG_FILENAME)
}

/// Start watching `cg`'s project. `on_sync` is told about each sync's result.
pub fn watch(
    cg: Arc<Mutex<CodeGraph>>,
    on_sync: impl Fn(&codegraph_index::IndexReport) + Send + 'static,
) -> notify::Result<Watcher> {
    let root = cg
        .lock()
        .map(|g| g.root().to_path_buf())
        .unwrap_or_default();
    let data_dir = crate::project::data_dir_name();
    let last_event: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let le = last_event.clone();
    let watch_root = root.clone();
    let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if matches!(ev.kind, notify::EventKind::Access(_)) {
            return;
        }
        if ev.paths.iter().any(|p| relevant(&watch_root, p, &data_dir)) {
            if let Ok(mut t) = le.lock() {
                *t = Some(Instant::now());
            }
        }
    })?;
    inner.watch(&root, RecursiveMode::Recursive)?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    std::thread::spawn(move || {
        while !stop2.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(200));
            let due = last_event
                .lock()
                .ok()
                .and_then(|t| *t)
                .is_some_and(|t| t.elapsed() >= DEBOUNCE);
            if !due {
                continue;
            }
            if let Ok(mut t) = last_event.lock() {
                *t = None;
            }
            if let Ok(mut g) = cg.lock() {
                if let Ok(report) = g.sync(None) {
                    on_sync(&report);
                }
            }
        }
    });
    Ok(Watcher {
        _inner: inner,
        stop,
    })
}
