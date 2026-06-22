use crate::closure::ClosureDirection;
use crate::edges::EdgeKind;
use crate::label_reachability::PatternAtom;
use crate::nodes::NodeKind;

/// Token types produced by the lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    Pipe,
    Fn,
    Type,
    Callers,
    Callees,
    Depth,
    Filter,
    Show,
    Taint,
    /// Backward control-flow analysis: walk *incoming* call edges
    /// to enumerate functions that must have called the target.
    /// The companion `extract_predicates` helper additionally
    /// surfaces the enclosing if/match/while predicate at each
    /// call site as edge metadata. Mirrors Magic's "what must have
    /// been true to reach here?" framing — the dual of `taint`.
    Preconditions,
    Kind,
    Equals,
    /// Left paren — only meaningful in extended grammar (set-op grouping).
    LParen,
    /// Right paren — only meaningful in extended grammar.
    RParen,
    /// Set-algebra union (`A union B`).
    Union,
    /// Set-algebra intersection (`A intersect B`).
    Intersect,
    /// Set-algebra difference, spelled either `A diff B` or `A \ B`.
    Diff,
    /// Path pattern: shortest single path between endpoints.
    Path,
    /// Path pattern: all simple paths between endpoints.
    Paths,
    /// `where` clause introducing intermediate-node predicates.
    Where,
    /// `intermediate` qualifier inside a `where` clause.
    Intermediate,
    /// `via EDGE_KIND` requirement: at least one edge of the given kind on the path.
    Via,
    /// Path-segment arrow (`->` or unicode `→`).
    Arrow,
    /// `entrypoints` selector — returns classified entrypoint nodes.
    Entrypoints,
    /// `since N` postfix filter — restrict working set to nodes whose
    /// `last_modified_revision >= N`. Pairs with
    /// [`crate::graph::CodeGraph::current_revision`] to answer "what
    /// changed since revision N?".
    Since,
    /// `hot N` — top-N by PageRank. As a bare selector ranks the entire
    /// graph; as a postfix op restricts the working set to its hottest N.
    Hot,
    /// `scc` — strongly connected components. Bare selector returns the
    /// union of all multi-element SCC members; as a postfix op restricts
    /// to SCCs containing the working-set nodes.
    Scc,
    /// `dominators of fn("X")` — dominator chain seeded at X.
    Dominators,
    /// `dominates fn("X")` — descendants of X in the dominator tree.
    Dominates,
    /// `of` — connector for `dominators of <expr>`.
    Of,
    /// `trait_impls of type("X")` — implementors of trait X.
    TraitImpls,
    /// `dispatch` — restrict working set to functions whose calls go
    /// through trait-method dispatch (postfix op).
    Dispatch,
    /// `cluster` — used in `cluster by type` (bare or postfix).
    Cluster,
    /// `by` — connector for `cluster by type`.
    By,
    /// `affected N since M` — `nodes_changed_within_depth(M, N)`.
    Affected,
    /// `multi_path {fn("a"), fn("b")} -> fn("c")` — multi-source shortest path.
    MultiPath,
    /// `{` — opens a brace list (multi-source `path` / `multi_path`).
    LBrace,
    /// `}` — closes a brace list.
    RBrace,
    /// `,` — separates entries in a brace list.
    Comma,
    /// `untested` — filter to functions with `coverage_tested == "false"` or
    /// no coverage data at all. Requires [`CoveragePass`] to have run first.
    Untested,
    /// `possible_types` — postfix filter; enriches output with
    /// `possible_input_types` / `possible_return_types` metadata from the
    /// working set. Requires [`PossibleTypesPass`] to have run.
    PossibleTypes,
    /// `co_changes` — postfix operator that computes temporal coupling
    /// (co-change analysis from git history) for the working set. Returns
    /// functions that frequently change together with the selected ones.
    CoChanges,
    /// `communities` — run Louvain community detection; filter to working set.
    Communities,
    /// `complexity` — postfix operator surfacing per-function complexity metrics.
    Complexity,
    /// `cfg` — postfix operator surfacing per-function control flow graph.
    Cfg,
    /// `dataflow` — postfix operator surfacing per-function dataflow analysis.
    Dataflow,
    /// `reachable via "<pattern>"` — label-constrained reachability
    /// expansion of the working set. See [`DslOp::ReachableVia`].
    Reachable,
    String(String),
    Number(usize),
    Ident(String),
}

/// Projection mode for the `show` operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projection {
    Fields,
    Signature,
    Body,
}

/// DSL operations — 9 variants.
///
/// The original plan capped at 8; v2 added `Preconditions` as a
/// targeted extension for backward control-flow analysis (the dual
/// of `Taint`). Keep the list tight: every new variant adds prompt
/// description bytes the LLM has to read on every request, so the
/// bar for adding a 10th operator is "no existing operator can do
/// this with reasonable composition".
#[derive(Debug, Clone, PartialEq)]
pub enum DslOp {
    SelectFn(String),
    SelectType(String),
    Callers,
    Callees,
    Depth(usize),
    Filter(NodeKind),
    Show(Projection),
    Taint(String),
    /// "What must have been true to reach this call site?" — walks
    /// incoming Calls edges (callers) iteratively up to the configured
    /// depth, with cycle detection. Like `Callers + Depth` but
    /// semantically distinct: indicates the model wants
    /// preconditions-style reasoning, not just a flat caller list.
    /// The renderer pairs this with `extract_predicates` over each
    /// caller's source span to surface the actual enclosing if/match
    /// expression text.
    Preconditions,
    /// `since N` postfix filter — restricts the working set to nodes whose
    /// `last_modified_revision >= N`. Composes with any other operator;
    /// chain it last in a pipe-query to answer "of the things selected,
    /// which changed since revision N?". The argument is a `u64` so callers
    /// can pass the value of [`crate::graph::CodeGraph::current_revision`]
    /// captured before a batch of mutations.
    Since(u64),
    /// `hot N` — top-N functions by PageRank centrality.
    ///
    /// Two semantics depending on chain position:
    /// - **Bare** (`hot 10`): rank every function in the graph; keep top N.
    /// - **Postfix** (`fn("auth") | callees | hot 5`): rank only nodes
    ///   already in the working set; keep its top N.
    ///
    /// Centrality is computed lazily (`graph.hottest_functions(n)` for the
    /// bare form; `graph.centrality()` then a re-rank for the postfix
    /// form). One PageRank computation per `hot` op — the working set is
    /// usually small relative to the graph, so re-running on the bare
    /// form vs. caching is a wash.
    Hot(usize),
    /// `scc` — strongly connected components.
    ///
    /// - **Bare** (`scc`): return every member of every multi-element SCC
    ///   in the graph. Singletons are excluded — the answer to "what
    ///   loops?" not "every node is in some SCC".
    /// - **Postfix** (`fn("foo") | scc`): restrict to the SCC(s)
    ///   containing the working-set nodes. Empty result if every input is
    ///   a singleton.
    ///
    /// `QueryResult.metadata` describes each surviving cluster as a line
    /// of the form `SCC[i] size=N members=[name, name, ...]`.
    Scc,
    /// `dispatch` — postfix filter restricting the working set to
    /// functions that participate in trait dispatch as the *caller*.
    /// Computed via [`crate::graph::CodeGraph::trait_dispatch_calls`].
    /// Pairs with `fn("foo") | dispatch` to answer "which calls in foo
    /// go through trait dispatch?".
    Dispatch,
    /// `cluster by type` — group functions by their primary type
    /// (most-frequent `UsesType` target). When chained postfix
    /// (`<expr> | cluster by type`), only functions in the working set
    /// participate.
    ///
    /// Result: union of every function across every cluster (the
    /// clustering itself surfaces in `metadata` as `cluster[i] type=T
    /// size=N members=[...]`).
    ClusterByType,
    /// `affected N since M` — every node within `N` undirected hops of
    /// any node modified at or after revision `M`. Wraps
    /// [`crate::graph::CodeGraph::nodes_changed_within_depth`].
    ///
    /// Spelled `affected` rather than reusing `depth`/`since` because
    /// `since N | depth M` would semantically mean "filter to recent,
    /// then expand outgoing N hops" — different ordering and direction.
    /// `affected N since M` is undirected and seeded at *every* recent
    /// node, which is what code-review questions like "what's near my
    /// recent changes?" actually want.
    Affected {
        depth: usize,
        since_rev: u64,
    },
    /// `untested` — postfix filter restricting the working set to Function
    /// nodes with `metadata["coverage_tested"] != "true"`. Functions that
    /// haven't been annotated by a coverage pass at all are included (they
    /// are presumed untested). Use after `run_coverage` to find dead code.
    Untested,
    /// `possible_types` — postfix enrichment operator that surfaces
    /// `metadata["possible_input_types"]` and `metadata["possible_return_types"]`
    /// for nodes in the working set. Also retains only nodes that have at
    /// least one possible type (filters out functions with no type edges).
    PossibleTypes,
    /// `co_changes` — postfix operator. For the current working set, runs
    /// co-change analysis against git history and expands the working set
    /// to include temporally coupled nodes. Metadata lines describe the
    /// coupling strength. Uses `min_support=2` by default (pairs must
    /// co-occur at least twice).
    CoChanges,
    /// `communities` — run Louvain community detection on the full graph,
    /// then filter results to the working set. Metadata lines describe
    /// each community as `community N: [node1, node2, ...]`.
    Communities,
    /// `complexity` — postfix enrichment operator surfacing per-function
    /// complexity metrics (cognitive, cyclomatic, nesting, Halstead, LOC,
    /// maintainability index) for nodes in the working set. Retains only
    /// Function nodes that have complexity data populated.
    Complexity,
    /// `cfg` — postfix enrichment operator surfacing per-function control
    /// flow graph for nodes in the working set. Retains only Function nodes
    /// that have CFG data populated.
    Cfg,
    /// `dataflow` — postfix enrichment operator surfacing per-function
    /// dataflow analysis (params, returns, assignments, arg flows, mutations)
    /// for nodes in the working set. Retains only Function nodes that have
    /// dataflow data populated.
    Dataflow,
    /// `reachable via "<pattern>" [incoming|outgoing]` — label-constrained
    /// reachability (RLC). Expands the working set to every node reachable
    /// via a path whose **edge-label sequence** matches the pattern, riding
    /// [`crate::label_reachability::reachable_targets`]. Unlike `depth N`
    /// (label-agnostic BFS) or `path ... via K` (≥1 edge of kind K anywhere
    /// on the path), the pattern constrains the *whole sequence*:
    /// `fn("handler") | reachable via "Calls+ Implements"` reaches only
    /// nodes at the end of one-or-more `Calls` edges followed by exactly
    /// one `Implements` edge.
    ///
    /// Direction defaults to `outgoing`; `incoming` runs the product BFS
    /// over reverse edges ("who reaches me via this label sequence").
    /// Seeds are *replaced* by the reachable set — a seed survives only if
    /// the pattern matches the zero-length path (empty pattern or all-`*`
    /// atoms). An empty working set is reported honestly in `metadata`
    /// rather than silently returning nothing.
    ReachableVia {
        pattern: Vec<PatternAtom>,
        direction: ClosureDirection,
    },
}

/// Top-level expression supporting set algebra, path patterns, and
/// entrypoint selectors on top of the legacy pipe-chain grammar.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// A traditional pipe-chain (e.g. `fn("foo") | callers | depth 3`).
    Pipe(Vec<DslOp>),
    /// `path` (single shortest) or `paths` (all simple) between two
    /// expressions, with optional intermediate-kind, edge-via, and
    /// depth qualifiers.
    PathQuery(PathQuery),
    /// `entrypoints` selector — optionally filtered to a specific
    /// [`EntrypointKind`].
    Entrypoints(Option<EntrypointKind>),
    /// Set-algebra binary op: union / intersect / diff (left minus right).
    SetOp {
        op: SetOp,
        left: Box<Expr>,
        right: Box<Expr>,
    },
    /// `dominators of <expr>` — for each node selected by `<expr>`, walk
    /// the dominator chain in the call graph and union the result.
    ///
    /// The dominator tree is rooted at a synthetic entry chosen by
    /// [`pick_dominator_root`]: prefer `fn main`, fall back to the
    /// function with the highest fan-in. The call graph isn't natively
    /// single-entry so this is a documented heuristic; callers needing a
    /// specific root should use [`crate::dominators::Dominators::build`]
    /// directly.
    DominatorsOf(Box<Expr>),
    /// `dominates <expr>` — for each node selected by `<expr>`, return
    /// every node it dominates (descendants in the dominator tree).
    /// Same root-selection policy as `DominatorsOf`.
    DominatesOf(Box<Expr>),
    /// `trait_impls of <expr>` — for each `Trait` node selected by
    /// `<expr>`, return the union of its direct implementors via
    /// `Implements` edges. Backed by
    /// [`crate::graph::CodeGraph::trait_hierarchies`].
    TraitImplsOf(Box<Expr>),
    /// An atom expression (e.g. `entrypoints`, `dominators of ...`)
    /// followed by pipe operators (e.g. `| untested | depth 3`).
    /// The atom is executed first to produce a working set, then the
    /// pipe ops are applied as postfix filters/transforms.
    PipeFrom { base: Box<Expr>, ops: Vec<DslOp> },
    /// `multi_path { <expr>, <expr>, ... } -> <expr>` — multi-source
    /// shortest path. Wraps
    /// [`crate::traversal::find_path_multi_source`]: every source set is
    /// seeded into a single BFS so the shortest path from *any* source
    /// to `to` wins. Optional trailing `depth N` qualifier (default 32).
    MultiPath {
        sources: Vec<Expr>,
        to: Box<Expr>,
        max_depth: Option<usize>,
    },
}

/// Set-algebra operator on `QueryResult` node sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetOp {
    Union,
    Intersect,
    /// Asymmetric difference (`A \ B` = nodes in A but not in B).
    Diff,
}

/// Path-pattern flavor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMode {
    /// Single shortest path.
    Shortest,
    /// All simple (non-self-intersecting) paths.
    AllSimple,
}

/// Parsed `path` / `paths` query AST.
#[derive(Debug, Clone, PartialEq)]
pub struct PathQuery {
    pub mode: PathMode,
    pub from: Box<Expr>,
    pub to: Box<Expr>,
    /// `where intermediate kind=K` — restrict intermediate node kinds.
    pub intermediate_kind: Option<NodeKind>,
    /// `via EdgeKind` — require at least one edge of this kind on the path.
    pub via_edge: Option<EdgeKind>,
    /// `depth N` — bound search depth (default 32 if unspecified).
    pub max_depth: Option<usize>,
}

/// Coarse classification of program entrypoints — mirrors the categories
/// produced by [`crate::analysis::CodeGraph::classify_entrypoints`] one-to-one.
/// Kept as a separate enum (rather than re-exporting `analysis::EntrypointKind`)
/// because the DSL surface is the user-facing keyword set: callers spell
/// `Main`/`PublicApi`/`Test`/`Bench`/`FfiExport` in queries and we translate
/// at the executor boundary. If `analysis::EntrypointKind` ever grows or
/// renames, the DSL keyword surface stays stable until we choose to expose
/// the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrypointKind {
    /// `fn main` at module root.
    Main,
    /// Public function exposed at crate root or `pub mod`.
    PublicApi,
    /// `#[test]`, `#[tokio::test]`, integration tests, etc.
    Test,
    /// `#[bench]` benchmark harness.
    Bench,
    /// FFI export: `pub extern "..." fn` or `#[no_mangle]`.
    FfiExport,
}

impl EntrypointKind {
    /// Translate an `analysis::EntrypointKind` (the source of truth) into the
    /// DSL-facing variant. The two enums currently align 1:1 but kept
    /// separate so the DSL keyword surface can evolve independently.
    pub(super) fn from_analysis(k: crate::analysis::EntrypointKind) -> Self {
        match k {
            crate::analysis::EntrypointKind::Main => Self::Main,
            crate::analysis::EntrypointKind::PublicApi => Self::PublicApi,
            crate::analysis::EntrypointKind::Test => Self::Test,
            crate::analysis::EntrypointKind::Bench => Self::Bench,
            crate::analysis::EntrypointKind::FfiExport => Self::FfiExport,
        }
    }
}
