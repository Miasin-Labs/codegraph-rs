//! File-path matching strategy.

use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Node, NodeKind};

/// Try to resolve a path-like reference (e.g., "snippets/drawer-menu.liquid")
/// by matching the filename against file nodes.
pub fn match_by_file_path(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if !reference.reference_name.contains('/') {
        return None;
    }

    // Extract the filename from the path
    let file_name = reference.reference_name.split('/').next_back()?;
    if file_name.is_empty() {
        return None;
    }

    // Search for file nodes with this name
    let candidates = context.get_nodes_by_name(file_name);
    let file_nodes: Vec<&Node> = candidates
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .collect();

    if file_nodes.is_empty() {
        return None;
    }

    // Prefer exact path match on qualified_name
    let exact_match = file_nodes.iter().find(|n| {
        n.qualified_name == reference.reference_name || n.file_path == reference.reference_name
    });
    if let Some(exact) = exact_match {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: exact.id.clone(),
            confidence: 0.95,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    // Fall back to suffix match (e.g., ref="snippets/foo.liquid" matches "src/snippets/foo.liquid")
    let suffix_match = file_nodes.iter().find(|n| {
        n.qualified_name.ends_with(&reference.reference_name)
            || n.file_path.ends_with(&reference.reference_name)
    });
    if let Some(suffix) = suffix_match {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: suffix.id.clone(),
            confidence: 0.85,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    // If only one file node with this name, use it with lower confidence
    if file_nodes.len() == 1 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: file_nodes[0].id.clone(),
            confidence: 0.7,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    None
}
