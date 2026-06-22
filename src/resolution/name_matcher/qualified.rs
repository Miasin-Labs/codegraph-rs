//! Qualified-name matching strategy.

use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};

/// Try to resolve by qualified name
pub fn match_by_qualified_name(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // Check if the reference name looks qualified (contains :: or .)
    if !reference.reference_name.contains("::") && !reference.reference_name.contains('.') {
        return None;
    }

    let candidates = context.get_nodes_by_qualified_name(&reference.reference_name);

    if candidates.len() == 1 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: 0.95,
            resolved_by: ResolvedBy::QualifiedName,
        });
    }

    // Try partial qualified name match
    let parts: Vec<&str> = reference.reference_name.split([':', '.']).collect();
    if let Some(last_name) = parts.last().filter(|s| !s.is_empty()) {
        let partial_candidates = context.get_nodes_by_name(last_name);
        for candidate in &partial_candidates {
            if candidate
                .qualified_name
                .ends_with(&reference.reference_name)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: candidate.id.clone(),
                    confidence: 0.85,
                    resolved_by: ResolvedBy::QualifiedName,
                });
            }
        }
    }

    None
}
