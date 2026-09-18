//! codegraph_files handler and list renderers.
//!
//! The structured payload lists files grouped by directory, honours
//! `maxDepth` (deeper directories collapse into file counts), and pages to
//! the MCP output budget with a `nextCursor`. Without `maxDepth` the server
//! picks the deepest level whose listing fits, so a whole-repo call on a
//! 70K-file tree returns a map, not megabytes.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{LEADING_DOT_SLASH_RE, json_len, locale_cmp, mcp_output_budget};
use super::super::output::{FileGroupOutput, FilesOutput};
use super::super::schema::ToolResult;
use super::glob_to_regex;
use super::listing::{Cursor, IndexedFile, Listing, listing_key};
use crate::error::Result;
use crate::utils::clamp;

/// Payload keys a page adds besides its directories (`maxDepth`,
/// `autoDepth`, `truncated`, `nextCursor`).
const PAGE_OVERHEAD: usize = 128;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_files(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let path_filter = args.get("path").and_then(|v| v.as_str());
        let pattern = args.get("pattern").and_then(|v| v.as_str());
        let format = args
            .get("format")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("tree");
        let include_metadata = args.get("includeMetadata") != Some(&Value::Bool(false));
        let requested_depth: Option<usize> = match args.get("maxDepth") {
            None | Some(Value::Null) => None,
            Some(v) => v.as_f64().map(|d| clamp(d, 1.0, 20.0) as usize),
        };
        let cursor = match args.get("cursor") {
            None | Some(Value::Null) => None,
            Some(Value::String(raw)) if !raw.trim().is_empty() => Some(raw.as_str()),
            Some(_) => {
                return Ok(self.validation_error_result(
                    "cursor",
                    "cursor must be the `nextCursor` string of a previous codegraph_files page",
                    "string",
                    None,
                ));
            }
        };

        let all_files = cg.get_files()?;

        // Filter by path prefix, normalizing root-ish and Windows-style
        // variants (#426).
        let normalized_filter = normalized_path_filter(path_filter).unwrap_or_default();
        let mut files: Vec<&crate::types::FileRecord> = all_files
            .iter()
            .filter(|f| {
                normalized_filter.is_empty()
                    || f.path == normalized_filter
                    || f.path.starts_with(&format!("{normalized_filter}/"))
            })
            .collect();

        // Filter by glob pattern
        if let Some(pattern) = pattern.filter(|p| !p.is_empty()) {
            let regex = glob_to_regex(pattern)?;
            files.retain(|f| regex.is_match(&f.path));
        }

        if files.is_empty() {
            return self.structured_result("Files: 0", &FilesOutput::empty());
        }

        let key = listing_key(&normalized_filter, pattern.unwrap_or_default());
        let cursor = match cursor.map(|raw| Cursor::decode(raw, &key)) {
            None => None,
            Some(Some(cursor)) => Some(cursor),
            Some(None) => {
                return Ok(self.validation_error_result(
                    "cursor",
                    "cursor does not belong to this listing — pass it with the same `path` and \
                     `pattern` as the call that returned it, or drop it to start over",
                    "the nextCursor of a previous page",
                    Some("unknown string"),
                ));
            }
        };

        let indexed: Vec<IndexedFile<'_>> = files
            .iter()
            .map(|f| IndexedFile {
                path: f.path.as_str(),
                symbols: f.node_count,
            })
            .collect();
        let languages: Vec<&str> = files.iter().map(|f| f.language.as_str()).collect();
        let mut payload = FilesOutput::empty();
        payload.total = files.len();
        payload.groups = file_groups(&languages);
        let budget = mcp_output_budget();
        let room = budget.saturating_sub(json_len(&payload) + PAGE_OVERHEAD);

        let (depth, auto_depth, offset) = match cursor {
            Some(cursor) => (cursor.depth, cursor.auto_depth, cursor.offset),
            None => match requested_depth {
                Some(depth) => (Some(depth), false, 0),
                None => match fitting_depth(&indexed, &normalized_filter, room) {
                    Some(depth) => (Some(depth), true, 0),
                    None => (None, false, 0),
                },
            },
        };
        let listing = Listing::build(&indexed, &normalized_filter, depth);
        let offset = offset.min(listing.len());
        let mut end = listing.page_end(offset, room);
        payload.max_depth = depth;
        payload.auto_depth = auto_depth;
        loop {
            payload.dirs = listing.dirs(offset, end);
            payload.truncated = end < listing.len();
            payload.next_cursor = payload.truncated.then(|| {
                Cursor {
                    depth,
                    auto_depth,
                    offset: end,
                }
                .encode(&key)
            });
            if json_len(&payload) <= budget || end <= offset + 1 {
                break;
            }
            end -= 1;
        }

        // The human-readable rendering (CLI, tests) follows the same depth.
        let base_depth = normalized_filter
            .split('/')
            .filter(|s| !s.is_empty())
            .count();
        let triples: Vec<(&str, &str, u32)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.language.as_str(), f.node_count))
            .collect();
        let output = match format {
            "flat" => self.format_files_flat(&triples, include_metadata),
            "grouped" => self.format_files_grouped(&triples, include_metadata),
            _ => self.format_files_tree(
                &triples,
                include_metadata,
                depth.map(|depth| depth + base_depth),
            ),
        };
        self.structured_result(&self.truncate_output(&output), &payload)
    }

    /// Format files as a flat list.
    fn format_files_flat(&self, files: &[(&str, &str, u32)], include_metadata: bool) -> String {
        let mut lines: Vec<String> = vec![format!("## Files ({})", files.len()), String::new()];

        let mut sorted: Vec<&(&str, &str, u32)> = files.iter().collect();
        sorted.sort_by(|a, b| locale_cmp(a.0, b.0));
        for (path, language, node_count) in sorted {
            if include_metadata {
                lines.push(format!("- {path} ({language}, {node_count} symbols)"));
            } else {
                lines.push(format!("- {path}"));
            }
        }

        lines.join("\n")
    }

    /// Format files grouped by language.
    fn format_files_grouped(&self, files: &[(&str, &str, u32)], include_metadata: bool) -> String {
        let mut lang_order: Vec<String> = Vec::new();
        let mut by_lang: HashMap<String, Vec<(&str, &str, u32)>> = HashMap::new();
        for f in files {
            if !by_lang.contains_key(f.1) {
                lang_order.push(f.1.to_string());
            }
            by_lang.entry(f.1.to_string()).or_default().push(*f);
        }

        let mut lines: Vec<String> = vec![
            format!("## Files by Language ({} total)", files.len()),
            String::new(),
        ];

        // Sort languages by file count (descending), stable.
        let mut sorted_langs = lang_order;
        sorted_langs.sort_by(|a, b| by_lang[b].len().cmp(&by_lang[a].len()));

        for lang in &sorted_langs {
            let lang_files = &by_lang[lang];
            lines.push(format!("### {} ({})", lang, lang_files.len()));
            let mut sorted: Vec<&(&str, &str, u32)> = lang_files.iter().collect();
            sorted.sort_by(|a, b| locale_cmp(a.0, b.0));
            for (path, _language, node_count) in sorted {
                if include_metadata {
                    lines.push(format!("- {path} ({node_count} symbols)"));
                } else {
                    lines.push(format!("- {path}"));
                }
            }
            lines.push(String::new());
        }

        lines.join("\n")
    }
}

/// With no `maxDepth`: `None` when the whole listing fits `room`, else the
/// deepest level whose listing still fits (at least 1 — a first level that
/// is itself too big is paged instead).
fn fitting_depth(files: &[IndexedFile<'_>], base: &str, room: usize) -> Option<usize> {
    if Listing::build(files, base, None).cost() <= room {
        return None;
    }
    let deepest = Listing::deepest(files, base);
    let mut chosen = 1;
    for depth in 2..deepest {
        if Listing::build(files, base, Some(depth)).cost() > room {
            break;
        }
        chosen = depth;
    }
    Some(chosen)
}

fn normalized_path_filter(path_filter: Option<&str>) -> Option<String> {
    match path_filter {
        Some(pf) if !pf.is_empty() => {
            let s = pf.replace('\\', "/");
            let s = LEADING_DOT_SLASH_RE.replace(&s, "").to_string();
            let s = if s == "." { String::new() } else { s };
            let normalized = s.trim_end_matches('/').to_string();
            if normalized.is_empty() {
                None
            } else {
                Some(normalized)
            }
        }
        _ => None,
    }
}

fn file_groups(languages: &[&str]) -> Vec<FileGroupOutput> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for language in languages {
        *counts.entry(language).or_default() += 1;
    }
    let mut groups: Vec<FileGroupOutput> = counts
        .into_iter()
        .map(|(language, count)| FileGroupOutput {
            language: language.to_string(),
            count,
        })
        .collect();
    groups.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| locale_cmp(&a.language, &b.language))
    });
    groups
}
