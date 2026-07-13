use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::types::Node;

/// Seed sets selected directly by the query and by nearby glue symbols.
pub(in crate::mcp::tools::explore) struct ExploreSeeds {
    pub glue_node_ids: HashSet<String>,
    pub named_seed_ids: HashSet<String>,
}

/// Nodes from one file plus the file-level relevance score used for ranking.
pub(in crate::mcp::tools::explore) struct FileGroup {
    pub nodes: Vec<Node>,
    pub score: i64,
}

/// Ranked file set and the intermediate sets that explain why files were kept.
pub(in crate::mcp::tools::explore) struct RankedExploreFiles {
    pub file_order: Vec<String>,
    pub file_groups: HashMap<String, FileGroup>,
    pub entry_node_ids: HashSet<String>,
    pub connected_to_entry: HashSet<String>,
    pub central_files: HashSet<String>,
    pub sorted_files: Vec<String>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "lowercase")]
pub(in crate::mcp::tools::explore) enum SourceChunkMode {
    Whole,
    Excerpt,
    Body,
    Signature,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct SourceChunk {
    pub start_line: usize,
    pub end_line: usize,
    pub mode: SourceChunkMode,
    pub symbols: Vec<String>,
    pub source: String,
    pub unicode_hazards: Vec<UnicodeHazard>,
}

impl SourceChunk {
    pub fn from_lines(
        lines: &[&str],
        start_line: i64,
        end_line: i64,
        mode: SourceChunkMode,
        mut symbols: Vec<String>,
    ) -> Option<Self> {
        let start_line = usize::try_from(start_line).ok()?;
        let requested_end = usize::try_from(end_line).ok()?;
        if start_line == 0 || requested_end < start_line || start_line > lines.len() {
            return None;
        }
        let end_line = requested_end.min(lines.len());
        let source = lines[start_line - 1..end_line].join("\n");
        if source.is_empty() {
            return None;
        }
        let mut seen = HashSet::new();
        symbols.retain(|symbol| seen.insert(symbol.clone()));
        let unicode_hazards = scan_unicode_hazards(&source, start_line);
        Some(Self {
            start_line,
            end_line,
            mode,
            symbols,
            source,
            unicode_hazards,
        })
    }
}

/// Category of a suspicious Unicode codepoint found verbatim in source.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::mcp::tools::explore) enum UnicodeHazardCategory {
    /// Bidirectional formatting/override control (e.g. U+202E).
    BidiControl,
    /// Zero-width or invisible format control (e.g. U+200B, U+FEFF).
    ZeroWidth,
    /// Private-use-area codepoint with no standard meaning.
    PrivateUse,
    /// Unicode noncharacter (e.g. U+FDD0, U+FFFE).
    Noncharacter,
    /// Disallowed C0/C1 control character (tab/newline/CR excluded).
    ControlChar,
}

/// A single suspicious codepoint located within a chunk's verbatim source.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct UnicodeHazard {
    pub codepoint: u32,
    pub line: usize,
    pub column: usize,
    pub category: UnicodeHazardCategory,
}

/// Classify a codepoint as a source-review hazard, or `None` for ordinary
/// text (including accented and CJK identifiers/comments).
fn classify_unicode_hazard(c: char) -> Option<UnicodeHazardCategory> {
    let cp = c as u32;
    if (cp <= 0x1F && !matches!(cp, 0x09 | 0x0A | 0x0D))
        || cp == 0x7F
        || (0x80..=0x9F).contains(&cp)
    {
        return Some(UnicodeHazardCategory::ControlChar);
    }
    if matches!(cp, 0x061C | 0x200E | 0x200F | 0x202A..=0x202E | 0x2066..=0x2069) {
        return Some(UnicodeHazardCategory::BidiControl);
    }
    if matches!(
        cp,
        0x00AD | 0x200B | 0x200C | 0x200D | 0x2060 | 0x2061 | 0x2062 | 0x2063 | 0x2064 | 0xFEFF
    ) {
        return Some(UnicodeHazardCategory::ZeroWidth);
    }
    if (0xE000..=0xF8FF).contains(&cp)
        || (0xF_0000..=0xF_FFFD).contains(&cp)
        || (0x10_0000..=0x10_FFFD).contains(&cp)
    {
        return Some(UnicodeHazardCategory::PrivateUse);
    }
    if (0xFDD0..=0xFDEF).contains(&cp) || (cp & 0xFFFE) == 0xFFFE {
        return Some(UnicodeHazardCategory::Noncharacter);
    }
    None
}

/// Scan verbatim source for suspicious codepoints, reporting each at its
/// absolute line (offset from `start_line`) and 1-based scalar column. The
/// source string is only read, never modified.
fn scan_unicode_hazards(source: &str, start_line: usize) -> Vec<UnicodeHazard> {
    let mut hazards = Vec::new();
    let mut line = start_line;
    let mut column = 1usize;
    for c in source.chars() {
        if c == '\n' {
            line += 1;
            column = 1;
            continue;
        }
        if let Some(category) = classify_unicode_hazard(c) {
            hazards.push(UnicodeHazard {
                codepoint: c as u32,
                line,
                column,
                category,
            });
        }
        column += 1;
    }
    hazards
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct StructuredSourceFile {
    pub path: String,
    pub language: String,
    pub chunks: Vec<SourceChunk>,
    pub source_truncated: bool,
}

/// Rendered source section ready to insert into a codegraph_explore response.
pub(in crate::mcp::tools::explore) struct RenderedFile {
    pub header: String,
    pub language: String,
    pub body: String,
    pub chunks: Vec<SourceChunk>,
    pub cost: usize,
}

impl RenderedFile {
    pub fn into_structured(self, path: &str) -> StructuredSourceFile {
        StructuredSourceFile {
            path: path.to_string(),
            language: self.language,
            chunks: self.chunks,
            source_truncated: false,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct ExploreRelationship {
    pub kind: String,
    pub source: String,
    pub target: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct ExploreAdditionalFile {
    pub path: String,
    pub symbols: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct ExploreLiteralLine {
    pub line_number: usize,
    pub text: String,
    pub terms: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct ExploreLiteralFile {
    pub file_path: String,
    pub language: String,
    pub lines: Vec<ExploreLiteralLine>,
}

/// Why a ranked file was not emitted with its source in this response.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::mcp::tools::explore) enum OmissionReason {
    /// The `maxFiles` cap was reached before this file's turn.
    MaxFiles,
    /// Including this file would exceed the output character budget.
    Budget,
    /// The file could not be read from disk (missing, moved, or outside root).
    Unavailable,
    /// No renderer produced any source for this file.
    NoSource,
}

/// A ranked file that was withheld, with the reason and its top symbols so the
/// agent can decide whether to query it explicitly.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct OmittedFile {
    pub path: String,
    pub reason: OmissionReason,
    pub symbols: Vec<String>,
}

/// Stateless follow-up hints derived from omitted files/symbols. No cursor.
#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct ExploreContinuation {
    pub suggested_queries: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools::explore) struct ExplorePayload<'a> {
    pub schema_version: u8,
    pub kind: &'static str,
    pub query: &'a str,
    pub total_symbols: usize,
    pub total_files: usize,
    pub files_included: usize,
    pub source_files: Vec<StructuredSourceFile>,
    pub relationships: Vec<ExploreRelationship>,
    pub additional_files: Vec<ExploreAdditionalFile>,
    pub literal_matches: Vec<ExploreLiteralFile>,
    pub trimmed: bool,
    pub files_omitted: usize,
    pub omissions: Vec<OmittedFile>,
    pub continuation: ExploreContinuation,
}

#[cfg(test)]
mod tests {
    use super::{SourceChunk, SourceChunkMode};

    #[test]
    fn source_chunk_rejects_empty_or_malformed_ranges() {
        assert!(SourceChunk::from_lines(&[], 1, 1, SourceChunkMode::Excerpt, Vec::new()).is_none());
        assert!(
            SourceChunk::from_lines(&["line"], 0, 1, SourceChunkMode::Excerpt, Vec::new())
                .is_none()
        );
        assert!(
            SourceChunk::from_lines(&["line"], 2, 1, SourceChunkMode::Excerpt, Vec::new())
                .is_none()
        );
    }
}
