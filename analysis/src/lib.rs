//! Code graph intelligence: a tree-sitter-backed symbol/call/type graph over
//! the workspace, queryable through a pipe-based DSL with set algebra, path
//! patterns, taint tracing, and preconditions.
//!
//! The graph indexes 12 languages via per-language adapters, stores nodes and
//! edges in a CSR (compressed sparse row) representation for fast traversal,
//! and supports incremental re-indexing driven by a file watcher. Advanced
//! analysis modules include control-flow graphs, dataflow, interprocedural
//! points-to, complexity metrics, community detection, and coverage
//! annotation. A `GraphSession` memoizes query results and invalidates caches
//! after edits.

pub mod adapter;
pub mod analysis;
pub mod analysis_tools;
pub mod bfs_directed;
pub mod builder;
pub mod cache;
pub mod call_site;
pub mod capabilities;
pub mod cascade;
pub mod cfg;
pub mod cfg_rules;
pub mod closure;
pub mod co_change;
pub mod communities;
pub mod complexity;
pub mod complexity_rules;
pub mod concurrency;
pub mod concurrency_rules;
pub mod content_index;
pub mod context;
pub mod coverage;
pub mod csr;
pub mod data_dir;
pub mod dataflow;
pub mod dataflow_rules;
pub mod dominators;
pub mod dsl;
pub mod edges;
pub mod enrichment;
pub mod fingerprint;
pub mod formatting;
pub mod framework_routes;
pub mod frontier;
#[cfg(feature = "gpu")]
pub mod gpu_bfs;
#[cfg(feature = "gpu")]
pub mod gpu_cochange;
#[cfg(feature = "gpu")]
pub mod gpu_dominators;
#[cfg(feature = "gpu")]
pub mod gpu_modularity;
#[cfg(feature = "gpu")]
pub mod gpu_pagerank;
#[cfg(feature = "gpu")]
pub mod gpu_scc;
pub mod graph;
pub mod history;
pub mod hll;
pub mod incremental;
pub(crate) mod index;
pub mod ir;
pub mod ir_map;
pub mod kind_specific;
pub mod label_reachability;
pub mod monomorphize;
pub mod nodes;
pub mod overlay;
pub mod partial;
pub mod pass;
pub mod persistence;
pub mod points_to;
pub mod polyglot;
pub mod possible_types;
pub mod predicates;
pub mod reactive;
pub mod resolver;
pub mod schema;
pub mod session;
pub mod slicing;
pub mod strata;
pub mod symbols;
pub mod taint_naming;
pub mod taint_v2;
pub mod traits_hierarchy;
pub mod traversal;
pub mod validation;
pub mod vuln;
pub mod worktree;

/// Grow the stack before another level of recursive descent.
///
/// Recursive walkers in this crate (CFG/dataflow/complexity/IR lowering,
/// language adapters, Tarjan SCC, Bron–Kerbosch, query-tree evaluation) recurse
/// to a depth set by their input — AST nesting, graph/cycle depth, or query
/// nesting — none of which is bounded a priori. On a worker thread with a fixed
/// stack (scheduler workers, the MCP engine thread) a pathologically deep input
/// would otherwise overflow and abort the process. Calling this at each
/// recursive function's head bounds depth by input size, never by thread stack.
/// Mirrors rustc's `ensure_sufficient_stack` and the root crate's guard.
#[inline]
pub fn ensure_sufficient_stack<R>(f: impl FnOnce() -> R) -> R {
    /// Trigger a new segment once remaining stack drops below this. Must exceed
    /// the deepest guard-free run of frames (one recursion level) with margin.
    const RED_ZONE: usize = 128 * 1024;
    /// Size of each freshly allocated segment — large enough that segment
    /// switches stay rare even on deeply nested inputs.
    const STACK_GROW: usize = 8 * 1024 * 1024;
    stacker::maybe_grow(RED_ZONE, STACK_GROW, f)
}

/// Whether the CUDA runtime (driver + NVRTC) is actually loadable in this
/// process. cudarc's NVRTC loader does not just return an error when the
/// library is missing — it panics, and a destructor in that stack panics
/// again during the unwind, so the process ABORTS (uncatchable by
/// `catch_unwind`). We therefore probe both libraries with `libloading`
/// FIRST — a plain `dlopen` that returns a `Result` — and only let the GPU
/// path call cudarc when both are present. Cached: the answer is fixed for
/// the process lifetime.
#[cfg(feature = "gpu")]
pub fn cuda_runtime_available() -> bool {
    use std::sync::OnceLock;
    static AVAIL: OnceLock<bool> = OnceLock::new();
    *AVAIL.get_or_init(|| {
        // Any one name per library is enough (loaders alias them).
        let cuda = ["libcuda.so", "libcuda.so.1"];
        let nvrtc = [
            "libnvrtc.so",
            "libnvrtc.so.12",
            "libnvrtc.so.13",
            "libnvrtc.so.11",
        ];
        let loadable = |names: &[&str]| {
            names
                .iter()
                .any(|n| unsafe { libloading::Library::new(n) }.is_ok())
        };
        loadable(&cuda) && loadable(&nvrtc)
    })
}

/// Run a GPU probe, returning `None` when the CUDA runtime is unavailable
/// (checked up-front, never triggering cudarc's aborting loader) or when the
/// inner closure itself fails. Used by every `*_gpu` entry point in this crate.
#[cfg(feature = "gpu")]
pub(crate) fn gpu_probe<R>(f: impl FnOnce() -> Option<R>) -> Option<R> {
    if !cuda_runtime_available() {
        return None;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f))
        .ok()
        .flatten()
}
