use crate::db::QueryBuilder;
use crate::error::Result;
use crate::types::Metadata;

/// One raw row from the `edges` table. Read with raw SQL (sorted) because
/// `QueryBuilder` exposes per-node edge lookups, not a bulk scan.
pub(super) struct EdgeRow {
    pub(super) source: String,
    pub(super) target: String,
    pub(super) kind: String,
    pub(super) metadata: Option<Metadata>,
    pub(super) line: Option<u32>,
    pub(super) col: Option<u32>,
}

pub(super) fn read_all_edges(queries: &QueryBuilder) -> Result<Vec<EdgeRow>> {
    let conn = queries.db().conn();
    let mut stmt = conn.prepare(
        "SELECT source, target, kind, metadata, line, col FROM edges \
         ORDER BY source, target, kind, COALESCE(line, -1), COALESCE(col, -1)",
    )?;
    let rows = stmt.query_map([], |row| {
        let metadata: Option<String> = row.get(3)?;
        Ok(EdgeRow {
            source: row.get(0)?,
            target: row.get(1)?,
            kind: row.get(2)?,
            metadata: metadata.and_then(|s| serde_json::from_str::<Metadata>(&s).ok()),
            line: row.get::<_, Option<i64>>(4)?.map(|v| v.max(0) as u32),
            col: row.get::<_, Option<i64>>(5)?.map(|v| v.max(0) as u32),
        })
    })?;
    rows.map(|r| r.map_err(Into::into)).collect()
}
