mod context;
mod policy;

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::go_module::load_go_module;
use crate::resolution::import_resolver::load_cpp_include_dirs;
use crate::resolution::path_aliases::load_project_aliases;
use crate::resolution::types::{AliasMap, GoModule, ImportMapping, ReExport, WorkspacePackages};
use crate::resolution::workspace_packages::load_workspace_packages;
use crate::types::{Language, Node, NodeKind};

pub(super) struct ResolverSnapshot {
    context: SnapshotContext,
}

type ImportCacheKey = (String, Language);

pub(super) struct SnapshotContext {
    project_root: String,
    nodes_by_id: HashMap<String, Node>,
    nodes_by_file: HashMap<String, Vec<Node>>,
    nodes_by_name: HashMap<String, Vec<Node>>,
    nodes_by_qualified_name: HashMap<String, Vec<Node>>,
    nodes_by_kind: HashMap<NodeKind, Vec<Node>>,
    nodes_by_lower_name: HashMap<String, Vec<Node>>,
    all_files: Vec<String>,
    known_files: HashSet<String>,
    known_names: HashSet<String>,
    project_aliases: Option<AliasMap>,
    go_module: Option<GoModule>,
    workspace_packages: Option<WorkspacePackages>,
    cpp_include_dirs: Vec<String>,
    file_cache: Mutex<HashMap<String, Option<String>>>,
    import_mapping_cache: Mutex<HashMap<ImportCacheKey, Vec<ImportMapping>>>,
    re_export_cache: Mutex<HashMap<ImportCacheKey, Vec<ReExport>>>,
}

impl ResolverSnapshot {
    pub(super) fn build(project_root: &str, queries: &QueryBuilder) -> Result<Self> {
        let nodes = queries.get_all_nodes()?;
        let all_files = queries.get_all_file_paths()?;
        let mut nodes_by_id = HashMap::with_capacity(nodes.len());
        let mut nodes_by_file: HashMap<String, Vec<Node>> = HashMap::new();
        let mut nodes_by_name: HashMap<String, Vec<Node>> = HashMap::new();
        let mut nodes_by_qualified_name: HashMap<String, Vec<Node>> = HashMap::new();
        let mut nodes_by_kind: HashMap<NodeKind, Vec<Node>> = HashMap::new();
        let mut nodes_by_lower_name: HashMap<String, Vec<Node>> = HashMap::new();
        let mut known_names = HashSet::new();

        for node in nodes {
            known_names.insert(node.name.clone());
            nodes_by_file
                .entry(node.file_path.clone())
                .or_default()
                .push(node.clone());
            nodes_by_name
                .entry(node.name.clone())
                .or_default()
                .push(node.clone());
            nodes_by_qualified_name
                .entry(node.qualified_name.clone())
                .or_default()
                .push(node.clone());
            nodes_by_kind
                .entry(node.kind)
                .or_default()
                .push(node.clone());
            nodes_by_lower_name
                .entry(node.name.to_lowercase())
                .or_default()
                .push(node.clone());
            nodes_by_id.insert(node.id.clone(), node);
        }

        let known_files: HashSet<String> = all_files.iter().cloned().collect();
        let context = SnapshotContext {
            project_root: project_root.to_string(),
            nodes_by_id,
            nodes_by_file,
            nodes_by_name,
            nodes_by_qualified_name,
            nodes_by_kind,
            nodes_by_lower_name,
            all_files,
            known_files,
            known_names,
            project_aliases: load_project_aliases(project_root),
            go_module: load_go_module(project_root),
            workspace_packages: load_workspace_packages(project_root),
            cpp_include_dirs: load_cpp_include_dirs(project_root),
            file_cache: Mutex::new(HashMap::new()),
            import_mapping_cache: Mutex::new(HashMap::new()),
            re_export_cache: Mutex::new(HashMap::new()),
        };

        Ok(Self { context })
    }

    pub(super) fn context(&self) -> &SnapshotContext {
        &self.context
    }
}

impl SnapshotContext {
    pub(super) fn get_node_by_id(&self, node_id: &str) -> Option<&Node> {
        self.nodes_by_id.get(node_id)
    }

    fn known_has(&self, name: &str) -> bool {
        self.known_names.contains(name)
    }
}
