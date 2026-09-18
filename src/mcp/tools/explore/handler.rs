use serde_json::{Map, Value};

use super::super::context::ToolHandler;
use super::super::context::notices::stale_slice_notice;
use super::super::format::{
    explore_line_numbers_enabled,
    get_explore_output_budget,
    num_or,
    ordered_nodes_from_subgraph,
};
use super::super::schema::ToolResult;
use super::execution::ExploreExecutionBudget;
use super::external::plan_external;
use super::literal::{append_literal_content_section, collect_literal_content_matches};
use super::payload::{
    ExplorePayloadInput,
    additional_file_payloads,
    explore_payload,
    relationship_payloads,
};
use super::related::{
    CoChangeProbe,
    RelatedPlan,
    RelatedRequest,
    plan_related_files,
    related_windows,
};
use super::relationships::{
    append_explore_footer,
    append_graph_sections,
    append_remaining_files,
    finish_explore_result,
};
use super::source::{SourceFilesRequest, render_source_files};
use crate::error::Result;
use crate::mcp::explore_session::{ProjectState, SESSION_ARG};
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
        // Recent commit history for related-file co-change, read while the
        // search below runs; dropped (and the process killed) if unused.
        let history = CoChangeProbe::spawn(&project_root);
        let prior = args
            .get(SESSION_ARG)
            .and_then(|value| serde_json::from_value::<ProjectState>(value.clone()).ok());
        let budget = match cg.get_stats() {
            Ok(stats) => get_explore_output_budget(stats.file_count),
            Err(_) => get_explore_output_budget(u64::MAX),
        };
        let max_files = clamp(
            num_or(args, "maxFiles", budget.default_max_files as f64),
            1.0,
            20.0,
        ) as usize;
        let execution_budget = ExploreExecutionBudget::default();
        let with_line_numbers = explore_line_numbers_enabled();
        let literal_matches = collect_literal_content_matches(
            &cg,
            &project_root,
            &query,
            execution_budget.literal_scan,
        )?;
        crate::graph::cancel::check()?;

        let subgraph = cg.find_relevant_context(
            &query,
            Some(&FindRelevantContextOptions {
                search_limit: Some(execution_budget.search_limit),
                traversal_depth: Some(execution_budget.traversal_depth),
                max_nodes: Some(execution_budget.max_nodes),
                min_score: Some(0.2),
                ..Default::default()
            }),
        )?;
        crate::graph::cancel::check()?;
        if subgraph.nodes.is_empty() {
            if !literal_matches.is_empty() {
                let mut lines = vec![
                    format!("## Exploration: {query}"),
                    String::new(),
                    format!(
                        "Found 0 symbols, plus {} literal content hit(s) across {} indexed file(s).",
                        literal_matches.total_matches, literal_matches.total_files
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
                    back_references: Vec::new(),
                    relationships: Vec::new(),
                    additional_files: Vec::new(),
                    related_files: Vec::new(),
                    related_windows: Vec::new(),
                    external: Vec::new(),
                    literal_matches: &literal_matches,
                    trimmed: false,
                    omissions: Vec::new(),
                    max_output_chars: budget.max_output_chars,
                })?;
                return finish_explore_result(self, "", lines, Some(payload));
            }
            let payload = explore_payload(ExplorePayloadInput {
                query: &query,
                total_symbols: 0,
                total_files: 0,
                files_included: 0,
                source_files: Vec::new(),
                back_references: Vec::new(),
                relationships: Vec::new(),
                additional_files: Vec::new(),
                related_files: Vec::new(),
                related_windows: Vec::new(),
                external: Vec::new(),
                literal_matches: &literal_matches,
                trimmed: false,
                omissions: Vec::new(),
                max_output_chars: budget.max_output_chars,
            })?;
            let mut lines = vec![format!("No relevant symbols found for `{query}`")];
            if let Some(message) = literal_matches.scan_truncation_message() {
                lines.push(String::new());
                lines.push(format!("> {message}"));
            } else {
                lines[0] = format!("No relevant code found for `{query}`");
            }
            return finish_explore_result(self, "", lines, Some(payload));
        }

        let roots = subgraph.roots.clone();
        let edges = subgraph.edges.clone();
        let mut nodes = ordered_nodes_from_subgraph(&subgraph);
        crate::graph::cancel::check()?;
        let seeds = self.collect_explore_seeds(
            &cg,
            &query,
            &roots,
            &mut nodes,
            execution_budget.named_seed_token_limit,
        )?;
        crate::graph::cancel::check()?;
        let ranked =
            self.rank_explore_files(&cg, &query, &roots, &edges, &nodes, &seeds.named_seed_ids)?;

        let summary = if literal_matches.is_empty() {
            format!(
                "Found {} symbols across {} files.",
                nodes.len(),
                ranked.file_order.len()
            )
        } else {
            format!(
                "Found {} symbols across {} files, plus {} literal content hit(s) across {} indexed file(s).",
                nodes.len(),
                ranked.file_order.len(),
                literal_matches.total_matches,
                literal_matches.total_files
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

        let related = match plan_related_files(
            &RelatedRequest {
                cg: &cg,
                query: &query,
                ranked: &ranked,
                budget,
            },
            history,
        ) {
            Ok(plan) => plan,
            Err(_) => {
                // Related files are an addition, not the answer: a failed
                // read drops them, a cancelled call still stops here.
                crate::graph::cancel::check()?;
                RelatedPlan::empty()
            }
        };
        // What the files about to be shown call in dependencies and linked
        // projects (followed into those graphs).
        let fed = self.federation();
        let external = plan_external(fed.as_ref(), &cg, &ranked, max_files);
        // Source gives up the room the related and external rows (and the
        // related windows) need, so the whole answer stays inside the one
        // explore budget.
        let mut source_budget = budget;
        source_budget.max_output_chars = budget
            .max_output_chars
            .saturating_sub(related.reserve + external.reserve);

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
                budget: source_budget,
                max_files,
                with_line_numbers,
                initial_chars,
                prior: prior.as_ref(),
            },
            &mut lines,
        )?;
        related.append_markdown(&mut lines);
        external.append_markdown(&mut lines);
        append_remaining_files(budget, &ranked, source_result.files_included, &mut lines);
        append_explore_footer(
            self,
            &cg,
            budget,
            source_result.files_included,
            source_result.any_file_trimmed,
            &mut lines,
        );
        let stale_files = source_result.stale_files.clone();
        let windows = related_windows(&cg, &project_root, &related, prior.as_ref());
        let payload = explore_payload(ExplorePayloadInput {
            query: &query,
            total_symbols: nodes.len(),
            total_files: ranked.file_order.len(),
            files_included: source_result.files_included,
            source_files: source_result.rendered_files,
            back_references: source_result.back_references,
            relationships: if budget.include_relationships {
                relationship_payloads(&edges, &nodes, budget.max_edges_per_relationship_kind)
            } else {
                Vec::new()
            },
            additional_files: if budget.include_additional_files {
                additional_file_payloads(
                    &ranked,
                    source_result.files_included,
                    20,
                    budget.max_symbols_in_file_header,
                )
            } else {
                Vec::new()
            },
            related_files: related.rows(),
            related_windows: windows,
            external: external.rows,
            literal_matches: &literal_matches,
            trimmed: source_result.any_file_trimmed,
            omissions: source_result.omissions,
            max_output_chars: budget.max_output_chars,
        })?;
        let mut result = finish_explore_result(self, &flow.text, lines, Some(payload))?;
        if !stale_files.is_empty() {
            result = result.with_notice(stale_slice_notice(&stale_files));
        }
        Ok(result)
    }
}
