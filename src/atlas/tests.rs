//! Store-level tests: registration, link resolution, worktree/clone
//! grouping, pruning, and read-only opens. None of them read
//! `CODEGRAPH_HOME` (another test in this binary sets it): every atlas path
//! is explicit.

use std::fs;
use std::path::{Path, PathBuf};

use super::git::tests::{fake_repo, fake_worktree};
use super::*;
use crate::directory::get_codegraph_dir;

/// A minimal current-schema index at `root` with one Rust file.
fn fake_index(root: &Path) {
    let dir = get_codegraph_dir(root);
    fs::create_dir_all(&dir).unwrap();
    let conn = rusqlite::Connection::open(dir.join("codegraph.db")).unwrap();
    conn.query_row("PRAGMA journal_mode = WAL", [], |_| Ok(()))
        .unwrap();
    conn.execute_batch(&format!(
        "CREATE TABLE schema_versions (version INTEGER PRIMARY KEY, applied_at INTEGER, description TEXT);
         INSERT INTO schema_versions VALUES ({}, 0, 'x');
         CREATE TABLE files (path TEXT PRIMARY KEY, language TEXT, indexed_at INTEGER, node_count INTEGER);
         INSERT INTO files VALUES ('src/lib.rs', 'rust', 1700000000000, 3), ('Cargo.toml', 'toml', 1700000000500, 0);
         CREATE TABLE nodes (id TEXT); INSERT INTO nodes VALUES ('a'), ('b'), ('c');
         CREATE TABLE edges (id INTEGER); INSERT INTO edges VALUES (1), (2);
         CREATE TABLE project_metadata (key TEXT PRIMARY KEY, value TEXT, updated_at INTEGER);
         INSERT INTO project_metadata VALUES ('indexed_with_extraction_version', '{}', 0),
                                             ('indexed_with_version', '9.9.9', 0);",
        crate::db::CURRENT_SCHEMA_VERSION,
        crate::extraction::EXTRACTION_VERSION,
    ))
    .unwrap();
}

fn crate_manifest(root: &Path, name: &str, deps: &[(&str, &str)]) {
    let mut text = format!("[package]\nname = \"{name}\"\n\n[dependencies]\n");
    for (dep, path) in deps {
        text.push_str(&format!("{dep} = {{ path = \"{path}\" }}\n"));
    }
    fs::create_dir_all(root).unwrap();
    fs::write(root.join("Cargo.toml"), text).unwrap();
}

fn real(path: &Path) -> PathBuf {
    canonical_root(path)
}

fn register(atlas: &mut Atlas, root: &Path) -> Registered {
    let facts = ProjectFacts::gather(root, GatherOptions::default());
    atlas.register(&facts).unwrap()
}

fn listing(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn registration_records_stats_and_resolves_path_deps_across_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("cg-home");
    let app = tmp.path().join("app");
    let lib = tmp.path().join("lib");
    crate_manifest(
        &app,
        "app",
        &[("lib", "../lib/crates/core"), ("gone", "../gone")],
    );
    crate_manifest(&lib.join("crates/core"), "core", &[]);
    fake_index(&app);

    let atlas_db = home.join("atlas.db");
    let outcome = register_project_at(&atlas_db, &app, GatherOptions::default());
    assert!(
        matches!(
            outcome,
            RegisterOutcome::Registered {
                links: 2,
                new: true,
                ..
            }
        ),
        "{outcome:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&atlas_db), 0o600);
        assert_eq!(mode(&home), 0o700);
    }

    let atlas = Atlas::open_read_only(&atlas_db).unwrap().unwrap();
    let project = atlas.project_by_root(&app).unwrap().unwrap();
    assert_eq!(project.root, real(&app));
    assert_eq!(project.name, "app");
    assert_eq!(project.status, ProjectStatus::Ok);
    assert_eq!(
        (project.file_count, project.node_count, project.edge_count),
        (Some(2), Some(3), Some(2))
    );
    assert!(project.counts_exact);
    assert_eq!(project.languages[0].language, "rust");
    assert_eq!(project.last_indexed_ms, Some(1_700_000_000_500));
    assert_eq!(project.engine_version.as_deref(), Some("9.9.9"));
    assert!(project.index_bytes.is_some_and(|b| b > 0));
    // `lib` isn't registered yet: the link is recorded, unresolved.
    let out = atlas.links_from(project.id).unwrap();
    assert_eq!(out.len(), 2);
    assert!(out.iter().all(|l| l.to_project.is_none()));
    let evidence = out[1].evidence.as_ref().unwrap();
    assert_eq!(evidence.path, real(&app).join("Cargo.toml"));
    assert_eq!(evidence.line, Some(5));
    drop(atlas);

    // Registering `lib` resolves the link into it (a subdirectory of lib).
    fake_index(&lib);
    register_project_at(&atlas_db, &lib, GatherOptions::default());
    let atlas = Atlas::open_read_only(&atlas_db).unwrap().unwrap();
    let lib_project = atlas.project_by_root(&lib).unwrap().unwrap();
    let into_lib = atlas.links_to(lib_project.id).unwrap();
    assert_eq!(into_lib.len(), 1);
    assert_eq!(into_lib[0].kind, LinkKind::CargoPathDep);
    assert_eq!(into_lib[0].from_project, project.id);
    assert_eq!(into_lib[0].detail.as_deref(), Some("lib"));
    assert_eq!(into_lib[0].to_path, real(&lib.join("crates/core")));
    // The read API finds the nearest registered root for any path below it.
    let found = atlas
        .project_for_path(&lib.join("crates/core/src/lib.rs"))
        .unwrap()
        .unwrap();
    assert_eq!(found.id, lib_project.id);
    assert!(atlas.project_for_path(tmp.path()).unwrap().is_none());
}

#[test]
fn re_registration_replaces_manifest_links_and_keeps_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let app = tmp.path().join("app");
    crate_manifest(&app, "app", &[("a", "../a")]);
    fake_index(&app);
    let mut atlas = Atlas::open_in_memory().unwrap();
    let first = register(&mut atlas, &app);
    crate_manifest(&app, "app", &[("b", "../b"), ("c", "../c")]);
    let second = register(&mut atlas, &app);
    assert_eq!(first.project_id, second.project_id);
    assert!(first.new && !second.new);
    let details: Vec<_> = atlas
        .links_from(first.project_id)
        .unwrap()
        .into_iter()
        .filter_map(|l| l.detail)
        .collect();
    assert_eq!(details, ["b", "c"]);
}

#[test]
fn worktrees_group_under_their_main_checkout_and_clones_share_a_remote() {
    let tmp = tempfile::tempdir().unwrap();
    let main = tmp.path().join("widget");
    let token = super::remote::tests::fake_token(&["gh", "p_"].concat());
    fake_repo(
        &main,
        &format!("https://ci:{token}@github.com/acme/widget.git"),
    );
    let worktree = main.join(".claude/worktrees/agent-1");
    fake_worktree(&main, &worktree, "agent-1", "feat/x");
    let clone = tmp.path().join("elsewhere/widget-clone");
    fake_repo(&clone, "git@github.com:acme/widget.git");
    for root in [&main, &worktree, &clone] {
        fake_index(root);
    }
    let mut atlas = Atlas::open_in_memory().unwrap();
    for root in [&main, &worktree, &clone] {
        register(&mut atlas, root);
    }
    let main_p = atlas.project_by_root(&main).unwrap().unwrap();
    let wt_p = atlas.project_by_root(&worktree).unwrap().unwrap();
    let clone_p = atlas.project_by_root(&clone).unwrap().unwrap();

    assert!(wt_p.is_worktree && !main_p.is_worktree);
    assert_eq!(wt_p.repo_root, main_p.repo_root);
    assert_eq!(wt_p.name, "widget@agent-1");
    assert_eq!(wt_p.branch.as_deref(), Some("feat/x"));
    let ids = |ps: Vec<Project>| ps.into_iter().map(|p| p.id).collect::<Vec<_>>();
    assert_eq!(ids(atlas.worktrees_of(&main_p).unwrap()), [wt_p.id]);
    assert_eq!(ids(atlas.worktrees_of(&wt_p).unwrap()), [main_p.id]);
    assert_eq!(ids(atlas.clones_of(&main_p).unwrap()), [clone_p.id]);
    assert_eq!(ids(atlas.projects_named("agent-1").unwrap()), [wt_p.id]);

    // Every checkout carries the credential-free remote.
    for p in [&main_p, &wt_p, &clone_p] {
        assert_eq!(p.remote.as_deref(), Some("github.com/acme/widget"));
    }
    assert_eq!(
        ids(atlas
            .projects_with_remote("https://github.com/ACME/widget.git")
            .unwrap()),
        {
            let mut all = vec![main_p.id, wt_p.id, clone_p.id];
            all.sort_by_key(|id| atlas.project(*id).unwrap().unwrap().root);
            all
        }
    );
    let stored: String = atlas
        .conn
        .query_row("SELECT group_concat(remote) FROM projects", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        !stored.contains(&token) && !stored.contains("ci:"),
        "{stored}"
    );

    // Derived links: the repositories are same_remote both ways (through
    // the main checkout, not each worktree); the nested worktree is NOT a
    // nested_workspace of its own main checkout.
    let kinds = |id| {
        atlas
            .links_from(id)
            .unwrap()
            .into_iter()
            .map(|l| (l.kind, l.to_project))
            .collect::<Vec<_>>()
    };
    assert_eq!(kinds(main_p.id), [(LinkKind::SameRemote, Some(clone_p.id))]);
    assert_eq!(kinds(clone_p.id), [(LinkKind::SameRemote, Some(main_p.id))]);
    assert!(kinds(wt_p.id).is_empty());
}

#[test]
fn prune_marks_then_removes_vanished_projects() {
    let tmp = tempfile::tempdir().unwrap();
    let keep = tmp.path().join("keep");
    let gone = tmp.path().join("gone");
    crate_manifest(&keep, "keep", &[("gone", "../gone")]);
    fake_index(&keep);
    fake_index(&gone);
    let mut atlas = Atlas::open_in_memory().unwrap();
    register(&mut atlas, &keep);
    register(&mut atlas, &gone);
    let keep_id = atlas.project_by_root(&keep).unwrap().unwrap().id;
    let gone_root = real(&gone);
    assert!(atlas.links_from(keep_id).unwrap()[0].to_project.is_some());

    fs::remove_dir_all(&gone).unwrap();
    let report = atlas.prune(false).unwrap();
    assert_eq!(report.checked, 2);
    assert_eq!(report.marked_missing, [gone_root.display().to_string()]);
    let marked = atlas.projects_named("gone").unwrap();
    assert_eq!(marked[0].status, ProjectStatus::Missing);
    // Marking twice reports nothing new.
    assert!(atlas.prune(false).unwrap().marked_missing.is_empty());

    let report = atlas.prune(true).unwrap();
    assert_eq!(report.removed, [gone_root.display().to_string()]);
    assert!(atlas.projects_named("gone").unwrap().is_empty());
    // The link survives with its target unresolved.
    let link = &atlas.links_from(keep_id).unwrap()[0];
    assert_eq!(link.to_project, None);
    assert_eq!(link.to_path, gone_root);
}

#[test]
fn read_only_open_creates_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("no-home-yet");
    assert!(
        Atlas::open_read_only(&home.join("atlas.db"))
            .unwrap()
            .is_none()
    );
    assert!(
        !home.exists(),
        "a read-only open must not create the directory"
    );

    let home = tmp.path().join("home");
    let db = home.join("atlas.db");
    let app = tmp.path().join("app");
    fake_index(&app);
    assert!(matches!(
        register_project_at(&db, &app, GatherOptions::default()),
        RegisterOutcome::Registered { .. }
    ));
    assert_eq!(
        listing(&home),
        ["atlas.db"],
        "the writer checkpointed on close"
    );
    let index_dir = get_codegraph_dir(&app);
    let before = listing(&index_dir);
    let atlas = Atlas::open_read_only(&db).unwrap().unwrap();
    assert_eq!(atlas.projects().unwrap().len(), 1);
    drop(atlas);
    assert_eq!(listing(&home), ["atlas.db"]);
    // Gathering facts never writes into the project's index directory.
    let _ = ProjectFacts::gather(&app, GatherOptions::default());
    assert_eq!(listing(&index_dir), before);
}

#[test]
fn unindexed_roots_are_not_registered() {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("home/atlas.db");
    assert_eq!(
        register_project_at(&db, tmp.path(), GatherOptions::default()),
        RegisterOutcome::NotIndexed
    );
    assert!(!db.exists());
}
