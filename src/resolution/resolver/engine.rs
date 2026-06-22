#[cfg(not(feature = "gpu"))]
use std::collections::HashMap;

use crate::resolution::import_resolver::{resolve_jvm_import, resolve_via_import};
use crate::resolution::name_matcher;
use crate::resolution::types::{FrameworkResolver, ResolutionContext, ResolvedRef, UnresolvedRef};
#[cfg(not(feature = "gpu"))]
use crate::resolution::types::{ResolutionResult, ResolutionStats};
use crate::types::{Language, Node};

pub(super) trait ResolutionPolicy {
    fn is_built_in_or_external(&self, reference: &UnresolvedRef) -> bool;
    fn has_any_possible_match(&self, name: &str) -> bool;
    fn has_any_possible_match_ci(&self, name: &str) -> bool;
    fn matches_any_import(&self, reference: &UnresolvedRef) -> bool;
}

#[allow(clippy::type_complexity)]
pub(super) fn resolve_one<P>(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    frameworks: &[Box<dyn FrameworkResolver>],
    policy: &P,
    known_hint: Option<bool>,
    ranked: Option<Option<&Node>>,
    s12: Option<Option<(&Node, bool)>>,
    fuzzy: Option<Option<(&Node, bool)>>,
) -> Option<ResolvedRef>
where
    P: ResolutionPolicy + ?Sized,
{
    if policy.is_built_in_or_external(reference) {
        return None;
    }

    let (ranked, s12, fuzzy) = if reference.language == Language::Apex {
        (None, None, None)
    } else {
        (ranked, s12, fuzzy)
    };

    if !known_hint.unwrap_or_else(|| policy.has_any_possible_match(&reference.reference_name))
        && !(reference.language == Language::Apex
            && policy.has_any_possible_match_ci(&reference.reference_name))
        && !policy.matches_any_import(reference)
        && !frameworks
            .iter()
            .any(|f| f.claims_reference(&reference.reference_name))
    {
        return None;
    }

    let jvm_import = resolve_jvm_import(reference, context);
    if jvm_import.is_some() {
        return jvm_import;
    }

    let mut candidates: Vec<ResolvedRef> = Vec::new();

    for framework in frameworks {
        if let Some(result) = framework.resolve(reference, context) {
            if result.confidence >= 0.9 {
                return Some(result);
            }
            candidates.push(result);
        }
    }

    if let Some(import_result) = resolve_via_import(reference, context) {
        if import_result.confidence >= 0.9 {
            return Some(import_result);
        }
        candidates.push(import_result);
    }

    if let Some(name_result) =
        name_matcher::match_reference_full_hints(reference, context, ranked, s12, fuzzy)
    {
        candidates.push(name_result);
    }

    candidates.into_iter().reduce(|best, curr| {
        if curr.confidence > best.confidence {
            curr
        } else {
            best
        }
    })
}

#[cfg(not(feature = "gpu"))]
pub(super) fn result_from_parts(
    total: usize,
    resolved: Vec<ResolvedRef>,
    unresolved: Vec<UnresolvedRef>,
) -> ResolutionResult {
    let mut by_method: HashMap<String, usize> = HashMap::new();
    for result in &resolved {
        *by_method
            .entry(result.resolved_by.as_str().to_string())
            .or_insert(0) += 1;
    }

    ResolutionResult {
        stats: ResolutionStats {
            total,
            resolved: resolved.len(),
            unresolved: unresolved.len(),
            by_method,
        },
        resolved,
        unresolved,
    }
}
