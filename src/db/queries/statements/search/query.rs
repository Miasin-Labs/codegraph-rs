use std::collections::HashSet;

use rusqlite::params_from_iter;
use rusqlite::types::Value;

use super::super::QueryBuilder;
use super::super::rows::node_from_row;
use super::filters::{intersect_filter_axis, push_kind_filter, push_language_filter};
use crate::error::Result;
use crate::search::{kind_bonus, name_match_bonus, parse_query, score_path_relevance};
use crate::types::{SearchOptions, SearchResult};

impl QueryBuilder {
    /// Search nodes by name using FTS with fallback to LIKE for better matching.
    ///
    /// Search strategy:
    /// 1. Try FTS5 prefix match (query*) for word-start matching
    /// 2. If no results, try LIKE for substring matching (e.g., "signIn" finds "signInWithGoogle")
    /// 3. Score results based on match quality
    pub fn search_nodes(&self, query: &str, options: &SearchOptions) -> Result<Vec<SearchResult>> {
        let limit = options.limit.unwrap_or(100);
        let offset = options.offset.unwrap_or(0);

        // Parse field-qualified bits out of the raw query (kind:, lang:,
        // path:, name:). Anything not recognised stays in `text` and goes
        // to FTS unchanged. Filters compose with the SearchOptions arg —
        // both are applied (intersection-style).
        let parsed = parse_query(query);
        let merged_kinds = match intersect_filter_axis(options.kinds.as_deref(), &parsed.kinds) {
            Ok(filters) => filters,
            Err(()) => return Ok(Vec::new()),
        };
        let merged_languages =
            match intersect_filter_axis(options.languages.as_deref(), &parsed.languages) {
                Ok(filters) => filters,
                Err(()) => return Ok(Vec::new()),
            };
        let path_filters = &parsed.path_filters;
        let name_filters = &parsed.name_filters;
        // The text portion drives FTS/LIKE; if all the user typed was
        // filters (`kind:function`), we still need *some* candidate set,
        // so synthesise an empty-text path that returns everything matching
        // the filters.
        let text = parsed.text.as_str();
        let kinds = merged_kinds.as_deref();
        let languages = merged_languages.as_deref();
        let exhaustive_candidates = !path_filters.is_empty() || !name_filters.is_empty();
        let fetch_limit = limit + offset;

        // First try FTS5 with prefix matching
        let mut results = if !text.is_empty() {
            self.search_nodes_fts(
                text,
                kinds,
                languages,
                fetch_limit,
                0,
                exhaustive_candidates,
            )
        } else {
            // Over-fetch by 5× when running filter-only (no text). The
            // post-scoring path: + name: filters can be very selective, so
            // a smaller multiplier risks returning fewer than `limit`
            // results despite the DB having plenty of matches.
            self.search_all_by_filters(kinds, languages, fetch_limit * 5, exhaustive_candidates)?
        };

        // If no FTS results, try LIKE-based substring search
        if results.is_empty() && text.chars().count() >= 2 {
            results = self.search_nodes_like(
                text,
                kinds,
                languages,
                fetch_limit,
                0,
                exhaustive_candidates,
            )?;
        }

        // Final fuzzy fallback: scan all known names and keep those within
        // a tight Levenshtein distance. Only fires when both FTS and LIKE
        // returned nothing AND there's a text portion long enough to be
        // worth fuzzing (1-char queries would match too much).
        if results.is_empty() && text.chars().count() >= 3 {
            results = self.search_nodes_fuzzy(
                text,
                kinds,
                languages,
                fetch_limit,
                exhaustive_candidates,
            )?;
        }

        // Supplement: ensure exact name matches are always candidates.
        // BM25 can bury short exact-match names (e.g. "getBean") under hundreds
        // of compound names (e.g. "getBeanDescriptor") in large codebases,
        // pushing them past the FTS fetch limit before post-hoc scoring can
        // help. Use the max BM25 score as the base so the name_match_bonus
        // (exact=30 vs prefix=20) actually differentiates them after rescoring.
        if !results.is_empty() && !text.is_empty() {
            let mut existing_ids: HashSet<String> =
                results.iter().map(|r| r.node.id.clone()).collect();
            let max_fts_score = results
                .iter()
                .map(|r| r.score)
                .fold(f64::NEG_INFINITY, f64::max);
            let terms: Vec<&str> = text
                .split_whitespace()
                .filter(|t| t.chars().count() >= 2)
                .collect();
            for term in terms {
                let mut sql = String::from("SELECT * FROM nodes WHERE name = ? COLLATE NOCASE");
                let mut params: Vec<Value> = vec![Value::Text(term.to_string())];
                push_kind_filter(&mut sql, &mut params, "", kinds);
                push_language_filter(&mut sql, &mut params, "", languages);
                sql.push_str(" LIMIT 20");
                let mut stmt = self.db.conn().prepare(&sql)?;
                let rows = stmt.query_map(params_from_iter(params), node_from_row)?;
                for row in rows {
                    let node = row?;
                    if !existing_ids.contains(&node.id) {
                        existing_ids.insert(node.id.clone());
                        results.push(SearchResult {
                            node,
                            score: max_fts_score,
                            highlights: None,
                        });
                    }
                }
            }
        }

        // Apply multi-signal scoring
        if !results.is_empty() && (!text.is_empty() || !query.is_empty()) {
            let scoring_query = if !text.is_empty() { text } else { query };
            for r in results.iter_mut() {
                r.score = r.score
                    + f64::from(kind_bonus(r.node.kind))
                    + f64::from(score_path_relevance(&r.node.file_path, scoring_query))
                    + f64::from(name_match_bonus(&r.node.name, scoring_query));
            }
            results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        // Apply path: + name: filters AFTER scoring. Scoring already uses
        // path/name as a soft signal; the explicit filters here are a hard
        // gate. Done last so the FTS limit fetched plenty of candidates to
        // narrow from.
        if !path_filters.is_empty() {
            let lowered: Vec<String> = path_filters.iter().map(|p| p.to_lowercase()).collect();
            results.retain(|r| {
                let fp = r.node.file_path.to_lowercase();
                lowered.iter().any(|p| fp.contains(p))
            });
        }
        if !name_filters.is_empty() {
            let lowered: Vec<String> = name_filters.iter().map(|n| n.to_lowercase()).collect();
            results.retain(|r| {
                let nm = r.node.name.to_lowercase();
                lowered.iter().any(|n| nm.contains(n))
            });
        }

        Ok(results.into_iter().skip(offset).take(limit).collect())
    }
}
