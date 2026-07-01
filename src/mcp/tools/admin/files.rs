//! codegraph_files handler and list renderers.

use std::collections::HashMap;

use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{LEADING_DOT_SLASH_RE, locale_cmp};
use super::super::output::{FileGroupOutput, FileOutput, FilesOutput};
use super::super::schema::ToolResult;
use super::glob_to_regex;
use crate::error::Result;
use crate::utils::clamp;

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
        let max_depth: Option<usize> = match args.get("maxDepth") {
            None | Some(Value::Null) => None,
            Some(v) => v.as_f64().map(|d| clamp(d, 1.0, 20.0) as usize),
        };

        // Get all files from the index
        let all_files: Vec<FileOutput> = cg
            .get_files()?
            .into_iter()
            .map(|f| FileOutput {
                path: f.path,
                language: f.language.as_str().to_string(),
                node_count: f.node_count,
            })
            .collect();

        if all_files.is_empty() {
            let output = FilesOutput {
                schema_version: 1,
                kind: "files",
                path_filter: normalized_path_filter(path_filter),
                pattern: pattern.map(str::to_string),
                format: format.to_string(),
                total: 0,
                files: Vec::new(),
                groups: Vec::new(),
            };
            return self.structured_result("Files: 0", &output);
        }

        // Filter by path prefix, normalizing root-ish and Windows-style
        // variants (#426).
        let normalized_filter = normalized_path_filter(path_filter).unwrap_or_default();
        let mut files: Vec<&FileOutput> = if !normalized_filter.is_empty() {
            all_files
                .iter()
                .filter(|f| {
                    f.path == normalized_filter
                        || f.path.starts_with(&format!("{normalized_filter}/"))
                })
                .collect()
        } else {
            all_files.iter().collect()
        };

        // Filter by glob pattern
        if let Some(pattern) = pattern.filter(|p| !p.is_empty()) {
            let regex = glob_to_regex(pattern)?;
            files.retain(|f| regex.is_match(&f.path));
        }

        if files.is_empty() {
            let output = FilesOutput {
                schema_version: 1,
                kind: "files",
                path_filter: normalized_path_filter(path_filter),
                pattern: pattern.map(str::to_string),
                format: format.to_string(),
                total: 0,
                files: Vec::new(),
                groups: Vec::new(),
            };
            return self.structured_result("Files: 0", &output);
        }

        let triples: Vec<(&str, &str, u32)> = files
            .iter()
            .map(|f| (f.path.as_str(), f.language.as_str(), f.node_count))
            .collect();
        let payload_files: Vec<FileOutput> = files.iter().map(|f| (*f).clone()).collect();
        let groups = file_groups(&payload_files);

        let output = match format {
            "flat" => self.format_files_flat(&triples, include_metadata),
            "grouped" => self.format_files_grouped(&triples, include_metadata),
            _ => self.format_files_tree(&triples, include_metadata, max_depth),
        };
        let payload = FilesOutput {
            schema_version: 1,
            kind: "files",
            path_filter: normalized_path_filter(path_filter),
            pattern: pattern.map(str::to_string),
            format: format.to_string(),
            total: payload_files.len(),
            files: payload_files,
            groups,
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

fn file_groups(files: &[FileOutput]) -> Vec<FileGroupOutput> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for file in files {
        *counts.entry(file.language.clone()).or_default() += 1;
    }
    let mut groups: Vec<FileGroupOutput> = counts
        .into_iter()
        .map(|(language, count)| FileGroupOutput { language, count })
        .collect();
    groups.sort_by(|a, b| {
        b.count
            .cmp(&a.count)
            .then_with(|| locale_cmp(&a.language, &b.language))
    });
    groups
}
