//! Analysis-engine runners over the bridged graph.
//!
//! Each function here drives one public capability of the
//! `codegraph-analysis` crate against a graph produced by
//! [`crate::analysis_bridge::build_analysis_graph`] and returns a
//! serde-serializable report with a stable camelCase JSON shape. The CLI
//! (`codegraph analyze …`) renders these; library users can call them
//! directly.
//!
//! ## Honesty contract (what runs at which precision)
//!
//! The SQLite bridge carries symbols, spans (line/column, no byte ranges)
//! and structural edges — it does NOT carry the analysis crate's per-function
//! IR/CFG/dataflow (upstream those are produced by the Rust language adapter
//! only; see `analysis/ADAPTER_PARITY.md`). Consequences:
//!
//! - **complexity** re-parses the on-disk sources with the host tree-sitter
//!   grammars and runs the analysis crate's metrics on the located function
//!   bodies. Languages without complexity rules are counted as skipped.
//! - **slice** and **taint** run the analysis crate's slicing/path machinery
//!   over a *call-graph* oracle ([`CallGraphOracle`]) — function-level hops
//!   along call edges, not value-level def-use chains. Every report says so
//!   in its `note` field instead of pretending otherwise. With
//!   `--value-level` (schema v5 indexes that store byte offsets), the
//!   oracle upgrades to the engine's interprocedural points-to analysis
//!   over per-function dataflow IR and the reports' `granularity` flips to
//!   `"value-level"` — see [`crate::analyze_ir`]. Pre-v5 indexes degrade
//!   back to call-graph with an explicit re-index note.
//! - **cfg** and **dataflow** ([`crate::analyze_ir`]) use the same source
//!   re-parse anchor pattern as **complexity**; languages without engine
//!   rule tables get honest capability notes.
//! - **communities**, **dominators**, **cycles**, **impact**, **centrality**,
//!   **critical**, **export**, and **stats** are pure graph algorithms and run
//!   at full fidelity over the bridged graph.
//! - **co-change** mines `git log --name-only` at *file* granularity — every
//!   symbol in a touched file counts as changed; same-file pairs are
//!   tautological and summarized instead of listed.
//! - **coverage** is line-granular from LCOV `DA` records summed over each
//!   function's span; annotating also un-blinds the DSL `untested` operator.
//! - **validate** judges arity edits at call-graph level: the bridge carries
//!   no per-call-site argument counts, so an arity change marks every direct
//!   caller as needing review.
//! - **generics** and **boundaries** read the engine's metadata contract
//!   (`generic_params`/`callee_type_args`, `http_route`/`ffi_export`/
//!   `wasm_export`); the SQLite bridge does not populate those keys yet, so
//!   both report that honestly instead of silently returning nothing.
//!
//! ## JSON envelope
//!
//! Every `analyze … --json` payload is wrapped in [`ReportEnvelope`] —
//! `{"schemaVersion": N, "kind": "<report-kind>", "data": …}` — mirroring the
//! engine's `codegraph_analysis::schema::Envelope` wire shape (camelCase per
//! the CLI convention). The engine's own `Envelope` cannot be reused directly
//! because its `kind` is the closed four-variant `PayloadKind` enum; see
//! `notes/close-tier1-needs.md`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use codegraph_analysis::capabilities::{Capability, CapabilityTree};
use codegraph_analysis::cascade::generate_cascade;
use codegraph_analysis::co_change::{CommitInfo, co_changes_for_nodes, compute_co_changes};
use codegraph_analysis::communities::louvain;
use codegraph_analysis::complexity::compute_complexity;
use codegraph_analysis::complexity_rules::LangRules;
use codegraph_analysis::coverage::{annotate_graph_from_lcov, parse_lcov};
use codegraph_analysis::dsl::aggregate::{AggExpr, parse_aggregate};
use codegraph_analysis::dsl::plan::{ScheduleStrategy, optimise_expr, pick_schedule_for_pipe};
use codegraph_analysis::dsl::provenance::trace_query;
use codegraph_analysis::dsl::{
    DslOp,
    Expr,
    QueryConfig as DslQueryConfig,
    QueryError as DslQueryError,
    parse_expr,
    run_query_expr,
};
use codegraph_analysis::edges::EdgeKind as AEdgeKind;
use codegraph_analysis::graph::CodeGraph as AnalysisGraph;
use codegraph_analysis::hll::approximate_reachability;
use codegraph_analysis::monomorphize::find_instantiations;
use codegraph_analysis::nodes::{NodeData as ANodeData, NodeId as ANodeId, NodeKind as ANodeKind};
use codegraph_analysis::pass::{GraphFlag, PassManager};
use codegraph_analysis::polyglot::{
    BoundaryKind,
    detect_ffi_exports,
    detect_http_routes,
    detect_wasm_exports,
    resolve_cross_language_calls,
};
use codegraph_analysis::possible_types::PossibleTypesPass;
use codegraph_analysis::predicates::{Predicate, extract_predicates};
use codegraph_analysis::schema::{PayloadKind, json_schema_for};
use codegraph_analysis::slicing::{DataflowOracle, backward_slice, forward_slice};
use codegraph_analysis::taint_naming::{classify_name, flow_priority};
use codegraph_analysis::traversal::{TraversalConfig, TraversalDirection, traverse};
use codegraph_analysis::validation::VirtualValidator;
use codegraph_analysis::{analysis, analysis_tools};
use serde::Serialize;
use tree_sitter::{Node as TsNode, Point, Tree};

use crate::analysis_bridge::{BaseSnapshot, StoredComplexity, UNRESOLVED_FILE};
use crate::extraction::{create_parser, detect_language};
use crate::types::Language;

/// Deterministic seed for Louvain community detection — same index, same
/// communities, every run.
const LOUVAIN_SEED: u64 = 42;

/// Louvain resolution parameter (1.0 = classic modularity).
const LOUVAIN_RESOLUTION: f64 = 1.0;

/// Depth bound for the reachability walk feeding dominator analysis.
const DOMINATOR_TRAVERSAL_DEPTH: usize = 64;

mod boundaries;
mod capabilities;
mod centrality;
mod co_change;
mod communities;
mod complexity;
mod coverage;
mod cycles;
mod diff;
mod dominators;
mod envelope;
mod export;
mod generics;
mod impact;
mod oracle;
mod query;
mod schema;
mod shared;
mod slicing;
mod stats;
mod taint;
mod taint_suggest;
mod traits;
mod types;
mod validation;
mod vuln;

pub use boundaries::*;
pub use capabilities::*;
pub use centrality::*;
pub use co_change::*;
pub use communities::*;
pub use complexity::*;
pub(crate) use complexity::{complexity_lang_id, locate_function_node};
pub use coverage::*;
pub(crate) use cycles::classify_cycle;
pub use cycles::*;
pub use diff::*;
pub use dominators::*;
pub use envelope::*;
pub use export::*;
#[cfg(test)]
pub(crate) use generics::signature_type_params;
pub use generics::*;
pub use impact::*;
pub use oracle::CallGraphOracle;
pub use query::*;
pub use schema::*;
pub use shared::{SourceReportCoverage, SymbolRef};
pub(crate) use shared::{
    edge_kind_label,
    function_byte_presence,
    is_placeholder,
    kind_label,
    source_report_note,
    symbol_ref,
    symbol_sort_key,
};
pub(crate) use slicing::call_graph_granularity_note;
pub use slicing::*;
pub use stats::*;
pub(crate) use taint::edge_label_between;
pub use taint::*;
pub use taint_suggest::*;
pub(crate) use traits::matches_symbol_filter;
pub use traits::*;
pub use types::*;
pub use validation::*;
#[cfg(test)]
pub(crate) use vuln::severity_for;
pub use vuln::*;

#[cfg(test)]
mod tests;
