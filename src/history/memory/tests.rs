use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, params};

use super::*;
use crate::history::event::{CallResult, RawToolCall};
use crate::history::ingest::IncrementalOptions;
use crate::history::session_start::session_start_digest_at;
use crate::history::sources::{
    EventSource,
    RawPrompt,
    RawSession,
    SourceEvent,
    SourceStats,
    Visit,
};
use crate::history::store::{HistoryDb, HistoryError};

/// The sessions ran three days ago (the digest looks back 14–30 days).
fn base() -> i64 {
    crate::history::time::now_ms() - 3 * 86_400_000
}

/// Replays a fixed event list; remembers the checkpoints it was handed.
struct Replay {
    events: Vec<SourceEvent>,
    seen_cursors: RefCell<Vec<HashMap<String, String>>>,
}

impl EventSource for Replay {
    fn id(&self) -> &'static str {
        "replay"
    }

    fn location(&self) -> String {
        "fixture".into()
    }

    fn visit_events(
        &self,
        visit: &Visit<'_>,
        sink: &mut dyn FnMut(SourceEvent) -> Result<(), HistoryError>,
    ) -> Result<SourceStats, HistoryError> {
        self.seen_cursors.borrow_mut().push(visit.cursors.clone());
        for e in &self.events {
            sink(e.clone())?;
        }
        Ok(SourceStats::default())
    }
}

/// A repository with a `.git`, a linked worktree, and a minimal codegraph
/// index: `src/parser.rs` (unchanged since the sessions) defines
/// `parse_expr` at line 42; `src/lib.rs` changed after them.
struct Fixture {
    _tmp: tempfile::TempDir,
    base: i64,
    repo: PathBuf,
    worktree: PathBuf,
    db: PathBuf,
}

fn fixture() -> Fixture {
    let base = base();
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(repo.join(".git/worktrees/agent-x")).unwrap();
    std::fs::create_dir_all(repo.join("src")).unwrap();
    let worktree = repo.join(".claude/worktrees/agent-x");
    std::fs::create_dir_all(worktree.join("src")).unwrap();
    std::fs::write(
        worktree.join(".git"),
        format!("gitdir: {}/.git/worktrees/agent-x\n", repo.display()),
    )
    .unwrap();
    let index = crate::db::get_database_path(&repo);
    std::fs::create_dir_all(index.parent().unwrap()).unwrap();
    let conn = Connection::open(&index).unwrap();
    conn.execute_batch(
        "CREATE TABLE files (path TEXT PRIMARY KEY, content_hash TEXT NOT NULL, modified_at INTEGER NOT NULL);
         CREATE TABLE nodes (name TEXT NOT NULL, file_path TEXT NOT NULL, start_line INTEGER NOT NULL);",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO files VALUES ('src/parser.rs', 'h-parser', ?1), ('src/lib.rs', 'h-lib', ?2)",
        params![base - 1_000, base + 500_000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO nodes VALUES ('parse_expr', 'src/parser.rs', 42), ('Lexer', 'src/lib.rs', 3)",
        [],
    )
    .unwrap();
    let db = tmp.path().join("state/history.db");
    Fixture {
        _tmp: tmp,
        base,
        repo,
        worktree,
        db,
    }
}

fn call(base: i64, session: &str, id: &str, at: i64, tool: &str, cwd: &Path) -> RawToolCall {
    RawToolCall {
        native_id: id.into(),
        ts_ms: Some(base + at),
        session: Some(session.into()),
        tool: tool.into(),
        cwd: Some(cwd.to_string_lossy().into_owned()),
        ..Default::default()
    }
}

fn file(path: &Path) -> Option<String> {
    Some(path.to_string_lossy().into_owned())
}

fn failed(text: &str) -> Option<CallResult> {
    Some(CallResult {
        is_error: Some(true),
        exit_code: Some(101),
        output_bytes: text.len() as u64,
        excerpt: Some(text.into()),
    })
}

fn passed() -> Option<CallResult> {
    Some(CallResult {
        exit_code: Some(0),
        is_error: Some(false),
        ..Default::default()
    })
}

/// Two sessions in the fixture repo (session A with a sub-agent working in
/// the worktree, session B a day later).
fn events(f: &Fixture) -> Vec<SourceEvent> {
    let repo = &f.repo;
    let parser = repo.join("src/parser.rs");
    let lib = repo.join("src/lib.rs");
    let session = |id: &str, parent: Option<&str>, cwd: &Path, at: i64| {
        SourceEvent::Session(RawSession {
            native_id: id.into(),
            parent: parent.map(str::to_owned),
            cwd: file(cwd),
            started_ms: Some(f.base + at),
        })
    };
    let prompt = |id: &str, ses: &str, at: i64| {
        SourceEvent::Prompt(RawPrompt {
            native_id: id.into(),
            session: ses.into(),
            ts_ms: Some(f.base + at),
        })
    };
    vec![
        session("A", None, repo, 0),
        prompt("A-p1", "A", 1_000),
        SourceEvent::call(RawToolCall {
            pattern: Some(r"parse_expr|UnknownThing\b".into()),
            ..call(f.base, "A", "a1", 2_000, "Grep", repo)
        }),
        SourceEvent::call(RawToolCall {
            file_path: file(&parser),
            result: Some(CallResult {
                output_bytes: 500,
                ..Default::default()
            }),
            ..call(f.base, "A", "a2", 3_000, "Read", repo)
        }),
        SourceEvent::call(RawToolCall {
            file_path: file(&parser),
            ..call(f.base, "A", "a3", 4_000, "Edit", repo)
        }),
        SourceEvent::call(RawToolCall {
            file_path: file(&lib),
            ..call(f.base, "A", "a4", 5_000, "Edit", repo)
        }),
        SourceEvent::call(RawToolCall {
            command: Some("cd src && cargo test -p parser 2>&1 | tail -30".into()),
            result: failed(
                "error[E0425]: cannot find value `x` in this scope\n --> src/parser.rs:9:5\nerror: could not compile `parser`",
            ),
            ..call(f.base, "A", "a5", 6_000, "Bash", repo)
        }),
        SourceEvent::Checkpoint {
            key: "unit-A".into(),
            value: "1".into(),
        },
        prompt("A-p2", "A", 10_000),
        session("A/sub", Some("A"), &f.worktree, 10_500),
        SourceEvent::call(RawToolCall {
            file_path: file(&f.worktree.join("src/parser.rs")),
            ..call(f.base, "A/sub", "s1", 11_000, "Read", &f.worktree)
        }),
        SourceEvent::call(RawToolCall {
            command: Some("cargo test -p parser".into()),
            result: passed(),
            ..call(f.base, "A", "a6", 12_000, "Bash", repo)
        }),
        SourceEvent::call(RawToolCall {
            command: Some("echo 'Sup3rS3cretPw!' | sudo -S systemctl restart thing".into()),
            ..call(f.base, "A", "a7", 13_000, "Bash", repo)
        }),
        SourceEvent::call(RawToolCall {
            pattern: Some(concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123").into()),
            ..call(f.base, "A", "a8", 14_000, "Grep", repo)
        }),
        session("B", None, repo, 86_400_000),
        prompt("B-p1", "B", 86_401_000),
        SourceEvent::call(RawToolCall {
            file_path: file(&parser),
            ..call(f.base, "B", "b1", 86_402_000, "Read", repo)
        }),
        SourceEvent::call(RawToolCall {
            command: Some("cargo check --all-targets".into()),
            result: failed("error[E0308]: mismatched types\n --> src/lib.rs:1:1"),
            ..call(f.base, "B", "b2", 86_403_000, "Bash", repo)
        }),
        SourceEvent::Checkpoint {
            key: "unit-B".into(),
            value: "7".into(),
        },
    ]
}

fn replay(events: Vec<SourceEvent>) -> Replay {
    Replay {
        events,
        seen_cursors: RefCell::new(Vec::new()),
    }
}

fn ingest(db: &Path, source: &Replay) {
    let mut hdb = HistoryDb::open(db).unwrap();
    let opts = IncrementalOptions {
        time_budget: None,
        max_events: None,
        git: false,
        ..Default::default()
    };
    hdb.ingest_events(&[source], &opts).unwrap();
}

/// Every row of every memory table, in a stable order.
fn snapshot(db: &Path) -> Vec<String> {
    let conn = Connection::open(db).unwrap();
    let mut out = Vec::new();
    for table in [
        "repos", "sessions", "episodes", "files", "touches", "idents", "lookups", "outcomes",
        "coedits",
    ] {
        let mut stmt = conn.prepare(&format!("SELECT * FROM {table}")).unwrap();
        let n = stmt.column_count();
        let rows = stmt
            .query_map([], |r| {
                let mut cells = Vec::new();
                for i in 0..n {
                    let v: rusqlite::types::Value = r.get(i)?;
                    cells.push(format!("{v:?}"));
                }
                Ok(format!("{table}: {}", cells.join(" | ")))
            })
            .unwrap();
        out.extend(rows.map(Result::unwrap));
    }
    out.sort();
    out
}

fn recall(f: &Fixture, about: &str) -> RecallReport {
    let req = RecallRequest {
        about: About::parse(about),
        since_ms: None,
        limit: 3,
    };
    recall_at(&f.db, &f.repo, &req, Duration::from_secs(5)).unwrap()
}

#[test]
fn ingest_is_idempotent_and_resumes_from_checkpoints() {
    let f = fixture();
    let source = replay(events(&f));
    ingest(&f.db, &source);
    let first = snapshot(&f.db);
    ingest(&f.db, &source);
    assert_eq!(snapshot(&f.db), first, "a second pass must change nothing");
    let seen = source.seen_cursors.borrow();
    assert!(seen[0].is_empty());
    assert_eq!(seen[1]["unit-A"], "1");
    assert_eq!(seen[1]["unit-B"], "7");
}

#[test]
fn episodes_touches_lookups_and_outcomes_are_recorded() {
    let f = fixture();
    ingest(&f.db, &replay(events(&f)));
    let conn = Connection::open(&f.db).unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(
        count("SELECT COUNT(*) FROM repos"),
        1,
        "the worktree folds into its repo"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM episodes"),
        3,
        "split at the three prompts"
    );
    // The sub-agent's read lands in session A's second episode.
    let second: i64 = conn
        .query_row(
            "SELECT e.calls FROM episodes e JOIN sessions s ON s.id = e.session_id
             ORDER BY e.started_at LIMIT 1 OFFSET 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(second, 4, "sub-agent read + 3 calls of the root");
    let paths: Vec<String> = conn
        .prepare("SELECT path FROM files ORDER BY path")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(paths, ["src/lib.rs", "src/parser.rs"]);
    let names: Vec<String> = conn
        .prepare("SELECT name FROM idents ORDER BY name")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(names.len(), 2);
    assert!(names.contains(&"parse_expr".to_owned()));
    assert!(
        names.iter().any(|n| n.starts_with('#') && n.len() == 17),
        "an identifier the index doesn't know is hashed: {names:?}"
    );
    let found: (String, Option<i64>) = conn
        .query_row(
            "SELECT f.path, l.found_line FROM lookups l JOIN idents i ON i.id = l.ident_id
             JOIN files f ON f.id = l.found_file_id WHERE i.name = 'parse_expr'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(found, ("src/parser.rs".to_owned(), Some(42)));
    let outcome: (String, String, i64, String) = conn
        .query_row(
            "SELECT kind, template, ok, err_codes FROM outcomes ORDER BY ts LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        outcome,
        (
            "test".to_owned(),
            "cargo test -p …".to_owned(),
            0,
            "E0425".to_owned()
        )
    );
    assert_eq!(count("SELECT COUNT(*) FROM coedits WHERE episodes = 1"), 1);
}

#[test]
fn nothing_secret_or_unresolved_reaches_the_store() {
    let f = fixture();
    ingest(&f.db, &replay(events(&f)));
    let dump = snapshot(&f.db).join("\n");
    let conn = Connection::open(&f.db).unwrap();
    let events: Vec<String> = conn
        .prepare("SELECT COALESCE(primary_cmd,'') || ' ' || COALESCE(chain,'') || ' ' || COALESCE(path,'') FROM tool_events")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let all = format!("{dump}\n{}", events.join("\n"));
    for secret in [
        "Sup3rS3cretPw",
        concat!("gh", "p_abcdefghijklmnopqrstuvwxyz0123"),
        "UnknownThing",
        "cannot find value",
    ] {
        assert!(!all.contains(secret), "{secret} leaked into the store");
    }
}

#[test]
fn recall_answers_path_symbol_failures_cochange_and_last() {
    let f = fixture();
    ingest(&f.db, &replay(events(&f)));

    let by_path = recall(&f, "src/");
    assert_eq!(by_path.episodes.len(), 3, "{by_path:#?}");
    let newest = &by_path.episodes[0];
    assert_eq!(newest.outcome, "build-fail");
    let parser = newest
        .files
        .iter()
        .find(|t| t.path == "src/parser.rs")
        .unwrap();
    assert_eq!((parser.op, parser.unchanged), ("read", Some(true)));
    let edited = &by_path.episodes[2];
    let lib = edited
        .files
        .iter()
        .find(|t| t.path == "src/lib.rs")
        .unwrap();
    assert_eq!(
        (lib.op, lib.unchanged),
        ("edit", Some(false)),
        "src/lib.rs changed since"
    );
    // Absolute paths name repository files too.
    let abs = recall(&f, &f.repo.join("src/lib.rs").to_string_lossy());
    assert_eq!(abs.episodes.len(), 1);

    let symbol = recall(&f, "symbol:parse_expr");
    assert_eq!(symbol.symbols[0].found, ["src/parser.rs:42"]);
    let hashed = recall(&f, "symbol:UnknownThing");
    assert_eq!(
        hashed.symbols[0].name, "UnknownThing",
        "found through its hash"
    );
    assert_eq!(hashed.symbols[0].found, ["src/parser.rs"]);

    let failures = recall(&f, "failures");
    let fixed: Vec<(String, bool, Vec<String>)> = failures
        .failures
        .iter()
        .map(|r| (r.command.clone(), r.fixed_later, r.codes.clone()))
        .collect();
    assert_eq!(
        fixed,
        [
            (
                "cargo check --all-targets".to_owned(),
                false,
                vec!["E0308".to_owned()]
            ),
            ("cargo test -p …".to_owned(), true, vec!["E0425".to_owned()]),
        ]
    );

    let pairs = recall(&f, "cochange");
    assert_eq!(
        pairs.cochange.len(),
        0,
        "one shared episode is below the support floor"
    );
    let pairs = recall(&f, "cochange:src/lib.rs");
    assert_eq!(
        (pairs.cochange[0].a.as_str(), pairs.cochange[0].b.as_str()),
        ("src/parser.rs", "src/lib.rs")
    );

    let last = recall(&f, "last");
    assert_eq!(last.episodes.len(), 3);
    for report in [&by_path, &symbol, &failures, &last] {
        let json = serde_json::to_string(report).unwrap();
        assert!(json.len() <= RECALL_BUDGET, "{} bytes", json.len());
    }
    let unknown = recall(&f, "src/nowhere/");
    assert!(unknown.episodes.is_empty() && unknown.note.is_some());
}

#[test]
fn a_burst_of_submits_is_one_episode() {
    let f = fixture();
    let prompt = |id: &str, at: i64| {
        SourceEvent::Prompt(RawPrompt {
            native_id: id.into(),
            session: "S".into(),
            ts_ms: Some(f.base + at),
        })
    };
    let events = vec![
        SourceEvent::Session(RawSession {
            native_id: "S".into(),
            cwd: file(&f.repo),
            ..Default::default()
        }),
        prompt("1", 0),
        prompt("2", 500),
        prompt("3", 1_400),
        prompt("4", 3_000),
        prompt("5", 60_000),
    ];
    let source = replay(events);
    ingest(&f.db, &source);
    ingest(&f.db, &source);
    let conn = Connection::open(&f.db).unwrap();
    let episodes: i64 = conn
        .query_row("SELECT COUNT(*) FROM episodes", [], |r| r.get(0))
        .unwrap();
    assert_eq!(
        episodes, 2,
        "submits 2 s apart or closer continue one prompt"
    );
}

#[test]
fn recall_fits_its_budget() {
    let row = |i: usize| EpisodeRow {
        ago: format!("{i}d"),
        source: "claude-code".into(),
        calls: 100,
        outcome: "edited".into(),
        files: (0..6)
            .map(|j| FileTouch {
                path: format!("src/some/deeply/nested/module_{i}/file_number_{j}.rs"),
                op: "edit",
                n: 3,
                unchanged: Some(true),
            })
            .collect(),
        more_files: 0,
    };
    let mut report = RecallReport {
        schema_version: 1,
        kind: "recall",
        about: "src/".into(),
        episodes: (0..20).map(row).collect(),
        ..Default::default()
    };
    report.fit(RECALL_BUDGET);
    assert!(report.truncated);
    assert!(serde_json::to_string(&report).unwrap().len() <= RECALL_BUDGET);
    assert!(!report.episodes.is_empty());
}

#[test]
fn digest_is_built_bounded_and_read_back() {
    let f = fixture();
    ingest(&f.db, &replay(events(&f)));
    let read = read_digest(&f.db, &f.repo);
    let DigestStatus::Ready { body, .. } = read.status else {
        panic!("{read:?}")
    };
    assert!(body.len() <= DIGEST_BUDGET);
    assert!(body.contains("src/parser.rs"), "{body}");
    assert!(
        body.contains("Failing, not fixed since: `cargo check --all-targets` E0308"),
        "{body}"
    );
    assert!(read.last_run_ms.is_some());

    // However much there is, the digest stays within its budget.
    let episodes: Vec<EpisodeRow> = (0..3)
        .map(|i| EpisodeRow {
            ago: "1h".into(),
            source: "opencode".into(),
            calls: 999,
            outcome: "tests-fail".into(),
            files: (0..6)
                .map(|j| FileTouch {
                    path: format!("a/very/long/path/that/goes/on/{i}/and/on/file_{j}.rs"),
                    op: "edit",
                    n: 1,
                    unchanged: None,
                })
                .collect(),
            more_files: 40,
        })
        .collect();
    let hot: Vec<(String, i64)> = (0..6)
        .map(|i| (format!("hot/file/number/{i}/x.rs"), 9))
        .collect();
    let body = super::digest::render(&episodes, &hot, &[]);
    assert!(body.len() <= DIGEST_BUDGET, "{} bytes", body.len());
}

#[test]
fn the_hook_reads_the_digest_once_per_session_without_writing() {
    let f = fixture();
    ingest(&f.db, &replay(events(&f)));
    let before = std::fs::read(&f.db).unwrap();
    let mut refreshed = 0;
    let started = std::time::Instant::now();
    let first = session_start_digest_at(&f.db, &f.repo.join("src"), Some("ses-1"), &mut |_| {
        refreshed += 1
    });
    let elapsed = started.elapsed();
    let block = first.expect("first prompt gets the digest");
    assert!(block.starts_with("<codegraph_history"));
    assert!(block.len() <= DIGEST_BUDGET + 300);
    assert!(
        elapsed < Duration::from_millis(500),
        "hook path took {elapsed:?}"
    );
    assert_eq!(
        session_start_digest_at(&f.db, &f.repo, Some("ses-1"), &mut |_| refreshed += 1),
        None,
        "later prompts of the session get nothing"
    );
    assert!(
        session_start_digest_at(&f.db, &f.worktree, Some("ses-2"), &mut |_| refreshed += 1)
            .is_some()
    );
    assert_eq!(
        session_start_digest_at(&f.db, &f.repo, None, &mut |_| refreshed += 1),
        None
    );
    assert_eq!(refreshed, 0, "a fresh store is not refreshed");
    assert_eq!(
        std::fs::read(&f.db).unwrap(),
        before,
        "the hook never writes the store"
    );
}

#[test]
fn the_hook_never_ingests_inline() {
    let f = fixture();
    let mut refreshed = Vec::new();
    // No store: nothing to show, a background refresh asked for, nothing created.
    let got = session_start_digest_at(&f.db, &f.repo, Some("s"), &mut |db| {
        refreshed.push(db.to_path_buf())
    });
    assert_eq!(got, None);
    assert_eq!(refreshed, std::slice::from_ref(&f.db));
    assert!(!f.db.exists());
    // An outdated store: same.
    std::fs::create_dir_all(f.db.parent().unwrap()).unwrap();
    let conn = Connection::open(&f.db).unwrap();
    conn.execute_batch("CREATE TABLE tool_events (id INTEGER); PRAGMA user_version = 2;")
        .unwrap();
    drop(conn);
    let before = std::fs::read(&f.db).unwrap();
    let got = session_start_digest_at(&f.db, &f.repo, Some("s"), &mut |db| {
        refreshed.push(db.to_path_buf())
    });
    assert_eq!(got, None);
    assert_eq!(refreshed.len(), 2);
    assert_eq!(std::fs::read(&f.db).unwrap(), before);
}
