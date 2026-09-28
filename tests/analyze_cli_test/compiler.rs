/// `cargo clippy` is installed (the compiler detector's test needs it).
fn clippy_available() -> bool {
    Command::new("cargo")
        .args(["clippy", "--version"])
        .stdin(Stdio::null())
        .output()
        .is_ok_and(|out| out.status.success())
}

/// A dependency-free crate with a mini router the index reads as an axum
/// route (`.route("/items", get(item))`): `item` → `first_byte` indexes the
/// request body unchecked; `offline` indexes the same way but only `serve`
/// (no route handler) calls it; `quiet`'s indexing carries a suppression
/// comment; `main` drops a `Result` (rustc's `unused_must_use`).
fn write_compiler_fixture(root: &std::path::Path) {
    support::write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    support::write(
        &root.join("src/main.rs"),
        "mod server;\n\nfn checked() -> Result<(), String> {\n    Err(\"no\".into())\n}\n\n\
         fn main() {\n    server::app().serve();\n    checked();\n}\n",
    );
    support::write(
        &root.join("src/server.rs"),
        r#"pub struct Handler(fn(&[u8]) -> u8);

pub fn get(handler: fn(&[u8]) -> u8) -> Handler {
    Handler(handler)
}

pub struct Router {
    routes: Vec<(&'static str, Handler)>,
}

impl Router {
    pub fn route(mut self, path: &'static str, handler: Handler) -> Self {
        self.routes.push((path, handler));
        self
    }

    pub fn serve(&self) {
        for (_, handler) in &self.routes {
            println!("{}", (handler.0)(b"request"));
        }
        println!("{}", offline(b"table", 2));
        println!("{}", quiet(b"table", 1));
    }
}

pub fn app() -> Router {
    Router { routes: Vec::new() }.route("/items", get(item))
}

fn item(body: &[u8]) -> u8 {
    first_byte(body)
}

fn first_byte(body: &[u8]) -> u8 {
    body[3]
}

fn offline(table: &[u8], i: usize) -> u8 {
    table[i]
}

fn quiet(table: &[u8], i: usize) -> u8 {
    // codegraph: ignore clippy::indexing_slicing — callers pass a checked index
    table[i]
}
"#,
    );
}

/// Run `codegraph analyze <args> --json` with cargo's output in the
/// project's own target dir.
fn run_compiler_json(root: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(bin())
        .arg("analyze")
        .args(args)
        .arg("--json")
        .current_dir(root)
        .env("CODEGRAPH_HOME", concat!(env!("CARGO_TARGET_TMPDIR"), "/codegraph-home"))
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CARGO_TARGET_DIR", root.join("target"))
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph");
    assert!(out.status.success(), "{}", stderr_str(&out));
    let envelope: serde_json::Value = serde_json::from_str(stdout_str(&out).trim())
        .unwrap_or_else(|e| panic!("bad JSON ({e}): {}", stdout_str(&out)));
    envelope["data"].clone()
}

#[test]
fn analyze_bugs_compiler_ranks_an_indexing_panic_a_route_reaches() {
    if !clippy_available() {
        eprintln!("cargo clippy not installed; skipped");
        return;
    }
    let (_dir, root) = temp_project();
    write_compiler_fixture(&root);
    let out = run_cli(&root, &["init"]);
    assert!(out.status.success(), "init failed: {}", stderr_str(&out));

    let report = run_compiler_json(
        &root,
        &["bugs", "--detector", "compiler", "--compiler-wait", "600"],
    );
    let compiler = &report["compiler"];
    assert_eq!(compiler["state"], "complete", "{report}");
    assert_eq!(compiler["cached"], false, "{report}");
    assert_eq!(compiler["compileErrors"], 0, "{report}");
    assert_eq!(compiler["entryKind"], "routes", "{report}");

    let findings = report["findings"].as_array().unwrap();
    assert!(
        findings.iter().all(|f| f["detector"] == "compiler"),
        "only the named family runs: {report}"
    );
    let at = |file: &str, line: u64| {
        findings
            .iter()
            .find(|f| f["file"] == file && f["line"] == line)
            .unwrap_or_else(|| panic!("no finding at {file}:{line} in {report}"))
    };
    let handler = at("src/server.rs", 35);
    assert_eq!(handler["rule"], "clippy::indexing_slicing");
    assert_eq!(handler["function"], "first_byte", "{handler}");
    assert!(
        handler["message"]
            .as_str()
            .unwrap()
            .contains("reachable from route `GET /items`"),
        "{handler}"
    );
    let offline = at("src/server.rs", 39);
    assert!(
        handler["confidence"].as_f64() > offline["confidence"].as_f64(),
        "the handler's panic outranks one no route reaches: {report}"
    );
    assert!(
        !findings.iter().any(|f| f["line"] == 44),
        "`codegraph: ignore` drops the finding: {report}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f["rule"] == "rustc::unused_must_use" && f["file"] == "src/main.rs"),
        "rustc lints are findings too: {report}"
    );

    // Nothing changed: the second call answers from the cache.
    let again = run_compiler_json(&root, &["bugs", "--detector", "compiler"]);
    assert_eq!(again["compiler"]["cached"], true, "{again}");
    assert_eq!(again["findings"], report["findings"]);

    // The review packet asks about the route and the panic.
    let packets = run_compiler_json(
        &root,
        &["review", "--detector", "compiler", "--at", "src/server.rs:35"],
    );
    let packet = &packets.as_array().unwrap()[0];
    let checklist: Vec<&str> = packet["checklist"]
        .as_array()
        .unwrap()
        .iter()
        .map(|q| q.as_str().unwrap())
        .collect();
    assert!(checklist[0].starts_with("Reachability: route `GET /items`"), "{packet}");
    assert!(checklist.iter().any(|q| q.contains("Trace it back")), "{packet}");
}
