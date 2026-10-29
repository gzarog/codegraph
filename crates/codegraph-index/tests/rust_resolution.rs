//! Rust-language resolution, ported from upstream's `__tests__/rust-*.test.ts`
//! (colbymchenry/codegraph @ 01e0ac3). Each module names the test file it
//! ports; fixtures and assertions follow the original.

mod common;

use common::{at, fq, Graph};

const CARGO_APP: &str = "[package]\nname = \"app\"\nversion = \"0.1.0\"\n";

/// rust-self-associated-type.test.ts: `Self::Error` in a signature is the
/// enclosing impl's associated type, `V::Value` a generic bound's — never a
/// struct of that name.
mod self_associated_type {
    use super::*;

    #[test]
    fn names_the_impls_associated_type_never_a_struct() {
        let g = Graph::new(&[
            ("Cargo.toml", "[package]\nname = \"serde\"\n"),
            (
                "src/de/value.rs",
                "pub struct Error {\n    msg: String,\n}\n\npub struct Value;\n",
            ),
            (
                "src/private/de.rs",
                "use crate::de::value::{Error, Value};

pub trait Deserializer {
    type Error;
}

pub struct ContentDeserializer<E> {
    err: E,
}

impl<E> Deserializer for ContentDeserializer<E> {
    type Error = E;

    fn deserialize_any<V: Visitor>(self, visitor: V) -> Result<V::Value, Self::Error> {
        todo!()
    }
}
",
            ),
        ]);
        let targets: Vec<String> = g
            .out_of_file("src/private/de.rs")
            .into_iter()
            .filter(|(e, _)| e.kind.as_str() == "references" && e.line == Some(14))
            .map(|(_, t)| at(&t))
            .collect();
        assert!(
            targets.contains(&"type_alias src/private/de.rs:12".to_string()),
            "{targets:?}"
        );
        assert!(
            !targets.iter().any(|t| t.contains("src/de/value.rs")),
            "{targets:?}"
        );
    }
}

/// rust-self-owner.test.ts: `self.m()` resolves on the impl's own type only.
mod self_owner {
    use super::*;

    const CARGO: &str = "[package]\nname=\"owners\"\nversion=\"0.1.0\"\nedition=\"2021\"\n";

    fn index(files: &[(&str, &str)]) -> Graph {
        let mut all = vec![("Cargo.toml".to_string(), CARGO.to_string())];
        all.extend(
            files
                .iter()
                .map(|(f, t)| (format!("src/{f}"), t.to_string())),
        );
        let refs: Vec<(&str, &str)> = all.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        Graph::new(&refs)
    }

    fn targets(g: &Graph, file: &str) -> Vec<String> {
        let caller = g
            .named_kind("run", "method")
            .into_iter()
            .find(|n| n.file_path == format!("src/{file}") && n.qualified_name == "Target::run")
            .expect("caller");
        g.out(&[&caller], &["calls"])
            .iter()
            .map(|(_, t)| fq(t))
            .collect()
    }

    #[test]
    fn does_not_borrow_a_missing_method_from_another_modules_namesake() {
        let g = index(&[
            ("lib.rs", "pub mod caller; pub mod decoy;"),
            (
                "caller.rs",
                "pub struct Target;\nimpl Target { pub fn run(&self) { self.reset(); } }",
            ),
            (
                "decoy.rs",
                "pub struct Target;\nimpl Target { pub fn reset(&self) {} }",
            ),
        ]);
        assert_eq!(targets(&g, "caller.rs"), Vec::<String>::new());
    }

    #[test]
    fn keeps_a_proven_local_owner_across_syncs() {
        let with = "pub struct Target;\nimpl Target { pub fn reset(&self) {} }\nimpl Target { pub fn run(&self) { self.reset(); } }";
        let without = "pub struct Target;\nimpl Target { pub fn run(&self) { self.reset(); } }";
        let mut g = index(&[
            ("lib.rs", "pub mod caller; pub mod decoy;"),
            ("caller.rs", with),
            (
                "decoy.rs",
                "pub struct Target;\nimpl Target { pub fn reset(&self) {} }",
            ),
        ]);
        assert_eq!(
            targets(&g, "caller.rs"),
            vec!["src/caller.rs:Target::reset"]
        );
        g.write("src/caller.rs", without);
        g.sync();
        assert_eq!(targets(&g, "caller.rs"), Vec::<String>::new());
        g.write("src/caller.rs", with);
        g.sync();
        assert_eq!(
            targets(&g, "caller.rs"),
            vec!["src/caller.rs:Target::reset"]
        );
    }

    #[test]
    fn keeps_a_unique_owner_whose_impl_is_split_across_files() {
        let g = index(&[
            (
                "lib.rs",
                "pub mod caller;\npub struct Target;\nimpl Target { pub fn reset(&self) {} }",
            ),
            (
                "caller.rs",
                "use crate::Target;\nimpl Target { pub fn run(&self) { self.reset(); } }",
            ),
        ]);
        assert_eq!(targets(&g, "caller.rs"), vec!["src/lib.rs:Target::reset"]);
    }

    #[test]
    fn declines_indistinguishable_inline_module_owners() {
        let g = index(&[(
            "lib.rs",
            "mod a {
    pub struct Target;
    impl Target { pub fn reset(&self) {} }
  }
  mod b {
    pub struct Target;
    impl Target { pub fn run(&self) { self.reset(); } }
  }",
        )]);
        assert_eq!(targets(&g, "lib.rs"), Vec::<String>::new());
    }
}

/// rust-prelude-scope.test.ts: a bare name reaches only what is in scope.
mod prelude_scope {
    use super::*;

    #[test]
    fn the_prelude_a_used_variant_a_same_file_item() {
        let g = Graph::new(&[
            ("Cargo.toml", CARGO_APP),
            (
                "src/lowargs.rs",
                "pub enum EncodingMode {
    Auto,
    Some(u8),
    Disabled,
}
pub enum ParseResult<T> {
    Ok(T),
    Err(String),
}
",
            ),
            ("src/hygiene.rs", "struct Ok;\nstruct Result;\nfn local() -> Ok { Ok }\n"),
            (
                "src/decompress.rs",
                "pub fn find(cmd: u8) -> Option<u8> {
    if cmd > 0 { return Some(cmd); }
    let r: std::result::Result<u8, ()> = Ok(cmd);
    None
}
",
            ),
            (
                "src/error.rs",
                "pub struct Error;\npub type Result<T> = std::result::Result<T, Error>;\n",
            ),
            ("src/api.rs", "pub fn load() -> crate::error::Result<u8> { unimplemented!() }\n"),
            (
                "src/modes.rs",
                "use crate::lowargs::EncodingMode::*;\npub fn pick() -> crate::lowargs::EncodingMode { Some(1) }\n",
            ),
        ]);
        let targets = |f: &str| -> Vec<String> {
            let from = g.named_kind(f, "function");
            let refs: Vec<_> = from.iter().collect();
            g.out(&refs, &["calls", "references", "instantiates"])
                .iter()
                .map(|(_, n)| format!("{}:{}", n.kind.as_str(), n.qualified_name))
                .collect()
        };
        let find: Vec<String> = targets("find")
            .into_iter()
            .filter(|t| t.contains("Some") || t.contains("Ok") || t.contains("Result"))
            .collect();
        assert_eq!(find, Vec::<String>::new(), "prelude Some/Ok");
        assert!(
            targets("pick").contains(&"enum_member:EncodingMode::Some".to_string()),
            "{:?}",
            targets("pick")
        );
        assert!(
            targets("load").iter().any(|t| t.ends_with(":Result")),
            "{:?}",
            targets("load")
        );
        assert!(
            targets("local").contains(&"struct:Ok".to_string()),
            "{:?}",
            targets("local")
        );
    }
}

/// rust-type-scope.test.ts: a bare type from another file is in scope only
/// through a `use`; an outside import is never a project item.
mod type_scope {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nfutures = \"0.3\"\n",
            ),
            ("src/lib.rs", "pub mod runtime;\npub mod io;\npub mod net;\n"),
            ("src/runtime/mod.rs", "pub mod context;\npub mod task;\npub mod driver;\n"),
            ("src/runtime/context.rs", "pub struct Context {\n    pub depth: u8,\n}\n"),
            ("src/runtime/task/mod.rs", "pub type Result<T> = std::result::Result<T, ()>;\n"),
            ("src/io/mod.rs", "pub mod copy;\n"),
            ("src/io/copy.rs", "pub struct Error;\n"),
            (
                "src/net.rs",
                "use std::io;
use std::task::{Context, Poll};

pub fn poll_ready(cx: &mut Context<'_>) -> Poll<io::Result<()>> {
    let _ = cx;
    Poll::Ready(Ok(()))
}

pub fn fail() -> std::io::Error {
    std::io::Error::other(\"x\")
}
",
            ),
            (
                "src/runtime/driver.rs",
                "use crate::runtime::context::Context;

pub fn enter(ctx: &Context) -> u8 {
    ctx.depth
}
",
            ),
            ("tests/support/panic.rs", "pub fn test_panic() {}\n"),
            (
                "tests/io_panic.rs",
                "mod support {
    pub mod panic;
}
use support::panic::test_panic;

#[test]
fn panics() {
    test_panic();
}
",
            ),
        ])
    }

    fn files(g: &Graph, file: &str) -> Vec<String> {
        g.out_of_file(file)
            .into_iter()
            .map(|(_, t)| t.file_path)
            .collect()
    }

    #[test]
    fn an_outside_import_is_never_a_project_item() {
        let g = graph();
        let t = files(&g, "src/net.rs");
        for f in [
            "src/runtime/context.rs",
            "src/runtime/task/mod.rs",
            "src/io/copy.rs",
        ] {
            assert!(!t.contains(&f.to_string()), "{f} in {t:?}");
        }
    }

    #[test]
    fn a_crate_local_use_and_an_inline_test_module_reach_the_item() {
        let g = graph();
        let t = files(&g, "src/runtime/driver.rs");
        assert!(t.contains(&"src/runtime/context.rs".to_string()), "{t:?}");
        let t = files(&g, "tests/io_panic.rs");
        assert!(t.contains(&"tests/support/panic.rs".to_string()), "{t:?}");
    }
}

/// rust-declared-type-scope.test.ts: a value's declared type is its binding's
/// in scope at the call.
mod declared_type_scope {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", CARGO_APP),
            (
                "src/main.rs",
                "use std::pin::Pin;

pub struct Reader;

impl Reader {
    pub fn poll_read(self: Pin<&mut Self>) {}
}

pub struct Other;

impl Other {
    pub fn poll_read(self: Pin<&mut Self>) {}
}

pub struct Holder {
    reader: Pin<Box<Other>>,
}

pub fn literal_field(mut reader: Pin<&mut Reader>) {
    let _h = Holder {
        reader: Box::pin(Other),
    };
    reader.as_mut().poll_read();
}

pub fn inner_block(mut reader: Pin<&mut Reader>, mut other: Other) {
    {
        let reader: Pin<&mut Other> = Pin::new(&mut other);
        let _ = reader;
    }
    reader.as_mut().poll_read();
}

fn main() {}
",
            ),
        ])
    }

    #[test]
    fn a_struct_literal_field_is_no_declaration() {
        let g = graph();
        let c = g.calls(&g.one("literal_field", "function"));
        assert!(c.contains(&"Reader::poll_read".to_string()), "{c:?}");
        assert!(!c.contains(&"Other::poll_read".to_string()), "{c:?}");
    }

    #[test]
    fn a_let_in_an_inner_block_does_not_shadow() {
        let g = graph();
        let c = g.calls(&g.one("inner_block", "function"));
        assert!(c.contains(&"Reader::poll_read".to_string()), "{c:?}");
        assert!(!c.contains(&"Other::poll_read".to_string()), "{c:?}");
    }
}

/// rust-framework-name-scope.test.ts: name heuristics never reach an item
/// declared inside a function body; a file's own item comes first.
mod framework_name_scope {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"crates/core\", \"crates/globset\", \"crates/web\", \"examples/tls\"]\n",
            ),
            ("crates/core/Cargo.toml", "[package]\nname = \"rg-core\"\n"),
            ("crates/web/Cargo.toml", "[package]\nname = \"web\"\n"),
            ("crates/web/src/lib.rs", "pub mod http;\n"),
            ("crates/web/src/http.rs", "pub use ::http::Uri;\n"),
            (
                "crates/web/src/routing/tests.rs",
                "fn outer_middleware_still_see_whole_url() {\n    struct Uri;\n    let _ = Uri;\n}\n",
            ),
            (
                "examples/tls/Cargo.toml",
                "[package]\nname = \"example-tls\"\n\n[dependencies]\nweb = { path = \"../../crates/web\" }\n",
            ),
            (
                "examples/tls/src/main.rs",
                "use web::http::Uri;\n\nfn make_https(uri: Uri, port: u16) -> Uri {\n    uri\n}\n",
            ),
            (
                "crates/core/src/flags/defs.rs",
                "pub struct Glob;\n\npub fn glob_flag() -> Glob {\n    Glob\n}\n",
            ),
            ("crates/globset/Cargo.toml", "[package]\nname = \"globset\"\n"),
            (
                "crates/globset/src/glob.rs",
                "pub struct Glob {
    glob: String,
}

impl Glob {
    pub fn new(glob: &str) -> Result<Glob, ()> {
        Ok(Glob { glob: glob.to_string() })
    }
}
",
            ),
        ])
    }

    fn targets(g: &Graph, file: &str) -> Vec<String> {
        g.out_of_file(file).iter().map(|(_, t)| fq(t)).collect()
    }

    #[test]
    fn never_reach_an_item_declared_inside_a_function_body() {
        let g = graph();
        let t: Vec<String> = targets(&g, "examples/tls/src/main.rs")
            .into_iter()
            .filter(|t| t.contains("outer_middleware"))
            .collect();
        assert_eq!(t, Vec::<String>::new());
    }

    #[test]
    fn take_the_files_own_item_first() {
        let g = graph();
        let globs: Vec<String> = targets(&g, "crates/globset/src/glob.rs")
            .into_iter()
            .filter(|t| t.ends_with(":Glob"))
            .collect();
        assert!(!globs.is_empty());
        assert!(
            globs.iter().all(|t| t == "crates/globset/src/glob.rs:Glob"),
            "{globs:?}"
        );
    }
}

/// rust-go-call-shape.test.ts: a bare call never reaches a method; a std-named
/// chained call needs a receiver named after the owner.
mod call_shape {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", CARGO_APP),
            (
                "src/guard.rs",
                "pub struct MutexGuard;
impl MutexGuard {
    pub fn map(self) -> Self { self }
}
pub struct CookieJar;
impl CookieJar {
    pub fn get(&self) -> Option<u8> { None }
}
pub struct Arg;
impl Arg {
    pub fn short(self, c: char) -> Self { self }
}
pub fn flag(name: &str) -> Arg { Arg }
",
            ),
            (
                "src/main.rs",
                "mod guard;
use guard::flag;
fn get(handler: u8) -> u8 { handler }
fn run(sym: Option<&str>) {
    let _ = sym.map(|s| s.len());
    let _ = get(1);
    let _ = flag(\"n\").short('n');
}
fn main() { run(None); }
",
            ),
            ("go.mod", "module example.com/app\n\ngo 1.21\n"),
            (
                "flags.go",
                "package app

type customString struct{}

func (c customString) String() string { return \"\" }

type FlagSet struct{}

func Flags() *FlagSet { return nil }
",
            ),
            (
                "cmd.go",
                "package app\n\nfunc run() {\n\tFlags().String(\"name\", \"\", \"\")\n}\n",
            ),
        ])
    }

    fn calls_in(g: &Graph, file: &str, f: &str) -> Vec<String> {
        let n = g.in_file(file).into_iter().find(|n| n.name == f).unwrap();
        g.calls(&n)
    }

    #[test]
    fn rust_bare_never_a_method_std_chain_names_need_their_receiver() {
        let g = graph();
        let t = calls_in(&g, "src/main.rs", "run");
        assert!(!t.iter().any(|t| t.ends_with("MutexGuard::map")), "{t:?}");
        assert!(!t.iter().any(|t| t.ends_with("CookieJar::get")), "{t:?}");
        assert!(t.iter().any(|t| t.ends_with("Arg::short")), "{t:?}");
    }

    #[test]
    fn go_a_std_named_chained_call_is_not_a_project_method() {
        let g = graph();
        let t = calls_in(&g, "cmd.go", "run");
        assert!(
            !t.iter().any(|t| t.ends_with("customString::String")),
            "{t:?}"
        );
    }
}

/// Join lines with `\n`, with a trailing newline (upstream's `lines(...)`).
fn lines(l: &[&str]) -> String {
    let mut s = l.join("\n");
    s.push('\n');
    s
}

/// rust-assoc-qualified-self.test.ts: `<T as Trait>::X` and `Iterator<Item = T>`.
mod assoc_qualified_self {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                "[package]\nname = \"web\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "pub mod handler;\npub mod serve;\npub mod strip;\n",
            ),
            (
                "src/handler.rs",
                &lines(&[
                    "pub trait Handler {",
                    "    type Future;",
                    "",
                    "    fn call(self) -> <Self as Handler>::Future;",
                    "}",
                    "",
                    "pub struct IntoHandler;",
                    "",
                    "impl Handler for IntoHandler {",
                    "    type Future = u8;",
                    "",
                    "    fn call(self) -> <Self as Handler>::Future {",
                    "        0",
                    "    }",
                    "}",
                ]),
            ),
            (
                "src/serve.rs",
                &lines(&[
                    "pub trait Listener {",
                    "    type Io;",
                    "}",
                    "",
                    "pub struct Ready;",
                    "",
                    "impl Listener for Ready {",
                    "    type Io = u16;",
                    "}",
                    "",
                    "pub fn handle<L: Listener>(io: <L as Listener>::Io) {}",
                    "",
                    "pub fn ready(io: <Ready as Listener>::Io) {}",
                ]),
            ),
            (
                "src/strip.rs",
                &lines(&[
                    "pub enum Item<T> {",
                    "    Both(T, T),",
                    "}",
                    "",
                    "pub fn segments(s: &str) -> impl Iterator<Item = &str> {",
                    "    s.split('/')",
                    "}",
                    "",
                    "pub fn zip(a: Vec<u8>) -> impl Iterator<Item = Item<u8>> {",
                    "    a.into_iter().map(|x| Item::Both(x, x))",
                    "}",
                ]),
            ),
        ])
    }

    #[test]
    fn self_as_trait_is_self_the_impls_own_nothing_in_the_trait() {
        let g = graph();
        assert_eq!(
            g.targets_on("src/handler.rs", 4, "Future"),
            Vec::<String>::new()
        );
        assert_eq!(
            g.targets_on("src/handler.rs", 12, "Future"),
            vec!["type_alias IntoHandler::Future src/handler.rs@10"]
        );
    }

    #[test]
    fn nothing_through_a_generic_the_types_impl_through_a_concrete_type() {
        let g = graph();
        assert_eq!(g.targets_on("src/serve.rs", 11, "Io"), Vec::<String>::new());
        assert_eq!(
            g.targets_on("src/serve.rs", 13, "Io"),
            vec!["type_alias Ready::Io src/serve.rs@8"]
        );
    }

    #[test]
    fn nothing_from_a_binding_while_the_bound_type_links() {
        let g = graph();
        assert_eq!(
            g.targets_on("src/strip.rs", 5, "Item"),
            Vec::<String>::new()
        );
        assert_eq!(
            g.targets_on("src/strip.rs", 9, "Item"),
            vec!["enum Item src/strip.rs@1"]
        );
    }
}

/// rust-foreign-impl-calls.test.ts: a call reaches a project impl on a std
/// type only as Rust picks it.
mod foreign_impl_calls {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/main.rs",
                "mod cli;\nmod names;\nmod slots;\nmod store;\nmod windows;\n\nfn main() {}\n",
            ),
            (
                "src/windows.rs",
                &lines(&[
                    "pub enum Idi {",
                    "    Install,",
                    "}",
                    "",
                    "impl From<Idi> for String {",
                    "    fn from(_e: Idi) -> Self {",
                    "        String::new()",
                    "    }",
                    "}",
                    "",
                    "pub fn label() -> String {",
                    "    String::from(Idi::Install)",
                    "}",
                ]),
            ),
            (
                "src/names.rs",
                &lines(&[
                    "use std::ffi::OsString;",
                    "",
                    "pub struct Str;",
                    "",
                    "impl From<Str> for OsString {",
                    "    fn from(_name: Str) -> Self {",
                    "        OsString::new()",
                    "    }",
                    "}",
                    "",
                    "pub fn os_name(name: Str) -> OsString {",
                    "    OsString::from(name)",
                    "}",
                ]),
            ),
            (
                "src/cli.rs",
                &lines(&[
                    "use std::ffi::OsString;",
                    "",
                    "pub fn banner() -> String {",
                    "    String::from(\"dx\")",
                    "}",
                    "",
                    "pub fn arg() -> OsString {",
                    "    OsString::from(\"--release\")",
                    "}",
                ]),
            ),
            (
                "src/store.rs",
                &lines(&[
                    "pub trait Storage {",
                    "    fn iter(&self) -> usize;",
                    "}",
                    "",
                    "pub struct Slot;",
                    "",
                    "impl Storage for Vec<Slot> {",
                    "    fn iter(&self) -> usize {",
                    "        self.len()",
                    "    }",
                    "}",
                ]),
            ),
            (
                "src/slots.rs",
                &lines(&[
                    "use crate::store::{Slot, Storage};",
                    "",
                    "pub fn count(slots: Vec<Slot>) -> usize {",
                    "    slots.iter()",
                    "}",
                ]),
            ),
            (
                "tests/files.rs",
                &lines(&[
                    "pub fn paths(files: Vec<String>) -> usize {",
                    "    files.iter().count()",
                    "}",
                ]),
            ),
        ])
    }

    #[test]
    fn not_a_std_trait_impl_unless_the_argument_is_its_type() {
        let g = graph();
        assert_eq!(
            g.targets_of("src/cli.rs", "from", &["calls"]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn reaches_it_when_the_argument_is_that_type() {
        let g = graph();
        assert_eq!(
            g.targets_of("src/windows.rs", "from", &["calls"]),
            vec!["src/windows.rs@6"]
        );
        assert_eq!(
            g.targets_of("src/names.rs", "from", &["calls"]),
            vec!["src/names.rs@6"]
        );
    }

    #[test]
    fn a_project_traits_impl_only_where_the_trait_is_in_scope() {
        let g = graph();
        assert_eq!(
            g.targets_of("src/slots.rs", "iter", &["calls"]),
            vec!["src/store.rs@8"]
        );
        assert_eq!(
            g.targets_of("tests/files.rs", "iter", &["calls"]),
            Vec::<String>::new()
        );
    }
}

/// rust-glob-use-items.test.ts: a name a glob brings in is an item of the
/// module the glob walks to.
mod glob_use_items {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"app\", \"core\", \"dioxus\", \"hooks\", \"rsx\"]\n"),
            (
                "dioxus/Cargo.toml",
                "[package]\nname = \"dioxus\"\nversion = \"0.1.0\"\n\n[dependencies]\ndioxus-core = { path = \"../core\" }\ndioxus-hooks = { path = \"../hooks\" }\n",
            ),
            (
                "dioxus/src/lib.rs",
                &lines(&[
                    "pub use dioxus_core::Ok;",
                    "pub use dioxus_hooks as hooks;",
                    "",
                    "pub mod error;",
                    "pub mod types;",
                    "",
                    "pub mod prelude {",
                    "    pub use crate::hooks::*;",
                    "    pub use crate::macros::*;",
                    "    pub use dioxus_core::{Element, ScopeId};",
                    "}",
                ]),
            ),
            ("dioxus/src/error.rs", "pub type Result<T> = std::result::Result<T, String>;\n"),
            ("dioxus/src/types.rs", "pub struct Config;\n"),
            ("core/Cargo.toml", "[package]\nname = \"dioxus-core\"\nversion = \"0.1.0\"\n"),
            (
                "core/src/lib.rs",
                "pub type Element = Option<u8>;\n\npub struct ScopeId(pub usize);\n\n#[allow(non_snake_case)]\npub fn Ok<T>(value: T) -> Result<T, ()> {\n    Result::Ok(value)\n}\n",
            ),
            (
                "hooks/Cargo.toml",
                "[package]\nname = \"dioxus-hooks\"\nversion = \"0.1.0\"\n\n[dependencies]\ndioxus-core = { path = \"../core\" }\n",
            ),
            ("hooks/src/lib.rs", "mod use_effect;\nmod use_signal;\n\npub use use_effect::*;\npub use use_signal::*;\n"),
            ("hooks/src/use_effect.rs", "use dioxus_core::*;\n\npub fn use_effect() {}\n"),
            ("hooks/src/use_signal.rs", "pub fn use_signal<T>(init: T) -> T {\n    init\n}\n"),
            ("rsx/Cargo.toml", "[package]\nname = \"dioxus-rsx\"\nversion = \"0.1.0\"\n"),
            ("rsx/src/lib.rs", "pub struct Element;\n\npub struct ScopeId;\n\npub fn use_signal() {}\n"),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ndioxus = { path = \"../dioxus\" }\n",
            ),
            (
                "app/src/main.rs",
                &lines(&[
                    "mod a;",
                    "mod error;",
                    "mod run;",
                    "mod types;",
                    "",
                    "use dioxus::prelude::*;",
                    "",
                    "fn app(scope: ScopeId) -> Element {",
                    "    let count = use_signal(0);",
                    "    use_effect();",
                    "    None",
                    "}",
                    "",
                    "fn parse() -> Result<u8, ()> {",
                    "    Ok(1)",
                    "}",
                    "",
                    "fn main() {}",
                ]),
            ),
            ("app/src/a/mod.rs", "mod b;\nmod c;\n\npub fn helper() {}\n"),
            ("app/src/a/b.rs", "use super::*;\n\npub fn run() {\n    helper();\n}\n"),
            ("app/src/a/c.rs", "pub fn helper() {}\n"),
            ("app/src/error.rs", "pub type Result<T> = std::result::Result<T, ()>;\n"),
            ("app/src/types.rs", "pub struct Config;\n"),
            (
                "app/src/run.rs",
                "use crate::error::*;\nuse dioxus::types::*;\n\npub fn go(config: Config) -> Result<()> {\n    Ok(())\n}\n",
            ),
        ])
    }

    #[test]
    fn a_prelude_glob_brings_in_what_it_re_exports_from_another_crate() {
        let g = graph();
        assert_eq!(
            g.targets_of("app/src/main.rs", "Element", &["references"]),
            vec!["core/src/lib.rs@1"]
        );
        assert_eq!(
            g.targets_of("app/src/main.rs", "ScopeId", &["references"]),
            vec!["core/src/lib.rs@3"]
        );
    }

    #[test]
    fn and_what_it_globs_in_through_a_crate_passed_on_as_a_module() {
        let g = graph();
        assert_eq!(
            g.targets_of("app/src/main.rs", "use_signal", &["calls"]),
            vec!["hooks/src/use_signal.rs@1"]
        );
        assert_eq!(
            g.targets_of("app/src/main.rs", "use_effect", &["calls"]),
            vec!["hooks/src/use_effect.rs@3"]
        );
    }

    #[test]
    fn only_what_each_module_passes_on() {
        let g = graph();
        assert_eq!(
            g.targets_of("app/src/main.rs", "Ok", &["calls"]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn use_super_glob_brings_in_the_parents_items_not_a_siblings() {
        let g = graph();
        assert_eq!(
            g.targets_of("app/src/a/b.rs", "helper", &["calls"]),
            vec!["app/src/a/mod.rs@4"]
        );
    }

    #[test]
    fn a_glob_over_another_crates_module_is_that_module() {
        let g = graph();
        assert_eq!(
            g.targets_of("app/src/run.rs", "Config", &["references"]),
            vec!["dioxus/src/types.rs@1"]
        );
        assert_eq!(
            g.targets_of("app/src/run.rs", "Result", &["references"]),
            vec!["app/src/error.rs@1"]
        );
    }
}

/// rust-turbofish-calls.test.ts: a turbofish call calls the function it wraps.
mod turbofish_calls {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", CARGO_APP),
            (
                "src/main.rs",
                &lines(&[
                    "pub struct Store {",
                    "    cache: Cache,",
                    "}",
                    "",
                    "impl Store {",
                    "    pub fn get<T>(&self) -> Option<T> { None }",
                    "",
                    "    pub fn own(&self) {",
                    "        let _ = self.get::<u16>();",
                    "        let _ = self.cache.load::<u8>();",
                    "    }",
                    "}",
                    "",
                    "pub struct Cache;",
                    "",
                    "impl Cache {",
                    "    pub fn load<T>(&self) -> Option<T> { None }",
                    "}",
                    "",
                    "/// A distractor: another `get` the typed receiver must win over.",
                    "pub struct Other;",
                    "",
                    "impl Other {",
                    "    pub fn get<T>(&self) -> Option<T> { None }",
                    "}",
                    "",
                    "pub struct Pool<T> {",
                    "    items: Vec<T>,",
                    "}",
                    "",
                    "impl<T> Pool<T> {",
                    "    pub fn new() -> Pool<T> { Pool { items: Vec::new() } }",
                    "    pub fn create<U>() -> Pool<T> { Pool { items: Vec::new() } }",
                    "    pub fn len(&self) -> usize { self.items.len() }",
                    "}",
                    "",
                    "pub fn helper<T>() -> u8 { 0 }",
                    "",
                    "pub fn run<F>(f: F) -> u8 { 0 }",
                    "",
                    "pub fn calls(s: &Store) {",
                    "    let _ = s.get::<u32>();",
                    "    let _ = helper::<Vec<u8>>();",
                    "    let _ = Pool::<u8>::new();",
                    "    let _ = Pool::<u8>::create::<u16>();",
                    "    let _ = run::<fn() -> u8>(helper::<u8>);",
                    "}",
                    "",
                    "pub fn chains() -> usize {",
                    "    Pool::<u8>::new().len() + Pool::<u8>::create::<u16>().len()",
                    "}",
                    "",
                    "fn main() {}",
                ]),
            ),
        ])
    }

    #[test]
    fn a_method_call_on_a_typed_local_reaches_its_types_method() {
        let g = graph();
        let c = g.callees("calls");
        assert!(c.contains(&"Store::get".to_string()), "{c:?}");
        assert!(!c.contains(&"Other::get".to_string()), "{c:?}");
    }

    #[test]
    fn a_free_function_and_a_path_call_reach_what_they_name() {
        let g = graph();
        let c = g.callees("calls");
        for want in ["helper", "run", "Pool::new", "Pool::create"] {
            assert!(c.contains(&want.to_string()), "{want} not in {c:?}");
        }
    }

    #[test]
    fn a_call_on_self_or_a_field_of_self_reaches_the_method() {
        let g = graph();
        assert_eq!(g.callees("own"), vec!["Cache::load", "Store::get"]);
    }

    #[test]
    fn a_chain_through_a_turbofish_path_reaches_what_it_returns() {
        let g = graph();
        assert_eq!(
            g.callees("chains"),
            vec!["Pool::create", "Pool::len", "Pool::len", "Pool::new"]
        );
    }

    #[test]
    fn keeps_no_type_arguments_in_a_call_reference() {
        let g = graph();
        let refs = g.store.refs_in_file("src/main.rs").unwrap();
        assert!(!refs.is_empty());
        let bad: Vec<String> = refs
            .into_iter()
            .map(|r| r.reference_name)
            .filter(|n| n.contains("::<"))
            .collect();
        assert_eq!(bad, Vec::<String>::new());
    }
}

/// rust-enum-variant-reference.test.ts: an enum used only through its
/// variants is a dependency of the code that uses them.
mod enum_variant_reference {
    use super::*;

    const CARGO: &str = "[package]\nname = \"repro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

    const MODE: &str = "pub enum Mode {
    A,
    B,
    C(u8),
    D { x: u8 },
}

pub fn takes(_m: Mode) {}

pub fn current() -> Mode { Mode::B }
";

    fn index(files: &[(&str, &str)]) -> Graph {
        let mut all = vec![("Cargo.toml".to_string(), CARGO.to_string())];
        all.extend(
            files
                .iter()
                .map(|(f, t)| (format!("src/{f}"), t.to_string())),
        );
        let refs: Vec<(&str, &str)> = all.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
        Graph::new(&refs)
    }

    fn of_kind_in(g: &Graph, kind: &str, file: &str, name: &str) -> codegraph_core::Node {
        g.named_kind(name, kind)
            .into_iter()
            .find(|n| n.file_path == format!("src/{file}"))
            .unwrap_or_else(|| panic!("{kind} {name} in {file}"))
    }

    /// `file:name` of every symbol with a `references` edge to `n`, sorted.
    fn referencers(g: &Graph, n: &codegraph_core::Node) -> Vec<String> {
        let mut v: Vec<String> = g
            .incoming(n, &["references"])
            .into_iter()
            .map(|(_, s)| format!("{}:{}", s.file_path.trim_start_matches("src/"), s.name))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    fn mode_refs(g: &Graph, file: &str) -> Vec<String> {
        referencers(g, &of_kind_in(g, "enum", file, "Mode"))
    }

    fn function(g: &Graph, name: &str) -> codegraph_core::Node {
        g.one(name, "function")
    }

    #[test]
    fn a_variant_path_in_an_expression_and_match_arms_through_the_module() {
        let g = index(&[
            ("mode.rs", MODE),
            (
                "variant_use.rs",
                "use crate::mode;

pub fn code(flag: bool) -> u8 {
    let m = if flag { mode::Mode::A } else { mode::Mode::B };
    match m {
        mode::Mode::A => 1,
        mode::Mode::B => 2,
        _ => 3,
    }
}
",
            ),
            (
                "main.rs",
                "mod mode;
mod variant_use;

fn main() {
    mode::takes(mode::Mode::A);
    let _ = variant_use::code(true);
}
",
            ),
        ]);
        assert_eq!(
            mode_refs(&g, "mode.rs"),
            vec![
                "main.rs:main",
                "mode.rs:current",
                "mode.rs:takes",
                "variant_use.rs:code"
            ]
        );
    }

    #[test]
    fn a_bare_variant_path_after_use_of_the_enum() {
        let g = index(&[
            ("lib.rs", "pub mod mode;\npub mod bare;\n"),
            ("mode.rs", MODE),
            (
                "bare.rs",
                "use crate::mode::{self, Mode};

pub fn pick() -> u8 {
    let _m = Mode::B;
    0
}

pub fn score() -> u8 {
    match mode::current() {
        Mode::A => 1,
        _ => 0,
    }
}
",
            ),
        ]);
        assert_eq!(
            mode_refs(&g, "mode.rs"),
            vec![
                "bare.rs:pick",
                "bare.rs:score",
                "mode.rs:current",
                "mode.rs:takes"
            ]
        );
        let pick = function(&g, "pick");
        let at: Vec<String> = g
            .out(&[&pick], &["references"])
            .iter()
            .map(|(e, _)| format!("{}:{}", e.line.unwrap_or(0), e.column.unwrap_or(0)))
            .collect();
        assert_eq!(at, vec!["4:13"]);
    }

    #[test]
    fn tuple_and_struct_variant_patterns_and_a_variant_value() {
        let g = index(&[
            ("lib.rs", "pub mod mode;\npub mod pat;\n"),
            ("mode.rs", MODE),
            (
                "pat.rs",
                "use crate::mode::{self, Mode};

pub fn tuple() -> u8 {
    match mode::current() {
        Mode::C(x) => x,
        _ => 0,
    }
}

pub fn structure() -> u8 {
    if let crate::mode::Mode::D { x } = mode::current() { x } else { 0 }
}

pub fn ctor_value(xs: Vec<u8>) -> usize {
    xs.into_iter().map(Mode::C).count()
}
",
            ),
        ]);
        assert_eq!(
            mode_refs(&g, "mode.rs"),
            vec![
                "mode.rs:current",
                "mode.rs:takes",
                "pat.rs:ctor_value",
                "pat.rs:structure",
                "pat.rs:tuple"
            ]
        );
    }

    #[test]
    fn constructing_a_variant_stays_linked_to_the_variant() {
        let g = index(&[
            ("lib.rs", "pub mod mode;\npub mod build;\n"),
            ("mode.rs", MODE),
            (
                "build.rs",
                "use crate::mode::Mode;

pub fn build() {
    let _c = Mode::C(3);
    let _d = Mode::D { x: 2 };
}
",
            ),
        ]);
        let build = function(&g, "build");
        let mut out: Vec<String> = g
            .out(&[&build], &[])
            .iter()
            .map(|(e, t)| format!("{}:{}", e.kind.as_str(), t.qualified_name))
            .collect();
        out.sort();
        assert_eq!(out, vec!["calls:Mode::C", "instantiates:Mode::D"]);
    }

    #[test]
    fn self_variant_inside_impl_in_the_enums_file_and_another() {
        let mode_with_impl = format!(
            "{MODE}
impl Mode {{
    pub fn flip(&self) -> u8 {{
        match self {{
            Self::A => 1,
            _ => 0,
        }}
    }}
}}
"
        );
        let g = index(&[
            ("lib.rs", "pub mod mode;\npub mod ops;\n"),
            ("mode.rs", &mode_with_impl),
            (
                "ops.rs",
                "use crate::mode::Mode;

impl Mode {
    pub fn is_b(&self) -> bool {
        if let Self::B = self { true } else { false }
    }
}
",
            ),
        ]);
        assert_eq!(
            mode_refs(&g, "mode.rs"),
            vec![
                "mode.rs:current",
                "mode.rs:flip",
                "mode.rs:takes",
                "ops.rs:is_b"
            ]
        );
    }

    #[test]
    fn a_variant_imported_by_name_and_used_bare_has_no_enum_path() {
        let g = index(&[
            ("lib.rs", "pub mod mode;\npub mod other;\npub mod glob;\n"),
            ("mode.rs", MODE),
            ("other.rs", "pub enum Mode {\n    A,\n    Z,\n}\n"),
            (
                "glob.rs",
                "use crate::mode::Mode::*;

pub fn bare(m: u8) -> u8 {
    match m {
        0 => { let _ = A; 1 }
        _ => 0,
    }
}
",
            ),
        ]);
        assert_eq!(
            mode_refs(&g, "mode.rs"),
            vec!["mode.rs:current", "mode.rs:takes"]
        );
        assert_eq!(mode_refs(&g, "other.rs"), Vec::<String>::new());
    }

    #[test]
    fn the_path_picks_the_type_that_declares_the_variant() {
        let g = index(&[
            (
                "lib.rs",
                "pub mod matching;\npub mod other;\npub mod user;\n",
            ),
            (
                "matching.rs",
                "pub enum Match {\n    None,\n    Ignore,\n}\n",
            ),
            ("other.rs", "pub struct Match;\n"),
            (
                "user.rs",
                "use crate::matching::Match;

pub enum ColorChoice {
    Auto,
    Ansi,
}

pub fn first() -> u8 {
    let _m = Match::None;
    0
}

pub fn color() -> u8 {
    let _c = termcolor::ColorChoice::AlwaysAnsi;
    0
}
",
            ),
        ]);
        assert_eq!(
            referencers(&g, &of_kind_in(&g, "enum", "matching.rs", "Match")),
            vec!["user.rs:first"]
        );
        assert_eq!(
            referencers(&g, &of_kind_in(&g, "struct", "other.rs", "Match")),
            Vec::<String>::new()
        );
        assert_eq!(
            referencers(&g, &of_kind_in(&g, "enum", "user.rs", "ColorChoice")),
            Vec::<String>::new()
        );
    }

    #[test]
    fn an_associated_const_or_function_is_not_a_variant_path() {
        let mode_with_impl = format!(
            "{MODE}
impl Mode {{
    pub const ALL: [u8; 2] = [0, 1];
    pub fn count() -> usize {{ Self::ALL.len() }}
}}
"
        );
        let g = index(&[
            ("lib.rs", "pub mod mode;\npub mod limits;\npub mod user;\n"),
            ("mode.rs", &mode_with_impl),
            (
                "limits.rs",
                "pub struct Limits;\n\nimpl Limits {\n    pub const MAX: u8 = 3;\n    pub fn new() -> Self { Limits }\n}\n",
            ),
            (
                "user.rs",
                "use crate::limits::Limits;
use crate::mode::Mode;

pub fn cap() -> u8 {
    Limits::MAX
}

pub fn all() -> usize {
    Mode::ALL.len()
}

pub fn make() {
    let _l = Limits::new();
    let _f = Limits::new;
}
",
            ),
        ]);
        assert_eq!(
            mode_refs(&g, "mode.rs"),
            vec!["mode.rs:current", "mode.rs:takes"]
        );
        assert_eq!(
            referencers(&g, &of_kind_in(&g, "struct", "limits.rs", "Limits")),
            Vec::<String>::new()
        );
        let make = function(&g, "make");
        let out: Vec<String> = g
            .out(&[&make], &[])
            .iter()
            .map(|(e, t)| format!("{}:{}", e.kind.as_str(), t.qualified_name))
            .collect();
        assert_eq!(out, vec!["calls:Limits::new"]);
    }

    #[test]
    fn std_and_prelude_enums_never_reach_a_same_named_project_enum() {
        let g = index(&[
            ("lib.rs", "pub mod shapes;\npub mod user;\n"),
            (
                "shapes.rs",
                "pub enum Ordering {
    Less,
    Greater,
}

pub enum Option {
    Some,
    None,
}
",
            ),
            (
                "user.rs",
                "use std::cmp::{self, Ordering};

pub fn sorted(a: u8, b: u8) -> bool {
    a.cmp(&b) == Ordering::Less || b.cmp(&a) == cmp::Ordering::Greater || a.cmp(&b) != std::cmp::Ordering::Less
}

pub fn first(o: Option<u8>) -> u8 {
    match o {
        Option::Some(x) => x,
        Option::None => 0,
    }
}

pub fn bare(o: Option<u8>, r: Result<u8, ()>) -> u8 {
    match (o, r) {
        (Some(x), Ok(_)) => x,
        (None, Err(())) => 0,
        _ => 1,
    }
}
",
            ),
        ]);
        assert_eq!(
            referencers(&g, &of_kind_in(&g, "enum", "shapes.rs", "Ordering")),
            Vec::<String>::new()
        );
        assert_eq!(
            referencers(&g, &of_kind_in(&g, "enum", "shapes.rs", "Option")),
            Vec::<String>::new()
        );
        for v in g
            .store
            .nodes_by_kind("enum_member".parse().unwrap())
            .unwrap()
        {
            let inc: Vec<String> = g
                .incoming(&v, &[])
                .into_iter()
                .filter(|(e, _)| e.kind.as_str() != "contains")
                .map(|(e, s)| format!("{} from {}", e.kind.as_str(), s.qualified_name))
                .collect();
            assert_eq!(inc, Vec::<String>::new(), "{}", v.qualified_name);
        }
    }

    #[test]
    fn a_module_path_an_associated_function_and_a_namesake_enum_are_not_references() {
        let mode_with_impl =
            format!("{MODE}\nimpl Mode {{\n    pub fn new() -> Mode {{ Mode::A }}\n}}\n");
        let g = index(&[
            (
                "lib.rs",
                "pub mod mode;\npub mod other;\npub mod util;\npub mod paths;\n",
            ),
            ("mode.rs", &mode_with_impl),
            ("other.rs", "pub enum Mode {\n    A,\n    Z,\n}\n"),
            ("util.rs", "pub fn take(x: u8) -> u8 { x }\n"),
            (
                "paths.rs",
                "use crate::mode;
use crate::other;
use crate::util;

pub fn via_module() -> u8 {
    util::take(3)
}

pub fn via_ctor() {
    let _m = mode::Mode::new();
}

pub fn via_other() -> u8 {
    match other::Mode::A {
        other::Mode::Z => 1,
        _ => 0,
    }
}

pub fn via_mode() {
    let _m = mode::Mode::A;
}
",
            ),
        ]);
        let via_module = function(&g, "via_module");
        assert!(g.out(&[&via_module], &["references"]).is_empty());
        assert_eq!(mode_refs(&g, "other.rs"), vec!["paths.rs:via_other"]);
        assert_eq!(
            mode_refs(&g, "mode.rs"),
            vec![
                "mode.rs:current",
                "mode.rs:new",
                "mode.rs:takes",
                "paths.rs:via_mode"
            ]
        );
    }
}

/// rust-generic-bound-paths.test.ts: a call through a generic parameter
/// reaches its bound trait's method (or a supertrait's).
mod generic_bound_paths {
    use super::*;

    const CARGO: &str = "[package]\nname = \"pb\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

    fn keys() -> String {
        lines(&[
            "pub struct RawMap;",
            "",
            "pub trait FfiMapKey: Sized {",
            "    unsafe fn get(map: RawMap, key: Self, value: *mut u8) -> bool;",
            "    unsafe fn insert(map: RawMap, key: Self, value: u8) -> bool;",
            "}",
            "",
            "pub trait MapKey: Clone + FfiMapKey {}",
        ])
    }

    fn calls(g: &Graph, caller: &str) -> Vec<String> {
        let node = g
            .by_qualified(caller)
            .into_iter()
            .find(|n| matches!(n.kind.as_str(), "function" | "method"))
            .unwrap_or_else(|| panic!("no function or method {caller}"));
        let mut v = g.calls(&node);
        v.sort();
        v
    }

    #[test]
    fn a_path_call_through_a_bound_is_the_supertraits_method() {
        let g = Graph::new(&[
            ("Cargo.toml", CARGO),
            ("src/lib.rs", &lines(&["pub mod keys;", "pub mod map;"])),
            ("src/keys.rs", &keys()),
            (
                "src/map.rs",
                &lines(&[
                    "use crate::keys::{MapKey, RawMap};",
                    "",
                    "pub struct Map;",
                    "",
                    "impl Map {",
                    "    pub fn map_get<Key: MapKey>(key: Key) -> bool {",
                    "        unsafe { Key::get(RawMap, key, std::ptr::null_mut()) }",
                    "    }",
                    "}",
                ]),
            ),
        ]);
        assert_eq!(calls(&g, "Map::map_get"), vec!["FfiMapKey::get"]);
    }

    #[test]
    fn a_where_clause_bound_and_an_impl_bound_count_the_same() {
        let g = Graph::new(&[
            ("Cargo.toml", CARGO),
            ("src/lib.rs", &lines(&["pub mod keys;", "pub mod map;"])),
            ("src/keys.rs", &keys()),
            (
                "src/map.rs",
                &lines(&[
                    "use crate::keys::{MapKey, RawMap};",
                    "",
                    "pub struct Map;",
                    "",
                    "impl Map {",
                    "    pub fn map_insert<Key>(key: Key) -> bool",
                    "    where",
                    "        Key: MapKey,",
                    "    {",
                    "        unsafe { Key::insert(RawMap, key, 1) }",
                    "    }",
                    "}",
                    "",
                    "pub struct Typed<K>(K);",
                    "",
                    "impl<K: MapKey> Typed<K> {",
                    "    pub fn lookup(&self) -> bool {",
                    "        unsafe { K::get(RawMap, self.0.clone(), std::ptr::null_mut()) }",
                    "    }",
                    "}",
                ]),
            ),
        ]);
        assert_eq!(calls(&g, "Map::map_insert"), vec!["FfiMapKey::insert"]);
        assert_eq!(calls(&g, "Typed::lookup"), vec!["FfiMapKey::get"]);
    }

    #[test]
    fn an_outside_bound_links_no_project_method() {
        let g = Graph::new(&[
            ("Cargo.toml", CARGO),
            (
                "src/lib.rs",
                &lines(&[
                    "pub struct Defaults;",
                    "",
                    "impl Defaults {",
                    "    pub fn default() -> Defaults {",
                    "        Defaults",
                    "    }",
                    "}",
                    "",
                    "pub fn make<T: Default>() -> T {",
                    "    T::default()",
                    "}",
                ]),
            ),
        ]);
        assert_eq!(calls(&g, "make"), Vec::<String>::new());
    }

    #[test]
    fn two_bound_traits_declaring_the_method_link_neither() {
        let g = Graph::new(&[
            ("Cargo.toml", CARGO),
            (
                "src/lib.rs",
                &lines(&[
                    "pub trait Engine {",
                    "    fn run() -> u8;",
                    "}",
                    "",
                    "pub trait Task {",
                    "    fn run() -> u8;",
                    "}",
                    "",
                    "pub fn start<T: Engine + Task>() -> u8 {",
                    "    T::run()",
                    "}",
                ]),
            ),
        ]);
        assert_eq!(calls(&g, "start"), Vec::<String>::new());
    }
}

/// rust-loop-tuple-field-types.test.ts: a `for` variable is an item of what
/// the loop iterates; a tuple pattern binds the tuple's field at its place.
mod loop_tuple_field_types {
    use super::*;

    fn oneshot() -> String {
        lines(&[
            "pub struct Sender;",
            "",
            "impl Sender {",
            "    pub fn send(&self) {}",
            "}",
            "",
            "pub struct Receiver;",
            "",
            "impl Receiver {",
            "    pub fn recv(&self) {}",
            "}",
            "",
            "pub fn channel() -> (Sender, Receiver) {",
            "    (Sender, Receiver)",
            "}",
        ])
    }

    fn graph() -> Graph {
        let os = oneshot();
        Graph::new(&[
            ("Cargo.toml", CARGO_APP),
            (
                "src/main.rs",
                "mod loom;\nmod model;\nmod oneshot;\nmod loom_oneshot;\n\nfn main() {}\n",
            ),
            ("src/oneshot.rs", &os),
            ("src/loom_oneshot.rs", &os),
            (
                "src/loom.rs",
                &lines(&[
                    "use crate::loom_oneshot as oneshot;",
                    "",
                    "pub fn run() {",
                    "    let (tx, rx) = oneshot::channel();",
                    "    tx.send();",
                    "    rx.recv();",
                    "}",
                ]),
            ),
            (
                "src/model.rs",
                &lines(&[
                    "pub struct Item;",
                    "",
                    "impl Item {",
                    "    pub fn price(&self) -> u32 {",
                    "        0",
                    "    }",
                    "}",
                    "",
                    "pub struct Key;",
                    "",
                    "impl Key {",
                    "    pub fn label(&self) {}",
                    "}",
                    "",
                    "// Same-file namesakes, which a guess by name alone may take.",
                    "pub struct Decoy;",
                    "",
                    "impl Decoy {",
                    "    pub fn price(&self) -> u32 {",
                    "        0",
                    "    }",
                    "    pub fn label(&self) {}",
                    "}",
                    "",
                    "pub struct Store {",
                    "    pub items: Vec<Item>,",
                    "    pub index: std::collections::HashMap<Key, Item>,",
                    "    pub pair: (Key, Item),",
                    "}",
                    "",
                    "impl Store {",
                    "    pub fn all(&self) -> impl Iterator<Item = &Item> {",
                    "        self.items.iter()",
                    "    }",
                    "    pub fn total(&self) -> u32 {",
                    "        let mut sum = 0;",
                    "        for item in &self.items {",
                    "            sum += item.price();",
                    "        }",
                    "        sum",
                    "    }",
                    "    pub fn entries(&self) {",
                    "        for (key, item) in &self.index {",
                    "            key.label();",
                    "            item.price();",
                    "        }",
                    "    }",
                    "    pub fn numbered(&self) {",
                    "        for (i, item) in self.items.iter().enumerate() {",
                    "            item.price();",
                    "        }",
                    "    }",
                    "    pub fn keys(&self) {",
                    "        for key in self.index.keys() {",
                    "            key.label();",
                    "        }",
                    "    }",
                    "    pub fn walk(&self) {",
                    "        for item in self.all() {",
                    "            item.price();",
                    "        }",
                    "    }",
                    "    pub fn split(&self) {",
                    "        let (key, item) = &self.pair;",
                    "        key.label();",
                    "        item.price();",
                    "    }",
                    "}",
                    "",
                    "fn make_pair() -> (Key, Item) {",
                    "    (Key, Item)",
                    "}",
                    "",
                    "pub fn unpack() {",
                    "    let (key, item) = make_pair();",
                    "    key.label();",
                    "    item.price();",
                    "}",
                    "",
                    "pub fn shadowed(store: Store, item: Decoy) {",
                    "    item.label();",
                    "    for item in store.index.values() {",
                    "        item.price();",
                    "    }",
                    "}",
                ]),
            ),
        ])
    }

    /// `file@line` of what the calls out of `f` (declared in `file`) reach, sorted.
    fn calls_from(g: &Graph, file: &str, f: &str) -> Vec<String> {
        let froms: Vec<_> = g
            .in_file(file)
            .into_iter()
            .filter(|n| n.name == f)
            .collect();
        let refs: Vec<_> = froms.iter().collect();
        let mut v: Vec<String> = g
            .out(&refs, &["calls"])
            .iter()
            .map(|(_, t)| format!("{}@{}", t.file_path, t.start_line))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    #[test]
    fn loops() {
        let g = graph();
        let m = "src/model.rs";
        assert_eq!(
            calls_from(&g, m, "total"),
            vec!["src/model.rs@4"],
            "field item"
        );
        assert_eq!(
            calls_from(&g, m, "entries"),
            vec!["src/model.rs@12", "src/model.rs@4"],
            "(k, v)"
        );
        assert_eq!(
            calls_from(&g, m, "numbered"),
            vec!["src/model.rs@4"],
            "enumerate"
        );
        assert_eq!(
            calls_from(&g, m, "keys"),
            vec!["src/model.rs@12"],
            ".keys()"
        );
        assert_eq!(
            calls_from(&g, m, "walk"),
            vec!["src/model.rs@32", "src/model.rs@4"],
            "impl Iterator"
        );
        assert_eq!(
            calls_from(&g, m, "shadowed"),
            vec!["src/model.rs@22", "src/model.rs@4"],
            "shadowing"
        );
    }

    #[test]
    fn tuple_patterns() {
        let g = graph();
        let m = "src/model.rs";
        assert_eq!(
            calls_from(&g, m, "split"),
            vec!["src/model.rs@12", "src/model.rs@4"],
            "tuple field"
        );
        assert_eq!(
            calls_from(&g, m, "unpack"),
            vec!["src/model.rs@12", "src/model.rs@4", "src/model.rs@70"],
            "returned tuple"
        );
        let c = calls_from(&g, "src/loom.rs", "run");
        assert!(c.contains(&"src/loom_oneshot.rs@4".to_string()), "{c:?}");
        assert!(c.contains(&"src/loom_oneshot.rs@10".to_string()), "{c:?}");
        assert!(!c.contains(&"src/oneshot.rs@4".to_string()), "{c:?}");
        assert!(!c.contains(&"src/oneshot.rs@10".to_string()), "{c:?}");
    }
}

/// What the calls on `line` of `file` link to, as `qualifiedName@file:line`, sorted.
fn calls_at(g: &Graph, file: &str, line: u32) -> Vec<String> {
    let mut v: Vec<String> = g
        .out_of_file(file)
        .into_iter()
        .filter(|(e, _)| e.kind.as_str() == "calls" && e.line == Some(line))
        .map(|(_, t)| format!("{}@{}:{}", t.qualified_name, t.file_path, t.start_line))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// `file@line` of what the calls out of the symbols named `f` in `file` reach, sorted.
fn calls_from(g: &Graph, file: &str, f: &str) -> Vec<String> {
    let froms: Vec<_> = g
        .in_file(file)
        .into_iter()
        .filter(|n| n.name == f)
        .collect();
    let refs: Vec<_> = froms.iter().collect();
    let mut v: Vec<String> = g
        .out(&refs, &["calls"])
        .iter()
        .map(|(_, t)| format!("{}@{}", t.file_path, t.start_line))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// rust-as-mut-chains.test.ts: a call on `.as_mut()`/`.as_ref()` is a call on
/// the receiver's declared type as `as_mut` passes it on.
mod as_mut_chains {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", "[package]\nname = \"rt\"\nversion = \"0.1.0\"\n"),
            (
                "src/main.rs",
                "mod copy;\nmod decoys;\nmod holder;\nmod interval;\nmod projected;\nmod pump;\nmod sleep;\n\nfn main() {}\n",
            ),
            (
                "src/sleep.rs",
                r#"use std::pin::Pin;

pub struct Sleep {
    deadline: u64,
}

impl Sleep {
    pub fn reset(self: Pin<&mut Self>, deadline: u64) {}

    pub fn deadline(&self) -> u64 {
        self.deadline
    }
}
"#,
            ),
            (
                "src/decoys.rs",
                r#"use std::pin::Pin;

pub struct Writer;

impl Writer {
    pub fn start_seek(self: Pin<&mut Self>, pos: u64) {}
}

pub struct GuardMut;

impl GuardMut {
    pub fn map(self, f: u8) -> u8 {
        f
    }
}

pub struct Lines;

impl Lines {
    pub fn as_pin_mut(self: Pin<&mut Self>) {}
}
"#,
            ),
            (
                "src/interval.rs",
                r#"use std::pin::Pin;

use crate::sleep::Sleep;

pub struct Interval {
    delay: Pin<Box<Sleep>>,
    timer: Option<Sleep>,
}

impl Interval {
    pub fn reset(&mut self, at: u64) {
        self.delay.as_mut().reset(at);
    }

    pub fn peek(&mut self) -> u64 {
        self.delay.as_ref().deadline()
    }

    pub fn stop(&mut self) {
        self.timer.as_mut().map(|_| ());
    }
}
"#,
            ),
            (
                "src/holder.rs",
                r#"use std::ptr::NonNull;

use crate::sleep::Sleep;

pub struct Holder {
    inner: Box<Sleep>,
}

impl Holder {
    pub fn check(&mut self) -> u64 {
        self.inner.as_mut().deadline()
    }
}

pub struct Shared {
    entry: NonNull<Sleep>,
    next: Option<Sleep>,
}

impl Shared {
    pub unsafe fn when(&self) -> u64 {
        unsafe { self.entry.as_ref().deadline() }
    }

    pub fn upcoming(&mut self) -> Option<u64> {
        Some(self.next.as_mut()?.deadline())
    }
}
"#,
            ),
            (
                "src/projected.rs",
                r#"use std::pin::Pin;

use crate::sleep::Sleep;

pin_project_lite::pin_project! {
    pub struct Timeout<W> {
        #[pin]
        inner: W,
        #[pin]
        delay: Sleep,
        timer: Option<Sleep>,
    }
}

impl<W> Timeout<W> {
    pub fn poll_seek(self: Pin<&mut Self>, pos: u64) {
        let mut me = self.project();
        me.inner.as_mut().start_seek(pos);
        me.delay.as_mut().reset(pos);
        me.timer.as_mut().as_pin_mut();
    }
}
"#,
            ),
            (
                "src/copy.rs",
                r#"use std::pin::Pin;

use crate::sleep::Sleep;

pub fn read_some<R: Unpin>(mut reader: Pin<&mut R>, deadline: Pin<&mut Sleep>) -> u64 {
    reader.as_mut().start_seek(0);
    deadline.as_ref().deadline()
}
"#,
            ),
            (
                "src/pump.rs",
                r#"use std::pin::Pin;

pub struct Pump;

impl Pump {
    fn step(self: Pin<&mut Self>) {}

    fn get_mut(&mut self) -> u8 {
        0
    }

    pub fn run(mut self: Pin<&mut Self>) {
        self.as_mut().step();
        self.as_mut().get_mut();
    }
}
"#,
            ),
        ])
    }

    const RESET: &str = "Sleep::reset@src/sleep.rs:8";
    const DEADLINE: &str = "Sleep::deadline@src/sleep.rs:10";

    #[test]
    fn fields() {
        let g = graph();
        assert_eq!(
            calls_at(&g, "src/interval.rs", 12),
            vec![RESET],
            "Pin<Box<T>>"
        );
        assert_eq!(
            calls_at(&g, "src/interval.rs", 16),
            vec![DEADLINE],
            "Pin<Box<T>> as_ref"
        );
        assert_eq!(
            calls_at(&g, "src/interval.rs", 20),
            Vec::<String>::new(),
            "Option<T>"
        );
        assert_eq!(calls_at(&g, "src/holder.rs", 11), vec![DEADLINE], "Box<T>");
        assert_eq!(
            calls_at(&g, "src/holder.rs", 22),
            vec![DEADLINE],
            "NonNull<T>"
        );
        assert_eq!(
            calls_at(&g, "src/holder.rs", 26),
            vec![DEADLINE],
            "Option<T>?"
        );
    }

    #[test]
    fn pin_projected_fields() {
        let g = graph();
        assert_eq!(calls_at(&g, "src/projected.rs", 18), Vec::<String>::new());
        assert_eq!(calls_at(&g, "src/projected.rs", 19), vec![RESET]);
        assert_eq!(calls_at(&g, "src/projected.rs", 20), Vec::<String>::new());
    }

    #[test]
    fn values_declared_pin_mut() {
        let g = graph();
        assert_eq!(calls_at(&g, "src/copy.rs", 6), Vec::<String>::new());
        assert_eq!(calls_at(&g, "src/copy.rs", 7), vec![DEADLINE]);
    }

    #[test]
    fn self_as_mut_through_pinned_self() {
        let g = graph();
        assert_eq!(
            calls_at(&g, "src/pump.rs", 13),
            vec!["Pump::step@src/pump.rs:6"]
        );
        assert_eq!(calls_at(&g, "src/pump.rs", 14), Vec::<String>::new());
    }
}

/// rust-associated-item-scope.test.ts: an associated `type`/`const` belongs
/// to its type, reached only through a path.
mod associated_item_scope {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", "[package]\nname = \"web\"\nversion = \"0.1.0\"\n"),
            (
                "src/lib.rs",
                "pub mod de;\npub mod error;\npub mod parts;\npub mod response;\npub mod routing;\npub mod value;\npub mod value_hint;\n",
            ),
            (
                "src/de/mod.rs",
                r#"macro_rules! declare_error_trait {
    () => {
        pub trait Error: Sized {}
    };
}

declare_error_trait!();

pub trait SeqAccess {
    type Error;
}

pub struct Access;

impl SeqAccess for Access {
    type Error = String;
}
"#,
            ),
            (
                "src/value.rs",
                "use crate::de::Error;\n\npub struct ValueError;\n\nimpl crate::de::Error for ValueError {}\n",
            ),
            ("src/response.rs", "pub struct Body;\n\npub type Response<T = Body> = http::Response<T>;\n"),
            ("src/error.rs", "pub struct Error;\n"),
            (
                "src/routing/mod.rs",
                r#"mod route;
#[cfg(test)]
mod tests;

use crate::response::Response;
use std::convert::Infallible;

pub struct Router;

impl<B> tower::Service<http::Request<B>> for Router {
    type Response = Response;
    type Error = Infallible;

    fn call(&mut self, req: http::Request<B>) -> Result<Self::Response, Self::Error> {
        todo!()
    }
}
"#,
            ),
            (
                "src/routing/tests.rs",
                "use crate::response::Response;\n\nfn handler() -> Response {\n    todo!()\n}\n",
            ),
            (
                "src/routing/route.rs",
                r#"use crate::response::Response;

pub struct Route;

impl<B> tower::Service<http::Request<B>> for Route {
    type Response = Response;

    fn call(&mut self, req: http::Request<B>) -> Self::Response {
        todo!()
    }
}

pub struct RouteFuture;

impl RouteFuture {
    pub fn new(response: Response) -> Self {
        RouteFuture
    }
}
"#,
            ),
            (
                "src/value_hint.rs",
                r#"use std::str::FromStr;

pub enum ValueHint {
    Unknown,
    Other,
}

impl FromStr for ValueHint {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "other" => Ok(ValueHint::Other),
            _ => Err(format!("unknown hint: {s}")),
        }
    }
}

pub fn parse(s: &str) -> Result<ValueHint, String> {
    if s.is_empty() {
        return Err(String::from("empty"));
    }
    s.parse()
}
"#,
            ),
            (
                "src/parts.rs",
                r#"use std::convert::Infallible;

pub trait IntoParts {
    type Error;
}

pub struct Extensions;

impl IntoParts for Extensions {
    type Error = Infallible;
}

#[derive(Debug)]
pub struct HeaderError;

impl std::error::Error for HeaderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        None
    }
}

pub struct Jar;

pub struct JarIter;

impl Jar {
    pub fn iter(&self) -> impl Iterator<Item = String> + '_ {
        JarIter
    }
}

impl Iterator for JarIter {
    type Item = String;

    fn next(&mut self) -> Option<Self::Item> {
        None
    }
}
"#,
            ),
        ])
    }

    fn named_out(g: &Graph, file: &str, from: &str, name: &str, kinds: &[&str]) -> Vec<String> {
        let n = g
            .in_file(file)
            .into_iter()
            .find(|n| n.name == from)
            .unwrap();
        g.out(&[&n], kinds)
            .into_iter()
            .filter(|(_, t)| t.name == name)
            .map(|(_, t)| format!("{} {}@{}", t.kind.as_str(), t.file_path, t.start_line))
            .collect()
    }

    #[test]
    fn not_what_a_bare_call_names() {
        let g = graph();
        assert_eq!(
            g.targets_of("src/value_hint.rs", "Err", &["calls"]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn not_what_a_bare_type_names() {
        let g = graph();
        assert_eq!(
            g.targets_of("src/routing/tests.rs", "Response", &["references"]),
            vec!["src/response.rs@3"]
        );
        assert_eq!(
            named_out(
                &g,
                "src/routing/route.rs",
                "new",
                "Response",
                &["references"]
            ),
            vec!["type_alias src/response.rs@3"]
        );
    }

    #[test]
    fn not_what_a_module_path_names() {
        let g = graph();
        assert_eq!(
            named_out(&g, "src/parts.rs", "source", "Error", &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn not_what_a_use_or_a_path_to_its_module_names() {
        let g = graph();
        let t: Vec<String> = g
            .out_of_file("src/value.rs")
            .into_iter()
            .filter(|(_, t)| t.name == "Error")
            .map(|(_, t)| format!("{} {}@{}", t.kind.as_str(), t.file_path, t.start_line))
            .collect();
        assert_eq!(t, Vec::<String>::new());
    }

    #[test]
    fn not_the_item_an_iterator_binding_names() {
        let g = graph();
        assert_eq!(
            named_out(&g, "src/parts.rs", "iter", "Item", &[]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn still_what_self_names_inside_its_impl() {
        let g = graph();
        assert_eq!(
            g.targets_of("src/value_hint.rs", "Err", &["references"]),
            vec!["src/value_hint.rs@9"]
        );
        let call = g
            .in_file("src/routing/mod.rs")
            .into_iter()
            .find(|n| n.name == "call")
            .unwrap();
        let mut t: Vec<String> = g
            .out(&[&call], &["references"])
            .into_iter()
            .map(|(_, t)| format!("{} {}@{}", t.name, t.file_path, t.start_line))
            .collect();
        t.sort();
        assert_eq!(
            t,
            vec![
                "Error src/routing/mod.rs@12",
                "Response src/routing/mod.rs@11"
            ]
        );
        assert_eq!(
            g.targets_of("src/parts.rs", "Item", &["references"]),
            vec!["src/parts.rs@33"]
        );
    }
}

/// rust-method-value-types.test.ts: a local bound to a method call's value
/// has the type that method returns.
mod method_value_types {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", CARGO_APP),
            (
                "src/main.rs",
                "mod app;\nmod codec;\nmod ext;\nmod net;\nmod other;\nmod printer;\nmod widget;\n\nfn main() {}\n",
            ),
            (
                "src/ext.rs",
                r#"pub trait Extension {}

pub struct Completer;

impl Extension for Completer {}

impl Completer {
    pub fn complete_at(&self) {}
}

pub struct Arg;

impl Arg {
    pub fn get<T: Extension>(&self) -> Option<&T> {
        None
    }
}
"#,
            ),
            (
                "src/codec.rs",
                r#"pub trait Encoder<Item> {
    fn encode(&mut self, item: Item);
}

pub struct Codec;
pub struct Frame;

impl Encoder<Frame> for Codec {
    fn encode(&mut self, item: Frame) {}
}

impl Encoder<String> for Codec {
    fn encode(&mut self, item: String) {}
}
"#,
            ),
            (
                "src/printer.rs",
                r#"pub trait WriteColor {}

pub struct Standard<W> {
    wtr: W,
}

impl<W: WriteColor> Standard<W> {
    pub fn sink(&self) {}
}

pub struct StandardBuilder;

impl StandardBuilder {
    pub fn build<W: WriteColor>(&self, wtr: W) -> Standard<W> {
        Standard { wtr }
    }
}

pub struct NoColor;

impl WriteColor for NoColor {}
"#,
            ),
            (
                "src/widget.rs",
                r#"pub trait Configure {
    fn configured(self) -> Self
    where
        Self: Sized,
    {
        self
    }
}

pub struct Widget;

impl Configure for Widget {}

impl Widget {
    pub fn draw(&self) {}
}
"#,
            ),
            (
                "src/net.rs",
                r#"pub struct Builder {
    port: u16,
}

impl Builder {
    pub fn new() -> Self {
        Builder { port: 0 }
    }
    pub fn port(self, port: u16) -> Self {
        Builder { port }
    }
    pub fn build(self) -> Server {
        Server
    }
}

pub struct Server;

impl Server {
    pub fn run(&self) {}
    fn state(&self) -> State {
        State
    }
    pub fn tick(&self) {
        let st = self.state();
        st.advance();
    }
}

pub struct State;

impl State {
    pub fn advance(&self) {}
}

pub struct Config;

impl Config {
    pub fn load(&self) -> Result<Settings, String> {
        Ok(Settings)
    }
}

pub struct Settings;

impl Settings {
    pub fn apply(&self) {}
}

pub struct Client;

impl Client {
    pub async fn connect(&self) -> Conn {
        Conn
    }
    pub fn fetch(&self) -> impl std::future::Future<Output = Conn> {
        async { Conn }
    }
}

pub struct Conn;

impl Conn {
    pub fn send(&self) {}
}
"#,
            ),
            (
                "src/other.rs",
                r#"pub struct Other;

impl Other {
    pub fn run(&self) {}
    pub fn advance(&self) {}
    pub fn apply(&self) {}
    pub fn send(&self) {}
    pub fn build(&self) -> Other {
        Other
    }
    pub async fn connect(&self) -> Other {
        Other
    }
    pub fn fetch(&self) -> Other {
        Other
    }
}
"#,
            ),
            (
                "src/app.rs",
                r#"use crate::net::{Builder, Client, Config};

pub fn start() {
    let b = Builder::new();
    let server = b.port(80).build();
    server.run();
}

pub fn setup(cfg: Config) -> Result<(), String> {
    let s = cfg.load()?;
    s.apply();
    Ok(())
}

pub async fn talk(c: Client) {
    let conn = c.connect().await;
    conn.send();
}

pub async fn fetched(c: Client) {
    let conn = c.fetch().await;
    conn.send();
}

// Same-file namesakes, which a guess by name alone prefers.
pub struct Decoy;

impl Decoy {
    pub fn run(&self) {}
    pub fn apply(&self) {}
    pub fn send(&self) {}
    pub fn complete_at(&self) {}
    pub fn draw(&self) {}
    pub fn sink(&self) {}
}

pub fn print(b: crate::printer::StandardBuilder) {
    let printer = b.build(crate::printer::NoColor);
    printer.sink();
}

pub fn complete(arg: crate::ext::Arg) {
    if let Some(completer) = arg.get::<crate::ext::Completer>() {
        completer.complete_at();
    }
}

pub fn encode_frame(mut codec: crate::codec::Codec) {
    use crate::codec::Encoder;
    codec.encode(crate::codec::Frame);
    codec.encode(String::new());
}

pub fn show(w: crate::widget::Widget) {
    use crate::widget::Configure;
    let w2 = w.configured();
    w2.draw();
}
"#,
            ),
        ])
    }

    #[test]
    fn a_builder_chains_last_method() {
        let g = graph();
        let c = calls_from(&g, "src/app.rs", "start");
        assert!(c.contains(&"src/net.rs@20".to_string()), "{c:?}");
        assert!(!c.contains(&"src/app.rs@29".to_string()), "{c:?}");
        assert!(!c.contains(&"src/other.rs@4".to_string()), "{c:?}");
    }

    #[test]
    fn question_mark_result_await_and_self_methods() {
        let g = graph();
        assert_eq!(
            calls_from(&g, "src/app.rs", "setup"),
            vec!["src/net.rs@39", "src/net.rs@47"],
            "?"
        );
        assert_eq!(
            calls_from(&g, "src/app.rs", "talk"),
            vec!["src/net.rs@53", "src/net.rs@64"],
            "await"
        );
        assert_eq!(
            calls_from(&g, "src/app.rs", "fetched"),
            vec!["src/net.rs@56", "src/net.rs@64"],
            "impl Future"
        );
        assert_eq!(
            calls_from(&g, "src/net.rs", "tick"),
            vec!["src/net.rs@21", "src/net.rs@33"],
            "self method"
        );
    }

    #[test]
    fn a_turbofish_gives_the_methods_type_parameter() {
        let g = graph();
        let c = calls_from(&g, "src/app.rs", "complete");
        assert!(c.contains(&"src/ext.rs@8".to_string()), "{c:?}");
        assert!(!c.contains(&"src/app.rs@32".to_string()), "{c:?}");
    }

    #[test]
    fn the_return_types_head_and_a_provided_self_method() {
        let g = graph();
        assert_eq!(
            calls_from(&g, "src/app.rs", "print"),
            vec!["src/printer.rs@14", "src/printer.rs@8"]
        );
        assert_eq!(
            calls_from(&g, "src/app.rs", "show"),
            vec!["src/widget.rs@15", "src/widget.rs@2"]
        );
    }

    #[test]
    fn the_impl_whose_trait_argument_is_the_calls_argument() {
        let g = graph();
        assert_eq!(
            calls_from(&g, "src/app.rs", "encode_frame"),
            vec!["src/codec.rs@13", "src/codec.rs@9"]
        );
    }
}

/// `kind qualifiedName file:line` of each edge target.
fn kqfl(t: &codegraph_core::Node) -> String {
    format!(
        "{} {} {}:{}",
        t.kind.as_str(),
        t.qualified_name,
        t.file_path,
        t.start_line
    )
}

/// rust-module-path-call.test.ts: a call through a module path is the free
/// function that module declares.
mod module_path_call {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", CARGO_APP),
            (
                "src/lib.rs",
                r#"mod util;
mod only;
mod a;
mod caller;
mod importer;
mod handle;
mod worker;
mod twice;
mod inline_reexport;
mod fs;
mod uses_std;

pub fn via_self() -> usize {
    self::util::take(1)
}
"#,
            ),
            (
                "src/twice.rs",
                r#"#[cfg(test)]
mod tests {
    pub fn pick() -> usize {
        0
    }
}

pub fn pick() -> usize {
    1
}

pub fn via_twice() -> usize {
    crate::twice::pick()
}
"#,
            ),
            (
                "src/inline_reexport.rs",
                r#"#[cfg(unix)]
mod imp {
    pub fn lone() {}
}
pub use imp::*;

pub fn via_inline() {
    crate::inline_reexport::lone();
}
"#,
            ),
            ("src/fs/mod.rs", "pub fn read(path: &str) -> usize {\n    path.len()\n}\n"),
            (
                "src/uses_std.rs",
                "use std::fs;\n\npub fn loads() {\n    let _ = fs::read(\"config.toml\");\n}\n",
            ),
            (
                "src/util.rs",
                r#"pub struct Buf;

impl Buf {
    pub fn take(&self, n: usize) -> usize {
        n
    }
}

pub trait Take {
    fn take(&self) -> usize;
}

fn outer() -> usize {
    fn take() -> usize {
        0
    }
    take()
}

pub fn take(n: usize) -> usize {
    n
}
"#,
            ),
            (
                "src/only.rs",
                "pub struct Only;\n\nimpl Only {\n    pub fn grab(&self) -> usize {\n        0\n    }\n}\n",
            ),
            ("src/a/mod.rs", "pub mod b;\n"),
            (
                "src/a/b.rs",
                "pub struct Deep;\n\nimpl Deep {\n    pub fn get(&self) -> usize {\n        0\n    }\n}\n\npub fn get() -> usize {\n    1\n}\n",
            ),
            ("src/importer.rs", "use crate::util::take;\n"),
            ("src/handle.rs", "pub struct Handle;\n"),
            (
                "src/worker.rs",
                r#"pub use crate::handle::Handle;
mod dump;

pub trait Lock {
    type Handle;
}

pub struct Guard;

impl Lock for Guard {
    type Handle = u8;
}
"#,
            ),
            ("src/worker/dump.rs", "use super::Handle;\n"),
            (
                "src/caller.rs",
                r#"use crate::util;

pub fn via_crate() -> usize {
    crate::util::take(1)
}

pub fn via_super() -> usize {
    super::util::take(1)
}

pub fn via_module() -> usize {
    util::take(1)
}

pub fn via_nested() -> usize {
    crate::a::b::get()
}

pub fn via_method_only() -> usize {
    crate::only::grab()
}

pub fn via_missing() -> usize {
    crate::nowhere::take(1)
}
"#,
            ),
        ])
    }

    fn calls(g: &Graph, file: &str, name: &str) -> Vec<String> {
        let f = g
            .in_file(file)
            .into_iter()
            .find(|n| n.name == name)
            .unwrap();
        g.out(&[&f], &["calls"])
            .iter()
            .map(|(_, t)| kqfl(t))
            .collect()
    }

    fn imports(g: &Graph, file: &str) -> Vec<String> {
        let nodes = g.in_file(file);
        let refs: Vec<_> = nodes.iter().collect();
        g.out(&refs, &["imports"])
            .iter()
            .map(|(_, t)| kqfl(t))
            .collect()
    }

    #[test]
    fn reach_the_modules_free_function_never_a_method_or_local_fn() {
        let g = graph();
        let take = vec!["function take src/util.rs:20"];
        assert_eq!(calls(&g, "src/caller.rs", "via_crate"), take);
        assert_eq!(calls(&g, "src/caller.rs", "via_super"), take);
        assert_eq!(calls(&g, "src/caller.rs", "via_module"), take);
        assert_eq!(calls(&g, "src/lib.rs", "via_self"), take);
    }

    #[test]
    fn bind_a_use_of_the_function_to_it() {
        let g = graph();
        assert_eq!(
            imports(&g, "src/importer.rs"),
            vec!["function take src/util.rs:20"]
        );
    }

    #[test]
    fn never_bind_a_use_to_an_associated_type() {
        let g = graph();
        let t: Vec<String> = imports(&g, "src/worker/dump.rs")
            .into_iter()
            .filter(|t| t.contains("src/worker.rs"))
            .collect();
        assert_eq!(t, Vec::<String>::new());
    }

    #[test]
    fn walk_a_nested_path_and_link_nothing_when_there_is_no_free_function() {
        let g = graph();
        assert_eq!(
            calls(&g, "src/caller.rs", "via_nested"),
            vec!["function get src/a/b.rs:9"]
        );
        assert_eq!(
            calls(&g, "src/caller.rs", "via_method_only"),
            Vec::<String>::new()
        );
        assert_eq!(
            calls(&g, "src/caller.rs", "via_missing"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn inline_modules_and_std_modules() {
        let g = graph();
        assert_eq!(
            calls(&g, "src/twice.rs", "via_twice"),
            vec!["function pick src/twice.rs:8"]
        );
        assert_eq!(
            calls(&g, "src/inline_reexport.rs", "via_inline"),
            vec!["function lone src/inline_reexport.rs:3"]
        );
        assert_eq!(calls(&g, "src/uses_std.rs", "loads"), Vec::<String>::new());
    }
}

/// rust-path-same-file.test.ts: a name written through a path is an item of
/// what the path names.
mod path_same_file {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                "[package]\nname = \"rt\"\nversion = \"0.1.0\"\n\n[dependencies]\nmio = \"1\"\n",
            ),
            (
                "src/lib.rs",
                "pub mod ast;\npub mod error;\npub mod exhaustiveness;\npub mod interest;\npub mod os_str;\npub mod position;\n",
            ),
            ("src/ast.rs", "pub enum Pattern {\n    Int,\n    Discard,\n}\n"),
            (
                "src/exhaustiveness.rs",
                r#"use crate::ast;

pub enum Pattern {
    Variable,
}

pub fn check(pattern: &ast::Pattern) -> bool {
    true
}

pub fn own() -> Pattern {
    Pattern::Variable
}
"#,
            ),
            (
                "src/error/mod.rs",
                r#"use std::{error, fmt, io};

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub struct Error;

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        Ok(())
    }
}

impl error::Error for Error {}
"#,
            ),
            (
                "src/interest.rs",
                "pub struct Interest(usize);\n\nimpl Interest {\n    pub fn to_mio(self) -> mio::Interest {\n        todo!()\n    }\n}\n",
            ),
            (
                "src/os_str.rs",
                "pub struct OsStr;\n\nimpl OsStr {\n    pub fn as_os_str(&self) -> &std::ffi::OsStr {\n        todo!()\n    }\n}\n",
            ),
            (
                "src/position.rs",
                r#"pub enum Source {
    Record { first: bool },
}

pub enum Builder {
    Record { fields: usize },
}

pub fn source() -> Source {
    Source::Record { first: true }
}

mod inner {
    pub struct Cell;
}

pub fn cell() -> inner::Cell {
    inner::Cell
}

mod flat {
pub struct Flat;
}

pub fn flat() -> flat::Flat {
    flat::Flat
}

mod outer {
    pub struct Shell;
    pub mod deep {
        pub struct Shell;
    }
}

pub fn shell() -> outer::Shell {
    todo!()
}
"#,
            ),
        ])
    }

    #[test]
    fn the_module_a_path_writes_not_the_files_namesake() {
        let g = graph();
        assert_eq!(
            g.targets_on("src/exhaustiveness.rs", 7, "Pattern"),
            vec!["enum Pattern src/ast.rs@1"]
        );
        assert_eq!(
            g.targets_on("src/exhaustiveness.rs", 11, "Pattern"),
            vec!["enum Pattern src/exhaustiveness.rs@3"]
        );
    }

    #[test]
    fn std_and_outside_crates_through_their_paths() {
        let g = graph();
        assert_eq!(
            g.targets_on("src/error/mod.rs", 8, "Error"),
            Vec::<String>::new()
        );
        assert_eq!(
            g.targets_on("src/error/mod.rs", 14, "Result"),
            Vec::<String>::new()
        );
        assert_eq!(
            g.targets_on("src/error/mod.rs", 19, "Error"),
            Vec::<String>::new()
        );
        assert_eq!(
            g.targets_on("src/interest.rs", 4, "Interest"),
            Vec::<String>::new()
        );
        assert_eq!(
            g.targets_on("src/os_str.rs", 4, "OsStr"),
            Vec::<String>::new()
        );
    }

    #[test]
    fn variants_and_inline_modules() {
        let g = graph();
        assert_eq!(
            g.targets_on("src/position.rs", 10, "Record"),
            vec!["enum_member Source::Record src/position.rs@2"]
        );
        assert_eq!(
            g.targets_on("src/position.rs", 17, "Cell"),
            vec!["struct Cell src/position.rs@14"]
        );
        assert_eq!(
            g.targets_on("src/position.rs", 25, "Flat"),
            vec!["struct Flat src/position.rs@22"]
        );
        assert_eq!(
            g.targets_on("src/position.rs", 36, "Shell"),
            vec!["struct Shell src/position.rs@30"]
        );
    }

    #[test]
    fn a_path_through_a_module_a_use_binds() {
        let g = Graph::new(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"rt\", \"rt-test\"]\n"),
            (
                "rt/Cargo.toml",
                "[package]\nname = \"rt\"\nversion = \"0.1.0\"\n\n[dev-dependencies]\nrt-test = { path = \"../rt-test\" }\n",
            ),
            ("rt/src/lib.rs", "pub mod runtime;\n"),
            ("rt/src/runtime/mod.rs", "pub mod io;\npub mod signal;\n"),
            ("rt/src/runtime/io/mod.rs", "pub struct Handle;\n"),
            (
                "rt/src/runtime/signal.rs",
                "use crate::runtime::io;\n\npub struct Handle;\n\npub fn new(io_handle: &io::Handle) -> Handle {\n    Handle\n}\n",
            ),
            (
                "rt-test/Cargo.toml",
                "[package]\nname = \"rt-test\"\nversion = \"0.1.0\"\n\n[dependencies]\nrt = { path = \"../rt\" }\n",
            ),
            ("rt-test/src/lib.rs", "pub mod io;\n"),
            ("rt-test/src/io.rs", "pub struct Handle;\n"),
        ]);
        assert_eq!(
            g.targets_on("rt/src/runtime/signal.rs", 5, "Handle"),
            vec![
                "struct Handle rt/src/runtime/io/mod.rs@1",
                "struct Handle rt/src/runtime/signal.rs@3"
            ]
        );
    }
}

/// rust-private-item-scope.test.ts: a private item is in scope only in its
/// module and the modules inside it; a `use` names its crate's item.
mod private_item_scope {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n"),
            (
                "src/main.rs",
                "mod error;\nmod codegen;\nmod serve;\nmod watch;\n\npub(crate) use error::*;\npub(crate) use codegen::*;\n\nfn main() {}\n",
            ),
            (
                "src/error.rs",
                "pub(crate) type Result<T, E = Error> = std::result::Result<T, E>;\n\npub struct Error;\n",
            ),
            ("src/codegen/mod.rs", "mod patch;\n\npub(crate) use patch::*;\n"),
            (
                "src/codegen/patch.rs",
                r#"mod jump;

type Result<T, E = PatchError> = std::result::Result<T, E>;

pub struct PatchError;

fn checksum(bytes: &[u8]) -> u32 {
    bytes.len() as u32
}

pub fn apply(bytes: &[u8]) -> Result<u32> {
    Ok(checksum(bytes))
}
"#,
            ),
            (
                "src/codegen/patch/jump.rs",
                "use super::*;\n\npub fn jump(bytes: &[u8]) -> Result<u32> {\n    Ok(checksum(bytes))\n}\n",
            ),
            ("src/serve.rs", "use crate::Result;\n\npub fn serve() -> Result<()> {\n    Ok(())\n}\n"),
            (
                "src/watch.rs",
                "use crate::{Error, Result};\n\npub fn watch() -> Result<(), Error> {\n    Ok(())\n}\n\nfn checksum(bytes: &[u8]) -> u32 {\n    bytes.len() as u32\n}\n\npub fn scan(bytes: &[u8]) -> u32 {\n    checksum(bytes)\n}\n",
            ),
        ])
    }

    #[test]
    fn records_the_visibility_of_a_type_alias() {
        let g = graph();
        let aliases = |f: &str| -> Vec<String> {
            g.in_file(f)
                .into_iter()
                .filter(|n| n.kind.as_str() == "type_alias")
                .map(|n| {
                    n.visibility
                        .map(|v| v.as_str().to_string())
                        .unwrap_or_default()
                })
                .collect()
        };
        assert_eq!(aliases("src/error.rs"), vec!["public"]);
        assert_eq!(aliases("src/codegen/patch.rs"), vec!["private"]);
    }

    #[test]
    fn out_of_scope_outside_its_module() {
        let g = graph();
        assert_eq!(
            g.targets_of("src/serve.rs", "Result", &["references"]),
            vec!["src/error.rs@1"]
        );
        assert_eq!(
            g.targets_of("src/watch.rs", "Result", &["references"]),
            vec!["src/error.rs@1"]
        );
    }

    #[test]
    fn in_scope_in_its_module_and_those_inside_it() {
        let g = graph();
        assert_eq!(
            g.targets_of("src/codegen/patch.rs", "Result", &["references"]),
            vec!["src/codegen/patch.rs@3"]
        );
        assert_eq!(
            g.targets_of("src/codegen/patch/jump.rs", "Result", &["references"]),
            vec!["src/codegen/patch.rs@3"]
        );
        assert_eq!(
            g.targets_of("src/codegen/patch/jump.rs", "checksum", &["calls"]),
            vec!["src/codegen/patch.rs@7"]
        );
        assert_eq!(
            g.targets_of("src/watch.rs", "checksum", &["calls"]),
            vec!["src/watch.rs@7"]
        );
        assert_eq!(
            g.targets_of("src/codegen/patch.rs", "checksum", &["calls"]),
            vec!["src/codegen/patch.rs@7"]
        );
    }

    fn workspace() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"app\", \"examples\", \"facade\", \"helper\"]\n\n[workspace.dependencies]\nhelper = { path = \"helper\" }\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nfacade = { path = \"../facade\" }\n\n[dev-dependencies]\nhelper.workspace = true\n",
            ),
            ("app/src/lib.rs", "mod io;\n"),
            ("app/src/io/mod.rs", "mod blocking;\n"),
            (
                "app/src/io/blocking.rs",
                "fn spawn<F: FnOnce()>(f: F) {\n    f()\n}\n\npub(crate) fn run() {\n    spawn(|| {});\n}\n",
            ),
            (
                "app/tests/notify.rs",
                r#"use facade::Thing;
use helper::task::spawn;

#[test]
fn notified() {
    let _task = spawn(());
}

fn keep(thing: Thing) -> Thing {
    thing
}
"#,
            ),
            (
                "examples/Cargo.toml",
                "[package]\nname = \"examples\"\nversion = \"0.1.0\"\n\n[dependencies]\napp = { path = \"../app\" }\n",
            ),
            (
                "examples/custom.rs",
                "mod runtime {\n    pub fn spawn<T>(task: T) -> T {\n        task\n    }\n}\n\nfn main() {\n    runtime::spawn(());\n}\n",
            ),
            (
                "facade/Cargo.toml",
                "[package]\nname = \"facade\"\nversion = \"0.1.0\"\n\n[dependencies]\nshared = { package = \"shared-types\", path = \"../types\" }\n",
            ),
            ("facade/src/lib.rs", "pub use shared::Thing;\n"),
            ("helper/Cargo.toml", "[package]\nname = \"helper\"\nversion = \"0.1.0\"\n"),
            ("helper/src/lib.rs", "pub mod task;\n"),
            ("helper/src/task.rs", "pub fn spawn<T>(task: T) -> T {\n    task\n}\n"),
            ("types/Cargo.toml", "[package]\nname = \"shared-types\"\nversion = \"0.1.0\"\n"),
            ("types/src/lib.rs", "pub struct Thing;\n"),
        ])
    }

    #[test]
    fn a_use_names_an_item_of_its_crate_or_a_re_export_from_a_dependency() {
        let g = workspace();
        assert_eq!(
            g.targets_of("app/tests/notify.rs", "spawn", &["calls"]),
            vec!["helper/src/task.rs@1"]
        );
        assert_eq!(
            g.targets_of("app/tests/notify.rs", "Thing", &["references"]),
            vec!["types/src/lib.rs@1"]
        );
        assert_eq!(
            g.targets_of("app/src/io/blocking.rs", "spawn", &["calls"]),
            vec!["app/src/io/blocking.rs@1"]
        );
    }
}

fn crate_toml(name: &str, deps: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n{deps}")
}

/// `calls` reference names extraction records for a Rust method body.
fn call_names(body: &str) -> Vec<String> {
    let source = format!("impl Svc {{\n    fn shapes(&self, x: &X) {{\n{body}\n    }}\n}}\n");
    let mut v: Vec<String> =
        codegraph_index::extract("shapes.rs", codegraph_core::Language::Rust, &source)
            .unresolved_references
            .into_iter()
            .filter(|r| r.reference_kind.as_str() == "calls")
            .map(|r| r.reference_name)
            .collect();
    v.sort();
    v
}

/// rust-receiver-chains.test.ts: a method call keeps the receiver chain it is
/// written on, and resolution reads the receiver's type through all of it.
mod receiver_chains {
    use super::*;

    fn issue() -> Graph {
        let app = crate_toml("app", "api = { path = \"../api\" }\n");
        Graph::new(&[
            (
                "Cargo.toml",
                "[workspace]\nmembers = [\"api\", \"app\"]\nresolver = \"2\"\n",
            ),
            ("api/Cargo.toml", &crate_toml("api", "")),
            (
                "api/src/lib.rs",
                "pub trait Store {\n    fn put(&self, key: &str);\n}\n",
            ),
            ("app/Cargo.toml", &app),
            (
                "app/src/lib.rs",
                r#"use api::Store;

pub struct Services { pub store: Box<dyn Store> }
pub struct Svc { pub services: Services, pub store: Box<dyn Store> }

impl Svc {
    pub fn a(&self) { self.services.store.put("a"); }
    pub fn b(&self) { let _ = self.services.store.put("b"); }
    pub fn c(&self) {
        self
            .services
            .store
            .put("c");
    }
    pub fn d(&self) { self.store.put("d"); }
    pub fn e(&self) {
        self
            .store
            .put("e");
    }
    pub fn m(&self) { self.store().put("m"); }
    pub fn store(&self) -> &dyn Store { &*self.store }
}
"#,
            ),
        ])
    }

    fn chains() -> Graph {
        Graph::new(&[
            ("Cargo.toml", &crate_toml("chains", "")),
            (
                "src/lib.rs",
                r#"pub struct Row { cells: Vec<u8> }

impl Row {
    pub fn get(&self, i: usize) -> u8 { self.cells[i] }
}

pub struct Table { rows: Vec<Row>, names: Vec<String> }

impl Table {
    pub fn get(&self, _k: &str) -> &Row { &self.rows[0] }
    pub fn names(&self) -> &Vec<String> { &self.names }
}

pub struct Db { pub table: Table }
pub struct Wrap(pub Table);

/// A project namesake of std's `len`.
pub struct Counter;

impl Counter {
    pub fn len(&self) -> usize { 0 }
}

impl Db {
    pub fn cell(&self) -> u8 { self.table.get("a").get(0) }
    pub fn count(&self) -> usize { self.table.names().len() }
}

impl Wrap {
    pub fn first(&self) -> u8 { self.0.get("c").get(2) }
}

pub fn local(db: &Db) -> u8 {
    db.table
        .get("b")
        .get(1)
}
"#,
            ),
        ])
    }

    /// The non-heuristic calls on `line` of `file`, as `qualifiedName resolvedBy refName`.
    fn at(g: &Graph, file: &str, line: u32) -> Vec<String> {
        let mut v: Vec<String> = g
            .out_of_file(file)
            .into_iter()
            .filter(|(e, _)| {
                e.kind.as_str() == "calls"
                    && e.line == Some(line)
                    && e.provenance != Some(codegraph_core::Provenance::Heuristic)
            })
            .map(|(e, t)| {
                let m = e.metadata.clone().unwrap_or_default();
                let s = |k: &str| m[k].as_str().unwrap_or("undefined").to_string();
                format!("{} {} {}", t.qualified_name, s("resolvedBy"), s("refName"))
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }

    #[test]
    fn the_issue_workspace() {
        let g = issue();
        let put = "Store::put instance-method self.services.store.put";
        assert_eq!(at(&g, "app/src/lib.rs", 7), vec![put]);
        assert_eq!(at(&g, "app/src/lib.rs", 8), vec![put]);
        assert_eq!(at(&g, "app/src/lib.rs", 10), vec![put]);
        assert_eq!(
            at(&g, "app/src/lib.rs", 21),
            vec![
                "Store::put instance-method self.store().put",
                "Svc::store qualified-name self.store"
            ]
        );
        assert_eq!(
            at(&g, "app/src/lib.rs", 15),
            vec!["Store::put instance-method self.store.put"]
        );
        assert_eq!(
            at(&g, "app/src/lib.rs", 17),
            vec!["Store::put instance-method self.store.put"]
        );
        let open: Vec<String> = g
            .store
            .refs_in_file("app/src/lib.rs")
            .unwrap()
            .into_iter()
            .filter(|r| r.reference_kind.as_str() == "calls")
            .map(|r| r.reference_name)
            .collect();
        assert_eq!(open, Vec::<String>::new(), "no call left unresolved");
    }

    #[test]
    fn a_later_link_calling_the_same_name_is_its_own_call() {
        let g = chains();
        assert_eq!(
            at(&g, "src/lib.rs", 25),
            vec![
                "Row::get instance-method self.table.get().get",
                "Table::get instance-method self.table.get"
            ]
        );
        assert_eq!(
            at(&g, "src/lib.rs", 30),
            vec![
                "Row::get instance-method self.0.get().get",
                "Table::get instance-method self.0.get"
            ]
        );
        assert_eq!(
            at(&g, "src/lib.rs", 34),
            vec![
                "Row::get instance-method db.table.get().get",
                "Table::get instance-method db.table.get"
            ]
        );
        assert_eq!(
            at(&g, "src/lib.rs", 26),
            vec!["Table::names instance-method self.table.names"]
        );
    }

    #[test]
    fn extraction_keeps_the_receiver_chain() {
        assert_eq!(
            call_names("self.services.store.put(\"a\");"),
            vec!["self.services.store.put"]
        );
        assert_eq!(
            call_names("self.store().put(\"m\");"),
            vec!["self.store", "self.store().put"]
        );
        assert_eq!(call_names("self.0.len();"), vec!["self.0.len"]);
        assert_eq!(call_names("x.map.get(\"k\");"), vec!["x.map.get"]);
        assert_eq!(
            call_names("x.get::<u8>(1).put(2);"),
            vec!["x.get", "x.get().put"]
        );
        assert_eq!(
            call_names("self\n            .services // the services\n            .store\n            .put(\"c\");"),
            vec!["self.services.store.put"]
        );
        assert_eq!(call_names("self.store.put(\"d\");"), vec!["self.store.put"]);
        assert_eq!(call_names("x.put(1);"), vec!["x.put"]);
        assert_eq!(call_names("self.put(1);"), vec!["self.put"]);
        assert_eq!(
            call_names("Foo::new().a().b();"),
            vec!["Foo::new", "Foo::new().a", "b"]
        );
    }

    #[test]
    fn extraction_stops_at_parentheses_question_marks_await_and_sixteen_links() {
        assert_eq!(call_names("(x).a.b();"), vec!["b"]);
        assert_eq!(call_names("self.a?.b();"), vec!["b"]);
        assert_eq!(call_names("self.a.b().await.c();"), vec!["c", "self.a.b"]);
        let links: Vec<String> = (0..17).map(|i| format!("l{i}()")).collect();
        let names = call_names(&format!("x.{}.m();", links.join(".")));
        assert_eq!(names.len(), 18, "{names:?}");
        assert!(names.contains(&"x.l0".to_string()));
        assert!(
            names.contains(&format!("x.{}.l16", links[..16].join("."))),
            "{names:?}"
        );
        assert!(names.contains(&"m".to_string()));
    }

    #[test]
    fn waits_under_its_name_and_resolves_once_a_sync_adds_the_method() {
        let mut g = Graph::new(&[
            ("Cargo.toml", &crate_toml("later", "")),
            (
                "src/lib.rs",
                r#"pub mod disk;

pub struct Services { pub disk: disk::Disk }
pub struct Svc { pub services: Services }

impl Svc {
    pub fn save(&self) { self.services.disk.flush(); }
}
"#,
            ),
            ("src/disk.rs", "pub struct Disk;\n"),
        ]);
        let parked: Vec<String> = g
            .store
            .refs_in_file("src/lib.rs")
            .unwrap()
            .into_iter()
            .filter(|r| r.reference_kind.as_str() == "calls")
            .map(|r| r.reference_name)
            .collect();
        assert_eq!(parked, vec!["self.services.disk.flush"]);
        g.write(
            "src/disk.rs",
            "pub struct Disk;\n\nimpl Disk {\n    pub fn flush(&self) {}\n}\n",
        );
        g.sync();
        let save = g
            .in_file("src/lib.rs")
            .into_iter()
            .find(|n| n.name == "save")
            .unwrap();
        let calls: Vec<String> = g
            .out(&[&save], &["calls"])
            .into_iter()
            .map(|(e, t)| {
                let r = e
                    .metadata
                    .as_ref()
                    .and_then(|m| m["refName"].as_str())
                    .unwrap_or("undefined")
                    .to_string();
                format!("{} {r}", t.qualified_name)
            })
            .collect();
        assert_eq!(calls, vec!["Disk::flush self.services.disk.flush"]);
    }
}

/// rust-route-handler-line.test.ts: a route's handler reference sits on the
/// line its handler is written on.
mod route_handler_line {
    use super::*;

    const MAIN_RS: &str = r#"mod actix_app;
mod handlers;
mod keys;
use axum::{routing::{delete, get, post}, Router};
use crate::handlers::used_handler;

pub fn app() -> Router {
    Router::new()
        .route("/single", get(handlers::single_line_handler))
        .route(
            "/multi",
            get(handlers::multi_line_handler),
        )
        .route(
            "/deep",
            get(
                handlers::deep_line_handler,
            ),
        )
        .route(
            "/items",
            get(handlers::list_items)
                .post(handlers::create_item)
                .delete(handlers::delete_item),
        )
        .route(
            "/used",
            post(used_handler),
        )
        .route(
            "/other",
            get(metrics::render_handler),
        )
        .route("/login", post(handlers::login))
        .route("/account", delete(handlers::delete))
        .route("/closure-inline", get(|| async { cache().get(keys::session_key) }))
        .route(
            "/closure",
            get(|| async move {
                cache().get(keys::session_key)
            }),
        )
}
"#;

    const ACTIX_RS: &str = r#"use actix_web::{web, App};
use crate::handlers;

pub fn actix() {
    App::new()
        .service(
            web::resource("/index/with/a/rather/long/path/{id}")
                .to(handlers::actix_index),
        )
        .service(
            web::resource("/user/{id}")
                .route(
                    web::get()
                        .to(handlers::actix_get_user),
                ),
        )
        .route(
            "/hey",
            web::get().to(handlers::actix_hello),
        );
}
"#;

    fn graph() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                "[package]\nname = \"repro\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\naxum = \"0.7\"\nactix-web = \"4\"\nmetrics = \"0.23\"\n",
            ),
            ("src/main.rs", MAIN_RS),
            ("src/actix_app.rs", ACTIX_RS),
            (
                "src/handlers/mod.rs",
                r#"pub async fn single_line_handler() {}
pub async fn multi_line_handler() {}
pub async fn deep_line_handler() {}
pub async fn list_items() {}
pub async fn create_item() {}
pub async fn delete_item() {}
pub async fn used_handler() {}
pub async fn render_handler() {}
pub async fn login() {}
pub async fn delete() {}
pub async fn actix_index() {}
pub async fn actix_get_user() {}
pub async fn actix_hello() {}
"#,
            ),
            ("src/keys.rs", "pub fn session_key() -> &'static str { \"s\" }\n"),
        ])
    }

    /// `METHOD /path -> file:handler` for every route → handler `references` edge.
    fn route_edges(g: &Graph, file: &str) -> Vec<String> {
        let routes: Vec<_> = g
            .in_file(file)
            .into_iter()
            .filter(|n| n.kind.as_str() == "route")
            .collect();
        let refs: Vec<_> = routes.iter().collect();
        let mut v: Vec<String> = g
            .out(&refs, &["references"])
            .into_iter()
            .map(|(e, t)| format!("{} -> {}:{}", g.node(&e.source).name, t.file_path, t.name))
            .collect();
        v.sort();
        v
    }

    fn has(v: &[String], s: &str) {
        assert!(v.iter().any(|x| x == s), "{s} not in {v:?}");
    }

    #[test]
    fn axum_wrapped_handlers() {
        let g = graph();
        let e = route_edges(&g, "src/main.rs");
        let h = "src/handlers/mod.rs";
        has(&e, &format!("GET /single -> {h}:single_line_handler"));
        has(&e, &format!("GET /multi -> {h}:multi_line_handler"));
        has(&e, &format!("GET /deep -> {h}:deep_line_handler"));
        has(&e, &format!("GET /items -> {h}:list_items"));
        has(&e, &format!("POST /items -> {h}:create_item"));
        has(&e, &format!("DELETE /items -> {h}:delete_item"));
        has(&e, &format!("POST /used -> {h}:used_handler"));
        has(&e, &format!("POST /login -> {h}:login"));
        has(&e, &format!("DELETE /account -> {h}:delete"));
        assert!(!e.iter().any(|x| x.contains("render_handler")), "{e:?}");
    }

    #[test]
    fn axum_a_call_inside_a_closure_handler_is_no_route() {
        let g = graph();
        let e = route_edges(&g, "src/main.rs");
        assert!(!e.iter().any(|x| x.contains("closure")), "{e:?}");
        let routes: Vec<String> = g
            .in_file("src/main.rs")
            .into_iter()
            .filter(|n| n.kind.as_str() == "route" && n.name.contains("closure"))
            .map(|n| n.name)
            .collect();
        assert_eq!(routes, Vec::<String>::new());
    }

    #[test]
    fn the_route_stays_on_the_route_line_the_reference_on_the_handler() {
        let g = graph();
        let lines: Vec<&str> = MAIN_RS.lines().collect();
        let line_of = |t: &str| lines.iter().position(|l| l.contains(t)).unwrap() as u32 + 1;
        let multi = g
            .in_file("src/main.rs")
            .into_iter()
            .find(|n| n.name == "GET /multi")
            .unwrap();
        assert_eq!(multi.start_line, line_of("\"/multi\"") - 1);
        let (e, _) = g
            .out(&[&multi], &["references"])
            .into_iter()
            .next()
            .expect("handler edge");
        assert_eq!(e.line, Some(line_of("handlers::multi_line_handler")));
        let single = g
            .in_file("src/main.rs")
            .into_iter()
            .find(|n| n.name == "GET /single")
            .unwrap();
        let (e, _) = g
            .out(&[&single], &["references"])
            .into_iter()
            .next()
            .expect("handler edge");
        assert_eq!(e.line, Some(single.start_line));
    }

    #[test]
    fn actix_wrapped_handlers() {
        let g = graph();
        let e = route_edges(&g, "src/actix_app.rs");
        let h = "src/handlers/mod.rs";
        has(
            &e,
            &format!("ANY /index/with/a/rather/long/path/{{id}} -> {h}:actix_index"),
        );
        has(&e, &format!("GET /user/{{id}} -> {h}:actix_get_user"));
        has(&e, &format!("GET /hey -> {h}:actix_hello"));
        let user: Vec<&String> = e
            .iter()
            .filter(|x| x.starts_with("GET /user/{id}"))
            .collect();
        assert_eq!(
            user,
            vec![&format!("GET /user/{{id}} -> {h}:actix_get_user")]
        );
    }
}

/// rust-self-field-types.test.ts: a call on a field of `self` is a call on
/// the type the struct declares for the field.
mod self_field_types {
    use super::*;

    const CONFIG: &str = "pub struct Config;\n\nimpl Config {\n    pub fn load(&self) {}\n}\n";

    fn graph() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nsemverlib = \"1\"\n",
            ),
            (
                "src/main.rs",
                "mod config_a;\nmod config_b;\nmod model;\nmod proc;\nmod range;\nmod use_range;\nmod ver;\nmod worker;\n\nfn main() {}\n",
            ),
            ("src/range.rs", "/// The project's own `Range`, a namesake of the dependency's.\npub struct Range;\n"),
            (
                "src/ver.rs",
                "pub trait Lowest {\n    fn lowest(&self) -> u32;\n}\n\nimpl Lowest for semverlib::Range {\n    fn lowest(&self) -> u32 {\n        0\n    }\n}\n",
            ),
            (
                "src/use_range.rs",
                "use semverlib::Range;\n\nuse crate::ver::Lowest;\n\npub fn check(r: Range) -> u32 {\n    r.lowest()\n}\n",
            ),
            (
                "src/proc.rs",
                r#"pub trait Kill {
    fn kill(&mut self) -> bool;
}

/// The project's own `Child`, holding std's.
pub struct Child {
    child: std::process::Child,
}

impl Kill for Child {
    fn kill(&mut self) -> bool {
        self.child.kill().is_ok()
    }
}
"#,
            ),
            ("src/config_a.rs", CONFIG),
            ("src/config_b.rs", CONFIG),
            (
                "src/model.rs",
                r#"pub trait Store {
    fn save(&self);
}

pub trait Render {
    fn render(&self);
}

pub struct State;

impl Clone for State {
    fn clone(&self) -> Self {
        State
    }
}

pub struct Inner;

impl Inner {
    pub fn run(&self) {}
}

// Namesakes, which a guess by name alone may take.
pub struct Decoy;

impl Decoy {
    pub fn save(&self) {}
    pub fn run(&self) {}
}
"#,
            ),
            (
                "src/worker.rs",
                r#"use std::sync::Arc;

use crate::config_b::Config;
use crate::model::{Inner, Render, State, Store};

pub struct Worker<S: Store> {
    store: S,
    shared: Arc<State>,
    cfg: Config,
    view: Box<dyn Render>,
}

impl<S: Store> Worker<S> {
    pub fn tick(&self) {
        self.store.save();
        let _ = self.shared.clone();
        self.cfg.load();
        self.view.render();
    }
}
"#,
            ),
        ])
    }

    #[test]
    fn the_method_of_the_type_the_struct_declares_for_the_field() {
        let g = graph();
        assert_eq!(
            calls_from(&g, "src/worker.rs", "tick"),
            vec!["src/config_b.rs@4", "src/model.rs@2", "src/model.rs@6"]
        );
    }

    #[test]
    fn no_project_impl_for_a_project_type_sharing_a_std_name() {
        let g = graph();
        let imp = g
            .in_file("src/proc.rs")
            .into_iter()
            .find(|n| n.name == "kill" && n.start_line == 11)
            .unwrap();
        assert!(g.out(&[&imp], &["calls"]).is_empty());
    }

    #[test]
    fn a_project_traits_impl_for_a_dependency_type() {
        let g = graph();
        assert_eq!(
            calls_from(&g, "src/use_range.rs", "check"),
            vec!["src/ver.rs@6"]
        );
    }
}

/// rust-self-path-calls.test.ts: `Self::x(…)` is `x` of the type `Self` is
/// where it is written.
mod self_path_calls {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            ("Cargo.toml", "[package]\nname = \"cli\"\nversion = \"0.1.0\"\n"),
            (
                "src/main.rs",
                "mod android;\nmod bound;\nmod config;\nmod error;\nmod frame;\nmod listener;\nmod message;\nmod parse;\nmod parser;\nmod store;\nmod workspace;\n\nfn main() {}\n",
            ),
            (
                "src/error.rs",
                r#"pub struct Error {
    kind: u8,
}

impl Error {
    pub fn new(kind: u8) -> Self {
        Error { kind }
    }

    pub fn io() -> Self {
        Self::new(1)
    }
}
"#,
            ),
            (
                "src/config.rs",
                r#"use crate::store::Store;

pub struct Config;

impl Config {
    pub fn new() -> Self {
        Config
    }
}

impl Store for Config {
    fn open() -> Self {
        Self::new()
    }
}
"#,
            ),
            (
                "src/store.rs",
                r#"pub trait Store {
    fn open() -> Self;

    fn open_default() -> Self
    where
        Self: Sized,
    {
        Self::open()
    }
}

pub fn open() {}
"#,
            ),
            (
                "src/message.rs",
                r#"pub enum Message {
    Text(String),
    Binary(Vec<u8>),
}

impl Message {
    pub fn text(s: &str) -> Self {
        Self::Text(s.to_string())
    }
}
"#,
            ),
            ("src/frame.rs", "pub enum Frame {\n    Text(String),\n}\n"),
            (
                "src/workspace.rs",
                r#"pub struct Workspace;

impl Workspace {
    pub fn current() -> Workspace {
        Workspace
    }

    pub fn reload() -> Workspace {
        Self::current()
    }

    pub fn depth(n: u32) -> u32 {
        if n == 0 { 0 } else { Self::depth(n - 1) + 1 }
    }
}
"#,
            ),
            (
                "src/android.rs",
                "pub struct AndroidTools;\n\nimpl AndroidTools {\n    pub fn current() -> Option<AndroidTools> {\n        None\n    }\n}\n",
            ),
            (
                "src/parse.rs",
                r#"pub trait Parser {
    fn parse() -> Self;
}

impl<T: Parser> Parser for Box<T> {
    fn parse() -> Self {
        Self::new(T::parse())
    }
}
"#,
            ),
            (
                "src/parser.rs",
                "pub struct Parser;\n\nimpl Parser {\n    pub fn new() -> Self {\n        Parser\n    }\n}\n",
            ),
            (
                "src/bound.rs",
                r#"use std::net::{SocketAddr, TcpListener};

pub trait Bound {
    fn local_addr(&self) -> std::io::Result<SocketAddr>;
}

impl Bound for TcpListener {
    fn local_addr(&self) -> std::io::Result<SocketAddr> {
        Self::local_addr(self)
    }
}
"#,
            ),
            (
                "src/listener.rs",
                r#"pub trait Listener {
    fn local_addr(&self) -> u16;
}

pub struct TcpListener;

impl TcpListener {
    pub fn local_addr(&self) -> u16 {
        0
    }
}

impl Listener for TcpListener {
    fn local_addr(&self) -> u16 {
        Self::local_addr(self)
    }
}
"#,
            ),
        ])
    }

    fn calls_at_decl(g: &Graph, file: &str, f: &str, line: u32) -> Vec<String> {
        let froms: Vec<_> = g
            .in_file(file)
            .into_iter()
            .filter(|n| n.name == f && n.start_line == line)
            .collect();
        let refs: Vec<_> = froms.iter().collect();
        let mut v: Vec<String> = g
            .out(&refs, &["calls"])
            .iter()
            .map(|(_, t)| format!("{}@{}", t.file_path, t.start_line))
            .collect();
        v.sort();
        v.dedup();
        v
    }

    #[test]
    fn the_impls_own_types_function() {
        let g = graph();
        assert_eq!(calls_from(&g, "src/error.rs", "io"), vec!["src/error.rs@6"]);
        assert_eq!(
            calls_from(&g, "src/workspace.rs", "reload"),
            vec!["src/workspace.rs@4"]
        );
        assert_eq!(
            calls_from(&g, "src/workspace.rs", "depth"),
            vec!["src/workspace.rs@12"]
        );
    }

    #[test]
    fn a_tuple_variant_built_through_self() {
        let g = graph();
        assert_eq!(
            calls_from(&g, "src/message.rs", "text"),
            vec!["src/message.rs@2"]
        );
    }

    #[test]
    fn the_types_own_in_a_trait_impl_the_traits_in_its_body() {
        let g = graph();
        assert_eq!(
            calls_from(&g, "src/config.rs", "open"),
            vec!["src/config.rs@6"]
        );
        assert_eq!(
            calls_from(&g, "src/store.rs", "open_default"),
            vec!["src/store.rs@2"]
        );
    }

    #[test]
    fn never_another_type_sharing_the_traits_name() {
        let g = graph();
        let t: Vec<String> = calls_from(&g, "src/parse.rs", "parse")
            .into_iter()
            .filter(|t| t.starts_with("src/parser.rs"))
            .collect();
        assert_eq!(t, Vec::<String>::new());
    }

    #[test]
    fn the_types_own_function_first_never_the_trait_method_written_in() {
        let g = graph();
        assert_eq!(
            calls_at_decl(&g, "src/listener.rs", "local_addr", 14),
            vec!["src/listener.rs@8"]
        );
        assert_eq!(
            calls_at_decl(&g, "src/bound.rs", "local_addr", 8),
            Vec::<String>::new()
        );
    }
}

/// rust-use-path-module.test.ts: a name a `use` binds is the item of that
/// name in the module its path walks to.
mod use_path_module {
    use super::*;

    fn tokio() -> Graph {
        Graph::new(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"tokio\", \"tokio-stream\"]\n"),
            ("tokio/Cargo.toml", "[package]\nname = \"tokio\"\nversion = \"0.1.0\"\n"),
            ("tokio/src/lib.rs", "pub mod fs;\npub mod net;\npub mod signal;\npub mod sync;\nmod loom;\n"),
            ("tokio/src/fs/mod.rs", "pub struct Ready;\n\npub struct Notified;\n"),
            ("tokio/src/loom/mod.rs", "pub(crate) struct Mutex<T>(T);\n"),
            ("tokio/src/net/mod.rs", "pub mod unix;\n"),
            ("tokio/src/net/unix/mod.rs", "pub mod pipe;\n"),
            (
                "tokio/src/net/unix/pipe.rs",
                r#"pub struct Receiver {
    fd: i32,
}

pub struct Sender {
    fd: i32,
}

pub fn channel() -> Receiver {
    Receiver { fd: 0 }
}
"#,
            ),
            (
                "tokio/src/sync/mod.rs",
                r#"macro_rules! cfg_sync {
    ($($item:item)*) => { $( $item )* }
}

cfg_sync! {
    mod mutex;
    pub use mutex::{Mutex, MutexGuard};

    pub(crate) mod notify;

    pub mod broadcast;
    pub mod watch;
}

pub mod futures {
    pub use super::notify::Notified;

    pub struct Ready;
}
"#,
            ),
            ("tokio/src/sync/mutex.rs", "pub struct Mutex<T> {\n    value: T,\n}\n\npub struct MutexGuard;\n"),
            ("tokio/src/sync/notify.rs", "pub struct Notified;\n"),
            ("tokio/src/sync/broadcast.rs", "use super::Mutex;\n\npub fn tail(lock: Mutex<u64>) {}\n"),
            (
                "tokio/src/sync/watch.rs",
                r#"macro_rules! sender {
    () => { pub struct Sender; }
}

sender!();

pub struct Receiver<T> {
    shared: T,
}

pub fn channel<T>(init: T) -> Receiver<T> {
    Receiver { shared: init }
}
"#,
            ),
            (
                "tokio/src/signal/mod.rs",
                r#"use crate::sync::futures::{Notified, Ready};
use crate::sync::watch::{channel, Receiver, Sender};
use crate::sync::Mutex;
use crate::gen::Thing;

pub fn make(lock: Mutex<u8>) -> Receiver<()> {
    channel(())
}

pub fn send(tx: Sender) {}

pub fn wait(n: Notified, r: Ready, t: Thing) {}
"#,
            ),
            ("tokio/src/signal/thing.rs", "pub struct Thing;\n"),
            (
                "tokio/tests/sync_watch.rs",
                "use tokio::sync::watch::Receiver;\n\nfn rx(r: Receiver<u8>) -> Receiver<u8> {\n    r\n}\n",
            ),
            (
                "tokio-stream/Cargo.toml",
                "[package]\nname = \"tokio-stream\"\nversion = \"0.1.0\"\n\n[dependencies]\ntokio = { path = \"../tokio\" }\n",
            ),
            ("tokio-stream/src/lib.rs", "pub mod wrappers;\n"),
            ("tokio-stream/src/wrappers/mod.rs", "mod watch;\npub use watch::stream;\n"),
            (
                "tokio-stream/src/wrappers/watch.rs",
                "use tokio::sync::watch::Receiver;\n\npub fn stream<T>(rx: Receiver<T>) -> Receiver<T> {\n    rx\n}\n",
            ),
        ])
    }

    const SIGNAL: &str = "tokio/src/signal/mod.rs";
    const REFS: &[&str] = &["references"];

    #[test]
    fn a_use_is_the_item_of_its_paths_module() {
        let g = tokio();
        let watch = vec!["tokio/src/sync/watch.rs@7"];
        assert_eq!(g.targets_of(SIGNAL, "Receiver", REFS), watch);
        assert_eq!(
            g.targets_of("tokio/tests/sync_watch.rs", "Receiver", REFS),
            watch
        );
        assert_eq!(
            g.targets_of("tokio-stream/src/wrappers/watch.rs", "Receiver", REFS),
            watch
        );
    }

    #[test]
    fn re_exports_super_inline_modules_and_calls() {
        let g = tokio();
        let mutex = vec!["tokio/src/sync/mutex.rs@1"];
        assert_eq!(
            g.targets_of(SIGNAL, "Mutex", REFS),
            mutex,
            "re-export inside a macro"
        );
        assert_eq!(
            g.targets_of("tokio/src/sync/broadcast.rs", "Mutex", REFS),
            mutex,
            "use super::"
        );
        assert_eq!(
            g.targets_of(SIGNAL, "Ready", REFS),
            vec!["tokio/src/sync/mod.rs@18"]
        );
        assert_eq!(
            g.targets_of(SIGNAL, "Notified", REFS),
            vec!["tokio/src/sync/notify.rs@1"]
        );
        assert_eq!(
            g.targets_of(SIGNAL, "channel", &["calls"]),
            vec!["tokio/src/sync/watch.rs@11"]
        );
    }

    #[test]
    fn nothing_for_an_unknown_item_name_matching_without_a_module_file() {
        let g = tokio();
        assert_eq!(g.targets_of(SIGNAL, "Sender", REFS), Vec::<String>::new());
        assert_eq!(
            g.targets_of(SIGNAL, "Thing", REFS),
            vec!["tokio/src/signal/thing.rs@1"]
        );
    }

    #[test]
    fn reads_an_edited_use_afresh_on_sync() {
        let mut g = tokio();
        g.write(
            "tokio/src/sync/broadcast.rs",
            "use crate::loom::Mutex;\n\npub fn tail(lock: Mutex<u64>) {}\n",
        );
        g.sync();
        assert_eq!(
            g.targets_of("tokio/src/sync/broadcast.rs", "Mutex", REFS),
            vec!["tokio/src/loom/mod.rs@1"]
        );
    }

    fn namespaces() -> Graph {
        Graph::new(&[
            ("Cargo.toml", "[workspace]\nmembers = [\"app\", \"clap\", \"clap_builder\", \"clap_derive\"]\n"),
            (
                "clap/Cargo.toml",
                "[package]\nname = \"clap\"\nversion = \"0.1.0\"\n\n[dependencies]\nclap_builder = { path = \"../clap_builder\" }\nclap_derive = { path = \"../clap_derive\" }\n",
            ),
            (
                "clap/src/lib.rs",
                "pub use clap_builder::*;\n\n#[cfg(feature = \"derive\")]\npub use clap_derive::{Parser, ValueEnum};\n",
            ),
            ("clap_builder/Cargo.toml", "[package]\nname = \"clap_builder\"\nversion = \"0.1.0\"\n"),
            ("clap_builder/src/lib.rs", "mod derive;\n\npub use crate::derive::{Parser, ValueEnum};\n"),
            ("clap_builder/src/derive.rs", "pub trait Parser {}\n\npub trait ValueEnum {}\n"),
            (
                "clap_derive/Cargo.toml",
                "[package]\nname = \"clap_derive\"\nversion = \"0.1.0\"\n\n[lib]\nproc-macro = true\n",
            ),
            (
                "clap_derive/src/lib.rs",
                "use proc_macro::TokenStream;\n\n#[proc_macro_derive(ValueEnum)]\npub fn value_enum(input: TokenStream) -> TokenStream {\n    input\n}\n",
            ),
            (
                "app/Cargo.toml",
                "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nclap = { path = \"../clap\" }\nglobal-hotkey = \"0.6\"\n",
            ),
            ("app/src/lib.rs", "mod desktop;\nmod mobile_shortcut;\nmod shell;\nmod shortcut;\nmod sys;\nmod user;\n"),
            (
                "app/src/shell.rs",
                "use clap::ValueEnum;\n\npub enum Shell {\n    Bash,\n}\n\nimpl ValueEnum for Shell {}\n",
            ),
            (
                "app/src/shortcut.rs",
                "#[cfg(not(target_os = \"ios\"))]\npub use global_hotkey::HotKey;\n\n#[cfg(target_os = \"ios\")]\npub use crate::mobile_shortcut::*;\n",
            ),
            ("app/src/mobile_shortcut.rs", "pub struct HotKey;\n"),
            ("app/src/desktop.rs", "use crate::shortcut::HotKey;\n\npub fn register(key: HotKey) {}\n"),
            (
                "app/src/sys/mod.rs",
                "#[cfg(target_os = \"ios\")]\n#[path = \"ios.rs\"]\nmod imp;\n\n#[cfg(not(target_os = \"ios\"))]\nmod imp;\n\npub use imp::Handle;\n",
            ),
            ("app/src/sys/imp.rs", "pub use std::fs::File as Handle;\n"),
            ("app/src/sys/ios.rs", "pub struct Handle;\n"),
            ("app/src/user.rs", "use crate::sys::Handle;\n\npub fn open(handle: Handle) {}\n"),
        ])
    }

    #[test]
    fn names_with_more_than_one_meaning() {
        let g = namespaces();
        assert_eq!(
            g.targets_of("app/src/shell.rs", "ValueEnum", &["implements"]),
            vec!["clap_builder/src/derive.rs@3"]
        );
        assert_eq!(
            g.targets_of("app/src/desktop.rs", "HotKey", REFS),
            vec!["app/src/mobile_shortcut.rs@1"]
        );
        assert_eq!(
            g.targets_of("app/src/user.rs", "Handle", REFS),
            vec!["app/src/sys/ios.rs@1"]
        );
    }
}

/// rust-typed-receivers.test.ts: a method call on a local or parameter is a
/// method of the type that name is declared with where the call is.
mod typed_receivers {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                r##"[package]
name = "app"
version = "0.1.0"

[dependencies]
netlib = "1"
"##,
            ),
            (
                "src/main.rs",
                r##"mod app;
mod ast;
mod canvas;
mod cli;
mod complete;
mod disk;
mod doc;
mod doc_use;
mod fallback;
mod flags;
mod form;
mod http;
mod io;
mod layer;
mod listener;
mod net;
mod once;
mod overrides;
mod paint;
mod plan;
mod render;
mod report;
mod request;
mod runner;
mod sched;
mod serve;
mod server;
mod settings;
mod shadow;
mod start;
mod store;
mod sync;
mod use_app;
mod use_net;
mod use_sync;
mod widget;
mod work;

fn main() {}
"##,
            ),
            (
                "src/once.rs",
                r##"use std::sync::Arc;

pub struct SetOnce;

impl SetOnce {
    pub fn new() -> Self {
        SetOnce
    }

    pub fn set(&self, v: u8) {}
}

pub fn fill() {
    let once = Arc::new(SetOnce::new());
    once.set(1);
}
"##,
            ),
            (
                "src/form.rs",
                r##"use std::rc::Rc;

pub struct FormData;
impl FormData {
    pub fn downcast(&self) -> Option<&u8> {
        None
    }
}

pub fn get(data: Rc<FormData>) {
    data.downcast();
}
"##,
            ),
            (
                "src/sched.rs",
                r##"use std::sync::Arc;

pub trait Schedule {
    fn schedule(&self, task: u32);
}

pub struct Handle;

impl Handle {
    pub fn spawn(&self) {}
}

impl Schedule for Arc<Handle> {
    fn schedule(&self, task: u32) {}
}

pub fn run(handle: Arc<Handle>) {
    handle.schedule(1);
    handle.spawn();
}
"##,
            ),
            (
                "src/flags.rs",
                r##"pub struct ContextSeparator;
impl ContextSeparator {
    pub fn into_bytes(self) {}
}
pub struct FieldSeparator;
impl FieldSeparator {
    pub fn into_bytes(self) {}
}

pub fn context() {
    use crate::flags::ContextSeparator as Separator;
    let bytes = |sep: Separator| sep.into_bytes();
}

pub fn field() {
    use crate::flags::FieldSeparator as Separator;
    let bytes = |sep: Separator| sep.into_bytes();
}
"##,
            ),
            (
                "src/doc.rs",
                r##"pub trait Documentable {
    fn to_doc(self) -> u8;
}

impl Documentable for netlib::EcoString {
    fn to_doc(self) -> u8 {
        0
    }
}

impl Documentable for &netlib::EcoString {
    fn to_doc(self) -> u8 {
        1
    }
}
"##,
            ),
            (
                "src/doc_use.rs",
                r##"use crate::doc::Documentable;
use netlib::EcoString;

pub fn by_ref(string: &EcoString) {
    string.to_doc();
}

pub fn by_value(string: EcoString) {
    string.to_doc();
}
"##,
            ),
            (
                "src/runner.rs",
                r##"pub enum Outcome {
    Ignore { note: Option<String> },
    Rebuild,
}

impl Outcome {
    pub fn escalate(self, other: Outcome) -> Outcome {
        other
    }
}

pub fn decide() {
    let mut result = Outcome::Ignore { note: None };
    result = result.escalate(Outcome::Rebuild);
}
"##,
            ),
            (
                "src/plan.rs",
                r##"pub struct Plan;

impl Plan {
    pub fn escalate(&self) {}
}
"##,
            ),
            (
                "src/ast.rs",
                r##"pub struct Statement<T> {
    pub value: T,
}

pub type TypedStatement = Statement<u32>;
pub type UntypedStatement = Statement<String>;

impl TypedStatement {
    pub fn location(&self) {}
}

impl UntypedStatement {
    pub fn location(&self) {}
}

pub fn visit(statement: &TypedStatement) {
    statement.location();
}

impl TypedStatement {
    pub fn type_(&self) {}
}

pub enum Node<T> {
    Leaf(Statement<T>),
}

pub fn typed(node: &Node<u32>) {
    match node {
        Node::Leaf(statement) => statement.type_(),
    }
}
"##,
            ),
            (
                "src/io.rs",
                r##"pub trait HttpClient {
    fn send(&self);
}
"##,
            ),
            (
                "src/http.rs",
                r##"pub struct HttpClient;

impl HttpClient {
    pub fn new() -> Self {
        HttpClient
    }
}

impl crate::io::HttpClient for HttpClient {
    fn send(&self) {}
}

pub fn fetch() {
    let http = HttpClient::new();
    http.send();
}
"##,
            ),
            (
                "src/sync/mod.rs",
                r##"pub mod batch;
pub mod semaphore;

pub use semaphore::Semaphore;
"##,
            ),
            (
                "src/sync/semaphore.rs",
                r##"pub struct Semaphore;

impl Semaphore {
    pub fn new() -> Self {
        Semaphore
    }

    pub fn try_acquire_owned(&self) {}
}
"##,
            ),
            (
                "src/sync/batch.rs",
                r##"pub struct Semaphore;

impl Semaphore {
    pub fn new() -> Self {
        Semaphore
    }
}
"##,
            ),
            (
                "src/use_sync.rs",
                r##"use std::sync::Arc;

use crate::sync::Semaphore;

pub fn take() {
    let sem = Arc::new(Semaphore::new());
    sem.try_acquire_owned();
}
"##,
            ),
            (
                "src/listener.rs",
                r##"pub trait Listener {
    fn local_addr(&self) -> u16;
}

impl Listener for netlib::TcpListener {
    fn local_addr(&self) -> u16 {
        Self::local_addr(self)
    }
}
"##,
            ),
            (
                "src/serve.rs",
                r##"use crate::listener::Listener;
use netlib::TcpListener;

pub fn addr(listener: TcpListener) {
    listener.local_addr();
}
"##,
            ),
            (
                "src/complete.rs",
                r##"pub struct ScratchRuntime;

impl netlib::Runtime for ScratchRuntime {
    fn complete(&self) {}
}

pub fn load_runtime() -> Box<dyn netlib::Runtime> {
    Box::new(ScratchRuntime)
}

pub fn check() {
    let runtime = load_runtime();
    runtime.complete();
}
"##,
            ),
            (
                "src/cli.rs",
                r##"use crate::overrides::Overrides;

pub enum Commands {
    Build(Overrides<BuildArgs>),
    Tools(Tools),
}

pub enum Tools {
    Assets(AssetsArgs),
}

pub struct BuildArgs;

pub struct AssetsArgs;

impl AssetsArgs {
    pub fn run(self) {}
}

pub fn dispatch(args: Commands) {
    match args {
        Commands::Build(opts) => opts.build(),
        Commands::Tools(Tools::Assets(opts)) => opts.run(),
    }
}
"##,
            ),
            (
                "src/overrides.rs",
                r##"use crate::cli::BuildArgs;

pub struct Overrides<T> {
    pub inner: T,
}

impl Overrides<BuildArgs> {
    pub fn build(self) {}
}
"##,
            ),
            (
                "src/request.rs",
                r##"pub struct BuildRequest;

impl BuildRequest {
    pub fn build(&self) {}
}
"##,
            ),
            (
                "src/server.rs",
                r##"pub struct Server;

impl Server {
    pub fn new() -> Self {
        Server
    }

    pub fn run(&self) {}
}
"##,
            ),
            (
                "src/work.rs",
                r##"pub struct Worker;

impl Worker {
    pub fn run(&self) {}
}
"##,
            ),
            (
                "src/start.rs",
                r##"use std::rc::Rc;
use std::sync::Arc;

use crate::server::Server;
use crate::work::Worker;

pub fn shared(server: Arc<Server>) {
    server.run();
}

pub fn boxed(worker: Box<Worker>) {
    worker.run();
}

pub fn counted(worker: &mut Rc<Worker>) {
    worker.run();
}

pub fn borrowed<'a>(srv: &'a Server) {
    srv.run();
}

pub fn pathed(w: crate::work::Worker) {
    w.run();
}

pub fn built() {
    let srv = Arc::new(Server::new());
    srv.run();
}

pub fn annotated() {
    let srv: Rc<Server> = Rc::new(Server::new());
    srv.run();
}
"##,
            ),
            (
                "src/app/mod.rs",
                r##"pub mod config;
"##,
            ),
            (
                "src/app/config.rs",
                r##"pub struct Config;

impl Config {
    pub fn load(&self) {}
}
"##,
            ),
            (
                "src/net/mod.rs",
                r##"pub mod config;
"##,
            ),
            (
                "src/net/config.rs",
                r##"pub struct Config;

impl Config {
    pub fn load(&self) {}
}
"##,
            ),
            (
                "src/use_app.rs",
                r##"use crate::app::config::Config;

pub fn read(cfg: Config) {
    cfg.load();
}
"##,
            ),
            (
                "src/use_net.rs",
                r##"use crate::net::config::Config;

pub fn read(cfg: &Config) {
    cfg.load();
}
"##,
            ),
            (
                "src/render.rs",
                r##"pub trait Render {
    fn draw(&self);

    fn describe(&self) -> String {
        String::new()
    }
}
"##,
            ),
            (
                "src/paint.rs",
                r##"pub trait Paint {
    fn draw(&self);
}
"##,
            ),
            (
                "src/widget.rs",
                r##"use crate::paint::Paint;
use crate::render::Render;

pub struct Widget;

impl Paint for Widget {
    fn draw(&self) {}
}

impl Render for Widget {
    fn draw(&self) {}
}
"##,
            ),
            (
                "src/report.rs",
                r##"pub struct Report;

impl Report {
    pub fn describe(&self) -> String {
        String::new()
    }
}
"##,
            ),
            (
                "src/canvas.rs",
                r##"use crate::render::Render;
use crate::widget::Widget;

pub fn show(widget: &Widget) {
    widget.draw();
    widget.describe();
}

pub fn paint_all(r: impl Render, d: &dyn Render, b: Box<dyn Render>) {
    r.describe();
    d.describe();
    b.describe();
}
"##,
            ),
            (
                "src/store.rs",
                r##"pub struct Store;

impl Store {
    pub fn sync_all(&self) {}
}
"##,
            ),
            (
                "src/disk.rs",
                r##"pub fn persist(file: std::fs::File) {
    file.sync_all();
}
"##,
            ),
            (
                "src/shadow.rs",
                r##"use crate::server::Server;
use crate::work::Worker;

fn spare() -> Option<Worker> {
    None
}

pub fn shadowed(srv: Server) {
    if let Some(srv) = spare() {
        srv.run();
    }
}
"##,
            ),
            (
                "src/settings.rs",
                r##"pub struct Settings;

impl Settings {
    pub fn combine(&self) {}

    pub fn merge(&self, other: &Self) {
        other.combine();
    }
}
"##,
            ),
            (
                "src/layer.rs",
                r##"pub struct Layer;

impl Layer {
    pub fn combine(&self) {}
}
"##,
            ),
            (
                "src/fallback.rs",
                r##"pub struct Engine;

impl Engine {
    pub fn ignite(&self) {}
}

fn engine() -> Engine {
    Engine
}

pub fn go() {
    let e = engine();
    e.ignite();
}
"##,
            ),
        ])
    }

    fn cf(g: &Graph, file: &str, f: &str) -> Vec<String> {
        calls_from(g, file, f)
    }

    #[test]
    fn match_arm_variant_fields() {
        let g = graph();
        assert_eq!(
            cf(&g, "src/cli.rs", "dispatch"),
            vec!["src/cli.rs@17", "src/overrides.rs@8"]
        );
    }

    #[test]
    fn through_references_box_rc_and_arc() {
        let g = graph();
        assert_eq!(cf(&g, "src/start.rs", "shared"), vec!["src/server.rs@8"]);
        assert_eq!(cf(&g, "src/start.rs", "boxed"), vec!["src/work.rs@4"]);
        assert_eq!(cf(&g, "src/start.rs", "counted"), vec!["src/work.rs@4"]);
        assert_eq!(cf(&g, "src/start.rs", "borrowed"), vec!["src/server.rs@8"]);
    }

    #[test]
    fn a_path_type_or_the_value_a_let_is_built_from() {
        let g = graph();
        assert_eq!(cf(&g, "src/start.rs", "pathed"), vec!["src/work.rs@4"]);
        assert_eq!(
            cf(&g, "src/start.rs", "built"),
            vec!["src/server.rs@4", "src/server.rs@8"]
        );
        assert_eq!(
            cf(&g, "src/start.rs", "annotated"),
            vec!["src/server.rs@4", "src/server.rs@8"]
        );
    }

    #[test]
    fn the_type_the_files_use_names() {
        let g = graph();
        assert_eq!(
            cf(&g, "src/use_app.rs", "read"),
            vec!["src/app/config.rs@4"]
        );
        assert_eq!(
            cf(&g, "src/use_net.rs", "read"),
            vec!["src/net/config.rs@4"]
        );
    }

    #[test]
    fn traits_in_scope_provided_methods_and_trait_objects() {
        let g = graph();
        assert_eq!(
            cf(&g, "src/canvas.rs", "show"),
            vec!["src/render.rs@4", "src/widget.rs@11"]
        );
        assert_eq!(
            cf(&g, "src/canvas.rs", "paint_all"),
            vec!["src/render.rs@4"]
        );
        assert_eq!(
            cf(&g, "src/settings.rs", "merge"),
            vec!["src/settings.rs@4"]
        );
    }

    #[test]
    fn outside_types_and_shadowing() {
        let g = graph();
        assert_eq!(cf(&g, "src/disk.rs", "persist"), Vec::<String>::new());
        let t: Vec<String> = cf(&g, "src/shadow.rs", "shadowed")
            .into_iter()
            .filter(|t| t.starts_with("src/server.rs"))
            .collect();
        assert_eq!(t, Vec::<String>::new());
    }

    #[test]
    fn name_strategies_where_no_declaration_writes_the_type() {
        let g = graph();
        assert_eq!(
            cf(&g, "src/fallback.rs", "go"),
            vec!["src/fallback.rs@4", "src/fallback.rs@7"]
        );
        assert_eq!(
            cf(&g, "src/complete.rs", "check"),
            vec!["src/complete.rs@4", "src/complete.rs@7"]
        );
    }

    #[test]
    fn struct_variant_literals_and_type_alias_impls() {
        let g = graph();
        assert_eq!(cf(&g, "src/runner.rs", "decide"), vec!["src/runner.rs@7"]);
        assert_eq!(cf(&g, "src/ast.rs", "visit"), vec!["src/ast.rs@9"]);
        assert_eq!(cf(&g, "src/ast.rs", "typed"), vec!["src/ast.rs@21"]);
    }

    #[test]
    fn impls_over_namesake_traits_and_the_one_type_with_the_method() {
        let g = graph();
        assert_eq!(
            cf(&g, "src/http.rs", "fetch"),
            vec!["src/http.rs@10", "src/http.rs@4"]
        );
        let t = cf(&g, "src/use_sync.rs", "take");
        assert!(t.contains(&"src/sync/semaphore.rs@8".to_string()), "{t:?}");
        assert_eq!(cf(&g, "src/serve.rs", "addr"), Vec::<String>::new());
    }

    #[test]
    fn arc_rc_and_impls_on_the_wrapper() {
        let g = graph();
        assert_eq!(
            cf(&g, "src/once.rs", "fill"),
            vec!["src/once.rs@10", "src/once.rs@6"]
        );
        assert_eq!(
            cf(&g, "src/sched.rs", "run"),
            vec!["src/sched.rs@10", "src/sched.rs@14"]
        );
        assert_eq!(cf(&g, "src/form.rs", "get"), vec!["src/form.rs@5"]);
    }

    #[test]
    fn block_uses_and_reference_impls() {
        let g = graph();
        assert_eq!(cf(&g, "src/flags.rs", "context"), vec!["src/flags.rs@3"]);
        assert_eq!(cf(&g, "src/flags.rs", "field"), vec!["src/flags.rs@7"]);
        assert_eq!(cf(&g, "src/doc_use.rs", "by_ref"), vec!["src/doc.rs@12"]);
        assert_eq!(cf(&g, "src/doc_use.rs", "by_value"), vec!["src/doc.rs@6"]);
    }
}

/// rust-wrapped-self-calls.test.ts: a method call reaches what Rust's method
/// lookup picks through wrappers, inherent-before-trait, fields and tuples.
mod wrapped_self_calls {
    use super::*;

    fn graph() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                r##"[package]
name = "rt"
version = "0.1.0"
"##,
            ),
            (
                "src/main.rs",
                r##"mod copy;
mod cursor;
mod erased;
mod file;
mod grid;
mod io;
mod link;
mod measure;
mod net;
mod runtime;
mod ser;
mod shape;
mod sleep;
mod tester;
mod wake;
mod waker;

fn main() {}
"##,
            ),
            (
                "src/erased.rs",
                r##"pub struct MakeErased {
    into_route: fn(u8) -> u8,
}

impl MakeErased {
    fn into_route(self: Box<Self>, state: u8) -> u8 {
        (self.into_route)(state)
    }

    fn call(self: Box<Self>, state: u8) -> u8 {
        self.into_route(state)
    }
}
"##,
            ),
            (
                "src/waker.rs",
                r##"use std::task::Waker;

trait WakerRef {
    fn wake(self);
}

impl WakerRef for Waker {
    fn wake(self) {
        self.wake();
    }
}

impl WakerRef for &Waker {
    fn wake(self) {
        self.wake_by_ref();
    }
}
"##,
            ),
            (
                "src/measure.rs",
                r##"use crate::grid::Grid;

pub trait Measure {
    fn len(&self) -> usize;
}

impl Measure for Grid {
    fn len(&self) -> usize {
        self.len()
    }
}
"##,
            ),
            (
                "src/tester.rs",
                r##"use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

pub struct Tester {
    done: bool,
}

impl Future for Tester {
    type Output = ();

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        if self.done {
            return Poll::Ready(());
        }
        self.done = true;
        self.poll(cx)
    }
}
"##,
            ),
            (
                "src/io.rs",
                r##"use std::pin::Pin;
use std::task::{Context, Poll};

pub trait AsyncWrite {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<usize>;
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()>;
    fn poll_write_vectored(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<usize> {
        self.poll_write(cx, buf)
    }
    fn is_write_vectored(&self) -> bool {
        false
    }
}

pub trait AsyncRead {
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<usize>;
}

impl<P> AsyncWrite for Pin<P>
where
    P: std::ops::DerefMut,
    P::Target: AsyncWrite,
{
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<usize> {
        Poll::Ready(buf.len())
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Poll::Ready(())
    }

    fn is_write_vectored(&self) -> bool {
        true
    }
}

impl<P> AsyncRead for Pin<P>
where
    P: std::ops::DerefMut,
    P::Target: AsyncRead,
{
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<usize> {
        Poll::Ready(0)
    }
}
"##,
            ),
            (
                "src/file.rs",
                r##"use std::pin::Pin;
use std::task::{Context, Poll};

use crate::io::AsyncWrite;

pub struct File {
    pending: usize,
}

impl File {
    pub fn get_mut(&mut self) -> &mut usize {
        &mut self.pending
    }

    fn complete(&mut self) -> usize {
        self.pending
    }
}

impl AsyncWrite for File {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<usize> {
        Poll::Ready(buf.len())
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let _ = self.complete();
        let me = self.get_mut();
        Poll::Ready(())
    }

    fn poll_write_vectored(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<usize> {
        self.poll_flush(cx);
        Poll::Ready(0)
    }
}
"##,
            ),
            (
                "src/cursor.rs",
                r##"use std::io::Cursor;
use std::pin::Pin;
use std::task::{Context, Poll};

use crate::io::AsyncWrite;

impl AsyncWrite for Cursor<Vec<u8>> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<usize> {
        self.poll_flush(cx);
        Poll::Ready(0)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Poll::Ready(())
    }
}

impl AsyncWrite for Cursor<Box<[u8]>> {
    fn poll_write(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<usize> {
        self.poll_flush(cx);
        Poll::Ready(0)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        Poll::Ready(())
    }
}
"##,
            ),
            (
                "src/copy.rs",
                r##"use std::pin::Pin;
use std::task::{Context, Poll};

use crate::file::File;
use crate::io::{AsyncRead, AsyncWrite};

pub fn read_some<R: AsyncRead + ?Sized>(read: Pin<&mut R>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<usize> {
    read.poll_read(cx, buf)
}

pub fn vectored<T: AsyncWrite + ?Sized>(io: Pin<&mut T>) -> bool {
    io.is_write_vectored()
}

pub fn flush_file(file: Pin<&mut File>, cx: &mut Context<'_>) -> Poll<()> {
    file.poll_flush(cx)
}
"##,
            ),
            (
                "src/sleep.rs",
                r##"use std::pin::Pin;

pub struct Sleep;

impl Sleep {
    pub fn reset(self: Pin<&mut Self>) {}

    pub fn deadline(&self) -> u64 {
        0
    }
}

pub fn rearm(sleep: Pin<&mut Sleep>) -> u64 {
    let at = sleep.deadline();
    sleep.reset();
    at
}
"##,
            ),
            (
                "src/runtime.rs",
                r##"use std::rc::Rc;

#[derive(Debug)]
pub struct Runtime;

impl Clone for Runtime {
    fn clone(&self) -> Self {
        Runtime
    }
}

impl Runtime {
    fn in_scope(&self) {}

    pub fn handle_event(self: Rc<Self>) {
        let me = self.clone();
        self.in_scope();
    }
}

pub struct Route;

impl Route {
    fn into_route(self: Box<Self>) -> Route {
        *self
    }

    fn call(self: Box<Self>) -> Route {
        self.into_route()
    }
}
"##,
            ),
            (
                "src/link.rs",
                r##"use std::ptr::NonNull;

pub unsafe trait Link {
    type Target;
    unsafe fn from_raw(ptr: NonNull<Self::Target>) -> Self;
}
"##,
            ),
            (
                "src/net.rs",
                r##"use std::ptr::NonNull;
use std::sync::Arc;

use crate::link::Link;

pub struct ScheduledIo;

unsafe impl Link for Arc<ScheduledIo> {
    type Target = ScheduledIo;

    unsafe fn from_raw(ptr: NonNull<ScheduledIo>) -> Arc<ScheduledIo> {
        unsafe { Arc::from_raw(ptr.as_ptr() as *const _) }
    }
}
"##,
            ),
            (
                "src/wake.rs",
                r##"use std::ptr::NonNull;
use std::sync::Arc;

use crate::link::Link;
use crate::net::ScheduledIo;

pub unsafe fn wake(ptr: NonNull<ScheduledIo>) -> Arc<ScheduledIo> {
    unsafe { Arc::from_raw(ptr.as_ptr()) }
}
"##,
            ),
            (
                "src/grid.rs",
                r##"pub struct Grid {
    cells: Vec<u8>,
}

impl Grid {
    pub fn len(&self) -> usize {
        self.cells.len()
    }
}
"##,
            ),
            (
                "src/shape.rs",
                r##"use crate::grid::Grid;

pub trait Shape {
    fn len(&self) -> usize;
    fn area(&self) -> usize;
}

impl Shape for Grid {
    fn len(&self) -> usize {
        0
    }

    fn area(&self) -> usize {
        self.len()
    }
}

pub fn count(grid: &Grid) -> usize {
    grid.len()
}
"##,
            ),
            (
                "src/ser.rs",
                r##"pub trait Serialize {
    fn serialize(&self, out: &mut String);
}

pub struct SocketAddr {
    ip: u32,
    port: u16,
}

impl Serialize for SocketAddr {
    fn serialize(&self, out: &mut String)
    where
        Self: Sized,
    {
        if out.is_empty() {
            out.push('[');
        } else {
            (self.ip, self.port).serialize(out)
        }
    }
}

impl Serialize for Path {
    fn serialize(&self, out: &mut String) {}
}

pub struct Path;
"##,
            ),
        ])
    }

    fn at(g: &Graph, file: &str, line: u32) -> Vec<String> {
        calls_at(g, file, line)
    }

    const NONE: Vec<String> = Vec::new();

    #[test]
    fn through_pinned_self() {
        let g = graph();
        assert_eq!(
            at(&g, "src/file.rs", 32),
            vec!["File::poll_flush@src/file.rs:25"]
        );
        assert_eq!(
            at(&g, "src/io.rs", 8),
            vec!["AsyncWrite::poll_write@src/io.rs:5"]
        );
        assert_eq!(
            at(&g, "src/cursor.rs", 9),
            vec!["Cursor::poll_flush@src/cursor.rs:13"]
        );
        assert_eq!(
            at(&g, "src/cursor.rs", 20),
            vec!["Cursor::poll_flush@src/cursor.rs:24"]
        );
        assert_eq!(at(&g, "src/file.rs", 27), NONE);
        assert_eq!(
            at(&g, "src/file.rs", 26),
            vec!["File::complete@src/file.rs:15"]
        );
    }

    #[test]
    fn on_a_value_declared_pin_mut() {
        let g = graph();
        assert_eq!(
            at(&g, "src/copy.rs", 8),
            vec!["AsyncRead::poll_read@src/io.rs:16"]
        );
        assert_eq!(
            at(&g, "src/copy.rs", 12),
            vec!["Pin::is_write_vectored@src/io.rs:32"]
        );
        assert_eq!(
            at(&g, "src/copy.rs", 16),
            vec!["File::poll_flush@src/file.rs:25"]
        );
        assert_eq!(
            at(&g, "src/sleep.rs", 14),
            vec!["Sleep::deadline@src/sleep.rs:8"]
        );
        assert_eq!(
            at(&g, "src/sleep.rs", 15),
            vec!["Sleep::reset@src/sleep.rs:6"]
        );
    }

    #[test]
    fn through_rc_self_and_box_self() {
        let g = graph();
        assert_eq!(
            at(&g, "src/runtime.rs", 17),
            vec!["Runtime::in_scope@src/runtime.rs:13"]
        );
        assert_eq!(
            at(&g, "src/runtime.rs", 29),
            vec!["Route::into_route@src/runtime.rs:24"]
        );
        assert_eq!(at(&g, "src/runtime.rs", 16), NONE);
        assert_eq!(at(&g, "src/erased.rs", 7), NONE);
        assert_eq!(
            at(&g, "src/erased.rs", 11),
            vec!["MakeErased::into_route@src/erased.rs:6"]
        );
    }

    #[test]
    fn a_trait_impl_calling_its_own_name() {
        let g = graph();
        assert_eq!(at(&g, "src/measure.rs", 9), vec!["Grid::len@src/grid.rs:6"]);
        assert_eq!(at(&g, "src/waker.rs", 9), NONE);
        assert_eq!(
            at(&g, "src/tester.rs", 17),
            vec!["Tester::poll@src/tester.rs:12"]
        );
    }

    #[test]
    fn inherent_before_trait_and_tuples() {
        let g = graph();
        assert_eq!(at(&g, "src/net.rs", 12), NONE);
        assert_eq!(at(&g, "src/wake.rs", 8), NONE);
        assert_eq!(at(&g, "src/shape.rs", 14), vec!["Grid::len@src/grid.rs:6"]);
        assert_eq!(at(&g, "src/shape.rs", 19), vec!["Grid::len@src/grid.rs:6"]);
        assert_eq!(at(&g, "src/ser.rs", 18), NONE);
    }
}

/// rust-written-receivers.test.ts: a call on a field chain, a struct literal
/// or a path through an outside type reaches what is written.
mod written_receivers {
    use super::*;

    /// The non-heuristic calls on `line` of `file`, as `qualifiedName resolvedBy`.
    fn at(g: &Graph, file: &str, line: u32) -> Vec<String> {
        let mut v: Vec<String> = g
            .out_of_file(file)
            .into_iter()
            .filter(|(e, _)| {
                e.kind.as_str() == "calls"
                    && e.line == Some(line)
                    && e.provenance != Some(codegraph_core::Provenance::Heuristic)
            })
            .map(|(e, t)| {
                let by = e
                    .metadata
                    .as_ref()
                    .and_then(|m| m["resolvedBy"].as_str())
                    .unwrap_or("undefined")
                    .to_string();
                format!("{} {by}", t.qualified_name)
            })
            .collect();
        v.sort();
        v.dedup();
        v
    }

    const NONE: Vec<String> = Vec::new();

    #[test]
    fn the_issue_crate() {
        let g = Graph::new(&[
            (
                "Cargo.toml",
                r##"[package]
name = "repro"
version = "0.1.0"
edition = "2021"

[dependencies]
serde_json = "1"
"##,
            ),
            (
                "src/main.rs",
                r##"use std::collections::HashMap;
use serde_json::Value as J;

pub enum Val {
    Null,
    Int(i64),
    List(Vec<Val>),
}

pub struct Obj {
    keys: Vec<String>,
}

impl Obj {
    pub fn contains(&self, k: &str) -> bool {
        self.keys.iter().any(|x| x == k)
    }
}

pub struct Ext;

impl Ext {
    pub fn get(name: &str) -> Ext {
        let _ = name;
        Ext
    }
}

pub struct Fail;

impl From<std::io::Error> for Fail {
    fn from(_: std::io::Error) -> Fail {
        Fail
    }
}

pub struct Xyz {
    x: f64,
}

pub struct Stat {
    n: f64,
}

pub trait ToVal {
    fn to_val(&self) -> Val;
}

impl ToVal for Xyz {
    fn to_val(&self) -> Val {
        Val::Int(self.x as i64)
    }
}

impl ToVal for Stat {
    fn to_val(&self) -> Val {
        Val::Int(self.n as i64)
    }
}

// 1. slice::contains, not Obj::contains
fn slice_has(names: &[&str], k: &str) -> bool {
    names.contains(&k)
}

// 2. HashMap::get on a struct field, not Ext::get
pub struct Vis {
    map: HashMap<String, i32>,
}

fn map_get(vis: &Vis) -> Option<&i32> {
    vis.map.get("a")
}

// 3. serde_json::Value::from via alias J, not Fail::from
fn to_json(i: i64) -> J {
    J::from(i)
}

// 4. method on struct literal: Xyz::to_val, not Stat::to_val
fn centroid() -> Val {
    Xyz { x: 1.0 }.to_val()
}

fn main() {
    let o = Obj { keys: vec![] };
    let _ = o.contains("a");
    let _ = Ext::get("x");
    let _ = Stat { n: 0.0 }.to_val();
    let _ = slice_has(&["a"], "a");
    let _ = map_get(&Vis { map: HashMap::new() });
    let _ = to_json(1);
    let _ = centroid();
    let _ = Val::List(vec![Val::Null]);
}
"##,
            ),
        ]);
        let m = "src/main.rs";
        assert_eq!(at(&g, m, 72), NONE, "HashMap field get");
        assert_eq!(at(&g, m, 77), NONE, "J::from under use … as J");
        assert_eq!(at(&g, m, 82), vec!["Xyz::to_val instance-method"]);
        assert_eq!(at(&g, m, 89), vec!["Stat::to_val instance-method"]);
        assert_eq!(at(&g, m, 63), NONE);
        assert_eq!(at(&g, m, 87), vec!["Obj::contains instance-method"]);
        assert_eq!(at(&g, m, 88), vec!["Ext::get qualified-name"]);
    }

    fn variants() -> Graph {
        Graph::new(&[
            (
                "Cargo.toml",
                r##"[package]
name = "variants"
version = "0.1.0"
edition = "2021"

[dependencies]
serde_json = "1"
"##,
            ),
            (
                "src/main.rs",
                r##"mod chains;
mod config;
mod paths;

fn main() {}
"##,
            ),
            (
                "src/config.rs",
                r##"pub struct Config {
    pub port: u16,
}

impl Config {
    pub fn load() -> Config {
        Config { port: 1 }
    }

    pub fn port(&self) -> u16 {
        self.port
    }
}

/// A project type named like `serde_json::Value`.
pub struct Value;

impl Default for Value {
    fn default() -> Value {
        Value
    }
}

pub struct Fail;

impl From<std::io::Error> for Fail {
    fn from(_: std::io::Error) -> Fail {
        Fail
    }
}
"##,
            ),
            (
                "src/chains.rs",
                r##"use std::collections::HashMap;

use crate::config::Config;

pub struct Inner {
    pub map: HashMap<String, i32>,
    pub cfg: Config,
}

pub struct Outer {
    pub inner: Inner,
    pub pair: (Config, u8),
}

pub struct Wrap(pub Config);

/// Namesakes of the methods the calls below make, beside them.
pub struct Dial;

impl Dial {
    pub fn port(&self) -> u16 {
        0
    }

    pub fn get(&self, _k: &str) -> Option<&i32> {
        None
    }
}

pub enum Shape {
    Circle { r: f64 },
}

impl Shape {
    pub fn area(&self) -> f64 {
        0.0
    }
}

pub struct Room {
    pub w: f64,
}

impl Room {
    pub fn area(&self) -> f64 {
        self.w
    }

    pub fn fresh(&self) -> f64 {
        Self { w: 2.0 }.area()
    }
}

impl Outer {
    pub fn deep_std(&self) -> Option<&i32> {
        self.inner.map.get("a")
    }

    pub fn deep_project(&self) -> u16 {
        self.inner.cfg.port()
    }

    pub fn tuple_field(&self) -> u16 {
        self.pair.0.port()
    }
}

impl Wrap {
    pub fn tuple_self(&self) -> u16 {
        self.0.port()
    }
}

pub fn param_chain(o: &Outer) -> u16 {
    o.inner.cfg.port()
}

pub fn param_chain_std(o: &Outer) -> i32 {
    o.inner
        .map
        .get("b")
        .copied()
        .unwrap_or(0)
}

pub fn local_chain() -> u16 {
    let w = Wrap(Config::load());
    w.0.port()
}

pub fn literal() -> u16 {
    Config { port: 2 }.port()
}

pub fn literal_in_parens() -> u16 {
    (&Config { port: 3 }).port()
}

pub fn variant_literal() -> f64 {
    Shape::Circle { r: 1.0 }.area()
}

/// A trait a derive implements (axum-extra's `#[derive(TypedPath)]`): the impl is the macro's.
pub trait TypedPath {
    fn with_query_params(self, q: u8) -> u8
    where
        Self: Sized,
    {
        q
    }
}

#[derive(TypedPath, Debug)]
pub struct UsersShow {
    pub id: u32,
}

pub fn derived() -> u8 {
    UsersShow { id: 1 }.with_query_params(2)
}
"##,
            ),
            (
                "src/paths.rs",
                r##"use serde_json as json;
use serde_json::Value;

pub fn written_outside(i: i64) -> serde_json::Value {
    serde_json::Value::from(i)
}

pub fn crate_alias(i: i64) -> json::Value {
    json::Value::from(i)
}

pub fn unrenamed_outside() -> Value {
    Value::default()
}

pub fn written_default() -> serde_json::Value {
    serde_json::Value::default()
}
"##,
            ),
        ])
    }

    #[test]
    fn field_chains() {
        let g = variants();
        let c = "src/chains.rs";
        assert_eq!(at(&g, c, 56), NONE);
        assert_eq!(at(&g, c, 79), NONE);
        for line in [60, 64, 70, 75, 88] {
            assert_eq!(
                at(&g, c, line),
                vec!["Config::port instance-method"],
                "line {line}"
            );
        }
    }

    #[test]
    fn struct_literals() {
        let g = variants();
        let c = "src/chains.rs";
        assert_eq!(at(&g, c, 92), vec!["Config::port instance-method"]);
        assert_eq!(at(&g, c, 96), vec!["Config::port instance-method"]);
        assert_eq!(at(&g, c, 100), vec!["Shape::area instance-method"]);
        assert_eq!(at(&g, c, 50), vec!["Room::area instance-method"]);
        assert_eq!(
            at(&g, c, 119),
            vec!["TypedPath::with_query_params instance-method"]
        );
    }

    #[test]
    fn paths_through_outside_types() {
        let g = variants();
        for line in [5, 9, 13, 17] {
            assert_eq!(at(&g, "src/paths.rs", line), NONE, "line {line}");
        }
        let g = Graph::new(&[
            (
                "Cargo.toml",
                r##"[package]
name = "keeps"
version = "0.1.0"
edition = "2021"

[dependencies]
serde_json = "1"
"##,
            ),
            (
                "src/main.rs",
                r##"use std::io::Write as W;
use std::path::PathBuf;

use serde_json::Value;

pub struct Fail;

impl From<Fail> for PathBuf {
    fn from(_: Fail) -> PathBuf {
        PathBuf::new()
    }
}

pub trait Blank {
    fn blank() -> Self;
}

impl Blank for serde_json::Value {
    fn blank() -> Self {
        serde_json::Value::Null
    }
}

pub struct Sink;

impl std::io::Write for Sink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn to_path(f: Fail) -> PathBuf {
    PathBuf::from(f)
}

fn blank() -> Value {
    Value::blank()
}

fn drain(s: &mut Sink) {
    let _ = W::flush(s);
}

fn main() {
    let _ = to_path(Fail);
    let _ = blank();
}
"##,
            ),
        ]);
        assert_eq!(
            at(&g, "src/main.rs", 37),
            vec!["PathBuf::from qualified-name"]
        );
        assert_eq!(
            at(&g, "src/main.rs", 41),
            vec!["Value::blank qualified-name"]
        );
        assert_eq!(
            at(&g, "src/main.rs", 45),
            vec!["Sink::flush instance-method"]
        );
    }
}
