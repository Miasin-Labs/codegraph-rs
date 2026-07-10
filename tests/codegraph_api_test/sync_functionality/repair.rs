#[tokio::test(flavor = "current_thread")]
async fn index_all_keeps_unresolved_refs_repairable_by_a_later_full_index() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path()).await;

    write(
        &dir.path().join("src/index.ts"),
        "export function caller() { return missingLater(); }",
    );
    cg.index_all(&IndexOptions::default()).await.unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();

    write(
        &dir.path().join("src/target.ts"),
        "export function missingLater() { return 42; }",
    );
    cg.index_all(&IndexOptions::default()).await.unwrap();

    let target = cg
        .search_nodes("missingLater", None)
        .unwrap()
        .into_iter()
        .map(|r| r.node)
        .find(|n| n.kind == NodeKind::Function)
        .expect("target function should be indexed");
    let callers = cg.get_callers(&target.id, None).unwrap();
    assert!(
        callers.iter().any(|r| r.node.name == "caller"),
        "caller should resolve to the late-added target"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn index_files_resolves_refs_and_repairs_late_targets() {
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/caller.ts"),
        "export function caller() { return missingLater(); }",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();

    let first = cg.index_files(&["src/caller.ts".to_string()]).await.unwrap();
    assert!(first.success);
    assert_eq!(first.files_indexed, 1);

    write(
        &dir.path().join("src/target.ts"),
        "export function missingLater() { return 42; }",
    );
    let second = cg.index_files(&["src/target.ts".to_string()]).await.unwrap();
    assert!(second.success);
    assert_eq!(second.files_indexed, 1);

    let target = cg
        .search_nodes("missingLater", None)
        .unwrap()
        .into_iter()
        .map(|r| r.node)
        .find(|n| n.kind == NodeKind::Function)
        .expect("target function should be indexed");
    let callers = cg.get_callers(&target.id, None).unwrap();
    assert!(
        callers.iter().any(|r| r.node.name == "caller"),
        "index_files should resolve existing refs after a late target is indexed"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn sync_repairs_callers_when_removed_target_reappears() {
    let dir = TempDir::new().unwrap();
    write(
        &dir.path().join("src/caller.ts"),
        "import { missingLater } from './target';\nexport function caller() { return missingLater(); }",
    );
    write(
        &dir.path().join("src/target.ts"),
        "export function missingLater() { return 42; }",
    );
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();

    let target = cg
        .search_nodes("missingLater", None)
        .unwrap()
        .into_iter()
        .map(|r| r.node)
        .find(|n| n.kind == NodeKind::Function)
        .expect("target function should be indexed");
    assert!(
        cg.get_callers(&target.id, None)
            .unwrap()
            .iter()
            .any(|r| r.node.name == "caller"),
        "initial full index should resolve caller"
    );

    fs::remove_file(dir.path().join("src/target.ts")).unwrap();
    let removed = cg.sync(&IndexOptions::default()).await.unwrap();
    assert_eq!(removed.files_removed, 1);
    assert!(
        !cg.search_nodes("missingLater", None)
            .unwrap()
            .into_iter()
            .any(|r| r.node.kind == NodeKind::Function),
        "deleted target function should not remain indexed"
    );

    write(
        &dir.path().join("src/target.ts"),
        "export function missingLater() { return 42; }",
    );
    let added = cg.sync(&IndexOptions::default()).await.unwrap();
    assert_eq!(added.files_added, 1);

    let restored_target = cg
        .search_nodes("missingLater", None)
        .unwrap()
        .into_iter()
        .map(|r| r.node)
        .find(|n| n.kind == NodeKind::Function)
        .expect("target function should be re-indexed");
    let callers = cg.get_callers(&restored_target.id, None).unwrap();
    assert!(
        callers.iter().any(|r| r.node.name == "caller"),
        "sync should restore caller edge when an unchanged caller's target reappears"
    );
}
