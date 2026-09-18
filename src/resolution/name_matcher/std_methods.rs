//! Rust method calls that name matching must leave unresolved.
//!
//! A method call on a chained or field receiver (`v.iter().next()`,
//! `self.map.get(k)`) reaches resolution as its bare method name, flagged as
//! having dropped its receiver. When that name is one std and its traits
//! define on nearly every type, the call almost always runs a std method, and
//! the only project symbol name matching can offer is an unrelated same-named
//! one (`.next()` landing on `FrontierIter::next`, `.cmp()` on
//! `PathState::cmp`). Method-call syntax never names a free function, field,
//! or variable either, so no name-matched target is right: the reference
//! stays unresolved. Project-specific names (`.get_outgoing_edges()`) keep
//! resolving as before.

use crate::resolution::types::UnresolvedRef;
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

/// A Rust call to a common std method whose receiver was dropped: its target
/// is a method on a type name matching cannot see.
pub(super) fn is_receiverless_std_method_call(reference: &UnresolvedRef) -> bool {
    reference.language == Language::Rust
        && reference.reference_kind == EdgeKind::Calls
        && receiver_was_dropped(reference.metadata.as_ref())
        && COMMON_STD_METHOD_NAMES
            .binary_search(&reference.reference_name.as_str())
            .is_ok()
}

#[cfg(test)]
mod tests {
    use super::COMMON_STD_METHOD_NAMES;

    /// `binary_search` needs the list sorted and free of duplicates.
    #[test]
    fn common_std_method_names_are_sorted_and_unique() {
        assert!(
            COMMON_STD_METHOD_NAMES
                .windows(2)
                .all(|pair| pair[0] < pair[1]),
            "keep COMMON_STD_METHOD_NAMES sorted and unique"
        );
    }
}
