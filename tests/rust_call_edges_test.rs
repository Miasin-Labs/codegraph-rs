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
