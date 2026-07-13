use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::format::{
    explore_line_numbers_enabled,
    get_explore_output_budget,
    num_or,
    ordered_nodes_from_subgraph,
};
use super::super::schema::ToolResult;
use super::literal::{append_literal_content_section, collect_literal_content_matches};
use super::payload::{
    ExplorePayloadInput,
    additional_file_payloads,
    explore_payload,
    relationship_payloads,
};
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
                    omissions: Vec::new(),
                })?;
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
                omissions: Vec::new(),
            })?;
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
            source_files: source_result.rendered_files,
            relationships: relationship_payloads(&edges, &nodes),
            additional_files: additional_file_payloads(&ranked, source_result.files_included),
            literal_matches: &literal_matches,
            trimmed: source_result.any_file_trimmed,
            omissions: source_result.omissions,
        })?;
        finish_explore_result(self, &flow.text, lines, Some(payload))
    }
}
