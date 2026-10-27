use super::*;
use codegraph_core::{
    now_ms, Edge, EdgeKind, FileRecord, Language, Node, NodeKind, ReferenceKind,
    UnresolvedReference,
};

fn file(path: &str) -> FileRecord {
    FileRecord {
        path: path.into(),
        content_hash: "h".into(),
        language: Language::TypeScript,
        size: 10,
        modified_at: 0,
        indexed_at: now_ms(),
        node_count: 0,
        errors: vec![],
        generated: false,
    }
}

fn node(id: &str, kind: NodeKind, name: &str, path: &str, line: u32) -> Node {
    Node::new(id, kind, name, path, Language::TypeScript, line, line + 2)
}

fn reference(from: &str, name: &str) -> UnresolvedReference {
    UnresolvedReference {
        from_node_id: from.into(),
        reference_name: name.into(),
        reference_kind: ReferenceKind::Calls,
        line: 2,
        column: 4,
        file_path: None,
        language: None,
        candidates: vec![],
        row_id: None,
    }
}

fn two_files(store: &mut Store) {
    let a = ExtractionResult {
        nodes: vec![
            node("file:a.ts", NodeKind::File, "a.ts", "a.ts", 1),
            node("fn:caller", NodeKind::Function, "callerFn", "a.ts", 1),
        ],
        edges: vec![Edge::new("file:a.ts", "fn:caller", EdgeKind::Contains)],
        unresolved_references: vec![reference("fn:caller", "parseConfig")],
        ..Default::default()
    };
    let b = ExtractionResult {
        nodes: vec![
            node("file:b.ts", NodeKind::File, "b.ts", "b.ts", 1),
            node("fn:parse", NodeKind::Function, "parseConfig", "b.ts", 3),
        ],
        edges: vec![Edge::new("file:b.ts", "fn:parse", EdgeKind::Contains)],
        ..Default::default()
    };
    store
        .store_files(&[(file("a.ts"), a), (file("b.ts"), b)])
        .unwrap();
}

#[test]
fn stores_and_reads_back() {
    let mut s = Store::open_in_memory().unwrap();
    two_files(&mut s);
    assert_eq!(s.file_count().unwrap(), 2);
    assert_eq!(s.node_count().unwrap(), 4);
    assert_eq!(s.nodes_in_file("a.ts").unwrap().len(), 2);
    assert_eq!(s.outgoing_edges("file:a.ts", &[]).unwrap().len(), 1);
    assert_eq!(s.count_refs(RefStatus::Pending).unwrap(), 1);
    assert_eq!(s.file("a.ts").unwrap().unwrap().node_count, 2);
    let stats = s.stats().unwrap();
    assert_eq!(stats.edge_count, 2);
}

#[test]
fn resolution_and_reindex_reopen() {
    let mut s = Store::open_in_memory().unwrap();
    two_files(&mut s);
    let pending = s.pending_refs_after(0, 100).unwrap();
    let r = &pending[0];
    let edge = Edge::new(&r.from_node_id, "fn:parse", EdgeKind::Calls);
    s.apply_resolutions(&[(r.row_id.unwrap(), edge, "b.ts".into())], &[])
        .unwrap();
    assert_eq!(
        s.incoming_edges("fn:parse", &[EdgeKind::Calls])
            .unwrap()
            .len(),
        1
    );
    assert_eq!(s.count_refs(RefStatus::Resolved).unwrap(), 1);
    assert_eq!(s.dependent_files("b.ts").unwrap(), vec!["a.ts".to_string()]);

    // Re-indexing b.ts drops the edge into it and reopens the reference.
    let b = ExtractionResult {
        nodes: vec![node(
            "fn:parse2",
            NodeKind::Function,
            "parseConfig",
            "b.ts",
            7,
        )],
        ..Default::default()
    };
    s.store_file(&file("b.ts"), &b).unwrap();
    assert_eq!(s.edge_count().unwrap(), 1);
    assert_eq!(s.count_refs(RefStatus::Pending).unwrap(), 1);
}

#[test]
fn failed_refs_reopen_by_tail() {
    let mut s = Store::open_in_memory().unwrap();
    two_files(&mut s);
    let id = s.pending_refs_after(0, 10).unwrap()[0].row_id.unwrap();
    s.apply_resolutions(&[], &[id]).unwrap();
    assert_eq!(s.count_refs(RefStatus::Failed).unwrap(), 1);
    let names: HashSet<String> = ["parseConfig".to_string()].into();
    assert_eq!(s.reopen_failed_refs_named(&names).unwrap(), 1);
}

#[test]
fn search_ranks_exact_names() {
    let mut s = Store::open_in_memory().unwrap();
    two_files(&mut s);
    let hits = s
        .search_nodes("parseConfig", &SearchOptions::default())
        .unwrap();
    assert_eq!(hits[0].node.id, "fn:parse");
    let fuzzy = s
        .search_nodes("parseConfg", &SearchOptions::default())
        .unwrap();
    assert_eq!(fuzzy[0].node.name, "parseConfig");
    let filtered = s
        .search_nodes("kind:file", &SearchOptions::default())
        .unwrap();
    assert!(filtered.iter().all(|r| r.node.kind == NodeKind::File));
    let by_path = s
        .search_nodes("path:b.ts parse", &SearchOptions::default())
        .unwrap();
    assert!(by_path.iter().all(|r| r.node.file_path == "b.ts"));
}

#[test]
fn delete_file_cascades() {
    let mut s = Store::open_in_memory().unwrap();
    two_files(&mut s);
    s.delete_file("a.ts").unwrap();
    assert_eq!(s.count_refs(RefStatus::Pending).unwrap(), 0);
    assert_eq!(s.node_count().unwrap(), 2);
    assert!(s.file("a.ts").unwrap().is_none());
}

#[test]
fn name_tails() {
    assert_eq!(name_tail("util.greet"), "greet");
    assert_eq!(name_tail("Foo::bar"), "bar");
    assert_eq!(name_tail("plain"), "plain");
    assert_eq!(name_tail("$this->save"), "save");
}

#[test]
fn segments() {
    let mut s = Store::open_in_memory().unwrap();
    two_files(&mut s);
    s.rebuild_name_segments().unwrap();
    assert_eq!(
        s.names_for_segment("config", 10).unwrap(),
        vec!["parseConfig".to_string()]
    );
}

#[test]
fn deprioritized_and_generated_rank_lower() {
    let mut s = Store::open_in_memory().unwrap();
    let mut gen = file("gen/api.pb.ts");
    gen.generated = true;
    let mk = |id: &str, path: &str| ExtractionResult {
        nodes: vec![node(id, NodeKind::Function, "handleRequest", path, 1)],
        ..Default::default()
    };
    s.store_files(&[
        (gen, mk("fn:gen", "gen/api.pb.ts")),
        (file("legacy/h.ts"), mk("fn:legacy", "legacy/h.ts")),
        (file("src/h.ts"), mk("fn:src", "src/h.ts")),
    ])
    .unwrap();
    s.set_deprioritized(Some(Box::new(|p: &str| p.starts_with("legacy/"))));
    let hits = s
        .search_nodes("handleRequest", &SearchOptions::default())
        .unwrap();
    assert_eq!(
        hits[0].node.id,
        "fn:src",
        "{:?}",
        hits.iter()
            .map(|h| (&h.node.id, h.score))
            .collect::<Vec<_>>()
    );
    assert!(
        s.is_deprioritized("legacy/x.ts")
            && s.is_deprioritized("gen/api.pb.ts")
            && !s.is_deprioritized("src/h.ts")
    );
}
