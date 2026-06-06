//! Search module — field-qualified FTS query parsing plus shared term
//! extraction / scoring helpers.
//!
//! Mirrors `src/search/` in the TS source (`query-parser.ts`,
//! `query-utils.ts`). There is no `index.ts` in the TS module; consumers
//! (db queries, context builder, MCP tools) import the files directly, so
//! everything public is re-exported here for convenience.

pub mod query_parser;
pub mod query_utils;

pub use query_parser::{ParsedQuery, bounded_edit_distance, parse_query};
pub use query_utils::{
    STOP_WORDS,
    extract_search_terms,
    extract_search_terms_opts,
    get_stem_variants,
    is_distinctive_identifier,
    is_test_file,
    kind_bonus,
    name_match_bonus,
    score_path_relevance,
};
