//! SQLite persistence for the CodeGraph knowledge graph: nodes, edges, files,
//! references awaiting resolution, full-text search and graph statistics.

use codegraph_core::search::{
    bounded_edit_distance, kind_bonus, name_match_bonus, parse_query, score_path_relevance,
    split_identifier_segments,
};
use codegraph_core::{
    Edge, EdgeKind, ExtractionError, ExtractionResult, FileRecord, GraphStats, Language, Node,
    NodeKind, Provenance, ReferenceKind, SearchOptions, SearchResult, UnresolvedReference,
};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension, Row, ToSql, Transaction};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const SCHEMA_VERSION: i64 = 1;
const SCHEMA: &str = include_str!("schema.sql");

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("database schema version {found} is newer than this build supports ({supported})")]
    SchemaTooNew { found: i64, supported: i64 },
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Resolution state of a stored reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefStatus {
    Pending,
    Resolved,
    Failed,
}

impl RefStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RefStatus::Pending => "pending",
            RefStatus::Resolved => "resolved",
            RefStatus::Failed => "failed",
        }
    }
}

/// The last `.`/`::`/`#`/`->`-separated segment of a reference name.
pub fn name_tail(name: &str) -> &str {
    let mut tail = name;
    for sep in ["::", "->", ".", "#", "\\", "/"] {
        if let Some(i) = tail.rfind(sep) {
            tail = &tail[i + sep.len()..];
        }
    }
    tail
}

const NODE_COLUMNS: &str = "id, kind, name, qualified_name, file_path, language, start_line, end_line, start_column, \
     end_column, docstring, signature, visibility, is_exported, is_async, is_static, is_abstract, decorators, \
     type_parameters, return_type, updated_at";

fn json_list(v: &[String]) -> Option<String> {
    (!v.is_empty()).then(|| serde_json::to_string(v).unwrap_or_default())
}

fn parse_list(s: Option<String>) -> Vec<String> {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn row_to_node(row: &Row<'_>) -> rusqlite::Result<Node> {
    let kind: String = row.get(1)?;
    let language: String = row.get(5)?;
    let visibility: Option<String> = row.get(12)?;
    Ok(Node {
        id: row.get(0)?,
        kind: kind.parse().unwrap_or(NodeKind::Variable),
        name: row.get(2)?,
        qualified_name: row.get(3)?,
        file_path: row.get(4)?,
        language: language.parse().unwrap_or(Language::Unknown),
        start_line: row.get(6)?,
        end_line: row.get(7)?,
        start_column: row.get(8)?,
        end_column: row.get(9)?,
        docstring: row.get(10)?,
        signature: row.get(11)?,
        visibility: visibility.and_then(|v| v.parse().ok()),
        is_exported: row.get::<_, i64>(13)? != 0,
        is_async: row.get::<_, i64>(14)? != 0,
        is_static: row.get::<_, i64>(15)? != 0,
        is_abstract: row.get::<_, i64>(16)? != 0,
        decorators: parse_list(row.get(17)?),
        type_parameters: parse_list(row.get(18)?),
        return_type: row.get(19)?,
        updated_at: row.get(20)?,
    })
}

const EDGE_COLUMNS: &str = "source, target, kind, metadata, line, col, provenance, id";

fn row_to_edge(row: &Row<'_>) -> rusqlite::Result<Edge> {
    let kind: String = row.get(2)?;
    let metadata: Option<String> = row.get(3)?;
    let provenance: Option<String> = row.get(6)?;
    Ok(Edge {
        source: row.get(0)?,
        target: row.get(1)?,
        kind: kind.parse().unwrap_or(EdgeKind::References),
        metadata: metadata.and_then(|m| serde_json::from_str(&m).ok()),
        line: row.get(4)?,
        column: row.get(5)?,
        provenance: provenance.and_then(|p| p.parse().ok()),
    })
}

const REF_COLUMNS: &str =
    "id, from_node_id, reference_name, reference_kind, line, col, candidates, file_path, language";

fn row_to_ref(row: &Row<'_>) -> rusqlite::Result<UnresolvedReference> {
    let kind: String = row.get(3)?;
    let language: String = row.get(8)?;
    Ok(UnresolvedReference {
        row_id: Some(row.get(0)?),
        from_node_id: row.get(1)?,
        reference_name: row.get(2)?,
        reference_kind: kind.parse().unwrap_or(ReferenceKind::References),
        line: row.get(4)?,
        column: row.get(5)?,
        candidates: parse_list(row.get(6)?),
        file_path: Some(row.get(7)?),
        language: language.parse().ok(),
    })
}

const FILE_COLUMNS: &str =
    "path, content_hash, language, size, modified_at, indexed_at, node_count, errors, generated";

fn row_to_file(row: &Row<'_>) -> rusqlite::Result<FileRecord> {
    let language: String = row.get(2)?;
    let errors: Option<String> = row.get(7)?;
    Ok(FileRecord {
        path: row.get(0)?,
        content_hash: row.get(1)?,
        language: language.parse().unwrap_or(Language::Unknown),
        size: row.get::<_, i64>(3)? as u64,
        modified_at: row.get(4)?,
        indexed_at: row.get(5)?,
        node_count: row.get(6)?,
        errors: errors
            .and_then(|e| serde_json::from_str::<Vec<ExtractionError>>(&e).ok())
            .unwrap_or_default(),
        generated: row.get::<_, i64>(8)? != 0,
    })
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

/// Chunk size for `IN (...)` lists, under SQLite's bound-parameter limit.
const IN_CHUNK: usize = 500;

/// A test on a project-relative path.
pub type PathPredicate = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// The per-project database.
pub struct Store {
    conn: Connection,
    path: PathBuf,
    project_name_tokens: HashSet<String>,
    /// Paths `codegraph.json` asks to rank lower.
    deprioritized: Option<PathPredicate>,
}

impl Store {
    /// Open (creating if needed) the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(&path)?;
        Self::init(conn, path)
    }

    /// An in-memory database (tests, scratch work).
    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?, PathBuf::from(":memory:"))
    }

    fn init(conn: Connection, path: PathBuf) -> Result<Self> {
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             PRAGMA foreign_keys = ON;
             PRAGMA temp_store = MEMORY;
             PRAGMA cache_size = -64000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        let store = Store {
            conn,
            path,
            project_name_tokens: HashSet::new(),
            deprioritized: None,
        };
        match store.metadata("schema_version")? {
            Some(v) => {
                let found: i64 = v.parse().unwrap_or(0);
                if found > SCHEMA_VERSION {
                    return Err(StoreError::SchemaTooNew {
                        found,
                        supported: SCHEMA_VERSION,
                    });
                }
            }
            None => store.set_metadata("schema_version", &SCHEMA_VERSION.to_string())?,
        }
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Words naming the project itself, ignored by path-relevance scoring.
    pub fn set_project_name_tokens(&mut self, tokens: HashSet<String>) {
        self.project_name_tokens = tokens;
    }

    /// Paths for which `matcher` returns true are ranked below other code.
    pub fn set_deprioritized(&mut self, matcher: Option<PathPredicate>) {
        self.deprioritized = matcher;
    }

    /// Generated files among `paths` (by the index-time verdict).
    pub fn generated_among(&self, paths: &[&str]) -> Result<HashSet<String>> {
        let mut out = HashSet::new();
        for chunk in paths.chunks(IN_CHUNK) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT path FROM files WHERE generated = 1 AND path IN ({})",
                placeholders(chunk.len())
            ))?;
            for p in stmt.query_map(params_from_iter(chunk), |r| r.get::<_, String>(0))? {
                out.insert(p?);
            }
        }
        Ok(out)
    }

    /// Whether `path` should rank below hand-written project code: a
    /// `deprioritize` match or a generated file.
    pub fn is_deprioritized(&self, path: &str) -> bool {
        self.deprioritized.as_ref().is_some_and(|m| m(path))
            || self
                .generated_among(&[path])
                .map(|g| !g.is_empty())
                .unwrap_or(false)
    }

    /// Run `f` in one transaction.
    pub fn transaction<T>(&mut self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction()?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    // ---------------------------------------------------------------- metadata

    pub fn metadata(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT value FROM project_metadata WHERE key = ?",
                [key],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn set_metadata(&self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO project_metadata (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
            params![key, value, codegraph_core::now_ms()],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------------ writes

    /// Replace everything stored for one file with a fresh extraction.
    /// References from other files that resolved into this file are reopened.
    pub fn store_file(&mut self, file: &FileRecord, result: &ExtractionResult) -> Result<()> {
        self.transaction(|tx| {
            write_file(tx, file, result)?;
            Ok(())
        })
    }

    /// Store many files in one transaction.
    pub fn store_files(&mut self, files: &[(FileRecord, ExtractionResult)]) -> Result<()> {
        self.transaction(|tx| {
            for (file, result) in files {
                write_file(tx, file, result)?;
            }
            Ok(())
        })
    }

    /// Remove a file and everything extracted from it.
    pub fn delete_file(&mut self, path: &str) -> Result<()> {
        self.transaction(|tx| {
            clear_file(tx, path)?;
            tx.execute("DELETE FROM files WHERE path = ?", [path])?;
            Ok(())
        })
    }

    pub fn insert_nodes(&mut self, nodes: &[Node]) -> Result<()> {
        self.transaction(|tx| {
            for n in nodes {
                insert_node(tx, n)?;
            }
            Ok(())
        })
    }

    pub fn insert_edges(&mut self, edges: &[Edge]) -> Result<usize> {
        self.transaction(|tx| {
            let mut n = 0;
            for e in edges {
                n += insert_edge(tx, e)?;
            }
            Ok(n)
        })
    }

    /// Delete edges by kind whose metadata marks them as produced by `producer`
    /// (`metadata.synthesizedBy`), e.g. before re-running a synthesis pass.
    pub fn delete_synthesized_edges(&self, producer: &str) -> Result<usize> {
        Ok(self.conn.execute(
            "DELETE FROM edges WHERE metadata IS NOT NULL AND json_valid(metadata)
               AND json_extract(metadata, '$.synthesizedBy') = ?",
            [producer],
        )?)
    }

    /// Delete nodes (and, by cascade, their edges and references).
    pub fn delete_nodes(&mut self, ids: &[String]) -> Result<()> {
        self.transaction(|tx| {
            for chunk in ids.chunks(IN_CHUNK) {
                tx.execute(
                    &format!(
                        "DELETE FROM nodes WHERE id IN ({})",
                        placeholders(chunk.len())
                    ),
                    params_from_iter(chunk),
                )?;
            }
            Ok(())
        })
    }

    /// Drop everything (a full re-index).
    pub fn clear(&mut self) -> Result<()> {
        self.conn.execute_batch(
            "DELETE FROM refs; DELETE FROM edges; DELETE FROM nodes; DELETE FROM files; DELETE FROM name_segments;
             INSERT INTO nodes_fts(nodes_fts) VALUES ('rebuild');",
        )?;
        Ok(())
    }

    // -------------------------------------------------------------- references

    /// Pending references in row-id order, `limit` at a time after `after_id`.
    pub fn pending_refs_after(
        &self,
        after_id: i64,
        limit: usize,
    ) -> Result<Vec<UnresolvedReference>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {REF_COLUMNS} FROM refs WHERE status = 'pending' AND id > ? ORDER BY id LIMIT ?"
        ))?;
        let rows = stmt.query_map(params![after_id, limit as i64], row_to_ref)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn count_refs(&self, status: RefStatus) -> Result<u64> {
        // A literal status lets SQLite use the partial pending index.
        let sql = format!(
            "SELECT COUNT(*) FROM refs WHERE status = '{}'",
            status.as_str()
        );
        Ok(self.conn.query_row(&sql, [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// References (any status) from nodes in `file_path`.
    pub fn refs_in_file(&self, file_path: &str) -> Result<Vec<UnresolvedReference>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {REF_COLUMNS} FROM refs WHERE file_path = ? ORDER BY id"
        ))?;
        let rows = stmt.query_map([file_path], row_to_ref)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Unresolved (pending or failed) references from one node.
    pub fn open_refs_from(&self, node_id: &str) -> Result<Vec<UnresolvedReference>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {REF_COLUMNS} FROM refs WHERE from_node_id = ? AND status != 'resolved' ORDER BY id"
        ))?;
        let rows = stmt.query_map([node_id], row_to_ref)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record resolution outcomes: each resolved ref gets its edge; the rest
    /// are marked failed.
    pub fn apply_resolutions(
        &mut self,
        resolved: &[(i64, Edge, String)],
        failed: &[i64],
    ) -> Result<()> {
        self.transaction(|tx| {
            {
                let mut mark = tx.prepare_cached(
                    "UPDATE refs SET status = 'resolved', target_id = ?2, target_file = ?3 WHERE id = ?1",
                )?;
                for (row_id, edge, target_file) in resolved {
                    insert_edge(tx, edge)?;
                    mark.execute(params![row_id, edge.target, target_file])?;
                }
            }
            let mut fail = tx.prepare_cached("UPDATE refs SET status = 'failed', target_id = NULL, target_file = NULL WHERE id = ?")?;
            for id in failed {
                fail.execute([id])?;
            }
            Ok(())
        })
    }

    /// Reopen failed references whose name or tail matches one of `names`
    /// (new symbols appeared that might satisfy them). Returns how many.
    pub fn reopen_failed_refs_named(&mut self, names: &HashSet<String>) -> Result<usize> {
        let names: Vec<&String> = names.iter().collect();
        self.transaction(|tx| {
            let mut n = 0;
            for chunk in names.chunks(IN_CHUNK) {
                let ph = placeholders(chunk.len());
                n += tx.execute(
                    &format!(
                        "UPDATE refs SET status = 'pending' WHERE status = 'failed'
                           AND (name_tail IN ({ph}) OR reference_name IN ({ph}))"
                    ),
                    params_from_iter(chunk.iter().chain(chunk.iter())),
                )?;
            }
            Ok(n)
        })
    }

    /// Drop the edges dynamic-dispatch joins produced and reopen the `fire:`
    /// and `http:` references behind them, so the next resolution re-joins
    /// them against the current binds and routes.
    pub fn reopen_dispatch_refs(&mut self) -> Result<usize> {
        self.transaction(|tx| {
            tx.execute(
                "DELETE FROM edges WHERE metadata IS NOT NULL AND json_valid(metadata)
                   AND json_extract(metadata, '$.synthesizedBy') = 'dispatch'",
                [],
            )?;
            Ok(tx.execute(
                "UPDATE refs SET status = 'pending', target_id = NULL, target_file = NULL
                  WHERE status != 'pending'
                    AND (reference_name LIKE 'fire:%' OR reference_name LIKE 'http:%')",
                [],
            )?)
        })
    }

    /// `(reference name, target id, file of the bind)` of every resolved
    /// `bind:` reference.
    pub fn resolved_binds(&self) -> Result<Vec<(String, String, String)>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT reference_name, target_id, COALESCE(file_path, '') FROM refs
              WHERE status = 'resolved' AND reference_name LIKE 'bind:%' AND target_id IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Reopen every failed reference.
    pub fn reopen_all_failed_refs(&self) -> Result<usize> {
        Ok(self.conn.execute(
            "UPDATE refs SET status = 'pending' WHERE status = 'failed'",
            [],
        )?)
    }

    // ------------------------------------------------------------------- nodes

    pub fn node(&self, id: &str) -> Result<Option<Node>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {NODE_COLUMNS} FROM nodes WHERE id = ?"))?;
        Ok(stmt.query_row([id], row_to_node).optional()?)
    }

    pub fn nodes_by_ids(&self, ids: &[String]) -> Result<HashMap<String, Node>> {
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(IN_CHUNK) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT {NODE_COLUMNS} FROM nodes WHERE id IN ({})",
                placeholders(chunk.len())
            ))?;
            for n in stmt.query_map(params_from_iter(chunk), row_to_node)? {
                let n = n?;
                out.insert(n.id.clone(), n);
            }
        }
        Ok(out)
    }

    fn query_nodes(&self, sql_where: &str, params: &[&dyn ToSql]) -> Result<Vec<Node>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {NODE_COLUMNS} FROM nodes WHERE {sql_where}"
        ))?;
        let rows = stmt.query_map(params, row_to_node)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn nodes_in_file(&self, file_path: &str) -> Result<Vec<Node>> {
        self.query_nodes(
            "file_path = ? ORDER BY start_line, start_column",
            &[&file_path],
        )
    }

    pub fn nodes_named(&self, name: &str) -> Result<Vec<Node>> {
        self.query_nodes("name = ?", &[&name])
    }

    pub fn nodes_named_case_insensitive(&self, name: &str) -> Result<Vec<Node>> {
        self.query_nodes("lower(name) = lower(?)", &[&name])
    }

    pub fn nodes_by_qualified_name(&self, qualified_name: &str) -> Result<Vec<Node>> {
        self.query_nodes("qualified_name = ?", &[&qualified_name])
    }

    /// Nodes whose qualified name ends with `suffix` (e.g. `Foo::bar`).
    pub fn nodes_by_qualified_suffix(&self, suffix: &str, limit: usize) -> Result<Vec<Node>> {
        let like = format!("%{}", escape_like(suffix));
        let name = name_tail(suffix).to_string();
        self.query_nodes(
            "name = ? AND (qualified_name = ? OR qualified_name LIKE ? ESCAPE '\\') LIMIT ?",
            &[&name, &suffix, &like, &(limit as i64)],
        )
    }

    pub fn nodes_by_kind(&self, kind: NodeKind) -> Result<Vec<Node>> {
        self.query_nodes("kind = ? ORDER BY file_path, start_line", &[&kind.as_str()])
    }

    pub fn file_node(&self, file_path: &str) -> Result<Option<Node>> {
        Ok(self
            .query_nodes("file_path = ? AND kind = 'file' LIMIT 1", &[&file_path])?
            .into_iter()
            .next())
    }

    /// The innermost node in `file_path` whose span covers `line`.
    pub fn node_at(&self, file_path: &str, line: u32) -> Result<Option<Node>> {
        Ok(self
            .query_nodes(
                "file_path = ? AND start_line <= ? AND end_line >= ? AND kind != 'file'
                 ORDER BY (end_line - start_line) ASC LIMIT 1",
                &[&file_path, &line, &line],
            )?
            .into_iter()
            .next())
    }

    pub fn all_nodes(&self) -> Result<Vec<Node>> {
        self.query_nodes("1 = 1 ORDER BY file_path, start_line", &[])
    }

    pub fn node_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM nodes", [], |r| r.get::<_, i64>(0))? as u64)
    }

    // ------------------------------------------------------------------- edges

    fn query_edges(
        &self,
        sql_where: &str,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        let mut out = Vec::new();
        for chunk in ids.chunks(IN_CHUNK) {
            let mut sql = format!(
                "SELECT {EDGE_COLUMNS} FROM edges WHERE {sql_where} IN ({})",
                placeholders(chunk.len())
            );
            let mut args: Vec<String> = chunk.to_vec();
            if !kinds.is_empty() {
                sql.push_str(&format!(" AND kind IN ({})", placeholders(kinds.len())));
                args.extend(kinds.iter().map(|k| k.as_str().to_string()));
            }
            sql.push_str(" ORDER BY id");
            let mut stmt = self.conn.prepare_cached(&sql)?;
            for e in stmt.query_map(params_from_iter(args), row_to_edge)? {
                out.push(e?);
            }
        }
        Ok(out)
    }

    pub fn outgoing_edges(&self, source: &str, kinds: &[EdgeKind]) -> Result<Vec<Edge>> {
        self.query_edges("source", &[source.to_string()], kinds)
    }

    pub fn incoming_edges(&self, target: &str, kinds: &[EdgeKind]) -> Result<Vec<Edge>> {
        self.query_edges("target", &[target.to_string()], kinds)
    }

    pub fn outgoing_edges_from(&self, sources: &[String], kinds: &[EdgeKind]) -> Result<Vec<Edge>> {
        self.query_edges("source", sources, kinds)
    }

    pub fn incoming_edges_to(&self, targets: &[String], kinds: &[EdgeKind]) -> Result<Vec<Edge>> {
        self.query_edges("target", targets, kinds)
    }

    /// Edges whose both endpoints are in `ids`.
    pub fn edges_among(&self, ids: &[String], kinds: &[EdgeKind]) -> Result<Vec<Edge>> {
        let set: HashSet<&String> = ids.iter().collect();
        Ok(self
            .outgoing_edges_from(ids, kinds)?
            .into_iter()
            .filter(|e| set.contains(&e.target))
            .collect())
    }

    pub fn edges_of_kind(&self, kind: EdgeKind) -> Result<Vec<Edge>> {
        let mut stmt = self.conn.prepare_cached(&format!(
            "SELECT {EDGE_COLUMNS} FROM edges WHERE kind = ? ORDER BY id"
        ))?;
        let rows = stmt.query_map([kind.as_str()], row_to_edge)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn edge_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// Incoming-edge counts per node, for ranking by how depended-on a symbol is.
    pub fn incoming_counts(
        &self,
        ids: &[String],
        kinds: &[EdgeKind],
    ) -> Result<HashMap<String, u64>> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(IN_CHUNK) {
            let mut sql = format!(
                "SELECT target, COUNT(*) FROM edges WHERE target IN ({})",
                placeholders(chunk.len())
            );
            let mut args: Vec<String> = chunk.to_vec();
            if !kinds.is_empty() {
                sql.push_str(&format!(" AND kind IN ({})", placeholders(kinds.len())));
                args.extend(kinds.iter().map(|k| k.as_str().to_string()));
            }
            sql.push_str(" GROUP BY target");
            let mut stmt = self.conn.prepare(&sql)?;
            for r in stmt.query_map(params_from_iter(args), |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
            })? {
                let (id, n) = r?;
                out.insert(id, n as u64);
            }
        }
        Ok(out)
    }

    /// Files whose symbols have edges into `file_path`'s symbols.
    pub fn dependent_files(&self, file_path: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT s.file_path FROM edges e
               JOIN nodes t ON t.id = e.target
               JOIN nodes s ON s.id = e.source
              WHERE t.file_path = ?1 AND s.file_path != ?1 AND e.kind != 'contains'
              ORDER BY s.file_path",
        )?;
        let rows = stmt.query_map([file_path], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Files `file_path`'s symbols have edges into.
    pub fn dependency_files(&self, file_path: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT t.file_path FROM edges e
               JOIN nodes s ON s.id = e.source
               JOIN nodes t ON t.id = e.target
              WHERE s.file_path = ?1 AND t.file_path != ?1 AND e.kind != 'contains'
              ORDER BY t.file_path",
        )?;
        let rows = stmt.query_map([file_path], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Non-file symbols with no incoming edge other than `contains`.
    pub fn unreferenced_nodes(&self, kinds: &[NodeKind], limit: usize) -> Result<Vec<Node>> {
        let kinds: Vec<&str> = kinds.iter().map(|k| k.as_str()).collect();
        let mut sql = format!(
            "SELECT {NODE_COLUMNS} FROM nodes n WHERE n.kind != 'file'
               AND NOT EXISTS (SELECT 1 FROM edges e WHERE e.target = n.id AND e.kind != 'contains')"
        );
        if !kinds.is_empty() {
            sql.push_str(&format!(" AND n.kind IN ({})", placeholders(kinds.len())));
        }
        sql.push_str(&format!(
            " ORDER BY n.file_path, n.start_line LIMIT {limit}"
        ));
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(kinds), row_to_node)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ------------------------------------------------------------------- files

    pub fn file(&self, path: &str) -> Result<Option<FileRecord>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {FILE_COLUMNS} FROM files WHERE path = ?"))?;
        Ok(stmt.query_row([path], row_to_file).optional()?)
    }

    pub fn all_files(&self) -> Result<Vec<FileRecord>> {
        let mut stmt = self
            .conn
            .prepare_cached(&format!("SELECT {FILE_COLUMNS} FROM files ORDER BY path"))?;
        let rows = stmt.query_map([], row_to_file)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// path → content hash for every tracked file.
    pub fn file_hashes(&self) -> Result<HashMap<String, String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path, content_hash FROM files")?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Record a new mtime for a file whose content didn't change.
    pub fn set_file_modified_at(&self, path: &str, modified_at: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE files SET modified_at = ? WHERE path = ?",
            params![modified_at, path],
        )?;
        Ok(())
    }

    pub fn file_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM files", [], |r| r.get::<_, i64>(0))? as u64)
    }

    /// Tracked paths under `dir` (project-relative, no trailing slash).
    pub fn files_under(&self, dir: &str) -> Result<Vec<String>> {
        let prefix = format!("{}/%", escape_like(dir.trim_end_matches('/')));
        let mut stmt = self
            .conn
            .prepare_cached("SELECT path FROM files WHERE path LIKE ? ESCAPE '\\' ORDER BY path")?;
        let rows = stmt.query_map([prefix], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn last_indexed_at(&self) -> Result<Option<i64>> {
        Ok(self
            .conn
            .query_row("SELECT MAX(indexed_at) FROM files", [], |r| r.get(0))?)
    }

    // ------------------------------------------------------------------ search

    /// Ranked symbol search. `query` may carry `kind:`/`lang:`/`path:`/`name:` filters.
    pub fn search_nodes(&self, query: &str, opts: &SearchOptions) -> Result<Vec<SearchResult>> {
        let limit = opts.limit.unwrap_or(100);
        let parsed = parse_query(query);
        let mut kinds = opts.kinds.clone();
        kinds.extend(parsed.kinds.iter().copied());
        let mut languages = opts.languages.clone();
        languages.extend(parsed.languages.iter().copied());
        let text = parsed.text.as_str();

        let mut results = if text.is_empty() {
            self.search_by_filters(&kinds, &languages, limit * 5)?
        } else {
            self.search_fts(text, &kinds, &languages, limit)?
        };
        if results.is_empty() && text.chars().count() >= 2 {
            results = self.search_like(text, &kinds, &languages, limit)?;
        }
        if results.is_empty() && text.chars().count() >= 3 {
            results = self.search_fuzzy(text, &kinds, &languages, limit)?;
        }

        // Exact-name hits for each query word, even when FTS ranked them low.
        if !results.is_empty() && !text.is_empty() {
            let mut seen: HashSet<String> = results.iter().map(|r| r.node.id.clone()).collect();
            let top = results.iter().map(|r| r.score).fold(f64::MIN, f64::max);
            for term in text.split_whitespace().filter(|t| t.len() >= 2) {
                for node in self.filtered_nodes(
                    "lower(name) = lower(?)",
                    &[term.to_string()],
                    &kinds,
                    &languages,
                    20,
                )? {
                    if seen.insert(node.id.clone()) {
                        results.push(SearchResult { node, score: top });
                    }
                }
            }
        }

        if !results.is_empty() && !text.is_empty() {
            let paths: Vec<&str> = results
                .iter()
                .map(|r| r.node.file_path.as_str())
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            let generated = self.generated_among(&paths)?;
            for r in &mut results {
                let low = generated.contains(&r.node.file_path)
                    || self
                        .deprioritized
                        .as_ref()
                        .is_some_and(|m| m(&r.node.file_path));
                let name_bonus = name_match_bonus(&r.node.name, text);
                r.score += kind_bonus(r.node.kind)
                    + score_path_relevance(&r.node.file_path, text, &self.project_name_tokens, low)
                    // A deprioritized exact name still beats unrelated code, not real code.
                    + if low { (name_bonus * 0.25).round() } else { name_bonus };
            }
            results.sort_by(|a, b| {
                b.score
                    .total_cmp(&a.score)
                    .then_with(|| a.node.id.cmp(&b.node.id))
            });
        }

        let path_filters: Vec<String> = parsed
            .path_filters
            .iter()
            .chain(&opts.include_patterns)
            .map(|p| p.to_lowercase())
            .collect();
        if !path_filters.is_empty() {
            results.retain(|r| {
                let fp = r.node.file_path.to_lowercase();
                path_filters.iter().any(|p| fp.contains(p.as_str()))
            });
        }
        let excludes: Vec<String> = opts
            .exclude_patterns
            .iter()
            .map(|p| p.to_lowercase())
            .collect();
        if !excludes.is_empty() {
            results.retain(|r| {
                let fp = r.node.file_path.to_lowercase();
                !excludes.iter().any(|p| fp.contains(p.as_str()))
            });
        }
        if !parsed.name_filters.is_empty() {
            let names: Vec<String> = parsed
                .name_filters
                .iter()
                .map(|n| n.to_lowercase())
                .collect();
            results.retain(|r| {
                let n = r.node.name.to_lowercase();
                names.iter().any(|f| n.contains(f.as_str()))
            });
        }
        let offset = opts.offset.unwrap_or(0);
        Ok(results.into_iter().skip(offset).take(limit).collect())
    }

    fn filtered_nodes(
        &self,
        cond: &str,
        args: &[String],
        kinds: &[NodeKind],
        languages: &[Language],
        limit: usize,
    ) -> Result<Vec<Node>> {
        let mut sql = format!("SELECT {NODE_COLUMNS} FROM nodes WHERE {cond}");
        let mut params: Vec<String> = args.to_vec();
        if !kinds.is_empty() {
            sql.push_str(&format!(" AND kind IN ({})", placeholders(kinds.len())));
            params.extend(kinds.iter().map(|k| k.as_str().to_string()));
        }
        if !languages.is_empty() {
            sql.push_str(&format!(
                " AND language IN ({})",
                placeholders(languages.len())
            ));
            params.extend(languages.iter().map(|l| l.as_str().to_string()));
        }
        sql.push_str(&format!(" LIMIT {limit}"));
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), row_to_node)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn search_by_filters(
        &self,
        kinds: &[NodeKind],
        languages: &[Language],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        Ok(self
            .filtered_nodes("1 = 1", &[], kinds, languages, limit)?
            .into_iter()
            .map(|node| SearchResult { node, score: 1.0 })
            .collect())
    }

    fn search_fts(
        &self,
        text: &str,
        kinds: &[NodeKind],
        languages: &[Language],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let terms: Vec<String> = text
            .replace("::", " ")
            .chars()
            .filter(|c| !matches!(c, '\'' | '"' | '*' | '(' | ')' | ':' | '^'))
            .collect::<String>()
            .split_whitespace()
            .filter(|t| {
                !matches!(
                    t.to_ascii_uppercase().as_str(),
                    "AND" | "OR" | "NOT" | "NEAR"
                )
            })
            .map(|t| format!("\"{t}\"*"))
            .collect();
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let fts = terms.join(" OR ");
        let mut sql = format!(
            "SELECT {cols}, -bm25(nodes_fts, 0.0, 10.0, 5.0, 1.0, 2.0) AS score
               FROM nodes_fts JOIN nodes n ON n.rowid = nodes_fts.rowid
              WHERE nodes_fts MATCH ?",
            cols = NODE_COLUMNS
                .split(", ")
                .map(|c| format!("n.{c}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let mut params: Vec<String> = vec![fts];
        if !kinds.is_empty() {
            sql.push_str(&format!(" AND n.kind IN ({})", placeholders(kinds.len())));
            params.extend(kinds.iter().map(|k| k.as_str().to_string()));
        }
        if !languages.is_empty() {
            sql.push_str(&format!(
                " AND n.language IN ({})",
                placeholders(languages.len())
            ));
            params.extend(languages.iter().map(|l| l.as_str().to_string()));
        }
        sql.push_str(&format!(
            " ORDER BY score DESC LIMIT {}",
            (limit * 5).max(100)
        ));
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |r| {
            Ok(SearchResult {
                node: row_to_node(r)?,
                score: r.get(21)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn search_like(
        &self,
        text: &str,
        kinds: &[NodeKind],
        languages: &[Language],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let esc = escape_like(text);
        let contains = format!("%{esc}%");
        let mut sql = format!(
            "SELECT {NODE_COLUMNS},
               CASE WHEN name = ?1 THEN 1.0 WHEN name LIKE ?2 ESCAPE '\\' THEN 0.9
                    WHEN name LIKE ?3 ESCAPE '\\' THEN 0.8 WHEN qualified_name LIKE ?3 ESCAPE '\\' THEN 0.7
                    ELSE 0.5 END AS score
               FROM nodes WHERE (name LIKE ?3 ESCAPE '\\' OR qualified_name LIKE ?3 ESCAPE '\\'
                                 OR docstring LIKE ?3 ESCAPE '\\' OR signature LIKE ?3 ESCAPE '\\')"
        );
        let mut params: Vec<String> = vec![text.to_string(), format!("{esc}%"), contains];
        if !kinds.is_empty() {
            sql.push_str(&format!(" AND kind IN ({})", placeholders(kinds.len())));
            params.extend(kinds.iter().map(|k| k.as_str().to_string()));
        }
        if !languages.is_empty() {
            sql.push_str(&format!(
                " AND language IN ({})",
                placeholders(languages.len())
            ));
            params.extend(languages.iter().map(|l| l.as_str().to_string()));
        }
        sql.push_str(&format!(" ORDER BY score DESC, length(name) LIMIT {limit}"));
        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |r| {
            Ok(SearchResult {
                node: row_to_node(r)?,
                score: r.get(21)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    fn search_fuzzy(
        &self,
        text: &str,
        kinds: &[NodeKind],
        languages: &[Language],
        limit: usize,
    ) -> Result<Vec<SearchResult>> {
        let lowered = text.to_lowercase();
        let max = if lowered.chars().count() <= 4 { 1 } else { 2 };
        let mut candidates: Vec<(usize, String)> = Vec::new();
        for name in self.distinct_names()? {
            let d = bounded_edit_distance(&name.to_lowercase(), &lowered, max);
            if d <= max {
                candidates.push((d, name));
            }
        }
        candidates.sort();
        candidates.truncate((limit * 2).max(50));
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        for (d, name) in candidates {
            for node in self.filtered_nodes("name = ?", &[name], kinds, languages, 5)? {
                if seen.insert(node.id.clone()) {
                    out.push(SearchResult {
                        node,
                        score: 1.0 / (1.0 + d as f64),
                    });
                }
            }
            if out.len() >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// Every distinct symbol name (excluding file nodes).
    pub fn distinct_names(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT DISTINCT name FROM nodes WHERE kind != 'file'")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Which of `names` exist as symbol names.
    pub fn existing_names(&self, names: &[String]) -> Result<HashSet<String>> {
        let mut out = HashSet::new();
        for chunk in names.chunks(IN_CHUNK) {
            let mut stmt = self.conn.prepare(&format!(
                "SELECT DISTINCT name FROM nodes WHERE name IN ({})",
                placeholders(chunk.len())
            ))?;
            for n in stmt.query_map(params_from_iter(chunk), |r| r.get::<_, String>(0))? {
                out.insert(n?);
            }
        }
        Ok(out)
    }

    // ----------------------------------------------------------- name segments

    /// Rebuild the segment → name vocabulary from current node names.
    pub fn rebuild_name_segments(&mut self) -> Result<()> {
        let names = self.distinct_names()?;
        self.transaction(|tx| {
            tx.execute("DELETE FROM name_segments", [])?;
            let mut ins = tx.prepare_cached(
                "INSERT OR IGNORE INTO name_segments (segment, name) VALUES (?, ?)",
            )?;
            for name in &names {
                for seg in split_identifier_segments(name) {
                    ins.execute(params![seg, name])?;
                }
            }
            Ok(())
        })
    }

    /// Symbol names containing the word `segment`.
    pub fn names_for_segment(&self, segment: &str, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare_cached("SELECT name FROM name_segments WHERE segment = ? LIMIT ?")?;
        let rows = stmt.query_map(params![segment, limit as i64], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    // ------------------------------------------------------------------- stats

    pub fn stats(&self) -> Result<GraphStats> {
        let grouped = |sql: &str| -> Result<Vec<(String, u64)>> {
            let mut stmt = self.conn.prepare(sql)?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u64))
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        };
        let size = |suffix: &str| -> u64 {
            let mut p = self.path.as_os_str().to_owned();
            p.push(suffix);
            std::fs::metadata(PathBuf::from(p))
                .map(|m| m.len())
                .unwrap_or(0)
        };
        Ok(GraphStats {
            node_count: self.node_count()?,
            edge_count: self.edge_count()?,
            file_count: self.file_count()?,
            nodes_by_kind: grouped(
                "SELECT kind, COUNT(*) FROM nodes GROUP BY kind ORDER BY COUNT(*) DESC",
            )?,
            edges_by_kind: grouped(
                "SELECT kind, COUNT(*) FROM edges GROUP BY kind ORDER BY COUNT(*) DESC",
            )?,
            files_by_language: grouped(
                "SELECT language, COUNT(*) FROM files GROUP BY language ORDER BY COUNT(*) DESC",
            )?,
            db_size_bytes: size(""),
            wal_size_bytes: size("-wal"),
            last_updated: self.last_indexed_at()?.unwrap_or(0),
        })
    }

    /// Fold the WAL back into the main database file.
    pub fn checkpoint(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    pub fn optimize(&self) -> Result<()> {
        self.conn.execute_batch(
            "INSERT INTO nodes_fts(nodes_fts) VALUES ('optimize'); PRAGMA optimize;",
        )?;
        Ok(())
    }
}

fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn insert_node(tx: &Connection, n: &Node) -> Result<()> {
    let mut stmt = tx.prepare_cached(&format!(
        "INSERT OR REPLACE INTO nodes ({NODE_COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)"
    ))?;
    stmt.execute(params![
        n.id,
        n.kind.as_str(),
        n.name,
        n.qualified_name,
        n.file_path,
        n.language.as_str(),
        n.start_line,
        n.end_line,
        n.start_column,
        n.end_column,
        n.docstring,
        n.signature,
        n.visibility.map(|v| v.as_str()),
        n.is_exported as i64,
        n.is_async as i64,
        n.is_static as i64,
        n.is_abstract as i64,
        json_list(&n.decorators),
        json_list(&n.type_parameters),
        n.return_type,
        n.updated_at,
    ])?;
    Ok(())
}

/// Insert an edge; returns 1 if it was new. Edges whose endpoints don't
/// exist are skipped (foreign keys would reject them).
fn insert_edge(tx: &Connection, e: &Edge) -> Result<usize> {
    let mut stmt = tx.prepare_cached(
        "INSERT OR IGNORE INTO edges (source, target, kind, metadata, line, col, provenance)
         SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
          WHERE EXISTS (SELECT 1 FROM nodes WHERE id = ?1) AND EXISTS (SELECT 1 FROM nodes WHERE id = ?2)",
    )?;
    Ok(stmt.execute(params![
        e.source,
        e.target,
        e.kind.as_str(),
        e.metadata.as_ref().map(|m| m.to_string()),
        e.line,
        e.column,
        e.provenance.map(Provenance::as_str),
    ])?)
}

fn clear_file(tx: &Connection, path: &str) -> Result<()> {
    // References that resolved into this file go back to pending: the nodes
    // they pointed at are about to be replaced.
    tx.execute(
        "UPDATE refs SET status = 'pending', target_id = NULL, target_file = NULL
          WHERE target_file = ?1 AND file_path != ?1",
        [path],
    )?;
    tx.execute("DELETE FROM nodes WHERE file_path = ?", [path])?;
    Ok(())
}

fn write_file(tx: &Connection, file: &FileRecord, result: &ExtractionResult) -> Result<()> {
    clear_file(tx, &file.path)?;
    for n in &result.nodes {
        insert_node(tx, n)?;
    }
    for e in &result.edges {
        insert_edge(tx, e)?;
    }
    let ids: HashSet<&str> = result.nodes.iter().map(|n| n.id.as_str()).collect();
    let mut ins = tx.prepare_cached(
        "INSERT INTO refs (from_node_id, reference_name, reference_kind, line, col, candidates, file_path, language, name_tail)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    for r in &result.unresolved_references {
        if !ids.contains(r.from_node_id.as_str()) {
            continue;
        }
        ins.execute(params![
            r.from_node_id,
            r.reference_name,
            r.reference_kind.as_str(),
            r.line,
            r.column,
            json_list(&r.candidates),
            r.file_path.as_deref().unwrap_or(&file.path),
            r.language.unwrap_or(file.language).as_str(),
            name_tail(&r.reference_name),
        ])?;
    }
    let errors =
        (!file.errors.is_empty()).then(|| serde_json::to_string(&file.errors).unwrap_or_default());
    tx.execute(
        &format!(
            "INSERT INTO files ({FILE_COLUMNS}) VALUES (?,?,?,?,?,?,?,?,?)
             ON CONFLICT(path) DO UPDATE SET content_hash = excluded.content_hash, language = excluded.language,
               size = excluded.size, modified_at = excluded.modified_at, indexed_at = excluded.indexed_at,
               node_count = excluded.node_count, errors = excluded.errors, generated = excluded.generated"
        ),
        params![
            file.path,
            file.content_hash,
            file.language.as_str(),
            file.size as i64,
            file.modified_at,
            file.indexed_at,
            result.nodes.len() as i64,
            errors,
            file.generated as i64,
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests;
