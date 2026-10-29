//! C and C++ resolution, ported from upstream CodeGraph's C/C++ paths:
//! resolution/index.ts's C/C++ gates, name-matcher.ts's C/C++ strategies and
//! the cpp-*.ts / c-*.ts helpers.

#![allow(dead_code)]

pub mod ctx;
pub mod family;
pub mod features;
pub mod includers;
pub mod includes;
pub mod js;
pub mod macro_vis;
pub mod namespaces;
pub mod raw_strings;

pub use ctx::{Ctx, Ref};
