//! Exact-name matching strategy.

use super::support::{compute_path_proximity, find_best_match};
use crate::resolution::jvm_scope;
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, Node};

/// Try to resolve a reference by exact name match
pub fn match_by_exact_name(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    match_by_exact_name_ranked(reference, context, None)
}

/// `match_by_exact_name` with an optional GPU-precomputed `find_best_match`
/// outcome for this reference's candidate set (feature `gpu`): `Some(None)` =
/// the kernel determined no candidate beats the selection floor,
/// `Some(Some(node))` = the kernel's winner (identical to the CPU pick by
/// construction — the kernel mirrors the scoring formula exactly and scans
/// candidates in `get_nodes_by_name` order). `None` = compute on CPU.
pub fn match_by_exact_name_ranked(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    ranked: Option<Option<&Node>>,
) -> Option<ResolvedRef> {
    let mut candidates = context.get_nodes_by_name(&reference.reference_name);

    // Apex symbols are case-insensitive — `MyClass` and `myclass` name the
    // same declaration. When the case-exact lookup finds nothing, retry
    // through the lowercase index, restricted to Apex nodes so a symbol from
    // a case-sensitive language that happens to fold equal can't hijack the
    // match. The exact (case-correct) path above stays untouched.
    if candidates.is_empty() && reference.language == Language::Apex {
        candidates = context
            .get_nodes_by_lower_name(&reference.reference_name.to_lowercase())
            .into_iter()
            .filter(|n| n.language == Language::Apex)
            .collect();
    }

    if candidates.is_empty() {
        return None;
    }

    // If only one match, use it — but penalize cross-language matches
    if candidates.len() == 1 {
        let is_cross_language = candidates[0].language != reference.language;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: if is_cross_language { 0.5 } else { 0.9 },
            resolved_by: ResolvedBy::ExactMatch,
        });
    }

    if reference.language == Language::Java || reference.language == Language::Kotlin {
        if let Some(jvm_match) = jvm_scope::match_exact_name(reference, context, &candidates) {
            return Some(jvm_match);
        }
    }

    // Multiple matches - try to narrow down
    let best = match ranked {
        Some(precomputed) => precomputed,
        None => find_best_match(reference, &candidates, context),
    };
    if let Some(best_match) = best {
        // Lower confidence when the match is from a distant/unrelated module
        let proximity = compute_path_proximity(&reference.file_path, &best_match.file_path);
        let confidence = if proximity >= 30 { 0.7 } else { 0.4 };
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: best_match.id.clone(),
            confidence,
            resolved_by: ResolvedBy::ExactMatch,
        });
    }

    None
}
