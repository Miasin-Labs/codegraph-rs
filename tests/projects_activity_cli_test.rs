//! Agent activity per atlas project, end to end through the built CLI:
//! `projects list|show` carry what sessions did in each project (and in
//! linked ones), and `history recall` resolves `--project` by atlas name
//! and follows `--related` links. Every run points `CODEGRAPH_HOME` at a
//! temp dir; nothing reads or writes the developer's stores.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use codegraph::history::sources::{RawPrompt, RawSession, SourceEvent, SourceStats, Visit};
use codegraph::history::{
    CallResult,
    EventSource,
    HistoryDb,
    HistoryError,
    IncrementalOptions,
    RawToolCall,
    now_ms,
};
use serde_json::Value;

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_HOME", home)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_BACKGROUND_SYNC", "1")
        .env("CODEGRAPH_TELEMETRY", "0")
        .env_remove("CODEGRAPH_ATLAS")
        .env_remove("CODEGRAPH_HISTORY_DB")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph")
}

fn json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let out = run(cwd, home, args);
    assert!(
        out.status.success(),
        "{args:?}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("JSON output")
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Replays fixed session events into the history store.
struct Replay(Vec<SourceEvent>);

impl EventSource for Replay {
    fn id(&self) -> &'static str {
        "replay"
    }

    fn location(&self) -> String {
        "fixture".into()
    }

    fn visit_events(
        &self,
        _visit: &Visit<'_>,
        sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
    ) -> Result<SourceStats, HistoryError> {
        for e in &self.0 {
            sink(e.clone())?;
        }
        Ok(SourceStats::default())
    }
}

/// One session in `root`, `hours` ago: an edit of `file`, then a build
/// (`failing` or not).
fn session(id: &str, root: &Path, hours: i64, file: &str, failing: bool) -> Vec<SourceEvent> {
    let at = now_ms() - hours * 3_600_000;
    let cwd = root.to_string_lossy().into_owned();
    let call = |n: &str, dt: i64, tool: &str| RawToolCall {
        native_id: format!("{id}-{n}"),
        ts_ms: Some(at + dt),
        session: Some(id.into()),
        tool: tool.into(),
        cwd: Some(cwd.clone()),
        ..Default::default()
    };
    vec![
        SourceEvent::Session(RawSession {
            native_id: id.into(),
            cwd: Some(cwd.clone()),
            started_ms: Some(at),
            ..Default::default()
        }),
        SourceEvent::Prompt(RawPrompt {
            native_id: format!("{id}-p"),
            session: id.into(),
            ts_ms: Some(at + 1),
        }),
        SourceEvent::call(RawToolCall {
            file_path: Some(root.join(file).to_string_lossy().into_owned()),
            ..call("edit", 10, "Edit")
        }),
        SourceEvent::call(RawToolCall {
            command: Some("cargo build".into()),
            result: Some(CallResult {
                is_error: Some(failing),
                exit_code: Some(if failing { 101 } else { 0 }),
                excerpt: failing.then(|| "error[E0599]: no method named `frob`".into()),
                ..Default::default()
            }),
            ..call("build", 20, "Bash")
        }),
    ]
}

/// `app` depends on `lib` by path; both indexed and registered; lib's
/// session (2h ago) left a failing build, app's (5h ago) built fine.
fn world() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let (app, lib, home) = (base.join("app"), base.join("lib"), base.join("cg-home"));
    write(
        &app.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nlib = { path = \"../lib\" }\n",
    );
    write(&app.join("src/main.rs"), "fn main() { lib::frob(); }\n");
    write(
        &lib.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    write(&lib.join("src/lib.rs"), "pub fn frob() {}\n");
    for root in [&app, &lib] {
        fs::create_dir_all(root.join(".git")).unwrap();
        let out = run(root, &home, &["init"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let mut events = session("S-app", &app, 5, "src/main.rs", false);
    events.extend(session("S-lib", &lib, 2, "src/lib.rs", true));
    let opts = IncrementalOptions {
        time_budget: None,
        max_events: None,
        git: false,
        ..Default::default()
    };
    HistoryDb::open(&home.join("history.db"))
        .unwrap()
        .ingest_events(&[&Replay(events)], &opts)
        .unwrap();
    (tmp, app, lib, home)
}

fn listed<'a>(list: &'a Value, name: &str) -> &'a Value {
    list["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("{name} not listed: {list}"))
}

#[test]
fn projects_carry_agent_activity_and_recall_follows_links() {
    let (tmp, app, lib, home) = world();
    let cwd = tmp.path();

    let list = json(cwd, &home, &["projects", "--json"]);
    for name in ["app", "lib"] {
        let p = listed(&list, name);
        assert_eq!(p["sessions30d"], 1, "{p}");
        assert!(p["lastActivityMs"].as_i64().is_some(), "{p}");
    }
    let newest = json(
        cwd,
        &home,
        &[
            "projects", "list", "--sort", "activity", "--limit", "1", "--json",
        ],
    );
    let rows = newest["projects"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["name"], "lib", "lib's session is the latest");

    let shown = json(cwd, &home, &["projects", "show", "app", "--json"]);
    let activity = &shown["activity"];
    assert_eq!(activity["episodes"][0]["files"][0]["path"], "src/main.rs");
    assert_eq!(activity["sessions30d"], 1);
    let linked = &shown["linkedActivity"][0];
    assert_eq!(linked["project"], "lib", "{shown}");
    assert_eq!(linked["relation"], "dependency");
    assert_eq!(linked["openFailures"][0]["codes"][0], "E0599");
    assert!(linked["sharedEditMs"].as_i64().is_some(), "lib was edited");

    let text = run(cwd, &home, &["projects", "show", "app"]);
    let text = String::from_utf8_lossy(&text.stdout);
    assert!(text.contains("Recent agent activity"), "{text}");
    assert!(text.contains("Linked projects' agent activity"), "{text}");

    // `--project` by atlas name, from outside both checkouts.
    let recall = json(
        cwd,
        &home,
        &[
            "history",
            "recall",
            "failures",
            "--project",
            "app",
            "--related",
            "--json",
        ],
    );
    assert!(
        recall["failures"].as_array().is_none(),
        "app's build passed: {recall}"
    );
    assert_eq!(recall["related"][0]["project"], "lib", "{recall}");
    assert_eq!(recall["related"][0]["failures"][0]["codes"][0], "E0599");
    let by_name = json(
        cwd,
        &home,
        &["history", "recall", "--project", "lib", "--json"],
    );
    assert_eq!(by_name["episodes"][0]["files"][0]["path"], "src/lib.rs");
    let unknown = run(cwd, &home, &["history", "recall", "--project", "nope"]);
    assert!(!unknown.status.success());

    let mut names: Vec<String> = fs::read_dir(&home)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert!(
        !names
            .iter()
            .any(|n| n.ends_with("-wal") || n.ends_with("-shm")),
        "reads leave no side files: {names:?}"
    );
    let _ = (app, lib);
}
