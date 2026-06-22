//! Shared MCP regexes and input bounds.

use std::sync::LazyLock;

use regex::Regex;

/// Maximum length for free-form string inputs (query, task, symbol).
/// Bounds memory and CPU when a buggy or hostile MCP client sends a
/// huge payload — without this an attacker could ship a 100MB string
/// and force a full FTS5 scan / OOM the server. 10 000 characters is
/// far beyond any realistic legitimate query.
pub(in crate::mcp::tools) const MAX_INPUT_LENGTH: usize = 10_000;

/// Maximum length for path-like string inputs (projectPath, path
/// filter, glob pattern). Paths beyond a few thousand chars are
/// never legitimate and signal abuse or a bug upstream.
pub(in crate::mcp::tools) const MAX_PATH_LENGTH: usize = 4_096;

/// Rust path roots that have no file-system equivalent — `crate` is the
/// current crate, `super` is the parent module, `self` is the current
/// module. Used by `matches_symbol` to strip these before file-path
/// matching so `crate::configurator::stage_apply::run` resolves the
/// same as `configurator::stage_apply::run`.
pub(in crate::mcp::tools) const RUST_PATH_PREFIXES: [&str; 3] = ["crate", "super", "self"];

// =============================================================================
// Shared regexes (JS regex literals → compiled once)
// =============================================================================

pub(in crate::mcp::tools) static QUALIFIER_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"::|[./]").unwrap());
pub(in crate::mcp::tools) static QUAL_DOT_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"::|\.").unwrap());
pub(in crate::mcp::tools) static TOKEN_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\s,()\[\]]+").unwrap());
pub(in crate::mcp::tools) static FILE_EXT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\.(?:java|kt|kts|ts|tsx|js|jsx|mjs|cjs|cs|py|go|rb|php|swift|rs|cpp|cc|cxx|c|h|hpp|scala|lua|dart|vue|svelte)$").unwrap()
});
pub(in crate::mcp::tools) static TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z_$][A-Za-z0-9_$]*(?:(?:::|\.)[A-Za-z0-9_$]+)*$").unwrap()
});
pub(in crate::mcp::tools) static TYPE_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Z][A-Za-z0-9]{3,}").unwrap());
pub(in crate::mcp::tools) static TEST_PATH_DIR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|/)(tests?|specs?|__tests__|testdata|mocks?|fixtures?)/").unwrap()
});
pub(in crate::mcp::tools) static TEST_PATH_EXT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(test|spec)\.[a-z]+$").unwrap());
pub(in crate::mcp::tools) static QUERY_MENTIONS_TESTS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(test|tests|testing|spec|verify|verifies)\b").unwrap());
pub(in crate::mcp::tools) static EXT_STRIP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.[^.]+$").unwrap());
pub(in crate::mcp::tools) static LEADING_DOT_SLASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\.?/+)+").unwrap());
pub(in crate::mcp::tools) static LOW_VALUE_RES: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"/(tests?|__tests?__|spec)/",
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
        r"\bicons?\b",
        r"\bi18n\b",
    ]
    .iter()
    .map(|p| Regex::new(p).unwrap())
    .collect()
});
