use super::ParseError;
use crate::graph::CodeGraph;
use crate::nodes::{NodeId, NodeKind};

mod aggregate;
mod dominators;
mod entrypoints;
mod expr;
mod multi_path;
mod path_query;
mod path_search;
mod pipe;
mod pipe_basic;
mod pipe_graph;
mod pipe_insights;
mod pipe_metadata;
mod seeded;
mod set;
mod trait_impls;

pub use expr::run_query_expr;
pub use pipe::run_query;

/// Configuration for query execution.
#[derive(Debug, Clone)]
pub struct QueryConfig {
    pub max_tokens: usize,
    pub max_nodes: usize,
}

impl Default for QueryConfig {
    fn default() -> Self {
        Self {
            max_tokens: 4000,
            max_nodes: 50,
        }
    }
}

/// Result of a query execution.
///
/// Serializable for stable JSON exports - see [`crate::schema`] for the
/// versioned envelope and JSON Schema definitions consumers can depend on.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct QueryResult {
    /// Nodes in the result set.
    pub nodes: Vec<NodeId>,
    /// Edges between result nodes: (from, to, edge_kind_description).
    pub edges: Vec<(NodeId, NodeId, String)>,
    /// Whether result was truncated.
    pub was_truncated: bool,
    /// Total nodes before truncation.
    pub total_before_truncation: usize,
    /// Cycles detected during traversal.
    pub cycles_detected: Vec<NodeId>,
    /// Free-form lines describing higher-order structure that doesn't fit
    /// in `nodes`/`edges`: SCC clusters with member lists, type clusters
    /// with their primary type, entrypoint kinds and reach metrics, etc.
    pub metadata: Vec<String>,
}

impl QueryResult {
    /// Render each result node as a structured `kind:qualified_name` handle string.
    pub fn handles(&self, graph: &CodeGraph) -> Vec<String> {
        self.nodes
            .iter()
            .filter_map(|id| {
                let node = graph.get_node(id)?;
                let prefix = match node.kind {
                    NodeKind::Function => "fn",
                    NodeKind::Struct => "struct",
                    NodeKind::Enum => "enum",
                    NodeKind::Trait => "trait",
                    NodeKind::Module => "mod",
                    NodeKind::EnumVariant => "variant",
                    NodeKind::Field => "field",
                    NodeKind::TypeAlias => "type",
                    NodeKind::Constant => "const",
                    NodeKind::Interface => "interface",
                };
                Some(format!("{}:{}", prefix, node.qualified_name))
            })
            .collect()
    }
}

/// Errors from query execution.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    #[error("parse error: {0}")]
    Parse(#[from] ParseError),
    #[error("execution error: {0}")]
    Execution(String),
}

/// Query engine - executes DSL operations against a CodeGraph.
pub struct QueryEngine<'a> {
    pub(super) graph: &'a CodeGraph,
}

impl<'a> QueryEngine<'a> {
    pub fn new(graph: &'a CodeGraph) -> Self {
        Self { graph }
    }
}
