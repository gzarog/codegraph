use codegraph_context::explore::{explore, ExploreOptions};
use codegraph_index::{index_project, IndexOptions, ProjectConfig};
use codegraph_store::Store;
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn indexed(root: &Path) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    index_project(
        &mut store,
        root,
        &ProjectConfig::default(),
        &IndexOptions::default(),
    )
    .unwrap();
    store
}

#[test]
fn anchors_show_the_symbol_around_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut src = String::new();
    for i in 0..40 {
        src.push_str(&format!("export function f{i}() {{\n  return {i};\n}}\n"));
    }
    write(root, "src/many.ts", &src);
    let store = indexed(root);
    // Line 62 is inside f20 (lines 61-63).
    let out = explore(&store, root, "src/many.ts:62", &ExploreOptions::default()).unwrap();
    assert!(out.contains("export function f20()"), "{out}");
    assert!(!out.contains("export function f0()"), "{out}");
}

#[test]
fn notes_dynamic_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/listeners.ts",
        "import { bus } from './bus';\nexport function onSaved(doc) { return doc; }\nbus.on('saved', onSaved);\n",
    );
    write(
        root,
        "src/client.ts",
        "import { bus } from './bus';\nexport function save(id: string) {\n  bus.emit('saved', id);\n}\n",
    );
    write(
        root,
        "src/bus.ts",
        "export const bus = { on() {}, emit() {} };\n",
    );
    let store = indexed(root);
    let out = explore(&store, root, "save", &ExploreOptions::default()).unwrap();
    assert!(out.contains("## Dynamic dispatch"), "{out}");
    assert!(
        out.contains("`save` emits `saved` → onSaved (src/listeners.ts:2)"),
        "{out}"
    );
    let out = explore(&store, root, "onSaved", &ExploreOptions::default()).unwrap();
    assert!(
        out.contains("`onSaved` is reached dynamically: save (src/client.ts:2) emits `saved`"),
        "{out}"
    );
}
