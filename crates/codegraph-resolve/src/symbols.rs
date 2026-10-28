//! In-memory symbol index over the whole graph, built once per resolution run.

use codegraph_core::{Language, Node, NodeKind};
use codegraph_store::Store;
use std::collections::{HashMap, HashSet};

pub type SymId = usize;

/// The parts of a node resolution needs.
#[derive(Debug, Clone)]
pub struct Sym {
    pub id: String,
    pub name: String,
    pub qualified_name: String,
    pub kind: NodeKind,
    pub file: String,
    pub language: Language,
    pub start_line: u32,
    pub end_line: u32,
    pub exported: bool,
    /// Statement text of an `import` node.
    pub statement: Option<String>,
    /// Declared inside a function/method body: visible only there.
    pub local: bool,
}

impl Sym {
    /// Qualified name of the enclosing type/namespace (`A::run` → `A`).
    pub fn owner(&self) -> Option<&str> {
        self.qualified_name.rsplit_once("::").map(|(o, _)| o)
    }
}

/// Languages whose symbols can reference each other.
pub fn family(lang: Language) -> u8 {
    use Language::*;
    match lang {
        TypeScript | JavaScript | Tsx | Jsx | ArkTs | Svelte | Vue | Astro => 1,
        C | Cpp | ObjC => 2,
        Java | Kotlin | Scala => 3,
        CSharp | VbNet | Razor => 4,
        Lua | Luau => 5,
        Cfml | CfScript | CfQuery => 6,
        Php | Twig => 7,
        Swift => 8,
        other => 100 + other as u8,
    }
}

pub fn same_family(a: Language, b: Language) -> bool {
    family(a) == family(b)
        || matches!(
            (a, b),
            (Language::Swift, Language::ObjC) | (Language::ObjC, Language::Swift)
        )
}

/// Languages whose identifiers are case-insensitive.
pub fn case_insensitive(lang: Language) -> bool {
    matches!(
        lang,
        Language::Php
            | Language::VbNet
            | Language::Pascal
            | Language::Cobol
            | Language::Cfml
            | Language::CfScript
    )
}

pub struct Symbols {
    pub syms: Vec<Sym>,
    by_id: HashMap<String, SymId>,
    by_name: HashMap<String, Vec<SymId>>,
    by_lower: HashMap<String, Vec<SymId>>,
    by_file: HashMap<String, Vec<SymId>>,
    /// Owner qualified name → member symbols.
    members: HashMap<String, Vec<SymId>>,
    files: HashSet<String>,
    files_lower: HashMap<String, String>,
    /// Supertype edges: sym → its direct supertypes.
    pub supers: HashMap<SymId, Vec<SymId>>,
}

fn is_indexable(n: &Node) -> bool {
    !matches!(n.kind, NodeKind::Parameter)
}

impl Symbols {
    pub fn load(store: &Store) -> Result<Self, codegraph_store::StoreError> {
        let nodes = store.all_nodes()?;
        let mut s = Symbols {
            syms: Vec::with_capacity(nodes.len()),
            by_id: HashMap::with_capacity(nodes.len()),
            by_name: HashMap::new(),
            by_lower: HashMap::new(),
            by_file: HashMap::new(),
            members: HashMap::new(),
            files: HashSet::new(),
            files_lower: HashMap::new(),
            supers: HashMap::new(),
        };
        for n in nodes.into_iter().filter(is_indexable) {
            s.add(n);
        }
        s.mark_locals();
        for f in store.all_files()? {
            s.files_lower.insert(f.path.to_lowercase(), f.path.clone());
            s.files.insert(f.path);
        }
        for kind in [
            codegraph_core::EdgeKind::Extends,
            codegraph_core::EdgeKind::Implements,
        ] {
            for e in store.edges_of_kind(kind)? {
                if let (Some(&a), Some(&b)) = (s.by_id.get(&e.source), s.by_id.get(&e.target)) {
                    s.add_super(a, b);
                }
            }
        }
        Ok(s)
    }

    fn add(&mut self, n: Node) {
        let idx = self.syms.len();
        self.by_id.insert(n.id.clone(), idx);
        self.by_name.entry(n.name.clone()).or_default().push(idx);
        self.by_lower
            .entry(n.name.to_lowercase())
            .or_default()
            .push(idx);
        self.by_file
            .entry(n.file_path.clone())
            .or_default()
            .push(idx);
        if let Some((owner, _)) = n.qualified_name.rsplit_once("::") {
            if n.kind != NodeKind::File {
                self.members.entry(owner.to_string()).or_default().push(idx);
            }
        }
        self.syms.push(Sym {
            id: n.id,
            exported: n.is_exported,
            name: n.name,
            qualified_name: n.qualified_name,
            kind: n.kind,
            file: n.file_path,
            language: n.language,
            start_line: n.start_line,
            end_line: n.end_line,
            statement: if n.kind == NodeKind::Import {
                n.signature
            } else {
                None
            },
            local: false,
        });
    }

    /// Flag symbols whose owner (same file) is a function or method.
    fn mark_locals(&mut self) {
        let mut callables: HashSet<(&str, &str)> = HashSet::new();
        for s in &self.syms {
            if s.kind.is_callable() {
                callables.insert((s.file.as_str(), s.qualified_name.as_str()));
            }
        }
        let locals: Vec<SymId> = self
            .syms
            .iter()
            .enumerate()
            .filter(|(_, s)| {
                s.owner()
                    .is_some_and(|o| callables.contains(&(s.file.as_str(), o)))
            })
            .map(|(i, _)| i)
            .collect();
        for i in locals {
            self.syms[i].local = true;
        }
    }

    /// Whether `target` can be referenced from `from`: a local symbol only
    /// from within its enclosing function.
    pub fn visible_from(&self, target: SymId, from: SymId) -> bool {
        let t = &self.syms[target];
        if !t.local {
            return true;
        }
        let f = &self.syms[from];
        let owner = t.owner().unwrap_or("");
        f.file == t.file
            && (f.qualified_name == owner || f.qualified_name.starts_with(&format!("{owner}::")))
    }

    pub fn add_super(&mut self, sub: SymId, sup: SymId) {
        let list = self.supers.entry(sub).or_default();
        if !list.contains(&sup) {
            list.push(sup);
        }
    }

    pub fn get(&self, id: SymId) -> &Sym {
        &self.syms[id]
    }

    pub fn by_id(&self, id: &str) -> Option<SymId> {
        self.by_id.get(id).copied()
    }

    pub fn named(&self, name: &str) -> &[SymId] {
        self.by_name.get(name).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn named_any_case(&self, name: &str) -> &[SymId] {
        self.by_lower
            .get(&name.to_lowercase())
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Symbols named `name`, case-insensitively for languages that are.
    pub fn named_for(&self, name: &str, lang: Language) -> &[SymId] {
        if case_insensitive(lang) {
            self.named_any_case(name)
        } else {
            self.named(name)
        }
    }

    pub fn in_file(&self, file: &str) -> &[SymId] {
        self.by_file.get(file).map(Vec::as_slice).unwrap_or(&[])
    }

    pub fn members_of(&self, owner_qualified: &str) -> &[SymId] {
        self.members
            .get(owner_qualified)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn file_exists(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    /// Exact-path lookup, falling back to a case-insensitive match.
    pub fn find_file(&self, path: &str) -> Option<&str> {
        if let Some(p) = self.files.get(path) {
            return Some(p.as_str());
        }
        self.files_lower
            .get(&path.to_lowercase())
            .map(String::as_str)
    }

    pub fn files(&self) -> impl Iterator<Item = &String> {
        self.files.iter()
    }

    /// The `file` node of a path.
    pub fn file_node(&self, file: &str) -> Option<SymId> {
        self.in_file(file)
            .iter()
            .copied()
            .find(|&i| self.syms[i].kind == NodeKind::File)
    }

    /// The innermost non-file symbol in `file` whose span covers `line`.
    pub fn enclosing(&self, file: &str, line: u32) -> Option<SymId> {
        self.in_file(file)
            .iter()
            .copied()
            .filter(|&i| {
                let s = &self.syms[i];
                s.kind != NodeKind::File && s.start_line <= line && line <= s.end_line
            })
            .min_by_key(|&i| self.syms[i].end_line - self.syms[i].start_line)
    }

    /// The type a member symbol belongs to, by qualified name, in the same file first.
    pub fn owner_type(&self, member: SymId) -> Option<SymId> {
        let s = &self.syms[member];
        let owner = s.owner()?;
        let owner_name = owner.rsplit("::").next().unwrap_or(owner);
        let candidates: Vec<SymId> = self
            .named(owner_name)
            .iter()
            .copied()
            .filter(|&i| {
                let c = &self.syms[i];
                c.qualified_name == owner
                    && (c.kind.is_type()
                        || matches!(c.kind, NodeKind::Module | NodeKind::Namespace))
            })
            .collect();
        candidates
            .iter()
            .copied()
            .find(|&i| self.syms[i].file == s.file)
            .or_else(|| candidates.first().copied())
    }

    /// `ty` and all its transitive supertypes (breadth-first, cycle-safe).
    pub fn type_lineage(&self, ty: SymId) -> Vec<SymId> {
        let mut out = vec![ty];
        let mut seen: HashSet<SymId> = [ty].into();
        let mut i = 0;
        while i < out.len() && out.len() < 64 {
            if let Some(sups) = self.supers.get(&out[i]) {
                for &s in sups {
                    if seen.insert(s) {
                        out.push(s);
                    }
                }
            }
            i += 1;
        }
        out
    }

    /// Members named `name` of `ty` or its supertypes, nearest type first.
    pub fn member_named(&self, ty: SymId, name: &str, lang: Language) -> Option<SymId> {
        for t in self.type_lineage(ty) {
            let qn = &self.syms[t].qualified_name;
            let owner = &self.syms[t];
            let named = |m: &SymId| {
                let s = &self.syms[*m];
                s.name == name || case_insensitive(lang) && s.name.eq_ignore_ascii_case(name)
            };
            let members = self.members_of(qn);
            // The type's own file first: two classes may share a name.
            if let Some(&m) = members
                .iter()
                .find(|m| named(m) && self.syms[**m].file == owner.file)
            {
                return Some(m);
            }
            if !one_file_types(owner.language) {
                let hit = members
                    .iter()
                    .copied()
                    .find(|m| named(m) && self.syms[*m].file_or_same_owner(owner));
                if hit.is_some() {
                    return hit;
                }
            }
        }
        None
    }
}

/// Languages whose class body holds all its members, in one file.
fn one_file_types(lang: Language) -> bool {
    matches!(
        lang,
        Language::Python
            | Language::Java
            | Language::TypeScript
            | Language::JavaScript
            | Language::Tsx
            | Language::Jsx
            | Language::Php
    )
}

impl Sym {
    /// Members of a type declared in another file (partial classes, Swift
    /// extensions, Go receiver methods) share its qualified name; accept them
    /// when the language family matches.
    fn file_or_same_owner(&self, owner: &Sym) -> bool {
        self.file == owner.file || same_family(self.language, owner.language)
    }
}
