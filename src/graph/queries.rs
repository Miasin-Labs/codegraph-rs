//! Graph Query Functions
//!
//! Higher-level query functions built on top of traversal algorithms.
//!
//! Ported from `src/graph/queries.ts`.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::db::queries::QueryBuilder;
use crate::error::{CodeGraphError, Result};
use crate::graph::traversal::GraphTraverser;
use crate::types::{Context, Edge, EdgeKind, Node, NodeKind, NodeRef, Subgraph};

/// Complexity metrics for a node (mirrors the inline return type of
/// `getNodeMetrics` in TS; camelCase serde keeps JSON wire parity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeMetrics {
    pub incoming_edge_count: usize,
    pub outgoing_edge_count: usize,
    pub call_count: usize,
    pub caller_count: usize,
    pub child_count: usize,
    pub depth: usize,
}

/// Graph query manager for complex queries.
pub struct GraphQueryManager {
    queries: Rc<QueryBuilder>,
    traverser: GraphTraverser,
}

impl GraphQueryManager {
    pub fn new(queries: Rc<QueryBuilder>) -> Self {
        let traverser = GraphTraverser::new(Rc::clone(&queries));
        GraphQueryManager { queries, traverser }
    }

    /// Get full context for a node.
    ///
    /// Returns the focal node along with its ancestors, children,
    /// and both incoming and outgoing references.
    ///
    /// * `node_id` - ID of the focal node
    ///
    /// Errors with `Node not found: <id>` when the node doesn't exist
    /// (mirrors the TS `throw`).
    pub fn get_context(&self, node_id: &str) -> Result<Context> {
        let focal = self
            .queries
            .get_node_by_id(node_id)?
            .ok_or_else(|| CodeGraphError::other(format!("Node not found: {node_id}")))?;

        // Get ancestors (containment hierarchy)
        let ancestors = self.traverser.get_ancestors(node_id)?;

        // Get children
        let children = self.traverser.get_children(node_id)?;

        // Get incoming references (things that reference this node)
        let incoming_edges = self.queries.get_incoming_edges(node_id, None)?;
        let mut incoming_refs: Vec<NodeRef> = Vec::new();
        for edge in incoming_edges {
            // Skip containment edges (already in ancestors)
            if edge.kind == EdgeKind::Contains {
                continue;
            }
            if let Some(node) = self.queries.get_node_by_id(&edge.source)? {
                incoming_refs.push(NodeRef { node, edge });
            }
        }

        // Get outgoing references (things this node references)
        let outgoing_edges = self.queries.get_outgoing_edges(node_id, None, None)?;
        let mut outgoing_refs: Vec<NodeRef> = Vec::new();
        for edge in outgoing_edges {
            // Skip containment edges (already in children)
            if edge.kind == EdgeKind::Contains {
                continue;
            }
            if let Some(node) = self.queries.get_node_by_id(&edge.target)? {
                outgoing_refs.push(NodeRef { node, edge });
            }
        }

        // Get type information (type_of, returns edges)
        let mut types: Vec<Node> = Vec::new();
        let type_edge_kinds: [EdgeKind; 2] = [EdgeKind::TypeOf, EdgeKind::Returns];
        for kind in type_edge_kinds {
            let type_edges = self
                .queries
                .get_outgoing_edges(node_id, Some(&[kind]), None)?;
            for edge in type_edges {
                if let Some(type_node) = self.queries.get_node_by_id(&edge.target)? {
                    if !types.iter().any(|t| t.id == type_node.id) {
                        types.push(type_node);
                    }
                }
            }
        }

        // Get relevant imports
        let mut imports: Vec<Node> = Vec::new();
        let file_node = ancestors.iter().find(|a| a.kind == NodeKind::File);
        if let Some(file_node) = file_node {
            let import_edges =
                self.queries
                    .get_outgoing_edges(&file_node.id, Some(&[EdgeKind::Imports]), None)?;
            for edge in import_edges {
                if let Some(import_node) = self.queries.get_node_by_id(&edge.target)? {
                    imports.push(import_node);
                }
            }
        }

        Ok(Context {
            focal,
            ancestors,
            children,
            incoming_refs,
            outgoing_refs,
            types,
            imports,
        })
    }

    /// Get dependencies of a file.
    ///
    /// Returns all files that this file imports from.
    ///
    /// * `file_path` - Path to the file
    pub fn get_file_dependencies(&self, file_path: &str) -> Result<Vec<String>> {
        let nodes = self.queries.get_nodes_by_file(file_path)?;
        let file_node = nodes.iter().find(|n| n.kind == NodeKind::File);

        let Some(file_node) = file_node else {
            return Ok(Vec::new());
        };

        // Insertion-ordered set (TS `Set` preserves insertion order).
        let mut dependencies: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let import_edges =
            self.queries
                .get_outgoing_edges(&file_node.id, Some(&[EdgeKind::Imports]), None)?;

        for edge in import_edges {
            if let Some(target_node) = self.queries.get_node_by_id(&edge.target)? {
                if target_node.file_path != file_path && seen.insert(target_node.file_path.clone())
                {
                    dependencies.push(target_node.file_path);
                }
            }
        }

        Ok(dependencies)
    }

    /// Get dependents of a file.
    ///
    /// Returns all files that import from this file.
    ///
    /// * `file_path` - Path to the file
    pub fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>> {
        let nodes = self.queries.get_nodes_by_file(file_path)?;
        // Insertion-ordered set (TS `Set` preserves insertion order).
        let mut dependents: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();

        // Check file-level incoming import edges (file:X imports file:Y)
        let file_node = nodes.iter().find(|n| n.kind == NodeKind::File);
        if let Some(file_node) = file_node {
            let incoming_file_edges = self
                .queries
                .get_incoming_edges(&file_node.id, Some(&[EdgeKind::Imports]))?;
            for edge in incoming_file_edges {
                if let Some(source_node) = self.queries.get_node_by_id(&edge.source)? {
                    if source_node.file_path != file_path
                        && seen.insert(source_node.file_path.clone())
                    {
                        dependents.push(source_node.file_path);
                    }
                }
            }
        }

        // Also check node-level imports of exported symbols
        for node in &nodes {
            if node.is_exported.unwrap_or(false) {
                let incoming_edges = self
                    .queries
                    .get_incoming_edges(&node.id, Some(&[EdgeKind::Imports]))?;
                for edge in incoming_edges {
                    if let Some(source_node) = self.queries.get_node_by_id(&edge.source)? {
                        if source_node.file_path != file_path
                            && seen.insert(source_node.file_path.clone())
                        {
                            dependents.push(source_node.file_path);
                        }
                    }
                }
            }
        }

        Ok(dependents)
    }

    /// Get all symbols exported by a file.
    ///
    /// * `file_path` - Path to the file
    pub fn get_exported_symbols(&self, file_path: &str) -> Result<Vec<Node>> {
        let nodes = self.queries.get_nodes_by_file(file_path)?;
        Ok(nodes
            .into_iter()
            .filter(|n| n.is_exported.unwrap_or(false))
            .collect())
    }

    /// Find symbols by qualified name pattern.
    ///
    /// * `pattern` - Pattern to match (supports `*` wildcard)
    pub fn find_by_qualified_name(&self, pattern: &str) -> Result<Vec<Node>> {
        // Convert glob pattern to regex (same escape set as the TS implementation)
        let mut regex_pattern = String::with_capacity(pattern.len() * 2);
        for ch in pattern.chars() {
            match ch {
                '.' | '+' | '^' | '$' | '{' | '}' | '(' | ')' | '|' | '[' | ']' | '\\' => {
                    regex_pattern.push('\\');
                    regex_pattern.push(ch);
                }
                '*' => regex_pattern.push_str(".*"),
                '?' => regex_pattern.push('.'),
                other => regex_pattern.push(other),
            }
        }

        let regex = Regex::new(&format!("^{regex_pattern}$"))
            .map_err(|e| CodeGraphError::other(e.to_string()))?;

        // This is inefficient for large graphs - would need FTS index on qualified_name
        // For now, use kind-based filtering if possible
        let mut all_nodes: Vec<Node> = Vec::new();
        let kinds: [NodeKind; 7] = [
            NodeKind::Class,
            NodeKind::Function,
            NodeKind::Method,
            NodeKind::Interface,
            NodeKind::TypeAlias,
            NodeKind::Variable,
            NodeKind::Constant,
        ];

        for kind in kinds {
            let nodes = self.queries.get_nodes_by_kind(kind)?;
            for node in nodes {
                if regex.is_match(&node.qualified_name) {
                    all_nodes.push(node);
                }
            }
        }

        Ok(all_nodes)
    }

    /// Get the module/package structure.
    ///
    /// Returns a map of directory paths to contained files.
    /// (TS returns an insertion-ordered `Map`; this `HashMap` is unordered —
    /// the per-directory file lists keep their order.)
    pub fn get_module_structure(&self) -> Result<HashMap<String, Vec<String>>> {
        let files = self.queries.get_all_files()?;
        let mut structure: HashMap<String, Vec<String>> = HashMap::new();

        for file in files {
            // TS: parts.slice(0, -1).join('/') || '.'
            let dir = match file.path.rsplit_once('/') {
                Some((dir, _)) if !dir.is_empty() => dir.to_string(),
                _ => ".".to_string(),
            };

            structure.entry(dir).or_default().push(file.path);
        }

        Ok(structure)
    }

    /// Find circular dependencies in the graph.
    ///
    /// Returns the cycles; each cycle is an array of file paths.
    pub fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>> {
        let files = self.queries.get_all_files()?;
        let mut cycles: Vec<Vec<String>> = Vec::new();
        let mut visited: HashSet<String> = HashSet::new();
        let mut recursion_stack: HashSet<String> = HashSet::new();

        for file in &files {
            if !visited.contains(&file.path) {
                self.dfs_cycles(
                    &file.path,
                    &[],
                    &mut cycles,
                    &mut visited,
                    &mut recursion_stack,
                )?;
            }
        }

        Ok(cycles)
    }

    /// Recursive cycle-detection helper (the inline `dfs` closure in TS).
    fn dfs_cycles(
        &self,
        file_path: &str,
        path: &[String],
        cycles: &mut Vec<Vec<String>>,
        visited: &mut HashSet<String>,
        recursion_stack: &mut HashSet<String>,
    ) -> Result<()> {
        if recursion_stack.contains(file_path) {
            // Found a cycle
            if let Some(cycle_start) = path.iter().position(|p| p == file_path) {
                cycles.push(path[cycle_start..].to_vec());
            }
            return Ok(());
        }

        if visited.contains(file_path) {
            return Ok(());
        }

        visited.insert(file_path.to_string());
        recursion_stack.insert(file_path.to_string());

        let dependencies = self.get_file_dependencies(file_path)?;
        let mut next_path = path.to_vec();
        next_path.push(file_path.to_string());
        for dep in dependencies {
            self.dfs_cycles(&dep, &next_path, cycles, visited, recursion_stack)?;
        }

        recursion_stack.remove(file_path);
        Ok(())
    }

    /// Get complexity metrics for a node.
    ///
    /// * `node_id` - ID of the node
    pub fn get_node_metrics(&self, node_id: &str) -> Result<NodeMetrics> {
        let incoming_edges = self.queries.get_incoming_edges(node_id, None)?;
        let outgoing_edges = self.queries.get_outgoing_edges(node_id, None, None)?;

        let call_count = outgoing_edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .count();
        let caller_count = incoming_edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .count();
        let child_count = outgoing_edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Contains)
            .count();

        let ancestors = self.traverser.get_ancestors(node_id)?;

        Ok(NodeMetrics {
            incoming_edge_count: incoming_edges.len(),
            outgoing_edge_count: outgoing_edges.len(),
            call_count,
            caller_count,
            child_count,
            depth: ancestors.len(),
        })
    }

    /// Find dead code (nodes with no incoming references).
    ///
    /// * `kinds` - Node kinds to check (default: functions, methods, classes)
    pub fn find_dead_code(&self, kinds: Option<&[NodeKind]>) -> Result<Vec<Node>> {
        const DEFAULT_KINDS: [NodeKind; 3] =
            [NodeKind::Function, NodeKind::Method, NodeKind::Class];
        let target_kinds = kinds.unwrap_or(&DEFAULT_KINDS);
        let mut dead_code: Vec<Node> = Vec::new();

        for kind in target_kinds {
            let nodes = self.queries.get_nodes_by_kind(*kind)?;
            for node in nodes {
                // Skip exported symbols (they may be used externally)
                if node.is_exported.unwrap_or(false) {
                    continue;
                }

                let incoming_edges = self.queries.get_incoming_edges(&node.id, None)?;

                // Filter out containment edges
                let references = incoming_edges
                    .iter()
                    .filter(|e| e.kind != EdgeKind::Contains)
                    .count();

                if references == 0 {
                    dead_code.push(node);
                }
            }
        }

        Ok(dead_code)
    }

    /// Get subgraph containing nodes matching a filter.
    ///
    /// * `filter` - Filter function to select nodes
    /// * `include_edges` - Whether to include edges between matching nodes
    ///   (TS default: `true`)
    pub fn get_filtered_subgraph<F>(&self, filter: F, include_edges: bool) -> Result<Subgraph>
    where
        F: Fn(&Node) -> bool,
    {
        let mut nodes: HashMap<String, Node> = HashMap::new();
        let mut edges: Vec<Edge> = Vec::new();

        // Get all nodes of common kinds
        let kinds: [NodeKind; 12] = [
            NodeKind::File,
            NodeKind::Module,
            NodeKind::Class,
            NodeKind::Struct,
            NodeKind::Interface,
            NodeKind::Trait,
            NodeKind::Function,
            NodeKind::Method,
            NodeKind::Variable,
            NodeKind::Constant,
            NodeKind::Enum,
            NodeKind::TypeAlias,
        ];

        for kind in kinds {
            let kind_nodes = self.queries.get_nodes_by_kind(kind)?;
            for node in kind_nodes {
                if filter(&node) {
                    nodes.insert(node.id.clone(), node);
                }
            }
        }

        // Include edges between matching nodes
        if include_edges {
            let node_ids: Vec<String> = nodes.keys().cloned().collect();
            edges.extend(self.queries.find_edges_between_nodes(&node_ids, None)?);
        }

        Ok(Subgraph {
            nodes,
            edges,
            roots: Vec::new(),
            confidence: None,
        })
    }

    /// Access the underlying traverser for direct traversal operations.
    pub fn get_traverser(&self) -> &GraphTraverser {
        &self.traverser
    }
}
