use codegraph_core::{EdgeKind, NodeKind};
use codegraph_index::{index_project, IndexOptions, ProjectConfig};
use codegraph_store::{RefStatus, Store};
use std::fs;
use std::path::Path;

fn write(root: &Path, rel: &str, text: &str) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, text).unwrap();
}

fn edge_exists(store: &Store, from_name: &str, to_name: &str, kind: EdgeKind) -> bool {
    let froms = store.nodes_named(from_name).unwrap();
    let tos = store.nodes_named(to_name).unwrap();
    froms.iter().any(|f| {
        store
            .outgoing_edges(&f.id, &[kind])
            .unwrap()
            .iter()
            .any(|e| tos.iter().any(|t| t.id == e.target))
    })
}

#[test]
fn indexes_resolves_and_syncs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    write(
        root,
        "src/util.ts",
        "export function helper(x: string) { return x; }\nexport class Base { hello() {} }\n",
    );
    write(
        root,
        "src/app.ts",
        "import { helper as h, Base } from './util';\nimport Svc from './svc';\nexport class App extends Base {\n  run() { h('a'); this.hello(); new Svc().go(); }\n}\n",
    );
    write(root, "src/svc.ts", "export default class Svc { go() {} }\n");
    write(root, "py/pkg/__init__.py", "from .core import compute\n");
    write(root, "py/pkg/core.py", "def compute():\n    return 1\n");
    write(
        root,
        "py/main.py",
        "from pkg import compute\n\ndef main():\n    compute()\n",
    );
    write(root, "node_modules/x/index.js", "function nope() {}\n");

    let mut store = Store::open_in_memory().unwrap();
    let config = ProjectConfig::default();
    let report = index_project(&mut store, root, &config, &IndexOptions::default()).unwrap();
    assert_eq!(report.files_added.len(), 6, "{report:?}");
    assert!(store.nodes_named("nope").unwrap().is_empty());

    assert!(
        edge_exists(&store, "run", "helper", EdgeKind::Calls),
        "aliased import call"
    );
    assert!(edge_exists(&store, "App", "Base", EdgeKind::Extends));
    assert!(
        edge_exists(&store, "run", "hello", EdgeKind::Calls),
        "inherited member via this"
    );
    assert!(
        edge_exists(&store, "run", "Svc", EdgeKind::Instantiates),
        "default import"
    );
    assert!(
        edge_exists(&store, "main", "compute", EdgeKind::Calls),
        "python package re-export"
    );
    let app_file = store.file_node("src/app.ts").unwrap().unwrap();
    let imports = store
        .outgoing_edges(&app_file.id, &[EdgeKind::Imports])
        .unwrap();
    assert!(imports.iter().any(|e| e.target == "file:src/util.ts"));

    // Unchanged: nothing re-extracted.
    let again = index_project(&mut store, root, &config, &IndexOptions::default()).unwrap();
    assert_eq!(again.files_indexed(), 0);
    assert_eq!(again.files_unchanged, 6);

    // Changing the callee's file re-resolves the caller's reference.
    write(root, "src/util.ts", "\n\nexport function helper(x: string) { return x + 1; }\nexport class Base { hello() {} }\n");
    let synced = index_project(&mut store, root, &config, &IndexOptions::default()).unwrap();
    assert_eq!(synced.files_changed, vec!["src/util.ts".to_string()]);
    assert!(
        edge_exists(&store, "run", "helper", EdgeKind::Calls),
        "edge restored after callee changed"
    );
    assert_eq!(store.count_refs(RefStatus::Pending).unwrap(), 0);

    // Removing a file drops its symbols.
    fs::remove_file(root.join("src/svc.ts")).unwrap();
    let removed = index_project(&mut store, root, &config, &IndexOptions::default()).unwrap();
    assert_eq!(removed.files_removed, vec!["src/svc.ts".to_string()]);
    assert!(store
        .nodes_by_kind(NodeKind::Class)
        .unwrap()
        .iter()
        .all(|n| n.name != "Svc"));
}
