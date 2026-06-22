use std::fs;
use std::path::PathBuf;

use codegraph::db::{DatabaseConnection, QueryBuilder};
use codegraph::resolution::{ReferenceResolver, UnresolvedRef, create_resolver};
use codegraph::types::{EdgeKind, FileRecord, Language, Node, NodeKind};
use tempfile::{TempDir, tempdir};

pub(crate) struct Fx {
    _dir: TempDir,
    root: PathBuf,
    conn: DatabaseConnection,
}

impl Fx {
    pub(crate) fn new() -> Self {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let conn = DatabaseConnection::initialize(root.join(".codegraph").join("codegraph.db"))
            .expect("initialize db");
        Self {
            _dir: dir,
            root,
            conn,
        }
    }

    pub(crate) fn q(&self) -> QueryBuilder {
        QueryBuilder::new(self.conn.get_db().expect("db"))
    }

    pub(crate) fn resolver(&self) -> ReferenceResolver {
        create_resolver(self.root.to_string_lossy().to_string(), self.q())
    }

    pub(crate) fn write(&self, rel: &str, content: &str) {
        let path = self.root.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir -p");
        }
        fs::write(path, content).expect("write fixture");
    }

    pub(crate) fn track(&self, q: &QueryBuilder, path: &str) {
        q.upsert_file(&FileRecord {
            path: path.to_string(),
            content_hash: "test".to_string(),
            language: Language::Java,
            size: 1,
            modified_at: 1,
            indexed_at: 1,
            node_count: 0,
            errors: None,
        })
        .expect("track file");
    }
}

pub(crate) fn node(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified_name: &str,
    file_path: &str,
    start_line: u32,
    end_line: u32,
) -> Node {
    Node::new(
        id,
        kind,
        name,
        qualified_name,
        file_path,
        Language::Java,
        start_line,
        end_line,
    )
}

pub(crate) fn class_node(id: &str, package_name: &str, file_path: &str) -> Node {
    node(
        id,
        NodeKind::Class,
        "Settings",
        &format!("{package_name}::Settings"),
        file_path,
        1,
        2,
    )
}

pub(crate) fn ref_from(
    caller: &Node,
    name: &str,
    kind: EdgeKind,
    file_path: &str,
) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: caller.id.clone(),
        reference_name: name.to_string(),
        reference_kind: kind,
        line: caller.start_line,
        column: 0,
        file_path: file_path.to_string(),
        language: Language::Java,
        candidates: None,
    }
}
