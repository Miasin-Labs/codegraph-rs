//! The shared dependency-graph store end to end, on synthetic machines:
//! lockfiles → located sources → registry → shards → read-only lookups,
//! plus rebuilds, locking, budgets, gc, the post-index trigger and the CLI.

#[path = "deps_test/fixture.rs"]
mod fixture;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime};

use codegraph::deps::builder::{BuildOptions, ShardResult, build_pending};
use codegraph::deps::gc::{GcPolicy, GcReason, gc};
use codegraph::deps::project::{canonical_root, record_project};
use codegraph::deps::registry::PendingScope;
use codegraph::deps::scope::{PartialReason, ShardLimits};
use codegraph::deps::shard::{BuildOutcome, BuildRequest, build_shard};
use codegraph::deps::store::StoreLock;
use codegraph::deps::trigger::{TriggerOutcome, after_project_indexed_in};
use codegraph::deps::{
    DepKey,
    DepSource,
    Ecosystem,
    Registry,
    ShardHandle,
    ShardMeta,
    ShardState,
    dependencies_of_in,
};
use codegraph::sync::background::BackgroundSync;
use fixture::{Machine, now_ms, snapshot, write};

fn alpha() -> DepKey {
    DepKey::new(Ecosystem::Crates, "alpha", "1.0.0")
}

fn beta() -> DepKey {
    DepKey::new(Ecosystem::Crates, "beta", "2.0.0")
}

fn open_registry(machine: &Machine) -> Registry {
    let home = machine.home();
    home.ensure().unwrap();
    Registry::open(&home.registry_path()).unwrap()
}

async fn build_project(
    machine: &Machine,
    registry: &Registry,
    root: &Path,
    options: &BuildOptions,
) -> Vec<ShardResult> {
    let canonical = canonical_root(root);
    build_pending(
        &machine.home(),
        registry,
        PendingScope::Project(&canonical),
        options,
        &mut |_| {},
    )
    .await
    .unwrap()
    .results
}

fn built_meta(result: &ShardResult) -> &ShardMeta {
    match result {
        ShardResult::Built { meta } => meta,
        other => panic!("expected a built shard, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn records_builds_and_reads_shards_without_touching_sources() {
    let machine = Machine::new();
    let project = machine.rust_project("app");
    let sources_before = snapshot(&machine.cargo_home);
    let mut registry = open_registry(&machine);

    let report =
        record_project(&mut registry, &project, &machine.roots(), false, now_ms()).unwrap();
    let crates = report.by_ecosystem[&Ecosystem::Crates];
    assert_eq!(crates.dependencies, 4, "{report:#?}");
    assert_eq!(crates.direct, 2, "alpha and the git fork");
    assert_eq!(crates.located, 2);
    assert_eq!(crates.unavailable, 1, "the git fork isn't checked out");
    assert_eq!(crates.path, 1);

    let results = build_project(&machine, &registry, &project, &BuildOptions::default()).await;
    assert_eq!(results.len(), 2, "{results:#?}");
    let alpha_meta = built_meta(&results[0]);
    assert_eq!(alpha_meta.key(), alpha(), "direct dependency first");
    assert_eq!(alpha_meta.state, ShardState::Ready);
    assert_eq!(
        alpha_meta.counts.selected_files, 2,
        "tests/benches/examples/build.rs excluded"
    );
    assert!(alpha_meta.counts.nodes > 0);
    assert_eq!(alpha_meta.source, DepSource::Registry);

    // The shared source tree was only read.
    assert_eq!(sources_before, snapshot(&machine.cargo_home));

    // Published atomically: the shard dir, no build leftovers.
    let home = machine.home();
    let crates_dir = home.ecosystem_dir(Ecosystem::Crates);
    let names: Vec<String> = fs::read_dir(&crates_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(names.iter().all(|n| !n.starts_with('.')), "{names:?}");
    let shard_dir = home.shard_dir(&alpha());
    let mut files: Vec<String> = fs::read_dir(&shard_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec!["codegraph.db", "meta.json"],
        "one self-contained database"
    );

    // Opening and querying a shard writes nothing, anywhere.
    let before = snapshot(&machine.codegraph_home);
    {
        let handle = ShardHandle::open(&home, &alpha()).expect("alpha shard");
        assert!(!handle.is_stale() && !handle.is_partial());
        assert_eq!(handle.source_dir(), machine.crate_dir("alpha", "1.0.0"));
        let twice = handle.symbols_named("twice").unwrap();
        assert_eq!(twice.len(), 1);
        assert_eq!(
            twice[0].file_path, "src/parse.rs",
            "paths relative to the dependency root"
        );
        let new = handle.lookup("Alpha::new").unwrap();
        assert_eq!(new.len(), 1, "qualified lookup");
        assert!(handle.symbols_named("in_test_only").unwrap().is_empty());
        assert!(handle.symbols_named("in_bench_only").unwrap().is_empty());
        let nodes = handle.queries().get_all_nodes().unwrap();
        assert!(nodes.iter().all(|n| !n.file_path.starts_with('/')));
    }
    assert!(ShardHandle::open(&home, &DepKey::new(Ecosystem::Crates, "alpha", "9.9.9")).is_none());
    assert_eq!(
        before,
        snapshot(&machine.codegraph_home),
        "read-only open created or changed files"
    );

    // A store that doesn't exist stays that way.
    let nowhere = codegraph::deps::DepsHome::at(machine.root.join("no-home/deps"));
    assert!(ShardHandle::open(&nowhere, &alpha()).is_none());
    assert!(dependencies_of_in(&nowhere, &project).unwrap().is_empty());
    assert!(!machine.root.join("no-home").exists());

    let deps = dependencies_of_in(&home, &project).unwrap();
    assert_eq!(deps.len(), 4);
    let state_of = |name: &str| deps.iter().find(|d| d.key.name == name).unwrap().state;
    assert_eq!(state_of("alpha"), ShardState::Ready);
    assert_eq!(state_of("beta"), ShardState::Ready);
    assert_eq!(state_of("forked"), ShardState::Unavailable);
    assert_eq!(state_of("localdep"), ShardState::Unavailable);
    let forked = deps.iter().find(|d| d.key.name == "forked").unwrap();
    assert_eq!(forked.key.version, "0.3.0+git.abcdef012345");

    // Idempotent: nothing pending, and a direct rebuild request is a no-op.
    assert!(
        build_project(&machine, &registry, &project, &BuildOptions::default())
            .await
            .is_empty()
    );
    let again = build_shard(
        &home,
        &BuildRequest {
            key: &alpha(),
            source: &DepSource::Registry,
            source_dir: &machine.crate_dir("alpha", "1.0.0"),
            limits: ShardLimits::default(),
            force: false,
        },
    )
    .await;
    assert!(matches!(again, BuildOutcome::UpToDate(_)), "{again:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_extractor_bump_rebuilds_lazily_and_atomically() {
    let machine = Machine::new();
    let project = machine.rust_project("app");
    let mut registry = open_registry(&machine);
    record_project(&mut registry, &project, &machine.roots(), false, now_ms()).unwrap();
    build_project(&machine, &registry, &project, &BuildOptions::default()).await;
    let home = machine.home();

    // Pretend alpha was built by an older extractor.
    let dir = home.shard_dir(&alpha());
    let mut meta = ShardMeta::read(&dir).unwrap();
    let first_build = meta.built_at_ms;
    meta.extractor_version = 1;
    meta.write(&dir).unwrap();
    rusqlite::Connection::open(home.registry_path())
        .unwrap()
        .execute(
            "UPDATE packages SET extractor_version = 1 WHERE name = 'alpha'",
            [],
        )
        .unwrap();

    // Still readable meanwhile, flagged stale; an open handle survives the swap.
    let old = ShardHandle::open(&home, &alpha()).unwrap();
    assert!(old.is_stale());
    let pending = registry.pending(PendingScope::All, false).unwrap();
    assert_eq!(
        pending.iter().map(|p| p.key.clone()).collect::<Vec<_>>(),
        vec![alpha()]
    );

    std::thread::sleep(Duration::from_millis(5));
    let results = build_project(&machine, &registry, &project, &BuildOptions::default()).await;
    assert_eq!(results.len(), 1);
    let rebuilt = built_meta(&results[0]);
    assert!(rebuilt.built_at_ms > first_build);
    assert!(!ShardHandle::open(&home, &alpha()).unwrap().is_stale());
    assert_eq!(
        old.symbols_named("twice").unwrap().len(),
        1,
        "old handle still reads"
    );
    let leftovers: Vec<_> = fs::read_dir(home.ecosystem_dir(Ecosystem::Crates))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert!(
        registry
            .pending(PendingScope::All, false)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_locked_shard_is_skipped_by_other_builders() {
    let machine = Machine::new();
    let project = machine.rust_project("app");
    let mut registry = open_registry(&machine);
    record_project(&mut registry, &project, &machine.roots(), false, now_ms()).unwrap();
    let home = machine.home();

    let held = StoreLock::try_acquire(&home.shard_lock_path(&alpha()))
        .unwrap()
        .unwrap();
    let results = build_project(&machine, &registry, &project, &BuildOptions::default()).await;
    assert!(
        matches!(&results[0], ShardResult::Locked { key } if *key == alpha()),
        "{results:#?}"
    );
    assert!(matches!(&results[1], ShardResult::Built { .. }));
    assert!(!home.shard_dir(&alpha()).exists());
    assert_eq!(
        registry.package(&alpha()).unwrap().unwrap().state,
        ShardState::Building,
        "left to the lock holder"
    );
    drop(held);

    // The lock holder "died": the next build picks it up.
    let results = build_project(&machine, &registry, &project, &BuildOptions::default()).await;
    assert_eq!(results.len(), 1);
    assert_eq!(built_meta(&results[0]).key(), alpha());
}

#[tokio::test(flavor = "multi_thread")]
async fn budgets_make_partial_shards_that_grow_with_bigger_budgets() {
    let machine = Machine::new();
    let project = machine.rust_project("app");
    let mut registry = open_registry(&machine);
    record_project(&mut registry, &project, &machine.roots(), false, now_ms()).unwrap();
    let home = machine.home();
    let tight = ShardLimits {
        max_files: 1,
        ..ShardLimits::default()
    };
    let (key, dir) = (alpha(), machine.crate_dir("alpha", "1.0.0"));
    let (key, dir) = (&key, dir.as_path());
    let request = move |limits| BuildRequest {
        key,
        source: &DepSource::Registry,
        source_dir: dir,
        limits,
        force: false,
    };

    let BuildOutcome::Built(meta) = build_shard(&home, &request(tight)).await else {
        panic!("expected a build");
    };
    assert_eq!(meta.state, ShardState::Partial);
    assert_eq!(meta.partial_reasons, vec![PartialReason::Files]);
    assert_eq!(meta.counts.selected_files, 1);
    let handle = ShardHandle::open(&home, &alpha()).unwrap();
    assert!(handle.is_partial());
    assert_eq!(
        handle.symbols_named("Alpha").unwrap().len(),
        1,
        "shallowest file first: src/lib.rs"
    );
    assert!(handle.symbols_named("twice").unwrap().is_empty());

    assert!(matches!(
        build_shard(&home, &request(tight)).await,
        BuildOutcome::UpToDate(_)
    ));
    let BuildOutcome::Built(full) = build_shard(&home, &request(ShardLimits::default())).await
    else {
        panic!("bigger budget should rebuild");
    };
    assert_eq!(full.state, ShardState::Ready);

    // A 1 ms time budget: extraction may stop between batches; whatever was
    // stored is published, marked partial for time if anything was cut.
    let timed = ShardLimits {
        time_ms: 1,
        ..ShardLimits::default()
    };
    let mut req = request(timed);
    req.force = true;
    let BuildOutcome::Built(meta) = build_shard(&home, &req).await else {
        panic!("a time budget never fails a build");
    };
    let complete = meta.counts.indexed_files == meta.counts.selected_files;
    assert_eq!(
        meta.partial_reasons.contains(&PartialReason::Time),
        !complete,
        "{meta:#?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gc_forgets_vanished_projects_and_removes_unused_then_lru_shards() {
    let machine = Machine::new();
    let app = machine.rust_project("app");
    let other = machine.beta_project("other");
    let mut registry = open_registry(&machine);
    for project in [&app, &other] {
        record_project(&mut registry, project, &machine.roots(), false, now_ms()).unwrap();
        build_project(&machine, &registry, project, &BuildOptions::default()).await;
    }
    let home = machine.home();
    // Another pass's per-crate artifact beside the shards must survive.
    let foreign = home
        .ecosystem_dir(Ecosystem::Crates)
        .join("beta-2.0.0.names");
    fs::write(&foreign, "beta\n").unwrap();
    let stale_tmp = home
        .ecosystem_dir(Ecosystem::Crates)
        .join(".tmp-beta-2.0.0-1-1");
    fs::create_dir_all(&stale_tmp).unwrap();
    assert_eq!(registry.users_of(&beta()).unwrap().len(), 2);

    // `app` goes away: beta is still used by `other`, alpha by no one.
    fs::remove_dir_all(&app).unwrap();
    let now = SystemTime::now();
    let dry = gc(
        &home,
        &registry,
        &GcPolicy {
            dry_run: true,
            ..GcPolicy::default()
        },
        now,
    )
    .unwrap();
    assert_eq!(dry.removed.len(), 1);
    assert!(home.shard_dir(&alpha()).exists(), "dry run removes nothing");

    let report = gc(
        &home,
        &registry,
        &GcPolicy::default(),
        now + Duration::from_secs(7200),
    )
    .unwrap();
    assert_eq!(report.projects_forgotten, vec![canonical_root(&app)]);
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].key, alpha());
    assert_eq!(report.removed[0].reason, GcReason::Unused);
    assert_eq!(report.leftovers_removed, 1);
    assert!(!home.shard_dir(&alpha()).exists());
    assert!(!stale_tmp.exists());
    assert!(home.shard_dir(&beta()).exists());
    assert!(foreign.is_file(), "gc only removes shard directories");

    // Over the size cap: least recently used goes.
    let capped = GcPolicy {
        max_total_bytes: Some(1),
        ..GcPolicy::default()
    };
    let report = gc(&home, &registry, &capped, SystemTime::now()).unwrap();
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].reason, GcReason::SizeCap);
    assert!(!home.shard_dir(&beta()).exists());
    assert_eq!(
        registry.package(&beta()).unwrap().unwrap().state,
        ShardState::Missing
    );
    assert!(foreign.is_file());

    // Unused past max_age: stale.
    build_project(&machine, &registry, &other, &BuildOptions::default()).await;
    record_project(&mut registry, &other, &machine.roots(), true, 0).unwrap();
    let report = gc(&home, &registry, &GcPolicy::default(), SystemTime::now()).unwrap();
    assert_eq!(report.removed.len(), 1);
    assert_eq!(report.removed[0].reason, GcReason::Stale);
}

#[tokio::test(flavor = "multi_thread")]
async fn npm_and_go_dependencies_record_and_build() {
    let machine = Machine::new();
    let web = machine.npm_project("web");
    let svc = machine.go_project("svc");
    let mut registry = open_registry(&machine);

    let report = record_project(&mut registry, &web, &machine.roots(), false, now_ms()).unwrap();
    let npm = report.by_ecosystem[&Ecosystem::Npm];
    assert_eq!(
        (npm.dependencies, npm.direct, npm.located, npm.unavailable),
        (2, 2, 1, 1)
    );
    let results = build_project(&machine, &registry, &web, &BuildOptions::default()).await;
    assert_eq!(results.len(), 1, "{results:#?}");
    let meta = built_meta(&results[0]);
    assert_eq!(
        meta.key(),
        DepKey::new(Ecosystem::Npm, "typed-lib", "1.2.0")
    );
    assert_eq!(
        meta.counts.selected_files, 1,
        "declarations only, no tests: {meta:#?}"
    );
    let handle = codegraph::deps::ShardHandle::open(&machine.home(), &meta.key()).unwrap();
    assert!(!handle.symbols_named("Client").unwrap().is_empty());
    assert!(!handle.symbols_named("connect").unwrap().is_empty());
    assert!(!web.join("node_modules/typed-lib/.codegraph").exists());

    let report = record_project(&mut registry, &svc, &machine.roots(), false, now_ms()).unwrap();
    let go = report.by_ecosystem[&Ecosystem::Go];
    assert_eq!(
        (go.dependencies, go.direct, go.located, go.unavailable),
        (2, 1, 1, 1)
    );
    let results = build_project(&machine, &registry, &svc, &BuildOptions::default()).await;
    let meta = built_meta(&results[0]);
    assert_eq!(
        meta.key(),
        DepKey::new(Ecosystem::Go, "github.com/BurntSushi/toml", "v1.2.3")
    );
    assert_eq!(meta.counts.selected_files, 1, "no _test.go");
    let dir = machine.home().shard_dir(&meta.key());
    assert!(
        dir.ends_with("go/github.com+!burnt!sushi+toml-v1.2.3"),
        "{}",
        dir.display()
    );
    let handle = codegraph::deps::ShardHandle::open(&machine.home(), &meta.key()).unwrap();
    assert!(handle.symbols_named("Decode").unwrap().len() >= 2);
    assert!(handle.symbols_named("TestOnly").unwrap().is_empty());
}

#[test]
fn the_index_trigger_records_once_and_asks_for_one_background_build() {
    let machine = Machine::new();
    let home = machine.home();
    let spawned = std::cell::RefCell::new(Vec::new());
    let spawn = |root: &Path| {
        spawned.borrow_mut().push(root.to_path_buf());
        BackgroundSync::Started
    };

    // No lockfiles: nothing written, not even the store.
    let bare = machine.root.join("bare");
    write(&bare.join("main.rs"), "fn main() {}\n");
    let outcome = after_project_indexed_in(&home, &machine.roots(), &bare, &spawn).unwrap();
    assert_eq!(outcome, TriggerOutcome::NoLockfiles);
    assert!(!machine.codegraph_home.exists());

    let project = machine.rust_project("app");
    let outcome = after_project_indexed_in(&home, &machine.roots(), &project, &spawn).unwrap();
    let TriggerOutcome::Recorded {
        unchanged,
        summary,
        pending,
        build,
    } = outcome
    else {
        panic!("{outcome:?}");
    };
    assert!(!unchanged);
    assert_eq!(summary.dependencies, 4);
    assert_eq!(pending, 2);
    assert_eq!(build, Some(BackgroundSync::Started));
    assert_eq!(
        spawned.borrow().as_slice(),
        [fs::canonicalize(&project).unwrap()]
    );

    // Unchanged lockfiles: no re-parse; still pending, so ask again (the
    // spawner itself dedupes on the builder lock).
    let outcome = after_project_indexed_in(&home, &machine.roots(), &project, &spawn).unwrap();
    assert!(
        matches!(
            outcome,
            TriggerOutcome::Recorded {
                unchanged: true,
                pending: 2,
                ..
            }
        ),
        "{outcome:?}"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(home.registry_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }
}

// ---------------------------------------------------------------------------
// CLI

fn run(machine: &Machine, cwd: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", machine.root.join("user-home"))
        .env("CODEGRAPH_HOME", &machine.codegraph_home)
        .env("CARGO_HOME", &machine.cargo_home)
        .env("GOMODCACHE", &machine.go_mod_cache)
        .env("CODEGRAPH_NO_BACKGROUND_SYNC", "1")
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_TELEMETRY", "0")
        .env("DO_NOT_TRACK", "1")
        .env_remove("CODEGRAPH_DEPS")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph")
}

fn json(output: &std::process::Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&output.stderr),
        String::from_utf8_lossy(&output.stdout)
    );
    serde_json::from_slice(&output.stdout).expect("JSON output")
}

#[test]
fn cli_index_records_dependencies_and_deps_commands_build_show_and_gc() {
    let machine = Machine::new();
    let project = machine.rust_project("app");
    let project_arg = project.to_string_lossy().into_owned();

    // `init` indexes, then records the lockfile (the background build is
    // disabled by CODEGRAPH_NO_BACKGROUND_SYNC).
    let out = run(&machine, &project, &["init", &project_arg]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let home = machine.home();
    assert!(
        home.registry_path().is_file(),
        "index recorded the dependencies"
    );
    // Indexing may write the resolver's per-crate method-name artifacts
    // (`<name>-<version>.api` files) beside the shards, but no shard — a
    // directory holding `meta.json` — is built inline.
    let crates_dir = home.ecosystem_dir(Ecosystem::Crates);
    let inline_shards = std::fs::read_dir(&crates_dir)
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().join("meta.json").is_file())
                .count()
        })
        .unwrap_or(0);
    assert_eq!(inline_shards, 0, "no inline shard build");
    assert!(!project.join(".codegraph").join("deps").exists());

    let listed = json(&run(
        &machine,
        &project,
        &["deps", "list", "--json", "--project", &project_arg],
    ));
    let deps = listed["dependencies"].as_array().unwrap();
    assert_eq!(deps.len(), 4);
    let alpha_row = deps.iter().find(|d| d["key"]["name"] == "alpha").unwrap();
    assert_eq!(alpha_row["state"], "missing");
    assert_eq!(alpha_row["direct"], true);

    let built = json(&run(
        &machine,
        &project,
        &["deps", "build", "--json", "--project", &project_arg],
    ));
    let results = built["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r["outcome"] == "built"));

    let status = json(&run(&machine, &project, &["deps", "status", "--json"]));
    assert_eq!(status["status"]["projects"], 1);
    assert!(status["status"]["shardBytes"].as_u64().unwrap() > 0);

    let shown = json(&run(
        &machine,
        &project,
        &["deps", "show", "alpha@1.0.0", "--symbol", "twice", "--json"],
    ));
    let entry = &shown[0];
    assert_eq!(entry["state"], "ready");
    assert_eq!(entry["symbols"][0]["name"], "twice");
    assert_eq!(entry["symbols"][0]["file"], "src/parse.rs");
    assert_eq!(entry["users"][0], canonical_root(&project));

    // Human output renders too.
    let out = run(
        &machine,
        &project,
        &["deps", "list", "--project", &project_arg],
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("alpha 1.0.0"));

    let gc_report = json(&run(
        &machine,
        &project,
        &["deps", "gc", "--dry-run", "--json"],
    ));
    assert_eq!(gc_report["removed"].as_array().unwrap().len(), 0);

    // `sync` with unchanged lockfiles leaves the recorded usage alone.
    let out = run(&machine, &project, &["sync", "--quiet", &project_arg]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let listed = json(&run(
        &machine,
        &project,
        &["deps", "list", "--json", "--project", &project_arg],
    ));
    let alpha_row = listed["dependencies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["key"]["name"] == "alpha")
        .unwrap()
        .clone();
    assert_eq!(alpha_row["state"], "ready");
}
