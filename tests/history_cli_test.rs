//! `codegraph history` end to end against the built binary: ingest is
//! idempotent and redacts, `show` is read-only and never creates state.

use std::path::Path;
use std::process::{Command, Output, Stdio};

fn run(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .current_dir(home)
        .env("HOME", home)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_TELEMETRY", "0")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph binary")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn json(out: &Output) -> serde_json::Value {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_str(&stdout(out)).expect("valid JSON")
}

/// One session: a Read, an Edit and a Bash call, each logged on several
/// span lines, the Bash one carrying a `sudo -S` password.
const LOG: &str = "\
2026-06-01T10:00:00.000001Z DEBUG s{}: jfc::stream: tool_done index=0 tool_name=Read tool_use_id=toolu_r1 input_len=1
2026-06-01T10:00:00.000002Z DEBUG dispatch_tools_batched{n=1}: jfc::agents: loading agents project_root=/nonexistent/proj
2026-06-01T10:00:00.000003Z DEBUG execute_tool{active_team_name=None kind=Read}: jfc::tools: read: starting file_path=/nonexistent/proj/src/lib.rs
2026-06-01T10:00:00.000004Z DEBUG execute_tool{active_team_name=None kind=Read}: jfc::tools: read: success file_path=/nonexistent/proj/src/lib.rs line_count=3
2026-06-01T10:00:00.000005Z DEBUG s{}: jfc::stream: tool_done index=1 tool_name=Edit tool_use_id=toolu_e1 input_len=1
2026-06-01T10:00:00.000006Z  INFO execute_tool{active_team_name=None kind=Edit}: jfc::tools: edit: starting file_path=/nonexistent/proj/src/lib.rs old_len=1 new_len=2 replace_all=false
2026-06-01T10:00:00.000007Z DEBUG s{}: jfc::stream: tool_done index=2 tool_name=Bash tool_use_id=toolu_b1 input_len=1
2026-06-01T10:00:00.000008Z  INFO execute_tool{active_team_name=None kind=Bash}: jfc::tools: bash: executing cmd=cd /nonexistent/proj && echo CliSecret99 | sudo -S cargo test timeout_ms=1 cwd=/nonexistent
2026-06-01T10:00:00.000009Z DEBUG execute_tool{active_team_name=None kind=Bash}: jfc::tools: bash: completed exit_code=0 stdout_len=1 stderr_len=0
";

#[test]
fn show_without_history_creates_nothing() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["history", "show"]);
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout(&out).contains("No tool-call history yet"),
        "{}",
        stdout(&out)
    );
    assert!(
        !home.path().join(".codegraph").exists(),
        "`history show` must not create ~/.codegraph"
    );

    let val = json(&run(home.path(), &["history", "show", "--json"]));
    assert_eq!(val["exists"], false);
    assert_eq!(val["total"], 0);
    assert!(!home.path().join(".codegraph").exists());
}

#[test]
fn ingest_twice_then_show_scoped_to_the_project() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join("logs");
    std::fs::create_dir(&logs).unwrap();
    std::fs::write(logs.join("ses_20260601_100000.log"), LOG).unwrap();
    let db = home.path().join("state/history.db");
    let (logs_s, db_s) = (logs.to_string_lossy(), db.to_string_lossy());
    let ingest = ["history", "ingest", "--logs", &logs_s, "--db", &db_s];

    let first = run(home.path(), &ingest);
    assert!(
        first.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        stdout(&first).contains("Ingested 3 new tool call(s)"),
        "{}",
        stdout(&first)
    );
    let second = run(home.path(), &ingest);
    assert!(
        stdout(&second).contains("Ingested 0 new tool call(s)"),
        "{}",
        stdout(&second)
    );

    let all = json(&run(
        home.path(),
        &["history", "show", "--json", "--db", &db_s],
    ));
    assert_eq!(all["exists"], true);
    assert_eq!(
        all["total"], 3,
        "one row per tool call, however often re-ingested"
    );
    assert_eq!(all["hot_commands"][0][0], "cargo");

    // `-p` sees the file rows, not just shell rows.
    let scoped = json(&run(
        home.path(),
        &[
            "history",
            "show",
            "--json",
            "--db",
            &db_s,
            "-p",
            "nonexistent/proj",
        ],
    ));
    assert_eq!(scoped["total"], 3);
    assert_eq!(scoped["hot_files"][0][0], "/nonexistent/proj/src/lib.rs");
    assert_eq!(scoped["hot_files"][0][1], 2);

    let bytes = std::fs::read(&db).unwrap();
    assert!(!String::from_utf8_lossy(&bytes).contains("CliSecret99"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&db).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    // The default path was never touched.
    assert!(!home.path().join(".codegraph").exists());
}

/// `recall` is read-only (no store, nothing created); an incremental ingest
/// of a Claude Code transcript feeds it.
#[test]
fn incremental_ingest_feeds_a_read_only_recall() {
    let home = tempfile::tempdir().unwrap();
    let repo = home.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let db = home.path().join("state/history.db");
    let (repo_s, db_s) = (repo.to_string_lossy(), db.to_string_lossy());

    let before = run(
        home.path(),
        &["history", "recall", "src/", "-p", &repo_s, "--db", &db_s],
    );
    assert!(
        before.status.success(),
        "{}",
        String::from_utf8_lossy(&before.stderr)
    );
    assert!(stdout(&before).contains("no agent history recorded yet"));
    assert!(!db.exists(), "recall must not create the store");

    let projects = home.path().join("cc");
    let transcript = projects.join("-repo/s1.jsonl");
    std::fs::create_dir_all(transcript.parent().unwrap()).unwrap();
    let cwd: &str = &repo_s;
    let lines = [
        format!(
            r#"{{"type":"user","uuid":"u1","cwd":"{cwd}","timestamp":"2026-09-10T10:00:01.000Z","message":{{"role":"user","content":"look at lib"}}}}"#
        ),
        format!(
            r#"{{"type":"assistant","uuid":"a1","cwd":"{cwd}","timestamp":"2026-09-10T10:00:02.000Z","message":{{"role":"assistant","content":[{{"type":"tool_use","id":"toolu_1","name":"Read","input":{{"file_path":"{cwd}/src/lib.rs"}}}}]}}}}"#
        ),
        format!(
            r#"{{"type":"user","uuid":"u2","cwd":"{cwd}","timestamp":"2026-09-10T10:00:03.000Z","message":{{"role":"user","content":[{{"type":"tool_result","tool_use_id":"toolu_1","content":"pub fn f() {{}}"}}]}}}}"#
        ),
    ];
    std::fs::write(&transcript, lines.join("\n") + "\n").unwrap();
    let projects_s = projects.to_string_lossy();
    let ingest = [
        "history",
        "ingest",
        "--incremental",
        "--source",
        "claude-code",
        "--claude-dir",
        &projects_s,
        "--db",
        &db_s,
        "--no-git",
    ];
    let out = run(home.path(), &ingest);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout(&out).contains("Ingested 1 new tool call(s)"),
        "{}",
        stdout(&out)
    );

    let recall = json(&run(
        home.path(),
        &[
            "history", "recall", "src/", "-p", &repo_s, "--db", &db_s, "--json",
        ],
    ));
    assert_eq!(recall["kind"], "recall");
    assert_eq!(recall["episodes"][0]["source"], "claude-code");
    assert_eq!(recall["episodes"][0]["files"][0]["path"], "src/lib.rs");
    assert_eq!(recall["episodes"][0]["files"][0]["op"], "read");
    assert!(!home.path().join(".codegraph").exists());
}
