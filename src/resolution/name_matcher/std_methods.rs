//! Rust method calls that name matching must leave unresolved.
//!
//! A method call on a chained or field receiver (`v.iter().next()`,
//! `self.map.get(k)`) reaches resolution as its bare method name, flagged as
//! having dropped its receiver; a call on a local whose type inference cannot
//! pin down (`|e| e.into_inner()`) keeps its receiver but gains no type. When
//! std, core, or alloc define a method of that name on any type or trait, the
//! call may well run it, and the only project symbol name matching can offer
//! is an unrelated same-named one (`.next()` landing on `FrontierIter::next`,
//! `.parent()` on `ModuleLocation::parent`). Such a call stays unresolved.
//! Project-specific names (`.get_outgoing_edges()`) keep resolving as before.
//!
//! A method a direct dependency of the project defines (`node.walk()`,
//! `value.as_object()`; names from the crate sources `Cargo.lock` pins,
//! through [`ResolutionContext::is_rust_dependency_method`]) is held to the
//! project types the calling file names (`dependency_names`).
//!
//! Two std lists back this:
//!
//! - [`STD_METHOD_NAMES`], generated from the toolchain's library source
//!   (`tests/std_method_names.rs` regenerates it), for receivers of unknown
//!   type;
//! - [`COMMON_STD_METHOD_NAMES`], the hand-picked names std defines on nearly
//!   every type, for a receiver whose project type is known but lacks the
//!   method (a derive or blanket impl supplies `clone`, `default`, `fmt`):
//!   a rarer name there still reaches the remaining strategies.

use super::std_method_names::STD_METHOD_NAMES;
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{EdgeKind, Language, receiver_was_dropped};

/// Method names std and its core traits define across many types (iterators,
/// collections, `Option`/`Result`, strings, `Clone`/`Ord`/`Hash`/`Default`).
pub(super) const COMMON_STD_METHOD_NAMES: &[&str] = &[
    "all",
    "and_then",
    "any",
    "as_bytes",
    "as_mut",
    "as_ref",
    "as_str",
    "borrow",
    "borrow_mut",
    "chain",
    "chars",
    "clone",
    "cloned",
    "cmp",
    "collect",
    "contains",
    "contains_key",
    "copied",
    "count",
    "default",
    "drain",
    "ends_with",
    "enumerate",
    "eq",
    "err",
    "expect",
    "extend",
    "filter",
    "filter_map",
    "find",
    "first",
    "flat_map",
    "fmt",
    "fold",
    "for_each",
    "from",
    "get",
    "get_mut",
    "hash",
    "insert",
    "into",
    "into_iter",
    "is_empty",
    "is_err",
    "is_none",
    "is_ok",
    "is_some",
    "iter",
    "iter_mut",
    "join",
    "keys",
    "last",
    "len",
    "lines",
    "lock",
    "map",
    "max",
    "min",
    "ne",
    "new",
    "next",
    "ok",
    "ok_or",
    "or_else",
    "parse",
    "partial_cmp",
    "pop",
    "push",
    "read",
    "recv",
    "remove",
    "replace",
    "retain",
    "rev",
    "send",
    "skip",
    "sort",
    "sort_by",
    "sort_by_key",
    "split",
    "starts_with",
    "sum",
    "take",
    "to_owned",
    "to_string",
    "trim",
    "unwrap",
    "unwrap_or",
    "unwrap_or_default",
    "unwrap_or_else",
    "values",
    "with_capacity",
    "write",
    "zip",
];

/// `name` is one of [`COMMON_STD_METHOD_NAMES`].
pub(super) fn is_common_std_method_name(name: &str) -> bool {
    COMMON_STD_METHOD_NAMES.binary_search(&name).is_ok()
}

/// `name` is a method std, core, or alloc define on some type or trait
/// ([`STD_METHOD_NAMES`]).
pub(super) fn is_std_method_name(name: &str) -> bool {
    STD_METHOD_NAMES.binary_search(&name).is_ok()
}

/// A Rust call to a std method name whose receiver was dropped: its target
/// is a method on a type name matching cannot see.
pub(super) fn is_receiverless_std_method_call(reference: &UnresolvedRef) -> bool {
    is_dropped_receiver_call(reference) && is_std_method_name(&reference.reference_name)
}

/// A Rust call whose receiver was dropped to a method a direct dependency
/// of the project defines (`crate::resolution::rust_deps`): its target may
/// be on a type name matching cannot see.
pub(super) fn is_receiverless_dependency_method_call(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> bool {
    is_dropped_receiver_call(reference)
        && context.is_rust_dependency_method(&reference.reference_name)
}

fn is_dropped_receiver_call(reference: &UnresolvedRef) -> bool {
    reference.language == Language::Rust
        && reference.reference_kind == EdgeKind::Calls
        && receiver_was_dropped(reference.metadata.as_ref())
}

#[cfg(test)]
mod tests {
    use super::{COMMON_STD_METHOD_NAMES, STD_METHOD_NAMES, is_std_method_name};

    /// `binary_search` needs both lists sorted and free of duplicates.
    #[test]
    fn std_method_name_lists_are_sorted_and_unique() {
        for (name, list) in [
            ("COMMON_STD_METHOD_NAMES", COMMON_STD_METHOD_NAMES),
            ("STD_METHOD_NAMES", STD_METHOD_NAMES),
        ] {
            assert!(
                list.windows(2).all(|pair| pair[0] < pair[1]),
                "keep {name} sorted and unique"
            );
        }
    }

    /// The hand list only narrows the generated one.
    #[test]
    fn common_std_method_names_are_std_method_names() {
        let missing: Vec<&str> = COMMON_STD_METHOD_NAMES
            .iter()
            .copied()
            .filter(|name| !is_std_method_name(name))
            .collect();
        assert!(missing.is_empty(), "not std methods: {missing:?}");
    }

    /// Names std defines beyond the hand list, which receivers of unknown
    /// type used to resolve by name alone.
    #[test]
    fn generated_names_cover_std_types_and_traits() {
        for name in [
            "count_ones",
            "fetch_add",
            "file_name",
            "into_inner",
            "parent",
            "spawn",
            "stderr",
            "write_all",
        ] {
            assert!(is_std_method_name(name), "{name}");
        }
        for name in ["get_outgoing_edges", "resolve_all", "stop"] {
            assert!(!is_std_method_name(name), "{name}");
        }
    }
}
