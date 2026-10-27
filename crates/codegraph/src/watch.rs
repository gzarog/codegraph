//! Keep an index in sync with the working tree: watch for file changes and
//! run an incremental sync once edits settle.

use crate::project::CodeGraph;
use codegraph_core::language::is_source_file;
use notify::{RecursiveMode, Watcher as _};
use std::collections::BTreeSet;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Quiet period after the last change before syncing.
pub const DEBOUNCE: Duration = Duration::from_millis(800);

pub struct Watcher {
    _inner: notify::RecommendedWatcher,
    stop: Arc<AtomicBool>,
    pending: Arc<Mutex<BTreeSet<String>>>,
}

impl Watcher {
    /// Project-relative paths changed on disk but not yet re-indexed.
    pub fn pending(&self) -> Vec<String> {
        self.pending
            .lock()
            .map(|p| p.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// The project-relative path of a change worth syncing for.
fn relevant(root: &Path, path: &Path, data_dir: &str) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    if rel
        .split('/')
        .any(|seg| seg == data_dir || codegraph_index::discover::DEFAULT_IGNORE_DIRS.contains(&seg))
    {
        return None;
    }
    (is_source_file(&rel)
        || rel.ends_with(".gitignore")
        || rel.ends_with(codegraph_index::config::PROJECT_CONFIG_FILENAME))
    .then_some(rel)
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
    let pending: Arc<Mutex<BTreeSet<String>>> = Arc::default();
    let (le, pend) = (last_event.clone(), pending.clone());
    let watch_root = root.clone();
    let mut inner = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        let Ok(ev) = res else { return };
        if matches!(ev.kind, notify::EventKind::Access(_)) {
            return;
        }
        let changed: Vec<String> = ev
            .paths
            .iter()
            .filter_map(|p| relevant(&watch_root, p, &data_dir))
            .collect();
        if changed.is_empty() {
            return;
        }
        if let Ok(mut p) = pend.lock() {
            p.extend(changed);
        }
        if let Ok(mut t) = le.lock() {
            *t = Some(Instant::now());
        }
    })?;
    inner.watch(&root, RecursiveMode::Recursive)?;
    let stop = Arc::new(AtomicBool::new(false));
    let (stop2, pend) = (stop.clone(), pending.clone());
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
            // Edits arriving during the sync are re-added by the event handler.
            let batch: BTreeSet<String> = pend
                .lock()
                .map(|mut p| std::mem::take(&mut *p))
                .unwrap_or_default();
            let Ok(mut g) = cg.lock() else { continue };
            match g.sync(None) {
                Ok(report) => on_sync(&report),
                Err(_) => {
                    // Another writer holds the index; retry after the next quiet period.
                    if let Ok(mut p) = pend.lock() {
                        p.extend(batch);
                    }
                    if let Ok(mut t) = last_event.lock() {
                        *t = Some(Instant::now());
                    }
                }
            }
        }
    });
    Ok(Watcher {
        _inner: inner,
        stop,
        pending,
    })
}
