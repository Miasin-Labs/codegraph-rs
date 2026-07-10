//! Cache sizing and snapshot threshold configuration.

use super::ReferenceResolver;
use crate::error::log_warn;

/// Cache size limits. Each per-resolver cache is bounded so memory
/// stays flat on large codebases (20k+ files). Sizes were chosen to
/// cover the working set for typical resolution batches without
/// exceeding a few hundred MB worst-case. Override via the env var
/// `CODEGRAPH_RESOLVER_CACHE_SIZE` (single integer applied to all
/// caches) when tuning for very large or very small projects.
const DEFAULT_CACHE_LIMIT: usize = 5_000;

/// Mirrors JS `Number.parseInt(raw, 10)`: skip leading whitespace, allow an
/// optional sign, parse the leading run of decimal digits, NaN otherwise.
pub(super) fn parse_int_prefix(raw: &str) -> Option<i64> {
    let s = raw.trim_start();
    let (sign, rest) = match s.strip_prefix('-') {
        Some(r) => (-1i64, r),
        None => (1i64, s.strip_prefix('+').unwrap_or(s)),
    };
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits.parse::<i64>().ok().map(|n| sign * n)
}

pub(super) fn resolve_cache_limit() -> usize {
    let raw = match std::env::var("CODEGRAPH_RESOLVER_CACHE_SIZE") {
        Ok(v) => v,
        Err(_) => return DEFAULT_CACHE_LIMIT,
    };
    if raw.is_empty() {
        // JS `if (!raw)` — empty string is falsy.
        return DEFAULT_CACHE_LIMIT;
    }
    match parse_int_prefix(&raw) {
        Some(parsed) if parsed > 0 => parsed as usize,
        _ => DEFAULT_CACHE_LIMIT,
    }
}

impl ReferenceResolver {
    /// Pre-build lightweight caches for resolution.
    /// Node lookups are now handled by indexed SQLite queries instead of
    /// loading all nodes into memory (which caused OOM on large codebases).
    /// We cache the set of known symbol names for fast pre-filtering.
    pub fn warm_caches(&self) {
        if self.context.caches_warmed.get() {
            return;
        }

        match self.context.queries.get_all_file_paths() {
            Ok(paths) => {
                *self.context.known_files.borrow_mut() = Some(paths.into_iter().collect());
            }
            Err(error) => {
                log_warn(
                    "Failed to warm known-files cache",
                    Some(&serde_json::json!({ "error": error.to_string() })),
                );
                *self.context.known_files.borrow_mut() = None;
            }
        }

        match self.context.queries.get_all_node_names() {
            Ok(names) => {
                *self.context.known_names.borrow_mut() = Some(names.into_iter().collect());
            }
            Err(error) => {
                log_warn(
                    "Failed to warm known-names cache",
                    Some(&serde_json::json!({ "error": error.to_string() })),
                );
                *self.context.known_names.borrow_mut() = None;
            }
        }

        self.context.caches_warmed.set(true);
    }

    /// Clear internal caches
    pub fn clear_caches(&self) {
        self.context.clear_caches();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_prefix_mirrors_js_parse_int() {
        assert_eq!(parse_int_prefix("123"), Some(123));
        assert_eq!(parse_int_prefix("  42  "), Some(42));
        assert_eq!(parse_int_prefix("123abc"), Some(123));
        assert_eq!(parse_int_prefix("-7"), Some(-7));
        assert_eq!(parse_int_prefix("+9"), Some(9));
        assert_eq!(parse_int_prefix("abc"), None);
        assert_eq!(parse_int_prefix(""), None);
        assert_eq!(parse_int_prefix("0"), Some(0));
    }
}
