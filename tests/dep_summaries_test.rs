//! Taint through a dependency's own code: a fixture crate in a fake cargo
//! registry, its shard, its summaries (what each function's return and
//! returned fields carry), the project's external edges into it, and a
//! Rust taint rule whose flows follow the summaries — where the library
//! models would say "a call's result carries its arguments".
//!
//! One test per binary: it points `CODEGRAPH_HOME` at a scratch home.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use codegraph::analyze::bugs::BugsOptions;
use codegraph::analyze::rules::{RuleSet, rules_report};
use codegraph::deps::builder::{BuildOptions, build_pending};
use codegraph::deps::locate::SourceRoots;
use codegraph::deps::project::{canonical_root, record_project};
use codegraph::deps::registry::PendingScope;
use codegraph::deps::summaries::compose::Access;
use codegraph::deps::summaries::{BuildLimits, EnsureOutcome, ensure, store};
use codegraph::deps::{DepKey, DepsHome, Ecosystem, Registry, ShardHandle};
use codegraph::resolution::external::{ExternalMode, ExternalOptions, FederationHome};
use codegraph::{CodeGraph, ExternalScope, IndexOptions, OpenOptions};

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// The dependency: a function that passes its input on, one that returns
/// a constant, and one that builds a struct field by field.
const WRAPLIB: &str = "pub struct Split {\n    pub path: String,\n    pub label: String,\n}\n\
pub fn wrap(input: String) -> String {\n    let out = format!(\"<{}>\", input);\n    out\n}\n\
pub fn constant(input: &str) -> String {\n    String::from(\"safe\")\n}\n\
pub fn split(path: String, label: String) -> Split {\n    Split { path, label }\n}\n";

/// The project: request input through each of them into a subprocess.
const APP: &str = "use wraplib::Split;\n\
pub async fn handler(Query(q): Query<Params>) {\n\
    let a = wraplib::wrap(q.a.clone());\n\
    Command::new(a).spawn();\n\
    let b = wraplib::constant(&q.b);\n\
    Command::new(b).spawn();\n\
    let s: Split = wraplib::split(q.path.clone(), \"fixed\".to_string());\n\
    Command::new(s.label).spawn();\n\
    Command::new(s.path).spawn();\n\
}\n";

const RULE: &str = r#"id: t-command
language: rust
severity: high
taint:
  sources:
    - query: |
        ((parameter pattern: (tuple_struct_pattern type: (_) . (_) @v)))
      value: v
  sinks:
    - query: |
        ((call_expression function: (scoped_identifier) @f arguments: (arguments . (_) @arg)) (#match? @f "Command::new$"))
      argument: arg
examples:
  bad:
    - |
      async fn h(Query(q): Query<P>) { Command::new(q.x).spawn(); }
"#;

struct Machine {
    _tmp: tempfile::TempDir,
    cargo_home: PathBuf,
    home: PathBuf,
    app: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let machine = Machine {
            cargo_home: root.join("cargo"),
            home: root.join("cghome"),
            app: root.join("app"),
            _tmp: tmp,
        };
        let krate = machine
            .cargo_home
            .join("registry/src/index.crates.io-1949cf8c6b5b557f/wraplib-1.0.0");
        write(
            &krate.join("Cargo.toml"),
            "[package]\nname = \"wraplib\"\nversion = \"1.0.0\"\n",
        );
        write(&krate.join("src/lib.rs"), WRAPLIB);
        write(
            &machine.app.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nwraplib = \"1\"\n",
        );
        write(&machine.app.join("src/lib.rs"), APP);
        write(
            &machine.app.join("Cargo.lock"),
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"wraplib\",\n]\n\n\
             [[package]]\nname = \"wraplib\"\nversion = \"1.0.0\"\n\
             source = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        );
        machine
    }

    fn deps(&self) -> DepsHome {
        DepsHome::at(self.home.join("deps"))
    }

    fn key(&self) -> DepKey {
        DepKey::new(Ecosystem::Crates, "wraplib", "1.0.0")
    }
}

/// The lines of `t-command` findings, with and without summaries.
fn findings(cg: &CodeGraph, root: &Path, summaries: bool) -> Vec<u32> {
    let rules = RuleSet::load(&[], &[("t.yaml".into(), RULE.into())], false);
    assert!(rules.errors.is_empty(), "{:?}", rules.errors);
    let options = BugsOptions {
        dependency_summaries: summaries.then_some(Access::ReadOnly),
        ..BugsOptions::default()
    };
    let report = rules_report(cg, root, &rules, &options).unwrap();
    let mut lines: Vec<u32> = report.findings.iter().map(|f| f.line).collect();
    lines.sort_unstable();
    lines
}

#[tokio::test(flavor = "multi_thread")]
async fn taint_follows_a_dependencys_summaries() {
    let machine = Machine::new();
    // The rules read summaries from the machine's store.
    // SAFETY-free: edition 2021, and this binary's only test.
    unsafe_free_set_home(&machine.home);

    // Index, record, build the shard.
    let cg = CodeGraph::init_sync(&machine.app).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    cg.close();
    let deps = machine.deps();
    deps.ensure().unwrap();
    let mut registry = Registry::open(&deps.registry_path()).unwrap();
    record_project(
        &mut registry,
        &machine.app,
        &SourceRoots::new(Some(machine.cargo_home.clone()), None),
        false,
        1,
    )
    .unwrap();
    let root = canonical_root(&machine.app);
    build_pending(
        &deps,
        &registry,
        PendingScope::Project(&root),
        &BuildOptions::default(),
        &mut |_| {},
    )
    .await
    .unwrap();

    // The dependency's summaries: computed once, stored beside its graph.
    let outcome = ensure(&deps, &machine.key(), &BuildLimits::default());
    assert!(
        matches!(outcome, EnsureOutcome::Built { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        ensure(&deps, &machine.key(), &BuildLimits::default()),
        EnsureOutcome::UpToDate
    );
    let handle = ShardHandle::open(&deps, &machine.key()).unwrap();
    let artifact = store::read(handle.dir(), handle.meta()).expect("current summaries");
    assert!(artifact.complete);
    let summary_of = |name: &str| {
        let index = artifact
            .functions
            .iter()
            .position(|f| f.q == name)
            .unwrap_or_else(|| panic!("{name} in the artifact"));
        artifact
            .summaries
            .iter()
            .find(|s| s.function as usize == index)
            .cloned()
    };
    let wrap = summary_of("wrap").expect("wrap passes its input on");
    assert_eq!(wrap.returns.len(), 1);
    assert_eq!(wrap.returns[0].input.slot, "p0");
    assert!(
        artifact.functions.iter().all(|f| f.q != "constant")
            || summary_of("constant").is_none_or(|s| s.returns.is_empty()),
        "constant returns none of its input"
    );
    let split = summary_of("split").expect("split's fields");
    let field = |name: &str| {
        split
            .fields
            .iter()
            .find(|(f, _)| f == name)
            .map(|(_, output)| {
                output
                    .inputs
                    .iter()
                    .map(|i| i.input.slot.clone())
                    .collect::<Vec<_>>()
            })
    };
    assert_eq!(field("path"), Some(vec!["p0".to_string()]));
    assert_eq!(field("label"), Some(vec!["p1".to_string()]));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(store::artifact_path(handle.dir()))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    drop(handle);

    // The project's calls into it become external edges.
    let cg = CodeGraph::open(&machine.app, &OpenOptions::default()).unwrap();
    cg.resolve_external(
        ExternalScope::AllUnresolved,
        &ExternalOptions {
            mode: ExternalMode::Full,
            budget: Duration::from_secs(60),
            max_open: 4,
            home: FederationHome::at(&machine.home),
            reach: None,
        },
    )
    .await
    .unwrap()
    .expect("the project lock is free");

    // Library models: every call's result carries its arguments, so all
    // four subprocesses take request input.
    assert_eq!(findings(&cg, &machine.app, false), vec![4, 6, 8, 9]);
    // Summaries: `constant` hands back none of it, and `split` keeps its
    // fields apart — only `wrap`'s result and `.path` carry the input.
    assert_eq!(findings(&cg, &machine.app, true), vec![4, 9]);
    // The evidence steps into the dependency, with its own file and line.
    let rules = RuleSet::load(&[], &[("t.yaml".into(), RULE.into())], false);
    let options = BugsOptions {
        dependency_summaries: Some(Access::ReadOnly),
        ..BugsOptions::default()
    };
    let report = rules_report(&cg, &machine.app, &rules, &options).unwrap();
    let wrapped = report.findings.iter().find(|f| f.line == 4).unwrap();
    let into_dependency = wrapped
        .evidence
        .iter()
        .find(|e| e.file.ends_with("wraplib-1.0.0/src/lib.rs"))
        .unwrap_or_else(|| panic!("a step in wraplib: {:#?}", wrapped.evidence));
    assert!(matches!(into_dependency.line, 5..=7), "{into_dependency:?}");
    cg.close();
}

/// Point this process's CodeGraph home at `home` (the rules' summaries
/// are read from `$CODEGRAPH_HOME/deps`).
fn unsafe_free_set_home(home: &Path) {
    // Edition 2021: `set_var` is a safe fn; this binary runs one test.
    std::env::set_var("CODEGRAPH_HOME", home);
}
