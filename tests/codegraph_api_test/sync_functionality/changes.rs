#[tokio::test(flavor = "current_thread")]
async fn get_changed_files_detects_added_files() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path()).await;

    write(
        &dir.path().join("src/new.ts"),
        "export function newFunc() { return 42; }",
    );

    let changes = cg.get_changed_files().unwrap();
    assert!(changes.added.contains(&"src/new.ts".to_string()));
    assert!(changes.modified.is_empty());
    assert!(changes.removed.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn get_changed_files_detects_modified_files() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path()).await;

    write(
        &dir.path().join("src/index.ts"),
        "export function hello() { return 'modified'; }",
    );

    let changes = cg.get_changed_files().unwrap();
    assert!(changes.added.is_empty());
    assert!(changes.modified.contains(&"src/index.ts".to_string()));
    assert!(changes.removed.is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn get_changed_files_detects_removed_files() {
    let dir = TempDir::new().unwrap();
    let cg = setup_indexed(dir.path()).await;

    fs::remove_file(dir.path().join("src/index.ts")).unwrap();

    let changes = cg.get_changed_files().unwrap();
    assert!(changes.added.is_empty());
    assert!(changes.modified.is_empty());
    assert!(changes.removed.contains(&"src/index.ts".to_string()));
}
