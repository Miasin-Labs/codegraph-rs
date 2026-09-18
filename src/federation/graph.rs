//! Which graph an external edge points into, and that graph opened.

use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use crate::db::{ExternalGraphKind, QueryBuilder};
use crate::deps::{DepKey, DepsHome, Ecosystem};
use crate::graph::GraphTraverser;
use crate::types::Node;

/// One graph by its external-edge identity: a dependency shard (key
/// `crates/<name>-<version>`) or a linked project (key = its canonical
/// root).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct GraphId {
    pub kind: ExternalGraphKind,
    pub key: String,
}

impl GraphId {
    pub fn new(kind: ExternalGraphKind, key: impl Into<String>) -> Self {
        Self {
            kind,
            key: key.into(),
        }
    }

    /// A project's own index, by its canonical root.
    pub fn project(root: &Path) -> Self {
        Self::new(
            ExternalGraphKind::Project,
            crate::atlas::canonical_root(root).to_string_lossy(),
        )
    }

    /// The shard directory of a dependency key, when the key is one this
    /// store could have written (`<ecosystem>/<one directory>`).
    pub(crate) fn shard_dir(&self, home: &DepsHome) -> Option<PathBuf> {
        if self.kind != ExternalGraphKind::Dependency {
            return None;
        }
        let (ecosystem, dir) = self.key.split_once('/')?;
        ecosystem.parse::<Ecosystem>().ok()?;
        let mut components = Path::new(dir).components();
        let single =
            matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none();
        single.then(|| home.root().join(&self.key))
    }
}

/// `serde_json@1.0.150` for a shard key `crates/serde_json-1.0.150` read
/// without its `meta.json`: the version starts at the first `-` followed by
/// a digit (crate names rarely contain one; the shard's own metadata is
/// preferred whenever the shard opens).
pub fn dependency_label(key: &str) -> String {
    let dir = key.rsplit('/').next().unwrap_or(key);
    let bytes = dir.as_bytes();
    let split = (1..bytes.len()).find(|&i| bytes[i - 1] == b'-' && bytes[i].is_ascii_digit());
    match split {
        Some(i) => format!("{}@{}", &dir[..i - 1], &dir[i..]),
        None => dir.to_string(),
    }
}

/// A graph, open read-only.
pub struct OpenGraph {
    pub id: GraphId,
    /// How answers name it: `serde_json@1.0.150`, `linkscope`.
    pub label: String,
    /// What node file paths are relative to (a shard's source directory, a
    /// project's root).
    pub root: PathBuf,
    /// The dependency version, for a shard.
    pub dependency: Option<DepKey>,
    /// The index records external edges (schema ≥ 10) — a project graph
    /// that can say who *it* calls in other graphs.
    pub has_external_edges: bool,
    queries: Rc<QueryBuilder>,
}

impl std::fmt::Debug for OpenGraph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenGraph")
            .field("id", &self.id)
            .field("label", &self.label)
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl OpenGraph {
    pub(crate) fn new(
        id: GraphId,
        label: String,
        root: PathBuf,
        dependency: Option<DepKey>,
        has_external_edges: bool,
        queries: QueryBuilder,
    ) -> Self {
        Self {
            id,
            label,
            root,
            dependency,
            has_external_edges,
            queries: Rc::new(queries),
        }
    }

    /// The graph's read-only query surface.
    pub fn queries(&self) -> &QueryBuilder {
        &self.queries
    }

    /// Callers, callees and impact inside this graph.
    pub fn traverser(&self) -> GraphTraverser {
        GraphTraverser::new(Rc::clone(&self.queries))
    }

    /// A node of this graph by id.
    pub fn node(&self, id: &str) -> Option<Node> {
        self.queries.get_node_by_id(id).ok().flatten()
    }

    /// The absolute path of a file of this graph.
    pub fn path_of(&self, file: &str) -> PathBuf {
        self.root.join(file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_split_name_and_version() {
        assert_eq!(
            dependency_label("crates/serde_json-1.0.150"),
            "serde_json@1.0.150"
        );
        assert_eq!(
            dependency_label("crates/tree-sitter-rust-0.24.0"),
            "tree-sitter-rust@0.24.0"
        );
        assert_eq!(
            dependency_label("crates/linkscope-0.1.0+git.3c76713b531a"),
            "linkscope@0.1.0+git.3c76713b531a"
        );
        assert_eq!(
            dependency_label("crates/x509-parser-0.15.1"),
            "x509-parser@0.15.1"
        );
    }

    #[test]
    fn shard_keys_stay_inside_the_store() {
        let home = DepsHome::at("/h/deps");
        let dir = |key: &str| GraphId::new(ExternalGraphKind::Dependency, key).shard_dir(&home);
        assert_eq!(
            dir("crates/serde-1.0.0"),
            Some(PathBuf::from("/h/deps/crates/serde-1.0.0"))
        );
        for bad in [
            "crates/../x",
            "crates/a/b",
            "nope/serde-1",
            "crates/",
            "/etc/passwd",
        ] {
            assert_eq!(dir(bad), None, "{bad}");
        }
    }
}
