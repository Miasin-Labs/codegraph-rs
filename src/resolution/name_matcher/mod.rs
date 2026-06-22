//! Name Matcher
//!
//! Handles symbol name matching for reference resolution.
//! Ported from `src/resolution/name-matcher.ts`.
//!
//! Retrieval-quality-critical: the scoring weights, thresholds, and
//! tiebreaks in child strategies are ported EXACTLY from the TS source — do
//! not tweak without re-validating retrieval.

mod exact;
mod file;
mod fuzzy;
mod method;
mod qualified;
mod receiver;
mod support;

#[cfg(test)]
mod tests;

use exact::match_by_exact_name_ranked as exact_ranked;
pub use exact::{match_by_exact_name, match_by_exact_name_ranked};
pub use file::match_by_file_path;
use file::match_by_file_path as file_path;
use fuzzy::match_fuzzy_hinted as fuzzy_hinted;
pub use fuzzy::{match_fuzzy, match_fuzzy_hinted};
use method::match_method_call_hinted as method_hinted;
#[cfg(feature = "gpu")]
pub(crate) use method::{capitalize_first_shared, split_method_call};
pub use method::{match_method_call, match_method_call_hinted};
pub use qualified::match_by_qualified_name;
use qualified::match_by_qualified_name as qualified_name;

use crate::resolution::types::{ResolutionContext, ResolvedRef, UnresolvedRef};
use crate::types::Node;

/// Match all strategies in order of confidence
pub fn match_reference(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    match_reference_ranked(reference, context, None)
}

/// `match_reference` with an optional GPU-precomputed exact-name ranking
/// (consulted only by strategy 3 — earlier strategies are unaffected).
pub fn match_reference_ranked(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    ranked: Option<Option<&Node>>,
) -> Option<ResolvedRef> {
    match_reference_hinted(reference, context, ranked, None)
}

/// Full hint surface: `ranked` feeds strategy 3 (exact-name), `s12` feeds
/// the method-call strategies 1+2.
pub fn match_reference_hinted(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    ranked: Option<Option<&Node>>,
    s12: Option<Option<(&Node, bool)>>,
) -> Option<ResolvedRef> {
    match_reference_full_hints(reference, context, ranked, s12, None)
}

/// Full hint surface incl. the strategy-4 fuzzy verdict.
pub fn match_reference_full_hints(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    ranked: Option<Option<&Node>>,
    s12: Option<Option<(&Node, bool)>>,
    fuzzy: Option<Option<(&Node, bool)>>,
) -> Option<ResolvedRef> {
    // Try strategies in order of confidence

    // 0. File path match (e.g., "snippets/drawer-menu.liquid" → file node)
    if let Some(result) = file_path(reference, context) {
        return Some(result);
    }

    // 1. Qualified name match (highest confidence)
    if let Some(result) = qualified_name(reference, context) {
        return Some(result);
    }

    // 2. Method call pattern
    if let Some(result) = method_hinted(reference, context, s12) {
        return Some(result);
    }

    // 3. Exact name match
    if let Some(result) = exact_ranked(reference, context, ranked) {
        return Some(result);
    }

    // 4. Fuzzy match (lowest confidence)
    if let Some(result) = fuzzy_hinted(reference, context, fuzzy) {
        return Some(result);
    }

    None
}
