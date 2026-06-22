mod git_based_sync {
    use super::*;

    #[test]
    fn detects_modified_files_via_git() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        write(
            &dir.path().join("src/index.ts"),
            "export function hello() { return 'modified'; }",
        );

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_modified, 1);
        assert!(
            result
                .changed_file_paths
                .as_deref()
                .unwrap_or(&[])
                .contains(&"src/index.ts".to_string())
        );
    }

    #[test]
    fn detects_new_untracked_files_via_git() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        write(
            &dir.path().join("src/new.ts"),
            "export function newFunc() { return 42; }",
        );

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 1);
        assert!(
            result
                .changed_file_paths
                .as_deref()
                .unwrap_or(&[])
                .contains(&"src/new.ts".to_string())
        );

        // Verify the function was indexed
        assert!(search_count(&cg, "newFunc") > 0);
    }

    #[test]
    fn stops_reporting_untracked_files_once_indexed_issue_206() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        // Untracked files stay `??` in git status even after codegraph indexes
        // them. Change detection must compare them against the DB by hash, not
        // report every untracked file as "added" on every sync/status.
        write(
            &dir.path().join("src/new.ts"),
            "export function newFunc() { return 42; }",
        );

        // First sync indexes the untracked file.
        let first = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(first.files_added, 1);

        // The file is still untracked in git, but now lives in the DB.
        assert!(search_count(&cg, "newFunc") > 0);

        // status must not keep flagging it as a pending addition...
        let changes = cg.get_changed_files().unwrap();
        assert!(!changes.added.contains(&"src/new.ts".to_string()));
        assert!(!changes.modified.contains(&"src/new.ts".to_string()));

        // ...and a second sync must be a no-op for it.
        let second = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(second.files_added, 0);
        assert_eq!(second.files_modified, 0);
    }

    #[test]
    fn reindexes_an_untracked_file_when_its_contents_change() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        let file_path = dir.path().join("src/new.ts");
        write(&file_path, "export function newFunc() { return 42; }");
        cg.sync(&IndexOptions::default()).unwrap();

        // Modify the still-untracked file.
        write(&file_path, "export function renamedFunc() { return 7; }");

        let changes = cg.get_changed_files().unwrap();
        assert!(changes.modified.contains(&"src/new.ts".to_string()));

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_modified, 1);
        assert!(search_count(&cg, "renamedFunc") > 0);
        assert_eq!(search_count(&cg, "newFunc"), 0);
    }

    #[test]
    fn detects_deleted_files_via_git() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        fs::remove_file(dir.path().join("src/index.ts")).unwrap();

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_removed, 1);

        // Verify function is gone
        assert_eq!(search_count(&cg, "hello"), 0);
    }

    #[test]
    fn indexes_a_tracked_file_that_grows_large_instead_of_dropping_it() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        assert!(search_count(&cg, "hello") > 0);

        // There is no size cap: a file that grows past 1 MiB is re-indexed
        // (not purged), so its new symbol appears and the old one is gone.
        let mut oversized = String::from("export function replacement() { return 1; }\n");
        oversized.push_str(&"x".repeat(2 * 1024 * 1024));
        write(&dir.path().join("src/index.ts"), &oversized);

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_modified, 1);
        assert_eq!(search_count(&cg, "hello"), 0);
        assert!(search_count(&cg, "replacement") > 0);
    }

    #[test]
    fn resolves_existing_unresolved_refs_when_a_later_sync_adds_the_target_symbol() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        write(
            &dir.path().join("src/index.ts"),
            "export function caller() { return missingTarget(); }",
        );
        cg.sync(&IndexOptions::default()).unwrap();

        write(
            &dir.path().join("src/target.ts"),
            "export function missingTarget() { return 42; }",
        );

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 1);

        let target = cg
            .search_nodes("missingTarget", None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.kind == NodeKind::Function)
            .expect("target function should be indexed");
        let callers = cg.get_callers(&target.id, None).unwrap();
        assert!(callers.iter().any(|r| r.node.name == "caller"));
    }

    #[test]
    fn skips_files_with_unsupported_extensions() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        // A .txt file has no supported grammar, so sync must not index it.
        write(&dir.path().join("src/notes.txt"), "just some notes");

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 0);
        assert_eq!(result.files_modified, 0);
    }

    #[test]
    fn reports_no_changes_on_clean_working_tree() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        let result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(result.files_added, 0);
        assert_eq!(result.files_modified, 0);
        assert_eq!(result.files_removed, 0);
        // TS: `expect(result.changedFilePaths).toBeUndefined()`
        assert!(result.changed_file_paths.is_none());
    }

    #[test]
    fn reports_files_changed_on_disk_even_when_git_status_is_clean() {
        let dir = TempDir::new().unwrap();
        let cg = setup_git_indexed(dir.path());

        write(
            &dir.path().join("src/index.ts"),
            "export function hello() { return 'from second commit'; }",
        );
        git(dir.path(), &["add", "-A"]);
        git(dir.path(), &["commit", "-q", "-m", "second"]);
        cg.sync(&IndexOptions::default()).unwrap();

        // Move the working tree to a different committed version. `git status`
        // is clean afterward, but CodeGraph's DB still reflects the first commit.
        git(dir.path(), &["checkout", "-q", "HEAD~1"]);
        let status = git_stdout(dir.path(), &["status", "--porcelain"]);
        assert!(!status.contains("src/index.ts"));

        let changes = cg.get_changed_files().unwrap();
        assert!(changes.modified.contains(&"src/index.ts".to_string()));
    }
}

// =============================================================================
// concurrent-locking.test.ts — issue #238 (DB pragmas + WAL concurrency)
// =============================================================================
