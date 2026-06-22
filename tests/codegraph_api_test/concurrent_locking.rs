mod concurrent_locking {
    use codegraph::DatabaseConnection;

    use super::*;

    #[test]
    fn uses_a_bounded_busy_timeout_not_the_old_2_minute_hang() {
        let dir = TempDir::new().unwrap();
        let conn = DatabaseConnection::initialize(dir.path().join("codegraph.db")).unwrap();
        let db = conn.get_db().unwrap();
        let ms: i64 = db
            .conn()
            .query_row("PRAGMA busy_timeout", [], |r| r.get(0))
            .unwrap();
        assert!(ms > 0);
        assert!(ms <= 30_000); // far below the old 120000
    }

    #[test]
    fn runs_in_wal_mode_and_get_journal_mode_surfaces_it() {
        let dir = TempDir::new().unwrap();
        let conn = DatabaseConnection::initialize(dir.path().join("codegraph.db")).unwrap();
        assert_eq!(conn.get_journal_mode().unwrap(), "wal");
    }

    #[test]
    fn a_read_on_a_2nd_connection_succeeds_while_a_writer_holds_the_lock() {
        let dir = TempDir::new().unwrap();
        let db_path = dir.path().join("codegraph.db");
        let writer = DatabaseConnection::initialize(&db_path).unwrap();
        // The property only holds under WAL; skip if the filesystem couldn't
        // enable it (TS skips the same way).
        if writer.get_journal_mode().unwrap() != "wal" {
            return;
        }
        let reader = DatabaseConnection::open(&db_path).unwrap();
        let writer_db = writer.get_db().unwrap();
        writer_db.conn().execute_batch("BEGIN EXCLUSIVE").unwrap(); // hard write lock, held open

        let t0 = Instant::now();
        let count: i64 = reader
            .get_db()
            .unwrap()
            .conn()
            .query_row("SELECT COUNT(*) AS c FROM nodes", [], |r| r.get(0))
            .unwrap();
        let waited = t0.elapsed();

        writer_db.conn().execute_batch("COMMIT").unwrap();

        assert_eq!(count, 0);
        assert!(waited < Duration::from_millis(1000)); // proceeds immediately, no busy wait
    }

    /// Facade-level FileLock contention: a lock file held by a LIVE process
    /// (our own PID stands in for "another process") makes indexAll return the
    /// exact TS lock-failure result and sync return the zero-shape (#449) —
    /// without erroring, and recoverable once the lock clears.
    #[test]
    fn index_all_and_sync_surface_lock_contention_without_erroring() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        let lock_path = dir.path().join(".codegraph").join("codegraph.lock");
        fs::write(&lock_path, format!("{}", std::process::id())).unwrap();

        // indexAll → the TS lock-failure shape
        let result = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(!result.success);
        assert_eq!(result.files_indexed, 0);
        assert_eq!(result.duration_ms, 0);
        assert_eq!(
            result.errors[0].message,
            "Could not acquire file lock - another process may be indexing"
        );
        assert_eq!(result.errors[0].severity, Severity::Error);

        // sync → the exact zero-shape the watcher detects (#449)
        let sync_result = cg.sync(&IndexOptions::default()).unwrap();
        assert_eq!(sync_result.files_checked, 0);
        assert_eq!(sync_result.duration_ms, 0);

        // The foreign lock must not be deleted by our failed attempts.
        assert!(lock_path.exists());

        // Once the other "process" releases, operations succeed again.
        fs::remove_file(&lock_path).unwrap();
        let result = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(result.success);
        let sync_result = cg.sync(&IndexOptions::default()).unwrap();
        assert!(sync_result.files_checked > 0);
    }

    /// A stale lock from a dead process is taken over (FileLock semantics
    /// surfaced through the facade).
    #[test]
    fn index_all_takes_over_a_stale_lock_from_a_dead_process() {
        let dir = TempDir::new().unwrap();
        let cg = setup_indexed(dir.path());

        let lock_path = dir.path().join(".codegraph").join("codegraph.lock");
        fs::write(&lock_path, "99999999").unwrap(); // dead PID

        let result = cg.index_all(&IndexOptions::default()).unwrap();
        assert!(result.success);
        // Our run released the lock afterwards.
        assert!(!lock_path.exists());
    }
}
