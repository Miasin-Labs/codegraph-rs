//! A symbol a question names inside another graph.
//!
//! `serde_json::from_str` asked from a project that depends on
//! `serde_json` is looked up the way phase 2 resolves that path in code:
//! from the crate's library root through its module layout and `pub use`
//! re-exports (another crate's too, ≤2 hops), else the one item of that
//! shape ([`crate::resolution::external`]'s path lookup). With a graph hint
//! (`graph: "serde_json"`, `"serde_json@1.0.150"`, a linked project's
//! name) a bare name is looked up by name in that graph. Only graphs the
//! project reaches are asked — its dependency versions with a built shard
//! and the projects it links to by a Cargo path dependency.

use std::path::Path;
use std::rc::Rc;

use super::graph::{GraphId, OpenGraph, dependency_label};
use super::set::GraphSet;
use crate::resolution::external::open::GraphCache;
use crate::resolution::external::rust::lookup::lookup_path;
use crate::resolution::external::{GraphLocation, Reach, ReachableGraph};
use crate::types::{EdgeKind, Node, NodeKind};

/// Path segments that never name another crate.
const LOCAL_ROOTS: &[&str] = &["crate", "self", "super", "Self", "std", "core", "alloc"];

/// A definition in another graph.
#[derive(Debug, Clone)]
pub struct ForeignSymbol {
    pub graph: Rc<OpenGraph>,
    pub node: Node,
}

impl GraphSet {
    /// Definitions `symbol` names in the graphs `project_root` reaches (see
    /// the module docs), at most `limit`. Empty when it names none.
    pub fn resolve_symbol(
        &self,
        project_root: &Path,
        symbol: &str,
        graph: Option<&str>,
        limit: usize,
    ) -> Vec<ForeignSymbol> {
        let segments: Vec<&str> = symbol
            .split("::")
            .map(str::trim)
            .filter(|segment| !segment.is_empty())
            .collect();
        if segments.is_empty() || self.deadline().expired() {
            return Vec::new();
        }
        let reach = self.reach(project_root);
        let (index, rest) = match graph {
            Some(hint) => {
                let Some(index) = self.find_graph(&reach, hint) else {
                    return Vec::new();
                };
                let krate = reach.graph(index).krate.as_str();
                let rest = if segments.len() > 1 && segments[0] == krate {
                    &segments[1..]
                } else {
                    &segments[..]
                };
                (index, rest)
            }
            None => {
                if segments.len() < 2 || LOCAL_ROOTS.contains(&segments[0]) {
                    return Vec::new();
                }
                let Some(index) = reach.index_of(segments[0]) else {
                    return Vec::new();
                };
                (index, &segments[1..])
            }
        };
        let rest: Vec<String> = rest.iter().map(|segment| segment.to_string()).collect();
        if let Some(found) = self.by_path(&reach, index, &rest) {
            return vec![found];
        }
        self.by_name(reach.graph(index), &rest, limit)
    }

    /// The reachable graph a hint names.
    fn find_graph(&self, reach: &Reach, hint: &str) -> Option<usize> {
        let hint = hint.trim();
        let normalized = hint.replace('-', "_");
        let graphs = reach.graphs();
        let matches = |graph: &ReachableGraph| {
            graph.krate == normalized
                || graph.key == hint
                || match graph.kind {
                    crate::db::ExternalGraphKind::Dependency => {
                        let label = dependency_label(&graph.key);
                        label == hint
                            || label.split_once('@').is_some_and(|(name, version)| {
                                format!("{}@{version}", name.replace('-', "_")) == normalized
                                    || name == hint
                            })
                    }
                    crate::db::ExternalGraphKind::Project => {
                        self.project_name(Path::new(&graph.key)) == hint
                    }
                }
        };
        graphs.iter().position(matches)
    }

    /// Phase 2's path lookup: `rest` from the crate's library root.
    fn by_path(&self, reach: &Reach, index: usize, rest: &[String]) -> Option<ForeignSymbol> {
        let cache = GraphCache::new(reach, 4);
        let krate = reach.graph(index).krate.clone();
        for kind in [EdgeKind::Calls, EdgeKind::References] {
            let Some(found) = lookup_path(&cache, &krate, rest, kind) else {
                continue;
            };
            let graph = self
                .open(&GraphId::new(found.graph.kind, found.graph.key.clone()))
                .ok()?;
            return Some(ForeignSymbol {
                graph,
                node: found.node,
            });
        }
        None
    }

    /// Definitions named like `rest`'s last segment (and qualified like
    /// all of it) in the crate's graph.
    fn by_name(
        &self,
        reachable: &ReachableGraph,
        rest: &[String],
        limit: usize,
    ) -> Vec<ForeignSymbol> {
        let Some(name) = rest.last() else {
            return Vec::new();
        };
        let Ok(graph) = self.open(&GraphId::new(reachable.kind, reachable.key.clone())) else {
            return Vec::new();
        };
        let crate_dir = match &reachable.location {
            GraphLocation::Project { crate_dir, .. } => crate_dir.as_str(),
            GraphLocation::Shard { .. } => "",
        };
        let suffix = rest.join("::");
        let Ok(nodes) = graph.queries().get_nodes_by_name(name) else {
            return Vec::new();
        };
        let mut found: Vec<Node> = nodes
            .into_iter()
            .filter(|node| {
                !matches!(
                    node.kind,
                    NodeKind::Import | NodeKind::Export | NodeKind::File | NodeKind::Parameter
                )
            })
            .filter(|node| {
                crate_dir.is_empty()
                    || node
                        .file_path
                        .strip_prefix(crate_dir)
                        .is_some_and(|tail| tail.starts_with('/'))
            })
            .filter(|node| {
                rest.len() < 2
                    || node.qualified_name == suffix
                    || node.qualified_name.ends_with(&format!("::{suffix}"))
            })
            .collect();
        found.sort_by(|a, b| {
            let public = |node: &Node| node.visibility != Some(crate::types::Visibility::Public);
            public(a)
                .cmp(&public(b))
                .then_with(|| a.file_path.cmp(&b.file_path))
                .then(a.start_line.cmp(&b.start_line))
        });
        found.truncate(limit);
        found
            .into_iter()
            .map(|node| ForeignSymbol {
                graph: Rc::clone(&graph),
                node,
            })
            .collect()
    }
}
