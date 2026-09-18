use std::path::PathBuf;

use super::*;
use crate::history::JfcLogs;

/// Two sessions in one repo, five calls: span-duplicated Bash, Read and Edit
/// calls, a secret in a command, and Reads announced before the session's root.
const SESSION_1: &str = "\
2026-06-01T10:00:00.000001Z DEBUG s{}: jfc::stream: tool_done index=0 tool_name=Read tool_use_id=toolu_r1 input_len=1
2026-06-01T10:00:00.000002Z DEBUG dispatch_tools_batched{n=1}: jfc::agents: loading agents project_root=/nonexistent/repo
2026-06-01T10:00:00.000003Z DEBUG execute_tool{active_team_name=None kind=Read}: jfc::tools: read: starting file_path=/nonexistent/repo/src/a.rs
2026-06-01T10:00:00.000004Z DEBUG execute_tool{active_team_name=None kind=Read}: jfc::tools: read: success file_path=/nonexistent/repo/src/a.rs line_count=3
2026-06-01T10:00:00.000005Z DEBUG s{}: jfc::stream: tool_done index=1 tool_name=Edit tool_use_id=toolu_e1 input_len=1
2026-06-01T10:00:00.000006Z  INFO execute_tool{active_team_name=None kind=Edit}: jfc::tools: edit: starting file_path=/nonexistent/repo/src/b.rs old_len=1 new_len=2 replace_all=false
2026-06-01T10:00:00.000007Z DEBUG execute_tool{active_team_name=None kind=Edit}: jfc::tools: edit: success file_path=/nonexistent/repo/src/b.rs count=1
2026-06-01T10:00:00.000008Z DEBUG s{}: jfc::stream: tool_done index=2 tool_name=Bash tool_use_id=toolu_b1 input_len=1
2026-06-01T10:00:00.000009Z  INFO jfc::ui::tool: StreamTool received tool_kind=Bash tool_id=toolu_b1 auto_mode=true
2026-06-01T10:00:00.000010Z  INFO execute_tool{active_team_name=None kind=Bash}: jfc::tools: bash: executing cmd=cd /nonexistent/repo && echo 'Hunter2Pass' | sudo -S cargo test timeout_ms=1 cwd=/nonexistent
2026-06-01T10:00:00.000011Z DEBUG execute_tool{active_team_name=None kind=Bash}: jfc::tools: bash: completed exit_code=0 stdout_len=1 stderr_len=0
";

const SESSION_2: &str = "\
2026-06-02T10:00:00.000001Z DEBUG s{}: jfc::stream: tool_done index=0 tool_name=Read tool_use_id=toolu_r2 input_len=1
2026-06-02T10:00:00.000002Z DEBUG s{}: jfc::stream: tool_done index=1 tool_name=Read tool_use_id=toolu_r3 input_len=1
2026-06-02T10:00:00.000003Z DEBUG dispatch_tools_batched{n=2}: jfc::agents: loading agents project_root=/nonexistent/repo
2026-06-02T10:00:00.000004Z DEBUG execute_tool{active_team_name=None kind=Read}: jfc::tools: read: starting file_path=/nonexistent/repo/src/a.rs
2026-06-02T10:00:00.000005Z DEBUG execute_tool{active_team_name=None kind=Read}: jfc::tools: read: starting file_path=/nonexistent/repo/src/b.rs offset=1 limit=2
";

fn fixture_logs() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("ses_20260601_100000.log"), SESSION_1).unwrap();
    std::fs::write(dir.path().join("ses_20260602_100000.log"), SESSION_2).unwrap();
    dir
}

fn db_path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join("state/nested/history.db")
}

#[test]
fn ingest_twice_keeps_one_row_per_call() {
    let logs = fixture_logs();
    let out = tempfile::tempdir().unwrap();
    let mut db = HistoryDb::open(&db_path(&out)).unwrap();
    let source = JfcLogs::new(logs.path());

    let first = db
        .ingest_source(&source, &IngestOptions::default())
        .unwrap();
    assert_eq!(first.source.inputs, 2);
    assert_eq!(first.source.calls, 5);
    assert_eq!(first.inserted, 5);
    assert_eq!(first.already_present, 0);
    assert_eq!(first.redacted, 1);
    assert_eq!(db.count(None).unwrap(), 5);

    let second = db
        .ingest_source(&source, &IngestOptions::default())
        .unwrap();
    assert_eq!(second.inserted, 0);
    assert_eq!(second.already_present, 5);
    assert_eq!(db.count(None).unwrap(), 5, "re-ingest must not duplicate");

    // Reopening (re-running migrations) keeps the rows.
    drop(db);
    let db = HistoryDb::open(&db_path(&out)).unwrap();
    assert_eq!(db.count(None).unwrap(), 5);
    assert_eq!(db.schema_version().unwrap(), schema::CURRENT_VERSION);
}

#[test]
fn project_scope_sees_file_rows_and_commands_are_profiled() {
    let logs = fixture_logs();
    let mut db = HistoryDb::open_in_memory().unwrap();
    db.ingest_source(&JfcLogs::new(logs.path()), &IngestOptions::default())
        .unwrap();

    // Every row — Read/Edit included — is attributed to the repo.
    assert_eq!(db.count(Some("nonexistent/repo")).unwrap(), 5);
    assert_eq!(db.count(Some("elsewhere")).unwrap(), 0);
    let files = db.hot_files(Some("nonexistent/repo"), 10).unwrap();
    assert_eq!(
        files,
        [
            ("/nonexistent/repo/src/a.rs".to_owned(), 2),
            ("/nonexistent/repo/src/b.rs".to_owned(), 2)
        ]
    );
    let tools = db.hot_tools(Some("repo"), 10).unwrap();
    assert_eq!(tools[0], ("Read".to_owned(), 3));

    // `cd … && …` profiles as the command it was for, not `cd`.
    assert_eq!(
        db.hot_commands(None, 10).unwrap(),
        [("cargo".to_owned(), 1)]
    );
    assert_eq!(
        db.hot_chains(None, 10).unwrap(),
        [("cd | echo | cargo".to_owned(), 1)]
    );
    // a.rs and b.rs are touched together in both sessions.
    assert_eq!(
        db.co_access(Some("repo"), 10).unwrap(),
        [(
            "/nonexistent/repo/src/a.rs".to_owned(),
            "/nonexistent/repo/src/b.rs".to_owned(),
            2
        )]
    );
}

#[test]
fn no_secret_reaches_the_file() {
    let logs = fixture_logs();
    let out = tempfile::tempdir().unwrap();
    let path = db_path(&out);
    let mut db = HistoryDb::open(&path).unwrap();
    db.ingest_source(&JfcLogs::new(logs.path()), &IngestOptions::default())
        .unwrap();
    drop(db);
    let bytes = std::fs::read(&path).unwrap();
    let hay = String::from_utf8_lossy(&bytes);
    assert!(!hay.contains("Hunter2Pass"));
    assert!(!hay.contains("toolu_b1"), "native ids are stored hashed");
}

#[test]
fn explicit_project_overrides_attribution() {
    let logs = fixture_logs();
    let mut db = HistoryDb::open_in_memory().unwrap();
    let opts = IngestOptions {
        project: Some("/pinned".to_owned()),
    };
    db.ingest_source(&JfcLogs::new(logs.path()), &opts).unwrap();
    assert_eq!(db.count(Some("/pinned")).unwrap(), 5);
}

#[test]
fn read_only_open_never_creates_state() {
    let out = tempfile::tempdir().unwrap();
    let path = db_path(&out);
    assert!(HistoryDb::open_read_only(&path).unwrap().is_none());
    assert!(!path.exists());
    assert!(
        !out.path().join("state").exists(),
        "no directory may be created"
    );

    // An empty file is "no history yet" too, and stays empty.
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"").unwrap();
    assert!(HistoryDb::open_read_only(&path).unwrap().is_none());
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
}

#[test]
fn read_only_open_reads_what_ingest_wrote() {
    let logs = fixture_logs();
    let out = tempfile::tempdir().unwrap();
    let path = db_path(&out);
    HistoryDb::open(&path)
        .unwrap()
        .ingest_source(&JfcLogs::new(logs.path()), &IngestOptions::default())
        .unwrap();
    let db = HistoryDb::open_read_only(&path)
        .unwrap()
        .expect("history exists");
    assert_eq!(db.count(None).unwrap(), 5);
}

#[test]
fn legacy_db_is_readable_then_migrated_on_write() {
    let out = tempfile::tempdir().unwrap();
    let path = out.path().join("history.db");
    {
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE tool_events (
                 id INTEGER PRIMARY KEY AUTOINCREMENT, ts TEXT, session TEXT, project TEXT,
                 tool_kind TEXT NOT NULL, primary_cmd TEXT, chain TEXT, path TEXT,
                 redacted INTEGER NOT NULL DEFAULT 0);
             INSERT INTO tool_events (tool_kind, primary_cmd) VALUES ('Bash', 'cd');",
        )
        .unwrap();
    }
    // `show` on an old DB: readable as is, and left untouched.
    let ro = HistoryDb::open_read_only(&path).unwrap().unwrap();
    assert_eq!(ro.count(None).unwrap(), 1);
    assert_eq!(ro.schema_version().unwrap(), 0);
    drop(ro);
    // `ingest` migrates, dropping the line-keyed legacy rows.
    let db = HistoryDb::open(&path).unwrap();
    assert_eq!(db.schema_version().unwrap(), schema::CURRENT_VERSION);
    assert_eq!(db.count(None).unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn created_db_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;

    let out = tempfile::tempdir().unwrap();
    let path = db_path(&out);
    HistoryDb::open(&path).unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    assert_eq!(mode(&out.path().join("state")), 0o700);
}

#[test]
fn ingest_calls_shares_the_redacting_writer() {
    let mut db = HistoryDb::open_in_memory().unwrap();
    let raw = RawToolCall {
        native_id: "n1".into(),
        tool: "Bash".into(),
        command: Some("PGPASSWORD=pg_secret_1 psql -h db".into()),
        cwd: Some("/nonexistent/w".into()),
        ..Default::default()
    };
    let report = db
        .ingest_calls("test", [raw.clone(), raw], &IngestOptions::default())
        .unwrap();
    assert_eq!(
        (report.inserted, report.already_present, report.redacted),
        (1, 1, 2)
    );
    assert_eq!(db.hot_commands(None, 5).unwrap(), [("psql".to_owned(), 1)]);
}
