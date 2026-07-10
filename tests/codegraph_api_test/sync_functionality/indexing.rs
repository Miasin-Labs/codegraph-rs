#[tokio::test(flavor = "current_thread")]
async fn sync_reindexes_added_files() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path()).await;

    write(
        &dir.path().join("src/new.ts"),
        "export function newFunc() { return 42; }",
    );

    let result = cg.sync(&IndexOptions::default()).await.unwrap();
    assert_eq!(result.files_added, 1);
    assert_eq!(result.files_modified, 0);
    assert_eq!(result.files_removed, 0);
    assert!(search_count(&cg, "newFunc") > 0);
}

#[tokio::test(flavor = "current_thread")]
async fn sync_reindexes_modified_files() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path()).await;

    write(
        &dir.path().join("src/index.ts"),
        "export function goodbye() { return 'farewell'; }",
    );

    let result = cg.sync(&IndexOptions::default()).await.unwrap();
    assert_eq!(result.files_modified, 1);
    assert!(search_count(&cg, "goodbye") > 0);
    assert_eq!(search_count(&cg, "hello"), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn sync_removes_nodes_from_deleted_files() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path()).await;

    fs::remove_file(dir.path().join("src/index.ts")).unwrap();

    let result = cg.sync(&IndexOptions::default()).await.unwrap();
    assert_eq!(result.files_removed, 1);
    assert_eq!(search_count(&cg, "hello"), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn sync_reports_no_changes_when_nothing_changed() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path()).await;

    let result = cg.sync(&IndexOptions::default()).await.unwrap();
    assert_eq!(result.files_added, 0);
    assert_eq!(result.files_modified, 0);
    assert_eq!(result.files_removed, 0);
    assert!(result.files_checked > 0);
}

#[tokio::test(flavor = "current_thread")]
async fn index_all_reconciles_nodes_from_deleted_files() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path()).await;

    fs::remove_file(dir.path().join("src/index.ts")).unwrap();

    let result = cg.index_all(&IndexOptions::default()).await.unwrap();
    assert!(result.success);
    assert_eq!(search_count(&cg, "hello"), 0);
}

#[tokio::test(flavor = "current_thread")]
#[cfg(target_os = "linux")]
async fn index_all_does_not_leave_persistent_parse_worker_threads() {
    let dir = TempDir::new().unwrap();
    for i in 0..96 {
        write(
            &dir.path().join(format!("src/file_{i}.ts")),
            &format!("export function f{i}() {{ return {i}; }}"),
        );
    }

    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    let result = cg.index_all(&IndexOptions::default()).await.unwrap();
    assert!(result.success);

    let parse_threads: Vec<String> = current_thread_names()
        .into_iter()
        .filter(|name| name.starts_with("cg-parse-"))
        .collect();
    assert!(
        parse_threads.is_empty(),
        "index_all left persistent parse worker threads behind: {parse_threads:?}"
    );
}
