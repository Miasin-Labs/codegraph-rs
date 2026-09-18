//! Which method names the reachable graphs define — a pre-filter so the
//! (comparatively costly) receiver inference runs only for calls a
//! reachable crate could answer.

use std::collections::HashSet;

use super::open::GraphCache;

/// The method names of every reachable crate, or no filter at all.
pub(crate) enum MethodNames {
    /// Every name passes (a handful of references: reading every graph's
    /// names would cost more than it saves).
    Unfiltered,
    Known(HashSet<String>),
}

impl MethodNames {
    /// Read the method names of every reachable graph (each opened once).
    pub(crate) fn read(cache: &GraphCache<'_>) -> MethodNames {
        let mut names = HashSet::new();
        for index in 0..cache.reach().graphs().len() {
            let Some(graph) = cache.get_index(index) else {
                continue;
            };
            let prefix = if graph.crate_dir.is_empty() {
                String::new()
            } else {
                format!("{}/", graph.crate_dir)
            };
            let Ok(mut stmt) = graph.db.conn().prepare(
                "SELECT DISTINCT name FROM nodes WHERE kind = 'method'
                   AND (?1 = '' OR substr(file_path, 1, length(?1)) = ?1)",
            ) else {
                continue;
            };
            let Ok(rows) = stmt.query_map([prefix.as_str()], |row| row.get::<_, String>(0)) else {
                continue;
            };
            names.extend(rows.flatten());
        }
        MethodNames::Known(names)
    }

    /// Some reachable crate may define a method `name`.
    pub(crate) fn may_define(&self, name: &str) -> bool {
        match self {
            MethodNames::Unfiltered => true,
            MethodNames::Known(names) => names.contains(name),
        }
    }
}
