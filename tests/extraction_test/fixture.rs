pub(crate) use std::fs;
use std::path::Path;
use std::process::Command;

pub(crate) use codegraph::db::{DatabaseConnection, QueryBuilder};
pub(crate) use codegraph::extraction::{
    ExtractionOrchestrator,
    FileStats,
    detect_language,
    extract_from_source,
    get_supported_languages,
    init_grammars,
    is_ida_generated_c,
    is_language_supported,
    is_source_file,
    load_all_grammars,
    scan_directory,
};
pub(crate) use codegraph::types::{
    EdgeKind,
    ExtractionResult,
    Language,
    Node,
    NodeKind,
    UnresolvedReference,
    Visibility,
};
pub(crate) use codegraph::utils::normalize_path;

/// `extractFromSource(path, code)` - the two-arg TS call shape.
pub(crate) fn extract(path: &str, code: &str) -> ExtractionResult {
    init_grammars();
    load_all_grammars();
    extract_from_source(path, code, None, None)
}

pub(crate) fn find_kind(result: &ExtractionResult, kind: NodeKind) -> Option<&Node> {
    result.nodes.iter().find(|n| n.kind == kind)
}

pub(crate) fn filter_kind(result: &ExtractionResult, kind: NodeKind) -> Vec<&Node> {
    result.nodes.iter().filter(|n| n.kind == kind).collect()
}

pub(crate) fn find_named<'a>(
    result: &'a ExtractionResult,
    kind: NodeKind,
    name: &str,
) -> Option<&'a Node> {
    result
        .nodes
        .iter()
        .find(|n| n.kind == kind && n.name == name)
}

pub(crate) fn names(nodes: &[&Node]) -> Vec<String> {
    nodes.iter().map(|n| n.name.clone()).collect()
}

pub(crate) fn refs_of_kind(result: &ExtractionResult, kind: EdgeKind) -> Vec<&UnresolvedReference> {
    result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == kind)
        .collect()
}

pub(crate) fn find_ref<'a>(
    result: &'a ExtractionResult,
    kind: EdgeKind,
    name: &str,
) -> Option<&'a UnresolvedReference> {
    result
        .unresolved_references
        .iter()
        .find(|r| r.reference_kind == kind && r.reference_name == name)
}

pub(crate) fn ref_names(refs: &[&UnresolvedReference]) -> Vec<String> {
    refs.iter().map(|r| r.reference_name.clone()).collect()
}

pub(crate) fn import_nodes(result: &ExtractionResult) -> Vec<&Node> {
    filter_kind(result, NodeKind::Import)
}

pub(crate) fn first_import(result: &ExtractionResult) -> Option<&Node> {
    find_kind(result, NodeKind::Import)
}

pub(crate) fn sig(node: &Node) -> &str {
    node.signature.as_deref().unwrap_or("")
}

/// The `CodeGraph.initSync(tempDir)` layers used directly: a `.codegraph` DB +
/// QueryBuilder + orchestrator over the project dir.
pub(crate) fn open_graph(dir: &Path) -> (DatabaseConnection, QueryBuilder) {
    let cg_dir = dir.join(".codegraph");
    fs::create_dir_all(&cg_dir).unwrap();
    let conn = DatabaseConnection::initialize(cg_dir.join("codegraph.db")).expect("initialize db");
    let db = conn.get_db().expect("get db");
    (conn, QueryBuilder::new(db))
}

pub(crate) fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
        ])
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git spawn");
    assert!(status.success(), "git {:?} failed", args);
}
