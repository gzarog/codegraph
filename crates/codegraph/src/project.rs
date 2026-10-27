//! The per-project facade: locating, creating and opening a project's index.

use codegraph_context::{ContextOptions, ExploreOptions};
use codegraph_core::{Node, NodeKind};
use codegraph_graph::Graph;
use codegraph_index::{discover_files, index_project, IndexOptions, IndexReport, ProjectConfig};
use codegraph_store::Store;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Name of the per-project data directory.
pub const DEFAULT_DATA_DIR: &str = ".codegraph";
pub const DB_FILE: &str = "codegraph.db";

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("{0} is not initialized (no {DEFAULT_DATA_DIR}/ directory); run `codegraph init` there first")]
    NotInitialized(PathBuf),
    #[error("refusing to index {0}: {1} (use --force to override)")]
    UnsafeRoot(PathBuf, &'static str),
    #[error(transparent)]
    Store(#[from] codegraph_store::StoreError),
    #[error(transparent)]
    Index(#[from] codegraph_index::IndexError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("another codegraph process (pid {0}) is updating this index; try again shortly, or run `codegraph unlock` if that process is gone")]
    Locked(u32),
}

/// How long a writer waits for another to finish before giving up.
const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

pub type Result<T> = std::result::Result<T, ProjectError>;

/// The data directory name, overridable with `CODEGRAPH_DIR` (a plain name).
pub fn data_dir_name() -> String {
    match std::env::var("CODEGRAPH_DIR") {
        Ok(v) if !v.is_empty() && !v.contains(['/', '\\']) && v != ".." && v != "." => v,
        _ => DEFAULT_DATA_DIR.to_string(),
    }
}

pub fn data_dir(root: &Path) -> PathBuf {
    root.join(data_dir_name())
}

pub fn db_path(root: &Path) -> PathBuf {
    data_dir(root).join(DB_FILE)
}

pub fn is_initialized(root: &Path) -> bool {
    db_path(root).is_file()
}

/// The nearest directory at or above `start` that has an index.
pub fn find_root(start: &Path) -> Option<PathBuf> {
    let start = start.canonicalize().ok()?;
    start
        .ancestors()
        .find(|d| is_initialized(d))
        .map(Path::to_path_buf)
}

/// Why a directory is a bad index root (home directory, filesystem root).
pub fn unsafe_root_reason(root: &Path) -> Option<&'static str> {
    let canon = root.canonicalize().ok()?;
    if canon.parent().is_none() {
        return Some("it is a filesystem root");
    }
    if let Some(home) = std::env::var_os("HOME")
        .map(PathBuf::from)
        .and_then(|h| h.canonicalize().ok())
    {
        if canon == home {
            return Some("it is your home directory");
        }
    }
    None
}

/// Files whose on-disk state differs from the index.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingChanges {
    pub added: Vec<String>,
    pub modified: Vec<String>,
    pub removed: Vec<String>,
}

impl PendingChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.modified.is_empty() && self.removed.is_empty()
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub root: String,
    pub db_path: String,
    pub version: String,
    pub stats: codegraph_core::GraphStats,
    pub pending: PendingChanges,
    pub pending_refs: u64,
    pub failed_refs: u64,
    pub frameworks: Vec<String>,
}

/// An open project index.
pub struct CodeGraph {
    root: PathBuf,
    store: Store,
    config: ProjectConfig,
}

impl CodeGraph {
    /// Create the data directory (if needed) and open the index.
    pub fn init(root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        let dir = data_dir(&root);
        std::fs::create_dir_all(&dir)?;
        // Keep the index out of version control without touching the project's .gitignore.
        let gi = dir.join(".gitignore");
        if !gi.exists() {
            std::fs::write(gi, "# Created by codegraph\n*\n")?;
        }
        Self::open_at(root)
    }

    /// Open an existing index at `root`.
    pub fn open(root: &Path) -> Result<Self> {
        let root = root.canonicalize()?;
        if !is_initialized(&root) {
            return Err(ProjectError::NotInitialized(root));
        }
        Self::open_at(root)
    }

    /// Open the index at or above `start`.
    pub fn open_nearest(start: &Path) -> Result<Self> {
        match find_root(start) {
            Some(root) => Self::open_at(root),
            None => Err(ProjectError::NotInitialized(start.to_path_buf())),
        }
    }

    fn open_at(root: PathBuf) -> Result<Self> {
        let mut store = Store::open(db_path(&root))?;
        store.set_project_name_tokens(codegraph_core::search::derive_project_name_tokens(&root));
        let config = ProjectConfig::load(&root);
        Ok(CodeGraph {
            root,
            store,
            config,
        })
    }

    /// Delete the project's index.
    pub fn uninit(root: &Path) -> Result<bool> {
        let dir = data_dir(root);
        if dir.is_dir() {
            std::fs::remove_dir_all(dir)?;
            return Ok(true);
        }
        Ok(false)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut Store {
        &mut self.store
    }

    pub fn config(&self) -> &ProjectConfig {
        &self.config
    }

    pub fn graph(&self) -> Graph<'_> {
        Graph::new(&self.store)
    }

    /// Re-read `codegraph.json`.
    pub fn reload_config(&mut self) {
        self.config = ProjectConfig::load(&self.root);
        self.apply_config();
    }

    fn apply_config(&mut self) {
        let matcher = (!self.config.deprioritize.is_empty()).then(|| {
            let set = codegraph_index::config::pattern_set(&self.config.deprioritize);
            Box::new(move |p: &str| set.is_match(p)) as Box<dyn Fn(&str) -> bool + Send + Sync>
        });
        self.store.set_deprioritized(matcher);
    }

    /// Full index: re-extract every file.
    pub fn index_all(
        &mut self,
        progress: Option<&(dyn Fn(codegraph_index::Progress) + Sync)>,
    ) -> Result<IndexReport> {
        let _lock = self.writer_lock()?;
        self.reload_config();
        self.store.clear()?;
        let report = index_project(
            &mut self.store,
            &self.root,
            &self.config,
            &IndexOptions {
                force: true,
                progress,
            },
        )?;
        self.store.optimize()?;
        self.store.checkpoint()?;
        Ok(report)
    }

    /// Incremental sync: re-extract changed files only.
    pub fn sync(
        &mut self,
        progress: Option<&(dyn Fn(codegraph_index::Progress) + Sync)>,
    ) -> Result<IndexReport> {
        let _lock = self.writer_lock()?;
        self.reload_config();
        let report = index_project(
            &mut self.store,
            &self.root,
            &self.config,
            &IndexOptions {
                force: false,
                progress,
            },
        )?;
        Ok(report)
    }

    fn writer_lock(&self) -> Result<crate::lock::WriterLock> {
        match crate::lock::acquire(&self.root, LOCK_WAIT)? {
            Some(lock) => Ok(lock),
            None => Err(ProjectError::Locked(
                crate::lock::holder(&self.root).unwrap_or(0),
            )),
        }
    }

    /// Files that changed on disk since the last index (cheap: size+mtime, then hash).
    pub fn pending_changes(&self) -> Result<PendingChanges> {
        let files = discover_files(&self.root, &self.config);
        let known: HashMap<String, codegraph_core::FileRecord> = self
            .store
            .all_files()?
            .into_iter()
            .map(|f| (f.path.clone(), f))
            .collect();
        let mut out = PendingChanges::default();
        for f in &files {
            match known.get(f) {
                None => out.added.push(f.clone()),
                Some(rec) => {
                    let Ok(meta) = std::fs::metadata(self.root.join(f)) else {
                        continue;
                    };
                    if meta.len() == rec.size {
                        let mtime = meta
                            .modified()
                            .ok()
                            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis() as i64)
                            .unwrap_or(0);
                        if mtime == rec.modified_at {
                            continue;
                        }
                    }
                    let changed = match std::fs::read(self.root.join(f)) {
                        Ok(bytes) if !rec.content_hash.starts_with("codegraph:") => {
                            codegraph_index::content_hash(&bytes) != rec.content_hash
                        }
                        Ok(_) => meta.len() != rec.size,
                        Err(_) => false,
                    };
                    if changed {
                        out.modified.push(f.clone());
                    }
                }
            }
        }
        let current: std::collections::HashSet<&String> = files.iter().collect();
        out.removed = known
            .keys()
            .filter(|k| !current.contains(k))
            .cloned()
            .collect();
        out.removed.sort();
        Ok(out)
    }

    pub fn status(&self) -> Result<Status> {
        Ok(Status {
            root: self.root.display().to_string(),
            db_path: db_path(&self.root).display().to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            stats: self.store.stats()?,
            pending: self.pending_changes()?,
            pending_refs: self.store.count_refs(codegraph_store::RefStatus::Pending)?,
            failed_refs: self.store.count_refs(codegraph_store::RefStatus::Failed)?,
            frameworks: self
                .store
                .metadata("frameworks")?
                .map(|f| {
                    f.split(',')
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Definitions named `name` (qualified names allowed), optionally narrowed to
    /// a file path or suffix. Returns the matches and whether the file filter matched.
    pub fn find_symbols(&self, name: &str, file: Option<&str>) -> Result<(Vec<Node>, bool)> {
        let mut nodes: Vec<Node> = codegraph_context::relevance::exact_matches(&self.store, name)?
            .into_iter()
            .filter(|n| {
                !matches!(
                    n.kind,
                    NodeKind::Import | NodeKind::Parameter | NodeKind::File
                )
            })
            .collect();
        if nodes.is_empty() {
            nodes = self
                .store
                .search_nodes(
                    name,
                    &codegraph_core::SearchOptions {
                        limit: Some(5),
                        ..Default::default()
                    },
                )?
                .into_iter()
                .map(|r| r.node)
                .filter(|n| {
                    !matches!(
                        n.kind,
                        NodeKind::Import | NodeKind::Parameter | NodeKind::File
                    )
                })
                .filter(|n| {
                    n.name
                        .eq_ignore_ascii_case(name.rsplit(['.', ':']).next().unwrap_or(name))
                })
                .collect();
        }
        if let Some(f) = file {
            let f = f.trim_start_matches("./");
            let narrowed: Vec<Node> = nodes
                .iter()
                .filter(|n| {
                    n.file_path == f
                        || n.file_path.ends_with(&format!("/{f}"))
                        || n.file_path.ends_with(f)
                })
                .cloned()
                .collect();
            if !narrowed.is_empty() {
                return Ok((narrowed, true));
            }
            return Ok((nodes, false));
        }
        Ok((nodes, true))
    }

    pub fn explore(&self, query: &str, max_files: Option<usize>) -> Result<String> {
        Ok(codegraph_context::explore(
            &self.store,
            &self.root,
            query,
            &ExploreOptions { max_files },
        )?)
    }

    pub fn context(&self, query: &str, opts: &ContextOptions) -> Result<String> {
        let ctx = codegraph_context::build_context(&self.store, &self.root, query, opts)?;
        Ok(codegraph_context::format_context(
            &self.store,
            &ctx,
            opts.format,
        )?)
    }

    /// Test (or other matching) files that transitively depend on `files`.
    pub fn affected(
        &self,
        files: &[String],
        depth: usize,
        is_target: &dyn Fn(&str) -> bool,
    ) -> Result<Vec<String>> {
        let mut seen: std::collections::HashSet<String> = files.iter().cloned().collect();
        let mut frontier: Vec<String> = files.to_vec();
        let mut hits: Vec<String> = files.iter().filter(|f| is_target(f)).cloned().collect();
        for _ in 0..depth {
            let mut next = Vec::new();
            for f in &frontier {
                for dep in self.store.dependent_files(f)? {
                    if seen.insert(dep.clone()) {
                        if is_target(&dep) {
                            hits.push(dep.clone());
                        }
                        next.push(dep);
                    }
                }
            }
            if next.is_empty() {
                break;
            }
            frontier = next;
        }
        hits.sort();
        hits.dedup();
        Ok(hits)
    }
}
