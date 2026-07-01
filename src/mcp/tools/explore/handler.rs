use serde_json::{Map, Value, json};

use super::super::context::ToolHandler;
use super::super::format::{
    explore_line_numbers_enabled,
    get_explore_output_budget,
    num_or,
    ordered_nodes_from_subgraph,
};
use super::super::schema::ToolResult;
use super::literal::{append_literal_content_section, collect_literal_content_matches};
use super::relationships::{
    append_explore_footer,
    append_graph_sections,
    append_remaining_files,
    finish_explore_result,
};
use super::source::{SourceFilesRequest, render_source_files};
use crate::error::Result;
use crate::types::FindRelevantContextOptions;
use crate::utils::clamp;

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_explore(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let query = match self.validate_string(args.get("query"), "query") {
            Ok(q) => q,
            Err(r) => return Ok(r),
        };

        let cg = self.get_code_graph(args.get("projectPath").and_then(|v| v.as_str()))?;
        let project_root = cg.get_project_root().to_path_buf();
        let budget = match cg.get_stats() {
            Ok(stats) => get_explore_output_budget(stats.file_count),
            Err(_) => get_explore_output_budget(u64::MAX),
        };
        let max_files = clamp(
            num_or(args, "maxFiles", budget.default_max_files as f64),
            1.0,
            20.0,
        ) as usize;
        let with_line_numbers = explore_line_numbers_enabled();
        let literal_matches = collect_literal_content_matches(&cg, &project_root, &query)?;

        let subgraph = cg.find_relevant_context(
            &query,
            Some(&FindRelevantContextOptions {
                search_limit: Some(8),
                traversal_depth: Some(3),
                max_nodes: Some(200),
                min_score: Some(0.2),
                ..Default::default()
            }),
        )?;
        if subgraph.nodes.is_empty() {
            if !literal_matches.is_empty() {
                let literal_file_count = literal_matches.len();
                let literal_line_count: usize = literal_matches.iter().map(|m| m.lines.len()).sum();
                let mut lines = vec![
                    format!("## Exploration: {query}"),
                    String::new(),
                    format!(
                        "Found 0 symbols, plus {literal_line_count} literal content hit(s) across {literal_file_count} indexed file(s)."
                    ),
                    String::new(),
                ];
                append_literal_content_section(&literal_matches, &mut lines);
                let payload = explore_payload(ExplorePayloadInput {
                    query: &query,
                    total_symbols: 0,
                    total_files: 0,
                    files_included: 0,
                    source_files: Vec::new(),
                    relationships: Vec::new(),
                    additional_files: Vec::new(),
                    literal_matches: &literal_matches,
                    trimmed: false,
                });
                return finish_explore_result(self, "", lines, Some(payload));
            }
            let payload = explore_payload(ExplorePayloadInput {
                query: &query,
                total_symbols: 0,
                total_files: 0,
                files_included: 0,
                source_files: Vec::new(),
                relationships: Vec::new(),
                additional_files: Vec::new(),
                literal_matches: &[],
                trimmed: false,
            });
            return finish_explore_result(
                self,
                "",
                vec![format!("No relevant code found for `{query}`")],
                Some(payload),
            );
        }

        let roots = subgraph.roots.clone();
        let edges = subgraph.edges.clone();
        let mut nodes = ordered_nodes_from_subgraph(&subgraph);
        let seeds = self.collect_explore_seeds(&cg, &query, &roots, &mut nodes)?;
        let ranked = self.rank_explore_files(&query, &roots, &edges, &nodes, &seeds.named_seed_ids);

        let literal_file_count = literal_matches.len();
        let literal_line_count: usize = literal_matches.iter().map(|m| m.lines.len()).sum();
        let summary = if literal_matches.is_empty() {
            format!(
                "Found {} symbols across {} files.",
                nodes.len(),
                ranked.file_order.len()
            )
        } else {
            format!(
                "Found {} symbols across {} files, plus {literal_line_count} literal content hit(s) across {literal_file_count} indexed file(s).",
                nodes.len(),
                ranked.file_order.len()
            )
        };
        let mut lines: Vec<String> = vec![
            format!("## Exploration: {query}"),
            String::new(),
            summary,
            String::new(),
        ];
        append_graph_sections(
            self,
            &cg,
            &roots,
            &edges,
            &nodes,
            &seeds.named_seed_ids,
            budget,
            &mut lines,
        );

        let flow = self.build_flow_from_named_symbols(&cg, &query);
        append_literal_content_section(&literal_matches, &mut lines);
        lines.push("### Source Code".to_string());
        lines.push(String::new());
        lines.push("> The code below is the **verbatim, current on-disk source** of these files — re-read from disk on this call and line-numbered, byte-for-byte identical to what the Read tool returns. It is NOT a summary, outline, or stale cache. Treat each block as a Read you have already performed: do not Read a file shown here.".to_string());
        lines.push(String::new());

        let initial_chars = lines.join("\n").len() + flow.text.len();
        let source_result = render_source_files(
            SourceFilesRequest {
                cg: &cg,
                project_root: &project_root,
                ranked: &ranked,
                nodes: &nodes,
                glue_node_ids: &seeds.glue_node_ids,
                flow: &flow,
                budget,
                max_files,
                with_line_numbers,
                initial_chars,
            },
            &mut lines,
        )?;
        append_remaining_files(budget, &ranked, source_result.files_included, &mut lines);
        append_explore_footer(
            self,
            &cg,
            budget,
            source_result.files_included,
            source_result.any_file_trimmed,
            &mut lines,
        );
        let payload = explore_payload(ExplorePayloadInput {
            query: &query,
            total_symbols: nodes.len(),
            total_files: ranked.file_order.len(),
            files_included: source_result.files_included,
            source_files: source_result
                .rendered_files
                .iter()
                .map(|file| {
                    json!({
                        "path": file.path,
                        "language": file.language,
                        "header": file.header,
                        "body": file.body,
                    })
                })
                .collect(),
            relationships: relationship_payloads(&edges, &nodes),
            additional_files: additional_file_payloads(&ranked, source_result.files_included),
            literal_matches: &literal_matches,
            trimmed: source_result.any_file_trimmed,
        });
        finish_explore_result(self, &flow.text, lines, Some(payload))
    }
}

struct ExplorePayloadInput<'a> {
    query: &'a str,
    total_symbols: usize,
    total_files: usize,
    files_included: usize,
    source_files: Vec<Value>,
    relationships: Vec<Value>,
    additional_files: Vec<Value>,
    literal_matches: &'a [super::literal::LiteralFileMatch],
    trimmed: bool,
}

fn explore_payload(input: ExplorePayloadInput<'_>) -> Value {
    let literal_matches = input
        .literal_matches
        .iter()
        .map(|file| {
            json!({
                "filePath": file.file_path,
                "language": file.language,
                "lines": file.lines.iter().map(|line| json!({
                    "lineNumber": line.line_number,
                    "text": line.text,
                    "terms": line.terms,
                })).collect::<Vec<_>>()
            })
        })
        .collect::<Vec<_>>();
    json!({
        "schemaVersion": 1,
        "kind": "explore",
        "query": input.query,
        "totalSymbols": input.total_symbols,
        "totalFiles": input.total_files,
        "filesIncluded": input.files_included,
        "sourceFiles": input.source_files,
        "relationships": input.relationships,
        "additionalFiles": input.additional_files,
        "literalMatches": literal_matches,
        "trimmed": input.trimmed,
    })
}

fn relationship_payloads(
    edges: &[crate::types::Edge],
    nodes: &super::super::format::OrderedNodeMap,
) -> Vec<Value> {
    edges
        .iter()
        .filter(|edge| edge.kind != crate::types::EdgeKind::Contains)
        .filter_map(|edge| {
            let source = nodes.get(&edge.source)?;
            let target = nodes.get(&edge.target)?;
            Some(json!({
                "kind": edge.kind.as_str(),
                "source": source.qualified_name.clone(),
                "target": target.qualified_name.clone(),
            }))
        })
        .collect()
}

fn additional_file_payloads(
    ranked: &super::types::RankedExploreFiles,
    files_included: usize,
) -> Vec<Value> {
    ranked
        .file_order
        .iter()
        .filter(|file_path| {
            !ranked
                .sorted_files
                .iter()
                .take(files_included)
                .any(|p| p == *file_path)
        })
        .take(20)
        .map(|file_path| {
            let symbols = ranked.file_groups[file_path]
                .nodes
                .iter()
                .map(|node| node.name.clone())
                .collect::<Vec<_>>();
            json!({
                "path": file_path,
                "symbols": symbols,
            })
        })
        .collect()
}
