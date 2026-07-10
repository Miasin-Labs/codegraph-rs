mod context;
mod policy;

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use crate::db::QueryBuilder;
use crate::error::Result;
use crate::resolution::go_module::load_go_module;
use crate::resolution::import_resolver::load_cpp_include_dirs;
use crate::resolution::path_aliases::load_project_aliases;
use crate::resolution::types::{AliasMap, GoModule, ImportMapping, ReExport, WorkspacePackages};
use crate::resolution::workspace_packages::load_workspace_packages;
use crate::types::{Language, Node, NodeKind};

pub(super) struct ResolverSnapshot {
    context: Arc<SnapshotContext>,
}

type ImportCacheKey = (String, Language);
type NodeIndex = u32;
type LookupIndex = HashMap<u64, IndexBucket>;

/// Most lookup keys identify a single node. Keeping that index inline avoids a
/// heap allocation per symbol while still supporting overloaded names and the
/// (rare) hash collision.
enum IndexBucket {
    One(NodeIndex),
    Many(Vec<NodeIndex>),
}

impl IndexBucket {
    fn push(&mut self, index: NodeIndex) {
        match self {
            Self::One(previous) => {
                *self = Self::Many(vec![*previous, index]);
            }
            Self::Many(indices) => indices.push(index),
        }
    }

    fn as_slice(&self) -> &[NodeIndex] {
        match self {
            Self::One(index) => std::slice::from_ref(index),
            Self::Many(indices) => indices,
        }
    }
}

fn fingerprint(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn add_lookup(index: &mut LookupIndex, key: &str, node_index: NodeIndex) {
    match index.entry(fingerprint(key)) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(IndexBucket::One(node_index));
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            entry.get_mut().push(node_index);
        }
    }
}

pub(super) struct SnapshotContext {
    project_root: String,
    nodes: Vec<Node>,
    nodes_by_id: LookupIndex,
    nodes_by_file: LookupIndex,
    nodes_by_name: LookupIndex,
    nodes_by_qualified_name: LookupIndex,
    nodes_by_kind: HashMap<NodeKind, IndexBucket>,
    nodes_by_lower_name: LookupIndex,
    all_files: Vec<String>,
    known_files: LookupIndex,
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
        let context = SnapshotContext::from_nodes(project_root, nodes, all_files)?;

        Ok(Self {
            context: Arc::new(context),
        })
    }

    pub(super) fn context(&self) -> &SnapshotContext {
        &self.context
    }

    pub(super) fn shared_context(&self) -> Arc<SnapshotContext> {
        Arc::clone(&self.context)
    }
}

impl SnapshotContext {
    pub(super) fn from_nodes(
        project_root: &str,
        nodes: Vec<Node>,
        all_files: Vec<String>,
    ) -> Result<Self> {
        let mut nodes_by_id = HashMap::with_capacity(nodes.len());
        let mut nodes_by_file = HashMap::new();
        let mut nodes_by_name = HashMap::new();
        let mut nodes_by_qualified_name = HashMap::new();
        let mut nodes_by_kind = HashMap::new();
        let mut nodes_by_lower_name = HashMap::new();

        for (offset, node) in nodes.iter().enumerate() {
            let index = NodeIndex::try_from(offset).map_err(|_| {
                crate::error::CodeGraphError::other(
                    "resolver snapshot exceeds the supported node count",
                )
            })?;
            add_lookup(&mut nodes_by_id, &node.id, index);
            add_lookup(&mut nodes_by_file, &node.file_path, index);
            add_lookup(&mut nodes_by_name, &node.name, index);
            add_lookup(&mut nodes_by_qualified_name, &node.qualified_name, index);
            match nodes_by_kind.entry(node.kind) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(IndexBucket::One(index));
                }
                std::collections::hash_map::Entry::Occupied(mut entry) => {
                    entry.get_mut().push(index);
                }
            }
            add_lookup(&mut nodes_by_lower_name, &node.name.to_lowercase(), index);
        }

        let mut known_files = HashMap::with_capacity(all_files.len());
        for (offset, file_path) in all_files.iter().enumerate() {
            let index = NodeIndex::try_from(offset).map_err(|_| {
                crate::error::CodeGraphError::other(
                    "resolver snapshot exceeds the supported file count",
                )
            })?;
            add_lookup(&mut known_files, file_path, index);
        }

        Ok(Self {
            project_root: project_root.to_string(),
            nodes,
            nodes_by_id,
            nodes_by_file,
            nodes_by_name,
            nodes_by_qualified_name,
            nodes_by_kind,
            nodes_by_lower_name,
            all_files,
            known_files,
            project_aliases: load_project_aliases(project_root),
            go_module: load_go_module(project_root),
            workspace_packages: load_workspace_packages(project_root),
            cpp_include_dirs: load_cpp_include_dirs(project_root),
            file_cache: Mutex::new(HashMap::new()),
            import_mapping_cache: Mutex::new(HashMap::new()),
            re_export_cache: Mutex::new(HashMap::new()),
        })
    }

    pub(super) fn get_node_by_id(&self, node_id: &str) -> Option<&Node> {
        self.nodes_by_id
            .get(&fingerprint(node_id))
            .into_iter()
            .flat_map(IndexBucket::as_slice)
            .filter_map(|index| self.nodes.get(*index as usize))
            .find(|node| node.id == node_id)
    }

    fn known_has(&self, name: &str) -> bool {
        self.lookup_node_refs(&self.nodes_by_name, name, |node, key| node.name == key)
            .next()
            .is_some()
    }

    fn lookup_node_refs<'a, F>(
        &'a self,
        index: &'a LookupIndex,
        key: &'a str,
        matches: F,
    ) -> impl Iterator<Item = &'a Node> + 'a
    where
        F: Fn(&Node, &str) -> bool + 'a,
    {
        index
            .get(&fingerprint(key))
            .into_iter()
            .flat_map(IndexBucket::as_slice)
            .filter_map(|index| self.nodes.get(*index as usize))
            .filter(move |node| matches(node, key))
    }

    fn lookup_nodes<F>(&self, index: &LookupIndex, key: &str, matches: F) -> Vec<Node>
    where
        F: Fn(&Node, &str) -> bool,
    {
        self.lookup_node_refs(index, key, matches)
            .cloned()
            .collect()
    }

    fn known_file(&self, file_path: &str) -> bool {
        self.known_files
            .get(&fingerprint(file_path))
            .into_iter()
            .flat_map(IndexBucket::as_slice)
            .filter_map(|index| self.all_files.get(*index as usize))
            .any(|candidate| candidate == file_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolution::types::ResolutionContext;

    fn node(id: &str, name: &str, qualified_name: &str, file_path: &str) -> Node {
        Node::new(
            id,
            NodeKind::Function,
            name,
            qualified_name,
            file_path,
            Language::Rust,
            1,
            2,
        )
    }

    #[test]
    fn compact_indexes_share_one_node_arena() {
        let context = SnapshotContext::from_nodes(
            "/missing",
            vec![
                node("n1", "run", "a::run", "src/a.rs"),
                node("n2", "run", "b::run", "src/b.rs"),
            ],
            vec!["src/a.rs".into(), "src/b.rs".into()],
        )
        .unwrap();

        assert_eq!(context.nodes.len(), 2);
        assert!(std::ptr::eq(
            context.get_node_by_id("n1").unwrap(),
            &context.nodes[0]
        ));
        assert_eq!(context.get_nodes_by_name("run"), context.nodes);
        assert!(context.file_exists("src/a.rs"));
        assert!(!context.file_exists("src/missing.rs"));

        // The old layout retained six complete `Node` values per symbol.
        // The compact layout retains one Node plus six four-byte indices.
        assert!(std::mem::size_of::<Node>() > 6 * std::mem::size_of::<NodeIndex>());
    }

    #[test]
    fn compact_indexes_keep_exact_lookup_semantics() {
        let context = SnapshotContext::from_nodes(
            "/missing",
            vec![
                node("n1", "Run", "a::Run", "src/a.rs"),
                node("n2", "run", "b::run", "src/a.rs"),
            ],
            vec!["src/a.rs".into()],
        )
        .unwrap();

        assert_eq!(context.get_nodes_by_name("Run").len(), 1);
        assert_eq!(context.get_nodes_by_qualified_name("b::run").len(), 1);
        assert_eq!(context.get_nodes_by_lower_name("run").len(), 2);
        assert_eq!(context.get_nodes_in_file("src/a.rs").len(), 2);
        assert!(context.get_node_by_id("unknown").is_none());
    }
}
