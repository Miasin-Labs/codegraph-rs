use serde_json::Value;

use super::literal::LiteralFileMatch;
use super::types::{
    ExploreAdditionalFile,
    ExploreContinuation,
    ExploreLiteralFile,
    ExploreLiteralLine,
    ExplorePayload,
    ExploreRelationship,
    OmittedFile,
    RankedExploreFiles,
    StructuredSourceFile,
};
use crate::error::Result;
use crate::mcp::tools::format::{OrderedNodeMap, output_char_cap};
use crate::types::{Edge, EdgeKind};

pub(in crate::mcp::tools::explore) struct ExplorePayloadInput<'a> {
    pub query: &'a str,
    pub total_symbols: usize,
    pub total_files: usize,
    pub files_included: usize,
    pub source_files: Vec<StructuredSourceFile>,
    pub relationships: Vec<ExploreRelationship>,
    pub additional_files: Vec<ExploreAdditionalFile>,
    pub literal_matches: &'a [LiteralFileMatch],
    pub trimmed: bool,
    pub omissions: Vec<OmittedFile>,
}

pub(in crate::mcp::tools::explore) fn explore_payload(
    input: ExplorePayloadInput<'_>,
) -> Result<Value> {
    let literal_matches = input
        .literal_matches
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
        relationships: input.relationships,
        additional_files: input.additional_files,
        literal_matches,
        trimmed: input.trimmed,
        files_omitted,
        omissions: input.omissions,
        continuation,
    };
    cap_explore_sources(&mut payload);
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

/// Under an opt-in output cap, shrink only chunk source text (marking the
/// affected file) so omission and continuation metadata always survives.
fn cap_explore_sources(payload: &mut ExplorePayload<'_>) {
    let Some(cap) = output_char_cap() else {
        return;
    };
    for _ in 0..64 {
        let size = serde_json::to_string(payload).map(|s| s.len()).unwrap_or(0);
        if size <= cap {
            return;
        }
        let mut target: Option<(usize, usize, usize)> = None;
        for (fi, file) in payload.source_files.iter().enumerate() {
            for (ci, chunk) in file.chunks.iter().enumerate() {
                let len = chunk.source.chars().count();
                if target.is_none_or(|(_, _, best)| len > best) {
                    target = Some((fi, ci, len));
                }
            }
        }
        let Some((fi, ci, len)) = target else {
            return;
        };
        if len <= 24 {
            return;
        }
        let keep = (len / 2).max(24);
        let file = &mut payload.source_files[fi];
        let chunk = &mut file.chunks[ci];
        let mut shortened: String = chunk.source.chars().take(keep).collect();
        shortened.push_str("... [truncated]");
        chunk.source = shortened;
        file.source_truncated = true;
    }
}

pub(in crate::mcp::tools::explore) fn relationship_payloads(
    edges: &[Edge],
    nodes: &OrderedNodeMap,
) -> Vec<ExploreRelationship> {
    edges
        .iter()
        .filter(|edge| edge.kind != EdgeKind::Contains)
        .filter_map(|edge| {
            let source = nodes.get(&edge.source)?;
            let target = nodes.get(&edge.target)?;
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
        .take(20)
        .map(|file_path| ExploreAdditionalFile {
            path: file_path.clone(),
            symbols: ranked.file_groups[file_path]
                .nodes
                .iter()
                .map(|node| node.name.clone())
                .collect(),
        })
        .collect()
}
