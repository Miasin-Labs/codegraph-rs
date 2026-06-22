use super::*;

/// `codegraph analyze` — the analysis engine (`codegraph-analysis`) running
/// over the bridged SQLite index. Pure reads of the index itself; the
/// bridged graph is snapshotted under `.codegraph/analysis/` and reused
/// until the index changes (`--no-cache` forces a rebuild).
#[derive(Subcommand)]
pub(crate) enum AnalyzeCommands {
    /// Run a pipe-based DSL query over the project graph
    #[command(after_help = "Examples (taken from the engine's test suite):
  codegraph analyze query 'fn(\"main\") | callees | depth 3'
      Everything main() reaches within 3 call hops.
  codegraph analyze query 'path fn(\"main\") -> fn(\"helper\")'
      Shortest call path between two functions (hops listed in path order).
  codegraph analyze query 'scc'
      Strongly-connected components: every mutual-recursion cluster.

Grammar: seed with fn(\"name\"), type(\"Name\"), entrypoints, scc, or hot N;
pipe through callers, callees, depth N, filter kind=K, since N,
reachable via \"Calls+\" [incoming], ...; combine with set algebra
(union / intersect / \\), path patterns (path A -> B,
paths A -> B via Calls), and aggregations (count, exists, group_by, ...).
Use --explain to print the optimised plan without executing, --why to
include per-row provenance (which operators produced each result).
The `untested` operator needs coverage data: pass --lcov <path> to
annotate the graph before the query runs (see `analyze coverage`).")]
    Query {
        /// The DSL query, e.g. 'fn("main") | callees | depth 3'
        #[arg(value_name = "dsl")]
        query: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum result nodes
        #[arg(long = "max-nodes", value_name = "number", default_value = "50")]
        max_nodes: String,
        /// Include why-provenance: which operators/predecessors produced each row
        #[arg(long)]
        why: bool,
        /// Print the optimised query plan without executing it
        #[arg(long)]
        explain: bool,
        /// Annotate LCOV coverage before the query runs (enables `untested`)
        #[arg(long, value_name = "path")]
        lcov: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Per-function complexity metrics (cyclomatic, cognitive, nesting, maintainability)
    Complexity {
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Show the N most complex functions
        #[arg(short = 't', long, value_name = "number", default_value = "20")]
        top: String,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Detect call-graph communities/modules (Louvain)
    Communities {
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum members listed per community
        #[arg(long, value_name = "number", default_value = "8")]
        sample: String,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Dominator analysis from an entry symbol (what every path must pass through)
    Dominators {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum reachable nodes analyzed
        #[arg(short = 't', long, value_name = "number", default_value = "50")]
        top: String,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Program slice from a symbol (call-graph granularity by default)
    Slice {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Slice direction: "fwd" (what it influences) or "bwd" (what affects it)
        #[arg(long, value_name = "fwd|bwd", default_value = "fwd")]
        direction: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum slice depth (call hops)
        #[arg(long, value_name = "number", default_value = "10")]
        depth: String,
        /// Value-level precision via per-function dataflow IR (needs byte
        /// offsets in the index; re-index pre-v5 projects to enable)
        #[arg(long = "value-level")]
        value_level: bool,
        /// Compact source-annotated report (one "name (file:line)" entry per
        /// line) plus the direct data-dependency set, rendered by the
        /// analysis engine's CPG facade. Fixed engine depth; --depth ignored.
        #[arg(long = "source")]
        source_annotated: bool,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Strongly-connected components: mutual recursion and dependency cycles
    Cycles {
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Signature-edit cascade: direct call sites to update, grouped by file
    Impact {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Proposed new signature shown in the generated tasks
        #[arg(short = 's', long, value_name = "signature")]
        signature: Option<String>,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Find call-graph paths from a source symbol to a sink symbol
    Taint {
        /// Source symbol (omit together with sink for --suggest mode)
        #[arg(value_name = "source-symbol")]
        source: Option<String>,
        /// Sink symbol
        #[arg(value_name = "sink-symbol")]
        sink: Option<String>,
        /// Suggest source/sink candidates by identifier naming instead of
        /// tracing paths (the default when no symbols are given)
        #[arg(long)]
        suggest: bool,
        /// Value-level precision via per-function dataflow IR (needs byte
        /// offsets in the index; re-index pre-v5 projects to enable)
        #[arg(long = "value-level")]
        value_level: bool,
        /// Compact source-annotated flow report (each flow rendered hop by
        /// hop as "name (file:line)" with sanitizer status), rendered by the
        /// analysis engine's CPG facade
        #[arg(long = "source")]
        source_annotated: bool,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Maximum intermediate nodes per path
        #[arg(long = "max-nodes", value_name = "number", default_value = "6")]
        max_nodes: String,
        /// Maximum suggested pairs (--suggest mode)
        #[arg(short = 't', long, value_name = "number", default_value = "20")]
        top: String,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Temporal coupling: symbols that change together across git history
    #[command(name = "co-change")]
    CoChange {
        /// Limit pairs to those touching this symbol
        #[arg(value_name = "symbol")]
        symbol: Option<String>,
        /// Minimum co-occurrence count for a pair to be reported
        #[arg(long = "min-support", value_name = "number", default_value = "2")]
        min_support: String,
        /// Maximum commits mined from git log
        #[arg(long = "max-commits", value_name = "number", default_value = "500")]
        max_commits: String,
        /// Show the N strongest pairs
        #[arg(short = 't', long, value_name = "number", default_value = "25")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Map LCOV coverage onto functions; find untested code
    #[command(
        after_help = "Annotating coverage also enables the analysis DSL's `untested` \
operator: `codegraph analyze query --lcov coverage/lcov.info 'entrypoints | untested'`."
    )]
    Coverage {
        /// Path to an LCOV-format coverage file (e.g. coverage/lcov.info)
        #[arg(long, value_name = "path")]
        lcov: String,
        /// List only untested functions
        #[arg(long)]
        untested: bool,
        /// Show at most N functions
        #[arg(short = 't', long, value_name = "number", default_value = "50")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Simulate a signature (arity) change before making it
    Validate {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Parameter count before the edit
        #[arg(long = "params-before", value_name = "number")]
        params_before: String,
        /// Parameter count after the edit
        #[arg(long = "params-after", value_name = "number")]
        params_after: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Trait/interface hierarchies, dispatch calls, and type clusters
    Traits {
        /// Limit output to this trait/type (exact name or qualified suffix)
        #[arg(value_name = "type")]
        type_name: Option<String>,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// PageRank centrality: the most depended-upon symbols
    Centrality {
        /// Show the N most central symbols
        #[arg(short = 't', long, value_name = "number", default_value = "20")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Articulation nodes and bridge edges: single points of failure
    Critical {
        /// Show at most N nodes and N edges
        #[arg(short = 't', long, value_name = "number", default_value = "25")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Export the graph as Graphviz DOT (pipe to `dot -Tsvg`)
    Export {
        /// Output format (only "dot" is supported)
        #[arg(short = 'f', long, value_name = "format", default_value = "dot")]
        format: String,
        /// Export only this symbol's neighborhood instead of the whole graph
        #[arg(short = 's', long, value_name = "symbol")]
        symbol: Option<String>,
        /// Neighborhood depth around --symbol (hops, both directions)
        #[arg(short = 'd', long, value_name = "number", default_value = "2")]
        depth: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Concrete types that can flow into/out of a function
    Types {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Generic definitions and callsite-supplied instantiations
    Generics {
        /// Limit output to this symbol (exact name or qualified suffix)
        #[arg(value_name = "symbol")]
        symbol: Option<String>,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Cross-language boundaries: HTTP routes, FFI and WASM exports
    Boundaries {
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Analysis-engine capability toggles and their env kill-switches
    Capabilities {
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Print the JSON Schema for an engine payload kind
    #[command(
        after_help = "Known kinds: query_result, entrypoint_summary, context_result, \
formatted_output."
    )]
    Schema {
        /// Payload kind (e.g. query_result)
        #[arg(value_name = "kind")]
        kind: String,
        /// Output as JSON (the schema is already JSON; printed identically)
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Bridged-graph statistics, optionally with reachability profiling
    Stats {
        /// Add per-node reachability counts (exact on small graphs,
        /// HyperLogLog estimates on large ones)
        #[arg(long = "estimate-reachability")]
        estimate_reachability: bool,
        /// Show the N widest-reaching nodes
        #[arg(short = 't', long, value_name = "number", default_value = "10")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Control-flow graph of one function: basic blocks + typed edges
    #[command(
        after_help = "Built by re-parsing the on-disk source with the host grammars and \
anchoring the function at its indexed position. CFG rules cover Rust, TypeScript/TSX, \
JavaScript/JSX, Python, Go, Java, C, C++, and PHP; other languages get an honest \
capability note."
    )]
    Cfg {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Per-function dataflow: params, returns, assignments, argument flows, mutations
    #[command(
        after_help = "Built by re-parsing the on-disk source with the host grammars and \
anchoring the function at its indexed position. Dataflow rules cover Rust, \
TypeScript/TSX, JavaScript/JSX, Python, and Go; other languages get an honest \
capability note."
    )]
    Dataflow {
        #[arg(value_name = "symbol")]
        symbol: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Working-tree vs base: what changed since the last cached snapshot
    #[command(
        after_help = "Compares the current bridged graph against a base snapshot: \
nodes/edges added/removed/changed, complexity deltas for changed functions, \
newly-introduced cycles, and the impact set of the delta. --base auto (default) \
uses the last cached snapshot built before the current index state — every \
analyze command caches one, and one previous generation is kept under \
.codegraph/analysis/*.prev. The run also annotates the current snapshot with \
per-function complexity so the NEXT diff reports full before/after deltas."
    )]
    Diff {
        /// Base snapshot: "auto", or a path to a snapshot file / cache directory
        #[arg(long, value_name = "snapshot|auto", default_value = "auto")]
        base: String,
        /// Impact BFS depth from the changed/added/removed symbols
        #[arg(long, value_name = "number", default_value = "3")]
        depth: String,
        /// Maximum entries listed per section
        #[arg(short = 't', long, value_name = "number", default_value = "50")]
        top: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Inference-based vulnerability scan: missing-auth (BAC/IDOR), unsanitized
    /// flows, and the concurrency/control-plane lint — rules inferred from the
    /// graph, not hardcoded
    #[command(
        after_help = "Discovers guards/sinks/sanitizers from the codebase itself \
(deviant-frequency mining over the call graph + name-lexicon taint seeds) and \
flags the call sites that deviate, plus a tree-sitter concurrency lint for \
lossy best-effort sends. Findings below --min-confidence are dropped."
    )]
    Vuln {
        /// Drop findings below this confidence (0.0–1.0)
        #[arg(long = "min-confidence", value_name = "number", default_value = "0.5")]
        min_confidence: String,
        /// Project path
        #[arg(short = 'p', long, value_name = "path")]
        path: Option<String>,
        /// Rebuild the analysis graph from the index, ignoring the cached snapshot
        #[arg(long = "no-cache")]
        no_cache: bool,
        /// Write a SARIF 2.1.0 log to this path (for GitHub Advanced Security, Defender, etc.)
        #[arg(long = "sarif", value_name = "path")]
        sarif: Option<String>,
        /// Write a standalone HTML report to this path
        #[arg(long = "html", value_name = "path")]
        html: Option<String>,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
}
