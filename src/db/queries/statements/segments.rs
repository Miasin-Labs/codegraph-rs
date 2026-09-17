use std::collections::HashMap;

use rusqlite::{OptionalExtension, params, params_from_iter};

use super::QueryBuilder;
use crate::error::Result;
use crate::search::split_identifier_segments;

const SEGMENT_VOCAB_STATE_KEY: &str = "name_segment_vocab_state";
const SEGMENT_VOCAB_COMPLETE: &str = "complete";

impl QueryBuilder {
    pub(crate) fn insert_name_segments(&self, name: &str) -> Result<()> {
        let segments = split_identifier_segments(name);
        if segments.is_empty() {
            return Ok(());
        }
        let mut stmt = self.db.conn().prepare_cached(
            "INSERT OR IGNORE INTO name_segment_vocab (segment, name) VALUES (?, ?)",
        )?;
        for segment in segments {
            stmt.execute(params![segment, name])?;
        }
        Ok(())
    }

    pub fn clear_name_segment_vocab(&self) -> Result<()> {
        self.db.exec("DELETE FROM name_segment_vocab")
    }

    pub fn is_name_segment_vocab_empty(&self) -> Result<bool> {
        let value = self
            .db
            .conn()
            .query_row("SELECT 1 FROM name_segment_vocab LIMIT 1", [], |row| {
                row.get::<_, i64>(0)
            })
            .optional()?;
        Ok(value.is_none())
    }

    /// True once a full rebuild has committed. Incremental node writes keep a
    /// complete vocabulary complete, but cannot complete a partial one (a
    /// fresh migration, or a rebuild from before rebuilds were atomic).
    pub fn is_name_segment_vocab_complete(&self) -> Result<bool> {
        Ok(self.get_metadata(SEGMENT_VOCAB_STATE_KEY)?.as_deref() == Some(SEGMENT_VOCAB_COMPLETE))
    }

    /// Distinct indexed names sorted after `after`, keyset-paginated so each
    /// page is an index range scan rather than an OFFSET re-walk. Empty names
    /// have no segments, so starting from `""` loses nothing.
    pub fn get_distinct_node_names_after(&self, after: &str, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT DISTINCT name FROM nodes
             WHERE kind NOT IN ('file', 'import') AND name > ?
             ORDER BY name LIMIT ?",
        )?;
        let rows = stmt.query_map(params![after, limit as i64], |row| row.get(0))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn insert_name_segments_batch(&self, names: &[String]) -> Result<()> {
        self.db.transaction(|| {
            for name in names {
                self.insert_name_segments(name)?;
            }
            Ok(())
        })
    }

    /// Rebuild the vocabulary from persisted nodes in one transaction, so a
    /// killed rebuild rolls back instead of leaving a partial vocabulary that
    /// looks healed.
    pub fn rebuild_name_segment_vocab(&self, batch_size: usize) -> Result<()> {
        let batch_size = batch_size.max(1);
        self.db.transaction(|| {
            self.clear_name_segment_vocab()?;
            let mut after = String::new();
            loop {
                let names = self.get_distinct_node_names_after(&after, batch_size)?;
                let Some(last) = names.last() else {
                    break;
                };
                after.clone_from(last);
                self.insert_name_segments_batch(&names)?;
            }
            self.set_metadata(SEGMENT_VOCAB_STATE_KEY, SEGMENT_VOCAB_COMPLETE)
        })
    }

    /// Count the distinct indexed names containing each requested segment.
    pub fn get_segment_name_counts(&self, segments: &[String]) -> Result<HashMap<String, usize>> {
        if segments.is_empty() {
            return Ok(HashMap::new());
        }
        let placeholders = std::iter::repeat_n("?", segments.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT segment, COUNT(*) FROM name_segment_vocab
             WHERE segment IN ({placeholders}) GROUP BY segment"
        );
        let mut stmt = self.db.conn().prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(segments.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn get_names_for_segment(&self, segment: &str, limit: usize) -> Result<Vec<String>> {
        let mut stmt = self.db.conn().prepare_cached(
            "SELECT name FROM name_segment_vocab
             WHERE segment = ? ORDER BY length(name), name LIMIT ?",
        )?;
        let rows = stmt.query_map(params![segment, limit as i64], |row| row.get(0))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// Names whose segments cover at least `min_words` distinct prompt words.
    pub fn get_segment_co_occurrence(
        &self,
        variants: &[(String, String)],
        min_words: usize,
        limit: usize,
    ) -> Result<Vec<(String, usize)>> {
        if variants.is_empty() {
            return Ok(Vec::new());
        }
        let variants_json = serde_json::to_string(
            &variants
                .iter()
                .map(|(segment, word)| serde_json::json!({ "segment": segment, "word": word }))
                .collect::<Vec<_>>(),
        )?;
        let mut stmt = self.db.conn().prepare(
            "WITH variants AS (
               SELECT json_extract(value, '$.segment') AS segment,
                      json_extract(value, '$.word') AS word
               FROM json_each(?)
             )
             SELECT vocab.name, COUNT(DISTINCT variants.word) AS matches
             FROM name_segment_vocab AS vocab
             JOIN variants ON variants.segment = vocab.segment
             GROUP BY vocab.name
             HAVING matches >= ?
             ORDER BY matches DESC, length(vocab.name), vocab.name
             LIMIT ?",
        )?;
        let rows = stmt.query_map(
            params![variants_json, min_words as i64, limit as i64],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize)),
        )?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }
}
