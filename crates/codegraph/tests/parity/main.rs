//! Upstream's behaviour tests (`__tests__/*.test.ts`), ported: each module
//! is one upstream area, each test keeps upstream's fixture files and
//! asserts what upstream asserts, through the helpers in `harness`.

// Helpers shared by every area; not each area uses each one.
#[allow(dead_code)]
mod harness;

mod python;
mod python_attr;
