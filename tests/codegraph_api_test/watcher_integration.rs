mod watcher_integration {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn watch_and_unwatch_via_codegraph_api() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path()).await;

        assert!(!cg.is_watching());

        let started = cg.watch(WatchOptions {
            // Test-only debounce: long enough to exercise timer wiring, short enough for the suite.
            debounce_ms: Some(200),
            inert_for_tests: true,
            ..Default::default()
        });
        assert!(started);
        assert!(cg.is_watching());
        assert!(cg.get_pending_files().is_empty());

        cg.unwatch();
        assert!(!cg.is_watching());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stops_watching_on_close() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path()).await;

        cg.watch(WatchOptions {
            debounce_ms: Some(200),
            inert_for_tests: true,
            ..Default::default()
        });
        assert!(cg.is_watching());

        cg.close();
        assert!(!cg.is_watching());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn auto_syncs_when_files_change_while_watching_real_watcher_end_to_end() {
        // The one test that exercises the genuine native watcher: a real file
        // write must propagate through OS events → debounce → sync into the
        // graph. The sync runs on the watcher's worker thread via a fresh
        // short-lived CodeGraph instance (this one is !Send); WAL makes the
        // write visible to this connection.
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path()).await;

        let initial_nodes = cg.get_stats().unwrap().node_count;

        let started = cg.watch(WatchOptions {
            debounce_ms: Some(300),
            ..Default::default()
        });
        if !started {
            // Watch policy can disable watching in constrained environments
            // (e.g. CODEGRAPH_NO_WATCH, WSL) — nothing to assert then.
            return;
        }
        // Let the watcher install before writing, so the event isn't missed.
        cg.wait_until_watcher_ready(None).unwrap();

        // Real fs write — no synthetic event. The live watcher must catch it.
        write(
            &dir.path().join("src/added.ts"),
            "export function added() { return 42; }",
        );

        // Wait for auto-sync to pick it up (real OS event delivery + debounce).
        wait_for(
            || cg.get_stats().unwrap().node_count > initial_nodes,
            Duration::from_secs(8),
            "auto-sync to index the new file",
        );

        // The new function should be in the graph.
        assert!(search_count(&cg, "added") > 0);

        cg.unwatch();
    }
}
