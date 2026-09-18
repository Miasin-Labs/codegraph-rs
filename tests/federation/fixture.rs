//! A synthetic machine for cross-graph resolution: a fake `$CARGO_HOME`
//! registry holding small crates shaped like the real ones (tree-sitter,
//! rusqlite, serde_json, clap → clap_builder), a Rust project `app` that
//! pins them in its `Cargo.lock` and depends by path on a linked project
//! `linkme`, and a scratch CodeGraph home (dependency store + atlas).
//! Nothing touches the real home.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use codegraph::atlas::{GatherOptions, register_project_at};
use codegraph::db::{DatabaseConnection, get_database_path};
use codegraph::deps::builder::{BuildOptions, build_pending};
use codegraph::deps::locate::SourceRoots;
use codegraph::deps::project::{canonical_root, record_project};
use codegraph::deps::registry::PendingScope;
use codegraph::deps::{DepsHome, Registry};
use codegraph::resolution::external::{
    ExternalMode,
    ExternalOptions,
    ExternalReport,
    FederationHome,
};
use codegraph::{CodeGraph, ExternalScope, IndexOptions, OpenOptions};

pub struct Federation {
    _tmp: tempfile::TempDir,
    pub root: PathBuf,
    pub cargo_home: PathBuf,
    pub home: PathBuf,
    pub app: PathBuf,
    pub linked: PathBuf,
}

/// `(crate, version, files)` of the fake registry.
const CRATES: &[(&str, &str, &[(&str, &str)])] = &[
    (
        "treelike",
        "0.2.0",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"treelike\"\nversion = \"0.2.0\"\n\n[lib]\npath = \"binding/lib.rs\"\n",
            ),
            (
                "binding/lib.rs",
                "pub struct Tree;\n\
                 pub struct Node<'t> {\n    tree: &'t Tree,\n}\n\
                 pub struct Cursor;\n\
                 impl Tree {\n\
                 \x20   pub fn root_node(&self) -> Node<'_> { Node { tree: self } }\n\
                 }\n\
                 impl<'t> Node<'t> {\n\
                 \x20   fn new(tree: &'t Tree) -> Self { Node { tree } }\n\
                 \x20   pub fn walk(&self) -> Cursor { Cursor }\n\
                 \x20   pub fn kind(&self) -> &'static str { \"\" }\n\
                 \x20   pub fn child(&self, index: u32) -> Option<Node<'t>> { None }\n\
                 }\n\
                 impl Cursor {\n\
                 \x20   pub fn goto_first_child(&mut self) -> bool { false }\n\
                 }\n\
                 pub fn parse(text: &str) -> Tree { Tree }\n",
            ),
        ],
    ),
    (
        "sqlish",
        "0.3.0",
        &[
            (
                "src/lib.rs",
                "mod cache;\n\
                 mod statement;\n\
                 pub use crate::cache::CachedStatement;\n\
                 pub use crate::statement::Statement;\n\
                 pub struct Error;\n\
                 pub type Result<T, E = Error> = std::result::Result<T, E>;\n\
                 pub type Stmt<'c> = Statement<'c>;\n\
                 pub struct Connection;\n\
                 impl Connection {\n\
                 \x20   pub fn open(path: &str) -> Result<Connection> { Ok(Connection) }\n\
                 \x20   pub fn prepare(&self, sql: &str) -> Result<Statement<'_>> { Ok(Statement::new(self)) }\n\
                 \x20   pub fn prepare_cached(&self, sql: &str) -> Result<CachedStatement<'_>> { todo!() }\n\
                 \x20   pub fn statement(&self) -> Stmt<'_> { Statement::new(self) }\n\
                 }\n",
            ),
            (
                "src/cache.rs",
                "use std::ops::Deref;\n\
                 use crate::Statement;\n\
                 pub struct CachedStatement<'c> {\n    stmt: Statement<'c>,\n}\n\
                 impl<'c> Deref for CachedStatement<'c> {\n\
                 \x20   type Target = Statement<'c>;\n\
                 \x20   fn deref(&self) -> &Statement<'c> { &self.stmt }\n\
                 }\n",
            ),
            (
                "src/statement.rs",
                "use crate::Connection;\n\
                 pub struct Statement<'c> {\n    conn: &'c Connection,\n}\n\
                 impl<'c> Statement<'c> {\n\
                 \x20   pub(crate) fn new(conn: &'c Connection) -> Self { Statement { conn } }\n\
                 \x20   pub fn query_map(&mut self) -> usize { 0 }\n\
                 }\n",
            ),
        ],
    ),
    (
        "jsonish",
        "1.0.0",
        &[(
            "src/lib.rs",
            "pub enum Value {\n    Null,\n    Text(String),\n}\n\
             impl Value {\n\
             \x20   pub fn as_text(&self) -> Option<&str> { None }\n\
             }\n\
             impl From<i8> for Value {\n    fn from(n: i8) -> Self { Value::Null }\n}\n\
             impl From<String> for Value {\n    fn from(s: String) -> Self { Value::Text(s) }\n}\n\
             pub fn from_str(text: &str) -> Value { Value::Null }\n",
        )],
    ),
    (
        "rexish",
        "1.1.0",
        &[
            (
                "src/lib.rs",
                "mod string;\npub mod bytes;\npub use crate::string::Regex;\n",
            ),
            (
                "src/string.rs",
                "pub struct Regex;\n\
                 impl Regex {\n\
                 \x20   pub fn new(pattern: &str) -> Regex { Regex }\n\
                 \x20   pub fn is_match(&self, text: &str) -> bool { false }\n\
                 }\n",
            ),
            (
                "src/bytes.rs",
                "pub struct Regex;\n\
                 impl Regex {\n\
                 \x20   pub fn new(pattern: &str) -> Regex { Regex }\n\
                 \x20   pub fn is_match(&self, text: &[u8]) -> bool { false }\n\
                 }\n",
            ),
        ],
    ),
    (
        "facade",
        "4.0.0",
        &[("src/lib.rs", "pub use facade_builder::*;\n")],
    ),
    (
        "facade_builder",
        "4.0.0",
        &[(
            "src/lib.rs",
            "pub trait Parser {\n    fn parse_args() -> Self where Self: Sized;\n}\n\
             pub struct Command;\n\
             impl Command {\n    pub fn new(name: &str) -> Command { Command }\n}\n",
        )],
    ),
];

/// The project's code: every way a reference reaches another graph, and
/// the ways it must not.
pub const APP_MAIN: &str = "use treelike::Node;\n\
use sqlish::Connection;\n\
use rexish::Regex;\n\
use facade::Parser;\n\
\n\
pub struct Cli;\n\
\n\
impl Parser for Cli {\n\
    fn parse_args() -> Self {\n\
        Cli\n\
    }\n\
}\n\
\n\
pub fn run(text: &str) {\n\
    let value = jsonish::from_str(text);\n\
    let tree = treelike::parse(text);\n\
    let root: Node = tree.root_node();\n\
    root.walk();\n\
    let conn = Connection::open(\"db\").unwrap();\n\
    let mut stmt = conn.prepare(\"q\").unwrap();\n\
    stmt.query_map();\n\
    conn.prepare(\"q\").unwrap().query_map();\n\
    linkme::trace_detail();\n\
    linkme::Field::text(\"a\");\n\
    linkme::deep::helper();\n\
    let _n = jsonish::Value::from(3i8);\n\
    let _hidden = Node::new(&tree);\n\
    value.as_text();\n\
    let _cmd = facade::Command::new(\"x\");\n\
    let unknown = make_thing();\n\
    unknown.walk();\n\
    conn.prepare_cached(\"q\").unwrap().query_map();\n\
    conn.statement().query_map();\n\
    let text_re = Regex::new(\"a\");\n\
    text_re.is_match(text);\n\
    let bytes_re = rexish::bytes::Regex::new(\"a\");\n\
    bytes_re.is_match(b\"a\");\n\
}\n\
\n\
fn make_thing() -> Thing {\n\
    Thing\n\
}\n\
\n\
pub struct Thing;\n";

impl Federation {
    pub fn new() -> Self {
        let tmp = tempfile::Builder::new()
            .prefix("codegraph-federation-test-")
            .tempdir()
            .unwrap();
        let root = fs::canonicalize(tmp.path()).unwrap();
        let federation = Federation {
            cargo_home: root.join("cargo"),
            home: root.join("cghome"),
            app: root.join("app"),
            linked: root.join("linkme"),
            root,
            _tmp: tmp,
        };
        for (name, version, files) in CRATES {
            let dir = federation.crate_dir(name, version);
            if !files.iter().any(|(path, _)| *path == "Cargo.toml") {
                write(
                    &dir.join("Cargo.toml"),
                    &format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n"),
                );
            }
            for (path, text) in *files {
                write(&dir.join(path), text);
            }
        }
        federation.write_linked();
        federation.write_app();
        federation
    }

    pub fn crate_dir(&self, name: &str, version: &str) -> PathBuf {
        self.cargo_home
            .join("registry/src/index.crates.io-1949cf8c6b5b557f")
            .join(format!("{name}-{version}"))
    }

    fn write_linked(&self) {
        write(
            &self.linked.join("Cargo.toml"),
            "[package]\nname = \"linkme\"\nversion = \"0.1.0\"\n",
        );
        write(
            &self.linked.join("src/lib.rs"),
            "pub mod deep;\n\
             pub fn trace_detail() {}\n\
             pub struct Field;\n\
             impl Field {\n    pub fn text(label: &str) -> Field { Field }\n}\n",
        );
        write(&self.linked.join("src/deep.rs"), "pub fn helper() {}\n");
    }

    fn write_app(&self) {
        write(
            &self.app.join("Cargo.toml"),
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\
             treelike = \"0.2\"\nsqlish = \"0.3\"\njsonish = \"1\"\nfacade = \"4\"\nrexish = \"1\"\n\
             linkme = { path = \"../linkme\" }\n",
        );
        write(&self.app.join("src/lib.rs"), APP_MAIN);
        let registry = "source = \"registry+https://github.com/rust-lang/crates.io-index\"";
        write(
            &self.app.join("Cargo.lock"),
            &format!(
                "version = 4\n\n\
                 [[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"facade\",\n \"jsonish\",\n \"linkme\",\n \"rexish\",\n \"sqlish\",\n \"treelike\",\n]\n\n\
                 [[package]]\nname = \"facade\"\nversion = \"4.0.0\"\n{registry}\ndependencies = [\n \"facade_builder\",\n]\n\n\
                 [[package]]\nname = \"facade_builder\"\nversion = \"4.0.0\"\n{registry}\n\n\
                 [[package]]\nname = \"jsonish\"\nversion = \"1.0.0\"\n{registry}\n\n\
                 [[package]]\nname = \"linkme\"\nversion = \"0.1.0\"\n\n\
                 [[package]]\nname = \"rexish\"\nversion = \"1.1.0\"\n{registry}\n\n\
                 [[package]]\nname = \"sqlish\"\nversion = \"0.3.0\"\n{registry}\n\n\
                 [[package]]\nname = \"treelike\"\nversion = \"0.2.0\"\n{registry}\n"
            ),
        );
    }

    pub fn deps_home(&self) -> DepsHome {
        DepsHome::at(self.home.join("deps"))
    }

    pub fn federation_home(&self) -> FederationHome {
        FederationHome::at(&self.home)
    }

    pub fn options(&self) -> ExternalOptions {
        ExternalOptions {
            mode: ExternalMode::Full,
            budget: Duration::from_secs(60),
            max_open: 4,
            home: self.federation_home(),
        }
    }

    /// Index `dir` from scratch (in-project resolution only).
    pub async fn index(&self, dir: &Path) {
        let cg = CodeGraph::init_sync(dir).unwrap();
        cg.index_all(&IndexOptions::default()).await.unwrap();
        cg.close();
    }

    /// Record `app`'s lockfile in the dependency registry.
    pub fn record(&self) {
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
    }

    /// Build every shard `app` is missing.
    pub async fn build_shards(&self) {
        let home = self.deps_home();
        let registry = Registry::open(&home.registry_path()).unwrap();
        let root = canonical_root(&self.app);
        build_pending(
            &home,
            &registry,
            PendingScope::Project(&root),
            &BuildOptions::default(),
            &mut |_| {},
        )
        .await
        .unwrap();
    }

    /// Register `dir` (indexed) in the scratch atlas.
    pub fn register(&self, dir: &Path) {
        register_project_at(&self.home.join("atlas.db"), dir, GatherOptions::default());
    }

    /// Everything but the pass: both projects indexed and registered, the
    /// lockfile recorded, and — when `shards` — the dependency shards built.
    pub async fn prepare(&self, shards: bool) {
        self.index(&self.linked).await;
        self.register(&self.linked);
        self.index(&self.app).await;
        self.register(&self.app);
        self.record();
        if shards {
            self.build_shards().await;
        }
    }

    /// Run the external pass on `app` like the CLI does after an index.
    pub async fn resolve(&self, scope: ExternalScope) -> ExternalReport {
        self.resolve_with(scope, self.options()).await
    }

    /// [`Self::resolve`] with explicit options (a budget, say).
    pub async fn resolve_with(
        &self,
        scope: ExternalScope,
        options: ExternalOptions,
    ) -> ExternalReport {
        let cg = CodeGraph::open(&self.app, &OpenOptions::default()).unwrap();
        let report = cg
            .resolve_external(scope, &options)
            .await
            .unwrap()
            .expect("the project lock is free");
        cg.close();
        report
    }

    /// `source -> graph::target (resolved_by)` for every external edge of
    /// `app`, the graph named by its last path segment.
    pub fn external_edges(&self) -> Vec<String> {
        let conn = DatabaseConnection::open(get_database_path(&self.app)).unwrap();
        let db = conn.get_db().unwrap();
        let mut stmt = db
            .conn()
            .prepare(
                "SELECT s.qualified_name, e.reference_name, e.target_graph_kind, e.target_graph_key,
                        e.target_qualified_name, e.resolved_by, e.kind
                 FROM external_edges e JOIN nodes s ON s.id = e.source ORDER BY 1, 2",
            )
            .unwrap();
        stmt.query_map([], |row| {
            let key: String = row.get(3)?;
            let graph = key.rsplit('/').next().unwrap_or_default().to_string();
            Ok(format!(
                "{} {} -> {}:{}::{} ({}, {})",
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                graph,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
    }

    /// `(reference as written, target file)` of every external edge.
    pub fn external_edge_targets(&self) -> Vec<(String, String)> {
        let conn = DatabaseConnection::open(get_database_path(&self.app)).unwrap();
        let db = conn.get_db().unwrap();
        let mut stmt = db
            .conn()
            .prepare("SELECT reference_name, target_file_path FROM external_edges ORDER BY 1, 2")
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// How many unresolved references of `app` are named `name`.
    pub fn unresolved_named(&self, name: &str) -> i64 {
        let conn = DatabaseConnection::open(get_database_path(&self.app)).unwrap();
        let db = conn.get_db().unwrap();
        db.conn()
            .query_row(
                "SELECT COUNT(*) FROM unresolved_refs WHERE reference_name = ?1",
                [name],
                |row| row.get(0),
            )
            .unwrap()
    }
}

pub fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Every file under `dir` with its size and modification time.
pub fn snapshot(dir: &Path) -> BTreeMap<PathBuf, (u64, std::time::SystemTime)> {
    walkdir::WalkDir::new(dir)
        .into_iter()
        .flatten()
        .map(|entry| {
            let meta = entry.metadata().unwrap();
            (
                entry.path().to_path_buf(),
                (
                    if meta.is_file() { meta.len() } else { 0 },
                    meta.modified().unwrap(),
                ),
            )
        })
        .collect()
}
