use std::collections::HashSet;

use super::pipeline::ExtractionOrchestrator;
use super::progress::{FileStats, now_ms};
use crate::error::Result;
use crate::types::{ExtractionResult, FileRecord, Language, Node, UnresolvedReference};
use crate::utils::sha256_hex;

/// Calculate SHA256 hash of file contents.
pub fn hash_content(content: &str) -> String {
    sha256_hex(content.as_bytes())
}

impl<'a> ExtractionOrchestrator<'a> {
    /// Store extraction result in database.
    pub(super) fn store_extraction_result(
        &self,
        file_path: &str,
        content: &str,
        language: Language,
        stats: &FileStats,
        result: &ExtractionResult,
    ) -> Result<()> {
        let content_hash = hash_content(content);

        // Check if file already exists and hasn't changed
        let existing_file = self.queries.get_file_by_path(file_path)?;
        if let Some(existing) = &existing_file {
            if existing.content_hash == content_hash {
                return Ok(()); // No changes
            }
        }

        // Delete existing data for this file
        if existing_file.is_some() {
            self.queries.delete_file(file_path)?;
        }

        // Filter out nodes with missing required fields before insertion.
        // This prevents FK violations when edges reference nodes that would
        // be silently skipped by insert_node() (see issue #42).
        let valid_nodes: Vec<Node> = result
            .nodes
            .iter()
            .filter(|n| !n.id.is_empty() && !n.name.is_empty() && !n.file_path.is_empty())
            .cloned()
            .collect();

        // Insert nodes
        if !valid_nodes.is_empty() {
            self.queries.insert_nodes(&valid_nodes)?;
        }

        let inserted_ids: HashSet<&str> = valid_nodes.iter().map(|n| n.id.as_str()).collect();

        // Filter edges to only reference nodes that were actually inserted
        if !result.edges.is_empty() {
            let valid_edges: Vec<crate::types::Edge> = result
                .edges
                .iter()
                .filter(|e| {
                    inserted_ids.contains(e.source.as_str())
                        && inserted_ids.contains(e.target.as_str())
                })
                .cloned()
                .collect();
            if !valid_edges.is_empty() {
                self.queries.insert_edges(&valid_edges)?;
            }
        }

        // Insert unresolved references in batch with denormalized filePath/language
        if !result.unresolved_references.is_empty() {
            let refs_with_context: Vec<UnresolvedReference> = result
                .unresolved_references
                .iter()
                .filter(|r| inserted_ids.contains(r.from_node_id.as_str()))
                .map(|r| {
                    let mut r = r.clone();
                    if r.file_path.is_none() {
                        r.file_path = Some(file_path.to_string());
                    }
                    if r.language.is_none() {
                        r.language = Some(language);
                    }
                    r
                })
                .collect();
            if !refs_with_context.is_empty() {
                self.queries
                    .insert_unresolved_refs_batch(&refs_with_context)?;
            }
        }

        // Insert file record
        let file_record = FileRecord {
            path: file_path.to_string(),
            content_hash,
            language,
            size: stats.size,
            modified_at: stats.modified_at_ms,
            indexed_at: now_ms(),
            node_count: result.nodes.len() as u32,
            errors: if result.errors.is_empty() {
                None
            } else {
                Some(result.errors.clone())
            },
        };
        self.queries.upsert_file(&file_record)
    }
}
