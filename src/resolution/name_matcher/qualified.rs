//! Qualified-name matching strategy.

use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::Node;

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
    let last_name = parts.last().filter(|s| !s.is_empty())?;
    let partial_candidates = context.get_nodes_by_name(last_name);
    let matches: Vec<&Node> = partial_candidates
        .iter()
        .filter(|candidate| {
            ends_at_name_boundary(&candidate.qualified_name, &reference.reference_name)
        })
        .collect();
    one_candidate(&matches, reference).map(|candidate| ResolvedRef {
        original: reference.clone(),
        target_node_id: candidate.id.clone(),
        confidence: 0.85,
        resolved_by: ResolvedBy::QualifiedName,
    })
}

/// The single match, or the single one in the referencing file. Several
/// same-named definitions elsewhere (a test `Ctx::new` in two modules) are
/// left to the strategies that can weigh imports and scope, rather than
/// resolved to whichever came first.
fn one_candidate<'a>(matches: &[&'a Node], reference: &UnresolvedRef) -> Option<&'a Node> {
    if let [only] = matches {
        return Some(only);
    }
    let mut local = matches
        .iter()
        .filter(|candidate| candidate.file_path == reference.file_path);
    match (local.next(), local.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// `qualified` ends with the whole path `suffix`: equal, or the suffix starts
/// right after a path separator. A raw `ends_with` let `Value::from` match
/// `TomlValue::from` and `Map::new` match `OrderedNodeMap::new`.
fn ends_at_name_boundary(qualified: &str, suffix: &str) -> bool {
    qualified
        .strip_suffix(suffix)
        .is_some_and(|head| head.is_empty() || head.ends_with([':', '.', '/', '\\']))
}
