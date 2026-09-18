//! The join against synthetic stores: an atlas and a history DB written
//! row by row (every path explicit — never `CODEGRAPH_HOME`), over real
//! directories with `.git` so repository detection works.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, params};

use super::*;
use crate::atlas::{Atlas, LinkKind, Project};
use crate::history::memory::{
    About,
    DIGEST_BUDGET,
    RECALL_BUDGET,
    RecallRequest,
    recall_with_atlas,
};
use crate::history::session_start::session_start_digest_at;
use crate::history::store::HistoryDb;
use crate::history::time::now_ms;

const HOUR: i64 = 3_600_000;

/// Temp dir with an empty atlas and history store.
struct World {
    _tmp: tempfile::TempDir,
    base: PathBuf,
    state: PathBuf,
    atlas: PathBuf,
    history: PathBuf,
    now: i64,
}

impl World {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let state = base.join("state");
        let atlas = state.join("atlas.db");
        let history = state.join("history.db");
        drop(Atlas::open(&atlas).unwrap());
        drop(HistoryDb::open(&history).unwrap());
        Self {
            _tmp: tmp,
            base,
            state,
            atlas,
            history,
            now: now_ms(),
        }
    }

    /// A checkout (`.git` directory) at `base/rel`.
    fn checkout(&self, rel: &str) -> PathBuf {
        let root = self.base.join(rel);
        fs::create_dir_all(root.join(".git")).unwrap();
        root
    }

    fn atlas_rw(&self) -> Connection {
        fixture_writer(&self.atlas)
    }

    fn history_rw(&self) -> Connection {
        fixture_writer(&self.history)
    }

    /// Register a project row: `(root, checkout, repo root, is worktree, remote)`.
    fn project(&self, name: &str, root: &Path, checkout: &Path, repo: &Path, remote: &str) -> i64 {
        let conn = self.atlas_rw();
        conn.execute(
            "INSERT INTO projects (root, name, checkout_root, repo_root, is_worktree, remote,
                                   last_seen, registered_at, status)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, 'ok')",
            params![
                root.to_string_lossy(),
                name,
                checkout.to_string_lossy(),
                repo.to_string_lossy(),
                checkout != repo,
                remote,
            ],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    fn link(&self, from: i64, to: i64, to_path: &Path, kind: LinkKind) {
        self.atlas_rw()
            .execute(
                "INSERT INTO project_links (from_project, to_project, to_path, kind, detail)
                 VALUES (?1, ?2, ?3, ?4, 'dep')",
                params![from, to, to_path.to_string_lossy(), kind.as_str()],
            )
            .unwrap();
    }

    fn repo(&self, root: &Path) -> i64 {
        let conn = self.history_rw();
        conn.execute(
            "INSERT INTO repos (root) VALUES (?1)",
            [root.to_string_lossy()],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// A root session with one episode in `repo`, `ago_h` hours ago.
    fn episode(&self, repo: i64, key: &str, ago_h: i64, outcome: Option<&str>) -> i64 {
        let conn = self.history_rw();
        let at = self.now - ago_h * HOUR;
        conn.execute(
            "INSERT INTO sessions (source, native_key, repo_id, started_at, calls)
             VALUES ('claude-code', ?1, ?2, ?3, 4)",
            params![key, repo, at],
        )
        .unwrap();
        let session = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO episodes (session_id, prompt_key, repo_id, started_at, ended_at, calls, outcome)
             VALUES (?1, ?2, ?3, ?4, ?5, 4, ?6)",
            params![session, format!("{key}-p"), repo, at, at + 60_000, outcome],
        )
        .unwrap();
        conn.last_insert_rowid()
    }

    /// `episode` touched `path` of `repo` (`op` e/r/s) `ago_h` hours ago.
    fn touch(&self, episode: i64, repo: i64, path: &str, op: &str, ago_h: i64) {
        let conn = self.history_rw();
        conn.execute(
            "INSERT OR IGNORE INTO files (repo_id, path) VALUES (?1, ?2)",
            params![repo, path],
        )
        .unwrap();
        let file: i64 = conn
            .query_row(
                "SELECT id FROM files WHERE repo_id = ?1 AND path = ?2",
                params![repo, path],
                |r| r.get(0),
            )
            .unwrap();
        let at = self.now - ago_h * HOUR;
        conn.execute(
            "INSERT INTO touches (episode_id, file_id, op, n, first_ts, last_ts)
             VALUES (?1, ?2, ?3, 1, ?4, ?4)",
            params![episode, file, op, at],
        )
        .unwrap();
    }

    /// A build/test run in `repo`.
    fn outcome(&self, episode: i64, repo: i64, key: &str, template: &str, ok: bool, ago_h: i64) {
        self.history_rw()
            .execute(
                "INSERT INTO outcomes (call_key, episode_id, repo_id, kind, template, ok, err_codes, ts)
                 VALUES (?1, ?2, ?3, 'build', ?4, ?5, 'E0599', ?6)",
                params![key, episode, repo, template, ok, self.now - ago_h * HOUR],
            )
            .unwrap();
    }

    fn read_atlas(&self) -> Atlas {
        Atlas::open_read_only(&self.atlas).unwrap().unwrap()
    }

    fn reader(&self) -> ActivityReader {
        ActivityReader::open(&self.history, Duration::from_secs(5))
            .unwrap()
            .unwrap()
    }

    fn state_listing(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.state)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

/// A writer for fixture rows (no fsync per insert).
fn fixture_writer(db: &Path) -> Connection {
    let conn = Connection::open(db).unwrap();
    conn.pragma_update(None, "synchronous", "OFF").unwrap();
    conn
}

fn project(atlas: &Atlas, id: i64) -> Project {
    atlas.project(id).unwrap().unwrap()
}

/// `app` (main checkout + a linked worktree + a nested tool project) and
/// the history of both checkouts: most folded into the main checkout, one
/// session recorded under the worktree's own root (history could not fold
/// it).
struct Repo {
    main: i64,
    worktree: i64,
    nested: i64,
    main_repo: i64,
    wt_repo: i64,
}

fn app_with_worktree(w: &World) -> Repo {
    let app = w.checkout("app");
    let wt = w.checkout("app-wt");
    let nested = app.join("tools/gen");
    fs::create_dir_all(&nested).unwrap();
    let main = w.project("app", &app, &app, &app, "github.com/o/app");
    let worktree = w.project("app@wt", &wt, &wt, &app, "github.com/o/app");
    let nested_id = w.project("gen", &nested, &app, &app, "github.com/o/app");
    let main_repo = w.repo(&app);
    let wt_repo = w.repo(&wt);
    let e1 = w.episode(main_repo, "s1", 2, Some("commit"));
    w.touch(e1, main_repo, "src/lib.rs", "e", 2);
    w.touch(e1, main_repo, "tools/gen/main.rs", "r", 2);
    let e2 = w.episode(main_repo, "s2", 24 * 40, None);
    w.touch(e2, main_repo, "src/old.rs", "r", 24 * 40);
    let e3 = w.episode(wt_repo, "s3", 1, Some("tests-fail"));
    w.touch(e3, wt_repo, "src/lib.rs", "e", 1);
    Repo {
        main,
        worktree,
        nested: nested_id,
        main_repo,
        wt_repo,
    }
}

#[test]
fn every_checkout_of_a_repository_reads_all_of_its_history() {
    let w = World::new();
    let r = app_with_worktree(&w);
    let atlas = w.read_atlas();
    let reader = w.reader();

    let main = reader
        .scope(&atlas, &project(&atlas, r.main))
        .unwrap()
        .unwrap();
    let wt = reader
        .scope(&atlas, &project(&atlas, r.worktree))
        .unwrap()
        .unwrap();
    let ids = |s: &HistoryScope| {
        let mut ids: Vec<i64> = s.ids().collect();
        ids.sort_unstable();
        ids
    };
    // The main checkout's (folded) history first, then the unfolded worktree's.
    assert_eq!(main.ids().collect::<Vec<_>>(), [r.main_repo, r.wt_repo]);
    assert_eq!(ids(&wt), ids(&main), "a worktree reads the same history");
    assert_eq!(main.prefix(), "");

    let summary = reader.summary(&main).unwrap();
    assert_eq!(summary.sessions_30d, 2, "s1 and s3; s2 is 40 days old");
    let latest = summary.last_activity_ms.unwrap();
    assert!(
        w.now - latest < 2 * HOUR,
        "the worktree session is the latest"
    );

    let activity = reader.activity(&main).unwrap();
    let outcomes: Vec<&str> = activity
        .episodes
        .iter()
        .map(|e| e.outcome.as_str())
        .collect();
    assert_eq!(
        outcomes,
        ["tests-fail", "commit", "explored"],
        "newest first, merged"
    );
    assert_eq!(activity.hot_files[0].path, "src/lib.rs");
    assert_eq!(
        activity.hot_files[0].episodes, 2,
        "counted across both repos"
    );
}

#[test]
fn a_nested_project_narrows_to_its_prefix() {
    let w = World::new();
    let r = app_with_worktree(&w);
    let atlas = w.read_atlas();
    let reader = w.reader();
    let nested = reader
        .scope(&atlas, &project(&atlas, r.nested))
        .unwrap()
        .unwrap();
    assert_eq!(nested.prefix(), "tools/gen/");
    let activity = reader.activity(&nested).unwrap();
    assert_eq!(
        activity.summary.sessions_30d, 1,
        "only s1 touched tools/gen/"
    );
    assert_eq!(activity.episodes.len(), 1);
    assert_eq!(activity.episodes[0].files[0].path, "tools/gen/main.rs");
    assert_eq!(activity.hot_files.len(), 1);
}

#[test]
fn a_project_outside_git_or_without_sessions_has_no_scope() {
    let w = World::new();
    let loose = w.base.join("loose");
    fs::create_dir_all(&loose).unwrap();
    let quiet = w.checkout("quiet");
    let conn = w.atlas_rw();
    conn.execute(
        "INSERT INTO projects (root, name, last_seen, registered_at, status)
         VALUES (?1, 'loose', 0, 0, 'ok')",
        [loose.to_string_lossy()],
    )
    .unwrap();
    let loose_id = conn.last_insert_rowid();
    drop(conn);
    let quiet_id = w.project("quiet", &quiet, &quiet, &quiet, "x/quiet");
    let atlas = w.read_atlas();
    let reader = w.reader();
    assert!(
        reader
            .scope(&atlas, &project(&atlas, loose_id))
            .unwrap()
            .is_none()
    );
    assert!(
        reader
            .scope(&atlas, &project(&atlas, quiet_id))
            .unwrap()
            .is_none()
    );
}

/// `app` uses `lib` (path dep); `tool` uses `app`; `fork` is a separate
/// clone of app's remote; a research dir nested in app is not a code link.
struct Linked {
    app_root: PathBuf,
    app: i64,
    app_repo: i64,
}

fn linked_world(w: &World) -> Linked {
    let app = w.checkout("app");
    let lib = w.checkout("lib");
    let tool = w.checkout("tool");
    let fork = w.checkout("fork");
    let research = w.checkout("app/research/x");
    let app_id = w.project("app", &app, &app, &app, "github.com/o/app");
    let lib_id = w.project("lib", &lib, &lib, &lib, "github.com/o/lib");
    let tool_id = w.project("tool", &tool, &tool, &tool, "github.com/o/tool");
    let fork_id = w.project("fork", &fork, &fork, &fork, "github.com/o/app");
    let research_id = w.project("x", &research, &research, &research, "github.com/r/x");
    w.link(app_id, lib_id, &lib, LinkKind::CargoPathDep);
    w.link(
        app_id,
        lib_id,
        &lib.join("crates/b"),
        LinkKind::CargoPathDep,
    );
    w.link(tool_id, app_id, &app, LinkKind::CargoPathDep);
    w.link(app_id, fork_id, &fork, LinkKind::SameRemote);
    w.link(app_id, research_id, &research, LinkKind::NestedWorkspace);

    let app_repo = w.repo(&app);
    let lib_repo = w.repo(&lib);
    let tool_repo = w.repo(&tool);
    let fork_repo = w.repo(&fork);
    let e = w.episode(app_repo, "a1", 3, Some("edited"));
    w.touch(e, app_repo, "src/main.rs", "e", 3);
    // lib: edited 5h ago, and a failing build no run fixed.
    let e = w.episode(lib_repo, "l1", 5, Some("build-fail"));
    w.touch(e, lib_repo, "src/lib.rs", "e", 5);
    w.outcome(e, lib_repo, "l1-o", "cargo build -p lib", false, 5);
    // tool: its session edited app's shared code 2h ago.
    let e = w.episode(tool_repo, "t1", 2, Some("edited"));
    w.touch(e, app_repo, "src/api.rs", "e", 2);
    w.touch(e, tool_repo, "src/main.rs", "r", 2);
    // fork: a failure that a later run fixed.
    let e = w.episode(fork_repo, "f1", 6, Some("tests-pass"));
    w.outcome(e, fork_repo, "f1-a", "cargo test", false, 6);
    w.outcome(e, fork_repo, "f1-b", "cargo test", true, 5);
    Linked {
        app_root: app,
        app: app_id,
        app_repo,
    }
}

#[test]
fn linked_projects_follow_code_links_both_ways_and_same_remote() {
    let w = World::new();
    let l = linked_world(&w);
    let atlas = w.read_atlas();
    let app = project(&atlas, l.app);
    let linked = linked_projects(&atlas, &app, true).unwrap();
    let got: Vec<(&str, Relation)> = linked
        .iter()
        .map(|x| (x.project.name.as_str(), x.relation))
        .collect();
    assert_eq!(
        got,
        [
            ("lib", Relation::Dependency),
            ("tool", Relation::Dependent),
            ("fork", Relation::SameRemote),
        ],
        "each once; nested research dirs are not code links"
    );
    let code_only = linked_projects(&atlas, &app, false).unwrap();
    assert_eq!(code_only.len(), 2);
}

#[test]
fn linked_activity_reports_open_failures_and_shared_edits() {
    let w = World::new();
    let l = linked_world(&w);
    let atlas = w.read_atlas();
    let reader = w.reader();
    let app = project(&atlas, l.app);
    let own = reader.scope(&atlas, &app).unwrap();
    let query = LinkedQuery {
        failures_since_ms: w.now - 24 * HOUR,
        edits_since_ms: w.now - 24 * HOUR,
        max: 8,
        detail: true,
        same_remote: true,
    };
    let rows = reader
        .linked_activity(&atlas, &app, own.as_ref(), query)
        .unwrap();
    let by = |name: &str| rows.iter().find(|r| r.project == name).unwrap();
    let lib = by("lib");
    assert_eq!(lib.open_failures.len(), 1);
    assert_eq!(lib.open_failures[0].command, "cargo build -p lib");
    let edited = lib.shared_edit_ms.unwrap();
    assert!(
        (w.now - edited - 5 * HOUR).abs() < 1_000,
        "lib's own file edit"
    );
    assert_eq!(lib.summary.sessions_30d, 1);
    let tool = by("tool");
    assert!(tool.open_failures.is_empty());
    let edited = tool.shared_edit_ms.unwrap();
    assert!(
        (w.now - edited - 2 * HOUR).abs() < 1_000,
        "tool's session edited app's file"
    );
    let fork = by("fork");
    assert!(fork.open_failures.is_empty(), "fixed later");
    assert!(fork.shared_edit_ms.is_none());
    assert!(!fork.is_notable());
}

#[test]
fn recall_related_labels_each_linked_project() {
    let w = World::new();
    let l = linked_world(&w);
    let req = |about: &str| RecallRequest {
        about: About::parse(about),
        since_ms: None,
        limit: 3,
        related: true,
    };
    let recall = |about: &str| {
        recall_with_atlas(
            &w.history,
            Some(&w.atlas),
            &l.app_root,
            &req(about),
            Duration::from_secs(5),
        )
        .unwrap()
    };
    let failures = recall("failures");
    let names: Vec<&str> = failures
        .related
        .iter()
        .map(|g| g.project.as_str())
        .collect();
    assert_eq!(names, ["lib", "fork"], "{failures:?}");
    assert_eq!(failures.related[0].relation, Relation::Dependency);
    assert_eq!(
        failures.related[0].failures[0].command,
        "cargo build -p lib"
    );

    // A path of app: tool's session that touched it.
    let path = recall("src/api.rs");
    assert_eq!(path.related.len(), 1, "{path:?}");
    assert_eq!(path.related[0].project, "tool");
    assert_eq!(path.related[0].relation, Relation::Dependent);
    assert_eq!(path.related[0].episodes[0].files[0].path, "src/api.rs");
    assert!(
        path.episodes.is_empty(),
        "no session of app itself touched src/api.rs"
    );
    let json = serde_json::to_value(&path).unwrap();
    assert_eq!(json["related"][0]["relation"], "dependent");
    assert!(path.render_text().contains("tool (depends on this):"));

    // Without `related`, the atlas is never read.
    let plain = recall_with_atlas(
        &w.history,
        Some(&w.atlas),
        &l.app_root,
        &RecallRequest {
            related: false,
            ..req("failures")
        },
        Duration::from_secs(5),
    )
    .unwrap();
    assert!(plain.related.is_empty());
    let _ = l.app_repo;
}

#[test]
fn recall_related_stays_within_the_answer_budget() {
    let w = World::new();
    let app = w.checkout("app");
    let app_id = w.project("app", &app, &app, &app, "github.com/o/app");
    let long = "a/very/long/directory/name/that/keeps/going/and/going/for/a/while";
    for i in 0..8 {
        let dep = w.checkout(&format!("dep{i}"));
        let dep_id = w.project(
            &format!("dependency-with-a-long-name-{i}"),
            &dep,
            &dep,
            &dep,
            &format!("github.com/o/dep{i}"),
        );
        w.link(app_id, dep_id, &dep, LinkKind::CargoPathDep);
        let repo = w.repo(&dep);
        for j in 0..4 {
            let e = w.episode(repo, &format!("d{i}-{j}"), 1 + j, None);
            for k in 0..6 {
                w.touch(e, repo, &format!("{long}/file_{k}.rs"), "e", 1 + j);
            }
        }
    }
    let report = recall_with_atlas(
        &w.history,
        Some(&w.atlas),
        &app,
        &RecallRequest {
            about: About::Last,
            since_ms: None,
            limit: 10,
            related: true,
        },
        Duration::from_secs(5),
    )
    .unwrap();
    let size = serde_json::to_string(&report).unwrap().len();
    assert!(size <= RECALL_BUDGET, "{size} bytes");
    assert!(report.truncated);
    assert!(
        !report.related.is_empty(),
        "the first linked projects survive"
    );
    assert!(report.related.iter().all(|g| !g.project.is_empty()));
}

#[test]
fn reads_create_nothing() {
    let w = World::new();
    let l = linked_world(&w);
    let before = w.state_listing();
    assert_eq!(before, ["atlas.db", "history.db"], "writers closed cleanly");
    {
        let atlas = w.read_atlas();
        let reader = w.reader();
        let app = project(&atlas, l.app);
        let scope = reader.scope(&atlas, &app).unwrap().unwrap();
        reader.activity(&scope).unwrap();
        let query = LinkedQuery {
            failures_since_ms: 0,
            edits_since_ms: 0,
            max: 8,
            detail: true,
            same_remote: true,
        };
        reader
            .linked_activity(&atlas, &app, Some(&scope), query)
            .unwrap();
        recall_with_atlas(
            &w.history,
            Some(&w.atlas),
            &l.app_root,
            &RecallRequest {
                about: About::Failures,
                since_ms: None,
                limit: 3,
                related: true,
            },
            Duration::from_secs(5),
        )
        .unwrap();
        assert!(linked_line(&w.history, &w.atlas, &l.app_root, &l.app_root, 1024).is_some());
    }
    assert_eq!(w.state_listing(), before, "no -wal/-shm, no new files");

    // No stores at all: nothing is created either.
    let empty = w.base.join("empty-home");
    assert!(
        ActivityReader::open(&empty.join("history.db"), Duration::from_secs(1))
            .unwrap()
            .is_none()
    );
    assert!(
        linked_line(
            &empty.join("history.db"),
            &empty.join("atlas.db"),
            &l.app_root,
            &l.app_root,
            1024
        )
        .is_none()
    );
    assert!(!empty.exists());
}

/// The block's body (between its tags).
fn block_body(block: &str) -> &str {
    let start = block.find('\n').unwrap() + 1;
    let end = block.rfind("\n</codegraph_history>").unwrap();
    &block[start..end]
}

fn set_digest(w: &World, repo: i64, body: &str) {
    let conn = w.history_rw();
    conn.execute(
        "INSERT OR REPLACE INTO repo_digests (repo_id, built_at, body) VALUES (?1, ?2, ?3)",
        params![repo, w.now, body],
    )
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO ingest_state (source, key, value) VALUES ('_meta', 'last_run', ?1)",
        [w.now.to_string()],
    )
    .unwrap();
}

#[test]
fn the_digest_gains_one_bounded_line_about_linked_projects() {
    let w = World::new();
    let l = linked_world(&w);
    set_digest(
        &w,
        l.app_repo,
        "Earlier agent sessions in this repository:\n- 3h ago, claude-code, 4 calls, edited: edit src/main.rs\nHot files, 30d (episodes): src/main.rs (1)\n",
    );
    let block = session_start_digest_at(
        &w.history,
        Some(&w.atlas),
        &l.app_root,
        Some("s-1"),
        &mut |_| {},
    )
    .unwrap();
    let body = block_body(&block);
    assert!(body.len() <= DIGEST_BUDGET);
    let linked: Vec<&str> = body
        .lines()
        .filter(|line| line.starts_with("Linked projects"))
        .collect();
    assert_eq!(linked.len(), 1, "{body}");
    let line = linked[0];
    assert!(line.contains("lib (dependency): edited "), "{line}");
    assert!(
        line.contains("1 open failure (`cargo build -p lib` E0599)"),
        "{line}"
    );
    assert!(
        line.contains("tool (depends on this): edited this project "),
        "{line}"
    );
    assert!(
        !line.contains("fork"),
        "clones and fixed failures are not notable"
    );
    assert!(body.contains("Hot files"), "room to spare: nothing dropped");

    // A digest near the budget makes room by dropping its hot-files line.
    let filler = format!(
        "- 1h ago, claude-code, 9 calls, explored: {}\n",
        "x".repeat(700)
    );
    let big = format!(
        "Earlier agent sessions in this repository:\n{filler}Hot files, 30d (episodes): {}\n",
        "src/a.rs (1), ".repeat(12)
    );
    assert!(big.len() <= DIGEST_BUDGET && big.len() > DIGEST_BUDGET - 150);
    set_digest(&w, l.app_repo, &big);
    let block = session_start_digest_at(
        &w.history,
        Some(&w.atlas),
        &l.app_root,
        Some("s-2"),
        &mut |_| {},
    )
    .unwrap();
    let body = block_body(&block);
    assert!(body.len() <= DIGEST_BUDGET, "{} bytes", body.len());
    assert!(!body.contains("Hot files"));
    assert_eq!(
        body.lines()
            .filter(|l| l.starts_with("Linked projects"))
            .count(),
        1
    );

    // No atlas: the digest as stored.
    let block =
        session_start_digest_at(&w.history, None, &l.app_root, Some("s-3"), &mut |_| {}).unwrap();
    assert!(!block.contains("Linked projects"));
}

#[test]
fn a_project_without_code_links_gets_no_line() {
    let w = World::new();
    let l = linked_world(&w);
    // lib has no code links out, and its one dependent (app) did nothing
    // to it; no line.
    let lib = w.base.join("lib");
    assert_eq!(
        linked_line(&w.history, &w.atlas, &lib, &lib, 1024),
        None,
        "app's sessions never edited lib"
    );
    // fork is linked only by remote: not a code link.
    let fork = w.base.join("fork");
    assert_eq!(linked_line(&w.history, &w.atlas, &fork, &fork, 1024), None);
    let _ = l;
}

#[test]
fn the_line_renders_only_what_fits() {
    let now = now_ms();
    let entry = |i: usize| LinkedActivity {
        project: format!("project-{i}"),
        root: PathBuf::from(format!("/p/{i}")),
        relation: Relation::Dependency,
        link: LinkKind::CargoPathDep,
        recorded: true,
        summary: ActivitySummary::default(),
        last_episode: None,
        open_failures: Vec::new(),
        shared_edit_ms: Some(now - HOUR),
    };
    let many: Vec<LinkedActivity> = (0..40).map(entry).collect();
    let line = hook_line::render(&many, now, LINKED_LINE_MAX).unwrap();
    assert!(line.len() <= LINKED_LINE_MAX, "{}", line.len());
    assert!(line.contains("project-0 (dependency): edited 1h ago"));
    assert!(!line.contains('\n'));
    assert_eq!(hook_line::render(&many, now, 20), None, "no room: no line");
    let quiet = LinkedActivity {
        shared_edit_ms: None,
        ..entry(0)
    };
    assert_eq!(hook_line::render(&[quiet], now, 500), None);
}
