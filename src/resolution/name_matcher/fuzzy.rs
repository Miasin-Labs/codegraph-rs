//! Fuzzy fallback matching strategy.

use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Node, NodeKind};

/// Fuzzy match - last resort with lower confidence
pub fn match_fuzzy(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    match_fuzzy_hinted(reference, context, None)
}

/// `match_fuzzy` with an optional GPU-precomputed uniqueness verdict:
/// `Some(None)` = the kernel proved no unique callable candidate exists;
/// `Some(Some((node, cross_language)))` = the unique winner.
pub fn match_fuzzy_hinted(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    fuzzy: Option<Option<(&Node, bool)>>,
) -> Option<ResolvedRef> {
    if let Some(precomputed) = fuzzy {
        return precomputed.map(|(node, cross)| ResolvedRef {
            original: reference.clone(),
            target_node_id: node.id.clone(),
            confidence: if cross { 0.3 } else { 0.5 },
            resolved_by: ResolvedBy::Fuzzy,
        });
    }
    let lower_name = reference.reference_name.to_lowercase();

    // Use pre-built lowercase index for O(1) lookup instead of scanning all nodes
    let candidates = context.get_nodes_by_lower_name(&lower_name);

    // Filter to callable kinds only (function, method, class)
    let callable_candidates: Vec<&Node> = candidates
        .iter()
        .filter(|n| {
            n.kind == NodeKind::Function || n.kind == NodeKind::Method || n.kind == NodeKind::Class
        })
        .collect();

    // Prefer same-language matches
    let same_language_candidates: Vec<&Node> = callable_candidates
        .iter()
        .filter(|n| n.language == reference.language)
        .copied()
        .collect();
    let final_candidates: &[&Node] = if !same_language_candidates.is_empty() {
        &same_language_candidates
    } else {
        &callable_candidates
    };

    if final_candidates.len() == 1 {
        let is_cross_language = final_candidates[0].language != reference.language;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: final_candidates[0].id.clone(),
            confidence: if is_cross_language { 0.3 } else { 0.5 },
            resolved_by: ResolvedBy::Fuzzy,
        });
    }

    None
}
