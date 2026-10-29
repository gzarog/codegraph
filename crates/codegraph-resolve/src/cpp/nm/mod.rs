//! The C/C++ strategies of upstream's name-matcher.ts, split by what they
//! read. Every part shares this module's imports and re-exports the others'
//! items, as the one TypeScript module they come from does.

pub use super::aliases::{self, Tri};
pub use super::ctx::{memo, Ctx, Ref};
pub use super::family::{is_c_family, same_language_family};
pub use super::includers;
pub use super::includes;
pub use super::js;
pub use super::namespaces;
pub use super::raw_strings::{self, NonCode};
pub use super::supertypes;
pub use crate::matcher::Match;
pub use crate::symbols::SymId;
pub use codegraph_core::{Language, NodeKind, ReferenceKind};
pub use fancy_regex::Regex;
pub use std::collections::{HashMap, HashSet};
pub use std::rc::Rc;
pub use std::sync::LazyLock;

mod args;
mod calls;
mod decl;
mod infer;
mod locals;
mod members;
mod method_call;
mod strategies;
mod unit;

pub use args::*;
pub use calls::*;
pub use decl::*;
pub use infer::*;
pub use locals::*;
pub use members::*;
pub use method_call::*;
pub use strategies::*;
pub use unit::*;

/// `CPP_CLASS_KINDS`.
pub const CPP_CLASS_KINDS: &[NodeKind] = &[NodeKind::Class, NodeKind::Struct, NodeKind::Union];
/// `CPP_TYPE_KINDS`.
pub const CPP_TYPE_KINDS: &[NodeKind] = &[
    NodeKind::Class,
    NodeKind::Struct,
    NodeKind::Union,
    NodeKind::Enum,
    NodeKind::Interface,
    NodeKind::TypeAlias,
];
