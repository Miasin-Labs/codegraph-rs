//! Graph Traversal Algorithms
//!
//! BFS and DFS traversal for the code knowledge graph.
//!
//! Ported from `src/graph/traversal.ts`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

use crate::db::queries::QueryBuilder;
use crate::error::Result;
use crate::types::{
    Direction,
    Edge,
    EdgeKind,
    Node,
    NodeKind,
    NodeRef,
    Subgraph,
    TraversalOptions,
};

/// Default `limit` for traversal (mirrors `DEFAULT_OPTIONS.limit` in TS).
const DEFAULT_LIMIT: usize = 1000;

/// Edge kinds followed by [`GraphTraverser::get_callers`] /
/// [`GraphTraverser::get_callees`] (mirrors the inline list in TS).
const RELATION_EDGE_KINDS: [EdgeKind; 3] =
    [EdgeKind::Calls, EdgeKind::References, EdgeKind::Imports];

/// Default traversal options, resolved from the optional fields of
/// [`TraversalOptions`] (mirrors `DEFAULT_OPTIONS` + the `{...}` spread in TS).
///
/// `max_depth: None` corresponds to the TS default of `Infinity`.
struct ResolvedOptions {
    max_depth: Option<u32>,
    edge_kinds: Vec<EdgeKind>,
    node_kinds: Vec<NodeKind>,
    direction: Direction,
    limit: usize,
    include_start: bool,
}

impl ResolvedOptions {
    fn resolve(options: &TraversalOptions) -> Self {
        ResolvedOptions {
            max_depth: options.max_depth,
            edge_kinds: options.edge_kinds.clone().unwrap_or_default(),
            node_kinds: options.node_kinds.clone().unwrap_or_default(),
            direction: options.direction.unwrap_or(Direction::Outgoing),
            limit: options.limit.unwrap_or(DEFAULT_LIMIT),
            include_start: options.include_start.unwrap_or(true),
        }
    }

    /// `depth >= opts.maxDepth` with `Infinity` semantics for `None`.
    fn depth_reached(&self, depth: u32) -> bool {
        self.max_depth.is_some_and(|m| depth >= m)
    }
}

/// Result of a single traversal step.
struct TraversalStep {
    node: Node,
    edge: Option<Edge>,
    depth: u32,
}

/// One step of a path returned by [`GraphTraverser::find_path`].
///
/// The first step's `edge` is `None` (TS: `edge: null`). Serialized with the
/// explicit `null` to stay wire-compatible with the TS shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PathStep {
    pub node: Node,
    pub edge: Option<Edge>,
}

/// Graph traverser for BFS and DFS traversal.
pub struct GraphTraverser {
    queries: Rc<QueryBuilder>,
}

impl GraphTraverser {
    pub fn new(queries: Rc<QueryBuilder>) -> Self {
        GraphTraverser { queries }
    }

    /// Traverse the graph using breadth-first search.
    ///
    /// * `start_id` - Starting node ID
    /// * `options` - Traversal options
    ///
    /// Returns a subgraph containing traversed nodes and edges.
    pub fn traverse_bfs(&self, start_id: &str, options: &TraversalOptions) -> Result<Subgraph> {
        let opts = ResolvedOptions::resolve(options);
        let start_node = match self.queries.get_node_by_id(start_id)? {
            Some(node) => node,
            None => return Ok(Subgraph::default()),
        };

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut queue: VecDeque<TraversalStep> = VecDeque::new();
        queue.push_back(TraversalStep {
            node: start_node.clone(),
            edge: None,
            depth: 0,
        });

        if opts.include_start {
            nodes.insert(start_node.id.clone(), start_node);
        }

        while !queue.is_empty() && nodes.len() < opts.limit {
            crate::graph::cancel::check()?;
            let TraversalStep { node, edge, depth } = queue.pop_front().expect("non-empty queue");

            if visited.contains(&node.id) {
                continue;
            }
            visited.insert(node.id.clone());

            // Add edge to result
            if let Some(edge) = edge {
                edges.push(edge);
            }

            // Check depth limit
            if opts.depth_reached(depth) {
                continue;
            }

            // Get adjacent edges, prioritizing structural edges (contains, calls)
            // over reference edges so BFS discovers internal structure before
            // fanning out to external references (e.g., component usages in templates).
            let mut adjacent_edges =
                self.get_adjacent_edges(&node.id, opts.direction, &opts.edge_kinds)?;
            adjacent_edges.sort_by_key(|e| match e.kind {
                EdgeKind::Contains => 0,
                EdgeKind::Calls => 1,
                _ => 2,
            });

            // Batch-fetch the unvisited neighbors in one query (was N+1 per BFS step).
            let want_ids: Vec<String> = adjacent_edges
                .iter()
                .map(|e| other_endpoint(e, &node.id).to_string())
                .filter(|id| !visited.contains(id))
                .collect();
            let neighbor_nodes = if !want_ids.is_empty() {
                self.queries.get_nodes_by_ids(&want_ids)?
            } else {
                HashMap::new()
            };

            for adj_edge in adjacent_edges {
                let next_node_id = other_endpoint(&adj_edge, &node.id);
                if visited.contains(next_node_id) {
                    continue;
                }

                let Some(next_node) = neighbor_nodes.get(next_node_id) else {
                    continue;
                };

                if !opts.node_kinds.is_empty() && !opts.node_kinds.contains(&next_node.kind) {
                    continue;
                }

                nodes.insert(next_node.id.clone(), next_node.clone());
                queue.push_back(TraversalStep {
                    node: next_node.clone(),
                    edge: Some(adj_edge),
                    depth: depth + 1,
                });
            }
        }

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![start_id.to_string()],
            confidence: None,
        })
    }

    /// Traverse the graph using depth-first search.
    ///
    /// * `start_id` - Starting node ID
    /// * `options` - Traversal options
    ///
    /// Returns a subgraph containing traversed nodes and edges.
    pub fn traverse_dfs(&self, start_id: &str, options: &TraversalOptions) -> Result<Subgraph> {
        let opts = ResolvedOptions::resolve(options);
        let start_node = match self.queries.get_node_by_id(start_id)? {
            Some(node) => node,
            None => return Ok(Subgraph::default()),
        };

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        if opts.include_start {
            nodes.insert(start_node.id.clone(), start_node.clone());
        }

        self.dfs_recursive(&start_node, 0, &opts, &mut nodes, &mut edges, &mut visited)?;

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![start_id.to_string()],
            confidence: None,
        })
    }

    /// Recursive DFS helper.
    fn dfs_recursive(
        &self,
        node: &Node,
        depth: u32,
        opts: &ResolvedOptions,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<()> {
        crate::graph::cancel::check()?;
        if visited.contains(&node.id) || nodes.len() >= opts.limit || opts.depth_reached(depth) {
            return Ok(());
        }

        visited.insert(node.id.clone());

        // Get adjacent edges
        let adjacent_edges = self.get_adjacent_edges(&node.id, opts.direction, &opts.edge_kinds)?;

        // Batch-fetch unvisited neighbors (was N+1 per DFS step).
        let want_ids: Vec<String> = adjacent_edges
            .iter()
            .map(|e| other_endpoint(e, &node.id).to_string())
            .filter(|id| !visited.contains(id))
            .collect();
        let neighbor_nodes = if !want_ids.is_empty() {
            self.queries.get_nodes_by_ids(&want_ids)?
        } else {
            HashMap::new()
        };

        for edge in adjacent_edges {
            let next_node_id = other_endpoint(&edge, &node.id);
            if visited.contains(next_node_id) {
                continue;
            }

            let Some(next_node) = neighbor_nodes.get(next_node_id) else {
                continue;
            };

            // Apply node kind filter
            if !opts.node_kinds.is_empty() && !opts.node_kinds.contains(&next_node.kind) {
                continue;
            }

            // Add node and edge to result
            nodes.insert(next_node.id.clone(), next_node.clone());
            edges.push(edge);

            // Recurse
            self.dfs_recursive(next_node, depth + 1, opts, nodes, edges, visited)?;
        }

        Ok(())
    }

    /// Get adjacent edges based on direction.
    fn get_adjacent_edges(
        &self,
        node_id: &str,
        direction: Direction,
        edge_kinds: &[EdgeKind],
    ) -> Result<Vec<Edge>> {
        let kinds = if edge_kinds.is_empty() {
            None
        } else {
            Some(edge_kinds)
        };

        match direction {
            Direction::Outgoing => self.queries.get_outgoing_edges(node_id, kinds, None),
            Direction::Incoming => self.queries.get_incoming_edges(node_id, kinds),
            Direction::Both => {
                // Both directions
                let mut edges = self.queries.get_outgoing_edges(node_id, kinds, None)?;
                edges.extend(self.queries.get_incoming_edges(node_id, kinds)?);
                Ok(edges)
            }
        }
    }

    /// Find all callers of a function/method.
    ///
    /// * `node_id` - ID of the function/method node
    /// * `max_depth` - Maximum depth to traverse (TS default: 1)
    ///
    /// Returns the nodes that call this function (with the connecting edge).
    pub fn get_callers(&self, node_id: &str, max_depth: u32) -> Result<Vec<NodeRef>> {
        self.traverse_relation_layers(
            node_id,
            max_depth,
            Direction::Incoming,
            &RELATION_EDGE_KINDS,
        )
    }

    /// Find all functions/methods called by a function.
    ///
    /// * `node_id` - ID of the function/method node
    /// * `max_depth` - Maximum depth to traverse (TS default: 1)
    ///
    /// Returns the nodes called by this function (with the connecting edge).
    pub fn get_callees(&self, node_id: &str, max_depth: u32) -> Result<Vec<NodeRef>> {
        self.traverse_relation_layers(
            node_id,
            max_depth,
            Direction::Outgoing,
            &RELATION_EDGE_KINDS,
        )
    }

    fn traverse_relation_layers(
        &self,
        start_id: &str,
        max_depth: u32,
        direction: Direction,
        edge_kinds: &[EdgeKind],
    ) -> Result<Vec<NodeRef>> {
        let mut result: Vec<NodeRef> = Vec::new();
        if max_depth == 0 {
            return Ok(result);
        }

        let incoming = matches!(direction, Direction::Incoming);
        let mut visited: HashSet<String> = HashSet::from([start_id.to_string()]);
        let mut frontier: Vec<String> = vec![start_id.to_string()];

        let mut depth = 0;
        while depth < max_depth && !frontier.is_empty() {
            crate::graph::cancel::check()?;
            let edges = if incoming {
                self.queries
                    .get_incoming_edges_for_targets(&frontier, Some(edge_kinds))?
            } else {
                self.queries
                    .get_outgoing_edges_for_sources(&frontier, Some(edge_kinds))?
            };
            if edges.is_empty() {
                break;
            }

            let mut next_ids_in_order: Vec<String> = Vec::new();
            let mut next_id_set: HashSet<&str> = HashSet::new();
            for edge in &edges {
                let next_id = if incoming { &edge.source } else { &edge.target };
                if visited.contains(next_id.as_str()) || next_id_set.contains(next_id.as_str()) {
                    continue;
                }
                next_id_set.insert(next_id);
                next_ids_in_order.push(next_id.clone());
            }
            if next_ids_in_order.is_empty() {
                break;
            }

            let next_nodes = self.queries.get_nodes_by_ids(&next_ids_in_order)?;
            let mut next_frontier: Vec<String> = Vec::new();
            let mut emitted_this_layer: HashSet<String> = HashSet::new();
            for edge in edges {
                let next_id = if incoming {
                    edge.source.clone()
                } else {
                    edge.target.clone()
                };
                if visited.contains(&next_id) || emitted_this_layer.contains(&next_id) {
                    continue;
                }

                let Some(node) = next_nodes.get(&next_id) else {
                    continue;
                };

                emitted_this_layer.insert(next_id.clone());
                visited.insert(next_id.clone());
                result.push(NodeRef {
                    node: node.clone(),
                    edge,
                });
                next_frontier.push(next_id);
            }

            frontier = next_frontier;
            depth += 1;
        }

        Ok(result)
    }

    /// Get the call graph for a function (both callers and callees).
    ///
    /// * `node_id` - ID of the function/method node
    /// * `depth` - Maximum depth in each direction (TS default: 2)
    ///
    /// Returns a subgraph containing the call graph.
    pub fn get_call_graph(&self, node_id: &str, depth: u32) -> Result<Subgraph> {
        let focal_node = match self.queries.get_node_by_id(node_id)? {
            Some(node) => node,
            None => return Ok(Subgraph::default()),
        };

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();

        // Add focal node
        nodes.insert(focal_node.id.clone(), focal_node);

        // Get callers
        let callers = self.get_callers(node_id, depth)?;
        for NodeRef { node, edge } in callers {
            nodes.insert(node.id.clone(), node);
            edges.push(edge);
        }

        // Get callees
        let callees = self.get_callees(node_id, depth)?;
        for NodeRef { node, edge } in callees {
            nodes.insert(node.id.clone(), node);
            edges.push(edge);
        }

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![node_id.to_string()],
            confidence: None,
        })
    }

    /// Get the type hierarchy for a class/interface.
    ///
    /// * `node_id` - ID of the class/interface node
    ///
    /// Returns a subgraph containing the type hierarchy.
    ///
    /// Faithful-port note: as in TS, the ancestors and descendants walks share
    /// one `visited` set and ancestors runs first, marking the focal node
    /// visited — so `get_type_descendants` returns immediately for the focal
    /// node and descendants are never traversed. Preserved verbatim (port
    /// faithfully, not creatively).
    pub fn get_type_hierarchy(&self, node_id: &str) -> Result<Subgraph> {
        let focal_node = match self.queries.get_node_by_id(node_id)? {
            Some(node) => node,
            None => return Ok(Subgraph::default()),
        };

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        // Add focal node
        nodes.insert(focal_node.id.clone(), focal_node);

        // Get ancestors (what this extends/implements)
        self.get_type_ancestors(node_id, &mut nodes, &mut edges, &mut visited)?;

        // Get descendants (what extends/implements this)
        self.get_type_descendants(node_id, &mut nodes, &mut edges, &mut visited)?;

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![node_id.to_string()],
            confidence: None,
        })
    }

    fn get_type_ancestors(
        &self,
        node_id: &str,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<()> {
        // Recursion guard — depth grows with the type-inheritance ancestor chain.
        crate::ensure_sufficient_stack(|| {
            self.get_type_ancestors_inner(node_id, nodes, edges, visited)
        })
    }

    fn get_type_ancestors_inner(
        &self,
        node_id: &str,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<()> {
        crate::graph::cancel::check()?;
        if visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        let outgoing_edges = self.queries.get_outgoing_edges(
            node_id,
            Some(&[EdgeKind::Extends, EdgeKind::Implements]),
            None,
        )?;
        if outgoing_edges.is_empty() {
            return Ok(());
        }
        let target_ids: Vec<String> = outgoing_edges.iter().map(|e| e.target.clone()).collect();
        let parents = self.queries.get_nodes_by_ids(&target_ids)?;

        for edge in outgoing_edges {
            if let Some(parent_node) = parents.get(&edge.target) {
                if !nodes.contains_key(&parent_node.id) {
                    let parent_id = parent_node.id.clone();
                    nodes.insert(parent_id.clone(), parent_node.clone());
                    edges.push(edge);
                    self.get_type_ancestors(&parent_id, nodes, edges, visited)?;
                }
            }
        }
        Ok(())
    }

    fn get_type_descendants(
        &self,
        node_id: &str,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<()> {
        // Recursion guard — depth grows with the type-inheritance descendant chain.
        crate::ensure_sufficient_stack(|| {
            self.get_type_descendants_inner(node_id, nodes, edges, visited)
        })
    }

    fn get_type_descendants_inner(
        &self,
        node_id: &str,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<()> {
        crate::graph::cancel::check()?;
        if visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        let incoming_edges = self
            .queries
            .get_incoming_edges(node_id, Some(&[EdgeKind::Extends, EdgeKind::Implements]))?;
        if incoming_edges.is_empty() {
            return Ok(());
        }
        let source_ids: Vec<String> = incoming_edges.iter().map(|e| e.source.clone()).collect();
        let children = self.queries.get_nodes_by_ids(&source_ids)?;

        for edge in incoming_edges {
            if let Some(child_node) = children.get(&edge.source) {
                if !nodes.contains_key(&child_node.id) {
                    let child_id = child_node.id.clone();
                    nodes.insert(child_id.clone(), child_node.clone());
                    edges.push(edge);
                    self.get_type_descendants(&child_id, nodes, edges, visited)?;
                }
            }
        }
        Ok(())
    }

    /// Find all usages of a symbol.
    ///
    /// * `node_id` - ID of the symbol node
    ///
    /// Returns the nodes and edges that reference this symbol.
    pub fn find_usages(&self, node_id: &str) -> Result<Vec<NodeRef>> {
        let mut result: Vec<NodeRef> = Vec::new();

        // Get all incoming edges (references, calls, type_of, etc.)
        let incoming_edges = self.queries.get_incoming_edges(node_id, None)?;
        if incoming_edges.is_empty() {
            return Ok(result);
        }

        // Batch-fetch source nodes (was N+1).
        let source_ids: Vec<String> = incoming_edges.iter().map(|e| e.source.clone()).collect();
        let sources = self.queries.get_nodes_by_ids(&source_ids)?;
        for edge in incoming_edges {
            if let Some(source_node) = sources.get(&edge.source) {
                result.push(NodeRef {
                    node: source_node.clone(),
                    edge,
                });
            }
        }

        Ok(result)
    }

    /// Calculate the impact radius of a node.
    ///
    /// Returns all nodes that could be affected by changes to this node.
    ///
    /// * `node_id` - ID of the node
    /// * `max_depth` - Maximum depth to traverse (TS default: 3)
    pub fn get_impact_radius(&self, node_id: &str, max_depth: u32) -> Result<Subgraph> {
        let focal_node = match self.queries.get_node_by_id(node_id)? {
            Some(node) => node,
            None => return Ok(Subgraph::default()),
        };

        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();

        // Add focal node
        nodes.insert(focal_node.id.clone(), focal_node);

        // Traverse incoming edges to find all dependents
        self.get_impact_recursive(node_id, max_depth, 0, &mut nodes, &mut edges, &mut visited)?;

        Ok(Subgraph {
            nodes,
            edges,
            roots: vec![node_id.to_string()],
            confidence: None,
        })
    }

    fn get_impact_recursive(
        &self,
        node_id: &str,
        max_depth: u32,
        current_depth: u32,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<()> {
        // Recursion guard — depth grows with the dependency fan-out traversal.
        crate::ensure_sufficient_stack(|| {
            self.get_impact_recursive_inner(
                node_id,
                max_depth,
                current_depth,
                nodes,
                edges,
                visited,
            )
        })
    }

    fn get_impact_recursive_inner(
        &self,
        node_id: &str,
        max_depth: u32,
        current_depth: u32,
        nodes: &mut HashMap<String, Node>,
        edges: &mut Vec<Edge>,
        visited: &mut HashSet<String>,
    ) -> Result<()> {
        crate::graph::cancel::check()?;
        if current_depth >= max_depth || visited.contains(node_id) {
            return Ok(());
        }
        visited.insert(node_id.to_string());

        // For container nodes (classes, interfaces, structs, etc.), also traverse
        // into their children so that callers of contained methods appear in impact
        if let Some(focal_node) = self.queries.get_node_by_id(node_id)? {
            let is_container = matches!(
                focal_node.kind,
                NodeKind::Class
                    | NodeKind::Interface
                    | NodeKind::Struct
                    | NodeKind::Trait
                    | NodeKind::Protocol
                    | NodeKind::Module
                    | NodeKind::Enum
            );
            if is_container {
                let contains_edges =
                    self.queries
                        .get_outgoing_edges(node_id, Some(&[EdgeKind::Contains]), None)?;
                if !contains_edges.is_empty() {
                    let child_ids: Vec<String> =
                        contains_edges.iter().map(|e| e.target.clone()).collect();
                    let children = self.queries.get_nodes_by_ids(&child_ids)?;
                    for edge in contains_edges {
                        if let Some(child_node) = children.get(&edge.target) {
                            if !visited.contains(&child_node.id) {
                                let child_id = child_node.id.clone();
                                nodes.insert(child_id.clone(), child_node.clone());
                                edges.push(edge);
                                // Recurse into children at the same depth (they're part of the same symbol)
                                self.get_impact_recursive(
                                    &child_id,
                                    max_depth,
                                    current_depth,
                                    nodes,
                                    edges,
                                    visited,
                                )?;
                            }
                        }
                    }
                }
            }
        }

        // Get all incoming edges (things that depend on this node). Exclude
        // `contains`: a container "contains" its members but does not *depend* on
        // them, so following it upward would climb to the parent class and then
        // re-expand every sibling member — exploding impact for a leaf symbol. (#536)
        let incoming_edges: Vec<Edge> = self
            .queries
            .get_incoming_edges(node_id, None)?
            .into_iter()
            .filter(|e| e.kind != EdgeKind::Contains)
            .collect();
        if incoming_edges.is_empty() {
            return Ok(());
        }
        let source_ids: Vec<String> = incoming_edges.iter().map(|e| e.source.clone()).collect();
        let sources = self.queries.get_nodes_by_ids(&source_ids)?;

        for edge in incoming_edges {
            if let Some(source_node) = sources.get(&edge.source) {
                if !nodes.contains_key(&source_node.id) {
                    let source_id = source_node.id.clone();
                    nodes.insert(source_id.clone(), source_node.clone());
                    edges.push(edge);
                    self.get_impact_recursive(
                        &source_id,
                        max_depth,
                        current_depth + 1,
                        nodes,
                        edges,
                        visited,
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Find the shortest path between two nodes.
    ///
    /// * `from_id` - Starting node ID
    /// * `to_id` - Target node ID
    /// * `edge_kinds` - Edge types to consider (all if empty)
    ///
    /// Returns the nodes and edges forming the path, or `None` if no path exists.
    pub fn find_path(
        &self,
        from_id: &str,
        to_id: &str,
        edge_kinds: &[EdgeKind],
    ) -> Result<Option<Vec<PathStep>>> {
        let from_node = self.queries.get_node_by_id(from_id)?;
        let to_node = self.queries.get_node_by_id(to_id)?;

        let (Some(from_node), Some(_to_node)) = (from_node, to_node) else {
            return Ok(None);
        };

        // Trivial path: the source is the target.
        if from_id == to_id {
            return Ok(Some(vec![PathStep {
                node: from_node,
                edge: None,
            }]));
        }

        let kinds = if edge_kinds.is_empty() {
            None
        } else {
            Some(edge_kinds)
        };

        // BFS with a predecessor map. Each node is recorded in `came_from`
        // exactly once — the first (hence shortest) time it is discovered — and
        // marked visited *on enqueue*, so it is never enqueued or expanded
        // twice. The path is reconstructed from `came_from` only after the
        // target is reached.
        //
        // The previous version carried a cloned `Vec<PathStep>` (each step
        // owning a cloned `Node`) on every queue entry and marked nodes visited
        // on *dequeue*. A node was therefore enqueued once per incoming edge,
        // deep-cloning the whole running path each time — O(V*E) `Node` clones,
        // enough to peg a core and balloon the heap on a large index (the
        // observed 100%-CPU / ~1GB-RSS hang of the `paths` tool). Recording one
        // parent edge per node instead makes it O(V+E) time and O(V) memory.
        let mut visited: HashSet<String> = HashSet::from([from_id.to_string()]);
        let mut came_from: HashMap<String, (String, Edge)> = HashMap::new();
        let mut node_cache: HashMap<String, Node> = HashMap::new();
        node_cache.insert(from_node.id.clone(), from_node);
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(from_id.to_string());

        while let Some(node_id) = queue.pop_front() {
            crate::graph::cancel::check()?;
            let outgoing_edges = self.queries.get_outgoing_edges(&node_id, kinds, None)?;
            if outgoing_edges.is_empty() {
                continue;
            }

            // Batch-fetch only the unvisited targets (was N+1 per BFS frontier).
            let want_ids: Vec<String> = outgoing_edges
                .iter()
                .map(|e| e.target.clone())
                .filter(|id| !visited.contains(id))
                .collect();
            let next_nodes = if !want_ids.is_empty() {
                self.queries.get_nodes_by_ids(&want_ids)?
            } else {
                HashMap::new()
            };

            for edge in outgoing_edges {
                let target = edge.target.clone();
                if visited.contains(&target) {
                    continue;
                }
                let Some(next_node) = next_nodes.get(&target) else {
                    continue;
                };

                visited.insert(target.clone());
                node_cache.insert(target.clone(), next_node.clone());
                came_from.insert(target.clone(), (node_id.clone(), edge));

                if target == to_id {
                    return Ok(Some(Self::reconstruct_path(
                        from_id,
                        to_id,
                        &came_from,
                        &mut node_cache,
                    )));
                }

                queue.push_back(target);
            }
        }

        Ok(None) // No path found
    }

    /// Rebuild the `from_id -> to_id` path from a BFS predecessor map.
    ///
    /// Walks parent edges backward from the target, then reverses so the result
    /// runs source-first (`path[0].edge == None`). The walk is capped at
    /// `came_from.len() + 1` steps as a belt-and-suspenders guard: the map is
    /// acyclic by construction (BFS records each node once), so the cap can only
    /// trip on corruption, in which case we stop rather than spin.
    fn reconstruct_path(
        from_id: &str,
        to_id: &str,
        came_from: &HashMap<String, (String, Edge)>,
        node_cache: &mut HashMap<String, Node>,
    ) -> Vec<PathStep> {
        let mut reverse: Vec<PathStep> = Vec::new();
        let mut current = to_id.to_string();
        let max_steps = came_from.len() + 1;

        for _ in 0..max_steps {
            // Each node appears once on a shortest path, so moving it out of the
            // cache is safe and avoids an extra clone.
            let Some(node) = node_cache.remove(&current) else {
                break;
            };
            if current == from_id {
                reverse.push(PathStep { node, edge: None });
                break;
            }
            let Some((parent, edge)) = came_from.get(&current) else {
                break;
            };
            reverse.push(PathStep {
                node,
                edge: Some(edge.clone()),
            });
            current = parent.clone();
        }

        reverse.reverse();
        reverse
    }

    /// Get the containment hierarchy for a node (ancestors).
    ///
    /// * `node_id` - ID of the node
    ///
    /// Returns the ancestor nodes from immediate parent to root.
    pub fn get_ancestors(&self, node_id: &str) -> Result<Vec<Node>> {
        let mut ancestors: Vec<Node> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut current_id = node_id.to_string();

        loop {
            if visited.contains(&current_id) {
                break;
            }
            visited.insert(current_id.clone());

            // Look for 'contains' edges pointing to this node
            let containing_edges = self
                .queries
                .get_incoming_edges(&current_id, Some(&[EdgeKind::Contains]))?;

            let Some(first_edge) = containing_edges.first() else {
                break;
            };

            // Typically there should be at most one containing parent
            if let Some(parent_node) = self.queries.get_node_by_id(&first_edge.source)? {
                current_id = parent_node.id.clone();
                ancestors.push(parent_node);
            } else {
                break;
            }
        }

        Ok(ancestors)
    }

    /// Get immediate children of a node.
    ///
    /// * `node_id` - ID of the node
    ///
    /// Returns the child nodes.
    pub fn get_children(&self, node_id: &str) -> Result<Vec<Node>> {
        let contains_edges =
            self.queries
                .get_outgoing_edges(node_id, Some(&[EdgeKind::Contains]), None)?;
        if contains_edges.is_empty() {
            return Ok(Vec::new());
        }

        // Batch-fetch (was N+1).
        let child_ids: Vec<String> = contains_edges.iter().map(|e| e.target.clone()).collect();
        let child_nodes = self.queries.get_nodes_by_ids(&child_ids)?;
        let mut children: Vec<Node> = Vec::new();
        for edge in &contains_edges {
            if let Some(child_node) = child_nodes.get(&edge.target) {
                children.push(child_node.clone());
            }
        }
        Ok(children)
    }
}

/// The opposite endpoint of an edge relative to `node_id`
/// (TS: `e.source === node.id ? e.target : e.source`).
fn other_endpoint<'a>(edge: &'a Edge, node_id: &str) -> &'a str {
    if edge.source == node_id {
        &edge.target
    } else {
        &edge.source
    }
}
