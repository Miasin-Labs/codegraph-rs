use std::collections::HashMap;

use crate::resolution::types::{ImportMapping, ResolutionContext, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind};

pub(super) struct Fixture {
    nodes: Vec<Node>,
    pub(super) files: HashMap<String, String>,
    pub(super) imports: Vec<ImportMapping>,
}

impl Fixture {
    pub(super) fn new(nodes: Vec<Node>) -> Self {
        Fixture {
            nodes,
            files: HashMap::new(),
            imports: Vec::new(),
        }
    }
}

impl ResolutionContext for Fixture {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.file_path == file_path)
            .cloned()
            .collect()
    }
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.name == name)
            .cloned()
            .collect()
    }
    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.qualified_name == qualified_name)
            .cloned()
            .collect()
    }
    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.kind == kind)
            .cloned()
            .collect()
    }
    fn file_exists(&self, file_path: &str) -> bool {
        self.files.contains_key(file_path)
    }
    fn read_file(&self, file_path: &str) -> Option<String> {
        self.files.get(file_path).cloned()
    }
    fn get_project_root(&self) -> &str {
        "/test"
    }
    fn get_all_files(&self) -> Vec<String> {
        self.files.keys().cloned().collect()
    }
    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.name.to_lowercase() == lower_name)
            .cloned()
            .collect()
    }
    fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
        self.imports.clone()
    }
}

pub(super) fn node(
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

pub(super) fn make_ref(
    name: &str,
    kind: EdgeKind,
    line: u32,
    file_path: &str,
    language: Language,
) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: "caller:main:caller:5".into(),
        reference_name: name.into(),
        reference_kind: kind,
        line,
        column: 10,
        file_path: file_path.into(),
        language,
        candidates: None,
    }
}
