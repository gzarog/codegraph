use super::*;
use codegraph_index::{index_project, IndexOptions, ProjectConfig};
use std::fs;

fn project() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let w = |rel: &str, text: &str| {
        let p = dir.path().join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    };
    w(
        "src/db.ts",
        "export function query() { return 1; }\nexport class Repo { find() { return query(); } }\n",
    );
    w("src/svc.ts", "import { Repo } from './db';\nexport function load() { return new Repo().find(); }\nfunction unused() {}\n");
    w(
        "src/api.ts",
        "import { load } from './svc';\nexport function handler() { return load(); }\n",
    );
    w("src/model.ts", "export class Animal {}\nexport class Dog extends Animal {}\nexport class Puppy extends Dog {}\n");
    let mut store = Store::open_in_memory().unwrap();
    index_project(
        &mut store,
        dir.path(),
        &ProjectConfig::default(),
        &IndexOptions::default(),
    )
    .unwrap();
    (dir, store)
}

fn id(store: &Store, name: &str) -> String {
    store
        .nodes_named(name)
        .unwrap()
        .into_iter()
        .find(|n| n.kind != NodeKind::File)
        .unwrap()
        .id
}

#[test]
fn callers_callees_impact_path() {
    let (_d, store) = project();
    let g = Graph::new(&store);
    let query = id(&store, "query");
    let callers: Vec<String> = g
        .callers(&query, &[])
        .unwrap()
        .into_iter()
        .map(|n| n.node.name)
        .collect();
    assert_eq!(callers, vec!["find"]);
    let handler = id(&store, "handler");
    let callees: Vec<String> = g
        .callees(&handler, &[])
        .unwrap()
        .into_iter()
        .map(|n| n.node.name)
        .collect();
    assert_eq!(callees, vec!["load"]);

    let impact = g.impact(&query, 4, 100).unwrap();
    let names: Vec<String> = impact
        .levels
        .iter()
        .flatten()
        .map(|n| n.node.name.clone())
        .collect();
    assert!(
        names.contains(&"find".to_string())
            && names.contains(&"load".to_string())
            && names.contains(&"handler".to_string()),
        "{names:?}"
    );

    let path = g
        .find_path(&handler, &query, &[], 6, false)
        .unwrap()
        .unwrap();
    let chain: Vec<String> = path.iter().map(|s| s.node.name.clone()).collect();
    assert_eq!(chain, vec!["handler", "load", "find", "query"]);
}

#[test]
fn hierarchy_and_dead_code() {
    let (_d, store) = project();
    let g = Graph::new(&store);
    let dog = id(&store, "Dog");
    let h = g.type_hierarchy(&dog, 10).unwrap();
    assert_eq!(
        h.ancestors
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Animal"]
    );
    assert_eq!(
        h.descendants
            .iter()
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>(),
        vec!["Puppy"]
    );
    let dead: Vec<String> = g
        .dead_code(10)
        .unwrap()
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(dead, vec!["unused"]);
    let find = id(&store, "find");
    let anc: Vec<String> = g
        .ancestors(&find)
        .unwrap()
        .into_iter()
        .map(|n| n.name)
        .collect();
    assert_eq!(anc, vec!["db.ts", "Repo"]);
}
