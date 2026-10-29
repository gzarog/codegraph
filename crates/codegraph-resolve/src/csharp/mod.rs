//! C# resolution, ported from upstream CodeGraph's C# paths: the C# branches
//! of name-matcher.ts's strategies and gates, read over the same context the
//! C/C++ port uses.

mod infer;
mod resolve;
mod scope;

pub use resolve::resolve;
