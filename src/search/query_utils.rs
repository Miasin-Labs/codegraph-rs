//! Search Query Utilities
//!
//! Shared module for search term extraction and scoring.

use std::collections::HashSet;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::types::NodeKind;

/// Common stop words to filter from search queries.
/// Includes generic English + code-specific noise words.
pub static STOP_WORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    [
        // English
        "the",
        "a",
        "an",
        "and",
        "or",
        "but",
        "in",
        "on",
        "at",
        "to",
        "for",
        "of",
        "with",
        "by",
        "from",
        "is",
        "it",
        "that",
        "this",
        "are",
        "was",
        "be",
        "has",
        "had",
        "have",
        "do",
        "does",
        "did",
        "will",
        "would",
        "could",
        "should",
        "may",
        "might",
        "can",
        "shall",
        "not",
        "no",
        "all",
        "each",
        "every",
        "how",
        "what",
        "where",
        "when",
        "who",
        "which",
        "why",
        "i",
        "me",
        "my",
        "we",
        "our",
        "you",
        "your",
        "he",
        "she",
        "they",
        "show",
        "give",
        "tell",
        "been",
        "done",
        "made",
        "used",
        "using",
        "work",
        "works",
        "found",
        "also",
        "into",
        "then",
        "than",
        "just",
        "more",
        "some",
        "such",
        "over",
        "only",
        "out",
        "its",
        "so",
        "up",
        "as",
        "if",
        "look",
        "need",
        "needs",
        "want",
        "happen",
        "happens",
        "affect",
        "affected",
        "break",
        "breaks",
        "failing",
        "implemented",
        "implement",
        // Code-specific noise (avoid filtering common symbol names like get/set/add/build/find/list)
        "code",
        "file",
        "files",
        "function",
        "method",
        "class",
        "type",
        "fix",
        "bug",
        "called",
    ]
    .into_iter()
    .collect()
});

// CamelCase compound identifiers: scrapeLoop, UserService, getCallGraph.
// `(?-u:\b)` is the ASCII word boundary — matches JS `\b` semantics
// (JS `\w` is ASCII-only).
static COMPOUND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?-u:\b)([a-zA-Z][a-zA-Z0-9]*(?:[A-Z][a-z]+)+|[A-Z][a-z]+(?:[A-Z][a-z]*)+)(?-u:\b)",
    )
    .unwrap()
});

// snake_case: scrape_loop, user_service
static SNAKE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)([a-zA-Z][a-zA-Z0-9]*(?:_[a-zA-Z0-9]+)+)(?-u:\b)").unwrap()
});

// camelCase boundary: "getUserName" → "get User Name" (first pass)
static CAMEL_BOUNDARY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([a-z])([A-Z])").unwrap());

// Acronym boundary: "HTTPServer" → "HTTP Server"
static ACRONYM_BOUNDARY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"([A-Z]+)([A-Z][a-z])").unwrap());

// Underscores and dots → spaces (snake_case, dot.notation)
static UNDERSCORE_DOT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[_.]+").unwrap());

// Split on any non-alphanumeric character
static NON_ALNUM_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^a-zA-Z0-9]+").unwrap());

// Word-level separators for nameMatchBonus: whitespace, _, ., -
static WORD_SEP_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[\s_.\-]+").unwrap());

// Whitespace runs
static WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").unwrap());

// Filename: separator-delimited test suffix — foo_test.go, foo.test.ts,
// foo-spec.rb, bar_spec.py (checked against the lowercased filename).
static SEP_TEST_FILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[._-](test|tests|spec|specs)\.[a-z0-9]+$").unwrap());

// Filename: CamelCase suffix (Java/Kotlin/Swift/C#/Scala): FooTest.kt,
// BarTests.swift, BazSpec.scala, QuxTestCase.java. Capital-led so
// "latest.kt"/"manifest.kt" (lowercase "test") are NOT matched.
static CAMEL_TEST_FILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:Test|Tests|TestCase|Tester|Spec|Specs)\.[A-Za-z0-9]+$").unwrap()
});

// CamelCase test source-set dirs (Kotlin Multiplatform / Gradle / Xcode):
// jvmTest/, commonTest/, androidTest/, iosTest/, integrationTest/. Capital-led
// so "latest/" / "manifest/" are not matched.
static CAMEL_TEST_DIR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|/)[A-Za-z0-9]*(?:Test|Tests|Spec)/").unwrap());

/// UTF-16 code-unit length — parity with JS `String.prototype.length`.
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// Node `path.basename` (posix semantics): last path segment, trailing
/// separators ignored. Empty string when there is no segment.
fn basename(p: &str) -> &str {
    Path::new(p)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or("")
}

/// Node `path.dirname` (posix semantics): everything before the last
/// segment; `"."` when there is no directory portion.
fn dirname(p: &str) -> &str {
    match Path::new(p).parent().and_then(|d| d.to_str()) {
        None | Some("") => ".",
        Some(d) => d,
    }
}

fn add_variant(variants: &mut Vec<String>, v: String) {
    if !variants.contains(&v) {
        variants.push(v);
    }
}

/// Generate stem variants of a search term by removing common English suffixes.
/// Used for FTS query expansion so "caching" also finds "cache", "eviction" finds "evict", etc.
/// Stems are used as PREFIX matches in FTS, so they don't need to be perfect English words.
pub fn get_stem_variants(term: &str) -> Vec<String> {
    let mut variants: Vec<String> = Vec::new();
    let t = term.to_lowercase();
    let len = t.chars().count();

    // -ing: caching→cach/cache, handling→handl/handle, running→run
    if t.ends_with("ing") && len > 5 {
        let base = &t[..t.len() - 3];
        add_variant(&mut variants, base.to_string());
        add_variant(&mut variants, format!("{base}e"));
        let bchars: Vec<char> = base.chars().collect();
        if bchars.len() >= 2 && bchars[bchars.len() - 1] == bchars[bchars.len() - 2] {
            add_variant(&mut variants, bchars[..bchars.len() - 1].iter().collect());
        }
    }

    // -tion/-sion: eviction→evict, expression→express
    if (t.ends_with("tion") || t.ends_with("sion")) && len > 5 {
        add_variant(&mut variants, t[..t.len() - 3].to_string());
    }

    // -ment: management→manage
    if t.ends_with("ment") && len > 6 {
        add_variant(&mut variants, t[..t.len() - 4].to_string());
    }

    // -ies: entries→entry
    if t.ends_with("ies") && len > 4 {
        add_variant(&mut variants, format!("{}y", &t[..t.len() - 3]));
    }
    // -es: processes→process, classes→class
    else if t.ends_with("es") && len > 4 {
        add_variant(&mut variants, t[..t.len() - 2].to_string());
    }
    // -s: errors→error (skip -ss endings like "class")
    else if t.ends_with('s') && !t.ends_with("ss") && len > 4 {
        add_variant(&mut variants, t[..t.len() - 1].to_string());
    }

    // -ed: handled→handle, propagated→propagate, carried→carry
    if t.ends_with("ed") && !t.ends_with("eed") && len > 4 {
        add_variant(&mut variants, t[..t.len() - 1].to_string());
        add_variant(&mut variants, t[..t.len() - 2].to_string());
        if t.ends_with("ied") && len > 5 {
            add_variant(&mut variants, format!("{}y", &t[..t.len() - 3]));
        }
    }

    // -er: builder→build/builde, handler→handl/handle, getter→get
    if t.ends_with("er") && len > 4 {
        let base = &t[..t.len() - 2];
        add_variant(&mut variants, base.to_string());
        add_variant(&mut variants, format!("{base}e"));
        let bchars: Vec<char> = base.chars().collect();
        if bchars.len() >= 2 && bchars[bchars.len() - 1] == bchars[bchars.len() - 2] {
            add_variant(&mut variants, bchars[..bchars.len() - 1].iter().collect());
        }
    }

    variants
        .into_iter()
        .filter(|v| v.chars().count() >= 3 && *v != t)
        .collect()
}

fn push_unique(tokens: &mut Vec<String>, seen: &mut HashSet<String>, t: String) {
    if seen.insert(t.clone()) {
        tokens.push(t);
    }
}

/// Extract meaningful search terms from a natural language query.
/// Splits camelCase, PascalCase, snake_case, SCREAMING_SNAKE, and dot.notation
/// into individual tokens before filtering.
///
/// Preserves original compound identifiers (e.g., "scrapeLoop") alongside
/// their split parts so that FTS can match both the full symbol name and
/// individual words within it.
///
/// Also generates stem variants (e.g., "caching"→"cache", "eviction"→"evict")
/// so FTS prefix matching can find related code symbols.
///
/// Stem generation is on by default (TS `options?.stems !== false`).
pub fn extract_search_terms(query: &str) -> Vec<String> {
    extract_search_terms_opts(query, true)
}

/// `extract_search_terms` with explicit control over stem-variant expansion
/// (TS `extractSearchTerms(query, { stems: false })`).
pub fn extract_search_terms_opts(query: &str, include_stems: bool) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    // First, extract and preserve compound identifiers before splitting
    // CamelCase: scrapeLoop, UserService, getCallGraph
    for cap in COMPOUND_RE.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            let s = m.as_str();
            if !s.is_empty() && s.len() >= 3 {
                // preserve full compound: "scrapeloop"
                push_unique(&mut tokens, &mut seen, s.to_lowercase());
            }
        }
    }

    // snake_case: scrape_loop, user_service
    for cap in SNAKE_RE.captures_iter(query) {
        if let Some(m) = cap.get(1) {
            let s = m.as_str();
            if !s.is_empty() && s.len() >= 3 {
                push_unique(&mut tokens, &mut seen, s.to_lowercase());
            }
        }
    }

    // Split camelCase / PascalCase: "getUserName" → "get User Name"
    let step1 = CAMEL_BOUNDARY_RE.replace_all(query, "${1} ${2}");
    let camel_split = ACRONYM_BOUNDARY_RE.replace_all(&step1, "${1} ${2}");

    // Replace underscores and dots with spaces (snake_case, dot.notation)
    let normalised = UNDERSCORE_DOT_RE.replace_all(&camel_split, " ");

    // Split on any non-alphanumeric character
    for word in NON_ALNUM_SPLIT_RE
        .split(&normalised)
        .filter(|w| !w.is_empty())
    {
        let lower = word.to_lowercase();
        // Words are ASCII alphanumeric (everything else was a separator),
        // so byte length equals JS string length here.
        if lower.len() < 3 {
            continue;
        }
        if STOP_WORDS.contains(lower.as_str()) {
            continue;
        }
        push_unique(&mut tokens, &mut seen, lower);
    }

    // Generate stem variants for broader FTS matching.
    // "caching" → "cache" finds CacheBuilder; "eviction" → "evict" finds evictEntries.
    // Also enables co-occurrence dampening by increasing term count above 1.
    // Stems are skipped when scoring path relevance (stems inflate path scores).
    if include_stems {
        let mut stems: Vec<String> = Vec::new();
        let mut stem_seen: HashSet<String> = HashSet::new();
        for token in &tokens {
            for variant in get_stem_variants(token) {
                if !seen.contains(&variant)
                    && !STOP_WORDS.contains(variant.as_str())
                    && stem_seen.insert(variant.clone())
                {
                    stems.push(variant);
                }
            }
        }
        for stem in stems {
            push_unique(&mut tokens, &mut seen, stem);
        }
    }

    tokens
}

/// Score path relevance to a query
/// Higher score = more relevant path
pub fn score_path_relevance(file_path: &str, query: &str) -> i32 {
    // Use base terms only — stem variants inflate path scores by generating
    // many near-duplicate terms that all match the same path segments.
    let terms = extract_search_terms_opts(query, false);
    if terms.is_empty() {
        return 0;
    }

    let path_lower = file_path.to_lowercase();
    let file_name = basename(file_path).to_lowercase();
    let dir_name = dirname(file_path).to_lowercase();
    let mut score: i32 = 0;

    for term in &terms {
        // Exact filename match (strongest)
        if file_name.contains(term.as_str()) {
            score += 10;
        }
        // Directory match
        if dir_name.contains(term.as_str()) {
            score += 5;
        }
        // General path match
        else if path_lower.contains(term.as_str()) {
            score += 3;
        }
    }

    // Deprioritize test files unless the query is explicitly about tests
    let query_lower = query.to_lowercase();
    let is_test_query = query_lower.contains("test") || query_lower.contains("spec");
    if !is_test_query && is_test_file(file_path) {
        score -= 15;
    }

    score
}

/// Check if a file path looks like a test file
pub fn is_test_file(file_path: &str) -> bool {
    let lower = file_path.to_lowercase();
    let file_name = basename(file_path); // original case — needed for camelCase boundaries
    let lower_name = file_name.to_lowercase();

    // --- Filename patterns ---
    if lower_name.starts_with("test_")                            // python: test_foo.py
        || lower_name.starts_with("test.")
        // separator-delimited: foo_test.go, foo.test.ts, foo-spec.rb, bar_spec.py
        || SEP_TEST_FILE_RE.is_match(&lower_name)
        // CamelCase suffix (Java/Kotlin/Swift/C#/Scala): FooTest.kt, BarTests.swift,
        // BazSpec.scala, QuxTestCase.java. Capital-led so "latest.kt"/"manifest.kt"
        // (lowercase "test") are NOT matched.
        || CAMEL_TEST_FILE_RE.is_match(file_name)
    {
        return true;
    }

    // --- Directory patterns ---
    if lower.contains("/tests/")
        || lower.contains("/test/")
        || lower.contains("/__tests__/")
        || lower.contains("/spec/")
        || lower.contains("/specs/")
        || lower.contains("/testlib/")
        || lower.contains("/testing/")
        || lower.starts_with("test/")
        || lower.starts_with("tests/")
        || lower.starts_with("spec/")
        || lower.starts_with("specs/")
        // CamelCase test source-set dirs (Kotlin Multiplatform / Gradle / Xcode):
        // jvmTest/, commonTest/, androidTest/, iosTest/, integrationTest/. Capital-led
        // so "latest/" / "manifest/" are not matched.
        || CAMEL_TEST_DIR_RE.is_match(file_path)
    {
        return true;
    }

    // Non-production directories: examples, samples, benchmarks, fixtures, demos.
    // Check both mid-path (/integration/) and start-of-path (integration/) since
    // file paths may be stored as relative paths without a leading slash.
    matches_non_production_dir(&lower)
}

/// Check if a path is in a non-production directory (integration, sample, example, etc.)
/// Handles both absolute paths (/foo/integration/bar) and relative paths (integration/bar).
fn matches_non_production_dir(lower_path: &str) -> bool {
    const DIRS: [&str; 11] = [
        "integration",
        "sample",
        "samples",
        "example",
        "examples",
        "fixture",
        "fixtures",
        "benchmark",
        "benchmarks",
        "demo",
        "demos",
    ];
    for dir in DIRS {
        if lower_path.contains(&format!("/{dir}/")) || lower_path.starts_with(&format!("{dir}/")) {
            return true;
        }
    }
    false
}

/// Bonus when a node's name matches the search query.
/// Exact matches get the largest boost; prefix matches get smaller boosts.
/// Multi-word queries also check individual term matches against the name.
pub fn name_match_bonus(node_name: &str, query: &str) -> i32 {
    let name_lower = node_name.to_lowercase();

    // Split query into word-level terms (handles "CacheBuilder build" → ["cache","builder","build"])
    let camel_separated = CAMEL_BOUNDARY_RE.replace_all(query, "${1} ${2}");
    let raw_terms: Vec<String> = WORD_SEP_RE
        .split(&camel_separated)
        .map(|t| t.to_lowercase())
        .filter(|t| utf16_len(t) >= 2)
        .collect();

    // Also keep original space-separated tokens for exact-term matching
    let query_tokens: Vec<String> = WS_RE
        .split(query)
        .map(|t| t.to_lowercase())
        .filter(|t| utf16_len(t) >= 2)
        .collect();

    // Full query as a single token (for compound identifiers like "CacheBuilder")
    let query_lower = WS_RE.replace_all(query, "").to_lowercase();

    // Exact match: query exactly equals the node name
    if name_lower == query_lower {
        return 80;
    }

    // Exact match on a query token: "CacheBuilder build" and node name is "build"
    if query_tokens.len() > 1 && query_tokens.contains(&name_lower) {
        return 60;
    }

    // Name starts with query — scale by length ratio so "Pod"→"Pod" (exact, handled above)
    // scores much higher than "Pod"→"PodGCControllerOptions" (ratio 0.125).
    if name_lower.starts_with(query_lower.as_str()) {
        let ratio = utf16_len(&query_lower) as f64 / utf16_len(&name_lower) as f64;
        return (10.0 + 30.0 * ratio).round() as i32;
    }

    // All camelCase-split terms appear in the name
    if raw_terms.len() > 1 && raw_terms.iter().all(|t| name_lower.contains(t.as_str())) {
        return 15;
    }

    // Name contains the full query as substring
    if name_lower.contains(query_lower.as_str()) {
        return 10;
    }

    0
}

/// Kind-based bonus for search ranking
/// Functions and classes are typically more relevant than variables/imports
pub fn kind_bonus(kind: NodeKind) -> i32 {
    match kind {
        NodeKind::Function => 10,
        NodeKind::Method => 10,
        NodeKind::Class => 8,
        NodeKind::Interface => 9,
        NodeKind::TypeAlias => 6,
        NodeKind::Struct => 6,
        NodeKind::Trait => 9,
        NodeKind::Enum => 5,
        NodeKind::Component => 8,
        NodeKind::Route => 9,
        NodeKind::Module => 4,
        NodeKind::Property => 3,
        NodeKind::Field => 3,
        NodeKind::Variable => 2,
        NodeKind::Constant => 3,
        NodeKind::Import => 1,
        NodeKind::Export => 1,
        NodeKind::Parameter => 0,
        NodeKind::Namespace => 4,
        NodeKind::File => 0,
        NodeKind::Protocol => 9,
        NodeKind::EnumMember => 3,
    }
}

/// Whether a query token looks like a code identifier the user deliberately typed
/// (camelCase / PascalCase-with-internal-caps / snake_case / has a digit) rather
/// than a plain dictionary word ("flat", "object", "screen").
///
/// Used to decide whether an EXACT name match earns the "the user named this
/// symbol" exemption from single-term dampening. A common English word that
/// happens to exact-match an unrelated symbol — the query "flat object" matching
/// a constant named `FLAT` — must NOT get that exemption, or the +exact-name
/// bonus floats it to the top of a prose query on its own.
///
/// Classifies the token AS THE USER TYPED IT, not the matched symbol's name:
/// "flat" (lowercase, descriptive) is non-distinctive even though it matches
/// `FLAT`. A leading-capital-only word ("Screen", "Zustand") is also treated as
/// a plain word — sentence-start capitalization and proper nouns aren't reliable
/// identifier signals.
pub fn is_distinctive_identifier(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    // snake_case / SCREAMING_SNAKE, or an embedded digit → a deliberate identifier.
    if token.chars().any(|c| c == '_' || c.is_ascii_digit()) {
        return true;
    }
    // An uppercase letter anywhere AFTER the first char → a camelCase/PascalCase
    // boundary (setLastEmail, OrgUserStore) or an acronym (REST, HTTP).
    if token.chars().skip(1).any(|c| c.is_ascii_uppercase()) {
        return true;
    }
    false
}

// =============================================================================
// Tests — supplemental coverage derived from the TS doc comments/examples
// (the TS suite has no dedicated query-utils test file).
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // === STOP_WORDS ===

    #[test]
    fn stop_words_contains_english_noise_but_not_common_symbol_names() {
        assert!(STOP_WORDS.contains("the"));
        assert!(STOP_WORDS.contains("function"));
        assert!(STOP_WORDS.contains("called"));
        // get/set/add/build/find/list deliberately NOT filtered
        assert!(!STOP_WORDS.contains("get"));
        assert!(!STOP_WORDS.contains("build"));
    }

    // === getStemVariants ===

    #[test]
    fn stems_ing_suffix() {
        let v = get_stem_variants("caching");
        assert!(v.contains(&"cach".to_string()));
        assert!(v.contains(&"cache".to_string()));
    }

    #[test]
    fn stems_doubled_consonant_ing() {
        let v = get_stem_variants("running");
        assert!(v.contains(&"run".to_string()));
    }

    #[test]
    fn stems_tion_suffix() {
        assert_eq!(get_stem_variants("eviction"), vec!["evict".to_string()]);
    }

    #[test]
    fn stems_ment_suffix() {
        assert!(get_stem_variants("management").contains(&"manage".to_string()));
    }

    #[test]
    fn stems_ies_suffix() {
        assert_eq!(get_stem_variants("entries"), vec!["entry".to_string()]);
    }

    #[test]
    fn stems_es_suffix() {
        assert!(get_stem_variants("classes").contains(&"class".to_string()));
    }

    #[test]
    fn stems_plural_s_but_not_ss() {
        assert!(get_stem_variants("errors").contains(&"error".to_string()));
        assert!(get_stem_variants("class").is_empty());
    }

    #[test]
    fn stems_ed_suffix_including_ied() {
        let v = get_stem_variants("carried");
        assert!(v.contains(&"carry".to_string()));
        let v = get_stem_variants("handled");
        assert!(v.contains(&"handle".to_string()));
    }

    #[test]
    fn stems_er_suffix() {
        let v = get_stem_variants("builder");
        assert!(v.contains(&"build".to_string()));
        let v = get_stem_variants("getter");
        assert!(v.contains(&"get".to_string()));
    }

    #[test]
    fn stems_filter_short_variants_and_self() {
        // Variants below 3 chars are dropped; the term itself never returned.
        for v in get_stem_variants("doing") {
            assert!(v.chars().count() >= 3);
            assert_ne!(v, "doing");
        }
    }

    // === extractSearchTerms ===

    #[test]
    fn extracts_compound_and_split_parts() {
        let terms = extract_search_terms("getUserName");
        assert!(terms.contains(&"getusername".to_string()));
        assert!(terms.contains(&"get".to_string()));
        assert!(terms.contains(&"user".to_string()));
        assert!(terms.contains(&"name".to_string()));
    }

    #[test]
    fn extracts_snake_case_compound_and_parts() {
        let terms = extract_search_terms("scrape_loop");
        assert!(terms.contains(&"scrape_loop".to_string()));
        assert!(terms.contains(&"scrape".to_string()));
        assert!(terms.contains(&"loop".to_string()));
    }

    #[test]
    fn filters_stop_words_and_short_words() {
        let terms = extract_search_terms("how does the cache work");
        assert_eq!(terms, vec!["cache".to_string()]);
    }

    #[test]
    fn includes_stem_variants_by_default_and_skips_when_disabled() {
        let with_stems = extract_search_terms("caching");
        assert!(with_stems.contains(&"caching".to_string()));
        assert!(with_stems.contains(&"cache".to_string()));

        let without = extract_search_terms_opts("caching", false);
        assert_eq!(without, vec!["caching".to_string()]);
    }

    #[test]
    fn splits_dot_notation() {
        let terms = extract_search_terms_opts("config.loader", false);
        assert!(terms.contains(&"config".to_string()));
        assert!(terms.contains(&"loader".to_string()));
    }

    // === scorePathRelevance ===

    #[test]
    fn scores_filename_directory_and_path_matches() {
        // dirName "src/cache" matches → +5
        assert_eq!(score_path_relevance("src/cache/builder.ts", "cache"), 5);
        // fileName match (+10) and dirName miss but pathLower hit (+3)
        assert_eq!(score_path_relevance("src/api/cache.ts", "cache"), 13);
    }

    #[test]
    fn returns_zero_for_no_terms() {
        assert_eq!(
            score_path_relevance("src/cache/builder.ts", "the of and"),
            0
        );
    }

    #[test]
    fn deprioritizes_test_files_for_non_test_queries() {
        // +10 (filename) +3 (path) -15 (test file) = -2
        assert_eq!(
            score_path_relevance("src/__tests__/cache.test.ts", "cache"),
            -2
        );
        // Explicit test query keeps the penalty off
        let s = score_path_relevance("src/__tests__/cache.test.ts", "cache test");
        assert!(s > 0);
    }

    // === isTestFile ===

    #[test]
    fn detects_test_filename_patterns() {
        assert!(is_test_file("test_foo.py"));
        assert!(is_test_file("src/foo_test.go"));
        assert!(is_test_file("src/foo.test.ts"));
        assert!(is_test_file("lib/foo-spec.rb"));
        assert!(is_test_file("bar_spec.py"));
    }

    #[test]
    fn detects_camelcase_test_filenames_capital_led_only() {
        assert!(is_test_file("src/FooTest.kt"));
        assert!(is_test_file("Sources/BarTests.swift"));
        assert!(is_test_file("src/BazSpec.scala"));
        assert!(is_test_file("src/QuxTestCase.java"));
        // lowercase "test" inside a word must NOT match
        assert!(!is_test_file("src/latest.kt"));
        assert!(!is_test_file("src/manifest.kt"));
    }

    #[test]
    fn detects_test_directories() {
        assert!(is_test_file("src/__tests__/foo.ts"));
        assert!(is_test_file("tests/foo.py"));
        assert!(is_test_file("spec/models/user_model.rb"));
        assert!(is_test_file("a/b/testing/helper.go"));
    }

    #[test]
    fn detects_camelcase_test_source_set_dirs() {
        assert!(is_test_file("src/jvmTest/kotlin/Foo.kt"));
        assert!(is_test_file("commonTest/kotlin/Foo.kt"));
        assert!(is_test_file("app/androidTest/Foo.kt"));
        // "latest/" must not match
        assert!(!is_test_file("src/latest/foo.kt"));
    }

    #[test]
    fn detects_non_production_dirs() {
        assert!(is_test_file("examples/demo.ts"));
        assert!(is_test_file("src/fixtures/data.json"));
        assert!(is_test_file("a/benchmarks/bench.rs"));
        assert!(is_test_file("integration/setup.ts"));
        assert!(!is_test_file("src/api/handler.ts"));
    }

    // === isTestFile — ported from __tests__/is-test-file.test.ts ===
    // Regression coverage for the cold-query fix: the heuristic previously
    // only knew Java/JS/Python conventions, so Kotlin (`*Test.kt`,
    // `jvmTest/`), Swift (`*Tests.swift`), and camelCase test source-set
    // dirs slipped through — which let OkHttp's tests flood
    // `codegraph_explore` results on a plain-language query. The
    // false-positive guards matter just as much: `latest.kt` /
    // `manifest.kt` / a `RealCall.kt` production file must NOT be flagged.

    #[test]
    fn flags_kotlin_test_files_and_source_sets() {
        assert!(is_test_file(
            "okhttp/src/jvmTest/kotlin/okhttp3/CallTest.kt"
        ));
        assert!(is_test_file(
            "okhttp/src/commonTest/kotlin/okhttp3/CompressionInterceptorTest.kt"
        ));
        assert!(is_test_file(
            "app/src/androidTest/java/com/example/FooTest.kt"
        ));
        assert!(is_test_file("module/src/integrationTest/kotlin/BarSpec.kt"));
    }

    #[test]
    fn flags_swift_test_files() {
        assert!(is_test_file("Tests/SessionTests.swift"));
        assert!(is_test_file("Sources/FooTest.swift"));
    }

    #[test]
    fn still_flags_the_previously_supported_conventions() {
        assert!(is_test_file("foo/test_bar.py"));
        assert!(is_test_file("pkg/bar_test.go"));
        assert!(is_test_file("src/foo.test.ts"));
        assert!(is_test_file("src/foo.spec.ts"));
        assert!(is_test_file("com/example/FooTest.java"));
        assert!(is_test_file("com/example/FooTestCase.java"));
        assert!(is_test_file("project/__tests__/foo.ts"));
        assert!(is_test_file("project/tests/foo.rb"));
    }

    #[test]
    fn does_not_flag_production_files_that_merely_contain_test_lowercase() {
        // The fix is capital-led so camelCase boundaries distinguish these.
        assert!(!is_test_file("src/latest/loader.kt"));
        assert!(!is_test_file("lib/manifest.kt"));
        assert!(!is_test_file(
            "okhttp/src/jvmMain/kotlin/okhttp3/internal/connection/RealCall.kt"
        ));
        assert!(!is_test_file("src/contestEntry.ts"));
        assert!(!is_test_file("pkg/greatest.go"));
    }

    #[test]
    fn does_not_flag_ordinary_production_source() {
        assert!(!is_test_file("src/flask/app.py"));
        assert!(!is_test_file(
            "src/vs/workbench/api/common/extensionHostMain.ts"
        ));
        assert!(!is_test_file(
            "okhttp/src/commonJvmAndroid/kotlin/okhttp3/OkHttpClient.kt"
        ));
    }

    // === nameMatchBonus ===

    #[test]
    fn exact_match_scores_80() {
        assert_eq!(name_match_bonus("CacheBuilder", "CacheBuilder"), 80);
        assert_eq!(name_match_bonus("cachebuilder", "CacheBuilder"), 80);
    }

    #[test]
    fn exact_token_match_in_multi_token_query_scores_60() {
        assert_eq!(name_match_bonus("build", "CacheBuilder build"), 60);
    }

    #[test]
    fn prefix_match_scales_by_length_ratio() {
        // "pod" (3) / "podgccontrolleroptions" (22): round(10 + 30*3/22) = 14
        assert_eq!(name_match_bonus("PodGCControllerOptions", "Pod"), 14);
    }

    #[test]
    fn all_camel_split_terms_present_scores_15() {
        assert_eq!(name_match_bonus("MyCacheBuilderImpl", "CacheBuilder"), 15);
    }

    #[test]
    fn substring_match_scores_10() {
        assert_eq!(name_match_bonus("supercache", "cache"), 10);
    }

    #[test]
    fn no_match_scores_0() {
        assert_eq!(name_match_bonus("unrelated", "cache"), 0);
    }

    // === kindBonus ===

    #[test]
    fn kind_bonus_matches_ts_table() {
        use crate::types::NodeKind;
        assert_eq!(kind_bonus(NodeKind::Function), 10);
        assert_eq!(kind_bonus(NodeKind::Method), 10);
        assert_eq!(kind_bonus(NodeKind::Interface), 9);
        assert_eq!(kind_bonus(NodeKind::Protocol), 9);
        assert_eq!(kind_bonus(NodeKind::Parameter), 0);
        assert_eq!(kind_bonus(NodeKind::File), 0);
        assert_eq!(kind_bonus(NodeKind::EnumMember), 3);
    }

    // === isDistinctiveIdentifier ===

    #[test]
    fn distinctive_identifiers() {
        assert!(is_distinctive_identifier("setLastEmail"));
        assert!(is_distinctive_identifier("OrgUserStore"));
        assert!(is_distinctive_identifier("REST"));
        assert!(is_distinctive_identifier("snake_case"));
        assert!(is_distinctive_identifier("v2"));
    }

    #[test]
    fn non_distinctive_plain_words() {
        assert!(!is_distinctive_identifier(""));
        assert!(!is_distinctive_identifier("flat"));
        assert!(!is_distinctive_identifier("object"));
        // Leading-capital-only words are plain words
        assert!(!is_distinctive_identifier("Screen"));
        assert!(!is_distinctive_identifier("Zustand"));
    }
}
