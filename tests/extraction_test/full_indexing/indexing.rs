use crate::extraction_test::fixture::*;

#[test]
fn full_indexing_indexes_a_typescript_file() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    fs::write(
        src_dir.join("utils.ts"),
        "
export function add(a: number, b: number): number {
  return a + b;
}

export function multiply(a: number, b: number): number {
  return a * b;
}
",
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");

    assert!(result.success);
    assert_eq!(result.files_indexed, 1);
    assert!(result.nodes_created >= 2);

    let nodes = queries.get_nodes_by_file("src/utils.ts").unwrap();
    assert!(nodes.len() >= 2);

    let add_func = nodes.iter().find(|n| n.name == "add").expect("add");
    assert_eq!(add_func.kind, NodeKind::Function);
}

#[test]
fn full_indexing_indexes_multiple_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();

    fs::write(
        src_dir.join("math.ts"),
        "export function add(a: number, b: number) { return a + b; }",
    )
    .unwrap();
    fs::write(
        src_dir.join("string.ts"),
        "export function capitalize(s: string) { return s.toUpperCase(); }",
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");

    assert!(result.success);
    assert_eq!(result.files_indexed, 2);

    let files = queries.get_all_files().unwrap();
    assert_eq!(files.len(), 2);
    let mut paths: Vec<&str> = files.iter().map(|file| file.path.as_str()).collect();
    paths.sort_unstable();
    assert_eq!(paths, vec!["src/math.ts", "src/string.ts"]);
    assert!(
        files
            .iter()
            .all(|file| file.language == Language::Typescript)
    );
}

#[test]
fn full_indexing_tracks_file_hashes_for_incremental_updates() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    fs::write(src_dir.join("main.ts"), "export const x = 1;").unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    orch.index_all(None, None, false).expect("index_all");

    let file = queries.get_file_by_path("src/main.ts").unwrap();
    let file = file.expect("tracked file");
    assert!(!file.content_hash.is_empty());

    fs::write(src_dir.join("main.ts"), "export const x = 2;").unwrap();

    let changes = orch.get_changed_files().expect("get_changed_files");
    assert!(changes.modified.contains(&"src/main.ts".to_string()));
}

#[test]
fn full_indexing_syncs_and_detects_changes() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    fs::write(
        src_dir.join("main.ts"),
        "export function original() { return 1; }",
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    orch.index_all(None, None, false).expect("index_all");

    let initial_nodes = queries.get_nodes_by_file("src/main.ts").unwrap();
    assert!(initial_nodes.iter().any(|n| n.name == "original"));

    fs::write(
        src_dir.join("main.ts"),
        "export function updated() { return 2; }",
    )
    .unwrap();

    let sync_result = orch.sync(None).expect("sync");
    assert_eq!(sync_result.files_modified, 1);

    let updated_nodes = queries.get_nodes_by_file("src/main.ts").unwrap();
    assert!(updated_nodes.iter().any(|n| n.name == "updated"));
    assert!(!updated_nodes.iter().any(|n| n.name == "original"));
}

#[test]
fn full_indexing_sync_refreshes_metadata_when_content_hash_is_unchanged() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    let file_path = src_dir.join("main.ts");
    let content = "export function kept() { return 1; }\n";
    fs::write(&file_path, content).unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    orch.index_all(None, None, false).expect("index_all");

    let before = queries
        .get_file_by_path("src/main.ts")
        .unwrap()
        .expect("tracked file");
    let mut current_stats = FileStats::from_metadata(&fs::metadata(&file_path).unwrap());
    for _ in 0..50 {
        std::thread::sleep(std::time::Duration::from_millis(25));
        fs::write(&file_path, format!("{content}\n")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(25));
        fs::write(&file_path, content).unwrap();
        current_stats = FileStats::from_metadata(&fs::metadata(&file_path).unwrap());
        if current_stats.modified_at_ms != before.modified_at {
            break;
        }
    }
    assert_ne!(
        current_stats.modified_at_ms, before.modified_at,
        "test setup failed to change mtime"
    );

    let sync_result = orch.sync(None).expect("sync");

    assert_eq!(sync_result.files_modified, 0);
    let after = queries
        .get_file_by_path("src/main.ts")
        .unwrap()
        .expect("tracked file after sync");
    assert_eq!(after.content_hash, before.content_hash);
    assert_eq!(after.modified_at, current_stats.modified_at_ms);
    assert_eq!(after.node_count, before.node_count);
    let nodes = queries.get_nodes_by_file("src/main.ts").unwrap();
    assert!(nodes.iter().any(|n| n.name == "kept"));
}
