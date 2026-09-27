//! Per-file scope index: which fns/classes enclose a line.
//!
//! Resolution asks for the enclosing fn or class of nearly every reference.
//! Scanning (and copying) every node of the file per reference is quadratic
//! in a bundled file with tens of thousands of nodes, so the snapshot builds
//! this once: each file's [`SCOPE_KINDS`] nodes sorted by start line, each
//! pointing at the nearest earlier node that still covers its start. Scopes
//! nest, so every node containing a line is on the chain up from the last
//! node starting at or before it; a query is a binary search and a walk up
//! that chain (as long as the nesting is deep).

use std::collections::HashMap;

use super::NodeIndex;
use crate::resolution::types::SCOPE_KINDS;
use crate::types::Node;

const NO_PARENT: u32 = u32::MAX;

#[derive(Default)]
struct FileScopes {
    starts: Vec<u32>,
    ends: Vec<u32>,
    nodes: Vec<NodeIndex>,
    parents: Vec<u32>,
}

#[derive(Default)]
pub(super) struct ScopeIndex(HashMap<String, FileScopes>);

impl ScopeIndex {
    pub(super) fn build(nodes: &[Node]) -> Self {
        let mut by_file: HashMap<&str, Vec<(u32, u32, NodeIndex)>> = HashMap::new();
        for (offset, node) in nodes.iter().enumerate() {
            if !SCOPE_KINDS.contains(&node.kind) {
                continue;
            }
            let Ok(index) = NodeIndex::try_from(offset) else {
                continue;
            };
            by_file.entry(node.file_path.as_str()).or_default().push((
                node.start_line,
                node.end_line.max(node.start_line),
                index,
            ));
        }
        let files = by_file
            .into_iter()
            .map(|(file, mut scopes)| {
                // Outer before inner when two start on the same line.
                scopes.sort_by_key(|&(start, end, index)| (start, std::cmp::Reverse(end), index));
                (file.to_string(), FileScopes::nest(scopes))
            })
            .collect();
        Self(files)
    }

    /// The scope nodes of `file_path` containing `line`, innermost first.
    pub(super) fn enclosing(&self, file_path: &str, line: u32) -> Vec<NodeIndex> {
        let Some(file) = self.0.get(file_path) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        let mut at = match file.starts.partition_point(|&start| start <= line) {
            0 => NO_PARENT,
            after => (after - 1) as u32,
        };
        while at != NO_PARENT {
            let i = at as usize;
            if file.ends[i] >= line {
                found.push(file.nodes[i]);
            }
            at = file.parents[i];
        }
        found
    }
}

impl FileScopes {
    fn nest(scopes: Vec<(u32, u32, NodeIndex)>) -> Self {
        let mut file = FileScopes::default();
        let mut open: Vec<u32> = Vec::new();
        for (position, (start, end, index)) in scopes.into_iter().enumerate() {
            while open
                .last()
                .is_some_and(|&top| file.ends[top as usize] < start)
            {
                open.pop();
            }
            file.parents.push(open.last().copied().unwrap_or(NO_PARENT));
            file.starts.push(start);
            file.ends.push(end);
            file.nodes.push(index);
            open.push(position as u32);
        }
        file
    }
}

#[cfg(test)]
mod tests {
    use super::ScopeIndex;
    use crate::types::{Language, Node, NodeKind};

    fn scope(id: &str, kind: NodeKind, start: u32, end: u32) -> Node {
        Node::new(id, kind, id, id, "a.js", Language::Javascript, start, end)
    }

    fn ids(index: &ScopeIndex, nodes: &[Node], line: u32) -> Vec<String> {
        index
            .enclosing("a.js", line)
            .into_iter()
            .map(|at| nodes[at as usize].id.clone())
            .collect()
    }

    #[test]
    fn finds_every_enclosing_scope_innermost_first() {
        let nodes = vec![
            scope("outer", NodeKind::Function, 1, 100),
            scope("a", NodeKind::Function, 2, 10),
            scope("a_inner", NodeKind::Function, 3, 5),
            scope("var", NodeKind::Variable, 4, 4),
            scope("b", NodeKind::Class, 20, 40),
            scope("b_method", NodeKind::Method, 21, 30),
            scope("touching", NodeKind::Function, 40, 50),
        ];
        let index = ScopeIndex::build(&nodes);
        assert_eq!(ids(&index, &nodes, 4), ["a_inner", "a", "outer"]);
        assert_eq!(ids(&index, &nodes, 7), ["a", "outer"]);
        assert_eq!(ids(&index, &nodes, 15), ["outer"]);
        assert_eq!(ids(&index, &nodes, 25), ["b_method", "b", "outer"]);
        assert_eq!(ids(&index, &nodes, 35), ["b", "outer"]);
        assert_eq!(ids(&index, &nodes, 40), ["touching", "b", "outer"]);
        assert_eq!(ids(&index, &nodes, 45), ["touching", "outer"]);
        assert!(ids(&index, &nodes, 101).is_empty());
        assert!(index.enclosing("other.js", 4).is_empty());
    }

    #[test]
    fn a_flat_file_walks_no_further_than_the_nesting() {
        let nodes: Vec<Node> = (0..10_000)
            .map(|i| scope(&format!("f{i}"), NodeKind::Function, i * 3 + 1, i * 3 + 2))
            .collect();
        let index = ScopeIndex::build(&nodes);
        assert_eq!(ids(&index, &nodes, 3 * 5_000 + 2), ["f5000"]);
        assert!(ids(&index, &nodes, 3 * 5_000 + 3).is_empty());
    }
}
