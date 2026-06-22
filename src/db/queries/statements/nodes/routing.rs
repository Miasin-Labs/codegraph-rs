use super::super::QueryBuilder;
use super::super::models::{DominantFile, RoutingManifest, RoutingManifestEntry, TopRouteFile};
use super::super::search::is_low_value_file;
use crate::error::Result;

impl QueryBuilder {
    /// Find the file that holds the densest concentration of the project's
    /// internal call graph — the "core" file. Used by context-builder to
    /// boost ranking of symbols in that file's directory.
    ///
    /// Returns None if no file has a meaningful concentration (e.g. spread
    /// evenly across many files, or empty index).
    ///
    /// "Internal" = source and target are in the same file. Cross-file
    /// edges aren't useful here — they don't tell us which file is the
    /// functional center.
    ///
    /// Excludes test/spec files from candidacy via path-pattern.
    pub fn get_dominant_file(&self) -> Result<Option<DominantFile>> {
        // Pull top 20 candidates; we then filter out test/generated files
        // in code (regex-grade matching that SQL LIKE can't express).
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT n.file_path AS file_path, COUNT(*) AS edge_count
             FROM edges e
             JOIN nodes n ON e.source = n.id
             JOIN nodes m ON e.target = m.id
             WHERE n.file_path = m.file_path
             GROUP BY n.file_path
             ORDER BY edge_count DESC
             LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
            ))
        })?;
        let mut filtered: Vec<(String, u64)> = Vec::new();
        for row in rows {
            let (file_path, edge_count) = row?;
            if !is_low_value_file(&file_path) {
                filtered.push((file_path, edge_count));
            }
        }
        if filtered.is_empty() || filtered[0].1 < 20 {
            return Ok(None);
        }
        Ok(Some(DominantFile {
            file_path: filtered[0].0.clone(),
            edge_count: filtered[0].1,
            next_edge_count: filtered.get(1).map(|r| r.1).unwrap_or(0),
        }))
    }

    /// Find the file that holds the densest concentration of the project's
    /// `route` nodes (framework-emitted: Express/Gin/Flask/Rails/Drupal/etc.).
    ///
    /// Excludes test/generated files from candidacy. Returns None if there
    /// are fewer than 3 non-test routes total, or if no file holds at least
    /// 30% of them (diffuse routing → no single answer file).
    pub fn get_top_route_file(&self) -> Result<Option<TopRouteFile>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT file_path, COUNT(*) AS cnt
             FROM nodes
             WHERE kind = 'route'
             GROUP BY file_path
             ORDER BY cnt DESC
             LIMIT 20",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?.max(0) as u64,
            ))
        })?;
        let mut filtered: Vec<(String, u64)> = Vec::new();
        for row in rows {
            let (file_path, cnt) = row?;
            if !is_low_value_file(&file_path) {
                filtered.push((file_path, cnt));
            }
        }
        if filtered.is_empty() {
            return Ok(None);
        }
        let total_routes: u64 = filtered.iter().map(|r| r.1).sum();
        let top = &filtered[0];
        if total_routes < 3 || top.1 < 3 {
            return Ok(None);
        }
        if (top.1 as f64) / (total_routes as f64) < 0.30 {
            return Ok(None);
        }
        Ok(Some(TopRouteFile {
            file_path: top.0.clone(),
            route_count: top.1,
            total_routes,
        }))
    }

    /// Build a URL → handler manifest from the index. Each route node's
    /// `references` edge points at the function/method that handles the
    /// request. We join them in one pass; the agent gets the canonical
    /// routing answer ("POST /users/login → AuthController#login") without
    /// having to parse the framework's route DSL itself.
    ///
    /// Also returns the file with the most handler endpoints.
    /// `limit` defaults to 40 when `None`.
    pub fn get_routing_manifest(&self, limit: Option<usize>) -> Result<Option<RoutingManifest>> {
        let limit = limit.unwrap_or(40);
        // Edge kind varies across framework resolvers: Spring/Rails/
        // Laravel/Drupal emit `references`, Express emits `calls`. Accept
        // both — the semantic is the same (route → its handler).
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT
               r.name AS url,
               h.name AS handler,
               h.file_path AS handler_file,
               h.start_line AS handler_line,
               h.kind AS handler_kind
             FROM nodes r
             JOIN edges e ON e.source = r.id
             JOIN nodes h ON e.target = h.id
             WHERE r.kind = 'route'
               AND e.kind IN ('references', 'calls')
               AND h.kind IN ('function', 'method', 'class')
             ORDER BY r.file_path, r.start_line
             LIMIT ?",
        )?;
        let rows = stmt.query_map([limit as i64], |row| {
            Ok(RoutingManifestEntry {
                url: row.get(0)?,
                handler: row.get(1)?,
                handler_file: row.get(2)?,
                handler_line: row.get(3)?,
                handler_kind: row.get(4)?,
            })
        })?;
        // Drop test/generated handlers — same hygiene as elsewhere.
        let mut filtered: Vec<RoutingManifestEntry> = Vec::new();
        for row in rows {
            let entry = row?;
            if !is_low_value_file(&entry.handler_file) {
                filtered.push(entry);
            }
        }
        if filtered.len() < 3 {
            return Ok(None);
        }
        // Identify the file holding the most handlers (the "primary handler
        // file"). Insertion-ordered so ties resolve to the first-seen file,
        // matching the TS Map iteration order.
        let mut file_counts: Vec<(String, u64)> = Vec::new();
        for entry in &filtered {
            match file_counts
                .iter_mut()
                .find(|(f, _)| f == &entry.handler_file)
            {
                Some((_, c)) => *c += 1,
                None => file_counts.push((entry.handler_file.clone(), 1)),
            }
        }
        let mut top_handler_file: Option<String> = None;
        let mut top_handler_file_count: u64 = 0;
        for (file, count) in &file_counts {
            if *count > top_handler_file_count {
                top_handler_file = Some(file.clone());
                top_handler_file_count = *count;
            }
        }
        let total_routes = filtered.len() as u64;
        Ok(Some(RoutingManifest {
            entries: filtered,
            top_handler_file,
            top_handler_file_count,
            total_routes,
        }))
    }
}
