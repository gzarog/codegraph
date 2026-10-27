//! Context for AI agents: ranking the symbols a question is about and
//! rendering their source, call paths and blast radius.

pub mod context;
pub mod explore;
pub mod memory;
pub mod relevance;
pub mod source;

pub use context::{build_context, format_context, ContextFormat, ContextOptions, TaskContext};
pub use explore::{budget_for, explore, explore_in_session, ExploreBudget, ExploreOptions};
pub use memory::ExploreMemory;
pub use source::{numbered, read_source, SourceFile};
