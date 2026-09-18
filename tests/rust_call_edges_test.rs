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
