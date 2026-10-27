//! Indexing: discover a project's source files, extract them in parallel,
//! store the results and resolve cross-file references. A sync re-extracts
//! only files whose content changed.

pub mod config;
pub mod discover;
pub mod generated;

pub use config::ProjectConfig;
pub use discover::{discover_files, MAX_SOURCE_FILE_SIZE_BYTES};

use codegraph_core::{
    detect_language, now_ms, ExtractionError, ExtractionResult, FileRecord, Language, Node,
    NodeKind, Severity,
};
use codegraph_frameworks::Framework;
use codegraph_store::Store;
use rayon::prelude::*;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Instant, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error(transparent)]
    Store(#[from] codegraph_store::StoreError),
    #[error(transparent)]
    Resolve(#[from] codegraph_resolve::ResolveError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, IndexError>;

/// Where indexing is, for progress display.
#[derive(Debug, Clone)]
pub enum Progress {
    Scanning,
    Extracting { done: usize, total: usize },
    Storing { done: usize, total: usize },
    Resolving { done: usize, total: usize },
    Done,
}

#[derive(Default)]
pub struct IndexOptions<'a> {
    /// Re-extract every file even when its content hash is unchanged.
    pub force: bool,
    pub progress: Option<&'a (dyn Fn(Progress) + Sync)>,
}

/// What an index or sync run did.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexReport {
    pub files_scanned: usize,
    pub files_added: Vec<String>,
    pub files_changed: Vec<String>,
    pub files_removed: Vec<String>,
    pub files_unchanged: usize,
    pub files_with_errors: usize,
    pub nodes_extracted: usize,
    pub refs_resolved: usize,
    pub refs_failed: usize,
    pub duration_ms: u128,
}

impl IndexReport {
    pub fn files_indexed(&self) -> usize {
        self.files_added.len() + self.files_changed.len()
    }

    pub fn changed_anything(&self) -> bool {
        self.files_indexed() > 0 || !self.files_removed.is_empty()
    }
}

/// sha256 hex of a file's content; oversized files hash to a size stamp.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    let digest = h.finalize();
    let mut s = String::with_capacity(64);
    for b in digest {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn oversize_stamp(size: u64) -> String {
    format!("codegraph:oversize:{size}")
}

struct Scanned {
    path: String,
    hash: String,
    size: u64,
    modified_at: i64,
    content: Option<String>,
}

fn scan(root: &Path, rel: &str) -> Option<Scanned> {
    let full = root.join(rel);
    let meta = std::fs::metadata(&full).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified_at = mtime_ms(&meta);
    let size = meta.len();
    if size > MAX_SOURCE_FILE_SIZE_BYTES {
        return Some(Scanned {
            path: rel.to_string(),
            hash: oversize_stamp(size),
            size,
            modified_at,
            content: None,
        });
    }
    let bytes = std::fs::read(&full).ok()?;
    let hash = content_hash(&bytes);
    // Binary content (NUL bytes) is never source.
    let content = (!bytes.contains(&0)).then(|| String::from_utf8_lossy(&bytes).into_owned());
    Some(Scanned {
        path: rel.to_string(),
        hash,
        size,
        modified_at,
        content,
    })
}

fn mtime_ms(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn file_only_result(
    path: &str,
    language: Language,
    end_line: u32,
    error: Option<ExtractionError>,
) -> ExtractionResult {
    let name = path.rsplit('/').next().unwrap_or(path);
    let mut node = Node::new(
        format!("file:{path}"),
        NodeKind::File,
        name,
        path,
        language,
        1,
        end_line.max(1),
    );
    node.qualified_name = path.to_string();
    node.updated_at = now_ms();
    ExtractionResult {
        nodes: vec![node],
        errors: error.into_iter().collect(),
        ..Default::default()
    }
}

/// Extract one file's content. Languages without a walker yet get a file node only.
pub fn extract(path: &str, language: Language, content: &str) -> ExtractionResult {
    let lines = content.lines().count() as u32;
    if !codegraph_extract::supports(language) {
        return file_only_result(path, language, lines, None);
    }
    let mut result = codegraph_extract::extract_file(path, content, language);
    if !result.nodes.iter().any(|n| n.kind == NodeKind::File) {
        let file = file_only_result(path, language, lines, None)
            .nodes
            .remove(0);
        result.nodes.insert(0, file);
    }
    result
}

fn build(
    config: &ProjectConfig,
    frameworks: &[Framework],
    s: Scanned,
) -> (FileRecord, ExtractionResult) {
    let (language, mut result, generated) = match &s.content {
        Some(content) => {
            let language = config
                .language_override(&s.path)
                .unwrap_or_else(|| detect_language(&s.path, Some(content)));
            let mut result = extract(&s.path, language, content);
            let routes =
                codegraph_frameworks::extract_routes(frameworks, &s.path, language, content);
            result.nodes.extend(routes.nodes);
            result.edges.extend(routes.edges);
            result.unresolved_references.extend(routes.refs);
            add_dispatch_sites(&s.path, language, content, &mut result);
            (language, result, generated::is_generated(&s.path, content))
        }
        None => {
            let language = config
                .language_override(&s.path)
                .unwrap_or_else(|| detect_language(&s.path, None));
            let mut err = ExtractionError::error(if s.size > MAX_SOURCE_FILE_SIZE_BYTES {
                format!(
                    "file is larger than {} bytes; not parsed",
                    MAX_SOURCE_FILE_SIZE_BYTES
                )
            } else {
                "binary content; not parsed".to_string()
            });
            err.severity = Severity::Warning;
            err.code = Some("skipped".into());
            (
                language,
                file_only_result(&s.path, language, 1, Some(err)),
                generated::is_generated_path(&s.path),
            )
        }
    };
    for e in &mut result.errors {
        e.file_path.get_or_insert_with(|| s.path.clone());
    }
    let record = FileRecord {
        path: s.path,
        content_hash: s.hash,
        language,
        size: s.size,
        modified_at: s.modified_at,
        indexed_at: now_ms(),
        node_count: result.nodes.len() as u32,
        errors: result.errors.clone(),
        generated,
    };
    (record, result)
}

/// Record event binds/fires, function-pointer slots, HTTP requests,
/// navigations and JSX renders as references from the innermost symbol
/// around each site.
fn add_dispatch_sites(
    path: &str,
    language: Language,
    content: &str,
    result: &mut ExtractionResult,
) {
    use codegraph_frameworks::dispatch;
    if !(language.is_js_family()
        || matches!(
            language,
            Language::Svelte
                | Language::Vue
                | Language::Astro
                | Language::Python
                | Language::Ruby
                | Language::Php
                | Language::Java
                | Language::Kotlin
                | Language::C
                | Language::Cpp
                | Language::ObjC
        ))
    {
        return;
    }
    let safe = codegraph_frameworks::strip::strip_comments(content, language);
    let mut sites = dispatch::sites(language, &safe);
    if codegraph_core::search::is_test_path(path) {
        sites.extend(dispatch::test_request_sites(language, &safe));
    }
    if codegraph_frameworks::jsx::applies(language) {
        sites.extend(
            codegraph_frameworks::jsx::renders(&safe)
                .into_iter()
                .map(|(line, name)| dispatch::Site {
                    line,
                    name,
                    kind: codegraph_core::ReferenceKind::Calls,
                    handler: None,
                }),
        );
    }
    if sites.is_empty() {
        return;
    }
    let file_id = format!("file:{path}");
    for site in sites {
        // Innermost callable/type covering the line; the file otherwise.
        let from = result
            .nodes
            .iter()
            .filter(|n| {
                n.kind != NodeKind::File
                    && n.kind != NodeKind::Route
                    && n.start_line <= site.line
                    && site.line <= n.end_line
            })
            .min_by_key(|n| n.end_line - n.start_line)
            .map(|n| n.id.clone())
            .unwrap_or_else(|| file_id.clone());
        result
            .unresolved_references
            .push(codegraph_core::UnresolvedReference {
                from_node_id: from,
                reference_name: site.name,
                reference_kind: site.kind,
                line: site.line,
                column: 0,
                file_path: Some(path.to_string()),
                language: Some(language),
                candidates: site.handler.into_iter().collect(),
                row_id: None,
            });
    }
}

const STORE_BATCH: usize = 200;

/// Index (or sync) the project at `root` into `store`.
pub fn index_project(
    store: &mut Store,
    root: &Path,
    config: &ProjectConfig,
    opts: &IndexOptions<'_>,
) -> Result<IndexReport> {
    let started = Instant::now();
    let progress = |p: Progress| {
        if let Some(cb) = opts.progress {
            cb(p)
        }
    };
    progress(Progress::Scanning);
    let files = discover_files(root, config);
    let frameworks =
        codegraph_frameworks::detect(&codegraph_frameworks::ProjectInfo::scan(root, &files));
    let framework_names = frameworks
        .iter()
        .map(|f| f.name())
        .collect::<Vec<_>>()
        .join(",");
    // A framework appearing or disappearing changes what unchanged files yield.
    let force = opts.force
        || store
            .metadata("frameworks")?
            .is_some_and(|prev| prev != framework_names);
    store.set_metadata("frameworks", &framework_names)?;
    let known: HashMap<String, FileRecord> = store
        .all_files()?
        .into_iter()
        .map(|f| (f.path.clone(), f))
        .collect();
    let mut report = IndexReport {
        files_scanned: files.len(),
        ..Default::default()
    };

    let current: HashSet<&str> = files.iter().map(String::as_str).collect();
    for path in known.keys() {
        if !current.contains(path.as_str()) {
            report.files_removed.push(path.clone());
        }
    }
    report.files_removed.sort();
    for path in &report.files_removed {
        store.delete_file(path)?;
    }

    // Size and mtime unchanged since the last index: skip without reading.
    let stat_unchanged = |rel: &str| -> bool {
        let Some(rec) = known.get(rel) else {
            return false;
        };
        let Ok(meta) = std::fs::metadata(root.join(rel)) else {
            return false;
        };
        meta.len() == rec.size && mtime_ms(&meta) == rec.modified_at
    };
    let scanned: Vec<Scanned> = files
        .par_iter()
        .filter(|rel| force || !stat_unchanged(rel))
        .filter_map(|rel| scan(root, rel))
        .collect();
    let (todo, touched): (Vec<Scanned>, Vec<Scanned>) = scanned
        .into_iter()
        .partition(|s| force || known.get(&s.path).map(|r| &r.content_hash) != Some(&s.hash));
    // Same content, new mtime (a touch or a checkout): remember the mtime so
    // the next sync skips the file without reading it.
    for s in &touched {
        store.set_file_modified_at(&s.path, s.modified_at)?;
    }
    report.files_unchanged = report.files_scanned - todo.len();
    for s in &todo {
        if known.contains_key(&s.path) {
            report.files_changed.push(s.path.clone());
        } else {
            report.files_added.push(s.path.clone());
        }
    }

    let total = todo.len();
    let done = AtomicUsize::new(0);
    let mut stored = 0;
    let mut new_names: HashSet<String> = HashSet::new();
    let mut todo = todo;
    while !todo.is_empty() {
        let rest = todo.split_off(todo.len().min(STORE_BATCH));
        let batch = std::mem::replace(&mut todo, rest);
        let built: Vec<(FileRecord, ExtractionResult)> = batch
            .into_par_iter()
            .map(|s| {
                let out = build(config, &frameworks, s);
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                if n.is_multiple_of(50) || n == total {
                    progress(Progress::Extracting { done: n, total });
                }
                out
            })
            .collect();
        for (_, result) in &built {
            report.nodes_extracted += result.nodes.len();
            if result.errors.iter().any(|e| e.severity == Severity::Error) {
                report.files_with_errors += 1;
            }
            new_names.extend(
                result
                    .nodes
                    .iter()
                    .filter(|n| n.kind != NodeKind::File)
                    .map(|n| n.name.clone()),
            );
        }
        store.store_files(&built)?;
        stored += built.len();
        progress(Progress::Storing {
            done: stored,
            total,
        });
    }

    if report.changed_anything() || force {
        // New symbols may satisfy references that failed before.
        if !new_names.is_empty() {
            store.reopen_failed_refs_named(&new_names)?;
        }
        let resolve_progress =
            |done: usize, total: usize| progress(Progress::Resolving { done, total });
        let timings = std::env::var_os("CODEGRAPH_TIMINGS").is_some();
        let t = Instant::now();
        let outcome = codegraph_resolve::resolve_pending(store, root, &resolve_progress)?;
        if timings {
            eprintln!(
                "[index] resolve: {:?} (stored by {:?})",
                t.elapsed(),
                t - started
            );
        }
        report.refs_resolved = outcome.resolved;
        report.refs_failed = outcome.failed;
        let t = Instant::now();
        codegraph_resolve::synth::synthesize(store, root)?;
        if timings {
            eprintln!("[index] synthesize: {:?}", t.elapsed());
        }
        let t = Instant::now();
        store.rebuild_name_segments()?;
        if timings {
            eprintln!("[index] name segments: {:?}", t.elapsed());
        }
        store.set_metadata("last_indexed_at", &now_ms().to_string())?;
    }
    report.duration_ms = started.elapsed().as_millis();
    progress(Progress::Done);
    Ok(report)
}

/// Group changed paths by language, for reporting.
pub fn languages_of(files: &[FileRecord]) -> HashMap<Language, usize> {
    let mut out = HashMap::new();
    for f in files {
        *out.entry(f.language).or_insert(0) += 1;
    }
    out
}
