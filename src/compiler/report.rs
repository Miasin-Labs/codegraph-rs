//! What a compiler pass found and did.

use std::collections::BTreeMap;

use serde::Serialize;

use super::run::RunStatus;
use crate::resolution::external::OpenStats;

/// How many samples of each verdict a report keeps.
pub const SAMPLES_KEPT: usize = 25;

/// Documents of the SCIP index.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCounts {
    pub total: usize,
    /// Applied: the file is indexed and unchanged since rust-analyzer ran.
    pub applied: usize,
    /// The file changed since (its verdicts wait for the next run).
    pub stale: usize,
    /// Not in the index (ignored by the indexer).
    pub unindexed: usize,
    /// Indexed Rust files with no document: outside every crate's module
    /// tree (`include!`d files, fixtures, unused modules). Their edges
    /// stay tree-sitter's.
    pub without_document: usize,
}

/// Definitions mapped onto nodes.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DefinitionCounts {
    pub mapped: usize,
    /// Nodes created for items a macro invocation generates.
    pub generated: usize,
    /// Members of `impl` blocks (inherent and trait).
    pub impl_members: usize,
    /// Definitions no node stands for, by `kind:reason`.
    pub unmapped: BTreeMap<String, usize>,
}

/// Verdicts on the edges tree-sitter resolution made.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EdgeVerdicts {
    /// Examined: an occurrence was found for the edge.
    pub examined: usize,
    pub confirmed: usize,
    /// Of the confirmed: items declared inside a function body, which
    /// rust-analyzer names as locals.
    pub local_items: usize,
    /// Pointed at another project node.
    pub corrected: usize,
    /// The compiler resolves it outside the project, or to a local: the
    /// edge is removed (the reference becomes an unresolved or external one).
    pub refuted_std: usize,
    /// …to an item a `#[derive]` or other procedural macro generated.
    pub refuted_generated: usize,
    pub refuted_dependency: usize,
    pub refuted_local: usize,
    /// The compiler names a project item no node stands for, or a symbol
    /// that cannot be judged: left as it was.
    pub unverifiable: usize,
    /// No occurrence names the edge's reference: left as it was.
    pub no_occurrence: usize,
    /// rust-analyzer never saw tree-sitter's target (a `cfg` it did not
    /// build, a file outside every crate): left as it was, the compiler's
    /// target added beside it.
    pub target_not_seen: usize,
    /// Edges a previous compiler pass wrote, re-examined.
    pub prior_kept: usize,
    pub prior_changed: usize,
}

/// One resolution strategy's verdicts (`resolvedBy` of the edge).
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrategyCounts {
    pub confirmed: usize,
    pub corrected: usize,
    pub refuted: usize,
    pub unverifiable: usize,
    pub no_occurrence: usize,
}

impl StrategyCounts {
    /// Share of the judged edges the compiler agreed with.
    pub fn precision(&self) -> Option<f64> {
        let judged = self.confirmed + self.corrected + self.refuted;
        (judged > 0).then(|| self.confirmed as f64 / judged as f64)
    }
}

/// Verdicts on the references tree-sitter left unresolved.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedVerdicts {
    pub examined: usize,
    /// Now an edge to a project node.
    pub resolved_in_project: usize,
    /// Now an external edge into a dependency shard.
    pub resolved_external: usize,
    /// Still unresolved, by why (`std`, `local`, `dependency-no-shard`, …).
    pub remaining: BTreeMap<String, usize>,
}

/// Verdicts on the external edges the external pass made.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalVerdicts {
    pub examined: usize,
    pub confirmed: usize,
    pub corrected: usize,
    /// The compiler resolves it inside the project.
    pub into_project: usize,
    /// The compiler resolves it to std or a local.
    pub refuted: usize,
    pub unverifiable: usize,
}

/// What the compiler added that tree-sitter never recorded.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AddedCounts {
    /// In-project edges by kind.
    pub edges: BTreeMap<String, usize>,
    /// Of those, edges inside macro invocations.
    pub in_macros: usize,
    /// External edges into dependency shards.
    pub external: usize,
    /// Occurrences left alone, by why.
    pub skipped: BTreeMap<String, usize>,
}

/// Trait-method → impl-method dispatch edges.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DispatchCounts {
    /// Heuristic dispatch edges the compiler agrees with.
    pub confirmed: usize,
    /// Dispatch edges only the compiler knew.
    pub added: usize,
    /// Heuristic dispatch edges to a method that does not implement that
    /// trait method.
    pub removed: usize,
}

/// A few verdicts spelled out, for a person to check.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Samples {
    pub corrected: Vec<String>,
    pub refuted: Vec<String>,
    pub resolved: Vec<String>,
    pub added: Vec<String>,
}

/// One compiler pass.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompilerReport {
    /// How the rust-analyzer run went (absent when only a cached index was
    /// applied).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run: Option<RunStatus>,
    pub tool_version: String,
    pub documents: DocumentCounts,
    pub definitions: DefinitionCounts,
    pub edges: EdgeVerdicts,
    /// Tree-sitter's edges by resolution strategy.
    pub by_strategy: BTreeMap<String, StrategyCounts>,
    pub unresolved: UnresolvedVerdicts,
    pub external: ExternalVerdicts,
    pub added: AddedCounts,
    pub dispatch: DispatchCounts,
    pub implements_added: usize,
    pub dependency_graphs: usize,
    pub opens: OpenStats,
    /// Every applied document was examined within the budget.
    pub complete: bool,
    pub elapsed_ms: u64,
    pub samples: Samples,
}

pub(crate) fn push_sample(samples: &mut Vec<String>, sample: impl FnOnce() -> String) {
    if samples.len() < SAMPLES_KEPT {
        samples.push(sample());
    }
}
