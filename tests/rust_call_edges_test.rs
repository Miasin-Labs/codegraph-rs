//! End-to-end Rust call edges: index a small crate, then read the `calls`
//! edges back from SQLite.

use std::fs;
use std::path::Path;

use codegraph::db::{DatabaseConnection, get_database_path};
use codegraph::{CodeGraph, ExternalScope, IndexOptions};

#[path = "federation/fixture.rs"]
mod federation;

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

/// Every `calls` edge as `source qualified name -> target qualified name`.
fn call_edges(root: &Path) -> Vec<String> {
    let conn = DatabaseConnection::open(get_database_path(root)).unwrap();
    let db = conn.get_db().unwrap();
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT s.qualified_name || ' -> ' || t.qualified_name
             FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
             WHERE e.kind = 'calls' ORDER BY 1",
        )
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

async fn index_crate(files: &[(&str, &str)]) -> (tempfile::TempDir, Vec<String>) {
    let dir = tempfile::TempDir::new().unwrap();
    for (path, content) in files {
        write(&dir.path().join(path), content);
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    cg.close();
    let edges = call_edges(dir.path());
    (dir, edges)
}

#[tokio::test(flavor = "current_thread")]
async fn calls_inside_macro_arguments_become_edges() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "mod widget;\nmod checks;\n"),
        (
            "src/widget.rs",
            "pub struct Widget;\n\
             impl Widget {\n    pub fn build_widget() -> Self { Widget }\n}\n\
             pub fn widget_count() -> usize { 1 }\n",
        ),
        (
            "src/checks.rs",
            "use crate::widget::{widget_count, Widget};\n\
             pub fn check_all() {\n\
             \x20   assert_eq!(widget_count(), 1);\n\
             \x20   let all = vec![Widget::build_widget()];\n\
             \x20   println!(\"{}\", all.len());\n\
             }\n",
        ),
    ])
    .await;
    for expected in [
        "check_all -> widget_count",
        "check_all -> Widget::build_widget",
    ] {
        assert!(
            edges.iter().any(|edge| edge == expected),
            "missing {expected}: {edges:?}"
        );
    }
}

/// `v.iter().next()` names only `next`; exact-name matching used to land it
/// on the project's sole `next`. A project-unique method name in a chain
/// still resolves.
#[tokio::test(flavor = "current_thread")]
async fn chained_std_calls_do_not_resolve_to_same_named_project_methods() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "mod frontier;\nmod graph;\nmod walk;\n"),
        (
            "src/frontier.rs",
            "pub struct FrontierIter(u32);\n\
             impl Iterator for FrontierIter {\n\
             \x20   type Item = u32;\n\
             \x20   fn next(&mut self) -> Option<u32> { None }\n\
             }\n",
        ),
        (
            "src/graph.rs",
            "pub struct Graph;\n\
             impl Graph {\n\
             \x20   pub fn get_outgoing_edges(&self, id: u32) -> Vec<u32> { vec![id] }\n\
             }\n\
             pub fn load_graph() -> Graph { Graph }\n",
        ),
        (
            "src/walk.rs",
            "use crate::graph::load_graph;\n\
             pub fn walk(v: Vec<u32>) -> Option<u32> {\n\
             \x20   let first = v.iter().next().copied();\n\
             \x20   let edges = load_graph().get_outgoing_edges(1);\n\
             \x20   assert_eq!(v.iter().next(), edges.first());\n\
             \x20   first\n\
             }\n",
        ),
    ])
    .await;
    assert!(
        !edges
            .iter()
            .any(|edge| edge == "walk -> FrontierIter::next"),
        "std `.next()` resolved to a project method: {edges:?}"
    );
    assert!(
        edges
            .iter()
            .any(|edge| edge == "walk -> Graph::get_outgoing_edges"),
        "project-unique chained call should resolve: {edges:?}"
    );
}

/// `chars.next()` and `map.get(k)` run std methods, and `HashMap::new()`
/// names a std type: none of them lands on a same-named project method.
/// A receiver whose type the code spells (`let graph = Graph::new()`)
/// resolves on that type.
#[tokio::test(flavor = "current_thread")]
async fn identifier_receivers_resolve_on_their_inferred_type() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "mod frontier;\nmod graph;\nmod walk;\n"),
        (
            "src/frontier.rs",
            "pub struct FrontierIter(u32);\n\
             impl Iterator for FrontierIter {\n\
             \x20   type Item = u32;\n\
             \x20   fn next(&mut self) -> Option<u32> { None }\n\
             }\n",
        ),
        (
            "src/graph.rs",
            "pub struct Graph;\n\
             impl Graph {\n\
             \x20   pub fn new() -> Self { Graph }\n\
             \x20   pub fn get(&self, id: u32) -> Option<u32> { Some(id) }\n\
             }\n\
             pub struct OrderedNodeMap;\n\
             impl OrderedNodeMap {\n\
             \x20   pub fn new() -> Self { OrderedNodeMap }\n\
             }\n",
        ),
        (
            "src/walk.rs",
            "use std::collections::HashMap;\n\
             use crate::graph::Graph;\n\
             pub fn walk(text: &str) -> usize {\n\
             \x20   let mut chars = text.chars();\n\
             \x20   let first = chars.next();\n\
             \x20   let graph = Graph::new();\n\
             \x20   let near = graph.get(1);\n\
             \x20   let map: HashMap<u32, u32> = HashMap::new();\n\
             \x20   let far = map.get(&1).copied();\n\
             \x20   [first, near, far].len()\n\
             }\n",
        ),
    ])
    .await;
    for wrong in ["walk -> FrontierIter::next", "walk -> OrderedNodeMap::new"] {
        assert!(
            !edges.iter().any(|edge| edge == wrong),
            "std call resolved to a project method ({wrong}): {edges:?}"
        );
    }
    assert!(
        edges.iter().any(|edge| edge == "walk -> Graph::new"),
        "missing walk -> Graph::new: {edges:?}"
    );
    let graph_gets = edges
        .iter()
        .filter(|edge| *edge == "walk -> Graph::get")
        .count();
    assert_eq!(graph_gets, 1, "only `graph.get` runs Graph::get: {edges:?}");
}

/// Call syntax limits what a call can run. A bare `stop()` names the
/// closure parameter and `probe(..)` the local closure, never a same-named
/// method or project fn; a bare `drop(..)`/`Ok(..)` is the prelude's unless
/// a `use` brings a project item into scope; and a std method name on a
/// receiver of unknown type (`root.join(..).parent()`,
/// `|poisoned| poisoned.into_inner()`) is never guessed onto a project
/// method. Typed receivers and fns nested in a method still resolve.
#[tokio::test(flavor = "current_thread")]
async fn call_syntax_limits_what_a_call_can_run() {
    let (_dir, edges) = index_crate(&[
        (
            "src/lib.rs",
            "mod daemon;\nmod edge;\nmod outcome;\nmod run;\n",
        ),
        (
            "src/daemon.rs",
            "pub struct Daemon;\n\
             impl Daemon {\n\
             \x20   pub fn stop(&self) {}\n\
             \x20   pub fn parent(&self) -> Option<Daemon> { None }\n\
             \x20   pub fn rank(&self) -> u8 {\n\
             \x20       fn kind_class(n: u8) -> u8 { n }\n\
             \x20       kind_class(1)\n\
             \x20   }\n\
             }\n\
             impl Drop for Daemon {\n\
             \x20   fn drop(&mut self) {}\n\
             }\n\
             pub fn probe(n: u8) -> u8 { n }\n",
        ),
        (
            "src/edge.rs",
            "pub struct TypedEdge(u8);\n\
             impl TypedEdge {\n\
             \x20   pub fn into_inner(self) -> u8 { self.0 }\n\
             }\n",
        ),
        (
            "src/outcome.rs",
            "pub enum Outcome {\n\
             \x20   Ok(u8),\n\
             \x20   Failed,\n\
             }\n\
             use Outcome::*;\n\
             pub fn outcome() -> Outcome {\n\
             \x20   Ok(1)\n\
             }\n",
        ),
        (
            "src/run.rs",
            "use std::path::Path;\n\
             use std::sync::Mutex;\n\
             use crate::edge::TypedEdge;\n\
             pub fn check(\n\
             \x20   root: &Path,\n\
             \x20   stop: &dyn Fn() -> bool,\n\
             \x20   lock: &Mutex<u8>,\n\
             \x20   edge: TypedEdge,\n\
             ) -> Result<u8, String> {\n\
             \x20   if stop() {\n\
             \x20       return Err(String::from(\"stopped\"));\n\
             \x20   }\n\
             \x20   let parent = root.join(\"x\").parent().map(|p| p.to_path_buf());\n\
             \x20   let value = *lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());\n\
             \x20   let probe = |n: u8| n + 1;\n\
             \x20   drop(parent);\n\
             \x20   Ok(probe(value) + edge.into_inner())\n\
             }\n",
        ),
    ])
    .await;
    for wrong in [
        "check -> Daemon::stop",
        "check -> Daemon::parent",
        "check -> Daemon::drop",
        "check -> probe",
        "check -> Outcome::Ok",
    ] {
        assert!(
            !edges.iter().any(|edge| edge == wrong),
            "call resolved against its syntax ({wrong}): {edges:?}"
        );
    }
    // `edge: TypedEdge` resolves on its type; `poisoned` has none.
    let into_inner = edges
        .iter()
        .filter(|edge| *edge == "check -> TypedEdge::into_inner")
        .count();
    assert_eq!(into_inner, 1, "only `edge.into_inner()`: {edges:?}");
    for expected in [
        "outcome -> Outcome::Ok",
        "Daemon::rank -> Daemon::kind_class",
    ] {
        assert!(
            edges.iter().any(|edge| edge == expected),
            "missing {expected}: {edges:?}"
        );
    }
}

/// `self.graph.get(1)` is recorded without its receiver; the field's
/// declared type resolves it, where name matching alone would leave a std
/// name (`get`) unresolved and land a shared one (`get_outgoing_edges`) on
/// the nearest same-named method, here the facade's own.
#[tokio::test(flavor = "current_thread")]
async fn self_field_receivers_resolve_on_their_declared_type() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "mod graph;\nmod facade;\n"),
        (
            "src/graph.rs",
            "pub struct Graph;\n\
             impl Graph {\n\
             \x20   pub fn get(&self, id: u32) -> Option<u32> { Some(id) }\n\
             \x20   pub fn get_outgoing_edges(&self, id: u32) -> Vec<u32> { vec![id] }\n\
             }\n",
        ),
        (
            "src/facade.rs",
            "use std::sync::Arc;\n\
             use crate::graph::Graph;\n\
             pub struct Facade {\n\
             \x20   graph: Arc<Graph>,\n\
             }\n\
             impl Facade {\n\
             \x20   pub fn get(&self, id: u32) -> Option<u32> {\n\
             \x20       self.graph.get(id)\n\
             \x20   }\n\
             \x20   pub fn get_outgoing_edges(&self, id: u32) -> Vec<u32> {\n\
             \x20       self.graph.get_outgoing_edges(id)\n\
             \x20   }\n\
             }\n",
        ),
    ])
    .await;
    for expected in [
        "Facade::get -> Graph::get",
        "Facade::get_outgoing_edges -> Graph::get_outgoing_edges",
    ] {
        assert!(
            edges.iter().any(|edge| edge == expected),
            "missing {expected}: {edges:?}"
        );
    }
    assert!(
        !edges
            .iter()
            .any(|edge| edge == "Facade::get_outgoing_edges -> Facade::get_outgoing_edges"),
        "the facade does not call itself: {edges:?}"
    );
}

/// A chained receiver is typed link by link: an associated fn's return
/// type, a method's, a field's, and what `RefCell::borrow_mut` and
/// `Mutex::lock().unwrap()` hand out. Std method names (`neg`, `as_u64`,
/// `clear`) resolve on the project type the chain reaches, and a
/// project-specific name lands on that type rather than a nearer namesake.
#[tokio::test(flavor = "current_thread")]
async fn chained_receivers_resolve_on_the_type_each_link_returns() {
    let (_dir, edges) = index_crate(&[
        ("src/lib.rs", "mod cache;\nmod rule;\nmod use_site;\n"),
        (
            "src/rule.rs",
            "pub struct Rule(u8);\n\
             impl Rule {\n\
             \x20   pub fn new(a: u8, b: u8) -> Self { Rule(a + b) }\n\
             \x20   pub fn neg(self) -> Rule { Rule(0) }\n\
             }\n\
             pub struct Digest(u64);\n\
             impl Digest {\n\
             \x20   pub fn as_u64(&self) -> u64 { self.0 }\n\
             }\n\
             pub struct Hasher;\n\
             impl Hasher {\n\
             \x20   pub fn finish(&self) -> Digest { Digest(0) }\n\
             }\n\
             pub struct TreeExtractor;\n\
             impl TreeExtractor {\n\
             \x20   pub fn new(path: &str) -> Self { TreeExtractor }\n\
             \x20   pub fn extract(&self) -> u8 { 0 }\n\
             }\n",
        ),
        (
            "src/cache.rs",
            "pub struct Cache;\n\
             impl Cache {\n\
             \x20   pub fn clear(&mut self) {}\n\
             }\n\
             pub struct Registry;\n\
             impl Registry {\n\
             \x20   pub fn lookup_entry(&self, key: &str) -> Option<u8> { None }\n\
             }\n",
        ),
        (
            "src/use_site.rs",
            "use std::cell::RefCell;\n\
             use std::sync::{Arc, Mutex};\n\
             use crate::cache::{Cache, Registry};\n\
             use crate::rule::{Hasher, Rule, TreeExtractor};\n\
             pub struct Negation;\n\
             impl Negation {\n\
             \x20   pub fn neg(self) -> Negation { Negation }\n\
             }\n\
             pub struct Count;\n\
             impl Count {\n\
             \x20   pub fn as_u64(&self) -> u64 { 0 }\n\
             }\n\
             pub struct OtherRegistry;\n\
             impl OtherRegistry {\n\
             \x20   pub fn lookup_entry(&self, key: &str) -> Option<u8> { None }\n\
             }\n\
             pub struct AstroExtractor;\n\
             impl AstroExtractor {\n\
             \x20   pub fn extract(&self) -> u8 { 1 }\n\
             }\n\
             pub struct Holder {\n\
             \x20   cache: RefCell<Cache>,\n\
             \x20   map: Arc<Mutex<Registry>>,\n\
             }\n\
             impl Holder {\n\
             \x20   pub fn run(&self, x: Hasher) {\n\
             \x20       Rule::new(1, 2).neg();\n\
             \x20       x.finish().as_u64();\n\
             \x20       self.cache.borrow_mut().clear();\n\
             \x20       self.map.lock().unwrap().lookup_entry(\"k\");\n\
             \x20       TreeExtractor::new(\"a.rs\").extract();\n\
             \x20   }\n\
             }\n",
        ),
    ])
    .await;
    for expected in [
        "Holder::run -> Rule::neg",
        "Holder::run -> Digest::as_u64",
        "Holder::run -> Cache::clear",
        "Holder::run -> Registry::lookup_entry",
        "Holder::run -> TreeExtractor::extract",
    ] {
        assert!(
            edges.iter().any(|edge| edge == expected),
            "missing {expected}: {edges:?}"
        );
    }
    for guessed in [
        "Holder::run -> Negation::neg",
        "Holder::run -> Count::as_u64",
        "Holder::run -> OtherRegistry::lookup_entry",
        "Holder::run -> AstroExtractor::extract",
    ] {
        assert!(
            !edges.iter().any(|edge| edge == guessed),
            "name-guessed {guessed}: {edges:?}"
        );
    }
}

/// A method a direct dependency defines (read from its vendored source,
/// which `Cargo.lock` pins) is not guessed onto a same-named project method
/// of a type the calling file never names when the receiver's type is
/// unknown; a project-specific name still is. The dependency's names are
/// kept in the project's `.codegraph/deps/`.
#[tokio::test(flavor = "current_thread")]
async fn dependency_method_names_are_not_resolved_by_name_alone() {
    let dir = tempfile::TempDir::new().unwrap();
    let files = [
        (
            "Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[dependencies]\ntreelike = \"0.2\"\n",
        ),
        (
            "Cargo.lock",
            "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\n \"treelike\",\n]\n\n[[package]]\nname = \"treelike\"\nversion = \"0.2.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n",
        ),
        (
            "vendor/treelike/Cargo.toml",
            "[package]\nname = \"treelike\"\nversion = \"0.2.0\"\n",
        ),
        (
            "vendor/treelike/src/lib.rs",
            "pub struct Node;\n\
             pub fn parse(text: &str) -> Node { Node }\n\
             impl Node {\n\
             \x20   pub fn walk(&self) {}\n\
             \x20   pub fn child(&self, index: usize) -> Node { Node }\n\
             }\n",
        ),
        ("src/lib.rs", "mod graph;\nmod visit;\n"),
        (
            "src/graph.rs",
            "pub struct Graph;\n\
             impl Graph {\n\
             \x20   pub fn walk(&self) {}\n\
             \x20   pub fn render_graph(&self) {}\n\
             }\n",
        ),
        (
            "src/visit.rs",
            "pub fn visit() {\n\
             \x20   let node = treelike::parse(\"x\");\n\
             \x20   node.walk();\n\
             \x20   treelike::parse(\"y\").child(0).walk();\n\
             \x20   node.render_graph();\n\
             }\n",
        ),
    ];
    for (path, content) in files {
        write(&dir.path().join(path), content);
    }
    let cg = CodeGraph::init_sync(dir.path()).unwrap();
    cg.index_all(&IndexOptions {
        dependency_scan: true,
        ..IndexOptions::default()
    })
    .await
    .unwrap();
    cg.close();
    let edges = call_edges(dir.path());
    assert!(
        !edges.iter().any(|edge| edge == "visit -> Graph::walk"),
        "a dependency's `walk` was guessed onto the project's: {edges:?}"
    );
    assert!(
        edges
            .iter()
            .any(|edge| edge == "visit -> Graph::render_graph"),
        "a project-specific name still resolves by name: {edges:?}"
    );
    assert!(
        dir.path()
            .join(".codegraph/deps/treelike-0.2.0.api")
            .is_file(),
        "the vendored crate's artifact stays with the project"
    );
}

/// Calls whose callee lives in a dependency: the in-project pass leaves
/// them unresolved (a dependency type runs no project method), and the
/// external pass resolves them into the dependency's shard — through a
/// path, a `use`, a typed receiver, and a chain typed by the dependency's
/// own return types.
#[tokio::test(flavor = "multi_thread")]
async fn dependency_calls_become_external_edges_into_their_shards() {
    let machine = federation::Federation::new();
    machine.prepare(true).await;
    assert!(
        !call_edges(&machine.app)
            .iter()
            .any(|edge| edge.contains("query_map") || edge.contains("from_str")),
        "a dependency's method landed on a project node"
    );
    let report = machine.resolve(ExternalScope::AllUnresolved).await;
    assert!(report.complete, "{report:#?}");
    let edges = machine.external_edges();
    for expected in [
        "run jsonish::from_str -> dependency:jsonish-1.0.0::from_str",
        "run Connection::open -> dependency:sqlish-0.3.0::Connection::open",
        "run root.walk -> dependency:treelike-0.2.0::Node::walk",
        "run query_map -> dependency:sqlish-0.3.0::Statement::query_map (dependency-chain",
        "run facade::Command::new -> dependency:facade_builder-4.0.0::Command::new (re-export",
    ] {
        assert!(
            edges.iter().any(|edge| edge.starts_with(expected)),
            "missing {expected}: {edges:#?}"
        );
    }
}

/// A path dependency that is its own indexed project resolves into that
/// project's index, by the crate's module paths.
#[tokio::test(flavor = "multi_thread")]
async fn path_dependency_calls_resolve_into_the_linked_project() {
    let machine = federation::Federation::new();
    machine.prepare(false).await;
    machine.resolve(ExternalScope::AllUnresolved).await;
    let edges = machine.external_edges();
    for expected in [
        "run linkme::trace_detail -> project:linkme::trace_detail",
        "run linkme::Field::text -> project:linkme::Field::text",
        "run linkme::deep::helper -> project:linkme::helper",
    ] {
        assert!(
            edges.iter().any(|edge| edge.starts_with(expected)),
            "missing {expected}: {edges:#?}"
        );
    }
}
