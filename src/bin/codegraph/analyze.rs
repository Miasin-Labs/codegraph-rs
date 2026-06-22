use super::*;

mod boundaries;
mod bridge;
mod capabilities;
mod centrality;
mod cfg;
mod co_change;
mod communities;
mod complexity;
mod coverage;
mod critical;
mod cycles;
mod dataflow;
mod diff;
mod dispatch;
mod dominators;
mod export;
mod generics;
mod impact;
mod query;
mod schema;
mod slice;
mod stats;
mod taint;
mod traits;
mod types;
mod validate;
mod vuln;

use boundaries::cmd_analyze_boundaries;
pub(crate) use bridge::{bridge_project_with_options, print_json, *};
use capabilities::cmd_analyze_capabilities;
use centrality::cmd_analyze_centrality;
use cfg::cmd_analyze_cfg;
use co_change::cmd_analyze_co_change;
use communities::cmd_analyze_communities;
use complexity::cmd_analyze_complexity;
use coverage::cmd_analyze_coverage;
use critical::cmd_analyze_critical;
use cycles::cmd_analyze_cycles;
use dataflow::cmd_analyze_dataflow;
use diff::cmd_analyze_diff;
pub(crate) use dispatch::cmd_analyze;
use dominators::cmd_analyze_dominators;
use export::cmd_analyze_export;
use generics::cmd_analyze_generics;
use impact::cmd_analyze_impact;
use query::cmd_analyze_query;
use schema::cmd_analyze_schema;
use slice::cmd_analyze_slice;
use stats::cmd_analyze_stats;
use taint::cmd_analyze_taint;
use traits::cmd_analyze_traits;
use types::cmd_analyze_types;
use validate::cmd_analyze_validate;
use vuln::cmd_analyze_vuln;

// =============================================================================
// analyze command family
//
// The analysis engine (`codegraph-analysis`) running over the
// project's bridged SQLite index (`analysis_bridge::build_analysis_graph`).
// All commands are pure reads of the index. Report shapes live in
// `codegraph::analyze`; this file only resolves symbols and renders.
//
// The bridged graph is snapshotted under `.codegraph/analysis/` keyed by an
// index fingerprint, so repeat invocations skip the full SQL re-read
// (`analysis_bridge::build_analysis_graph_cached`). `--no-cache` forces a
// rebuild; cache hits print a one-line "(cached graph)" notice in human
// output only — `--json` stays pure JSON.
// =============================================================================

/// Entry cap for the `analyze slice --source` annotated lists (slice +
/// data dependencies). The engine summarizes anything beyond the cap.
const SOURCE_REPORT_MAX_ENTRIES: usize = 50;

/// Rendered-flow cap for `analyze taint --source` — same cap the default
/// taint path rendering uses; the engine summarizes flows beyond it.
const SOURCE_TAINT_MAX_PATHS: usize = 25;
fn cycle_kind_label(kind: &str) -> &'static str {
    match kind {
        "mutualRecursion" => "mutual recursion",
        "selfRecursion" => "direct recursion",
        "moduleCycle" => "module cycle",
        _ => "mixed cycle",
    }
}
