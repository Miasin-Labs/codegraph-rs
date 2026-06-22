use std::collections::HashSet;

use rusqlite::params_from_iter;
use rusqlite::types::Value;

use super::super::QueryBuilder;
use super::super::rows::node_from_row;
use super::filters::{push_kind_filter, push_language_filter};
use crate::error::Result;
use crate::types::{SearchOptions, SearchResult};

impl QueryBuilder {
    /// Find nodes by exact name match.
    ///
    /// Used for hybrid search — looks up symbols by exact name or
    /// case-insensitive match. Returns high-confidence matches for known
    /// symbol names extracted from a query.
    pub fn find_nodes_by_exact_name(
        &self,
        names: &[String],
        options: &SearchOptions,
    ) -> Result<Vec<SearchResult>> {
        if names.is_empty() {
            return Ok(Vec::new());
        }

        let kinds = options.kinds.as_deref();
        let languages = options.languages.as_deref();
        let limit = options.limit.unwrap_or(50);

        // Two-pass approach to handle common names (e.g., "run" has 40+ matches):
        // Pass 1: Find which files contain distinctive (rare) symbols from the query.
        // Pass 2: Query each name, boosting results that co-locate with distinctive symbols.

        // Pass 1: Find files containing each queried name, identify distinctive names
        let mut name_to_files: Vec<(String, HashSet<String>)> = Vec::new();
        for name in names {
            let mut sql =
                String::from("SELECT DISTINCT file_path FROM nodes WHERE name COLLATE NOCASE = ?");
            let mut params: Vec<Value> = vec![Value::Text(name.clone())];
            push_kind_filter(&mut sql, &mut params, "", kinds);
            sql.push_str(" LIMIT 100");
            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), |row| row.get::<_, String>(0))?;
            let mut files = HashSet::new();
            for row in rows {
                files.insert(row?);
            }
            name_to_files.push((name.to_lowercase(), files));
        }

        // Distinctive names are those with fewer than 10 file matches
        // (e.g., "scrapeLoop" = 1 file)
        let mut distinctive_files: HashSet<String> = HashSet::new();
        for (_, files) in &name_to_files {
            if !files.is_empty() && files.len() < 10 {
                for f in files {
                    distinctive_files.insert(f.clone());
                }
            }
        }

        // Pass 2: Query each name with per-name limit, scoring by co-location
        let per_name_limit = 8usize.max(limit.div_ceil(names.len()));
        let mut all_results: Vec<SearchResult> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();

        for name in names {
            let mut sql = String::from(
                "SELECT nodes.*, 1.0 as score
                 FROM nodes
                 WHERE name COLLATE NOCASE = ?",
            );
            let mut params: Vec<Value> = vec![Value::Text(name.clone())];
            push_kind_filter(&mut sql, &mut params, "", kinds);
            push_language_filter(&mut sql, &mut params, "", languages);

            // Fetch enough to find co-located results among common names
            sql.push_str(" LIMIT ?");
            params.push(Value::Integer((per_name_limit * 3).max(50) as i64));

            let mut stmt = self.db.conn().prepare(&sql)?;
            let rows = stmt.query_map(params_from_iter(params), |row| {
                let node = node_from_row(row)?;
                let score: f64 = row.get("score")?;
                Ok((node, score))
            })?;
            let mut name_results: Vec<SearchResult> = Vec::new();
            for row in rows {
                let (node, score) = row?;
                if seen_ids.contains(&node.id) {
                    continue;
                }
                // Boost results in files that also contain distinctive symbols
                let co_location_boost = if distinctive_files.contains(&node.file_path) {
                    20.0
                } else {
                    0.0
                };
                name_results.push(SearchResult {
                    node,
                    score: score + co_location_boost,
                    highlights: None,
                });
            }

            // Sort by score (co-located first), take per-name limit
            name_results.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            for r in name_results.into_iter().take(per_name_limit) {
                seen_ids.insert(r.node.id.clone());
                all_results.push(r);
            }
        }

        // Sort all results by score so co-located results bubble up
        all_results.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        all_results.truncate(limit);
        Ok(all_results)
    }
}
