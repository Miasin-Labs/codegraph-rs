//! Field-qualified search query parser.
//!
//! Splits a raw query like
//!
//! ```text
//! kind:function name:auth path:src/api authenticate
//! ```
//!
//! into structured filters (kind=function, name="auth", path prefix
//! "src/api") plus the free-text portion ("authenticate") that goes
//! to FTS. Free-text and filters compose: filters narrow the result
//! set, FTS scores within the narrowed set.
//!
//! Recognised fields (case-insensitive, value is the rest until
//! whitespace):
//!
//! - `kind:`    one of function|method|class|interface|struct|...
//! - `lang:`    one of typescript|python|go|...   (alias: `language:`)
//! - `path:`    case-insensitive substring of file_path
//! - `name:`    case-insensitive substring of the symbol's name
//!
//! Unknown field prefixes (e.g. `foo:bar`) are passed through to FTS
//! as plain text — that's how someone searching for `TODO:` gets a
//! result instead of a parse error.
//!
//! Quoting:
//! `kind:function path:"src/some path/with spaces"` → handled by stripping
//! the surrounding double quotes from the value (single token only,
//! no nested escapes).

use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::types::{Language, NodeKind};

/// Parsed representation of a search query (mirrors TS `ParsedQuery`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ParsedQuery {
    /// Free-text portion to feed to FTS / LIKE. May be empty.
    pub text: String,
    /// kind: filters (OR'd). Empty when none specified.
    pub kinds: Vec<NodeKind>,
    /// lang:/language: filters (OR'd). Empty when none specified.
    pub languages: Vec<Language>,
    /// path: filters (OR'd, case-insensitive substring of file_path). Empty when none.
    pub path_filters: Vec<String>,
    /// name: filters (OR'd, case-insensitive substring of node.name).
    pub name_filters: Vec<String>,
}

// Validation is derived from the canonical `NODE_KINDS` / `LANGUAGES`
// arrays in `crate::types` (via their `FromStr` impls) so adding a new
// kind or language doesn't silently fall through to plain text here.

/// Whitespace test matching JS `/\s/` (ECMAScript WhiteSpace +
/// LineTerminator). Rust's `char::is_whitespace` covers the Unicode
/// `White_Space` set; JS additionally treats U+FEFF (BOM) as whitespace.
fn is_js_whitespace(c: char) -> bool {
    c.is_whitespace() || c == '\u{FEFF}'
}

/// Strip a surrounding pair of double quotes from `s`. Allows users to
/// keep whitespace in path filters: `path:"my dir/file"`.
fn unquote(s: &str) -> &str {
    // The quote characters are ASCII, so byte-based bounds checks and
    // slicing are safe here.
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Parse a raw query into structured filters + remaining text.
/// Always returns a value; never panics.
pub fn parse_query(raw: &str) -> ParsedQuery {
    let mut out = ParsedQuery::default();

    // Tokenise on whitespace, preserving quoted spans as part of the
    // current token. Quotes can appear at the start (`"…"`) OR mid-token
    // (`path:"…"`); in both cases everything from the opening `"` to the
    // matching `"` is included in the token, whitespace and all.
    //
    // Indexing is char-based (the TS source indexes UTF-16 code units) so
    // multi-byte input — CJK identifiers, fullwidth punctuation — never
    // lands on a byte boundary mid-character.
    let chars: Vec<char> = raw.chars().collect();
    let mut tokens: Vec<String> = Vec::new();
    let mut i = 0usize;
    while i < chars.len() {
        while i < chars.len() && is_js_whitespace(chars[i]) {
            i += 1;
        }
        if i >= chars.len() {
            break;
        }
        let start = i;
        while i < chars.len() && !is_js_whitespace(chars[i]) {
            if chars[i] == '"' {
                match chars[i + 1..].iter().position(|&c| c == '"') {
                    None => {
                        // Unterminated quote — swallow the rest of the input as
                        // one token. Forgiving rather than throwing.
                        i = chars.len();
                        break;
                    }
                    Some(offset) => {
                        i = i + 1 + offset + 1;
                        continue;
                    }
                }
            }
            i += 1;
        }
        tokens.push(chars[start..i].iter().collect());
    }

    let mut text_parts: Vec<String> = Vec::new();
    for tok in tokens {
        let tok_chars: Vec<char> = tok.chars().collect();
        let colon = match tok_chars.iter().position(|&c| c == ':') {
            Some(idx) if idx > 0 && idx != tok_chars.len() - 1 => idx,
            // No colon, leading colon, or trailing colon → plain text.
            _ => {
                text_parts.push(tok);
                continue;
            }
        };
        let key = tok_chars[..colon].iter().collect::<String>().to_lowercase();
        let value_owned: String = tok_chars[colon + 1..].iter().collect();
        let value_raw = unquote(&value_owned);
        if value_raw.is_empty() {
            text_parts.push(tok);
            continue;
        }
        match key.as_str() {
            "kind" => {
                if let Ok(kind) = NodeKind::from_str(value_raw) {
                    out.kinds.push(kind);
                } else {
                    text_parts.push(tok);
                }
            }
            "lang" | "language" => {
                let lower = value_raw.to_lowercase();
                if let Ok(lang) = Language::from_str(&lower) {
                    out.languages.push(lang);
                } else {
                    text_parts.push(tok);
                }
            }
            "path" => out.path_filters.push(value_raw.to_string()),
            "name" => out.name_filters.push(value_raw.to_string()),
            _ => text_parts.push(tok),
        }
    }

    out.text = text_parts
        .join(" ")
        .trim_matches(|c: char| is_js_whitespace(c))
        .to_string();
    out
}

/// Damerau-Levenshtein-ish bounded edit distance. Returns `max_dist + 1`
/// as soon as the distance is known to exceed `max_dist`; that early-exit
/// makes the fuzzy fallback cheap even over tens of thousands of names.
///
/// Pure DP, O(min(len(a), len(b))) memory. Compares case-folded inputs;
/// callers should pass lowercased strings.
///
/// Lengths and comparisons operate on UTF-16 code units for exact parity
/// with the TS implementation (`charCodeAt`).
pub fn bounded_edit_distance(a: &str, b: &str, max_dist: usize) -> usize {
    if a == b {
        return 0;
    }
    let a_units: Vec<u16> = a.encode_utf16().collect();
    let b_units: Vec<u16> = b.encode_utf16().collect();
    let al = a_units.len();
    let bl = b_units.len();
    if al.abs_diff(bl) > max_dist {
        return max_dist + 1;
    }
    if al == 0 {
        return bl;
    }
    if bl == 0 {
        return al;
    }

    let mut prev: Vec<usize> = (0..=bl).collect();
    let mut cur: Vec<usize> = vec![0; bl + 1];

    for i in 1..=al {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=bl {
            let cost = if a_units[i - 1] == b_units[j - 1] {
                0
            } else {
                1
            };
            let insertion = cur[j - 1] + 1;
            let deletion = prev[j] + 1;
            let substitution = prev[j - 1] + cost;
            cur[j] = insertion.min(deletion).min(substitution);
            if cur[j] < row_min {
                row_min = cur[j];
            }
        }
        if row_min > max_dist {
            return max_dist + 1;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[bl]
}

// =============================================================================
// Tests — ported from __tests__/search-query-parser.test.ts
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Language, NodeKind};

    // === parseQuery ===

    #[test]
    fn returns_plain_text_for_a_query_with_no_field_prefixes() {
        let r = parse_query("authenticate user");
        assert_eq!(r.text, "authenticate user");
        assert!(r.kinds.is_empty());
        assert!(r.languages.is_empty());
        assert!(r.path_filters.is_empty());
        assert!(r.name_filters.is_empty());
    }

    #[test]
    fn extracts_kind_filter_and_removes_it_from_text() {
        let r = parse_query("kind:function auth");
        assert_eq!(r.kinds, vec![NodeKind::Function]);
        assert_eq!(r.text, "auth");
    }

    #[test]
    fn extracts_lang_and_language_as_the_same_filter_family() {
        let a = parse_query("lang:typescript foo");
        let b = parse_query("language:typescript foo");
        assert_eq!(a.languages, vec![Language::Typescript]);
        assert_eq!(b.languages, vec![Language::Typescript]);
    }

    #[test]
    fn handles_multiple_kind_filters_as_an_or_set() {
        let r = parse_query("kind:function kind:method auth");
        let mut kinds: Vec<&str> = r.kinds.iter().map(|k| k.as_str()).collect();
        kinds.sort_unstable();
        assert_eq!(kinds, vec!["function", "method"]);
    }

    #[test]
    fn extracts_path_and_name_as_substring_filters_kept_verbatim() {
        let r = parse_query("path:src/api name:Handler");
        assert_eq!(r.path_filters, vec!["src/api"]);
        assert_eq!(r.name_filters, vec!["Handler"]);
    }

    #[test]
    fn preserves_quoted_spans_as_a_single_token_whitespace_in_path() {
        let r = parse_query("path:\"my dir/file\" foo");
        assert_eq!(r.path_filters, vec!["my dir/file"]);
        assert_eq!(r.text, "foo");
    }

    #[test]
    fn passes_url_like_tokens_through_to_text_does_not_match_http_as_a_field() {
        let r = parse_query("http://example.com");
        assert_eq!(r.text, "http://example.com");
        assert!(r.kinds.is_empty());
    }

    #[test]
    fn passes_empty_value_tokens_through_as_text_kind_colon() {
        let r = parse_query("kind: foo");
        assert!(r.kinds.is_empty());
        // The trailing-colon token comes back as plain text
        assert!(r.text.contains("kind:"));
    }

    #[test]
    fn passes_unknown_field_prefixes_through_as_text_todo_keeps_the_colon() {
        let r = parse_query("TODO: needs review");
        assert_eq!(r.text, "TODO: needs review");
        assert!(r.kinds.is_empty());
    }

    #[test]
    fn rejects_unknown_values_for_kind_passes_the_whole_token_to_text() {
        let r = parse_query("kind:invalid foo");
        // Invalid kind value falls back to text
        assert!(r.kinds.is_empty());
        assert!(r.text.contains("kind:invalid"));
    }

    #[test]
    fn handles_all_filters_no_text_query() {
        let r = parse_query("kind:function lang:typescript");
        assert_eq!(r.kinds, vec![NodeKind::Function]);
        assert_eq!(r.languages, vec![Language::Typescript]);
        assert_eq!(r.text, "");
    }

    #[test]
    fn survives_empty_input() {
        let r = parse_query("");
        assert_eq!(r.text, "");
        assert!(r.kinds.is_empty());
    }

    #[test]
    fn survives_a_very_long_input_no_allocation_explosion() {
        let huge = "foo ".repeat(5000); // 20k chars
        let r = parse_query(&huge);
        assert!(!r.text.is_empty());
    }

    // === parseQuery — CJK / multi-byte safety (Rust-port supplements) ===
    // The TS implementation indexes UTF-16 code units; the Rust port indexes
    // chars. These cases pin the multi-byte-safe behavior.

    #[test]
    fn cjk_free_text_passes_through_unchanged() {
        let r = parse_query("認証 ユーザー");
        assert_eq!(r.text, "認証 ユーザー");
        assert!(r.kinds.is_empty());
    }

    #[test]
    fn cjk_path_filter_value_kept_verbatim() {
        let r = parse_query("path:src/认证 处理");
        assert_eq!(r.path_filters, vec!["src/认证"]);
        assert_eq!(r.text, "处理");
    }

    #[test]
    fn cjk_quoted_name_filter_preserves_whitespace() {
        let r = parse_query("name:\"日本 語\" foo");
        assert_eq!(r.name_filters, vec!["日本 語"]);
        assert_eq!(r.text, "foo");
    }

    #[test]
    fn fullwidth_colon_is_not_a_field_separator() {
        // U+FF1A FULLWIDTH COLON is not ASCII ':' — the token stays text.
        let r = parse_query("kind：function");
        assert!(r.kinds.is_empty());
        assert_eq!(r.text, "kind：function");
    }

    #[test]
    fn ideographic_space_tokenises_like_ascii_whitespace() {
        // U+3000 IDEOGRAPHIC SPACE is whitespace in both JS /\s/ and Rust.
        let r = parse_query("foo\u{3000}bar");
        assert_eq!(r.text, "foo bar");
    }

    // === boundedEditDistance ===

    #[test]
    fn returns_0_for_identical_strings() {
        assert_eq!(bounded_edit_distance("user", "user", 2), 0);
    }

    #[test]
    fn returns_1_for_a_single_substitution() {
        assert_eq!(bounded_edit_distance("user", "usar", 2), 1);
    }

    #[test]
    fn returns_1_for_a_single_insertion() {
        assert_eq!(bounded_edit_distance("user", "users", 2), 1);
    }

    #[test]
    fn returns_1_for_a_single_deletion() {
        assert_eq!(bounded_edit_distance("users", "user", 2), 1);
    }

    #[test]
    fn returns_2_for_a_transposition_two_edits_in_basic_levenshtein() {
        // 'aple' vs 'palp' would be 2; pick a clearer pair.
        // 'foo' vs 'fou': substitution + insertion = 2 if different lengths.
        assert_eq!(bounded_edit_distance("confg", "configX", 2), 2);
    }

    #[test]
    fn returns_max_dist_plus_1_when_distance_clearly_exceeds_budget() {
        assert_eq!(bounded_edit_distance("foo", "completely-different", 2), 3);
    }

    #[test]
    fn respects_length_difference_shortcut() {
        // |len(a) - len(b)| > maxDist must immediately be over budget
        assert_eq!(bounded_edit_distance("a", "aaaaaaa", 2), 3);
    }

    #[test]
    fn handles_empty_inputs() {
        assert_eq!(bounded_edit_distance("", "", 2), 0);
        assert_eq!(bounded_edit_distance("a", "", 2), 1);
        assert_eq!(bounded_edit_distance("", "abc", 2), 3);
    }

    #[test]
    fn is_case_sensitive_caller_must_lowercase_if_case_insensitive_match_wanted() {
        assert_eq!(bounded_edit_distance("Foo", "foo", 2), 1);
    }

    #[test]
    fn early_exits_when_row_min_exceeds_budget_correctness_not_just_perf() {
        // 'aaaaa' vs 'bbbbb': distance is 5, well over budget 2
        assert_eq!(bounded_edit_distance("aaaaa", "bbbbb", 2), 3);
    }

    // === boundedEditDistance — CJK supplement (Rust port) ===

    #[test]
    fn bounded_edit_distance_counts_cjk_chars() {
        assert_eq!(bounded_edit_distance("日本語", "日本誤", 2), 1);
        assert_eq!(bounded_edit_distance("认证", "认证", 2), 0);
        assert_eq!(bounded_edit_distance("认证", "认", 2), 1);
    }
}
