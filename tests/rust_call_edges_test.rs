//! End-to-end Rust call edges: index a small crate, then read the `calls`
//! edges back from SQLite.

use std::fs;
use std::path::Path;

use codegraph::db::{DatabaseConnection, get_database_path};
use codegraph::{CodeGraph, IndexOptions};

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
