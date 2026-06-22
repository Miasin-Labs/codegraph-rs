mod foundation_facade {
    use super::*;

    #[test]
    fn open_sync_errors_on_uninitialized_project() {
        let dir = TempDir::new().unwrap();
        let err = CodeGraph::open_sync(dir.path()).unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("not initialized"),
            "error should match /not initialized/i, got: {err}"
        );
    }

    #[test]
    fn init_sync_errors_when_already_initialized() {
        let dir = TempDir::new().unwrap();
        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        cg.close();
        let err = CodeGraph::init_sync(dir.path()).unwrap_err().to_string();
        assert!(
            err.to_lowercase().contains("already initialized"),
            "error should match /already initialized/i, got: {err}"
        );
    }

    #[test]
    fn open_sync_returns_a_working_instance_with_project_root() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());
        cg.close();

        assert!(CodeGraph::is_initialized(dir.path()));
        let reopened = CodeGraph::open_sync(dir.path()).unwrap();
        // path.resolve parity: the resolved root locates the same directory
        assert!(reopened.get_project_root().join(".codegraph").is_dir());
        assert!(search_count(&reopened, "hello") > 0);
    }

    #[test]
    fn get_stats_optimize_and_clear() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        let stats = cg.get_stats().unwrap();
        assert!(stats.node_count > 0);
        assert!(stats.edge_count > 0);
        assert_eq!(stats.file_count, 1);
        assert!(stats.db_size_bytes > 0);

        cg.optimize().unwrap();

        cg.clear().unwrap();
        let cleared = cg.get_stats().unwrap();
        assert_eq!(cleared.node_count, 0);
        assert_eq!(cleared.edge_count, 0);
        assert_eq!(cleared.file_count, 0);
    }

    #[test]
    fn backend_and_journal_mode_surface_through_the_facade() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());
        assert_eq!(cg.get_backend().as_str(), "native");
        assert_eq!(cg.get_journal_mode().unwrap(), "wal");
        assert!(cg.get_last_indexed_at().unwrap().is_some());
    }

    #[test]
    #[allow(deprecated)]
    fn destroy_alias_closes_but_keeps_codegraph_dir() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());
        cg.destroy();
        assert!(dir.path().join(".codegraph").is_dir());
    }

    #[test]
    fn uninitialize_removes_the_codegraph_dir() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());
        cg.uninitialize().unwrap();
        assert!(!dir.path().join(".codegraph").exists());
        assert!(!CodeGraph::is_initialized(dir.path()));
    }

    #[test]
    fn graph_query_methods_handle_unknown_nodes() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        // getContext on a missing node → Err "Node not found: <id>"
        let err = cg.get_context("nonexistent").unwrap_err().to_string();
        assert!(err.contains("Node not found"), "got: {err}");

        // Traversals/usages on unknown ids → empty results (TS parity)
        assert!(cg.traverse("nonexistent", None).unwrap().nodes.is_empty());
        assert!(
            cg.get_call_graph("nonexistent", None)
                .unwrap()
                .nodes
                .is_empty()
        );
        assert!(
            cg.get_type_hierarchy("nonexistent")
                .unwrap()
                .nodes
                .is_empty()
        );
        assert!(cg.find_usages("nonexistent").unwrap().is_empty());
    }

    #[test]
    fn is_indexing_is_false_outside_and_true_inside_a_progress_callback() {
        let dir = TempDir::new().unwrap();
        write(
            &dir.path().join("src/index.ts"),
            "export function hello() { return 'world'; }",
        );
        let cg = CodeGraph::init_sync(dir.path()).unwrap();
        assert!(!cg.is_indexing());

        let observed = std::cell::Cell::new(false);
        let cg_ref = &cg;
        let on_progress = |_p: &codegraph::IndexProgress| {
            if cg_ref.is_indexing() {
                observed.set(true);
            }
        };
        cg.index_all(&IndexOptions {
            on_progress: Some(&on_progress),
            ..Default::default()
        })
        .unwrap();

        assert!(
            observed.get(),
            "is_indexing() should be true during indexAll"
        );
        assert!(!cg.is_indexing());
    }
}
