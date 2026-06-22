use std::collections::HashSet;

use rusqlite::params_from_iter;
use rusqlite::types::Value;

use super::super::QueryBuilder;
use super::super::rows::node_from_row;
use super::filters::{push_kind_filter, push_language_filter};
use crate::error::Result;
use crate::search::bounded_edit_distance;
use crate::types::{Language, NodeKind, SearchResult};

impl QueryBuilder {
    /// Match-everything path used when the user supplied only field
    /// filters (`kind:function lang:typescript`) with no text. Returns
    /// candidates ordered by name; the caller's filter pass narrows to
    /// what was asked for.
    pub(super) fn search_all_by_filters(
        &self,
        kinds: Option<&[NodeKind]>,
        languages: Option<&[Language]>,
        limit: usize,
        exhaustive_candidates: bool,
    ) -> Result<Vec<SearchResult>> {
        let mut sql = String::from("SELECT * FROM nodes WHERE 1=1");
        let mut params: Vec<Value> = Vec::new();
        push_kind_filter(&mut sql, &mut params, "", kinds);
        push_language_filter(&mut sql, &mut params, "", languages);
        sql.push_str(" ORDER BY name");
        if !exhaustive_candidates {
            sql.push_str(" LIMIT ?");
            params.push(Value::Integer(limit as i64));
        }
        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params), node_from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(SearchResult {
                node: row?,
                score: 1.0,
                highlights: None,
            });
        }
        Ok(out)
    }

    /// Fuzzy fallback: when zero FTS/LIKE hits, try an edit-distance
    /// sweep over the distinct symbol-name set. Caps `max_dist` at 2 so
    /// `getUssr` finds `getUser` but `process` doesn't match `prosody`.
    /// Bounded edit distance keeps each comparison cheap; the per-query
    /// scan is O(distinct-name-count) which is far smaller than total
    /// node count on any real codebase.
    pub(super) fn search_nodes_fuzzy(
        &self,
        text: &str,
        kinds: Option<&[NodeKind]>,
        languages: Option<&[Language]>,
        limit: usize,
        exhaustive_candidates: bool,
    ) -> Result<Vec<SearchResult>> {
        let lowered = text.to_lowercase();
        let max_dist = if lowered.chars().count() <= 4 { 1 } else { 2 };

        // Pull the distinct name list once. Even on a 200k-node project the
        // distinct name set is typically O(10k) because most names repeat.
        // The candidate-cap below bounds memory regardless.
        let all_names = self.get_all_node_names()?;
        let mut candidates: Vec<(String, usize)> = Vec::new();
        for name in all_names {
            let dist = bounded_edit_distance(&name.to_lowercase(), &lowered, max_dist);
            if dist <= max_dist {
                candidates.push((name, dist));
            }
        }
        candidates.sort_by_key(|c| c.1);

        // Cap the per-name follow-up queries. Each survivor triggers a
        // separate `SELECT * FROM nodes WHERE name = ?`; without this cap
        // a project with many similar names (`getUser1`, `getUser2`...)
        // could fan out far beyond `limit` queries before the inner-loop
        // limit kicks in.
        let followup_cap = if exhaustive_candidates {
            candidates.len()
        } else {
            (limit * 2).max(50)
        };
        candidates.truncate(followup_cap);

        let mut results: Vec<SearchResult> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (name, dist) in candidates {
            if !exhaustive_candidates && results.len() >= limit {
                break;
            }
            let mut sql = String::from("SELECT * FROM nodes WHERE name = ?");
            let mut params: Vec<Value> = vec![Value::Text(name)];
            push_kind_filter(&mut sql, &mut params, "", kinds);
            push_language_filter(&mut sql, &mut params, "", languages);
            if !exhaustive_candidates {
                sql.push_str(" LIMIT 5");
            }
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), node_from_row)?;
            for row in rows {
                let node = row?;
                if seen.contains(&node.id) {
                    continue;
                }
                seen.insert(node.id.clone());
                // Lower the score for each edit step away from the query so
                // exact-match fallbacks (dist 0) outrank dist-2 typos.
                results.push(SearchResult {
                    node,
                    score: 1.0 / (1.0 + dist as f64),
                    highlights: None,
                });
                if !exhaustive_candidates && results.len() >= limit {
                    break;
                }
            }
        }
        Ok(results)
    }
}
