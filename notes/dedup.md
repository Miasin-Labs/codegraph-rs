# Integration-debt cleanup: de-dup of `db/queries.rs::local`

The parallel port left `rust/src/db/queries.rs` with a `pub(crate) mod local`
holding private copies of search/extraction helpers (the real modules were
stubs at the time). This pass compared every copy against its now-ported
canonical home, with the TS source as referee, then deleted the copies.

## What was de-duplicated

| Helper | Canonical home (now used) | TS referee |
|---|---|---|
| `parse_query` / `ParsedQuery` | `crate::search::query_parser` | `src/search/query-parser.ts` |
| `bounded_edit_distance` | `crate::search::query_parser` | `src/search/query-parser.ts` |
| `kind_bonus` | `crate::search::query_utils` | `src/search/query-utils.ts` |
| `name_match_bonus` | `crate::search::query_utils` | `src/search/query-utils.ts` |
| `score_path_relevance` (+ private `extract_search_terms`, `basename`, `dirname`) | `crate::search::query_utils` | `src/search/query-utils.ts` |
| `is_test_file` (+ `matches_non_production_dir`) | `crate::search::query_utils` | `src/search/query-utils.ts` |
| `is_generated_file` | `crate::extraction::generated_detection` | `src/extraction/generated-detection.ts` |

**Kept in `queries.rs` (NOT moved)** — these are defined inline in the TS
`src/db/queries.ts` too, so `queries.rs` is their real home:

- `is_low_value_file` + `LOW_VALUE_PATTERNS` (TS `isLowValueFile`, queries.ts
  line ~32). Now calls `crate::extraction::generated_detection::is_generated_file`
  instead of the deleted local copy.
- `is_fts_operator` + `FTS_OPERATOR_RE` (TS inline `/^(AND|OR|NOT|NEAR)$/i`,
  queries.ts line ~1003).

The `mod local` block (~580 lines) is deleted. The only call-site change
beyond imports: `kind_bonus` / `score_path_relevance` / `name_match_bonus`
return `i32` in the canonical modules (TS returns plain numbers; the local
copies had widened to `f64`), so the rescoring sum in `search_nodes` wraps
them in `f64::from(...)`.

## Divergences found

**None in the canonical modules** — `crate::search::{query_parser,query_utils}`
and `crate::extraction::generated_detection` were verified faithful to the TS
referee (including UTF-16 code-unit semantics, JS `\s`/`\b` semantics, and the
exact pattern lists). No fixes were needed there.

All divergences were in the **local copies**, i.e. ways `mod local` was
*less* faithful to TS than the canonical port. Deleting the copies resolves
each one (behavior now strictly more TS-faithful):

1. **`bounded_edit_distance`** — local compared `char`s; TS compares UTF-16
   code units (`charCodeAt`). Canonical uses `encode_utf16()`. Differs only
   for astral-plane input (emoji, CJK-ext identifiers).
2. **`name_match_bonus` empty-query guards** — local added
   `!query_lower.is_empty()` guards before the prefix/substring checks; TS has
   no such guards (an all-whitespace query returns 10 via `startsWith('')`).
   Canonical matches TS. Also: local filtered terms by `chars().count() >= 2`
   vs TS UTF-16 `length >= 2` (canonical uses `utf16_len`).
3. **`parse_query` whitespace** — local used Rust `char::is_whitespace` for
   tokenising and `str::trim` for the final trim; JS `/\s/` and
   `String.prototype.trim` additionally treat U+FEFF (BOM) as whitespace.
   Canonical handles this via `is_js_whitespace`.
4. **`extract_search_terms` word boundaries** — local regexes used Rust
   `\b` (Unicode word boundary); JS `\b` is ASCII (`\w` is ASCII-only).
   Canonical uses `(?-u:\b)`. Differs when an identifier abuts non-ASCII text
   (e.g. CJK prose around a camelCase symbol).
5. **`basename`/`dirname`** — local used `rsplit('/')` /`rfind('/')`; Node's
   `path.basename` ignores trailing separators (`"src/api/"` → `"api"`, local
   returned `""`). Canonical uses `std::path::Path`, which matches Node here.

`is_generated_file`, `is_low_value_file`, `kind_bonus`, and `is_test_file`
local copies were behaviorally identical to canonical/TS (same pattern lists,
same table, same logic) — pure deletions.

## `__tests__/is-test-file.test.ts` coverage status

The sync agent's flag was correct: the suite targets `src/search/query-utils`
but had **not** been ported into `rust/src/search/query_utils.rs` (that file
only had supplemental cases derived from TS doc comments, overlapping but not
matching the suite — e.g. `integrationTest/`, `Tests/SessionTests.swift`,
`RealCall.kt`, `contestEntry.ts`, `greatest.go`, `commonJvmAndroid/` were all
missing). **Now ported in full**: all 5 `it` blocks / 22 assertions appear
verbatim in `query_utils.rs` under
"`isTestFile — ported from __tests__/is-test-file.test.ts`"
(`flags_kotlin_test_files_and_source_sets`, `flags_swift_test_files`,
`still_flags_the_previously_supported_conventions`,
`does_not_flag_production_files_that_merely_contain_test_lowercase`,
`does_not_flag_ordinary_production_source`). All pass against the existing
canonical implementation — no implementation change was required.

The trimmed `queries.rs` test module now covers only the inline helpers it
owns (`is_low_value_file`, `is_fts_operator` + a generated-file spot check);
the deleted local-module tests were duplicates of suites already living in
`query_parser.rs` / `query_utils.rs` / `generated_detection.rs`.

## Verification

- `cargo test --test db_test` — **49/49 pass**
- `cargo test --lib db::` — 2/2 pass (trimmed inline-helper tests)
- `cargo test --lib search::` — **67/67 pass** (62 prior + 5 newly ported)
- `cargo test --lib extraction::generated_detection` — 4/4 pass
- `cargo check` — clean, 0 warnings
