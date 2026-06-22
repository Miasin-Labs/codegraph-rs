use std::sync::LazyLock;

use regex::Regex;
use rusqlite::types::Value;

use super::super::rows::placeholders;
use crate::extraction::generated_detection::is_generated_file;
use crate::types::{EdgeKind, Language, NodeKind};

const NAME_SUBSTRING_CANDIDATE_MULTIPLIER: usize = 20;
const NAME_SUBSTRING_MAX_SORT_CANDIDATES: usize = 2_000;

fn unique_filters<T: Copy + Eq>(filters: &[T]) -> Vec<T> {
    let mut unique = Vec::new();
    for filter in filters {
        if !unique.contains(filter) {
            unique.push(*filter);
        }
    }
    unique
}

pub(super) fn intersect_filter_axis<T: Copy + Eq>(
    option_filters: Option<&[T]>,
    query_filters: &[T],
) -> std::result::Result<Option<Vec<T>>, ()> {
    let option_filters = option_filters.filter(|filters| !filters.is_empty());
    let query_filters = if query_filters.is_empty() {
        None
    } else {
        Some(query_filters)
    };

    match (option_filters, query_filters) {
        (None, None) => Ok(None),
        (Some(filters), None) | (None, Some(filters)) => Ok(Some(unique_filters(filters))),
        (Some(options), Some(query)) => {
            let mut intersection = Vec::new();
            for filter in options {
                if query.contains(filter) && !intersection.contains(filter) {
                    intersection.push(*filter);
                }
            }
            if intersection.is_empty() {
                Err(())
            } else {
                Ok(Some(intersection))
            }
        }
    }
}

pub(super) fn push_kind_filter(
    sql: &mut String,
    params: &mut Vec<Value>,
    col_prefix: &str,
    kinds: Option<&[NodeKind]>,
) {
    if let Some(kinds) = kinds {
        if !kinds.is_empty() {
            sql.push_str(&format!(
                " AND {}kind IN ({})",
                col_prefix,
                placeholders(kinds.len())
            ));
            for k in kinds {
                params.push(Value::Text(k.as_str().to_string()));
            }
        }
    }
}

pub(super) fn push_language_filter(
    sql: &mut String,
    params: &mut Vec<Value>,
    col_prefix: &str,
    languages: Option<&[Language]>,
) {
    if let Some(languages) = languages {
        if !languages.is_empty() {
            sql.push_str(&format!(
                " AND {}language IN ({})",
                col_prefix,
                placeholders(languages.len())
            ));
            for l in languages {
                params.push(Value::Text(l.as_str().to_string()));
            }
        }
    }
}

pub(in crate::db::queries::statements) fn push_edge_kind_filter(
    sql: &mut String,
    params: &mut Vec<Value>,
    kinds: Option<&[EdgeKind]>,
) {
    if let Some(kinds) = kinds {
        if !kinds.is_empty() {
            sql.push_str(&format!(" AND kind IN ({})", placeholders(kinds.len())));
            for k in kinds {
                params.push(Value::Text(k.as_str().to_string()));
            }
        }
    }
}

pub(super) fn name_substring_candidate_limit(result_limit: usize) -> usize {
    result_limit
        .saturating_mul(NAME_SUBSTRING_CANDIDATE_MULTIPLIER)
        .min(NAME_SUBSTRING_MAX_SORT_CANDIDATES)
}

// =============================================================================
// Inline helpers — defined inline in TS `src/db/queries.ts` as well (they are
// NOT part of the shared search/extraction modules).
// =============================================================================

/// TS `isLowValueFile` patterns, applied to the lowercased path.
static LOW_VALUE_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"(?:^|/)(tests?|__tests?__|spec)/",
        r"_test\.go$",
        r"(?:^|/)test_[^/]+\.py$",
        r"_test\.py$",
        r"_spec\.rb$",
        r"_test\.rb$",
        r"\.(test|spec)\.[jt]sx?$",
        r"(test|spec|tests)\.(java|kt|scala)$",
        r"(tests?|spec)\.cs$",
        r"tests?\.swift$",
        r"_test\.dart$",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("static regex"))
    .collect()
});

/// Path-only heuristic for files that should not be candidates for
/// "dominant file" detection: test/spec files and tool-generated files.
/// Generated files (`*.pb.go`, `*.pulsar.go`, mock outputs, …) often
/// have huge in-file edge counts that dwarf the real source — etcd's
/// `rpc.pb.go` has 4× the in-file edges of `server.go`.
pub(in crate::db::queries::statements) fn is_low_value_file(file_path: &str) -> bool {
    let lp = file_path.to_lowercase();
    LOW_VALUE_PATTERNS.iter().any(|p| p.is_match(&lp)) || is_generated_file(file_path)
}

static FTS_OPERATOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(AND|OR|NOT|NEAR)$").expect("static regex"));

/// Whether a term is an FTS5 boolean operator (stripped to prevent query
/// manipulation) — TS inline `/^(AND|OR|NOT|NEAR)$/i`.
pub(super) fn is_fts_operator(term: &str) -> bool {
    FTS_OPERATOR_RE.is_match(term)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    //! Coverage for the helpers defined inline in this file. The shared
    //! search/extraction helpers are tested where they live:
    //! `crate::search::query_parser`, `crate::search::query_utils`,
    //! `crate::extraction::generated_detection`.

    use super::{is_fts_operator, is_low_value_file, name_substring_candidate_limit};
    use crate::extraction::generated_detection::is_generated_file;

    #[test]
    fn low_value_and_generated_file_detection() {
        // Test/spec paths
        assert!(is_low_value_file("src/__tests__/foo.ts"));
        assert!(is_low_value_file("pkg/server_test.go"));
        assert!(is_low_value_file("lib/foo.spec.tsx"));
        // Generated protobuf stubs (the etcd rpc.pb.go case from the TS docs)
        assert!(is_low_value_file("api/etcdserverpb/rpc.pb.go"));
        assert!(is_generated_file("gen/types.pb.go"));
        assert!(is_generated_file("client_grpc_pb.js"));
        // Real source survives
        assert!(!is_low_value_file("server/etcdserver/server.go"));
        assert!(!is_generated_file("src/db/queries.rs"));
    }

    #[test]
    fn fts_operator_detection_case_insensitive() {
        assert!(is_fts_operator("AND"));
        assert!(is_fts_operator("or"));
        assert!(is_fts_operator("Near"));
        assert!(!is_fts_operator("Andrew"));
    }

    #[test]
    fn name_substring_candidate_window_is_bounded() {
        assert_eq!(name_substring_candidate_limit(1), 20);
        assert_eq!(name_substring_candidate_limit(30), 600);
        assert_eq!(name_substring_candidate_limit(200), 2_000);
        assert_eq!(name_substring_candidate_limit(10_000), 2_000);
    }
}
