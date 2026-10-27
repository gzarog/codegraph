//! CodeGraph: a local-first knowledge graph of a codebase for AI coding
//! agents. This crate is the project facade, the MCP server and the CLI.

pub mod format;
pub mod installer;
pub mod mcp;
pub mod project;
pub mod prompt;
pub mod watch;

pub use codegraph_context::{ContextFormat, ContextOptions};
pub use codegraph_core as core;
pub use codegraph_index::{IndexReport, Progress};
pub use project::{find_root, is_initialized, CodeGraph, ProjectError};
