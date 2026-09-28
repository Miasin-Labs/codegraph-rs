//! Dependency shards with rustdoc API indexes, end to end: a fake registry
//! holding `widgets` and `widgets_core` (`tests/rustdoc_fixture/`, whose
//! rustdoc JSON is checked in beside them), a project using them in every
//! way the tree-sitter heuristics cannot follow — a re-export through a
//! private module, a renamed re-export, an inline `pub mod task { pub use
//! widgets_core::task::*; }`, a trait's provided method, a blanket impl —
//! and the same with JSON of a format this build does not read, which must
//! fall back to today's behaviour.
//!
//! One test fn: the JSON is found through `CODEGRAPH_DEPS_RUSTDOC_JSON_DIR`,
//! a process-wide variable.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use codegraph::db::{DatabaseConnection, get_database_path};
use codegraph::deps::builder::{BuildOptions, build_pending};
use codegraph::deps::locate::SourceRoots;
use codegraph::deps::project::{canonical_root, record_project};
use codegraph::deps::registry::PendingScope;
use codegraph::deps::{DepKey, DepsHome, Ecosystem, Registry, ShardMeta};
use codegraph::resolution::external::{ExternalMode, ExternalOptions, FederationHome};
use codegraph::{CodeGraph, ExternalScope, IndexOptions, OpenOptions};

const APP: &str = "use widgets::prelude::Cog;\n\
use widgets_core::{Named, Shout};\n\
\n\
pub fn run() {\n\
    let gear = widgets::Gear::new(3);\n\
    gear.teeth();\n\
    gear.greeting();\n\
    gear.shout();\n\
    let _cog = Cog::new(1);\n\
    let ready: widgets::task::Poll<u8> = widgets::task::Poll::Ready(1);\n\
    ready.is_ready();\n\
}\n";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/rustdoc_fixture")
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn copy_dir(from: &Path, to: &Path) {
    for entry in walkdir::WalkDir::new(from).into_iter().flatten() {
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(from).unwrap();
            fs::create_dir_all(to.join(relative).parent().unwrap()).unwrap();
            fs::copy(entry.path(), to.join(relative)).unwrap();
        }
    }
}

struct Machine {
    _tmp: tempfile::TempDir,
    cargo_home: PathBuf,
    home: PathBuf,
    app: PathBuf,
    json: PathBuf,
}

impl Machine {
    /// The registry, the project and — with `format` as their
    /// `format_version` — the crates' rustdoc JSON.
    fn new(format: u32) -> Machine {
        let tmp = tempfile::Builder::new()
            .prefix("codegraph-rustdoc-test-")
            .tempdir()
            .unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let cargo_home = root.join("cargo");
        let registry = cargo_home.join("registry/src/index.crates.io-1949cf8c6b5b557f");
        let core_dir = registry.join("widgets_core-0.1.0");
        copy_dir(&fixture().join("widgets_core"), &core_dir);
        write(
            &core_dir.join("Cargo.toml"),
            "[package]\nname = \"widgets_core\"\nversion = \"0.1.0\"\n",
        );
        let widgets_dir = registry.join("widgets-0.1.0");
        copy_dir(&fixture().join("widgets"), &widgets_dir);
        write(
            &widgets_dir.join("Cargo.toml"),
            "[package]\nname = \"widgets\"\nversion = \"0.1.0\"\n\n[dependencies]\nwidgets_core = \"0.1\"\n",
        );
        // The JSON as cargo writes it for registry crates: another crate's
        // spans are absolute paths into its source.
        let json = root.join("json");
        for krate in ["widgets", "widgets_core"] {
            let text = fs::read_to_string(fixture().join(format!("json/{krate}.json"))).unwrap();
            let text = text
                .replace(
                    "\"filename\":\"widgets_core/",
                    &format!("\"filename\":\"{}/", core_dir.display()),
                )
                .replace(
                    "\"format_version\":61}",
                    &format!("\"format_version\":{format}}}"),
                );
            write(&json.join(format!("{krate}.json")), &text);
        }
        let app = root.join("app");
        write(
            &app.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\nwidgets = \"0.1\"\nwidgets_core = \"0.1\"\n",
        );
        write(&app.join("src/lib.rs"), APP);
        let source = "source = \"registry+https://github.com/rust-lang/crates.io-index\"";
        write(
            &app.join("Cargo.lock"),
            &format!(
                "version = 4\n\n\
                 [[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"widgets\",\n \"widgets_core\",\n]\n\n\
                 [[package]]\nname = \"widgets\"\nversion = \"0.1.0\"\n{source}\ndependencies = [\n \"widgets_core\",\n]\n\n\
                 [[package]]\nname = \"widgets_core\"\nversion = \"0.1.0\"\n{source}\n"
            ),
        );
        Machine {
            home: root.join("cghome"),
            cargo_home,
            app,
            json,
            _tmp: tmp,
        }
    }

    fn deps_home(&self) -> DepsHome {
        DepsHome::at(self.home.join("deps"))
    }

    async fn prepare(&self) {
        let cg = CodeGraph::init_sync(&self.app).unwrap();
        cg.index_all(&IndexOptions::default()).await.unwrap();
        cg.close();
        let home = self.deps_home();
        home.ensure().unwrap();
        let mut registry = Registry::open(&home.registry_path()).unwrap();
        record_project(
            &mut registry,
            &self.app,
            &SourceRoots::new(Some(self.cargo_home.clone()), None),
            false,
            1,
        )
        .unwrap();
        // The one test of this binary sets it, around the build only.
        std::env::set_var("CODEGRAPH_DEPS_RUSTDOC_JSON_DIR", &self.json);
        build_pending(
            &home,
            &registry,
            PendingScope::Project(&canonical_root(&self.app)),
            &BuildOptions::default(),
            &mut |_| {},
        )
        .await
        .unwrap();
        std::env::remove_var("CODEGRAPH_DEPS_RUSTDOC_JSON_DIR");
        let cg = CodeGraph::open(&self.app, &OpenOptions::default()).unwrap();
        cg.resolve_external(
            ExternalScope::AllUnresolved,
            &ExternalOptions {
                mode: ExternalMode::Full,
                budget: Duration::from_secs(60),
                max_open: 4,
                home: FederationHome::at(&self.home),
                reach: None,
            },
        )
        .await
        .unwrap()
        .expect("the project lock is free");
        cg.close();
    }

    fn meta(&self, name: &str) -> ShardMeta {
        let key = DepKey::new(Ecosystem::Crates, name, "0.1.0");
        ShardMeta::read(&self.deps_home().shard_dir(&key)).unwrap()
    }

    /// `reference -> crate::qualified name (file:line, resolved by)`.
    fn edges(&self) -> Vec<String> {
        let conn = DatabaseConnection::open(get_database_path(&self.app)).unwrap();
        let db = conn.get_db().unwrap();
        let mut stmt = db
            .conn()
            .prepare(
                "SELECT reference_name, target_graph_key, target_qualified_name,
                        target_file_path, target_line, resolved_by
                 FROM external_edges ORDER BY 1, 2",
            )
            .unwrap();
        stmt.query_map([], |row| {
            let key: String = row.get(1)?;
            Ok(format!(
                "{} -> {}::{} ({}:{}, {})",
                row.get::<_, String>(0)?,
                key.rsplit('/').next().unwrap_or_default(),
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn api_indexes_resolve_what_the_heuristics_cannot_and_fall_back_when_unreadable() {
    // Supported JSON: every reference lands on the item rustdoc names.
    let machine = Machine::new(61);
    machine.prepare().await;
    for name in ["widgets", "widgets_core"] {
        let api = machine.meta(name).api;
        assert!(api.is_current(), "{name}: {api:?}");
        assert_eq!(api.crates, [name.replace('-', "_")], "{name}: {api:?}");
        assert!(api.error.is_none(), "{name}: {api:?}");
    }
    let edges = machine.edges();
    for expected in [
        // A re-export through a private module (`pub use parts::gear::Gear`).
        "widgets::Gear::new -> widgets-0.1.0::Gear::new (src/parts/gear.rs:8, qualified-name)",
        // The type's inherent method, the trait's provided method (in the
        // crate defining the trait), and the blanket impl's method.
        "gear.teeth -> widgets-0.1.0::Gear::teeth (src/parts/gear.rs:12, instance-method)",
        "gear.greeting -> widgets_core-0.1.0::Named::greeting (src/lib.rs:17, instance-method)",
        "gear.shout -> widgets_core-0.1.0::T::shout (src/lib.rs:27, instance-method)",
        // A renamed re-export (`pub use crate::Gear as Cog`).
        "Cog::new -> widgets-0.1.0::Gear::new (src/parts/gear.rs:8, qualified-name)",
        // `pub mod task { pub use widgets_core::task::*; }`, and a method
        // of the type it re-exports.
        "widgets::task::Poll::Ready -> widgets_core-0.1.0::task::Poll::Ready (src/lib.rs:3, re-export)",
        "ready.is_ready -> widgets_core-0.1.0::Poll::is_ready (src/lib.rs:8, instance-method)",
    ] {
        assert!(
            edges.iter().any(|edge| edge == expected),
            "missing {expected}:\n{edges:#?}"
        );
    }

    // JSON of an unknown format is refused: the shards are built as before,
    // with no index, and resolve as before.
    let fallback = Machine::new(9999);
    fallback.prepare().await;
    for name in ["widgets", "widgets_core"] {
        let api = fallback.meta(name).api;
        assert!(!api.is_current(), "{name}: {api:?}");
        let error = api.error.unwrap_or_default();
        assert!(error.contains("format_version 9999"), "{name}: {error}");
    }
    let edges = fallback.edges();
    assert!(
        edges
            .iter()
            .any(|edge| edge.starts_with("widgets::Gear::new -> widgets-0.1.0::Gear::new")),
        "{edges:#?}"
    );
    assert!(
        !edges
            .iter()
            .any(|edge| edge.starts_with("widgets::task::Poll::Ready")),
        "the heuristics follow no inline glob module:\n{edges:#?}"
    );
}
