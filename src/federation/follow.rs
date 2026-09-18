//! Following one external edge into its graph.
//!
//! The edge names its target by `target_node_id` — a hash of file, kind,
//! name and line, so stable while the item does not move. When the target
//! graph was rebuilt (a shard for a newer extractor, a re-indexed linked
//! project) and the id is gone, the target is found again by what the
//! edge also recorded: qualified name + kind + file (the nearest to the
//! recorded line among same-named overloads), else the one item of that
//! qualified name and kind anywhere in the graph.

use std::rc::Rc;

use super::graph::{GraphId, OpenGraph};
use super::set::GraphSet;
use crate::db::ExternalEdge;
use crate::types::{Node, NodeKind};

/// Why a graph could not answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Unavailable {
    /// No shard / no index there (never built, removed, garbage-collected).
    Missing,
    /// There, but not readable by this build (another schema, corrupt).
    Unreadable,
    /// The request's budget ran out first.
    Deadline,
}

impl Unavailable {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Unreadable => "unreadable",
            Self::Deadline => "deadline",
        }
    }

    /// The phrase answers use: "target not available (…)".
    pub fn describe(self, kind: crate::db::ExternalGraphKind) -> &'static str {
        match (self, kind) {
            (Self::Missing, crate::db::ExternalGraphKind::Dependency) => "shard not built",
            (Self::Missing, crate::db::ExternalGraphKind::Project) => "project index missing",
            (Self::Unreadable, _) => "graph unreadable",
            (Self::Deadline, _) => "out of time",
        }
    }
}

/// Where an edge's target is now.
#[derive(Debug, Clone)]
pub enum Target {
    /// At the id the edge recorded.
    Found(Node),
    /// Found again by qualified name + kind + file after the id moved.
    Moved(Node),
    /// The graph opened, but no longer holds the item.
    Gone,
    Unavailable(Unavailable),
    /// Not looked up (following is off): the place the edge recorded.
    Recorded,
}

impl Target {
    pub fn node(&self) -> Option<&Node> {
        match self {
            Self::Found(node) | Self::Moved(node) => Some(node),
            Self::Gone | Self::Unavailable(_) | Self::Recorded => None,
        }
    }
}

/// One external edge, followed.
#[derive(Debug, Clone)]
pub struct Followed {
    pub edge: ExternalEdge,
    /// How answers name the target graph.
    pub label: String,
    pub graph: Option<Rc<OpenGraph>>,
    pub target: Target,
}

impl Followed {
    /// `edge` as recorded, without opening its graph (following is off).
    pub fn recorded(edge: &ExternalEdge) -> Self {
        let label = match edge.target_graph_kind {
            crate::db::ExternalGraphKind::Dependency => {
                super::graph::dependency_label(&edge.target_graph_key)
            }
            crate::db::ExternalGraphKind::Project => std::path::Path::new(&edge.target_graph_key)
                .file_name()
                .map_or_else(
                    || edge.target_graph_key.clone(),
                    |name| name.to_string_lossy().into_owned(),
                ),
        };
        Self {
            edge: edge.clone(),
            label,
            graph: None,
            target: Target::Recorded,
        }
    }

    /// The target's current line (the recorded one when the graph is not
    /// available).
    pub fn line(&self) -> Option<u32> {
        self.target
            .node()
            .map(|node| node.start_line)
            .or(self.edge.target_line)
    }

    /// The target's file, relative to its graph's root.
    pub fn file(&self) -> &str {
        self.target
            .node()
            .map_or(self.edge.target_file_path.as_str(), |node| {
                node.file_path.as_str()
            })
    }
}

impl GraphSet {
    /// Follow `edge` into its graph.
    pub fn follow(&self, edge: &ExternalEdge) -> Followed {
        let id = GraphId::new(edge.target_graph_kind, edge.target_graph_key.clone());
        match self.open(&id) {
            Ok(graph) => {
                let target = find_target(
                    &graph,
                    &edge.target_node_id,
                    &edge.target_qualified_name,
                    edge.target_kind,
                    &edge.target_file_path,
                    edge.target_line,
                );
                Followed {
                    edge: edge.clone(),
                    label: graph.label.clone(),
                    graph: Some(graph),
                    target,
                }
            }
            Err(reason) => Followed {
                edge: edge.clone(),
                label: self.label_of(&id),
                graph: None,
                target: Target::Unavailable(reason),
            },
        }
    }

    /// Follow each edge, in order. Once the budget is spent, a graph not
    /// already open reports its edges unavailable (out of time).
    pub fn follow_all(&self, edges: &[ExternalEdge]) -> Vec<Followed> {
        edges.iter().map(|edge| self.follow(edge)).collect()
    }
}

/// The node an edge recorded, in `graph`.
pub(crate) fn find_target(
    graph: &OpenGraph,
    id: &str,
    qualified_name: &str,
    kind: NodeKind,
    file: &str,
    line: Option<u32>,
) -> Target {
    let queries = graph.queries();
    match queries.get_node_by_id(id) {
        Ok(Some(node)) if node.kind == kind => return Target::Found(node),
        Ok(_) => {}
        Err(error) if super::deadline::is_interrupt(&error) => {
            return Target::Unavailable(super::Unavailable::Deadline);
        }
        Err(_) => return Target::Unavailable(super::Unavailable::Unreadable),
    }
    let Ok(named) = queries.get_nodes_by_qualified_name_exact(qualified_name) else {
        return Target::Unavailable(super::Unavailable::Unreadable);
    };
    let of_kind: Vec<Node> = named.into_iter().filter(|node| node.kind == kind).collect();
    let in_file = of_kind
        .iter()
        .filter(|node| node.file_path == file)
        .min_by_key(|node| line.map_or(0, |line| node.start_line.abs_diff(line)));
    if let Some(node) = in_file {
        return Target::Moved(node.clone());
    }
    match of_kind.as_slice() {
        [only] => Target::Moved(only.clone()),
        _ => Target::Gone,
    }
}
