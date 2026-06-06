//! Token-budgeted, dataflow-seeded context packing for the CLI `context`
//! command — the glue between the host index and the analysis engine's
//! context modules (`codegraph_analysis::context::{budget, clustering,
//! dataflow_seed, expansion, heuristics, measure, render, resolver,
//! retrieval_gate}`).
//!
//! Pipeline (`--strategy analysis`):
//! 1. The caller bridges the SQLite index into an analysis graph
//!    ([`crate::analysis_bridge::build_analysis_graph`]).
//! 2. [`codegraph_analysis::context::build_context`] resolves the task's
//!    identifiers ([`resolver`]), seeds entry points from their dataflow
//!    dependencies ([`dataflow_seed`], DRACO), gates the related-node
//!    expansion ([`retrieval_gate`], Repoformer) and runs the BFS +
//!    type-hierarchy expansion ([`expansion`]) when the gate retrieves.
//! 3. The selection is rendered with [`render::render_context`], and the
//!    entry/related symbols' source is appended as per-file clustered
//!    slices ([`clustering`]) under the engine's adaptive
//!    [`ExploreBudget`] caps.
//! 4. The output is trimmed to the requested token budget
//!    ([`trim_to_token_budget`], ~4 chars/token — the same heuristic the
//!    engine's own token-budgeted formatter uses) and measured.
//!
//! ## Honest capability
//!
//! The bridged graph carries call and type-usage edges only — the index
//! stores no value-level dataflow and no byte ranges. When the resolved
//! seed symbols have no type-flow edges (`uses_type`/`returns`/`type_of`)
//! to follow, dataflow seeding degrades to call-graph seeding and the
//! report says so ([`AnalysisContextReport::seeding`], plus a note); it
//! never fabricates dataflow that isn't in the graph.
//!
//! CLI ONLY — the MCP tool surface does not use this module.
//!
//! [`resolver`]: codegraph_analysis::context::resolver
//! [`dataflow_seed`]: codegraph_analysis::context::dataflow_seed
//! [`retrieval_gate`]: codegraph_analysis::context::retrieval_gate
//! [`expansion`]: codegraph_analysis::context::expansion
//! [`clustering`]: codegraph_analysis::context::clustering
//! [`render::render_context`]: codegraph_analysis::context::render::render_context

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use codegraph_analysis::capabilities::{Capability, CapabilityTree};
use codegraph_analysis::context::budget::ExploreBudget;
use codegraph_analysis::context::{
    ContextOptions as EngineContextOptions,
    build_context as engine_build_context,
    clustering,
    measure,
    render,
    resolve_symbol,
};
use codegraph_analysis::edges::EdgeKind as AEdgeKind;
use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::nodes::{NodeId as ANodeId, NodeKind as ANodeKind};
use codegraph_analysis::partial::{self, PartialStructError};
use serde::Serialize;

use crate::analysis_bridge::UNRESOLVED_FILE;

// =============================================================================
// Token measurement
// =============================================================================

/// Rough chars-per-token heuristic — the same estimate the analysis
/// engine's token-budgeted formatter uses (`formatting.rs`, chars / 4).
pub const CHARS_PER_TOKEN: usize = 4;

/// Measure a rendered output in (estimated) tokens.
pub fn measure_tokens(text: &str) -> usize {
    text.len().div_ceil(CHARS_PER_TOKEN)
}

/// Note appended when output is cut to a token budget. Its length is
/// reserved inside the budget, so the trimmed output (note included)
/// never exceeds `budget_tokens`.
const TRIM_NOTE: &str =
    "\n\n> Output trimmed to the requested token budget — narrow the task to see more.";

/// Trim `text` to roughly `budget_tokens` tokens (`budget_tokens * 4`
/// chars). Returns `(text, false)` unchanged when it already fits.
///
/// The cut lands on a UTF-8 boundary, prefers a newline in the last ~8 %
/// of the kept span (cosmetic), and reserves room for [`TRIM_NOTE`] so the
/// result measures within −10 % / +0 % of the budget — tight enough that
/// `measure_tokens(trimmed)` is always within ±10 % of `budget_tokens`
/// for budgets comfortably larger than the note itself.
pub fn trim_to_token_budget(text: &str, budget_tokens: usize) -> (String, bool) {
    let max_chars = budget_tokens.saturating_mul(CHARS_PER_TOKEN);
    if text.len() <= max_chars {
        return (text.to_string(), false);
    }
    // Degenerate budgets (smaller than the note itself): hard cut, no note.
    if max_chars <= TRIM_NOTE.len() * 2 {
        let mut cut = max_chars;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        return (text[..cut].to_string(), true);
    }
    let keep = max_chars - TRIM_NOTE.len();
    let mut cut = keep;
    while cut > 0 && !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let floor = keep.saturating_sub(keep / 12);
    let end = text[..cut]
        .rfind('\n')
        .filter(|i| *i >= floor)
        .unwrap_or(cut);
    (format!("{}{}", &text[..end], TRIM_NOTE), true)
}

// =============================================================================
// Options / report
// =============================================================================

/// How the entry points were seeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SeedingMode {
    /// Type-flow edges (`uses_type`/`returns`/`type_of`) existed for the
    /// resolved symbols — DRACO dataflow seeding was effective.
    Dataflow,
    /// No type-flow edges on the resolved symbols (the bridged index has
    /// no value-level dataflow) — seeding followed call edges only.
    CallGraph,
}

/// Options for [`build_analysis_context`].
#[derive(Debug, Clone)]
pub struct AnalysisContextOptions {
    /// Trim the rendered markdown to roughly this many tokens. `None`
    /// falls back to the engine's adaptive character budget for the
    /// project's size tier.
    pub budget_tokens: Option<usize>,
    /// Maximum entry-point + related nodes the engine may surface.
    pub max_nodes: usize,
}

impl Default for AnalysisContextOptions {
    fn default() -> Self {
        Self {
            budget_tokens: None,
            max_nodes: 20,
        }
    }
}

/// Result of the analysis-strategy context build: the rendered (and
/// possibly trimmed) markdown plus everything needed to report honestly
/// on how it was produced.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AnalysisContextReport {
    pub query: String,
    /// Always `"analysis"` — present so `--json` output is self-describing.
    pub strategy: &'static str,
    /// The rendered markdown (already trimmed to budget).
    pub markdown: String,
    pub entry_point_count: usize,
    pub related_count: usize,
    /// Files with source slices included in the markdown.
    pub file_count: usize,
    /// Structs rendered as field-level partial views ("Partial struct
    /// views" markdown section). 0 when the bridged graph carries no field
    /// metadata (the default bridge — see `CODEGRAPH_ANALYSIS_FIELDS`) or
    /// when no selected function touches a strict subset of a struct's
    /// fields.
    pub partial_struct_views: usize,
    pub seeding: SeedingMode,
    /// The Repoformer retrieval gate abstained from related-node expansion
    /// (the entry points were self-contained).
    pub gate_abstained: bool,
    /// Expansion work (related nodes) the gate avoided producing.
    pub expansion_work_saved: usize,
    pub budget_tokens: Option<usize>,
    /// `measure_tokens(markdown)` — chars / 4.
    pub measured_tokens: usize,
    pub truncated: bool,
    /// Capability / degradation notes. Surfaced on stderr by the CLI's
    /// `--verbose`; always present here.
    pub notes: Vec<String>,
}

// =============================================================================
// Pipeline
// =============================================================================

/// Build token-budgeted context for `task` over a bridged analysis graph.
///
/// `project_root` anchors the graph's relative file paths for source
/// reading. Never errors: unresolvable tasks and unreadable files degrade
/// to smaller output (with notes), not failures.
pub fn build_analysis_context(
    graph: &AnalysisGraph,
    project_root: &Path,
    task: &str,
    options: &AnalysisContextOptions,
) -> AnalysisContextReport {
    let mut notes: Vec<String> = Vec::new();

    // --- Selection: resolver + DRACO seeding + retrieval gate + expansion ---
    let ctx = engine_build_context(
        graph,
        None,
        task,
        EngineContextOptions {
            max_nodes: options.max_nodes,
            include_code: false, // source is rendered via clustering below
            traversal_depth: 1,
            force_expand: false,
        },
    );

    // --- Honest seeding mode -------------------------------------------------
    let name_seeds = resolve_name_seeds(graph, task);
    let type_flow_seeded = name_seeds.iter().any(|id| {
        graph.get_edges_from(id).iter().any(|(_, edge)| {
            matches!(
                edge.kind,
                AEdgeKind::UsesType | AEdgeKind::Returns | AEdgeKind::TypeOf
            )
        })
    });
    let seeding = if type_flow_seeded {
        SeedingMode::Dataflow
    } else {
        SeedingMode::CallGraph
    };
    if name_seeds.is_empty() {
        notes.push(
            "No identifiers from the task resolved to indexed symbols; context may be empty. \
             Name a function/class/module from the codebase to anchor the context."
                .to_string(),
        );
    } else if !type_flow_seeded {
        notes.push(
            "Dataflow seeding degraded to call-graph seeding: the bridged index has no \
             type-flow edges (uses_type/returns) for the resolved symbols — value-level \
             dataflow is not stored in the index. Expansion followed call edges instead."
                .to_string(),
        );
    }

    // --- Retrieval-gate measurement (deterministic, no wall clock) ----------
    let gate = measure::measure_gate(graph, task);
    if gate.gate_abstained && ctx.related.is_empty() && !ctx.entry_points.is_empty() {
        notes.push(format!(
            "Retrieval gate abstained: the entry points are self-contained (no cross-file \
             or external references), so related-symbol expansion was skipped \
             ({} nodes of expansion work avoided).",
            gate.work_saved()
        ));
    }

    // --- Render: engine context head + clustered per-file source ------------
    let budget = ExploreBudget::for_file_count(distinct_file_count(graph));
    let mut markdown = render::render_context(
        graph,
        task,
        &ctx.entry_points,
        &ctx.related,
        &[],
        ctx.intent,
        &budget,
    );

    // --- Field-level partial struct views (engine partial-struct contract) ---
    let (partial_md, partial_struct_views, partial_notes) =
        build_partial_struct_sections(graph, &ctx.entry_points, &ctx.related);
    notes.extend(partial_notes);
    if !partial_md.is_empty() {
        markdown.push_str("\n\n");
        markdown.push_str(&partial_md);
    }

    let (blocks, additional) = build_source_blocks(
        graph,
        project_root,
        &ctx.entry_points,
        &ctx.related,
        &budget,
    );
    let file_count = blocks.len();
    if !blocks.is_empty() {
        markdown.push_str("\n\n### Source Code\n\n");
        for block in &blocks {
            markdown.push_str(&format!(
                "#### {} — {}\n\n```{}\n{}```\n\n",
                block.path, block.header, block.language, block.body
            ));
        }
    }
    if budget.include_additional_files && !additional.is_empty() {
        markdown.push_str("### Additional relevant files (not shown)\n\n");
        for (path, symbols) in additional.iter().take(10) {
            markdown.push_str(&format!("- {path}: {symbols}\n"));
        }
        if additional.len() > 10 {
            markdown.push_str(&format!("- ... and {} more files\n", additional.len() - 10));
        }
    }

    // --- Budget trim + measurement -------------------------------------------
    let (markdown, truncated) = match options.budget_tokens {
        Some(tokens) => trim_to_token_budget(&markdown, tokens),
        None => trim_to_token_budget(&markdown, budget.max_output_chars / CHARS_PER_TOKEN),
    };
    if truncated {
        notes.push(match options.budget_tokens {
            Some(tokens) => format!("Output trimmed to the requested --budget of {tokens} tokens."),
            None => format!(
                "Output trimmed to the project's adaptive budget (~{} tokens); pass --budget \
                 to override.",
                budget.max_output_chars / CHARS_PER_TOKEN
            ),
        });
    }
    let measured_tokens = measure_tokens(&markdown);

    AnalysisContextReport {
        query: task.to_string(),
        strategy: "analysis",
        markdown,
        entry_point_count: ctx.entry_points.len(),
        related_count: ctx.related.len(),
        file_count,
        partial_struct_views,
        seeding,
        gate_abstained: gate.gate_abstained,
        expansion_work_saved: gate.work_saved(),
        budget_tokens: options.budget_tokens,
        measured_tokens,
        truncated,
        notes,
    }
}

// =============================================================================
// Seeding helpers
// =============================================================================

/// Identifier-shaped tokens from a free-form task, resolved through the
/// engine's qualified-name resolver. Mirrors the engine's own entry-point
/// picking (which is private) closely enough to report the seeding mode
/// honestly.
fn resolve_name_seeds(graph: &AnalysisGraph, task: &str) -> Vec<ANodeId> {
    const STOP_WORDS: &[&str] = &[
        "the", "and", "for", "with", "from", "this", "that", "have", "into", "but", "not", "are",
        "was", "were", "has", "had", "its", "can", "did", "may", "also", "than", "then", "them",
        "how", "what", "when", "where", "which", "who", "why", "does", "doing", "done", "use",
        "used", "using", "about", "through", "reach", "reaches", "work", "works", "working",
    ];
    let mut seen: HashSet<ANodeId> = HashSet::new();
    let mut out: Vec<ANodeId> = Vec::new();
    for raw in
        task.split(|c: char| !c.is_alphanumeric() && c != '_' && c != ':' && c != '.' && c != '/')
    {
        if raw.len() < 3
            || !raw.chars().any(|c| c.is_alphabetic())
            || STOP_WORDS.contains(&raw.to_lowercase().as_str())
        {
            continue;
        }
        for id in resolve_symbol(graph, raw) {
            if seen.insert(id.clone()) {
                out.push(id);
            }
        }
        if out.len() >= 8 {
            break;
        }
    }
    out
}

fn distinct_file_count(graph: &AnalysisGraph) -> usize {
    let mut files: HashSet<&Path> = HashSet::new();
    for id in graph.all_node_ids() {
        if let Some(node) = graph.get_node(id) {
            files.insert(node.file_path.as_path());
        }
    }
    files.len()
}

// =============================================================================
// Partial struct views (field-level granularity)
// =============================================================================

/// Render field-level views of the selected structs: only the fields the
/// selected (focal-flow) functions touch, marked with the engine's
/// accessed-field markers. Returns `(markdown_section, view_count, notes)` —
/// the section is empty when nothing qualifies.
///
/// Field data is the engine's partial-struct metadata contract
/// ([`partial::STRUCT_FIELDS_KEY`] / [`partial::ACCESSED_FIELDS_KEY`]),
/// which the bridge registers only under
/// [`crate::analysis_bridge::BridgeOptions::include_fields`]
/// (`CODEGRAPH_ANALYSIS_FIELDS=1`). Honest notes over silent emptiness:
/// missing field data and the engine's `PartialStruct` capability
/// kill-switch each produce an explanatory note instead of a quietly
/// absent section.
fn build_partial_struct_sections(
    graph: &AnalysisGraph,
    entry_points: &[ANodeId],
    related: &[ANodeId],
) -> (String, usize, Vec<String>) {
    // Selection split, deduped, in selection order (entries first).
    let mut seen: HashSet<&ANodeId> = HashSet::new();
    let mut structs: Vec<&ANodeId> = Vec::new();
    let mut functions: Vec<&ANodeId> = Vec::new();
    for id in entry_points.iter().chain(related.iter()) {
        if !seen.insert(id) {
            continue;
        }
        match graph.get_node(id).map(|n| n.kind) {
            Some(ANodeKind::Struct) => structs.push(id),
            Some(ANodeKind::Function) => functions.push(id),
            _ => {}
        }
    }
    if structs.is_empty() || functions.is_empty() {
        return (String::new(), 0, Vec::new());
    }

    // Honor the engine's kill-switch exactly like its session surface does
    // (the graph-level reads below are not gated, so gate here — the render
    // layer is where the capability is meant to apply).
    if !CapabilityTree::from_env().is_enabled(Capability::PartialStruct) {
        return (
            String::new(),
            0,
            vec![format!(
                "Partial struct views skipped: {}.",
                PartialStructError::Disabled
            )],
        );
    }

    let mut notes: Vec<String> = Vec::new();
    let mut sections: Vec<String> = Vec::new();
    let mut missing_field_data = 0usize;

    for sid in structs {
        let Some(snode) = graph.get_node(sid) else {
            continue;
        };
        // Engine-typed field data only: the default (fieldless) bridge folds
        // a legacy JSON name-array under the same `fields` key, which the
        // engine decoder yields nothing for — that counts as "no field
        // data", same as an absent key.
        let has_engine_fields = snode
            .metadata
            .get(partial::STRUCT_FIELDS_KEY)
            .map(|raw| !partial::parse_fields_metadata(raw).is_empty())
            .unwrap_or(false);
        if !has_engine_fields {
            missing_field_data += 1;
            continue;
        }

        // Merge the engine's per-function views: which of this struct's
        // fields does each selected function touch?
        let mut accessed_by: BTreeMap<String, Vec<String>> = BTreeMap::new();
        let mut merged_view: Option<partial::PartialView> = None;
        for fid in &functions {
            let view = match partial::try_get_partial_struct(graph, sid, fid) {
                Ok(view) => view,
                Err(err) => {
                    // Verbatim per the engine's guidance — every variant is
                    // a one-line honest capability note.
                    notes.push(format!("Partial struct view unavailable: {err}."));
                    continue;
                }
            };
            let fn_name = graph
                .get_node(fid)
                .map(|n| n.name.clone())
                .unwrap_or_default();
            for field in view.visible_fields() {
                let who = accessed_by.entry(field.name.clone()).or_default();
                if !who.contains(&fn_name) {
                    who.push(fn_name.clone());
                }
            }
            merged_view = Some(view);
        }
        let Some(view) = merged_view else {
            continue;
        };

        let total = view.all_fields.len();
        let touched = accessed_by.len();
        // Only a strict subset earns a partial view: untouched structs add
        // nothing, fully-touched structs are not partial.
        if touched == 0 || touched >= total {
            continue;
        }

        let mut lines: Vec<String> = vec![
            format!(
                "#### `{}` — {} ({touched} of {total} fields accessed)",
                view.struct_name,
                snode.file_path.display(),
            ),
            String::new(),
        ];
        // Declaration order via the engine's marker API; a field is marked
        // when any selected function accesses it.
        for (field, _marker) in view.all_fields_with_markers() {
            let Some(who) = accessed_by.get(&field.name) else {
                continue;
            };
            let shown: Vec<String> = who.iter().take(3).map(|n| format!("`{n}`")).collect();
            let mut line = format!("- ✓ `{}`", field.name);
            if !field.type_str.is_empty() {
                line.push_str(&format!(": {}", field.type_str));
            }
            line.push_str(if field.is_public { " (pub)" } else { " (priv)" });
            line.push_str(&format!(" — accessed by {}", shown.join(", ")));
            if who.len() > 3 {
                line.push_str(&format!(" and {} more", who.len() - 3));
            }
            lines.push(line);
        }
        lines.push(format!(
            "- … {} more fields not touched by the selected symbols",
            total - touched
        ));
        sections.push(lines.join("\n"));
    }

    if missing_field_data > 0 {
        notes.push(format!(
            "Field-level partial struct views unavailable for {missing_field_data} selected \
             struct(s): the bridged graph carries no field metadata for them. Re-run with \
             `context --fields` (or set CODEGRAPH_ANALYSIS_FIELDS=1) to carry field/property \
             metadata (the analysis snapshot cache rebuilds automatically)."
        ));
    }
    if sections.is_empty() {
        return (String::new(), 0, notes);
    }
    let mut md = String::from(
        "### Partial struct views\n\nOnly the fields the selected symbols touch are shown \
         (✓ = accessed, from the index's field metadata).\n\n",
    );
    md.push_str(&sections.join("\n\n"));
    (md, sections.len(), notes)
}

// =============================================================================
// Clustered source blocks
// =============================================================================

struct SourceBlock {
    path: String,
    language: &'static str,
    header: String,
    body: String,
}

/// Group the selected nodes by file, cluster each file's spans
/// ([`clustering`]), and slice the source under the engine budget's
/// per-file caps. Entry-point files render first; overflow files are
/// returned as a `(path, symbols)` "not shown" list.
fn build_source_blocks(
    graph: &AnalysisGraph,
    project_root: &Path,
    entry_points: &[ANodeId],
    related: &[ANodeId],
    budget: &ExploreBudget,
) -> (Vec<SourceBlock>, Vec<(String, String)>) {
    let entry_set: HashSet<ANodeId> = entry_points.iter().cloned().collect();

    let mut by_file: BTreeMap<PathBuf, Vec<ANodeId>> = BTreeMap::new();
    for id in entry_points.iter().chain(related.iter()) {
        let Some(node) = graph.get_node(id) else {
            continue;
        };
        if node.span.start_line == 0 || node.file_path.to_str() == Some(UNRESOLVED_FILE) {
            continue;
        }
        by_file
            .entry(node.file_path.clone())
            .or_default()
            .push(id.clone());
    }

    // Entry-point-bearing files first (most entries wins), then path order.
    let mut files: Vec<(PathBuf, Vec<ANodeId>)> = by_file.into_iter().collect();
    files.sort_by(|a, b| {
        let ea = a.1.iter().filter(|id| entry_set.contains(id)).count();
        let eb = b.1.iter().filter(|id| entry_set.contains(id)).count();
        eb.cmp(&ea).then_with(|| a.0.cmp(&b.0))
    });

    let mut blocks: Vec<SourceBlock> = Vec::new();
    let mut additional: Vec<(String, String)> = Vec::new();
    for (file, ids) in files {
        let symbols_inline = || {
            let names: Vec<String> = ids
                .iter()
                .filter_map(|id| graph.get_node(id))
                .map(|n| n.name.clone())
                .collect();
            names.join(", ")
        };
        if blocks.len() >= budget.default_max_files {
            additional.push((file.display().to_string(), symbols_inline()));
            continue;
        }
        let abs = project_root.join(&file);
        let Ok(content) = std::fs::read_to_string(&abs) else {
            // Unreadable (deleted/moved since indexing): list, don't fail.
            additional.push((file.display().to_string(), symbols_inline()));
            continue;
        };
        let line_count = content.lines().count() as u32;
        let ranges = clustering::build_ranges(graph, &ids, &entry_set, line_count);
        if ranges.is_empty() {
            continue;
        }
        let clusters = clustering::build_clusters(&ranges, budget.gap_threshold);
        let ranked = clustering::rank_clusters_for_inclusion(&clusters);

        // Pick clusters by rank under the per-file cap, then display in
        // source order.
        let mut chosen: Vec<usize> = Vec::new();
        let mut used = 0usize;
        for idx in ranked {
            let Some(slice) = clustering::read_cluster_source(&abs, &clusters[idx], 2) else {
                continue;
            };
            if !chosen.is_empty() && used + slice.len() > budget.max_chars_per_file {
                continue;
            }
            used += slice.len().min(budget.max_chars_per_file);
            chosen.push(idx);
            if used >= budget.max_chars_per_file {
                break;
            }
        }
        chosen.sort_unstable();

        let mut body = String::new();
        let mut header_symbols: Vec<String> = Vec::new();
        for idx in &chosen {
            let cluster = &clusters[*idx];
            let Some(slice) = clustering::read_cluster_source(&abs, cluster, 2) else {
                continue;
            };
            if !body.is_empty() {
                body.push_str("...\n");
            }
            if body.is_empty() && slice.len() > budget.max_chars_per_file {
                let mut cut = budget.max_chars_per_file;
                while cut > 0 && !slice.is_char_boundary(cut) {
                    cut -= 1;
                }
                body.push_str(&slice[..cut]);
                if !body.ends_with('\n') {
                    body.push('\n');
                }
            } else {
                body.push_str(&slice);
            }
            header_symbols.extend(cluster.symbols.iter().cloned());
        }
        if body.is_empty() {
            continue;
        }
        blocks.push(SourceBlock {
            path: file.display().to_string(),
            language: language_for(&file),
            header: clustering::build_file_header(
                &header_symbols,
                budget.max_symbols_in_file_header,
            ),
            body,
        });
    }
    (blocks, additional)
}

/// Fence tag from a file extension (best effort; empty for unknown).
fn language_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "tsx",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "jsx",
        "py" => "python",
        "rs" => "rust",
        "go" => "go",
        "java" => "java",
        "c" | "h" => "c",
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "php" => "php",
        "rb" => "ruby",
        "swift" => "swift",
        "kt" | "kts" => "kotlin",
        "scala" => "scala",
        "lua" | "luau" => "lua",
        "dart" => "dart",
        "m" | "mm" => "objc",
        "pas" | "dpr" => "pascal",
        _ => "",
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use codegraph_analysis::edges::EdgeData as AEdgeData;
    use codegraph_analysis::nodes::{
        NodeData as ANodeData,
        NodeKind as ANodeKind,
        Span as ASpan,
        Visibility as AVisibility,
    };

    use super::*;

    fn span_in(file: &str, start: u32, end: u32) -> ASpan {
        ASpan {
            file: PathBuf::from(file),
            start_line: start,
            start_col: 0,
            end_line: end,
            end_col: 0,
            byte_range: 0..0,
        }
    }

    fn node_in(name: &str, kind: ANodeKind, file: &str, start: u32, end: u32) -> ANodeData {
        ANodeData {
            id: ANodeId::new(file, name, kind),
            kind,
            name: name.to_string(),
            qualified_name: name.to_string(),
            file_path: PathBuf::from(file),
            span: span_in(file, start, end),
            visibility: AVisibility::Public,
            metadata: HashMap::new(),
            birth_revision: 0,
            last_modified_revision: 0,
            complexity: None,
            cfg: None,
            dataflow: None,
        }
    }

    fn edge(kind: AEdgeKind) -> AEdgeData {
        AEdgeData {
            kind,
            source_span: span_in("x", 1, 1),
            weight: 1.0,
        }
    }

    // --- token measurement / trimming ---------------------------------------

    #[test]
    fn measure_tokens_uses_chars_over_four() {
        assert_eq!(measure_tokens(""), 0);
        assert_eq!(measure_tokens("abcd"), 1);
        assert_eq!(measure_tokens("abcde"), 2);
    }

    #[test]
    fn trim_noop_when_under_budget() {
        let text = "short output\n";
        let (out, truncated) = trim_to_token_budget(text, 100);
        assert_eq!(out, text);
        assert!(!truncated);
    }

    #[test]
    fn trim_lands_within_ten_percent_of_budget() {
        // Realistic markdown: many short lines, well over budget.
        let text = "0123456789 abcdef\n".repeat(500); // 9000 chars
        for budget in [100usize, 250, 500, 1000] {
            let (out, truncated) = trim_to_token_budget(&text, budget);
            assert!(truncated, "budget {budget} should trim");
            let measured = measure_tokens(&out);
            assert!(
                measured <= budget,
                "budget {budget}: measured {measured} exceeds budget"
            );
            assert!(
                measured * 10 >= budget * 9,
                "budget {budget}: measured {measured} below 90% of budget"
            );
            assert!(out.contains("trimmed to the requested token budget"));
        }
    }

    #[test]
    fn trim_degenerate_budget_hard_cuts_without_note() {
        let text = "x".repeat(1000);
        let (out, truncated) = trim_to_token_budget(&text, 10); // 40 chars < note
        assert!(truncated);
        assert_eq!(out.len(), 40);
        assert!(!out.contains("trimmed"));
    }

    // --- seeding-mode honesty -------------------------------------------------

    /// Calls-only graph: seeding degrades to call-graph and says so.
    #[test]
    fn calls_only_graph_degrades_to_call_graph_seeding() {
        let mut g = AnalysisGraph::new();
        let a = g.add_node(node_in("alpha", ANodeKind::Function, "src/a.rs", 1, 5));
        let b = g.add_node(node_in("beta", ANodeKind::Function, "src/b.rs", 1, 5));
        g.add_edge(&a, &b, edge(AEdgeKind::Calls)).unwrap();

        let report = build_analysis_context(
            &g,
            Path::new("/nonexistent/codegraph_ctx"),
            "look at alpha",
            &AnalysisContextOptions::default(),
        );
        assert_eq!(report.seeding, SeedingMode::CallGraph);
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("call-graph seeding")),
            "degradation note expected: {:?}",
            report.notes
        );
        assert_eq!(report.entry_point_count + report.related_count, 2);
    }

    /// A uses_type edge from the resolved symbol flips seeding to dataflow
    /// and the degradation note disappears.
    #[test]
    fn type_flow_edge_enables_dataflow_seeding() {
        let mut g = AnalysisGraph::new();
        let f = g.add_node(node_in("handler", ANodeKind::Function, "src/h.rs", 1, 5));
        let t = g.add_node(node_in("Request", ANodeKind::Struct, "src/t.rs", 1, 5));
        g.add_edge(&f, &t, edge(AEdgeKind::UsesType)).unwrap();

        let report = build_analysis_context(
            &g,
            Path::new("/nonexistent/codegraph_ctx"),
            "look at handler",
            &AnalysisContextOptions::default(),
        );
        assert_eq!(report.seeding, SeedingMode::Dataflow);
        assert!(
            !report
                .notes
                .iter()
                .any(|n| n.contains("call-graph seeding")),
            "no degradation note expected: {:?}",
            report.notes
        );
    }

    /// Unresolvable task: empty-resolution note, no panic, no fabrication.
    #[test]
    fn unresolvable_task_reports_empty_resolution() {
        let g = AnalysisGraph::new();
        let report = build_analysis_context(
            &g,
            Path::new("/nonexistent/codegraph_ctx"),
            "zzz_no_such_symbol",
            &AnalysisContextOptions::default(),
        );
        assert_eq!(report.entry_point_count, 0);
        assert!(report.notes.iter().any(|n| n.contains("resolved")));
        assert_eq!(report.file_count, 0);
    }

    // --- partial struct views ---------------------------------------------------

    /// 10 fields, `host`/`port` first — the canonical partial-view fixture.
    fn ten_fields() -> Vec<partial::FieldInfo> {
        let mut fields = vec![
            partial::FieldInfo {
                name: "host".to_string(),
                type_str: "string".to_string(),
                is_public: true,
            },
            partial::FieldInfo {
                name: "port".to_string(),
                type_str: "number".to_string(),
                is_public: false,
            },
        ];
        for i in 3..=10 {
            fields.push(partial::FieldInfo {
                name: format!("extra{i:02}"),
                type_str: "string".to_string(),
                is_public: true,
            });
        }
        fields
    }

    /// A function touching 2 of a 10-field struct renders the partial view:
    /// only the touched fields, marked, with an omitted-count line.
    #[test]
    fn partial_struct_view_renders_flow_touched_fields() {
        let mut g = AnalysisGraph::new();
        let f = g.add_node(node_in(
            "loadConfig",
            ANodeKind::Function,
            "src/load.ts",
            1,
            5,
        ));
        let s = g.add_node(node_in("BigConfig", ANodeKind::Struct, "src/cfg.ts", 1, 12));
        g.add_edge(&f, &s, edge(AEdgeKind::UsesType)).unwrap();
        partial::set_struct_fields(&mut g, &s, &ten_fields()).unwrap();
        partial::set_accessed_fields(&mut g, &f, &["host".to_string(), "port".to_string()])
            .unwrap();

        let report = build_analysis_context(
            &g,
            Path::new("/nonexistent/codegraph_ctx"),
            "how does loadConfig use BigConfig",
            &AnalysisContextOptions::default(),
        );
        assert_eq!(report.partial_struct_views, 1, "notes: {:?}", report.notes);
        assert!(report.markdown.contains("### Partial struct views"));
        assert!(
            report
                .markdown
                .contains("`BigConfig` — src/cfg.ts (2 of 10 fields accessed)"),
            "got: {}",
            report.markdown
        );
        assert!(report.markdown.contains("- ✓ `host`: string (pub)"));
        assert!(report.markdown.contains("- ✓ `port`: number (priv)"));
        assert!(report.markdown.contains("accessed by `loadConfig`"));
        assert!(
            report
                .markdown
                .contains("- … 8 more fields not touched by the selected symbols")
        );
        // Untouched fields are not expanded in the partial section.
        assert!(!report.markdown.contains("- ✓ `extra03`"));
    }

    /// A function touching every field is not a partial view — nothing to
    /// trim, no section.
    #[test]
    fn fully_touched_struct_renders_no_partial_view() {
        let mut g = AnalysisGraph::new();
        let f = g.add_node(node_in(
            "loadConfig",
            ANodeKind::Function,
            "src/load.ts",
            1,
            5,
        ));
        let s = g.add_node(node_in("BigConfig", ANodeKind::Struct, "src/cfg.ts", 1, 12));
        g.add_edge(&f, &s, edge(AEdgeKind::UsesType)).unwrap();
        partial::set_struct_fields(&mut g, &s, &ten_fields()).unwrap();
        let all: Vec<String> = ten_fields().into_iter().map(|fi| fi.name).collect();
        partial::set_accessed_fields(&mut g, &f, &all).unwrap();

        let report = build_analysis_context(
            &g,
            Path::new("/nonexistent/codegraph_ctx"),
            "how does loadConfig use BigConfig",
            &AnalysisContextOptions::default(),
        );
        assert_eq!(report.partial_struct_views, 0);
        assert!(!report.markdown.contains("Partial struct views"));
    }

    /// Default bridge (no field carrying): a selected struct without field
    /// metadata produces an honest note naming the gate — never a silently
    /// absent section.
    #[test]
    fn struct_without_field_data_notes_honestly() {
        let mut g = AnalysisGraph::new();
        let f = g.add_node(node_in(
            "loadConfig",
            ANodeKind::Function,
            "src/load.ts",
            1,
            5,
        ));
        let s = g.add_node(node_in("BigConfig", ANodeKind::Struct, "src/cfg.ts", 1, 12));
        g.add_edge(&f, &s, edge(AEdgeKind::UsesType)).unwrap();

        let report = build_analysis_context(
            &g,
            Path::new("/nonexistent/codegraph_ctx"),
            "how does loadConfig use BigConfig",
            &AnalysisContextOptions::default(),
        );
        assert_eq!(report.partial_struct_views, 0);
        assert!(!report.markdown.contains("Partial struct views"));
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("CODEGRAPH_ANALYSIS_FIELDS")),
            "expected the field-gate note, got: {:?}",
            report.notes
        );
    }

    // --- clustered source blocks ----------------------------------------------

    #[test]
    fn source_blocks_render_clustered_file_slices() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        let source: String = (1..=30).map(|i| format!("fn line_{i}() {{}}\n")).collect();
        fs::write(root.join("src/a.rs"), &source).unwrap();

        let mut g = AnalysisGraph::new();
        let a = g.add_node(node_in("alpha", ANodeKind::Function, "src/a.rs", 2, 4));
        let b = g.add_node(node_in("beta", ANodeKind::Function, "src/a.rs", 6, 8));
        let budget = ExploreBudget::for_file_count(10);
        let (blocks, additional) = build_source_blocks(&g, root, &[a], &[b], &budget);
        assert_eq!(blocks.len(), 1);
        assert!(additional.is_empty());
        let block = &blocks[0];
        assert_eq!(block.path, "src/a.rs");
        assert_eq!(block.language, "rust");
        assert!(block.header.contains("alpha"));
        // Line-numbered slice covering both clustered symbols.
        assert!(block.body.contains("2\tfn line_2()"));
        assert!(block.body.contains("8\tfn line_8()"));
    }

    #[test]
    fn unreadable_files_fall_back_to_additional_list() {
        let mut g = AnalysisGraph::new();
        let a = g.add_node(node_in("alpha", ANodeKind::Function, "src/gone.rs", 2, 4));
        let budget = ExploreBudget::for_file_count(10);
        let (blocks, additional) = build_source_blocks(
            &g,
            Path::new("/nonexistent/codegraph_ctx"),
            &[a],
            &[],
            &budget,
        );
        assert!(blocks.is_empty());
        assert_eq!(additional.len(), 1);
        assert_eq!(additional[0].0, "src/gone.rs");
        assert!(additional[0].1.contains("alpha"));
    }

    #[test]
    fn placeholder_nodes_are_never_rendered_as_source() {
        let mut g = AnalysisGraph::new();
        let p = g.add_node(node_in("ghost", ANodeKind::Function, UNRESOLVED_FILE, 0, 0));
        let budget = ExploreBudget::for_file_count(10);
        let (blocks, additional) = build_source_blocks(&g, Path::new("/"), &[p], &[], &budget);
        assert!(blocks.is_empty());
        assert!(additional.is_empty());
    }
}
