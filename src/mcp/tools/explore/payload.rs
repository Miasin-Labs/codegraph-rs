use serde_json::Value;

use super::literal::LiteralContentMatches;
use super::types::{
    ExploreAdditionalFile,
    ExploreBackReference,
    ExploreContinuation,
    ExploreLiteralFile,
    ExploreLiteralLine,
    ExplorePayload,
    ExploreRelationship,
    OmittedFile,
    RankedExploreFiles,
    StructuredSourceFile,
};
use crate::error::{CodeGraphError, Result};
use crate::mcp::tools::format::{OrderedNodeMap, output_char_cap};
use crate::types::{Edge, EdgeKind};

pub(in crate::mcp::tools::explore) struct ExplorePayloadInput<'a> {
    pub query: &'a str,
    pub total_symbols: usize,
    pub total_files: usize,
    pub files_included: usize,
    pub source_files: Vec<StructuredSourceFile>,
    pub back_references: Vec<ExploreBackReference>,
    pub relationships: Vec<ExploreRelationship>,
    pub additional_files: Vec<ExploreAdditionalFile>,
    pub literal_matches: &'a LiteralContentMatches,
    pub trimmed: bool,
    pub omissions: Vec<OmittedFile>,
    pub max_output_chars: usize,
}

pub(in crate::mcp::tools::explore) fn explore_payload(
    input: ExplorePayloadInput<'_>,
) -> Result<Value> {
    let literal_matches = input
        .literal_matches
        .files
        .iter()
        .map(|file| ExploreLiteralFile {
            file_path: file.file_path.clone(),
            language: file.language.clone(),
            lines: file
                .lines
                .iter()
                .map(|line| ExploreLiteralLine {
                    line_number: line.line_number,
                    text: line.text.clone(),
                    terms: line.terms.clone(),
                })
                .collect(),
        })
        .collect();
    let files_omitted = input.omissions.len();
    let continuation = build_continuation(&input.omissions);
    let mut payload = ExplorePayload {
        schema_version: 2,
        kind: "explore",
        query: input.query,
        total_symbols: input.total_symbols,
        total_files: input.total_files,
        files_included: input.files_included,
        source_files: input.source_files,
        back_references: input.back_references,
        relationships: input.relationships,
        additional_files: input.additional_files,
        literal_matches,
        trimmed: input.trimmed || input.literal_matches.scan_was_truncated(),
        files_omitted,
        omissions: input.omissions,
        continuation,
    };
    if !cap_explore_payload(&mut payload, input.max_output_chars) {
        return Err(CodeGraphError::other(
            "Explore metadata exceeds the output budget after all optional evidence was withheld",
        ));
    }
    Ok(serde_json::to_value(payload)?)
}

/// Suggest stateless follow-up queries from omitted files' top symbols so the
/// agent can retrieve withheld source without any cursor state.
fn build_continuation(omissions: &[OmittedFile]) -> ExploreContinuation {
    let mut seen = std::collections::HashSet::new();
    let suggested_queries = omissions
        .iter()
        .filter(|file| !file.symbols.is_empty())
        .map(|file| {
            file.symbols
                .iter()
                .take(3)
                .cloned()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .filter(|query| !query.is_empty() && seen.insert(query.clone()))
        .take(4)
        .collect();
    ExploreContinuation { suggested_queries }
}

/// Shed low-value metadata, then whole evidence units, until the complete
/// serialized payload fits. Source and literal evidence remain verbatim.
fn cap_explore_payload(payload: &mut ExplorePayload<'_>, adaptive_cap: usize) -> bool {
    let cap = output_char_cap().map_or(adaptive_cap, |configured| configured.min(adaptive_cap));
    let serialized_len = |payload: &ExplorePayload<'_>| {
        serde_json::to_string(payload)
            .map(|value| value.len())
            .unwrap_or(0)
    };
    if serialized_len(payload) <= cap {
        return true;
    }

    if serialized_len(payload) > cap && !payload.additional_files.is_empty() {
        payload.additional_files.clear();
        payload.trimmed = true;
    }
    while serialized_len(payload) > cap && !payload.relationships.is_empty() {
        payload
            .relationships
            .truncate(payload.relationships.len().saturating_sub(16));
        payload.trimmed = true;
    }

    while serialized_len(payload) > cap {
        let mut target: Option<(usize, usize, usize)> = None;
        for (file_index, file) in payload.source_files.iter().enumerate() {
            for (chunk_index, chunk) in file.chunks.iter().enumerate() {
                let len = chunk.source.len();
                if target.is_none_or(|(_, _, best)| len > best) {
                    target = Some((file_index, chunk_index, len));
                }
            }
        }
        let Some((file_index, chunk_index, _)) = target else {
            break;
        };
        let file = &mut payload.source_files[file_index];
        file.chunks.remove(chunk_index);
        file.source_truncated = true;
        payload.trimmed = true;
    }

    while serialized_len(payload) > cap {
        let mut target: Option<(usize, usize, usize)> = None;
        for (file_index, file) in payload.literal_matches.iter().enumerate() {
            for (line_index, line) in file.lines.iter().enumerate() {
                let len = line.text.chars().count();
                if target.is_none_or(|(_, _, best)| len > best) {
                    target = Some((file_index, line_index, len));
                }
            }
        }
        let Some((file_index, line_index, _)) = target else {
            break;
        };
        payload.literal_matches[file_index].lines.remove(line_index);
        payload.trimmed = true;
    }

    payload
        .literal_matches
        .retain(|file| !file.lines.is_empty());

    if serialized_len(payload) > cap && !payload.continuation.suggested_queries.is_empty() {
        payload.continuation.suggested_queries.clear();
        payload.trimmed = true;
    }
    if serialized_len(payload) > cap
        && payload
            .omissions
            .iter()
            .any(|omission| !omission.symbols.is_empty())
    {
        for omission in &mut payload.omissions {
            omission.symbols.clear();
        }
        payload.trimmed = true;
    }
    while serialized_len(payload) > cap && payload.omissions.pop().is_some() {
        payload.trimmed = true;
    }
    serialized_len(payload) <= cap
}

pub(in crate::mcp::tools::explore) fn relationship_payloads(
    edges: &[Edge],
    nodes: &OrderedNodeMap,
    max_per_kind: usize,
) -> Vec<ExploreRelationship> {
    let mut counts = std::collections::HashMap::new();
    edges
        .iter()
        .filter_map(|edge| {
            if edge.kind == EdgeKind::Contains {
                return None;
            }
            let count = counts.entry(edge.kind.as_str()).or_insert(0usize);
            if *count >= max_per_kind {
                return None;
            }
            let source = nodes.get(&edge.source)?;
            let target = nodes.get(&edge.target)?;
            *count += 1;
            Some(ExploreRelationship {
                kind: edge.kind.as_str().to_string(),
                source: source.qualified_name.clone(),
                target: target.qualified_name.clone(),
            })
        })
        .collect()
}

pub(in crate::mcp::tools::explore) fn additional_file_payloads(
    ranked: &RankedExploreFiles,
    files_included: usize,
    max_files: usize,
    max_symbols_per_file: usize,
) -> Vec<ExploreAdditionalFile> {
    ranked
        .file_order
        .iter()
        .filter(|file_path| {
            !ranked
                .sorted_files
                .iter()
                .take(files_included)
                .any(|path| path == *file_path)
        })
        .take(max_files)
        .map(|file_path| ExploreAdditionalFile {
            path: file_path.clone(),
            symbols: ranked.file_groups[file_path]
                .nodes
                .iter()
                .take(max_symbols_per_file)
                .map(|node| node.name.clone())
                .collect(),
        })
        .collect()
}

#[cfg(test)]
mod tests;
