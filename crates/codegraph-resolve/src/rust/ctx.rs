//! What the Rust strategies read: files (blanked), their scopes and `use`s,
//! the crate graph, and the symbols, each cached for one resolution run.

use super::crates::CrateGraph;
use super::scopes::Scopes;
use super::uses::Uses;
use crate::symbols::{SymId, Symbols};
use codegraph_core::{Language, NodeKind, ReferenceKind};
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

/// A file's text and where each of its lines starts.
pub struct Source {
    pub text: String,
    starts: Vec<usize>,
}

impl Source {
    fn new(text: String) -> Source {
        let mut starts = vec![0];
        starts.extend(text.match_indices('\n').map(|(i, _)| i + 1));
        Source { text, starts }
    }

    pub fn line_count(&self) -> usize {
        if self.text.ends_with('\n') {
            self.starts.len() - 1
        } else {
            self.starts.len()
        }
    }

    /// The 0-based line `i`, without its line break.
    pub fn line0(&self, i: usize) -> Option<&str> {
        if i >= self.line_count() {
            return None;
        }
        let start = self.starts[i];
        let end = self.starts.get(i + 1).map_or(self.text.len(), |e| e - 1);
        Some(self.text[start..end].trim_end_matches('\r'))
    }

    /// Lines `from..to` (0-based), each without its line break.
    pub fn lines_in(&self, from: usize, to: usize) -> impl Iterator<Item = &str> {
        (from..to.min(self.line_count())).filter_map(move |i| self.line0(i))
    }
}

/// The reference being resolved.
#[derive(Debug, Clone)]
pub struct Ref<'r> {
    pub from: SymId,
    pub file: &'r str,
    pub line: u32,
    pub column: u32,
    pub name: &'r str,
    pub kind: ReferenceKind,
}

impl<'r> Ref<'r> {
    pub fn with_name(&self, name: &'r str) -> Ref<'r> {
        Ref {
            name,
            ..self.clone()
        }
    }
}

/// Answers keyed by a family of question and its text.
pub type Memo = RefCell<HashMap<(u8, String), Option<Rc<dyn std::any::Any>>>>;

pub struct Ctx<'a> {
    pub syms: &'a Symbols,
    pub root: &'a Path,
    sources: RefCell<HashMap<String, Option<Rc<Source>>>>,
    scopes: RefCell<HashMap<String, Option<Rc<Scopes>>>>,
    uses: RefCell<HashMap<String, Rc<Uses>>>,
    graph: OnceCell<CrateGraph>,
    /// Memos keyed by text, per family of question.
    pub memo: Memo,
    /// Refs resolved through a typed receiver: a trait's own (private-looking) method is in reach.
    pub typed: RefCell<std::collections::HashSet<(SymId, u32, u32)>>,
}

impl<'a> Ctx<'a> {
    pub fn new(syms: &'a Symbols, root: &'a Path) -> Self {
        Ctx {
            syms,
            root,
            sources: RefCell::default(),
            scopes: RefCell::default(),
            uses: RefCell::default(),
            graph: OnceCell::new(),
            memo: RefCell::default(),
            typed: RefCell::default(),
        }
    }

    pub fn graph(&self) -> &CrateGraph {
        self.graph.get_or_init(|| CrateGraph::load(self.root))
    }

    /// A file's text, as read from disk.
    pub fn source(&self, file: &str) -> Option<Rc<Source>> {
        if let Some(s) = self.sources.borrow().get(file) {
            return s.clone();
        }
        let s = std::fs::read_to_string(self.root.join(file))
            .ok()
            .map(|t| Rc::new(Source::new(t)));
        let mut map = self.sources.borrow_mut();
        if map.len() > 512 {
            map.clear();
        }
        map.insert(file.to_string(), s.clone());
        s
    }

    /// Whether a file is in the index (or on disk, for a manifest's neighbours).
    pub fn file_exists(&self, file: &str) -> bool {
        self.syms.file_exists(file)
    }

    pub fn scopes(&self, file: &str) -> Option<Rc<Scopes>> {
        if let Some(s) = self.scopes.borrow().get(file) {
            return s.clone();
        }
        let s = self
            .source(file)
            .map(|src| Rc::new(Scopes::read(&src.text)));
        let mut map = self.scopes.borrow_mut();
        if map.len() > 256 {
            map.clear();
        }
        map.insert(file.to_string(), s.clone());
        s
    }

    pub fn uses(&self, file: &str) -> Rc<Uses> {
        if let Some(u) = self.uses.borrow().get(file) {
            return u.clone();
        }
        let u = Rc::new(Uses::read(self, file));
        self.uses.borrow_mut().insert(file.to_string(), u.clone());
        u
    }

    /// The 1-based lines of a file.
    pub fn line(&self, file: &str, line: u32) -> Option<String> {
        let src = self.source(file)?;
        src.line0((line as usize).checked_sub(1)?)
            .map(str::to_string)
    }

    /// Where a symbol starts in its file, as an offset into its scopes' text.
    pub fn node_offset(&self, id: SymId) -> usize {
        let s = self.syms.get(id);
        self.scopes(&s.file)
            .map_or(0, |sc| sc.offset(s.start_line, s.start_column))
    }

    /// The offset of a reference in its file's text.
    pub fn ref_offset(&self, r: &Ref<'_>) -> Option<usize> {
        self.scopes(r.file).map(|sc| sc.offset(r.line, r.column))
    }

    pub fn rust_named(&self, name: &str) -> std::vec::IntoIter<SymId> {
        let syms = self.syms;
        syms.named(name)
            .iter()
            .copied()
            .filter(|&i| syms.get(i).language == Language::Rust && syms.get(i).name == name)
            .collect::<Vec<_>>()
            .into_iter()
    }

    pub fn by_qualified(&self, qname: &str) -> Vec<SymId> {
        let tail = qname.rsplit("::").next().unwrap_or(qname);
        self.rust_named(tail)
            .filter(|&i| self.syms.get(i).qualified_name == qname)
            .collect()
    }

    /// Whether a Rust item of one of `kinds` is named `name` in the project.
    pub fn is_project_named(&self, name: &str, kinds: &[NodeKind]) -> bool {
        self.rust_named(name)
            .any(|i| kinds.contains(&self.syms.get(i).kind))
    }

    /// Memoised `f`, by family and key.
    pub fn memo<T: Clone + 'static>(&self, family: u8, key: String, f: impl FnOnce() -> T) -> T {
        if let Some(Some(v)) = self.memo.borrow().get(&(family, key.clone())) {
            if let Some(v) = v.downcast_ref::<T>() {
                return v.clone();
            }
        }
        let v = f();
        let mut m = self.memo.borrow_mut();
        if m.len() > 100_000 {
            m.clear();
        }
        m.insert((family, key), Some(Rc::new(v.clone())));
        v
    }
}
