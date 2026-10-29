//! Rust name resolution, ported from upstream CodeGraph's Rust strategies
//! (`name-matcher.ts`, `rust-scopes.ts`, colbymchenry/codegraph @ 01e0ac3):
//! what a name means where it is written, by the module tree, the `use`s and
//! globs in scope, visibility, and the declared type of a call's receiver.

pub mod crates;
pub mod ctx;
pub mod impls;
pub mod scopes;
pub mod types;
pub mod uses;

pub use ctx::{Ctx, Ref};

use crate::matcher::{kind_accepts, Match};
use crate::symbols::{Sym, SymId};
use codegraph_core::{Language, NodeKind, ReferenceKind, Visibility};
use std::collections::HashSet;
use types::{instance, Hit, Read, Written};
use uses::{is_ident, Walk};

/// Roots that name a crate of the standard library.
pub const STD_ROOTS: &[&str] = &["std", "core", "alloc", "proc_macro"];

/// Names the Rust prelude puts in every module.
pub const PRELUDE: &[&str] = &[
    "Ok", "Err", "Some", "None", "Result", "Option", "Box", "Vec", "String", "Default", "Drop", "Iterator", "IntoIterator", "From",
    "Into", "Clone", "Copy", "Send", "Sync", "Sized", "ToString", "ToOwned", "PartialEq", "Eq", "PartialOrd", "Ord", "AsRef", "AsMut",
    "Fn", "FnMut", "FnOnce", "Extend", "drop",
];

/// Rust items declared in a module, whose own `pub` (or its absence) says who can name them.
pub const MODULE_ITEM_KINDS: &[NodeKind] = &[
    NodeKind::Function,
    NodeKind::Struct,
    NodeKind::Enum,
    NodeKind::Union,
    NodeKind::TypeAlias,
    NodeKind::Trait,
    NodeKind::Constant,
    NodeKind::Variable,
];

pub const DECLARED_TYPE_KINDS: &[NodeKind] = &[NodeKind::Struct, NodeKind::Enum, NodeKind::Union, NodeKind::TypeAlias, NodeKind::Trait];
pub const OWNER_KINDS: &[NodeKind] = &[NodeKind::Struct, NodeKind::Enum, NodeKind::Union];

/// Std types a crate can implement a trait for but never declare a method on.
pub const STD_TYPES: &[&str] = &[
    "String", "str", "Vec", "VecDeque", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "BinaryHeap", "LinkedList", "Box", "Rc", "Arc",
    "Weak", "Cow", "Cell", "RefCell", "Mutex", "RwLock", "Option", "Result", "PathBuf", "Path", "OsString", "OsStr", "CString", "CStr",
    "NonNull", "Pin", "Duration", "Instant", "SystemTime", "PhantomData", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16",
    "i32", "i64", "i128", "isize", "f32", "f64", "bool", "char",
];

pub const PRIMITIVE_TYPES: &[&str] = &[
    "bool", "char", "str", "u8", "u16", "u32", "u64", "u128", "usize", "i8", "i16", "i32", "i64", "i128", "isize", "f32", "f64",
];

/// Types that hand a method call on to what they hold when the method is not their own.
pub const DEREF_WRAPPERS: &[&str] = &[
    "Box", "Rc", "Arc", "Pin", "Cow", "MutexGuard", "RwLockReadGuard", "RwLockWriteGuard", "Ref", "RefMut", "ManuallyDrop",
];
pub const SELF_WRAPPERS: &[&str] = &["Pin", "Box", "Rc", "Arc"];
pub const MARKER_TRAITS: &[&str] = &["Send", "Sync", "Unpin", "Sized", "Copy", "UnwindSafe", "RefUnwindSafe"];

const RC_INHERENT: &str = "new new_cyclic new_uninit new_uninit_slice new_zeroed pin try_new try_unwrap into_inner unwrap_or_clone into_raw as_ptr from_raw increment_strong_count decrement_strong_count downgrade weak_count strong_count get_mut make_mut ptr_eq downcast assume_init";
const PIN_METHODS: &str = "as_ref as_mut as_deref_mut set get_ref get_mut get_unchecked_mut into_ref map_unchecked map_unchecked_mut";
const POINTER_TRAIT_METHODS: &str = "clone clone_from as_ref borrow deref fmt eq ne partial_cmp lt le gt ge cmp max min clamp hash into try_into to_string";

/// What std declares on its own types, which Rust picks before any trait's function of the same name.
pub fn std_inherent(ty: &str, name: &str) -> bool {
    let list: &str = match ty {
        "Arc" | "Rc" => RC_INHERENT,
        "Weak" => "new upgrade strong_count weak_count as_ptr into_raw from_raw ptr_eq",
        "Box" => "new new_uninit new_uninit_slice new_zeroed pin try_new from_raw into_raw from_non_null into_non_null leak into_pin into_boxed_slice into_inner as_ptr as_mut_ptr downcast assume_init write",
        "NonNull" => "new new_unchecked dangling as_ptr as_ref as_mut cast add sub offset byte_add byte_sub byte_offset offset_from read read_unaligned read_volatile write write_bytes write_unaligned write_volatile replace swap copy_to copy_from copy_to_nonoverlapping copy_from_nonoverlapping drop_in_place from_ref from_mut slice_from_raw_parts len addr with_addr map_addr align_offset is_aligned",
        "Pin" => return "new new_unchecked into_inner into_inner_unchecked static_ref static_mut".split(' ').any(|n| n == name) || PIN_METHODS.split(' ').any(|n| n == name),
        "Cell" => "new set swap replace into_inner get update as_ptr get_mut from_mut take",
        "RefCell" => "new into_inner replace replace_with swap borrow try_borrow borrow_mut try_borrow_mut as_ptr get_mut take",
        "Mutex" => "new lock try_lock is_poisoned clear_poison into_inner get_mut",
        "RwLock" => "new read try_read write try_write is_poisoned clear_poison into_inner get_mut",
        "Vec" => "new with_capacity from_raw_parts capacity reserve reserve_exact try_reserve try_reserve_exact shrink_to_fit shrink_to into_boxed_slice truncate as_slice as_mut_slice as_ptr as_mut_ptr set_len swap_remove insert remove retain retain_mut dedup_by_key dedup_by dedup push pop pop_if append drain clear len is_empty split_off resize_with resize leak spare_capacity_mut extend_from_slice extend_from_within splice extract_if into_flattened",
        "String" => "new with_capacity from_utf8 from_utf8_lossy from_utf8_unchecked from_utf16 from_utf16_lossy from_raw_parts into_bytes as_str as_mut_str push_str extend_from_within capacity reserve reserve_exact try_reserve try_reserve_exact shrink_to_fit shrink_to push as_bytes truncate pop remove retain insert insert_str as_mut_vec len is_empty split_off clear drain replace_range into_boxed_str leak",
        "Option" => "is_some is_some_and is_none is_none_or as_ref as_mut as_pin_ref as_pin_mut as_slice as_mut_slice expect unwrap unwrap_or unwrap_or_else unwrap_or_default unwrap_unchecked map inspect map_or map_or_else ok_or ok_or_else as_deref as_deref_mut iter iter_mut and and_then filter or or_else xor insert get_or_insert get_or_insert_default get_or_insert_with take take_if replace zip unzip copied cloned transpose flatten",
        "Result" => "is_ok is_ok_and is_err is_err_and ok err as_ref as_mut map map_or map_or_else map_err inspect inspect_err as_deref as_deref_mut iter iter_mut expect unwrap unwrap_or_default expect_err unwrap_err into_ok into_err and and_then or or_else unwrap_or unwrap_or_else unwrap_unchecked unwrap_err_unchecked copied cloned transpose flatten",
        _ => return false,
    };
    list.split(' ').any(|n| n == name)
}

/// What a std wrapper a value is reached through answers itself, ahead of the type it wraps.
pub fn wrapper_own_method(wrapper: &str, name: &str) -> bool {
    let extra = match wrapper {
        "Pin" => return PIN_METHODS.split(' ').chain("deref deref_mut fmt eq ne partial_cmp lt le gt ge cmp max min clamp hash into try_into to_string".split(' ')).any(|n| n == name),
        "Box" => "as_mut borrow_mut deref_mut",
        "Rc" | "Arc" => "",
        "ManuallyDrop" => "deref_mut",
        "Cow" => "into_owned to_mut is_borrowed is_owned",
        "MutexGuard" | "RwLockWriteGuard" | "RefMut" => return matches!(name, "deref" | "deref_mut" | "fmt"),
        "RwLockReadGuard" | "Ref" => return matches!(name, "deref" | "fmt"),
        _ => return false,
    };
    POINTER_TRAIT_METHODS.split(' ').chain(extra.split(' ')).any(|n| n == name)
}

/// Methods of Rust's Option / Result / iterators / collections / strings / smart pointers.
const STD_METHODS: &[&str] = &[
    "unwrap", "unwrap_or", "unwrap_or_else", "unwrap_or_default", "unwrap_err", "unwrap_unchecked", "expect", "expect_err", "ok", "err",
    "map", "map_err", "map_or", "map_or_else", "and_then", "or_else", "ok_or", "ok_or_else", "is_some", "is_none", "is_ok", "is_err",
    "is_some_and", "as_ref", "as_mut", "as_deref", "clone", "cloned", "copied", "iter", "iter_mut", "into_iter", "collect", "enumerate",
    "zip", "rev", "chain", "skip", "step_by", "peekable", "flat_map", "filter_map", "flatten", "any", "all", "len", "is_empty", "push",
    "push_str", "pop", "extend", "drain", "clear", "retain", "truncate", "reserve", "with_capacity", "capacity", "sort", "sort_by",
    "sort_by_key", "dedup", "split_off", "contains_key", "to_string", "to_owned", "to_vec", "as_str", "as_bytes", "as_slice", "as_ptr",
    "into", "try_into", "borrow", "borrow_mut", "deref", "deref_mut", "chars", "bytes", "lines", "starts_with", "ends_with", "trim",
    "to_lowercase", "to_uppercase", "windows", "chunks", "then", "then_some", "eq", "cmp", "partial_cmp", "read_to_end", "read_to_string",
    "fetch_add", "fetch_sub", "current_dir", "stdout", "stderr", "stdin", "success", "failure",
];

/// Whether a Rust item is an associated one: a `type` or `const` written in an `impl` or a trait.
pub fn is_associated_item(s: &Sym) -> bool {
    s.language == Language::Rust
        && matches!(s.kind, NodeKind::TypeAlias | NodeKind::Variable | NodeKind::Constant)
        && s.qualified_name.contains("::")
}

fn qualified(target: SymId, confidence: f32) -> Match {
    Match {
        target,
        strategy: "qualified-name",
        confidence,
    }
}

/// What the Rust strategies made of a reference.
pub enum Outcome {
    /// This is the target.
    Hit(Match),
    /// No project target: the reference stays unresolved.
    None,
    /// Not decided here: the generic strategies decide, under the scope gate.
    Pass,
}

impl From<Hit> for Outcome {
    fn from(h: Hit) -> Outcome {
        match h {
            Hit::Found(m) => Outcome::Hit(m),
            Hit::Nothing => Outcome::None,
            Hit::Unread => Outcome::Pass,
        }
    }
}

/// `self|ident` root, then `.link`s (fields, `name()` calls, tuple indexes), then `.method`.
fn chained_call(name: &str) -> Option<(Vec<String>, String)> {
    let mut parts: Vec<&str> = name.split('.').collect();
    if parts.len() < 3 {
        return None;
    }
    let method = parts.pop()?;
    if !is_ident(method) {
        return None;
    }
    let root = parts[0];
    if !is_ident(root) {
        return None;
    }
    for l in &parts[1..] {
        let id = l.strip_suffix("()").unwrap_or(l);
        if !(is_ident(id) || (!l.ends_with("()") && l.chars().all(|c| c.is_ascii_digit()) && !l.is_empty())) {
            return None;
        }
    }
    let links: Vec<String> = parts.iter().map(|s| s.to_string()).collect();
    if root == "self" && links.len() == 2 && is_ident(&links[1]) {
        return None;
    }
    Some((links, method.to_string()))
}

impl<'a> Ctx<'a> {
    /// Resolve a Rust reference; `generic` runs the language-generic strategies.
    pub fn resolve(&self, r: &Ref<'_>, generic: &mut dyn FnMut(&str) -> Option<Match>) -> Option<Match> {
        if r.kind == ReferenceKind::Imports {
            return self.resolve_import(r, generic);
        }
        let name = r.name.trim();
        if r.kind == ReferenceKind::Calls {
            if let Some((chain, method)) = chained_call(name) {
                let by_name = r.with_name(&method);
                let hit = self.written_receiver_call(&by_name, Some(&chain));
                return match hit {
                    Outcome::Hit(m) => Some(m),
                    Outcome::None => None,
                    Outcome::Pass => self.resolve_by_name(&by_name, generic),
                };
            }
        }
        self.resolve_by_name(r, generic)
    }

    fn resolve_by_name(&self, r: &Ref<'_>, generic: &mut dyn FnMut(&str) -> Option<Match>) -> Option<Match> {
        match self.strategies(r) {
            Outcome::Hit(m) => return self.gate(Some(m), r),
            Outcome::None => return None,
            Outcome::Pass => {}
        }
        if is_ident(r.name) {
            if let Some(m) = self.bare(r) {
                return Some(m);
            }
            // The generic strategies only where no Rust declaration of the name is in reach.
            let any = self.rust_named(r.name).any(|i| kind_accepts(r.kind, self.syms.get(i).kind));
            if any {
                return None;
            }
            return self.gate(generic(r.name), r);
        }
        match self.path(r) {
            Outcome::Hit(m) => return self.gate(Some(m), r),
            Outcome::None => return None,
            Outcome::Pass => {}
        }
        self.gate(generic(r.name), r)
    }

    /// The Rust-specific call strategies, in upstream's order.
    fn strategies(&self, r: &Ref<'_>) -> Outcome {
        let name = r.name;
        if r.kind == ReferenceKind::Calls {
            if let Some((recv, method)) = name.rsplit_once('.') {
                if recv == "self" {
                    return match self.self_call(method, r) {
                        Some(m) => Outcome::Hit(m),
                        None => Outcome::None,
                    };
                }
                if let Some(field) = recv.strip_prefix("self.") {
                    if is_ident(field) || field.chars().all(|c| c.is_ascii_digit()) {
                        let typed = self.typed_self_field_call(field, method, r);
                        return match typed {
                            Some(h) => h.into(),
                            None => match self.self_field_call_fallback(field, method, r) {
                                Some(m) => Outcome::Hit(m),
                                None => Outcome::None,
                            },
                        };
                    }
                }
                if is_ident(recv) && recv.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
                    match self.typed_receiver_call(recv, method, r) {
                        Some(Hit::Found(m)) => return Outcome::Hit(m),
                        Some(Hit::Nothing) => return Outcome::None,
                        _ => {}
                    }
                    return Outcome::Pass;
                }
                // A call on a call's value or another written receiver: read where it is written.
                if is_ident(method) {
                    return self.written_receiver_call(&r.with_name(method), None);
                }
            }
            if let Some((owner, method)) = name.rsplit_once("::") {
                if owner == "Self" {
                    if let Some(o) = self.self_path(method, r) {
                        return match o {
                            Some(m) => Outcome::Hit(m),
                            None => Outcome::None,
                        };
                    }
                }
                if is_ident(owner) && owner.starts_with(|c: char| c.is_ascii_uppercase()) && owner != "Self" {
                    if let Some(o) = self.generic_path(owner, method, r) {
                        return match o {
                            Some(m) => Outcome::Hit(m),
                            None => Outcome::None,
                        };
                    }
                }
            }
            if is_ident(name) {
                if let Some(h) = self.as_mut_chain_call(r) {
                    return h.into_outcome();
                }
                let w = self.written_receiver_call(r, None);
                if !matches!(w, Outcome::Pass) {
                    return w;
                }
            }
        }
        Outcome::Pass
    }

    /// Upstream's `gateRustScope`: a bare name reaches only what is in scope, a
    /// path call through an outside type only the project's impls for it, and
    /// a call reaches a project impl on a std type only as Rust picks it.
    pub fn gate(&self, m: Option<Match>, r: &Ref<'_>) -> Option<Match> {
        let m = m?;
        let bare = is_ident(r.name);
        if !bare && r.kind != ReferenceKind::Calls {
            return Some(m);
        }
        if bare && !self.name_in_scope(m.target, r) {
            return None;
        }
        if r.kind != ReferenceKind::Calls {
            return Some(m);
        }
        if !bare && !self.path_call_reachable(m.target, r) {
            return None;
        }
        self.foreign_impl_callable(m.target, r).then_some(m)
    }

    /// A Rust `use` path: the item its path walks to.
    fn resolve_import(&self, r: &Ref<'_>, generic: &mut dyn FnMut(&str) -> Option<Match>) -> Option<Match> {
        let segs: Vec<String> = r.name.split("::").map(|s| s.trim().to_string()).collect();
        if segs.len() < 2 {
            // `crate`, `self`, a crate's name alone: the module, which no edge needs.
            return None;
        }
        let at = self.ref_offset(r).unwrap_or(0);
        let from = self.module_at(r.file, at);
        match self.use_path_items(&segs, &from, &mut HashSet::new()) {
            Some(items) => {
                let items: Vec<SymId> = items.into_iter().filter(|&i| !is_associated_item(self.syms.get(i))).collect();
                let best = self.prefer_file(&items, r.file).into_iter().next()?;
                Some(Match {
                    target: best,
                    strategy: "import-binding",
                    confidence: 0.95,
                })
            }
            None => {
                // A module the path names, or one the project can't walk to.
                if let Walk::Module(m) = self.walk_modules(&segs, &from, 0) {
                    if m.block.is_none() {
                        if let Some(f) = self.syms.file_node(&m.file) {
                            return Some(Match {
                                target: f,
                                strategy: "import-module",
                                confidence: 0.95,
                            });
                        }
                    }
                    return None;
                }
                if matches!(self.walk_modules(&segs[..segs.len() - 1], &from, 0), Walk::Outside) {
                    return None;
                }
                let m = generic(r.name)?;
                let t = self.syms.get(m.target);
                (!is_associated_item(t) && t.kind != NodeKind::File).then_some(m)
            }
        }
    }

    /// A bare Rust name: the candidate in scope where it is written.
    fn bare(&self, r: &Ref<'_>) -> Option<Match> {
        let line = self.line(r.file, r.line).unwrap_or_default();
        let shape = if r.kind == ReferenceKind::Calls { call_shape(&line, r.column as usize, r.name) } else { Shape::Unknown };
        let from = self.syms.get(r.from);
        let candidates: Vec<SymId> = self
            .rust_named(r.name)
            .filter(|&i| {
                let s = self.syms.get(i);
                kind_accepts(r.kind, s.kind)
                    && i != r.from
                    && self.syms.visible_from(i, r.from)
                    && !matches!(s.kind, NodeKind::Field | NodeKind::Property)
                    && shape.allows(s, self)
                    && self.visibility_allows(i, r)
                    && self.name_in_scope(i, r)
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        // A function's local item first, then the file's own, what its `use`s name, then the nearest.
        let uses = self.use_items(r.name, r.file);
        let globs = self.glob_items(r.name, r.file);
        let rank = |i: SymId| {
            let s = self.syms.get(i);
            let local = s.local && s.file == from.file;
            (
                local,
                s.file == r.file,
                uses.as_ref().is_some_and(|u| u.contains(&i)),
                globs.as_ref().is_some_and(|g| g.contains(&i)),
                kind_pref(r.kind, s.kind),
                types::proximity(r.file, &s.file),
                std::cmp::Reverse(s.start_line),
            )
        };
        let best = candidates.iter().copied().max_by_key(|&i| rank(i))?;
        let s = self.syms.get(best);
        let strategy = if s.file == r.file { "same-file" } else if uses.as_ref().is_some_and(|u| u.contains(&best)) { "import" } else { "exact-match" };
        Some(Match {
            target: best,
            strategy,
            confidence: 0.9,
        })
    }

    /// Rust's visibility rule for a name reached across files (upstream's `isVisibleAcrossFiles`).
    fn visibility_allows(&self, cand: SymId, r: &Ref<'_>) -> bool {
        let c = self.syms.get(cand);
        if c.file == r.file || c.visibility != Some(Visibility::Private) {
            return true;
        }
        if self.is_trait_impl_method(cand) {
            return true;
        }
        if c.kind == NodeKind::Method && self.typed.borrow().contains(&(r.from, r.line, r.column)) {
            if self.trait_of(cand).is_some_and(|t| self.syms.get(t).visibility != Some(Visibility::Private)) {
                return true;
            }
        }
        r.file.starts_with(&format!("{}/", uses::module_dir(&c.file)))
    }

    /// A Rust path — `crate::util::take`, `util::take`, `Type::assoc`, `mode::Mode` — by what it walks to.
    fn path(&self, r: &Ref<'_>) -> Outcome {
        let segs: Vec<String> = r.name.split("::").map(|s| s.trim().to_string()).collect();
        if segs.len() < 2 || !segs.iter().all(|s| is_ident(s)) {
            return Outcome::Pass;
        }
        let last = segs.last().unwrap().clone();
        let head = &segs[..segs.len() - 1];
        let at = self.ref_offset(r).unwrap_or(0);
        // A module path: every segment before the item a module (lower-case, or `crate`/`self`/`super`).
        if head.iter().all(|s| s.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')) {
            let mut path: Vec<String> = head.to_vec();
            // The path's head as a `use` binds it.
            if let Some(bound) = self.use_path(&head[0], r.file, Some(at)).filter(|b| !b.is_empty()) {
                if !matches!(head[0].as_str(), "crate" | "self" | "super") {
                    path = bound;
                    path.extend(head[1..].iter().cloned());
                }
            }
            let from = self.module_at(r.file, at);
            match self.walk_modules(&path, &from, 0) {
                Walk::Outside => return Outcome::None,
                Walk::Unknown => {
                    if head.len() == 1 && self.graph().dependencies.contains(&head[0]) && !self.graph().project.contains(&head[0]) {
                        return Outcome::None;
                    }
                    return Outcome::Pass;
                }
                Walk::Module(m) => {
                    let items = self.module_items(&m, &last, &mut HashSet::new(), !self.module_within(&from, &m));
                    let Some(items) = items else { return Outcome::Pass };
                    let ok: Vec<SymId> = items
                        .into_iter()
                        .filter(|&i| kind_accepts(r.kind, self.syms.get(i).kind) && !is_associated_item(self.syms.get(i)))
                        .collect();
                    return match self.prefer_file(&ok, r.file).first() {
                        Some(&t) => Outcome::Hit(Match {
                            target: t,
                            strategy: "module-path",
                            confidence: 0.9,
                        }),
                        None => Outcome::None,
                    };
                }
            }
        }
        // `Type::member` / `module::Type::member`: the type the path names, then its member.
        let owner = head.last().unwrap();
        if !owner.starts_with(|c: char| c.is_ascii_uppercase()) || owner == "Self" {
            return Outcome::Pass;
        }
        let lookup = self.type_lookup(owner, &head[..head.len() - 1], r.file, Some(at));
        if lookup.outside {
            // Only the project's own impls for that type (or of that trait).
            let impls: Vec<SymId> = self
                .methods_named(&lookup.name, &last)
                .into_iter()
                .filter(|&m| self.path_call_reachable(m, r) && self.foreign_impl_callable(m, r))
                .collect();
            return match impls.as_slice() {
                [one] => Outcome::Hit(qualified(*one, 0.85)),
                [] => Outcome::None,
                many => {
                    let traits: HashSet<String> = many.iter().map(|&m| self.trait_impl_header(m).map(|h| h.trait_).unwrap_or_default()).collect();
                    if traits.len() == 1 {
                        Outcome::Hit(qualified(self.prefer_file(many, r.file)[0], 0.8))
                    } else {
                        Outcome::None
                    }
                }
            };
        }
        if lookup.nodes.is_empty() {
            return Outcome::Pass;
        }
        let mut found = Vec::new();
        for &t in &lookup.nodes {
            let ts = self.syms.get(t);
            let q = format!("{}::{last}", ts.qualified_name);
            for i in self.rust_named(&last) {
                let s = self.syms.get(i);
                if s.qualified_name == q && kind_accepts(r.kind, s.kind) {
                    // A variant is its enum's file's; a method may be in another file's impl.
                    if s.kind == NodeKind::EnumMember && s.file != ts.file {
                        continue;
                    }
                    found.push(i);
                }
            }
        }
        if found.is_empty() {
            // A trait's methods through its path (`Trait::method(x)`), or an alias's target type.
            return Outcome::Pass;
        }
        let inherent: Vec<SymId> = found.iter().copied().filter(|&m| !self.is_trait_impl_method(m)).collect();
        let pool = if inherent.is_empty() { found } else { inherent };
        let type_files: HashSet<String> = lookup.nodes.iter().map(|&n| self.syms.get(n).file.clone()).collect();
        match self.pick_method(&pool, &type_files, r, 0.9) {
            Hit::Found(m) => Outcome::Hit(qualified(m.target, if self.syms.get(m.target).file == r.file { 0.9 } else { 0.8 })),
            _ => Outcome::None,
        }
    }

    /// Whether a Rust call through a type's path can mean `target`.
    pub fn path_call_reachable(&self, target: SymId, r: &Ref<'_>) -> bool {
        let t = self.syms.get(target);
        if t.language != Language::Rust {
            return true;
        }
        let segments: Vec<&str> = r.name.split("::").collect();
        if segments.len() < 2 || !segments.iter().all(|s| is_ident(s)) {
            return true;
        }
        let ty = segments[segments.len() - 2];
        if !ty.starts_with(|c: char| c.is_ascii_uppercase()) || ty == "Self" {
            return true;
        }
        let path: Vec<String> = segments[..segments.len() - 2].iter().map(|s| s.to_string()).collect();
        let Some(outside) = self.outside_path_type(ty, &path, r) else { return true };
        let imp = if t.kind == NodeKind::Method { self.trait_impl_header(target) } else { None };
        let Some(imp) = imp else { return false };
        if imp.trait_ == outside {
            return true;
        }
        if imp.self_ != outside {
            return false;
        }
        let written = impls::strip_amp(&imp.self_text);
        let head = types::path_head(written);
        let (segs, name) = head.map(|h| (h.0, h.1)).unwrap_or((Vec::new(), imp.self_.clone()));
        let lookup = self.type_lookup(&name, &segs, &t.file, Some(self.node_offset(target)));
        lookup.outside || !lookup.nodes.iter().any(|&n| OWNER_KINDS.contains(&self.syms.get(n).kind))
    }

    fn outside_path_type(&self, ty: &str, path: &[String], r: &Ref<'_>) -> Option<String> {
        let at = self.ref_offset(r);
        let (root, real) = if path.is_empty() {
            let bound = self.use_path(ty, r.file, at).filter(|b| b.len() >= 2)?;
            (bound[0].clone(), bound.last().unwrap().clone())
        } else {
            let root = self.use_path(&path[0], r.file, at).and_then(|p| p.first().cloned()).unwrap_or_else(|| path[0].clone());
            (root, ty.to_string())
        };
        self.graph().is_outside_crate(&root, r.file).then_some(real)
    }

    /// Whether a Rust call can reach this method of a project `impl` on a std type, as Rust picks one.
    pub fn foreign_impl_callable(&self, target: SymId, r: &Ref<'_>) -> bool {
        let t = self.syms.get(target);
        if t.language != Language::Rust || t.kind != NodeKind::Method {
            return true;
        }
        let owner = t.owner().and_then(|o| o.rsplit("::").next()).unwrap_or("");
        if !STD_TYPES.contains(&owner) || self.is_project_named(owner, DECLARED_TYPE_KINDS) {
            return true;
        }
        let Some(imp) = self.trait_impl_header(target) else { return true };
        if imp.self_ != owner || imp.trait_.is_empty() {
            return true;
        }
        if std_inherent(owner, &t.name) {
            return false;
        }
        if self.is_project_named(&imp.trait_, &[NodeKind::Trait]) {
            return self.trait_in_scope(&imp.trait_, r, target);
        }
        match imp.trait_args.iter().find(|a| self.is_project_named(a, DECLARED_TYPE_KINDS)) {
            None => true,
            Some(local) => self.first_arg_is(local, r),
        }
    }

    // ----------------------------------------------------------- self calls

    /// `self.method()` — the method on the type the call sits inside.
    fn self_call(&self, method: &str, r: &Ref<'_>) -> Option<Match> {
        let caller = self.syms.get(r.from);
        let (owner, _) = caller.qualified_name.rsplit_once("::")?;
        let qn = format!("{owner}::{method}");
        let mut owned: Vec<SymId> = self.by_qualified(&qn).into_iter().filter(|&i| self.syms.get(i).kind == NodeKind::Method).collect();
        let owners: Vec<SymId> = self
            .by_qualified(owner)
            .into_iter()
            .filter(|&i| matches!(self.syms.get(i).kind, NodeKind::Struct | NodeKind::Enum | NodeKind::Union | NodeKind::Trait | NodeKind::Class))
            .collect();
        if owners.len() > 1 {
            if owners.iter().filter(|&&o| self.syms.get(o).file == caller.file).count() != 1 {
                return None;
            }
            owned.retain(|&i| self.syms.get(i).file == caller.file);
        }
        // `(self.into_route)(state)` calls what a field holds, no method.
        if let Some(l) = self.line(r.file, r.line) {
            let at = l.get(r.column as usize..).unwrap_or("");
            if let Some(rest) = at.strip_prefix('(') {
                let inner = rest.trim_start();
                if let Some(x) = inner.strip_prefix("self") {
                    let x = x.trim_start();
                    if let Some(y) = x.strip_prefix('.') {
                        let y = y.trim_start();
                        if y.starts_with(method) && y[method.len()..].trim_start().starts_with(')') {
                            return None;
                        }
                    }
                }
            }
        }
        // In a trait impl, `fn wake(self) { self.wake() }` hands the call on to the type's own `wake`.
        if caller.name == method && self.is_trait_impl_method(r.from) {
            let inherent: Vec<SymId> = owned.iter().copied().filter(|&n| !self.is_trait_impl_method(n)).collect();
            if !inherent.is_empty() {
                owned = inherent;
            } else {
                let st = self.self_type_at(r);
                if st.as_ref().is_none_or(|s| self.is_outside_self(s, r.file)) {
                    return None;
                }
            }
        }
        if let Some(wrapper) = self.self_wrapper(r.from) {
            if !self.is_project_named(wrapper, DECLARED_TYPE_KINDS) {
                let inner = owner.rsplit("::").next().unwrap_or(owner);
                match self.through_wrapper(wrapper, method, &owned, Some(inner), r) {
                    Through::Target(t) => return t.map(|t| qualified(t, 0.9)),
                    Through::Candidates(c) => owned = c,
                }
            }
        }
        let target = self.pick_own_method(&owned, r)?;
        Some(qualified(target, 0.9))
    }

    /// Which of a Rust type's methods of one name a call on the type reaches, as Rust picks.
    fn pick_own_method(&self, owned: &[SymId], r: &Ref<'_>) -> Option<SymId> {
        if owned.len() <= 1 {
            return owned.first().copied();
        }
        let inherent: Vec<SymId> = owned.iter().copied().filter(|&n| !self.is_trait_impl_method(n)).collect();
        if !inherent.is_empty() {
            return (inherent.len() == 1).then(|| inherent[0]);
        }
        let header = self.enclosing_impl_line(r);
        let block: Vec<SymId> = owned
            .iter()
            .copied()
            .filter(|&n| {
                let s = self.syms.get(n);
                s.file == r.file && self.impl_line_at(&s.file, s.start_line) == header
            })
            .collect();
        (block.len() == 1).then(|| block[0])
    }

    /// Rust's method lookup on a value reached through a std wrapper.
    fn through_wrapper(&self, wrapper: &str, name: &str, own: &[SymId], inner: Option<&str>, r: &Ref<'_>) -> Through {
        let through: Vec<SymId> = own.iter().copied().filter(|&n| self.self_wrapper(n) == Some(wrapper)).collect();
        if !through.is_empty() {
            return Through::Candidates(through);
        }
        if wrapper_own_method(wrapper, name) {
            return Through::Target(None);
        }
        let on_wrapper: Vec<SymId> = self
            .methods_named(wrapper, name)
            .into_iter()
            .filter(|&n| self.self_wrapper(n).is_none() && self.impl_for(n, inner))
            .collect();
        if !on_wrapper.is_empty() {
            return Through::Target(Some(self.prefer_file(&on_wrapper, r.file)[0]));
        }
        Through::Candidates(own.to_vec())
    }

    /// A call on a value declared `Pin<…>`, on the type it pins.
    fn pinned_call_on(&self, pointee: Option<String>, name: &str, r: &Ref<'_>) -> Option<Match> {
        let inner = pointee.filter(|p| !self.is_type_param(p, r));
        let own: Vec<SymId> = match &inner {
            Some(i) => self.methods_named(i, name),
            None => Vec::new(),
        };
        let target = match self.through_wrapper("Pin", name, &own, inner.as_deref(), r) {
            Through::Target(t) => t,
            Through::Candidates(c) => {
                let inherent: Vec<SymId> = c.iter().copied().filter(|&n| !self.is_trait_impl_method(n)).collect();
                let pool = if inherent.is_empty() { c } else { inherent };
                self.prefer_file(&pool, r.file).first().copied()
            }
        };
        target.map(|t| instance(t, 0.9))
    }

    /// What a `Pin<…>` type pins — `Pin<&mut R>` → `R`, `Pin<&mut Self>` → the impl's type.
    fn pinned_pointee(&self, ty: &str, r: &Ref<'_>) -> Option<String> {
        let t = impls::strip_amp(ty);
        let t = ["std::pin::", "core::pin::"].iter().find_map(|p| t.strip_prefix(p)).unwrap_or(t);
        let t = t.strip_prefix("Pin")?.trim_start().strip_prefix('<')?.trim_start();
        let mut t = impls::strip_amp(t);
        for p in ["std::boxed::Box<", "alloc::boxed::Box<", "Box<"] {
            if let Some(r) = t.strip_prefix(p) {
                t = r.trim_start();
                break;
            }
        }
        let t = t.strip_prefix("dyn ").or_else(|| t.strip_prefix("impl ")).unwrap_or(t).trim_start();
        let path: String = t.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':').collect();
        let name = path.rsplit("::").next()?.trim().to_string();
        if name.is_empty() {
            return None;
        }
        if name == "Self" {
            return self.self_type_at(r).map(|s| s.ty);
        }
        Some(name)
    }

    /// Whether `name` is a type parameter the `fn` or the `impl` around `r` declares.
    fn is_type_param(&self, name: &str, r: &Ref<'_>) -> bool {
        let Some(sc) = self.scopes(r.file) else { return false };
        let at = sc.offset(r.line, r.column);
        let declares = |generics: &str| {
            split_generic_names(generics).iter().any(|g| g == name)
        };
        sc.fn_at(at).is_some_and(|f| declares(&f.generics)) || sc.impl_at(at).is_some_and(|b| b.kind == scopes::BlockKind::Impl && declares(&b.generics))
    }

    /// The type a Rust local or parameter is written with, as its binding in scope writes it.
    fn declared_type_text(&self, name: &str, r: &Ref<'_>) -> Option<String> {
        let sc = self.scopes(r.file)?;
        let (b, ambiguous) = sc.binding_at(name, sc.offset(r.line, r.column))?;
        if ambiguous {
            return None;
        }
        b.ty.as_deref().map(type_text_at)
    }

    /// A call on what `.as_mut()` or `.as_ref()` hands back.
    fn as_mut_chain_call(&self, r: &Ref<'_>) -> Option<Hit3> {
        let line = self.line(r.file, r.line)?;
        if !line.contains(".as_mut()") && !line.contains(".as_ref()") && !collapse(&line).contains(".as_mut()") && !collapse(&line).contains(".as_ref()") {
            return None;
        }
        let text = collapse(line.get(r.column as usize..)?);
        // `base[.field].as_mut()[?].name(` or `::<`
        let (head, rest) = text.split_once(".as_mut()").or_else(|| text.split_once(".as_ref()"))?;
        let (tried, rest) = match rest.strip_prefix('?') {
            Some(r2) => (true, r2),
            None => (false, rest),
        };
        let rest = rest.strip_prefix('.')?;
        if !rest.starts_with(r.name) || !(rest[r.name.len()..].starts_with('(') || rest[r.name.len()..].starts_with("::<")) {
            return None;
        }
        let (base, field) = match head.split_once('.') {
            Some((b, f)) => (b, Some(f)),
            None => (head, None),
        };
        if !is_ident(base) || field.is_some_and(|f| !is_ident(f)) {
            return None;
        }
        let caller = self.syms.get(r.from);
        let Some(field) = field else {
            if base == "self" {
                return (self.self_wrapper(r.from) == Some("Pin") && !tried).then(|| Hit3::from(self.self_call(r.name, r)));
            }
            let declared = self.declared_type_text(base, r)?;
            return self.lent_call(&declared, r.name, r, tried);
        };
        let projected = base != "self";
        if projected && !self.projects_self(base, r) {
            return None;
        }
        let owner = caller.owner().and_then(|o| o.rsplit("::").next())?;
        let (ty, pinned) = self.field_declaration(owner, field, r)?;
        let ty = if projected && pinned { format!("Pin<&mut {ty}>") } else { ty };
        self.lent_call(&ty, r.name, r, tried)
    }

    /// What `name` called on `x.as_mut()` reaches for an `x` of the type `ty` as written.
    fn lent_call(&self, ty: &str, name: &str, r: &Ref<'_>, tried: bool) -> Option<Hit3> {
        let (outer, args, text) = type_head(ty)?;
        if outer == "Pin" && !self.is_project_named("Pin", DECLARED_TYPE_KINDS) {
            return if tried { None } else { Some(Hit3::from(self.pinned_call_on(self.pinned_pointee(&text, r), name, r))) };
        }
        if self.is_project_named(&outer, DECLARED_TYPE_KINDS) {
            return None;
        }
        if ["Box", "NonNull", "Rc", "Arc", "Cow"].contains(&outer.as_str()) {
            return if tried { None } else { self.call_on_type(&type_text_at(&args), name, r) };
        }
        if outer == "Option" || outer == "Result" {
            return if tried { self.call_on_type(&type_text_at(&args), name, r) } else { Some(Hit3::Nothing) };
        }
        if STD_TYPES.contains(&outer.as_str()) || self.is_type_param(&outer, r) {
            return Some(Hit3::Nothing);
        }
        None
    }

    /// What `name` called on a value of the Rust type `ty` as written reaches.
    fn call_on_type(&self, ty: &str, name: &str, r: &Ref<'_>) -> Option<Hit3> {
        let (outer, args, text) = type_head(ty)?;
        if outer == "Pin" && !self.is_project_named("Pin", DECLARED_TYPE_KINDS) {
            return Some(Hit3::from(self.pinned_call_on(self.pinned_pointee(&text, r), name, r)));
        }
        let outer = if outer == "Self" { self.self_type_at(r)?.ty } else { outer };
        if self.is_type_param(&outer, r) {
            return Some(Hit3::Nothing);
        }
        if ["Box", "NonNull", "Rc", "Arc", "Cow"].contains(&outer.as_str()) && !self.is_project_named(&outer, DECLARED_TYPE_KINDS) {
            return if wrapper_own_method(&outer, name) { Some(Hit3::Nothing) } else { self.call_on_type(&type_text_at(&args), name, r) };
        }
        let st = impls::SelfType {
            ty: outer.clone(),
            root: None,
            trait_: None,
            is_trait: false,
        };
        if self.is_outside_self(&st, r.file) {
            return Some(Hit3::Nothing);
        }
        if !self.is_project_named(&outer, DECLARED_TYPE_KINDS) {
            return None;
        }
        Some(Hit3::from(self.method_on_type(&outer, name, r)))
    }

    /// Upstream's `resolveMethodOnType`, for a Rust type by name: its method of that name, the nearest.
    fn method_on_type(&self, ty: &str, name: &str, r: &Ref<'_>) -> Option<Match> {
        let methods = self.methods_named(ty, name);
        if methods.is_empty() {
            return None;
        }
        let inherent: Vec<SymId> = methods.iter().copied().filter(|&m| !self.is_trait_impl_method(m)).collect();
        let pool = if inherent.is_empty() { methods } else { inherent };
        match self.pick_method(&pool, &HashSet::new(), r, 0.85) {
            Hit::Found(m) => Some(m),
            _ => None,
        }
    }

    /// Whether a Rust local is bound by pin-project's `let me = self.project()`.
    fn projects_self(&self, name: &str, r: &Ref<'_>) -> bool {
        let Some(sc) = self.scopes(r.file) else { return false };
        let Some((b, _)) = sc.binding_at(name, sc.offset(r.line, r.column)) else { return false };
        let init = collapse(b.init.as_deref().unwrap_or(""));
        init == "self.project()" || init == "self.as_mut().project()"
    }

    /// How a field of the Rust type `owner` is declared: its type as written, and whether pin-project pins it.
    fn field_declaration(&self, owner: &str, field: &str, r: &Ref<'_>) -> Option<(String, bool)> {
        let decls: Vec<SymId> = self.rust_named(owner).filter(|&n| matches!(self.syms.get(n).kind, NodeKind::Struct | NodeKind::Union)).collect();
        let mut spans: Vec<Vec<String>> = Vec::new();
        for d in self.prefer_file(&decls, r.file) {
            let s = self.syms.get(d);
            if let Some(src) = self.source(&s.file) {
                spans.push(src.lines().skip(s.start_line as usize - 1).take((s.end_line - s.start_line + 1) as usize).map(str::to_string).collect());
            }
        }
        if spans.is_empty() {
            let src = self.source(r.file)?;
            let lines: Vec<&str> = src.lines().collect();
            let at = lines.iter().position(|l| {
                let l = l.split("//").next().unwrap_or("");
                uses::find_word(l, "struct").is_some_and(|i| l[i + 6..].trim_start().starts_with(owner) && !l[i + 6..].trim_start()[owner.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_'))
            })?;
            let mut depth = 0;
            let mut end = at;
            let mut opened = false;
            while end < lines.len() {
                for ch in lines[end].split("//").next().unwrap_or("").chars() {
                    if ch == '{' {
                        depth += 1;
                        opened = true;
                    } else if ch == '}' {
                        depth -= 1;
                    }
                }
                if opened && depth <= 0 {
                    break;
                }
                end += 1;
            }
            spans.push(lines[at..=end.min(lines.len() - 1)].iter().map(|s| s.to_string()).collect());
        }
        for span in spans {
            for (i, raw) in span.iter().enumerate() {
                let line = raw.split("//").next().unwrap_or("");
                let Some(at) = field_decl_at(line, field) else { continue };
                let pinned = line[..at].contains("#[pin]") || (i > 0 && span[i - 1].trim() == "#[pin]");
                let after = line[at + field.len()..].trim_start().strip_prefix(':')?;
                return Some((type_text_at(after), pinned));
            }
        }
        None
    }

    /// A call through a field of the enclosing type, read as a written type is.
    fn typed_self_field_call(&self, field: &str, method: &str, r: &Ref<'_>) -> Option<Hit> {
        let sc = self.scopes(r.file)?;
        let at = sc.offset(r.line, r.column);
        let written = self.field_value_pub("self", field, at, &sc, r)?;
        let h = self.typed_method_call(&self.read_type(&written, r), method, r, 0);
        match h {
            Hit::Unread => None,
            Hit::Found(_) => {
                self.typed.borrow_mut().insert((r.from, r.line, r.column));
                Some(h)
            }
            Hit::Nothing => Some(h),
        }
    }

    /// The plain-name reading of `self.<field>.<method>()`: the field's simple type name.
    fn self_field_call_fallback(&self, field: &str, method: &str, r: &Ref<'_>) -> Option<Match> {
        let caller = self.syms.get(r.from);
        let owner = caller.owner()?.rsplit("::").next()?;
        let decls: Vec<SymId> = self.rust_named(owner).filter(|&n| matches!(self.syms.get(n).kind, NodeKind::Struct | NodeKind::Union | NodeKind::Class)).collect();
        for d in self.prefer_file(&decls, r.file) {
            let s = self.syms.get(d);
            let src = self.source(&s.file)?;
            for raw in src.lines().skip(s.start_line as usize - 1).take((s.end_line - s.start_line + 1) as usize) {
                let line = raw.split("//").next().unwrap_or("");
                let Some(at) = field_decl_at(line, field) else { continue };
                let after = line[at + field.len()..].trim_start().strip_prefix(':')?;
                let ty_text: String = after.chars().take_while(|c| !matches!(c, ',' | '{' | '}')).collect();
                let ty = field_type_name(&ty_text)?;
                return self.method_on_type(&ty, method, r);
            }
        }
        None
    }

    /// A call on a local or a parameter, as the type its binding in scope declares makes it.
    fn typed_receiver_call(&self, receiver: &str, method: &str, r: &Ref<'_>) -> Option<Hit> {
        let sc = self.scopes(r.file)?;
        let offset = sc.offset(r.line, r.column);
        let (b, ambiguous) = sc.binding_at(receiver, offset)?;
        if ambiguous {
            return Some(Hit::Unread);
        }
        let Some(written) = self.binding_type(b, &sc, r, 0) else { return Some(Hit::Unread) };
        let h = self.typed_method_call(&self.read_type(&written, r), method, r, 0);
        if let Hit::Found(_) = h {
            self.typed.borrow_mut().insert((r.from, r.line, r.column));
        }
        Some(h)
    }

    /// A call on a field chain, a struct literal or a receiver chain kept by extraction, as written.
    fn written_receiver_call(&self, r: &Ref<'_>, chain: Option<&[String]>) -> Outcome {
        let Some(sc) = self.scopes(r.file) else { return Outcome::Pass };
        let start = sc.offset(r.line, r.column);
        let line_end = sc.line_starts.get(r.line as usize).copied().unwrap_or(sc.code.len());
        if start >= line_end {
            return Outcome::Pass;
        }
        let receiver = match chain {
            Some(c) => chained_receiver(&sc, start, c, r.name),
            None => written_call_receiver(&sc, start, r.name),
        };
        let Some(receiver) = receiver else { return Outcome::Pass };
        let Some(written) = self.value_type(&receiver, start, &sc, r, 0) else { return Outcome::Pass };
        let read = self.read_type(&written, r);
        if let Read::Named { name, path, written: w, wrappers, .. } = &read {
            if !wrappers.iter().any(|x| wrapper_own_method(x, r.name)) {
                let lookup = self.type_lookup(name, path, &w.file, Some(w.at));
                if lookup.undeclared && self.methods_named(&lookup.name, r.name).is_empty() {
                    return Outcome::Pass;
                }
            }
        }
        let mut h = self.typed_method_call(&read, r.name, r, 0);
        if let (Hit::Nothing, Read::Named { .. }) = (&h, &read) {
            if let Some(m) = self.derived_trait_method(&read, r.name, r) {
                h = Hit::Found(m);
            }
        }
        if let Hit::Found(_) = h {
            self.typed.borrow_mut().insert((r.from, r.line, r.column));
        }
        h.into()
    }

    /// The method a project trait that a type derives provides.
    fn derived_trait_method(&self, read: &Read, method: &str, r: &Ref<'_>) -> Option<Match> {
        let Read::Named { name, path, written, .. } = read else { return None };
        let lookup = self.type_lookup(name, path, &written.file, Some(written.at));
        let types: Vec<SymId> = if lookup.outside { Vec::new() } else { lookup.nodes.iter().copied().filter(|&n| OWNER_KINDS.contains(&self.syms.get(n).kind)).collect() };
        if types.len() != 1 {
            return None;
        }
        let mut found = Vec::new();
        for d in self.derived_names(types[0]) {
            let traits: Vec<SymId> = self.rust_named(&d).filter(|&n| self.syms.get(n).kind == NodeKind::Trait).collect();
            for m in self.trait_methods(&traits, method, 0) {
                if let Some(t) = self.trait_of(m) {
                    if self.has_body(m) && self.trait_in_scope(&self.syms.get(t).name, r, m) {
                        found.push(m);
                    }
                }
            }
        }
        let owners: HashSet<Option<SymId>> = found.iter().map(|&m| self.trait_of(m)).collect();
        (owners.len() == 1).then(|| instance(found[0], 0.85))
    }

    /// The names a Rust type's `#[derive(…)]` attributes write.
    fn derived_names(&self, decl: SymId) -> Vec<String> {
        let s = self.syms.get(decl);
        let Some(src) = self.source(&s.file) else { return Vec::new() };
        let lines: Vec<&str> = src.lines().collect();
        let mut names = Vec::new();
        let mut i = s.start_line as usize;
        // The node may start at its attributes; read up from the keyword's line too.
        let kw = (i.saturating_sub(1)..lines.len().min(i + 8)).find(|&k| {
            let l = lines[k];
            ["struct", "enum", "union"].iter().any(|w| uses::find_word(l, w).is_some_and(|p| l[p + w.len()..].trim_start().starts_with(&s.name)))
        });
        let Some(kw) = kw else { return names };
        i = kw + 1;
        let mut k = i;
        while k > 0 {
            k -= 1;
            let l = lines[k].trim();
            let is_kw_line = k == kw;
            if !is_kw_line && !l.starts_with("#[") && !l.ends_with(")]") && !l.is_empty() {
                break;
            }
            let mut rest = lines[k];
            while let Some(p) = uses::find_word(rest, "derive") {
                let after = rest[p + 6..].trim_start();
                let Some(inner) = after.strip_prefix('(') else { break };
                let end = inner.find(')').unwrap_or(inner.len());
                for part in inner[..end].split(',') {
                    let n = part.trim().rsplit("::").next().unwrap_or("").trim();
                    if is_ident(n) {
                        names.push(n.to_string());
                    }
                }
                rest = &inner[end..];
            }
        }
        names
    }

    // ------------------------------------------------------------- `Self::` and generic paths

    /// A Rust `Self::x(…)` call is `x` of the type `Self` is where it is written.
    fn self_path(&self, name: &str, r: &Ref<'_>) -> Option<Option<Match>> {
        let st = self.self_type_at(r)?;
        let outside = !st.is_trait && self.is_outside_self(&st, r.file);
        let in_trait = |n: SymId, t: &str| {
            let s = self.syms.get(n);
            self.syms.in_file(&s.file).iter().any(|&i| {
                let x = self.syms.get(i);
                x.kind == NodeKind::Trait && x.name == t && x.start_line <= s.start_line && x.end_line >= s.start_line
            })
        };
        let pick = |owner: &str, trait_: bool| -> Option<SymId> {
            let q = format!("{owner}::{name}");
            let qs = format!("::{q}");
            let all: Vec<SymId> = self
                .rust_named(name)
                .filter(|&n| {
                    let s = self.syms.get(n);
                    matches!(s.kind, NodeKind::Method | NodeKind::Function | NodeKind::EnumMember)
                        && (s.qualified_name == q || s.qualified_name.ends_with(&qs))
                        && (!trait_ || in_trait(n, owner))
                        && (trait_ || !outside || s.file == r.file)
                        && (st.trait_.is_none() || n != r.from)
                })
                .collect();
            let inherent: Vec<SymId> = all.iter().copied().filter(|&n| !self.is_trait_impl_method(n)).collect();
            let candidates = if !inherent.is_empty() {
                inherent
            } else if all.len() == 1 {
                all
            } else {
                Vec::new()
            };
            if candidates.is_empty() {
                return None;
            }
            if let Some(&own) = candidates.iter().find(|&&n| self.syms.get(n).file == r.file) {
                return Some(own);
            }
            candidates.iter().copied().max_by_key(|&c| types::proximity(r.file, &self.syms.get(c).file))
        };
        let delegates = self.syms.get(r.from).name == name;
        let target = pick(&st.ty, st.is_trait).or_else(|| match &st.trait_ {
            Some(t) if !outside && !delegates => pick(t, true),
            _ => None,
        });
        Some(target.map(|t| qualified(t, if self.syms.get(t).file == r.file { 0.9 } else { 0.8 })))
    }

    /// A call through a generic type parameter: the method its bounds' traits declare.
    fn generic_path(&self, name: &str, method: &str, r: &Ref<'_>) -> Option<Option<Match>> {
        let sc = self.scopes(r.file)?;
        let written = Written::new(name, r.file, sc.offset(r.line, r.column));
        let bounds = self.generic_bounds(name, &written)?;
        let traits: Vec<String> = bounds
            .into_iter()
            .filter(|b| !b.trim().starts_with(['\'', '?']) && !MARKER_TRAITS.contains(&types::path_head(b).map(|h| h.1).unwrap_or_default().as_str()))
            .collect();
        if traits.is_empty() {
            return Some(None);
        }
        let h = self.traits_method_call(&traits, &written, method, r);
        Some(match h {
            Hit::Found(m) => {
                self.typed.borrow_mut().insert((r.from, r.line, r.column));
                Some(m)
            }
            _ => None,
        })
    }
}

/// Through a wrapper: the candidates left, or the target itself (none for std's own).
enum Through {
    Candidates(Vec<SymId>),
    Target(Option<SymId>),
}

/// A strategy's three answers, for the `as_mut` chain reading.
enum Hit3 {
    Found(Match),
    Nothing,
}

impl From<Option<Match>> for Hit3 {
    fn from(m: Option<Match>) -> Self {
        match m {
            Some(m) => Hit3::Found(m),
            None => Hit3::Nothing,
        }
    }
}

impl Hit3 {
    fn into_outcome(self) -> Outcome {
        match self {
            Hit3::Found(m) => Outcome::Hit(m),
            Hit3::Nothing => Outcome::None,
        }
    }
}

/// Preference among acceptable targets of one name (higher first).
fn kind_pref(kind: ReferenceKind, target: NodeKind) -> u8 {
    match kind {
        ReferenceKind::Calls => match target {
            NodeKind::Function | NodeKind::Method => 3,
            NodeKind::Struct | NodeKind::Enum | NodeKind::EnumMember => 2,
            _ => 1,
        },
        _ => {
            if target.is_type() || target == NodeKind::Enum {
                3
            } else {
                1
            }
        }
    }
}

/// How a bare Rust call is written: after `::`, after a `.` on a receiver, or bare.
#[derive(Debug, Clone)]
enum Shape {
    Unknown,
    Path,
    Bare,
    Chained(String),
}

impl Shape {
    fn allows(&self, s: &Sym, ctx: &Ctx<'_>) -> bool {
        let _ = ctx;
        match self {
            Shape::Unknown | Shape::Path => true,
            Shape::Bare => s.kind != NodeKind::Method,
            Shape::Chained(recv) => {
                if recv == "()" {
                    return false;
                }
                if s.kind != NodeKind::Method {
                    return s.kind != NodeKind::Function;
                }
                !STD_METHODS.contains(&s.name.as_str()) || recv == "self" || recv == "Self" || (!recv.is_empty() && shares_receiver_word(recv, &s.qualified_name))
            }
        }
    }
}

fn call_shape(line: &str, col: usize, name: &str) -> Shape {
    let at = if line.get(col..).is_some_and(|t| t.starts_with(name)) {
        Some(col)
    } else {
        let mut from = 0;
        let mut hit = None;
        while let Some(i) = line[from..].find(name).map(|i| from + i) {
            from = i + 1;
            if i > 0 && line[..i].ends_with(|c: char| c.is_alphanumeric() || c == '_' || c == '$') {
                continue;
            }
            let rest = line[i + name.len()..].trim_start();
            if rest.starts_with('(') || rest.starts_with("::<") || rest.starts_with('!') {
                hit = Some(i);
                break;
            }
        }
        hit
    };
    let Some(start) = at else { return Shape::Unknown };
    let before = line[..start].trim_end();
    if before.ends_with("::") {
        return Shape::Path;
    }
    if !before.ends_with('.') {
        return Shape::Bare;
    }
    let recv = before.trim_end_matches('.').trim_end().trim_end_matches('?').trim_end();
    if is_tuple_receiver(recv) {
        return Shape::Chained("()".into());
    }
    Shape::Chained(receiver_name(recv))
}

fn is_tuple_receiver(src: &str) -> bool {
    if !src.ends_with(')') {
        return false;
    }
    let b = src.as_bytes();
    let mut depth = 0;
    let mut comma = false;
    let mut i = b.len();
    while i > 0 {
        i -= 1;
        match b[i] {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            b',' if depth == 1 => comma = true,
            _ => {}
        }
    }
    let before = src[..i].trim_end();
    comma && !before.ends_with(|c: char| c.is_alphanumeric() || "_$>])!?".contains(c))
}

/// The receiver a `….name(` is written on, as its path and dotted identifiers with arguments dropped.
fn receiver_name(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::new();
    let mut i = b.len();
    while i > 0 {
        let ch = b[i - 1];
        if matches!(ch, b')' | b']' | b'}') {
            let open = match ch {
                b')' => b'(',
                b']' => b'[',
                _ => b'{',
            };
            let mut depth = 0;
            let mut j = i;
            let mut found = false;
            while j > 0 {
                j -= 1;
                if b[j] == ch {
                    depth += 1;
                } else if b[j] == open {
                    depth -= 1;
                    if depth == 0 {
                        found = true;
                        break;
                    }
                }
            }
            if !found {
                return String::new();
            }
            i = j;
        } else if ch.is_ascii_alphanumeric() || matches!(ch, b'_' | b'$' | b'.' | b':' | b'!') {
            if ch != b'!' {
                out.insert(0, ch as char);
            }
            i -= 1;
        } else {
            break;
        }
    }
    out.trim_matches(|c| c == '.' || c == ':').to_string()
}

fn shares_receiver_word(receiver: &str, qualified_name: &str) -> bool {
    let Some((owner, _)) = qualified_name.rsplit_once("::") else { return false };
    let flat = |w: &str| -> String {
        let s: String = w.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
        s.trim_end_matches(|c: char| c.is_ascii_digit()).to_string()
    };
    let last_recv = receiver.rsplit('.').next().unwrap_or(receiver);
    let last_owner = owner.rsplit([':', '.']).next().unwrap_or(owner);
    if flat(last_recv) == flat(last_owner) {
        return true;
    }
    let words = |s: &str| -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = String::new();
        let chars: Vec<char> = s.chars().collect();
        for (i, &c) in chars.iter().enumerate() {
            let boundary = !c.is_alphanumeric()
                || (c.is_uppercase() && i > 0 && (chars[i - 1].is_lowercase() || (chars.get(i + 1).is_some_and(|n| n.is_lowercase()) && chars[i - 1].is_uppercase())));
            if boundary && !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            if c.is_alphanumeric() {
                cur.push(c);
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out.into_iter().filter(|w| w.len() > 1).collect()
    };
    let owner_words: HashSet<String> = words(owner).iter().map(|w| flat(w)).filter(|w| w.len() > 2).collect();
    words(receiver).iter().any(|w| owner_words.contains(&flat(w)))
}

/// The type written at the start of `text`, up to what ends it at its own depth.
pub fn type_text_at(text: &str) -> String {
    let b = text.as_bytes();
    let mut depth = 0i32;
    for (i, &ch) in b.iter().enumerate() {
        match ch {
            b'<' | b'(' | b'[' => depth += 1,
            b'>' if i == 0 || b[i - 1] != b'-' => depth = (depth - 1).max(0),
            b')' | b']' => {
                if depth == 0 {
                    return text[..i].trim().to_string();
                }
                depth -= 1;
            }
            b',' | b'=' | b';' | b'{' if depth == 0 => return text[..i].trim().to_string(),
            _ => {}
        }
    }
    text.trim().to_string()
}

/// A type by its last name, after `&`, `dyn`, `impl`: (outer, args, text).
fn type_head(ty: &str) -> Option<(String, String, String)> {
    let text = impls::strip_amp(ty);
    let text = text.strip_prefix("dyn ").or_else(|| text.strip_prefix("impl ")).unwrap_or(text).trim().to_string();
    let path_end = text.find('<').unwrap_or(text.len());
    let path = text[..path_end].trim();
    let outer = path.rsplit("::").next()?.trim().to_string();
    if !is_ident(&outer) {
        return None;
    }
    let args = if path_end < text.len() {
        let end = text.rfind('>').unwrap_or(text.len());
        text[path_end + 1..end.max(path_end + 1)].to_string()
    } else {
        String::new()
    };
    Some((outer, args, text))
}

/// Reduce a Rust field's declared type text to the simple name of the type a method call on it auto-derefs to.
fn field_type_name(raw: &str) -> Option<String> {
    let mut t = raw.trim().to_string();
    loop {
        let before = t.clone();
        t = impls::strip_amp(&t).to_string();
        for p in ["Box", "Rc", "Arc"] {
            if let Some(r) = t.strip_prefix(p) {
                if let Some(r) = r.trim_start().strip_prefix('<') {
                    t = r.trim_start().to_string();
                }
            }
        }
        for p in ["dyn ", "impl "] {
            if let Some(r) = t.strip_prefix(p) {
                t = r.trim_start().to_string();
            }
        }
        if t == before {
            break;
        }
    }
    let cut = t.find(['<', '>', '+']).unwrap_or(t.len());
    let t = t[..cut].trim();
    let seg = t.split("::").filter(|s| !s.is_empty()).last()?.to_string();
    if !is_ident(&seg)
        || ["bool", "char", "str", "String", "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64", "Self", "self"].contains(&seg.as_str())
    {
        return None;
    }
    if seg.len() == 1 && seg.starts_with(|c: char| c.is_ascii_uppercase()) {
        return None;
    }
    Some(seg)
}

/// Where `field:` is declared on a struct line (`pub field: T`, `field: T,`).
fn field_decl_at(line: &str, field: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(i) = uses::find_word(&line[from..], field).map(|i| from + i) {
        from = i + 1;
        let before = line[..i].trim_end();
        let ok_before = before.is_empty() || before.ends_with(['{', '(', ',']) || before.ends_with(']') || before.ends_with("pub") || before.ends_with(')');
        let after = line[i + field.len()..].trim_start();
        if ok_before && after.starts_with(':') && !after.starts_with("::") {
            return Some(i);
        }
    }
    None
}

fn split_generic_names(generics: &str) -> Vec<String> {
    scopes::split_top_level(generics, ',')
        .iter()
        .filter_map(|p| {
            let p = p.trim_start().strip_prefix("const ").unwrap_or(p.trim_start());
            let id: String = p.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_').collect();
            (!id.is_empty()).then_some(id)
        })
        .collect()
}

fn collapse(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Past the whitespace at `i`.
fn space_end(code: &[u8], mut i: usize) -> usize {
    while i < code.len() && code[i].is_ascii_whitespace() {
        i += 1;
    }
    i
}

/// Past the arguments of a call at `p` — `(…)`, after a turbofish `::<…>`.
fn call_args_end(sc: &scopes::Scopes, p: usize) -> Option<usize> {
    let code = sc.code.as_bytes();
    let mut q = space_end(code, p);
    if code[q..].starts_with(b"::") {
        q = space_end(code, q + 2);
        if code.get(q) != Some(&b'<') {
            return None;
        }
        let end = scopes::angle_close(code, q, (q + 512).min(code.len()))?;
        q = space_end(code, end + 1);
    }
    if code.get(q) != Some(&b'(') {
        return None;
    }
    sc.close.get(&q).map(|e| e + 1)
}

fn ident_or_index_at(code: &str, at: usize) -> Option<&str> {
    if let Some(id) = scopes::ident_at(code, at) {
        return Some(id);
    }
    let b = code.as_bytes();
    let mut e = at;
    while e < b.len() && b[e].is_ascii_digit() {
        e += 1;
    }
    (e > at).then(|| &code[at..e])
}

/// A field chain or a struct literal at `from`, up to `limit`: where it ends, its hops, and whether it writes its type.
fn field_chain_at(sc: &scopes::Scopes, from: usize, limit: usize) -> Option<(usize, usize, bool)> {
    let code = sc.code.as_str();
    let b = code.as_bytes();
    let mut p = from;
    let mut typed = false;
    if b.get(p) == Some(&b'(') {
        let end = *sc.close.get(&p)?;
        if end >= limit {
            return None;
        }
        let mut q = space_end(b, p + 1);
        if b.get(q) == Some(&b'&') {
            q = space_end(b, q + 1);
            if code[q..].starts_with("mut") && !b.get(q + 3).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                q = space_end(b, q + 3);
            }
        }
        let (iend, hops, ityped) = field_chain_at(sc, q, end)?;
        if space_end(b, iend) != end || (hops == 0 && !ityped) {
            return None;
        }
        p = end + 1;
        typed = true;
    } else {
        let mut segs = Vec::new();
        let mut q = p;
        loop {
            let Some(id) = scopes::ident_at(code, q) else { break };
            segs.push(id);
            q += id.len();
            let r = space_end(b, q);
            if code[r..].starts_with("::") && scopes::ident_at(code, space_end(b, r + 2)).is_some() {
                q = space_end(b, r + 2);
            } else {
                break;
            }
        }
        if segs.is_empty() || q > limit {
            return None;
        }
        let last = *segs.last().unwrap();
        let mut q2 = space_end(b, q);
        if code[q2..].starts_with("::") {
            let lt = space_end(b, q2 + 2);
            if b.get(lt) != Some(&b'<') {
                return None;
            }
            let end = scopes::angle_close(b, lt, (lt + 512).min(b.len()))?;
            q2 = space_end(b, end + 1);
        }
        if last.starts_with(|c: char| c.is_ascii_uppercase()) && b.get(q2) == Some(&b'{') {
            let end = *sc.close.get(&q2)?;
            if end >= limit {
                return None;
            }
            p = end + 1;
            typed = true;
        } else if segs.len() == 1 && last.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') {
            p = q;
        } else {
            return None;
        }
    }
    let mut hops = 0;
    loop {
        let q = space_end(b, p);
        if q >= limit || b.get(q) != Some(&b'.') || b.get(q + 1) == Some(&b'.') {
            break;
        }
        let q = space_end(b, q + 1);
        let Some(id) = ident_or_index_at(code, q) else { break };
        if id == "await" || call_args_end(sc, q + id.len()).is_some() {
            break;
        }
        p = q + id.len();
        hops += 1;
    }
    Some((p, hops, typed))
}

/// The receiver of the call `name(…)` whose expression starts at `start`, when it is a field chain or a struct literal.
fn written_call_receiver(sc: &scopes::Scopes, start: usize, name: &str) -> Option<String> {
    let code = sc.code.as_str();
    let b = code.as_bytes();
    if start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_') {
        return None;
    }
    let (end, hops, typed) = field_chain_at(sc, start, b.len())?;
    if hops == 0 && !typed {
        return None;
    }
    let dot = space_end(b, end);
    if b.get(dot) != Some(&b'.') || b.get(dot + 1) == Some(&b'.') {
        return None;
    }
    let at = space_end(b, dot + 1);
    if !code[at..].starts_with(name) || b.get(at + name.len()).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
        return None;
    }
    let mut p = call_args_end(sc, at + name.len())?;
    loop {
        p = space_end(b, p);
        match b.get(p) {
            Some(b'?') => {
                p += 1;
                continue;
            }
            Some(b'[') => {
                p = sc.close.get(&p)? + 1;
                continue;
            }
            Some(b'.') if b.get(p + 1) != Some(&b'.') => {}
            _ => break,
        }
        p = space_end(b, p + 1);
        let Some(id) = ident_or_index_at(code, p) else { break };
        p += id.len();
        let Some(args) = call_args_end(sc, p) else { continue };
        if id == name {
            return None;
        }
        p = args;
    }
    Some(code[start..dot].trim().to_string())
}

/// The receiver of a call kept with its receiver chain, as the blanked text writes it.
fn chained_receiver(sc: &scopes::Scopes, start: usize, chain: &[String], name: &str) -> Option<String> {
    let code = sc.code.as_str();
    let b = code.as_bytes();
    let word = |at: usize, id: &str| code[at..].starts_with(id) && !b.get(at + id.len()).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_');
    if (start > 0 && (b[start - 1].is_ascii_alphanumeric() || b[start - 1] == b'_')) || !word(start, &chain[0]) {
        return None;
    }
    let mut p = start + chain[0].len();
    let mut end = p;
    let last = format!("{name}()");
    for link in chain[1..].iter().chain(std::iter::once(&last)) {
        end = p;
        let call = link.ends_with("()");
        let id = link.strip_suffix("()").unwrap_or(link);
        let dot = space_end(b, p);
        if b.get(dot) != Some(&b'.') || b.get(dot + 1) == Some(&b'.') {
            return None;
        }
        let at = space_end(b, dot + 1);
        if !word(at, id) {
            return None;
        }
        let args = call_args_end(sc, at + id.len());
        if call != args.is_some() {
            return None;
        }
        p = if call { args.unwrap() } else { at + id.len() };
    }
    Some(code[start..end].to_string())
}

impl<'a> Ctx<'a> {
    pub fn field_value_pub(&self, receiver: &str, field: &str, at: usize, sc: &scopes::Scopes, r: &Ref<'_>) -> Option<Written> {
        // The field read through `self`: built through `value_type` on `self.field`.
        self.value_type(&format!("{receiver}.{field}"), at, sc, r, 0)
    }
}
