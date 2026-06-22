pub(crate) use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use codegraph::db::{DatabaseConnection, QueryBuilder};
pub(crate) use codegraph::resolution::frameworks::{
    detect_frameworks,
    get_all_framework_resolvers,
    get_applicable_frameworks,
};
pub(crate) use codegraph::resolution::import_resolver::clear_cpp_include_dir_cache;
pub(crate) use codegraph::resolution::{
    FrameworkResolver,
    ImportMapping,
    ReferenceResolver,
    ResolutionContext,
    ResolvedRef,
    UnresolvedRef,
    create_resolver,
};
pub(crate) use codegraph::types::{
    Edge,
    EdgeKind,
    FileRecord,
    Language,
    Node,
    NodeKind,
    UnresolvedReference,
};
use tempfile::{TempDir, tempdir};

// =============================================================================
// Fixture helpers
// =============================================================================

pub(crate) struct Fx {
    _dir: TempDir,
    root: PathBuf,
    conn: DatabaseConnection,
}

impl Fx {
    pub(crate) fn new() -> Fx {
        let dir = tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let conn = DatabaseConnection::initialize(root.join(".codegraph").join("codegraph.db"))
            .expect("initialize db");
        Fx {
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

    /// Write a REAL file under the project root (creates parent dirs).
    pub(crate) fn write(&self, rel: &str, content: &str) {
        let p = self.root.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).expect("mkdir -p");
        }
        fs::write(p, content).expect("write fixture file");
    }

    /// Track a file in the `files` table (what indexing does) so
    /// `warm_caches`' known-files set sees it.
    pub(crate) fn track(&self, q: &QueryBuilder, path: &str, language: Language) {
        q.upsert_file(&FileRecord {
            path: path.to_string(),
            content_hash: "test".to_string(),
            language,
            size: 1,
            modified_at: 1,
            indexed_at: 1,
            node_count: 0,
            errors: None,
        })
        .expect("upsert file");
    }
}

pub(crate) fn node(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified_name: &str,
    file_path: &str,
    language: Language,
    start_line: u32,
    end_line: u32,
) -> Node {
    Node::new(
        id,
        kind,
        name,
        qualified_name,
        file_path,
        language,
        start_line,
        end_line,
    )
}

pub(crate) fn exported(mut n: Node) -> Node {
    n.is_exported = Some(true);
    n
}

pub(crate) fn uref(
    from: &str,
    name: &str,
    kind: EdgeKind,
    line: u32,
    file_path: &str,
    language: Language,
) -> UnresolvedReference {
    UnresolvedReference {
        from_node_id: from.to_string(),
        reference_name: name.to_string(),
        reference_kind: kind,
        line,
        column: 0,
        file_path: Some(file_path.to_string()),
        language: Some(language),
        candidates: None,
    }
}

pub(crate) fn incoming(q: &QueryBuilder, id: &str, kind: EdgeKind) -> Vec<Edge> {
    q.get_incoming_edges(id, Some(&[kind]))
        .expect("incoming edges")
}

pub(crate) fn outgoing(q: &QueryBuilder, id: &str, kind: EdgeKind) -> Vec<Edge> {
    q.get_outgoing_edges(id, Some(&[kind]), None)
        .expect("outgoing edges")
}

pub(crate) fn source_files(q: &QueryBuilder, edges: &[Edge]) -> Vec<String> {
    edges
        .iter()
        .filter_map(|e| q.get_node_by_id(&e.source).ok().flatten())
        .map(|n| n.file_path)
        .collect()
}

// =============================================================================
// Mock ResolutionContext for the framework-detection cases (mirrors the TS
// inline object-literal contexts)
// =============================================================================

#[derive(Default)]
pub(crate) struct MockCtx {
    pub(crate) files: Vec<String>,
    pub(crate) contents: HashMap<String, String>,
    pub(crate) existing: Vec<String>,
    pub(crate) root: String,
}

impl ResolutionContext for MockCtx {
    fn get_nodes_in_file(&self, _: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_name(&self, _: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_qualified_name(&self, _: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_kind(&self, _: NodeKind) -> Vec<Node> {
        Vec::new()
    }
    fn file_exists(&self, p: &str) -> bool {
        self.existing.iter().any(|f| f == p)
    }
    fn read_file(&self, p: &str) -> Option<String> {
        self.contents.get(p).cloned()
    }
    fn get_project_root(&self) -> &str {
        &self.root
    }
    fn get_all_files(&self) -> Vec<String> {
        self.files.clone()
    }
    fn get_nodes_by_lower_name(&self, _: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_import_mappings(&self, _: &str, _: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
}

// =============================================================================
// Framework Detection (resolution.test.ts)
// =============================================================================
