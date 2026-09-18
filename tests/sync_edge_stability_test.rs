//! A no-op edit to a file must not move or drop the edges that point INTO it.
//!
//! Sync replaces a changed file's nodes, which cascades away the inbound
//! edges; they are restored as unresolved references and retried. Restoring
//! them under the target's bare name (`new` instead of `Helper::new`) and
//! retrying only exact bare-name matches left qualified calls unresolved or
//! re-resolved to a different same-named symbol.

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

/// Every `calls` edge as `source file:line -> target qualified@file`.
fn call_edges(root: &Path) -> Vec<String> {
    let conn = DatabaseConnection::open(get_database_path(root)).unwrap();
    let db = conn.get_db().unwrap();
    let mut stmt = db
        .conn()
        .prepare(
            "SELECT s.file_path || ':' || e.line || ' -> ' || t.qualified_name || '@' || t.file_path
             FROM edges e JOIN nodes s ON s.id = e.source JOIN nodes t ON t.id = e.target
             WHERE e.kind = 'calls' ORDER BY 1",
        )
        .unwrap();
    stmt.query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[tokio::test(flavor = "current_thread")]
async fn inbound_qualified_calls_survive_a_no_op_edit_of_their_target_file() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path();
    write(
        &root.join("src/helper.rs"),
        "pub struct Helper;\nimpl Helper {\n    pub fn new() -> Self { Helper }\n}\n\
         pub struct Other;\nimpl Other {\n    pub fn new() -> Self { Other }\n}\n",
    );
    // Real codebases have many `new`s; a bare-name retry picks among them.
    for (file, ty) in [
        ("alpha", "Alpha"),
        ("beta", "Beta"),
        ("cache", "Cache"),
        ("delta", "Delta"),
    ] {
        write(
            &root.join(format!("src/{file}.rs")),
            &format!("pub struct {ty};\nimpl {ty} {{\n    pub fn new() -> Self {{ {ty} }}\n}}\n"),
        );
    }
    write(
        &root.join("src/lib.rs"),
        "mod alpha;\nmod beta;\nmod cache;\nmod delta;\nmod helper;\nmod zone;\n",
    );
    write(
        &root.join("src/zone.rs"),
        "use crate::helper::Helper;\n\
         pub fn build() -> Helper { Helper::new() }\n\
         pub fn make() { let _h = crate::helper::Helper::new(); }\n",
    );

    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).await.unwrap();
    let before = call_edges(root);
    assert!(
        before
            .iter()
            .any(|edge| edge.contains("-> Helper::new@src/helper.rs")),
        "fixture must resolve Helper::new first: {before:?}"
    );

    // A no-op edit: the target file changes on disk but not in meaning.
    let helper = root.join("src/helper.rs");
    let mut text = fs::read_to_string(&helper).unwrap();
    text.push_str("// touched\n");
    fs::write(&helper, text).unwrap();
    cg.sync(&IndexOptions::default()).await.unwrap();

    let after = call_edges(root);
    assert_eq!(before, after, "a no-op edit moved or dropped inbound edges");
    cg.close();
}
