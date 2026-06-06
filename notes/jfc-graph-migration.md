# jfc-graph → codegraph-analysis migration

Date: 2026-06-06
Source: `/home/cole/RustProjects/active/jfc/crates/jfc-graph` (untouched — jfc keeps its copy)
Destination: `/home/cole/RustProjects/active/codegraph-rs/analysis/` — workspace member
`codegraph-analysis` (lib name `codegraph_analysis`), version 0.1.0, edition 2024.

Everything moved 100%: 89 `src/` files, 9 `tests/` files (incl. fixtures and
`.gitkeep`), 1 bench, 1 example, 4 top-level docs (`README.md`, `PLAN.md`,
`DESIGN_FUTURE.md`, `ADAPTER_PARITY.md`), and the full `research/` corpus
(7,715 files, ~259 MB).

## Licensing

The jfc workspace is `AGPL-3.0`. Cole Leavitt is the **sole author** of
jfc-graph (verified), so he is entitled to relicense his own work: this copy
is published under **MIT**, matching the rest of codegraph-rs (`LICENSE` at
the repo root). The original under `jfc/crates/jfc-graph` remains AGPL-3.0 and
was not modified in any way.

## Capability inventory (every module, one line each)

### Crate root
- `lib.rs` — crate root: tree-sitter-backed symbol/call/type graph, queryable via a pipe-based DSL with set algebra, path patterns, taint tracing, and preconditions; declares all 60+ modules.

### Language adapters (`src/adapter/`, 12 languages)
- `adapter/mod.rs` — `LanguageAdapter` trait + `AdapterRegistry` mapping file extensions to adapters; parse outcomes and syntax-error surfacing.
- `adapter/rust.rs` — Rust adapter (tree-sitter-rust): functions, impls, traits, structs/enums, call sites, IR lowering.
- `adapter/typescript.rs` — TypeScript/TSX adapter (tree-sitter-typescript).
- `adapter/python.rs` — Python adapter (tree-sitter-python).
- `adapter/go.rs` — Go adapter (tree-sitter-go).
- `adapter/cpp.rs` — C++ adapter (`.cpp/.cc/.cxx/.hpp/.hh/.hxx`, tree-sitter-cpp).
- `adapter/c.rs` — C adapter (`.c/.h`, tree-sitter-c).
- `adapter/java.rs` — Java adapter (tree-sitter-java).
- `adapter/csharp.rs` — C# adapter (tree-sitter-c-sharp).
- `adapter/php.rs` — PHP adapter (tree-sitter-php).
- `adapter/ruby.rs` — Ruby adapter (tree-sitter-ruby).
- `adapter/kotlin.rs` — Kotlin adapter (tree-sitter-kotlin-sg).
- `adapter/swift.rs` — Swift adapter (tree-sitter-swift).

### Core graph model & storage
- `graph.rs` — core `CodeGraph` on petgraph `StableGraph` (stable `NodeIndex` across removals); node-ID allocation contract pinned by `tests/node_id_stability.rs`.
- `nodes.rs` — `NodeData` / `NodeKind` / `Span` / `Visibility` node model.
- `edges.rs` — `EdgeData` / `EdgeKind` relationship semantics.
- `kind_specific.rs` — typed per-kind metadata accessors layered over the serialised `NodeData` shape.
- `index.rs` (pub(crate)) — fast-lookup secondary indices over `CodeGraph` (by kind, name, file…).
- `csr.rs` — read-optimised CSR (compressed sparse row) snapshot of the graph for fast traversal.
- `frontier.rs` — hybrid sparse/dense vertex frontier for BFS-style traversals (Ligra/Yang 2018).
- `symbols.rs` — symbol table mapping human-readable handles to node locations for semantic editing, with recursive-resolution cycle detection.

### Build / index pipeline
- `builder.rs` — workspace walker + graph builder: runs adapters over files, populates the graph, runs the cross-file resolver, lowers IR.
- `call_site.rs` — unresolved call-site capture during extraction, fed to the resolver post-pass.
- `resolver.rs` — cross-file call-reference resolver: matches captured call sites to same-named definitions with path-proximity scoring.
- `enrichment.rs` — LSP enrichment layer: `LspDataProvider` trait (implemented by jfc's LspClient downstream) resolves `UnresolvedCall` edges precisely.
- `content_index.rs` — persistent, mtime-validated content index backing `graph_grep`.
- `framework_routes.rs` — web-framework route detection (codegraph `route`-node parity).
- `polyglot.rs` — cross-language boundary detection (HTTP routes, gRPC, FFI, WASM exports) emitting edges across language subgraphs.

### Incrementality / persistence / caching
- `fingerprint.rs` — iteration-order-independent BLAKE3 fingerprints of graph state (cross-machine stable; golden-pinned by `tests/fingerprint_stability.rs`).
- `cache.rs` — in-memory per-file memoization + opt-in on-disk persistence keyed by content fingerprint (`JFC_GRAPH_CACHE_DIR`, `$HOME/.cache/jfc-graph/v1`).
- `incremental.rs` — hand-rolled Adapton-style incremental query cache with read-set tracking.
- `reactive.rs` — demand-driven reactive query framework (Salsa-lite): fine-grained input→output dependency tracking.
- `persistence.rs` — event-sourced graph persistence (base snapshot + ordered events, undo via replay).
- `overlay.rs` — base-graph + branch-diff overlay so monorepo contributors share a base index (postcard-serialised snapshots).
- `partial.rs` — partial struct selection: field-level granularity for context windows.
- `data_dir.rs` — per-workspace data-dir resolution (`JFC_GRAPH_DATA_DIR` → XDG cache → `$HOME/.cache/jfc-graph/<hash>` → in-workspace `.jfc-graph/` fallback).
- `worktree.rs` — detects when a cached index belongs to a different git worktree and refuses to silently borrow the parent checkout's index.
- `capabilities.rs` — modular capability tree; features disabled via `JFC_GRAPH_CAP_*=0/false/off/no` env vars.
- `pass.rs` — pass framework: passes declare precondition/postcondition `GraphFlag`s, `PassManager` orders and runs them.
- `history.rs` — recent-query history store for inspection.

### Traversal & graph algorithms
- `traversal.rs` — BFS/DFS via petgraph `Bfs`/`Dfs`/`Reversed` (cycle-detected, depth-bounded); `traverse_csr` and `traverse_petgraph` paths.
- `bfs_directed.rs` — direction-optimised push/pull BFS over the CSR snapshot (GraphBLAS push-pull, Yang 2018).
- `closure.rs` — transitive-closure / fixpoint operations over the call graph (bounded depth-N reachability).
- `hll.rs` — HyperLogLog approximate per-node reachability for >100k-node graphs.
- `label_reachability.rs` — label-constrained reachability (RLC): "reachable via a path whose edge-label sequence matches a constraint".
- `dominators.rs` — dominator-tree computation, generic over any directed graph with an entry node.
- `analysis.rs` — petgraph algorithm suite: SCC (Tarjan, mutual recursion), critical nodes, and related whole-graph metrics.
- `communities.rs` — Louvain modularity community detection on the undirected projection.
- `strata.rs` — stratified negation for rule-based queries.

### Intraprocedural analysis (IR, CFG, dataflow, complexity)
- `ir.rs` — language-agnostic intermediate representation; adapters lower ASTs once, analyses share it.
- `ir_map.rs` — interprocedural IR map built from a live graph; the real `DataflowOracle` for slicing/taint.
- `cfg.rs` — per-function control-flow graph (basic blocks, typed edges) from tree-sitter ASTs.
- `cfg_rules.rs` — per-language tree-sitter-node-kind → control-flow-construct rules.
- `dataflow.rs` — per-function dataflow: parameter flows, returns, assignments, argument flows, mutation detection.
- `dataflow_rules.rs` — per-language dataflow extraction rules.
- `complexity.rs` — cognitive/cyclomatic complexity, nesting depth, Halstead, LOC, maintainability index.
- `complexity_rules.rs` — per-language complexity rules (branches, nesting, logical ops, Halstead operators).
- `predicates.rs` — backward control-flow predicate extraction powering the `preconditions` DSL operator.

### Interprocedural analysis
- `points_to.rs` — Andersen-style field-sensitive, flow-insensitive points-to analysis over the IR.
- `possible_types.rs` — possible-subtype propagation: concrete types flowing into/out of each function.
- `monomorphize.rs` — generic monomorphization detection: callsites supplying concrete type arguments annotate generic defs.
- `slicing.rs` — forward and backward program slicing over the dataflow oracle.
- `taint_v2.rs` — interprocedural taint analysis: sources → sinks across call boundaries, sanitizer-aware.
- `taint_naming.rs` — bimodal name-based taint source/sink inference (Fluffy / fluentTQL line of work).
- `traits_hierarchy.rs` — trait/type-centric analyses: implementation hierarchies, trait-dispatch call edges, type-based clustering.
- `co_change.rs` — temporal coupling from git history (functions/files that change together).
- `coverage.rs` — LCOV parsing; annotates function nodes with line/branch coverage.
- `cascade.rs` — generates structured `CascadeTask`s (grouped by file) when a signature change ripples to call sites.
- `validation.rs` — virtual edit validation: pre-commit simulation of signature changes against the graph.

### Query DSL (`src/dsl/`)
- `dsl/mod.rs` — the pipe-based graph query DSL: grammar, parser, evaluator, set algebra, path patterns, taint operator (~4.3k lines).
- `dsl/aggregate.rs` — aggregation, edge selection, let-bindings, and quantifiers (non-node-set results).
- `dsl/plan.rs` — query-plan optimiser.
- `dsl/provenance.rs` — why-provenance: which inputs justified each result row.
- `dsl/stream.rs` — streaming/lazy iterator execution path through the DSL.

### Agent-facing context engine (`src/context/`)
- `context/mod.rs` — agent-friendly context builder mirroring codegraph's `codegraph_context`/`explore`/`search`/`callers`/`callees`/`impact` shapes.
- `context/budget.rs` — adaptive output budget scaled by indexed-file count.
- `context/clustering.rs` — symbol clustering + merged source-snippet extraction for `include_source`.
- `context/dataflow_seed.rs` — DRACO dataflow-guided retrieval seeding (arXiv:2405.17337).
- `context/expansion.rs` — subgraph expansion strategies layered on BFS traversal.
- `context/heuristics.rs` — lightweight intent detection over task descriptions (feature vs bug framing).
- `context/measure.rs` — quantified before/after measurement for the retrieval gate.
- `context/render.rs` — consistent markdown rendering for context/caller/callee/impact output.
- `context/resolver.rs` — qualified-name resolution with multi-language separators (`a.b`, `a::b`, `a#b`).
- `context/retrieval_gate.rs` — Repoformer-style when-to-retrieve gating (arXiv:2403.10059): abstain from graph retrieval on self-contained queries.

### Output / interop / facade
- `analysis_tools.rs` — agent-facing CPG façade: slicing, data dependencies, and taint flows as compact source-annotated, path-capped reports.
- `formatting.rs` — token-budgeted output formatting for query results.
- `schema.rs` — stable JSON Schemas (`QueryResult`, `EntrypointSummary`, `ContextResult`, `FormattedOutput`) + envelope helpers for downstream tools.
- `session.rs` — `GraphSession` high-level facade (the single entry point the host application consumes): memoizes queries, invalidates after edits.

### Tests / benches / examples / docs
- `tests/fingerprint_stability.rs` — golden BLAKE3 fingerprint values; pins cross-version/cross-machine fingerprint stability.
- `tests/node_id_stability.rs` — pins the node-ID allocation contract documented in `graph.rs`.
- `tests/fixtures/` — Rust fixtures (`sample.rs`, `deep_call_chain.rs`, `mutual_recursion.rs`, `partial_struct.rs`, `multi_file/`).
- `benches/graph_bench.rs` — criterion benchmarks: CSR vs petgraph traversal, DSL queries, critical-node analysis, incremental query cache.
- `examples/try_context.rs` — drives the context engine against a real workspace; exercises schema wrapping, overlay snapshots, data-dir resolution, worktree detection.
- `README.md`, `PLAN.md`, `DESIGN_FUTURE.md`, `ADAPTER_PARITY.md` — design/parity/roadmap docs (crate references renamed).
- `research/` — reference corpus moved verbatim (papers as PDF+txt on incremental computation, GraphBLAS/Ligra/GraphIt, Datalog, CPG+LLM, taint slicing, agent economies; vendored reference checkouts: arbor, joern, petgraph, rust-analyzer, salsa, stack-graphs, tokio, tree-sitter, tree-sitter-graph, tree-sitter-rust, rust, lsp-types).

## Renames performed

- `jfc_graph` → `codegraph_analysis` in every crate path (use statements, qualified paths, doc examples) across `src/`, `tests/`, `benches/`, `examples/`.
- `jfc-graph` → `codegraph-analysis` in **comments/docs that name the crate** (module headers in `pass.rs`, `dsl/plan.rs`, `framework_routes.rs`, `fingerprint.rs`, `dominators.rs`, `enrichment.rs`, `taint_v2.rs`, `context/mod.rs`, bench/test/example headers, `cargo run/bench -p` command refs), the user-facing worktree mismatch message in `worktree.rs`, and the `QueryResult` JSON-schema description in `schema.rs` (nothing asserts that string).
- In the four moved markdown docs: `crates/jfc-graph` → `analysis`, `jfc-graph` → `codegraph-analysis`, `jfc_graph` → `codegraph_analysis`. Mentions of "jfc" the host application (e.g. "`GraphSession` that jfc consumes", future `jfc-lsp` binary) were deliberately kept — they are historical context about the original consumer.
- Test-fixture/temp-dir strings containing `jfc` (`jfc_routes_*`, `jfc-ci-*`, `jfc-graph-overlay-test-*`, `jfc_irmap_test_*`, `jfc-graph-worktree-test-*`, `jfc-graph-try-base.json`, `crates/jfc/src/...` resolver-scoring fixtures) were NOT renamed — they are domain strings, not crate references.

## Cargo changes

`analysis/Cargo.toml` (was `crates/jfc-graph/Cargo.toml`):
- `name = "codegraph-analysis"`, `[lib] name = "codegraph_analysis"`, `version = "0.1.0"`, `description` kept.
- `edition.workspace = true` → `edition = "2024"`; `license.workspace = true` (AGPL-3.0) → `license = "MIT"`; `publish.workspace = true` → `publish = false` (same resolved value as in jfc).
- Workspace deps inlined with jfc's concrete versions: `serde = { version = "1.0.221", features = ["derive", "rc"] }`, `serde_json = { version = "1.0.144", features = ["preserve_order", "raw_value"] }`, `thiserror = "2.0.12"`, `tracing = "0.1.40"`.
- `[lints] workspace = true` → inlined verbatim copy of jfc's `[workspace.lints.rust]` / `[workspace.lints.clippy]` blocks (the code was written against those allow/deny levels, e.g. `unexpected_cfgs = allow`).
- All 12 grammar crates kept at identical versions (they bind through `tree-sitter-language`, so they are runtime-version-agnostic).

Root `Cargo.toml`: added only

```toml
[workspace]
members = ["analysis"]
```

The root package stays the implicit workspace root; nothing else restructured.
`Cargo.lock` regenerated to include the analysis crate's dependency tree.

### Deviation: tree-sitter `=0.25.10` → `=0.26.9` (forced)

The migration brief assumed jfc-graph's pinned `tree-sitter = "=0.25.10"` could
coexist with codegraph-rs's `tree-sitter = "0.26"` in one workspace. **It
cannot:** the `tree-sitter` crate declares `links = "tree-sitter"`, and Cargo
rejects any dependency graph (the workspace shares one) containing two
versions of a `links` package — `cargo check` fails at version resolution,
before compiling anything. Resolution options were (a) drop workspace
membership, (b) downgrade codegraph-rs (forbidden — must stay untouched), or
(c) unify the analysis crate on 0.26. Chose (c), pinned **exact** (`=0.26.9`)
to preserve the original determinism intent:

- The crate's runtime API surface is only `Parser`/`Tree`/`Node`/`Language` — source-compatible across 0.25 → 0.26 (zero code changes needed).
- AST shape comes from the grammar crates, which are unchanged.
- Full evidence it's safe: all 783 tests pass, including the golden `fingerprint_stability` and `node_id_stability` suites, with zero warnings.

The original pin + rationale comment is preserved/annotated in
`analysis/Cargo.toml`. If upstream jfc-graph changes are ever re-synced, this
is the one intentional divergence to remember.

## jfc-specific behavior kept (follow-up candidates)

Behavior was preserved per the brief. **Update 2026-06-06 (codegraph-native
rebrand):** items 1–5 are now DONE — this copy is fully independent of jfc,
so the rebrand deliberately breaks compatibility with jfc's caches/env vars
(no fallback to old names, by design). See "Codegraph-native rebrand" below.

1. ~~**Env vars**~~ **DONE:** `JFC_GRAPH_DATA_DIR` → `CODEGRAPH_ANALYSIS_DATA_DIR` (`data_dir.rs`), `JFC_GRAPH_CACHE_DIR` → `CODEGRAPH_ANALYSIS_CACHE_DIR` (`cache.rs`), `JFC_GRAPH_CAP_*` → `CODEGRAPH_ANALYSIS_CAP_*` (`capabilities.rs`, all 6). No old-name fallback — deliberate, this copy has no jfc users.
2. ~~**On-disk locations**~~ **DONE:** data dir is now `$XDG_CACHE_HOME|$HOME/.cache/codegraph-analysis/<workspace-hash>/`, analysis cache `$HOME/.cache/codegraph-analysis/v1/`, in-workspace fallback `.codegraph-analysis/`, and the worktree-convention doc mention is `.codegraph-worktrees/`. Pre-rebrand caches are orphaned by design (cold-rebuild only, no correctness impact).
3. ~~**Fingerprint domain strings**~~ **DONE:** now `"codegraph-analysis::CodeGraph::nodes"` / `"codegraph-analysis::CodeGraph::edges"` — a deliberate one-time fingerprint break recorded in a comment at the hash site in `fingerprint.rs`. No golden constants needed rebasing: `tests/fingerprint_stability.rs` pins *relational* invariants (order-independence, sensitivity), and the two pinned hex constants in `fingerprint.rs`'s `fingerprint_is_cross_machine_stable` hash `"hello world"`/empty — neither includes the domain prefix. All 6 + 4 stability tests pass unchanged.
4. ~~**Tracing targets**~~ **DONE:** `jfc::graph::{resolver,session,builder,pass,parser}` → `codegraph::analysis::{resolver,session,builder,pass,parser}`.
5. ~~**`session.rs` / `enrichment.rs` jfc-as-consumer docs**~~ **DONE (reworded):** comments now say "the host application" instead of jfc. Still open: `enrichment::LspDataProvider` has no in-repo implementor (jfc's LspClient was the implementor) — decide whether codegraph-rs grows its own provider or the trait stays dormant.
6. ~~**`research/arbor/` embedded `.git`**~~ **RESOLVED:** `/analysis/research/` is gitignored (decision committed with the absorption) — the 259 MB corpus stays on disk for reference but out of version control; jfc continues to version the original. No gitlinks recorded.
7. ~~**Functional overlap with the host crate**~~ **RESOLVED BY POLICY** (adjudicated in `notes/prototype-gap-matrix.md`): `worktree.rs` and `framework_routes.rs` are SUPERSEDED rows — the host's `sync::worktree` and 21 framework resolvers win, the engine modules stay engine-internal/test-support only. `data_dir.rs` remains the engine's own cache-location logic; the host-facing analysis cache lives under `<project>/.codegraph/analysis/` via the snapshot cache, so the two don't collide. `enrichment::LspDataProvider` (item 5 residual) is classified N/A until a codegraph-lsp exists.

## Codegraph-native rebrand (2026-06-06)

Second wave: every jfc-ism in `analysis/` renamed to codegraph-native. This
copy has NO jfc-compat constraints (jfc keeps its own copy), so old env vars,
cache paths, and fingerprint domains were dropped without fallbacks.

- Env vars, on-disk dirs, tracing targets, fingerprint domains: items 1–4
  above.
- Comment/doc rewording (jfc-as-host → codegraph/host-application phrasing):
  `session.rs`, `enrichment.rs`, `overlay.rs`, `cache.rs`, `kind_specific.rs`,
  `incremental.rs`, `analysis_tools.rs`, `label_reachability.rs`,
  `context/retrieval_gate.rs`, `worktree.rs` (incl. the `repo/.jfc/` →
  `repo/.codegraph/` monorepo example), `data_dir.rs`,
  `examples/try_context.rs`, host-side `src/bin/codegraph.rs` +
  `tests/analyze_cli_test.rs` headers, `analysis/README.md` (`GraphSession`
  consumer row), `analysis/DESIGN_FUTURE.md` (`jfc-lsp` → `codegraph-lsp`).
- Test-fixture/temp-dir strings (previously deliberately kept) also renamed
  so the sweep is clean: `jfc_routes_*` → `codegraph_routes_*`, `jfc-ci-*` →
  `codegraph-ci-*`, `jfc-graph-{overlay,worktree}-test-*` →
  `codegraph-analysis-…`, `jfc_irmap_test_*`/`/nonexistent/jfc_irmap/` →
  `codegraph_…`, `jfc_analysistools_*` → `codegraph_analysistools_*`,
  `/tmp/jfc-test-root` → `/tmp/codegraph-test-root`,
  `jfc-graph-try-base.json` → `codegraph-analysis-try-base.json`, and the
  resolver path-proximity fixtures `crates/jfc/...` → `crates/app/...`
  (same segment count, scores unchanged).
- Remaining `jfc` mentions after the sweep (`rg -i jfc analysis/src src`),
  all deliberate: the one-time-rebrand comment at the domain-string hash
  site in `analysis/src/fingerprint.rs` (references the old prefix + this
  file), historical attribution in the root `README.md`, and this notes file.
- Verified: `cargo test --workspace` green (773 + 6 + 4 analysis tests, 427
  host lib tests, all integration suites), `cargo clippy --workspace
  --all-targets` zero warnings, `cargo fmt --all --check` clean.

## Verification (all green, 2026-06-06)

- `cargo check -p codegraph-analysis` — clean.
- `cargo test -p codegraph-analysis` — 773 unit + 6 fingerprint_stability + 4 node_id_stability passed, 0 failed (2 ignored doc-tests).
- `cargo bench -p codegraph-analysis --no-run` — `graph_bench` compiles.
- `cargo build -p codegraph-analysis --examples` — `try_context` compiles.
- `cargo test -p codegraph-rs --lib` — 418 passed, 0 failed (existing package untouched and green).
- `cargo check --workspace --all-targets` — zero warnings, zero errors.
- Exactly one `tree-sitter` (0.26.9) in `Cargo.lock`; grammar crate versions unified across both packages.

## User-facing exposure (2026-06-06, follow-up)

The migrated engine is now reachable by users through the CLI and library
API — **not** through MCP (per the retrieval philosophy: new MCP tools get
under-picked; the agent-facing tool surface in `src/mcp/` is untouched).

### What was added

- **`src/analyze.rs`** (new library module, `codegraph::analyze`) — report
  runners that drive the analysis crate's public API over a graph produced
  by `analysis_bridge::build_analysis_graph`. Every report is a serde struct
  with a stable camelCase JSON shape. Includes `CallGraphOracle`, a
  `slicing::DataflowOracle` implementation derived from `Calls`/
  `UnresolvedCall` edges — the "coarse interprocedural pass derived from
  Calls edges" the slicing module documents as the drop-in until real IR is
  available (`def_uses` → callers, `use_defs` → callees, matching
  `PointsToOracle`'s direction convention).
- **`codegraph analyze` subcommand family** (`src/bin/codegraph.rs`, same
  clap-derive + output conventions as the rest of the CLI; human output by
  default, `--json` for the stable shape):
  - `complexity [--top N]` → `complexity::compute_complexity` — re-parses
    on-disk sources with the host's compiled grammars, locates each indexed
    function by its recorded line/column (the bridge has no byte ranges) via
    the language's `complexity_rules::LangRules` body fields, and reports
    cyclomatic/cognitive/nesting/LOC/Halstead/maintainability. 12 languages
    have rules; everything else lands in the report's `skipped` breakdown.
  - `communities` → `communities::louvain` (resolution 1.0, fixed seed 42
    for determinism); singletons summarized, multi-member communities listed.
  - `dominators <symbol>` → `traversal::traverse` (reachable set, BFS order,
    `--top` cap) + `analysis::dominator_chain` per node for immediate
    dominator + chain depth.
  - `slice <symbol> [--direction fwd|bwd]` → `slicing::forward_slice` /
    `backward_slice` over `CallGraphOracle`.
  - `cycles` → `analysis::find_mutual_recursion` (SCCs incl. self-loops and
    module/import cycles — the bridge maps files to Module nodes, so import
    cycles surface here too) + `analysis::cycle_break_suggestions`.
  - `impact <symbol> [--signature <sig>]` → `cascade::generate_cascade` —
    per-call-site signature-edit cascade grouped by file, distinct from the
    BFS radius of the pre-existing `codegraph impact`.
  - `taint <source> <sink>` → `analysis::taint_paths` (all simple paths,
    `--max-nodes` intermediate cap), each hop annotated with its edge kind.
- Symbol arguments resolve through the host index's FTS search with the same
  exact-match conventions as `callers`/`callees`/`impact`, then map onto
  analysis NodeIds via `BridgeResult::id_map`.

### Honesty boundaries (no stubs that lie)

- **Value-level slicing/taint is NOT claimed.** `taint_v2::analyze` +
  `slicing::PointsToOracle` need `ir_map::build_ir_map`, which locates
  functions by **byte span** — and the bridge's spans are `0..0` (the SQLite
  schema stores line/column only). Rather than feed it garbage anchors, the
  CLI runs the engine's call-graph–level primitives and every `slice`/
  `taint` report carries a `note` + `granularity: "call-graph"` field saying
  exactly that (upstream, IR/CFG/dataflow are produced by the Rust source
  adapter only — see `ADAPTER_PARITY.md`).
- Placeholder nodes (`<unresolved>`) stay visible as such in slices/paths
  instead of masquerading as real definitions.
- `dominators` recomputes the dominator tree per reported node
  (`dominator_chain` is the only public NodeId-level API; `graph.inner()` is
  `pub(crate)`), so the reachable set is capped by `--top` (default 50).

### Tests / docs

- `src/analyze.rs` unit tests (6) — hand-built graphs pin slice direction
  semantics, dominator chains, cycle classification, taint hop annotation,
  cascade grouping, community determinism.
- `tests/analyze_cli_test.rs` (12) — end-to-end against the built binary
  (`CARGO_BIN_EXE_codegraph`): init → index a TS fixture with known ground
  truth (call chain `main → compute → helper`, mutual pair `ping ↔ pong`),
  then every subcommand asserted via `--json`, plus human-output smoke,
  unknown-symbol (exit 0 + message), uninitialized-project (exit 1), and
  invalid `--direction` (exit 1) contracts.
- README gained an "Analysis engine (`codegraph analyze`)" section covering
  the commands and the all-language vs 12-language vs bridge-unavailable
  fidelity split.

## Final gate — assimilation wave complete (2026-06-06)

Gate run after the rebrand + snapshot-cache + DSL + context-budget waves.
**Status: GREEN.**

- `cargo test --workspace --all-targets`: **2,203 passed, 0 failed,
  0 ignored** across 34 targets — `codegraph-analysis` 779 lib (+5 new
  resolver-fallback tests) + 6 `fingerprint_stability` + 4
  `node_id_stability`; `codegraph-rs` 447 lib + 967 integration (incl. the
  wave's `analyze_cache_test.rs` ×6 and `context_budget_test.rs` ×10).
  `cargo clippy --workspace --all-targets`: zero warnings. `cargo fmt
  --all --check`: clean.
- Release smoke (this repo's own index): `analyze complexity --top 3` cold
  then warm — `(cached graph)` notice on the second run, identical report;
  `analyze query 'fn("main") | callees | depth 3'` and `'scc'` (both
  documented examples) return ranked node sets; `context "how does the
  daemon handshake work" --strategy analysis --budget 2000` anchors
  `Daemon` + `HandshakeState` with on-topic source, honest
  `seeding: "call-graph"` note, 451 measured tokens; `--budget 100`
  truncates and says so.
- One fix landed during the gate (the context smoke initially returned an
  EMPTY context): `context::resolver::resolve_symbol` only accepted exact
  case-sensitive names, so natural-language tokens ("daemon", "handshake")
  anchored nothing. Added a **zero-exact-hit relaxed fallback** for
  unqualified symbols (case-insensitive exact tier, then case-insensitive
  name-prefix tier, deterministic `(name, file)` ordering) in
  `analysis/src/context/resolver.rs` + 5 pinning tests. Exact-hit behavior
  is unchanged (fallback never fires once any exact match exists; qualified
  symbols never relax). Also added "work"/"works"/"working" to both
  context stop-word lists (`analysis/src/context/mod.rs`,
  `src/context_analysis.rs`) — "work" was prefix-anchoring the watcher's
  `Worker*` types into a daemon question. No golden values were rebased;
  no deliberate-rebrand test updates were needed this wave.
- `rg -i jfc analysis/src src` → exactly one justified remnant: the
  one-time fingerprint-domain rebrand comment in
  `analysis/src/fingerprint.rs` (lines ~240–242), which intentionally
  records the old `"jfc-graph::CodeGraph::*"` prefix and points here.
- README "Analysis engine" section now documents the query DSL (with
  `--why`/`--explain`), the `.codegraph/analysis/` snapshot cache +
  `(cached graph)`/`--no-cache` contract, the native
  `CODEGRAPH_ANALYSIS_{DATA_DIR,CACHE_DIR,CAP_*}` env vars (explicitly: no
  `JFC_*` compatibility), and `codegraph context --strategy analysis
  --budget`.
- Nothing committed (per the gate brief). Still-open follow-ups: items 6–7
  above (vendored `research/` checkouts + `data_dir`/`worktree`/
  `framework_routes` overlap with the host crate), and the dormant
  `enrichment::LspDataProvider` trait from item 5.
