use rusqlite::params_from_iter;
use rusqlite::types::Value;

use super::super::QueryBuilder;
use super::super::rows::node_from_row;
use super::filters::{is_fts_operator, push_kind_filter, push_language_filter};
use crate::error::Result;
use crate::types::{Language, NodeKind, SearchResult};

impl QueryBuilder {
    /// FTS5 search with prefix matching.
    pub(super) fn search_nodes_fts(
        &self,
        query: &str,
        kinds: Option<&[NodeKind]>,
        languages: Option<&[Language]>,
        limit: usize,
        offset: usize,
        exhaustive_candidates: bool,
    ) -> Vec<SearchResult> {
        // Add prefix wildcard for better matching (e.g., "auth" matches
        // "AuthService", "authenticate"). Escape special FTS5 characters
        // and add prefix wildcard.
        //
        // `::` is a qualifier separator in Rust/C++/Ruby, not a token char,
        // so treat it as whitespace before the strip step. Otherwise queries
        // like `stage_apply::run` collapse to `stage_applyrun` (the colons
        // are stripped without splitting) and find nothing. See #173.
        let cleaned: String = query
            .replace("::", " ") // Rust/C++/Ruby qualifier separator
            .chars()
            .filter(|c| !matches!(c, '\'' | '"' | '*' | '(' | ')' | ':' | '^')) // FTS5 special chars
            .collect();
        let fts_query = cleaned
            .split_whitespace()
            .filter(|term| !term.is_empty())
            // Strip FTS5 boolean operators to prevent query manipulation
            .filter(|term| !is_fts_operator(term))
            .map(|term| format!("\"{term}\"*")) // Prefix match each term
            .collect::<Vec<_>>()
            .join(" OR ");

        if fts_query.is_empty() {
            return Vec::new();
        }

        // BM25 column weights: id=0, name=20, qualified_name=5, docstring=1, signature=2
        // Heavy name weight ensures exact/prefix name matches rank above incidental
        // mentions in long docstrings or qualified names of nested symbols.
        // Fetch 5x requested limit so post-hoc rescoring (kind_bonus, path
        // relevance, name_match_bonus) can promote results that BM25 alone
        // undervalues.
        let fts_limit = (limit * 5).max(100);

        let mut sql = String::from(
            "SELECT nodes.*, bm25(nodes_fts, 0, 20, 5, 1, 2) as score
             FROM nodes_fts
             JOIN nodes ON nodes_fts.id = nodes.id
             WHERE nodes_fts MATCH ?",
        );
        let mut params: Vec<Value> = vec![Value::Text(fts_query)];
        push_kind_filter(&mut sql, &mut params, "nodes.", kinds);
        push_language_filter(&mut sql, &mut params, "nodes.", languages);
        sql.push_str(" ORDER BY score");
        if !exhaustive_candidates {
            sql.push_str(" LIMIT ? OFFSET ?");
            params.push(Value::Integer(fts_limit as i64));
            params.push(Value::Integer(offset as i64));
        }

        // FTS query failed → return empty (TS try/catch parity)
        let run = || -> Result<Vec<SearchResult>> {
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), |row| {
                let node = node_from_row(row)?;
                let score: f64 = row.get("score")?;
                Ok(SearchResult {
                    node,
                    score: score.abs(), // bm25 returns negative scores
                    highlights: None,
                })
            })?;
            let mut out = Vec::new();
            for row in rows {
                out.push(row?);
            }
            Ok(out)
        };
        run().unwrap_or_default()
    }

    /// LIKE-based substring search for cases where FTS doesn't match.
    /// Useful for camelCase matching (e.g., "signIn" finds "signInWithGoogle").
    pub(super) fn search_nodes_like(
        &self,
        query: &str,
        kinds: Option<&[NodeKind]>,
        languages: Option<&[Language]>,
        limit: usize,
        offset: usize,
        exhaustive_candidates: bool,
    ) -> Result<Vec<SearchResult>> {
        let mut sql = String::from(
            "SELECT nodes.*,
               CASE
                 WHEN name = ? THEN 1.0
                 WHEN name LIKE ? THEN 0.9
                 WHEN name LIKE ? THEN 0.8
                 WHEN qualified_name LIKE ? THEN 0.7
                 ELSE 0.5
               END as score
             FROM nodes
             WHERE (
               name LIKE ? OR
               qualified_name LIKE ? OR
               name LIKE ?
             )",
        );

        // Pattern variants for better matching
        let exact_match = query.to_string();
        let starts_with = format!("{query}%");
        let contains = format!("%{query}%");

        let mut params: Vec<Value> = vec![
            Value::Text(exact_match),         // Exact match score
            Value::Text(starts_with.clone()), // Starts with score
            Value::Text(contains.clone()),    // Contains score
            Value::Text(contains.clone()),    // Qualified name score
            Value::Text(contains.clone()),    // WHERE: name contains
            Value::Text(contains),            // WHERE: qualified_name contains
            Value::Text(starts_with),         // WHERE: name starts with
        ];

        push_kind_filter(&mut sql, &mut params, "", kinds);
        push_language_filter(&mut sql, &mut params, "", languages);

        sql.push_str(" ORDER BY score DESC, length(name) ASC");
        if !exhaustive_candidates {
            sql.push_str(" LIMIT ? OFFSET ?");
            params.push(Value::Integer(limit as i64));
            params.push(Value::Integer(offset as i64));
        }

        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), |row| {
            let node = node_from_row(row)?;
            let score: f64 = row.get("score")?;
            Ok(SearchResult {
                node,
                score,
                highlights: None,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }
}
