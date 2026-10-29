//! What the C/C++ strategies read — files and their lines, the symbols, the
//! project's file list — and the memos upstream keeps per resolution context.

use super::js;
use crate::symbols::{SymId, Symbols};
use codegraph_core::{Language, NodeKind, ReferenceKind};
use fancy_regex::Regex;
use std::any::Any;
use std::cell::{OnceCell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::Rc;

/// A file's text and its lines (`split(/\r?\n/)`).
pub struct Source {
    pub text: String,
    pub lines: Vec<String>,
}

/// The reference being resolved (upstream's `UnresolvedRef`).
#[derive(Debug, Clone)]
pub struct Ref {
    pub from: SymId,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub name: String,
    pub kind: ReferenceKind,
    pub lang: Language,
}

impl Ref {
    pub fn with_name(&self, name: impl Into<String>) -> Ref {
        Ref {
            name: name.into(),
            ..self.clone()
        }
    }

    pub fn is_cpp(&self) -> bool {
        self.lang == Language::Cpp
    }

    pub fn is_c(&self) -> bool {
        self.lang == Language::C
    }
}

type MemoMap = RefCell<HashMap<(u16, String), Rc<dyn Any>>>;

pub struct Ctx<'a> {
    pub syms: &'a Symbols,
    pub root: &'a Path,
    sources: RefCell<HashMap<String, Option<Rc<Source>>>>,
    regexes: RefCell<HashMap<String, Rc<Regex>>>,
    files: OnceCell<Vec<String>>,
    memo: MemoMap,
}

impl<'a> Ctx<'a> {
    pub fn new(syms: &'a Symbols, root: &'a Path) -> Self {
        Ctx {
            syms,
            root,
            sources: RefCell::default(),
            regexes: RefCell::default(),
            files: OnceCell::new(),
            memo: RefCell::default(),
        }
    }

    /// A compiled regex, cached by its pattern.
    pub fn re(&self, pattern: &str) -> Rc<Regex> {
        if let Some(r) = self.regexes.borrow().get(pattern) {
            return r.clone();
        }
        let r = Rc::new(Regex::new(pattern).unwrap_or_else(|e| {
            panic!("bad regex {pattern:?}: {e}");
        }));
        let mut map = self.regexes.borrow_mut();
        if map.len() > 20_000 {
            map.clear();
        }
        map.insert(pattern.to_string(), r.clone());
        r
    }

    /// `context.readFile(file)`: a project file's text, from disk.
    pub fn source(&self, file: &str) -> Option<Rc<Source>> {
        if let Some(s) = self.sources.borrow().get(file) {
            return s.clone();
        }
        let s = std::fs::read(self.root.join(file)).ok().map(|bytes| {
            let text = String::from_utf8(bytes)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            let lines = js::split_lines(&text);
            Rc::new(Source { text, lines })
        });
        let mut map = self.sources.borrow_mut();
        if map.len() > 2048 {
            map.clear();
        }
        map.insert(file.to_string(), s.clone());
        s
    }

    pub fn read(&self, file: &str) -> Option<Rc<Source>> {
        self.source(file)
    }

    /// Line `line` (1-based) of a file, or "".
    pub fn line(&self, file: &str, line: u32) -> String {
        self.source(file)
            .and_then(|s| {
                (line as usize)
                    .checked_sub(1)
                    .and_then(|i| s.lines.get(i).cloned())
            })
            .unwrap_or_default()
    }

    /// `context.getAllFiles()`: every indexed path, sorted.
    pub fn all_files(&self) -> &[String] {
        self.files.get_or_init(|| {
            let mut files: Vec<String> = self.syms.files().cloned().collect();
            files.sort();
            files
        })
    }

    /// `context.fileExists(path)`: indexed, or on disk inside the project.
    pub fn file_exists(&self, path: &str) -> bool {
        if self.syms.file_exists(path) || self.syms.file_exists(&path.replace('\\', "/")) {
            return true;
        }
        if path.split('/').any(|seg| seg == "..") || Path::new(path).is_absolute() {
            return false;
        }
        self.memo(0, format!("exists\0{path}"), || {
            self.root.join(path).exists()
        })
    }

    pub fn is_indexed(&self, path: &str) -> bool {
        self.syms.file_exists(path)
    }

    /// `getNodesByName`.
    pub fn named(&self, name: &str) -> &'a [SymId] {
        self.syms.named(name)
    }

    /// `getNodesByQualifiedName`.
    pub fn by_qname(&self, qname: &str) -> &'a [SymId] {
        self.syms.by_qualified(qname)
    }

    /// `getNodesInFile`.
    pub fn in_file(&self, file: &str) -> &'a [SymId] {
        self.syms.in_file(file)
    }

    pub fn sym(&self, id: SymId) -> &'a crate::symbols::Sym {
        self.syms.get(id)
    }

    pub fn kind(&self, id: SymId) -> NodeKind {
        self.syms.get(id).kind
    }

    /// Memoised `f`, by family and key (upstream's per-context WeakMaps).
    pub fn memo<T: Clone + 'static>(&self, family: u16, key: String, f: impl FnOnce() -> T) -> T {
        if let Some(v) = self.memo.borrow().get(&(family, key.clone())) {
            if let Some(v) = v.downcast_ref::<T>() {
                return v.clone();
            }
        }
        let v = f();
        self.memo
            .borrow_mut()
            .insert((family, key), Rc::new(v.clone()));
        v
    }

    /// A memo already computed, if any.
    pub fn memo_get<T: Clone + 'static>(&self, family: u16, key: &str) -> Option<T> {
        self.memo
            .borrow()
            .get(&(family, key.to_string()))
            .and_then(|v| v.downcast_ref::<T>().cloned())
    }

    pub fn memo_set<T: 'static>(&self, family: u16, key: String, v: T) {
        self.memo.borrow_mut().insert((family, key), Rc::new(v));
    }
}

/// Memo families, one per upstream memo table.
pub mod memo {
    pub const FILE_EXISTS: u16 = 0;
    pub const IMPORT_PATH: u16 = 1;
    pub const INCLUDE_DIRS: u16 = 2;
    pub const INCLUDE_TARGET: u16 = 3;
    pub const INCLUDERS: u16 = 4;
    pub const INCLUDES_BY_NAME: u16 = 5;
    pub const INCLUDES_BY_STEM: u16 = 6;
    pub const NS_ALIASES: u16 = 7;
    pub const NS_MACROS: u16 = 8;
    pub const NS_FRAMES: u16 = 9;
    pub const NON_CODE_LINES: u16 = 10;
    pub const LOCAL_ALIAS_LINES: u16 = 11;
    pub const TEMPLATE_PARAMS: u16 = 12;
    pub const BASES: u16 = 13;
    pub const LOCAL_TO: u16 = 14;
    pub const USINGS: u16 = 15;
    pub const FEATURES: u16 = 16;
    pub const MACRO_VIS: u16 = 17;
    pub const STRIPPED: u16 = 18;
}
