//! The atlas end to end through the built CLI: `init`/`index`/`sync`
//! register projects and their manifest links; `codegraph projects …`
//! lists, shows, scans, prunes and draws them. Every run points
//! `CODEGRAPH_HOME` at a temp dir, so nothing touches the developer's atlas.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

fn run(cwd: &Path, home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(args)
        .current_dir(cwd)
        .env("CODEGRAPH_HOME", home)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_NO_BACKGROUND_SYNC", "1")
        .env_remove("CODEGRAPH_ATLAS")
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph")
}

fn ok(out: Output) -> Output {
    assert!(
        out.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn json(cwd: &Path, home: &Path, args: &[&str]) -> Value {
    let out = ok(run(cwd, home, args));
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "{args:?} printed no JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// `app` depends on `lib` by path; both are small Rust crates.
fn workspace() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path().canonicalize().unwrap();
    let app = base.join("app");
    let lib = base.join("lib");
    write(
        &app.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nlib = { path = \"../lib\" }\n",
    );
    write(&app.join("src/main.rs"), "fn main() { lib::hello(); }\n");
    write(
        &lib.join("Cargo.toml"),
        "[package]\nname = \"lib\"\nversion = \"0.1.0\"\n",
    );
    write(&lib.join("src/lib.rs"), "pub fn hello() {}\n");
    (tmp, app, lib)
}

fn project<'a>(list: &'a Value, root: &Path) -> &'a Value {
    list["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["root"].as_str() == Some(root.to_str().unwrap()))
        .unwrap_or_else(|| panic!("{} not listed: {list}", root.display()))
}

#[test]
fn init_index_and_sync_register_projects_and_their_links() {
    let (tmp, app, lib) = workspace();
    let home = tmp.path().join("cg-home");

    let init = ok(run(&app, &home, &["init"]));
    assert!(
        !String::from_utf8_lossy(&init.stderr).contains("Atlas"),
        "a registration that worked says nothing"
    );
    ok(run(&lib, &home, &["init"]));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(home.join("atlas.db"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    let list = json(&app, &home, &["projects", "--json"]);
    assert_eq!(list["projects"].as_array().unwrap().len(), 2);
    let app_p = project(&list, &app);
    let lib_p = project(&list, &lib);
    assert_eq!(app_p["name"], "app");
    assert_eq!(app_p["status"], "ok");
    assert!(app_p["nodeCount"].as_u64().unwrap() > 0);
    assert!(app_p["fileCount"].as_u64().unwrap() >= 1);
    assert_eq!(app_p["languages"][0]["language"], "rust");
    let (app_id, lib_id) = (app_p["id"].clone(), lib_p["id"].clone());

    // The path dep resolves to lib, with manifest:line evidence.
    let links = json(&app, &home, &["projects", "links", "app", "--json"]);
    let out = links["linksOut"].as_array().unwrap();
    assert_eq!(out.len(), 1, "{links}");
    assert_eq!(out[0]["kind"], "cargo_path_dep");
    assert_eq!(out[0]["toProject"], lib_id);
    assert_eq!(out[0]["detail"], "lib");
    assert_eq!(out[0]["evidence"]["line"], 6);
    let shown = json(&lib, &home, &["projects", "show", ".", "--json"]);
    assert_eq!(shown["project"]["id"], lib_id);
    assert_eq!(shown["linksIn"][0]["fromProject"], app_id);

    // A sync re-reads the manifests: a new path dep appears.
    write(
        &tmp.path().join("extra/Cargo.toml"),
        "[package]\nname = \"extra\"\n",
    );
    write(
        &app.join("Cargo.toml"),
        "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nlib = { path = \"../lib\" }\nextra = { path = \"../extra\" }\n",
    );
    ok(run(&app, &home, &["sync"]));
    let links = json(
        &app,
        &home,
        &["projects", "links", "app", "--all", "--json"],
    );
    let details: Vec<&str> = links["linksOut"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|l| l["detail"].as_str())
        .collect();
    assert_eq!(details, ["extra", "lib"], "{links}");

    // A quiet index refreshes the same row.
    ok(run(&app, &home, &["index", "--quiet"]));
    let again = json(&app, &home, &["projects", "--json"]);
    assert_eq!(project(&again, &app)["id"], app_id);
    assert!(
        project(&again, &app)["lastSeenMs"].as_i64() >= app_p["lastSeenMs"].as_i64(),
        "{again}"
    );

    let graph = ok(run(&app, &home, &["projects", "graph"]));
    let graph = String::from_utf8_lossy(&graph.stdout);
    assert!(graph.starts_with("flowchart LR\n"), "{graph}");
    assert!(
        graph.contains(&format!("p{app_id} -- cargo path dep --> p{lib_id}")),
        "{graph}"
    );
}

#[test]
fn scan_registers_existing_indexes_and_prune_marks_vanished_ones() {
    let (tmp, app, lib) = workspace();
    let build_home = tmp.path().join("build-home");
    ok(run(&app, &build_home, &["init"]));
    ok(run(&lib, &build_home, &["init"]));

    // A fresh atlas learns both from a read-only scan.
    let home = tmp.path().join("scan-home");
    let report = json(
        tmp.path(),
        &home,
        &[
            "projects",
            "scan",
            "--root",
            tmp.path().to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(report["found"].as_array().unwrap().len(), 2, "{report}");
    assert_eq!(report["new"], 2);
    assert_eq!(report["budgetExhausted"], false);
    let list = json(tmp.path(), &home, &["projects", "--json"]);
    assert_eq!(list["projects"].as_array().unwrap().len(), 2);

    fs::remove_dir_all(&lib).unwrap();
    let pruned = json(tmp.path(), &home, &["projects", "prune", "--json"]);
    assert_eq!(pruned["markedMissing"][0], lib.to_str().unwrap());
    let list = json(tmp.path(), &home, &["projects", "--json"]);
    assert_eq!(project(&list, &lib)["status"], "missing");
    let pruned = json(
        tmp.path(),
        &home,
        &["projects", "prune", "--remove", "--json"],
    );
    assert_eq!(pruned["removed"][0], lib.to_str().unwrap());
    let list = json(tmp.path(), &home, &["projects", "--json"]);
    assert_eq!(list["projects"].as_array().unwrap().len(), 1);
}

#[test]
fn reads_never_create_the_atlas_and_the_atlas_can_be_switched_off() {
    let (tmp, app, _lib) = workspace();
    let home = tmp.path().join("untouched-home");
    let list = json(&app, &home, &["projects", "--json"]);
    assert_eq!(list["projects"].as_array().unwrap().len(), 0);
    ok(run(&app, &home, &["projects", "graph"]));
    assert!(!home.exists(), "reads must not create the machine-wide dir");

    let out = Command::new(env!("CARGO_BIN_EXE_codegraph"))
        .args(["init"])
        .current_dir(&app)
        .env("CODEGRAPH_HOME", &home)
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_ATLAS", "0")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(!home.join("atlas.db").exists());

    let missing = run(&app, &home, &["projects", "show", "nope"]);
    assert!(!missing.status.success());
}
