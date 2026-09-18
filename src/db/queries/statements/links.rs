//! Cross-file links: the edges that leave or enter one file, joined to the
//! symbol at the far end. Explore ranks a file's one-hop neighbourhood from
//! these rows, so both reads are set-based and capped rather than walked
//! node by node.

use rusqlite::params;

use super::QueryBuilder;
use crate::error::Result;
use crate::types::{EdgeKind, NodeKind};

/// One edge between a symbol in the queried file and a symbol in another file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossFileLink {
    /// `true` when the queried file's symbol is the edge source.
    pub outgoing: bool,
    pub kind: EdgeKind,
    /// Name of the endpoint inside the queried file.
    pub local_name: String,
    pub far_id: String,
    pub far_name: String,
    pub far_kind: NodeKind,
    pub far_file: String,
    pub far_start_line: u32,
    pub far_end_line: u32,
}

// The local side is read through a capped, line-ordered subquery so a file
// with tens of thousands of symbols (generated register headers) costs at
// most `?3` index probes per direction, whatever its edge count.
const OUTGOING_SQL: &str = "SELECT local.name, e.kind, far.id, far.name, far.kind, far.file_path, far.start_line, far.end_line
     FROM (SELECT id, name FROM nodes WHERE file_path = ?1 ORDER BY start_line LIMIT ?3) AS local
     JOIN edges e ON e.source = local.id
     JOIN nodes far ON far.id = e.target
     WHERE e.kind != 'contains' AND far.file_path != ?1
     LIMIT ?2";

const INCOMING_SQL: &str = "SELECT local.name, e.kind, far.id, far.name, far.kind, far.file_path, far.start_line, far.end_line
     FROM (SELECT id, name FROM nodes WHERE file_path = ?1 ORDER BY start_line LIMIT ?3) AS local
     JOIN edges e ON e.target = local.id
     JOIN nodes far ON far.id = e.source
     WHERE e.kind != 'contains' AND far.file_path != ?1
     LIMIT ?2";

impl QueryBuilder {
    /// Non-`contains` edges between `file_path` and any other file: at most
    /// `limit` rows per direction (outgoing first), read from the file's first
    /// `max_local_nodes` symbols by line. Rows whose kinds do not parse are
    /// skipped rather than failing the read.
    pub fn get_cross_file_links(
        &self,
        file_path: &str,
        limit: usize,
        max_local_nodes: usize,
    ) -> Result<Vec<CrossFileLink>> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let max_local_nodes = i64::try_from(max_local_nodes).unwrap_or(i64::MAX);
        let mut links = Vec::new();
        for (outgoing, sql) in [(true, OUTGOING_SQL), (false, INCOMING_SQL)] {
            let mut stmt = self.db.conn().prepare_cached(sql)?;
            let rows = stmt.query_map(params![file_path, limit, max_local_nodes], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, u32>(6)?,
                    row.get::<_, u32>(7)?,
                ))
            })?;
            for row in rows {
                let (local_name, kind, far_id, far_name, far_kind, far_file, start, end) = row?;
                let (Ok(kind), Ok(far_kind)) =
                    (kind.parse::<EdgeKind>(), far_kind.parse::<NodeKind>())
                else {
                    continue;
                };
                links.push(CrossFileLink {
                    outgoing,
                    kind,
                    local_name,
                    far_id,
                    far_name,
                    far_kind,
                    far_file,
                    far_start_line: start,
                    far_end_line: end,
                });
            }
        }
        Ok(links)
    }

    /// Non-`contains` edges into (`incoming`) or out of one node, counted up
    /// to `cap` so a hub symbol costs no more than `cap` index entries.
    pub fn count_node_edges_capped(
        &self,
        node_id: &str,
        incoming: bool,
        cap: usize,
    ) -> Result<usize> {
        let sql = if incoming {
            "SELECT COUNT(*) FROM (SELECT 1 FROM edges WHERE target = ?1 AND kind != 'contains' LIMIT ?2)"
        } else {
            "SELECT COUNT(*) FROM (SELECT 1 FROM edges WHERE source = ?1 AND kind != 'contains' LIMIT ?2)"
        };
        let cap = i64::try_from(cap).unwrap_or(i64::MAX);
        let count: i64 = self
            .db
            .conn()
            .prepare_cached(sql)?
            .query_row(params![node_id, cap], |row| row.get(0))?;
        Ok(usize::try_from(count).unwrap_or(0))
    }
}
