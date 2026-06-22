use rusqlite::params_from_iter;
use rusqlite::types::Value;

use super::super::QueryBuilder;
use super::super::rows::node_from_row;
use super::filters::{name_substring_candidate_limit, push_kind_filter, push_language_filter};
use crate::error::Result;
use crate::types::{SearchOptions, SearchResult};

impl QueryBuilder {
    /// Find nodes whose name contains a substring (LIKE-based).
    /// Useful for CamelCase-part matching where FTS fails because
    /// e.g. "TransportSearchAction" is one FTS token, not matchable by "Search"*.
    ///
    /// Results are ordered by name length (shorter = more likely to be the core type).
    /// `exclude_prefix` excludes prefix matches (handled by FTS-based prefix search).
    pub fn find_nodes_by_name_substring(
        &self,
        substring: &str,
        options: &SearchOptions,
        exclude_prefix: bool,
    ) -> Result<Vec<SearchResult>> {
        let kinds = options.kinds.as_deref();
        let languages = options.languages.as_deref();
        let limit = options.limit.unwrap_or(30);
        if limit == 0 {
            return Ok(Vec::new());
        }

        let mut sql = String::from(
            "SELECT nodes.*, 1.0 as score
             FROM nodes
             WHERE name LIKE ?",
        );
        let mut params: Vec<Value> = vec![Value::Text(format!("%{substring}%"))];

        // Exclude prefix matches (handled by FTS-based prefix search in Step 2b)
        if exclude_prefix {
            sql.push_str(" AND name NOT LIKE ?");
            params.push(Value::Text(format!("{substring}%")));
        }

        push_kind_filter(&mut sql, &mut params, "", kinds);
        push_language_filter(&mut sql, &mut params, "", languages);

        // Do not ask SQLite to ORDER BY length(name) over every `%term%` match.
        // On very large indexes (notably broad roots such as `$HOME` or kernel
        // trees), that materializes and sorts huge temp sets for explore's
        // CamelCase expansion. Fetch a bounded candidate window instead and
        // sort that small slice in-process.
        sql.push_str(" LIMIT ?");
        params.push(Value::Integer(name_substring_candidate_limit(limit) as i64));

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
        out.sort_by(|a, b| {
            a.node
                .name
                .chars()
                .count()
                .cmp(&b.node.name.chars().count())
                .then_with(|| a.node.name.cmp(&b.node.name))
                .then_with(|| a.node.id.cmp(&b.node.id))
        });
        out.truncate(limit);
        Ok(out)
    }
}
