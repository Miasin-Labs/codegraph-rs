//! Strict matching for ArkUI chained attribute helpers.

use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, NodeKind};

const ATTRIBUTE_DECORATORS: &[&str] = &["Extend", "Styles", "AnimatableExtend", "Builder"];

pub(super) fn match_attribute_helper(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if reference.language != Language::Arkts || !reference.reference_name.starts_with('.') {
        return None;
    }
    let base = reference.reference_name.strip_prefix('.')?;
    let mut candidates: Vec<_> = context
        .get_nodes_by_name(base)
        .into_iter()
        .filter(|node| {
            node.language == Language::Arkts
                && node.kind == NodeKind::Function
                && node.decorators.as_ref().is_some_and(|decorators| {
                    decorators
                        .iter()
                        .any(|decorator| ATTRIBUTE_DECORATORS.contains(&decorator.as_str()))
                })
        })
        .collect();
    if candidates.len() > 1 {
        candidates.retain(|node| node.file_path == reference.file_path);
    }
    if candidates.len() != 1 {
        return None;
    }
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: candidates.remove(0).id,
        confidence: 0.85,
        resolved_by: ResolvedBy::ExactMatch,
    })
}
