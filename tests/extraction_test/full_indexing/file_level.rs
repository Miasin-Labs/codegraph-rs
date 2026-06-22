use crate::extraction_test::fixture::*;

#[test]
fn full_indexing_counts_file_level_tracked_yaml_files_as_indexed() {
    let temp_dir = tempfile::tempdir().unwrap();
    fs::write(temp_dir.path().join("app.yaml"), "name: test\n").unwrap();
    fs::write(temp_dir.path().join("routes.yml"), "route: value\n").unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");

    assert!(result.success);
    assert_eq!(result.files_indexed, 2);
    assert_eq!(result.files_skipped, 0);
    let mut tracked: Vec<String> = queries
        .get_all_files()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    tracked.sort();
    assert_eq!(tracked, vec!["app.yaml", "routes.yml"]);
}

#[test]
fn full_indexing_counts_file_level_tracked_yaml_twig_files_as_indexed_in_index_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    fs::write(temp_dir.path().join("app.yaml"), "name: test\n").unwrap();
    fs::write(temp_dir.path().join("view.twig"), "{{ title }}\n").unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch
        .index_files(&["app.yaml".to_string(), "view.twig".to_string()])
        .expect("index_files");

    assert!(result.success);
    assert_eq!(result.files_indexed, 2);
    assert_eq!(result.files_skipped, 0);

    let mut tracked: Vec<String> = queries
        .get_all_files()
        .unwrap()
        .into_iter()
        .map(|f| format!("{}:{}", f.path, f.language))
        .collect();
    tracked.sort();
    assert_eq!(tracked, vec!["app.yaml:yaml", "view.twig:twig"]);
}

#[test]
fn full_indexing_counts_file_level_tracked_properties_files_as_indexed() {
    let temp_dir = tempfile::tempdir().unwrap();
    fs::write(
        temp_dir.path().join("application.properties"),
        "server.port=8080\n",
    )
    .unwrap();
    fs::write(temp_dir.path().join("log.properties"), "log.level=INFO\n").unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");

    assert!(result.success);
    assert_eq!(result.files_indexed, 2);
    assert_eq!(result.files_skipped, 0);
}

#[test]
fn full_indexing_counts_the_full_file_level_tracked_class_in_index_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    fs::write(temp_dir.path().join("app.yaml"), "name: test\n").unwrap();
    fs::write(temp_dir.path().join("view.twig"), "{{ title }}\n").unwrap();
    fs::write(
        temp_dir.path().join("application.properties"),
        "server.port=8080\n",
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch
        .index_files(&[
            "app.yaml".to_string(),
            "view.twig".to_string(),
            "application.properties".to_string(),
        ])
        .expect("index_files");

    assert!(result.success);
    assert_eq!(result.files_indexed, 3);
    assert_eq!(result.files_skipped, 0);

    let mut tracked: Vec<String> = queries
        .get_all_files()
        .unwrap()
        .into_iter()
        .map(|f| format!("{}:{}", f.path, f.language))
        .collect();
    tracked.sort();
    assert_eq!(
        tracked,
        vec![
            "app.yaml:yaml",
            "application.properties:properties",
            "view.twig:twig"
        ]
    );
}
