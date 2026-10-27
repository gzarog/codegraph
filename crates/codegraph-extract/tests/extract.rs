use codegraph_core::{EdgeKind, Language, NodeKind, ReferenceKind};
use codegraph_extract::extract_file;

#[test]
fn typescript_symbols_edges_and_refs() {
    let src = r#"
import { helper } from './util';

/** Greets people. */
export class Greeter {
  greet(name: string): string {
    return helper(name);
  }
}

export async function main() {
  const g = new Greeter();
  g.greet('x');
}
"#;
    let r = extract_file("src/app.ts", src, Language::TypeScript);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let class = r.nodes.iter().find(|n| n.kind == NodeKind::Class).expect("class");
    assert_eq!(class.name, "Greeter");
    assert!(class.is_exported);
    assert_eq!(class.docstring.as_deref(), Some("Greets people."));
    let method = r.nodes.iter().find(|n| n.kind == NodeKind::Method).expect("method");
    assert_eq!(method.qualified_name, "Greeter::greet");
    let main = r.nodes.iter().find(|n| n.name == "main").expect("main");
    assert!(main.is_async);
    assert!(r.edges.iter().any(|e| e.kind == EdgeKind::Contains && e.target == method.id && e.source == class.id));
    assert!(r.unresolved_references.iter().any(|u| u.reference_name == "helper" && u.reference_kind == ReferenceKind::Calls));
    assert!(r.unresolved_references.iter().any(|u| u.reference_kind == ReferenceKind::Imports));
}

#[test]
fn every_walker_runs() {
    let samples: &[(Language, &str, &str)] = &[
        (Language::Python, "a.py", "class A:\n    def f(self):\n        g()\n"),
        (Language::Go, "a.go", "package a\nfunc F() { G() }\n"),
        (Language::Rust, "a.rs", "struct S;\nimpl S { fn f(&self) { g(); } }\n"),
        (Language::Java, "A.java", "class A { void f() { g(); } }\n"),
        (Language::C, "a.c", "int f(void) { return g(); }\n"),
        (Language::Cpp, "a.cpp", "class A { void f(); };\n"),
        (Language::CSharp, "A.cs", "class A { void F() { G(); } }\n"),
        (Language::Ruby, "a.rb", "class A\n  def f\n    g\n  end\nend\n"),
        (Language::Php, "a.php", "<?php class A { function f() { g(); } }\n"),
        (Language::Swift, "a.swift", "class A { func f() { g() } }\n"),
        (Language::Kotlin, "a.kt", "class A { fun f() { g() } }\n"),
        (Language::Scala, "a.scala", "class A { def f() = g() }\n"),
        (Language::Dart, "a.dart", "class A { void f() { g(); } }\n"),
        (Language::Lua, "a.lua", "function f() g() end\n"),
        (Language::R, "a.r", "f <- function() { g() }\n"),
        (Language::JavaScript, "a.js", "function f() { g(); }\n"),
    ];
    for (lang, path, src) in samples {
        let r = extract_file(path, src, *lang);
        assert!(r.errors.iter().all(|e| e.severity == codegraph_core::Severity::Warning), "{lang}: {:?}", r.errors);
        assert!(r.nodes.iter().any(|n| n.kind != NodeKind::File), "{lang}: no symbols");
    }
}
