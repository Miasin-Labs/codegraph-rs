# search module port notes

Ported from `src/search/query-parser.ts` and `src/search/query-utils.ts`.
Status: complete, `cargo check` clean, 62/62 in-module tests pass
(`cargo test --lib search::`).

## Public API surface (re-exported from `crate::search`)

```rust
// query_parser
pub struct ParsedQuery {            // serde camelCase: pathFilters/nameFilters
    pub text: String,
    pub kinds: Vec<NodeKind>,
    pub languages: Vec<Language>,
    pub path_filters: Vec<String>,
    pub name_filters: Vec<String>,
}
pub fn parse_query(raw: &str) -> ParsedQuery;
pub fn bounded_edit_distance(a: &str, b: &str, max_dist: usize) -> usize;

// query_utils
pub static STOP_WORDS: LazyLock<HashSet<&'static str>>;
pub fn get_stem_variants(term: &str) -> Vec<String>;
pub fn extract_search_terms(query: &str) -> Vec<String>;                     // TS default (stems on)
pub fn extract_search_terms_opts(query: &str, include_stems: bool) -> Vec<String>; // TS { stems: false }
pub fn score_path_relevance(file_path: &str, query: &str) -> i32;           // can be negative
pub fn is_test_file(file_path: &str) -> bool;
pub fn name_match_bonus(node_name: &str, query: &str) -> i32;
pub fn kind_bonus(kind: NodeKind) -> i32;
pub fn is_distinctive_identifier(token: &str) -> bool;
```

Known TS consumers for the wiring wave:

- `src/db/queries.ts` → `parse_query`, `bounded_edit_distance`, `kind_bonus`,
  `name_match_bonus`, `score_path_relevance`.
- `src/context/index.ts` → `is_test_file`, `extract_search_terms` (+`_opts`
  for `{ stems: false }`), `score_path_relevance`, `get_stem_variants`,
  `is_distinctive_identifier`.
- `src/mcp/tools.ts` → `is_test_file`.
- (`src/bin/codegraph.ts` defines its own local `isTestFile` for the
  `affected` command — that one is NOT this function; it lives with the CLI.)

## Fidelity notes / deviations

- **String-length semantics.** JS `.length` / `charCodeAt` are UTF-16.
  - `bounded_edit_distance` operates on UTF-16 code units
    (`encode_utf16()`) — exact parity, including astral-plane input.
  - `name_match_bonus` length checks and the prefix-ratio use UTF-16 lengths.
  - `parse_query` tokenisation and `get_stem_variants` use `char` (Unicode
    scalar) indexing instead of UTF-16 — identical for all BMP input (incl.
    CJK); diverges only for lone-surrogate edge cases that can't occur in
    valid Rust strings anyway.
- **CJK:** the TS module has no explicit CJK branch (the orchestrator-task
  brief mentioned one; the only CJK reference in TS `src/` is in
  `extraction/index.ts` git-path handling). CJK safety here means the
  tokenizer/value handling never splits multi-byte chars — covered by 7
  supplemental tests (CJK free text, CJK path/name filter values, fullwidth
  colon U+FF1A not a separator, U+3000 ideographic space as whitespace, CJK
  edit distance).
- **Whitespace class:** JS `/\s/` = ECMAScript WhiteSpace+LineTerminator
  (includes U+FEFF, excludes U+0085 NEL); Rust `char::is_whitespace()` is
  Unicode `White_Space` (excludes U+FEFF, includes U+0085). Port uses
  `is_whitespace() || c == '\u{FEFF}'`, so the only residual divergence is
  U+0085 being treated as a token separator (JS would keep it inside a
  token). Same situation for the regex `\s` in `name_match_bonus`
  (rust regex `\s` ≠ JS `\s` by exactly FEFF/NEL). Negligible in practice.
- **Word boundaries:** JS `\b` is ASCII (`\w` = `[A-Za-z0-9_]`); ported as
  `(?-u:\b)` in the `regex` crate so CJK-adjacent identifiers tokenise the
  same as in Node.
- **`kind:` validation** goes through `NodeKind::from_str` /
  `Language::from_str` (exact string match against the canonical arrays in
  `crate::types`) instead of TS's derived `Set`s — same semantics: invalid
  values fall through to free text; `lang:` value is lowercased first,
  `kind:` value is case-sensitive (matches TS).
- **`kind_bonus`** takes `NodeKind` (exhaustive match), so the TS `?? 0`
  unknown-kind fallback is unrepresentable — all 22 kinds are mapped to the
  exact TS values.
- **`basename`/`dirname`** (Node `path`) implemented privately via
  `std::path::Path` (platform-native separators, like Node's
  platform-specific `path`). Differences: Node `dirname('/') === '/'` vs
  here `"."`; Node `basename('.') === '.'` vs here `""`. Both are only used
  as substring-match haystacks in `score_path_relevance` — no observable
  scoring difference.
- **`Math.round`** → `f64::round`: identical for the positive values
  produced here (half-up vs half-away-from-zero only differs at negative
  halves).
- JS `Set` insertion-order iteration is preserved (Vec + HashSet dedup), so
  `extract_search_terms` / `get_stem_variants` return terms in the same
  order as TS.

## Dropped Node-isms

- `import * as path from 'path'` → private `basename`/`dirname` helpers
  (see above). No other Node dependencies existed in this module.

## Integrator notes

- No shared-file changes needed; no blockers. Uses only `crate::types`,
  `regex`, `serde`, std (`LazyLock`, stable since 1.80).
- `ParsedQuery` derives Serialize/Deserialize with camelCase renames in case
  it ever crosses the wire; TS keeps it internal, so nothing depends on it.
- Tests are all in-module `#[cfg(test)]` (pure functions — per task brief);
  no `tests/` integration file needed. `__tests__/search-query-parser.test.ts`
  is fully ported (13 parseQuery + 10 boundedEditDistance cases), plus
  supplemental CJK tests and query-utils tests derived from TS doc examples
  (the TS suite has no query-utils test file).
