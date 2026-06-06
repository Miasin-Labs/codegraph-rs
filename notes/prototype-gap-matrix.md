# Prototype capability-gap matrix: jfc-graph (analysis/) vs codegraph-rs

Date: 2026-06-06. Companion to `notes/jfc-graph-migration.md` (full module
inventory) — this doc is the **authoritative gap matrix**: for every
capability the prototype has, what codegraph has today, and where the
capability should live in the "right way" production system.

Framing (Cole): *"jfc-graph was a cheap way of making codegraph; now I want to
make codegraph the right way."*

## Scope & ground truth (verified against sources, 2026-06-06)

- **Prototype** = `analysis/` (crate `codegraph-analysis`, 89 `src/` files).
- **Production** = root package `codegraph-rs` (`src/`): SQLite/FTS5 index,
  19 tree-sitter language extractor modules + 6 standalone extractors
  (svelte/vue/liquid/dfm/mybatis/ida_c), resolution pipeline with ~20
  framework resolvers + callback synthesizer, graph traversal, MCP server
  (8 tools: search/callers/callees/impact/node/explore/status/files), CLI.
- **Bridge** = `src/analysis_bridge.rs`: SQLite → analysis graph. Projects
  22 host node kinds onto the prototype's 5 (Function/Struct/Enum/Module/
  Trait); **drops** Field/Property/EnumMember/TypeAlias/Constant/Parameter/
  Import/Export/Route/Component nodes (Field/Property rows are folded into
  parent metadata — name-array by default, typed engine `FieldInfo` under
  `BridgeOptions::include_fields`, see the `partial.rs` row); carries
  Calls/Contains/Implements/UsesType/References edges + `signature` in
  metadata; spans carry line/column plus tree-sitter byte offsets (schema
  v5; pre-v5 rows degrade to `byte_range: 0..0`).
- **Exposed today** (`src/analyze.rs` + `src/analyze_ir.rs` +
  `src/bin/codegraph.rs`; updated 2026-06-06 after the close-list
  execution): `codegraph analyze {query, complexity, communities,
  dominators, slice, cycles, impact, taint, co-change, coverage, validate,
  traits, centrality, critical, export, types, generics, boundaries,
  capabilities, schema, stats, cfg, dataflow, diff}` plus
  `context --budget --strategy analysis [--fields]`. The close list below
  is executed except item 20 (see its entry; 16 closed 2026-06-06 via
  `analyze slice|taint --source` + the source-level `preconditions`
  enrichment; 18 closed 2026-06-06 via `context --fields` /
  `CODEGRAPH_ANALYSIS_FIELDS=1` — fields ride node *metadata*, not nodes).
- **LANDED (formerly in-flight)**: (a) the `JFC_*` →
  `CODEGRAPH_ANALYSIS_*` rebrand (env vars, cache dirs, fingerprint
  domains); (b) the analysis-graph snapshot cache under
  `.codegraph/analysis/` — now keeping one rotated `.prev` generation that
  feeds `analyze diff --base auto`; (c) `analyze query "<dsl>"`;
  (d) `context --budget --strategy analysis`.

### Honesty constraints that shape every placement decision

1. **Byte ranges landed (schema v5)** → nodes carry tree-sitter byte
   offsets, so IR-backed analyses run over the bridge via source re-parse:
   `analyze slice|taint --value-level` (points-to over per-function
   dataflow IR) and `analyze cfg|dataflow`. Pre-v5 indexes degrade to
   `granularity: "call-graph"` with an explicit re-index note — the
   reports state their own precision either way.
2. **ADAPTER_PARITY.md**: even upstream, only the **Rust adapter** populates
   per-function IR/CFG/dataflow/accessed_fields. The bridge populates none.
   The exposed `analyze complexity` sidesteps this by **re-parsing on-disk
   sources with the host grammars** and anchoring by line/col — the proven
   pattern for any source-level analysis over the bridge.
3. **DSL operator reachable ≠ data present.** `analyze query` is live;
   operators needing per-node data return rows only when that data is
   annotated first: `untested` needs `--lcov` (which runs the coverage
   annotation before the query), `cfg`/`dataflow` operators stay empty
   over the bridge (per-function IR is computed on demand by `analyze
   cfg|dataflow`, not persisted on nodes), and `since` has nothing to key
   on (bridged revisions are 0).

Gap status legend: **EXPOSED** (via analyze/context CLI today) ·
**UNEXPOSED** (engine has it, no codegraph surface) · **SUPERSEDED**
(codegraph's own version is better) · **N/A** (jfc-host-specific).
(The former IN-FLIGHT status is retired — all four in-flight items
landed.)

---

## The matrix (all 89 src modules)

### 1. Language adapters — 13 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Tree-sitter extraction, 12 languages (`adapter/mod.rs`, `adapter/{rust,typescript,python,go,cpp,c,java,csharp,php,ruby,kotlin,swift}.rs`) | `LanguageAdapter` trait + registry; parses files, emits 5-kind nodes + 7-kind edges; Rust adapter additionally lowers IR/CFG/dataflow/call-sites (others don't — see ADAPTER_PARITY.md) | Host extraction: 19 language extractor modules + 6 standalone extractors, 22 node kinds, 12 edge kinds, into SQLite; bridge re-projects for analysis | SUPERSEDED (×13) | Host extraction stays the one extraction pipeline. The adapters' **only** irreplaceable function is IR lowering (Rust-only today). Right way: host extraction stores **byte ranges** in SQLite, then either (a) port the IR lowering into a bridge post-pass that re-parses with host grammars (the `analyze complexity` pattern), or (b) keep the Rust adapter as the reference IR producer for tests. Do not grow the adapter fleet. |

### 2. Core graph model & storage — 8 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Stable typed graph (`graph.rs`, `nodes.rs`, `edges.rs`, `index.rs`) | petgraph `StableGraph` wrapper, content-addressed NodeIds, edge-invariant enforcement, secondary indices | The bridged in-memory graph every `analyze` command runs on; SQLite remains source of truth | EXPOSED (substrate, ×4) | Keep as the in-memory analysis substrate fed by the bridge. Never a second source of truth — SQLite owns persistence. |
| Read-optimized traversal substrate (`csr.rs`, `frontier.rs`) | CSR snapshot + hybrid sparse/dense frontier (Ligra-style) for fast BFS | Used by `traversal::traverse` → exercised by `analyze dominators`/`slice` | EXPOSED (substrate, ×2) | Internal; with the snapshot cache landed, consider persisting the CSR alongside it so big-repo analyze calls skip rebuild. |
| Typed per-kind metadata accessors (`kind_specific.rs`) | `KindData` projections over serialized `NodeData` | `analyze query` landed but renders host-side report shapes directly over the graph; this stays an engine-internal helper | UNEXPOSED (internal) | Internal rendering helper; nothing to expose. |
| Symbol handle table (`symbols.rs`) | `fn:module::name` handles → NodeId+Span, for semantic editing; cycle-detected resolution | Host FTS5 search + the exact-match symbol-resolution conventions already used by `callers`/`callees`/`impact`/`analyze` | SUPERSEDED | Host FTS5 is the symbol-resolution surface. The semantic-editing handle layer stays dormant unless codegraph ever grows edit tooling. |

### 3. Build / index pipeline — 7 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Workspace walk + graph build (`builder.rs`) | gitignore-aware walker, runs adapters, cross-file resolve, IR lowering | Host `indexAll`/orchestrator → SQLite; `analysis_bridge::build_analysis_graph` builds the analysis graph from the index | SUPERSEDED | Bridge is the build path. `GraphSession::from_directory` stays for the analysis crate's own tests only. |
| Call-site capture (`call_site.rs`) | Unresolved call-site capture fed to resolver post-pass | Host extraction captures call sites; resolution pipeline resolves them | SUPERSEDED | Host pipeline. |
| Cross-file reference resolution (`resolver.rs`) | Same-name matching with path-proximity scoring (effectively Rust-only upstream) | Host `src/resolution/`: import resolver + tsconfig/cargo path aliases + name matcher + ~20 framework resolvers + callback synthesizer | SUPERSEDED | Host resolution wins decisively; nothing to port. |
| LSP enrichment (`enrichment.rs`) | `LspDataProvider` trait to precisely resolve `UnresolvedCall` edges; implementor was jfc's LspClient | None — no implementor in this repo | N/A | jfc-host-specific. Leave the trait dormant; revisit only if a `codegraph-lsp` (DESIGN_FUTURE P15) materializes, in which case rust-analyzer/host LSP becomes the provider. |
| Content index / graph_grep backing (`content_index.rs`) | mtime-validated file-content cache for grep/snippets | Host SQLite FTS5 + `codegraph query` + MCP search/node/explore snippets | SUPERSEDED | Host FTS5. (`GraphSession::grep/outline` remain internal conveniences.) |
| Framework route detection (`framework_routes.rs`) | Regex/AST route detection for a handful of web frameworks; `Route` annotations + `FrameworkRoutesPass` | Host emits first-class `route` nodes from ~20 framework resolvers (Express, Laravel, Rails-family, Spring/Play, Gin, NestJS, React/Vue/Svelte routers, Drupal, Fabric, Expo, Swift/ObjC, Cargo…) | SUPERSEDED (verified) | Host framework resolvers are the one route system; bridge already drops `Route` nodes deliberately (5-kind projection). If analysis queries ever need routes, map them as `Module`-tagged metadata through the bridge rather than reviving this module. |
| Polyglot boundary detection (`polyglot.rs`) | Detects HTTP-route, FFI (`extern "C"`/JNI), WASM-export boundaries; `resolve_cross_language_calls` stitches caller→handler edges across language subgraphs; `PolyglotReport` | Partial overlap only: host has route nodes and a Swift/ObjC bridge resolver, but **no FFI/WASM boundary detection and no cross-language boundary report** | EXPOSED — `analyze boundaries` | Shipped: `codegraph analyze boundaries` → `polyglot::{detect_http_routes, detect_ffi_exports, detect_wasm_exports, resolve_cross_language_calls}` (honest note while the bridge doesn't populate the boundary metadata keys). Long term, fold the stitched cross-language edges into the host resolution pipeline as `provenance:'heuristic'` edges so `codegraph_explore` rides them (the dynamic-dispatch-coverage playbook pattern). |

### 4. Incrementality / persistence / caching — 12 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Graph fingerprinting (`fingerprint.rs`) | Order-independent BLAKE3 digest, golden-pinned | Landed: the rebranded fingerprint keys the snapshot cache under `.codegraph/analysis/` | EXPOSED (substrate) | Snapshot-cache key under `.codegraph/`. |
| Per-file memo + on-disk cache (`cache.rs`) | Content-fingerprint-keyed memoization, `JFC_GRAPH_CACHE_DIR` | Landed: snapshot cache under `.codegraph/analysis/` (with one rotated `.prev` generation), `--no-cache` bypass, `CODEGRAPH_ANALYSIS_CACHE_DIR` override | EXPOSED (substrate) | `.codegraph/` is the only cache home. |
| Data-dir resolution (`data_dir.rs`) | XDG-cache/workspace-hash dir resolution, `JFC_GRAPH_DATA_DIR` | Landed (`CODEGRAPH_ANALYSIS_DATA_DIR`); host owns `.codegraph/` resolution | EXPOSED (substrate) | Host `.codegraph/` discovery is canonical; analysis-side resolution only feeds the snapshot cache. |
| Capability toggles (`capabilities.rs`) | Feature tree with dependency cascading, `JFC_GRAPH_CAP_*` env kill-switches | `CODEGRAPH_ANALYSIS_CAP_*` env kill-switches + `codegraph analyze capabilities` printing the resolved tree, env names, and dependency cascades | EXPOSED — `analyze capabilities` | Shipped per the close list. |
| Incremental query cache (`incremental.rs`) | Adapton-style memo with read-set tracking | None — every CLI run loads the snapshot cache (or re-bridges on a miss); no cross-invocation query memo | UNEXPOSED (close list 20 — open) | Only pays off in a long-lived process: host the bridged `GraphSession` inside the MCP daemon (`src/mcp/daemon.rs`) and invalidate per file-sync event. Not a CLI surface. |
| Reactive recompute (`reactive.rs`) | Salsa-lite input→output dependency tracking | None | UNEXPOSED | Same placement as `incremental.rs` — daemon-resident session invalidation. Pick ONE of incremental/reactive when that lands; don't ship both. |
| Event-sourced persistence (`persistence.rs`) | Base snapshot + ordered `GraphEvent`s, undo via replay, versioned on-disk schema | Host SQLite is state-based; the landed snapshot cache covers fast reload | UNEXPOSED | Recommend **leave internal / do not expose**: SQLite + the snapshot cache make event-sourcing redundant for codegraph's read-mostly use. Revisit only if undo/edit tooling appears. |
| Query history (`history.rs`) | Recent-query ring buffer for inspection | None | UNEXPOSED | Low priority; only meaningful in a daemon/TUI. If the daemon hosts a session, surface as a debug log, not a tool. |
| Base+branch-diff overlay (`overlay.rs`) | Shared base snapshot + per-branch diff (postcard/bincode), `diff_against_base`/`apply_diff_to_graph` | `codegraph analyze diff [--base <snapshot\|auto>]`: working-tree vs base over the snapshot cache — the cache keeps one rotated `.prev` generation as the auto base; explicit `--base` loads any snapshot file/cache dir (engine entry: `overlay::{save,load}_snapshot_bincode`) | EXPOSED — `analyze diff` | Shipped (close list 21). The team/monorepo shared-base distribution story (CI-published base snapshots) remains future work on top. |
| Partial struct views (`partial.rs`) | Field-level granularity for context windows (`get_partial_struct`, accessed-field markers) | `codegraph context "<task>" --strategy analysis --fields` (or `CODEGRAPH_ANALYSIS_FIELDS=1`, which also covers every `analyze` command): the bridge registers each Field/Property row contained in a Struct as typed `FieldInfo` **metadata** via `partial::set_struct_fields` (no Field nodes — node count unchanged, which beats the originally-planned node carrying), and function→field `references`/`type_of` edges as `partial::set_accessed_fields`; `context --strategy analysis` then renders a "Partial struct views" section (only flow-touched fields, ✓-marked, with visibility + accessor names) when a selected function touches a strict subset of a selected struct's fields. Snapshot cache is keyed by the flag (no cross-state leaks). Honest notes otherwise: fieldless structs and the `CODEGRAPH_ANALYSIS_CAP_PARTIAL_STRUCT=0` kill-switch each name the gate. Data reality: C#/Java/Scala extractors emit field/property rows; **no host extractor emits function→field access edges yet** (validated on Java member access), so real-world views need that extraction follow-up — registered-data rendering is pinned by `tests/analyze_fields_test.rs` | EXPOSED — `context --strategy analysis --fields` | Done (close list 18) via metadata, not bridged Field nodes. Extraction-side follow-up: emit function→field `references` edges so views light up without planted data. |
| Worktree mismatch detection (`worktree.rs`) | Refuses to silently borrow a parent checkout's cached index | Host `src/sync/worktree.rs`: `detect_worktree_index_mismatch` + user-facing warnings, already wired into CLI (verified) | SUPERSEDED | Host version is canonical; analysis copy only needs to guard the snapshot cache — reuse the host check, delete the duplicate when convenient. |
| Pass framework (`pass.rs`) | Pre/postcondition `GraphFlag`s + `PassManager` ordering | `PassManager` runs `PossibleTypesPass` inside `analyze types`; `CoveragePass` semantics ride `--lcov` annotation | EXPOSED (substrate) | Internal orchestration only; no user surface. |

### 5. Traversal & graph algorithms — 9 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| BFS/DFS traversal (`traversal.rs`, `bfs_directed.rs`) | Depth-bounded, cycle-detected traversal; direction-optimised push/pull BFS over CSR | Drives `analyze dominators`/`slice`; host also has its own `src/graph/traversal.rs` for impact/affected | EXPOSED (×2) | As-is. |
| Transitive closure (`closure.rs`) | Bounded depth-N reachability fixpoints | Host `codegraph impact`/`affected` (BFS radius) + `analyze slice --depth` cover the user-facing need | SUPERSEDED | Use host impact/affected; keep closure internal for future Datalog work. |
| Approximate reachability (`hll.rs`) | HyperLogLog per-node reachability estimates for >100k-node graphs | `codegraph analyze stats --estimate-reachability`: exact BFS at/below 5k nodes, HLL above | EXPOSED — `analyze stats --estimate-reachability` | Shipped per the close list (threshold-gated). |
| Label-constrained reachability (`label_reachability.rs`) | RLC: reachable via edge-label-sequence patterns (`Calls+ Implements?`) | Wired into the DSL: `analyze query '... | reachable via "Calls+" [incoming]'` (`label_reachability::{parse_pattern, reachable_targets}`) | EXPOSED — `analyze query 'reachable via ...'` | Shipped as the DSL `reachable via` operator. |
| Dominators (`dominators.rs`) | Generic dominator trees | `analyze dominators` | EXPOSED | As-is (note: recomputes per node because `graph.inner()` is pub(crate) — expose a whole-tree API in the analysis crate to lift the `--top` cap). |
| Whole-graph algorithm suite (`analysis.rs`) | SCC, dominator chains, taint paths, k-shortest paths, **centrality (PageRank)**, **critical/articulation nodes**, **bridge edges**, components, toposort, transitive reduction, **parallel edit groups/coloring**, **maximal cliques**, all-pairs distances, **Dot export** | EXPOSED: `cycles` (SCC + break suggestions), `dominators` (chains), `taint` (paths), `analyze centrality` (PageRank), `analyze critical` (articulation nodes + bridge edges), `analyze export --format dot` (whole graph or `--symbol` neighborhood). Cliques, parallel edit groups, all-pairs distances, toposort remain unsurfaced | EXPOSED (close list 5/6 shipped) | Remaining algorithms (cliques/parallel groups/distances/toposort) stay unsurfaced until a concrete need shows. |
| Community detection (`communities.rs`) | Louvain modularity | `analyze communities` | EXPOSED | As-is. |
| Stratified negation (`strata.rs`) | Rule stratification for negation-safe rule queries | None | UNEXPOSED | Leave internal — its only consumer is the not-yet-built Datalog engine (DESIGN_FUTURE P13). No surface until that exists. |

### 6. Intraprocedural analysis (IR / CFG / dataflow / complexity) — 9 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Language-agnostic IR (`ir.rs`, `ir_map.rs`) | Adapters lower ASTs once; `build_ir_map` builds the interprocedural `DataflowOracle` for value-level slicing/taint | Byte offsets landed (schema v5); `src/analyze_ir.rs` lowers IR via host-grammar re-parse (`lower_for_language`: Rust/TS/JS/Python/Go) and roots the oracle for `slice|taint --value-level` | EXPOSED — `analyze slice\|taint --value-level` (×2) | Shipped as the `--value-level` upgrade path; reports carry `irCoverage` so partial lowering is visible. |
| Control-flow graphs (`cfg.rs`, `cfg_rules.rs`) | Per-function basic blocks + typed edges; rules for rust/ts/js/python/go/java/c/cpp/php (+ more) — but only the Rust *adapter* populates `node.cfg` upstream (ADAPTER_PARITY) | `codegraph analyze cfg <symbol>`: re-parse anchor pattern → `cfg::build_cfg`; rule-covered languages only, honest capability note otherwise. DSL `cfg` operator still returns empty over the bridge (IR not persisted on nodes) | EXPOSED — `analyze cfg` (×2) | Shipped per the close list (Tier 2, item 14). |
| Per-function dataflow (`dataflow.rs`, `dataflow_rules.rs`) | Param flows, returns, assignments, arg flows, mutation detection (Rust-extraction-only upstream) | `codegraph analyze dataflow <symbol>`: re-parse anchor pattern → `dataflow::extract_dataflow`, gated by `dataflow_rules` coverage. DSL `dataflow` operator still empty over the bridge | EXPOSED — `analyze dataflow` (×2) | Shipped per the close list (Tier 2, item 15). |
| Complexity metrics (`complexity.rs`, `complexity_rules.rs`) | Cyclomatic/cognitive/nesting/Halstead/LOC/MI, 12+ language rule tables | `analyze complexity [--top N]` (re-parse pattern, skipped-language breakdown) | EXPOSED (×2) | As-is — this is the reference implementation of the re-parse pattern. |
| Precondition predicates (`predicates.rs`) | Backward control-flow predicate extraction (`extract_predicates` needs source + byte_pos) powering the DSL `preconditions` renderer | `analyze query '... | preconditions'` now renders both flavors: the call-graph caller walk **and** the source-level guards — `query_report_with_sources` detects the operator, and for every Calls edge between result nodes re-reads the call site under the project root and runs `extract_predicates` (call-site line/col → byte position; anchoring honesty-gated on v5 node byte offsets). Guards render outermost-first in human + `--json` output (`preconditions` section on the query report); pre-v5 rows, non-Rust call sites, unreadable files, and stale positions each get a counted note instead of fabricated or silently-missing guards | EXPOSED — `analyze query '... \| preconditions'` (source-level) | Done for Rust (the engine extractor is tree-sitter-Rust-specific); per-language predicate analogues remain engine-side future work. |

### 7. Interprocedural analysis — 11 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Points-to / alias analysis (`points_to.rs`) | Andersen-style field-sensitive, flow-insensitive points-to over IR; `PointsToTable::{pts_of, may_alias}` | Backs `slice|taint --value-level`: `slicing::PointsToOracle` runs `points_to::analyze_interprocedural` over the rooted IR map | EXPOSED — backs `analyze slice\|taint --value-level` | A direct `analyze alias <fn> <var-a> <var-b>` surface remains optional future work. |
| Possible-types propagation (`possible_types.rs`) | Concrete subtype sets flowing into/out of each function (`propagate_possible_types`); DSL `possible_types` operator exists | `codegraph analyze types <symbol>` runs `PossibleTypesPass` through `PassManager`, then reports concrete input/return type sets | EXPOSED — `analyze types` | Shipped per the close list (item 7). |
| Monomorphization detection (`monomorphize.rs`) | Generic defs + callsites supplying concrete type args (`find_instantiations`, `annotate`) — signature/metadata-driven | `codegraph analyze generics [symbol]` → `monomorphize::find_instantiations` + a signature heuristic for likely-generic definitions (honest note: the bridge doesn't populate the generics metadata contract yet) | EXPOSED — `analyze generics` | Shipped per the close list (item 8). |
| Program slicing (`slicing.rs`) | Forward/backward slices over a `DataflowOracle` | `analyze slice`: call-graph granularity by default (`CallGraphOracle`), value-level via `--value-level` (IR oracle, v5 indexes) — the report's `granularity` field states which ran | EXPOSED (call-graph + `--value-level`) | Done — both oracles shipped. |
| Value-level taint (`taint_v2.rs`) | Interprocedural source→sink with sanitizer awareness over IR | `analyze taint --value-level` runs `taint_v2::analyze` over the rooted IR map; default stays graph-path `analysis::taint_paths` with the honest call-graph note | EXPOSED — `analyze taint --value-level` | Shipped behind the same report shape, as planned. |
| Name-based taint inference (`taint_naming.rs`) | Bimodal source/sink classification from identifier names (`classify_name`, `flow_priority`) | `codegraph analyze taint --suggest` ranks candidate source/sink pairs by `classify_name` + `flow_priority` when no symbols are given | EXPOSED — `analyze taint --suggest` | Shipped per the close list (item 9). |
| Trait/type hierarchy analyses (`traits_hierarchy.rs`) | Implementation hierarchies, trait-dispatch call edges, type-based clustering | `codegraph analyze traits [type]` → `trait_hierarchies()/trait_dispatch_calls()/cluster_by_primary_type()`; DSL operators also reachable via `analyze query` | EXPOSED — `analyze traits` | Shipped (item 4). Still open: feed dispatch edges to the host resolution pipeline as heuristic edges (dynamic-dispatch coverage for `codegraph_explore`). |
| Co-change / temporal coupling (`co_change.rs`) | Mines `git log --name-only` (`fetch_git_history`), maps commits to nodes, confidence-scored pairs (`compute_co_changes`, `co_changes_for_nodes`); `GraphSession::co_changes`; DSL `co_changes` op shells git live | `codegraph analyze co-change [symbol] [--min-support N] [--max-commits N]`: repo-wide pairs via host-side git-log mining + `compute_co_changes`, per-symbol via `co_changes_for_nodes` | EXPOSED — `analyze co-change` | Shipped (item 1); the engine's own `fetch_git_history` parser bug is documented in `notes/close-tier1-needs.md`. |
| Coverage mapping (`coverage.rs`) | LCOV parsing (`parse_lcov`), annotates function nodes with line/branch coverage (`annotate_graph_from_lcov`, `CoveragePass`); DSL `untested` filter | `codegraph analyze coverage --lcov <path> [--untested]` parses + annotates the bridged graph; `analyze query --lcov` annotates before DSL eval so `untested` returns real rows | EXPOSED — `analyze coverage` | Shipped (item 2). |
| Cascade task generation (`cascade.rs`) | `CascadeTask`s grouped by file when a signature change ripples | `analyze impact [--signature]` | EXPOSED | As-is. |
| Virtual edit validation (`validation.rs`) | `VirtualValidator::validate_signature_change(target, arity_before, arity_after)` — pre-commit simulation, per-call-site breakage verdicts; `preview_affected_call_sites` | `codegraph analyze validate <symbol> --params-before N --params-after M`: pass/fail + per-caller verdicts + affected-call-site preview (distinct from `analyze impact`'s cascade tasks) | EXPOSED — `analyze validate` | Shipped (item 3). |

### 8. Query DSL — 5 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Pipe DSL engine (`dsl/mod.rs`, `dsl/aggregate.rs`, `dsl/plan.rs`, `dsl/provenance.rs`, `dsl/stream.rs`) | ~30 operators (callers/callees/depth/filter/show, set algebra, path patterns w/ `via`, taint, preconditions, entrypoints, scc, dominators, trait_impls, dispatch, cluster, affected, multi_path, untested, possible_types, co_changes, communities, complexity, cfg, dataflow, hot, since), aggregation/let/quantifiers, plan optimiser, why-provenance, streaming eval | `codegraph analyze query "<dsl>"` (with `--explain` plan output, `--why` provenance, `--max-nodes`, `--lcov` pre-annotation) over `run_query_expr` | EXPOSED — `analyze query` (×5) | Shipped. Per-operator data requirements are constraint #3 above: `untested` needs `--lcov`; `cfg`/`dataflow`/`since` stay empty over the bridge. |

### 9. Agent-facing context engine — 10 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Context builder + supporting machinery (`context/mod.rs`, `budget.rs`, `clustering.rs`, `expansion.rs`, `render.rs`, `resolver.rs`, `dataflow_seed.rs`, `heuristics.rs`, `retrieval_gate.rs`) | `build_context(task, opts)`: intent classification (heuristics), Repoformer-style retrieve/abstain gate, DRACO dataflow seeding, subgraph expansion, file-count-scaled budget, clustering + source merging, markdown render, multi-separator symbol resolution | `codegraph context "<task>" --budget <tokens> --strategy analysis` drives `build_context` (all nine modules) over the bridged graph | EXPOSED — `context --strategy analysis` (×9) | Shipped. Per the TS-side lesson ("adapt the tool to the agent"), do NOT turn the retrieval gate into a new MCP tool — if it ever crosses to MCP, it shapes existing tool *responses* server-side. |
| Retrieval measurement harness (`context/measure.rs`) | Quantified before/after gate-savings measurement | Host has `scripts/agent-eval`-style methodology (TS repo); nothing in Rust | UNEXPOSED | Leave internal — it's an eval harness, not a user capability. Use it from benches/tests when tuning the gate; never a CLI/MCP surface. |

### 10. Output / interop / facade — 4 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| CPG report façade (`analysis_tools.rs`) | `program_slice`/`data_dependencies`/`taint_flow` as compact source-annotated, path-capped text reports | `analyze slice --source` (annotated slice + the data-dependency companion) and `analyze taint <src> <snk> --source` (annotated flows, capped with the engine's "raise max_paths" trailer) render these functions verbatim; `--json` wraps them in the standard envelope (`sliceSource`/`taintSource`) with a byte-offset coverage block. Line-span annotation works on any index; value-level fidelity rides v5 byte offsets — the host note states which ran, the pre-v5 re-index fix, and warns when the cwd is not the project root (the façade resolves the graph's relative paths against the cwd). Mutually exclusive with `--value-level` (same oracle, different renderer) | EXPOSED — `analyze slice\|taint --source` | Done (close list 16). |
| Token-budgeted formatting (`formatting.rs`) | `format_query_result` under a token budget; `FormattedOutput` | The landed `analyze query` renders its own host-side report shapes (`QueryReport`) directly over the graph; `format_query_result` is not on the path | UNEXPOSED (engine-internal) | Stays internal unless the host ever adopts the engine's token-budgeted renderer. |
| Stable JSON schemas + envelopes (`schema.rs`) | JSON Schemas for QueryResult/EntrypointSummary/ContextResult/FormattedOutput; `Envelope` with payload-kind tagging; `json_schema_for` | `codegraph analyze schema <kind>` prints `json_schema_for(kind)`; every `analyze ... --json` is wrapped in the host `ReportEnvelope` (mirrors the engine's `Envelope` wire shape — the engine type's closed `PayloadKind` can't carry host report kinds, see `notes/close-tier1-needs.md`) | EXPOSED — `analyze schema` + the `--json` envelope | Shipped (item 12). |
| High-level facade (`session.rs`) | `GraphSession`: query memoization, edit invalidation, co_changes, context, search/callers/callees/impact/node/explore/grep/outline, overlay open/save | Library facade only: `BridgeResult::into_session` exists, but the landed `analyze query`/`context --strategy analysis` drive the bridged graph directly. Its `explore/search/node/grep/outline` duplicate host MCP tools | UNEXPOSED (library facade) | Keep as a library convenience. Its agent-shaped methods (explore/search/node) are SUPERSEDED by host MCP tools — never expose them as a second agent surface. |

---

## CLOSE LIST — prioritized UNEXPOSED items worth exposing

Status update 2026-06-06 (rev 3: items 16 and 18 closed): every item below is **CLOSED** except
20 (marked OPEN with rationale).

Tier 1 — works today over the bridge, zero extraction changes:

1. **CLOSED — `codegraph analyze co-change [symbol] [--min-support N] [--max-commits N]`**: repo-wide pairs via host-side git-log mining + `compute_co_changes`, per-symbol via `co_changes_for_nodes`.
2. **CLOSED — `codegraph analyze coverage --lcov <path> [--untested]`**: `coverage::parse_lcov` + `annotate_graph_from_lcov`; `analyze query --lcov` annotates pre-DSL so `untested` returns real rows.
3. **CLOSED — `codegraph analyze validate <symbol> --params-before N --params-after M`**: `validation::VirtualValidator::{validate_signature_change, preview_affected_call_sites}`.
4. **CLOSED — `codegraph analyze traits [type]`**: `traits_hierarchy::{trait_hierarchies, trait_dispatch_calls, cluster_by_primary_type}`.
5. **CLOSED — `codegraph analyze centrality [--top N]` / `analyze critical`**: `analysis::{centrality, critical_nodes, bridge_edges}`.
6. **CLOSED — `codegraph analyze export --format dot [--symbol X --depth N]`**: `analysis::{to_dot, to_dot_subgraph}`.
7. **CLOSED — `codegraph analyze types <symbol>`**: `possible_types::PossibleTypesPass` run through `pass::PassManager`.
8. **CLOSED — `codegraph analyze generics [symbol]`**: `monomorphize::find_instantiations` + signature heuristic.
9. **CLOSED — `codegraph analyze taint --suggest`**: `taint_naming::{classify_name, flow_priority}` when source/sink args omitted.
10. **CLOSED — `codegraph analyze boundaries`**: `polyglot::{detect_http_routes, detect_ffi_exports, detect_wasm_exports, resolve_cross_language_calls}`. Still open longer-term: feed stitched cross-language edges into host resolution as heuristic edges.
11. **CLOSED — `codegraph analyze capabilities`**: `capabilities::CapabilityTree::from_env()` + the `CODEGRAPH_ANALYSIS_CAP_*` env names and dependency cascades.
12. **CLOSED — `codegraph analyze schema <kind>`** + every `analyze … --json` wrapped in the versioned `ReportEnvelope` (host mirror of `schema::Envelope`).
13. **CLOSED — `codegraph analyze stats [--estimate-reachability]`**: `hll::approximate_reachability`, threshold-gated (exact BFS ≤ 5k nodes).

Tier 2 — needs the complexity-style source re-parse anchor (host grammars, line/col):

14. **CLOSED — `codegraph analyze cfg <symbol>`**: `cfg::build_cfg` + `cfg_rules::CfgRules::for_language`, honesty-gated with skip notes.
15. **CLOSED — `codegraph analyze dataflow <symbol>`**: `dataflow::extract_dataflow` + `dataflow_rules`, same gating.
16. **CLOSED — source-annotated slice/taint reports**: `analyze slice|taint --source` → `analysis_tools::{program_slice, data_dependencies, taint_flow}` rendered verbatim (`--json` kinds `sliceSource`/`taintSource` under the standard envelope, with byte-offset coverage + honesty notes; mutually exclusive with `--value-level`). The same change wired the source-level `predicates.rs` row: `analyze query '… | preconditions'` now shows the actual guarding conditions (Rust; pre-v5 indexes get the re-index note). Tests: `tests/analyze_source_reports_test.rs`.

Tier 3 — blocked on schema/architecture work (do these to unblock, don't expose half-bridged):

17. **CLOSED — byte ranges in the SQLite schema (v5)**: unlocked `analyze slice|taint --value-level` (`src/analyze_ir.rs`: host-grammar IR lowering → `slicing::PointsToOracle` over `points_to`, `taint_v2::analyze`); reports' `granularity` flips honestly, pre-v5 indexes degrade with a re-index note.
18. **CLOSED — field metadata through the bridge (flag-gated)** → `codegraph context "<task>" --strategy analysis --fields` (CLI flag, ORs with `CODEGRAPH_ANALYSIS_FIELDS=1`). Implemented as node **metadata**, not bridged Field nodes: `BridgeOptions::include_fields` registers Struct-contained Field/Property rows via `partial::set_struct_fields` (typed name/type/visibility, signature-derived) and function→field access edges via `partial::set_accessed_fields`, then `context --strategy analysis` renders flow-touched partial views via `partial::try_get_partial_struct` (every `Err` variant becomes a one-line honest note, per the engine guidance). Zero node-count delta, snapshot cache keyed by the flag state, invalid names counted in `BridgeStats::fields_skipped_invalid`. Tests: `tests/analyze_fields_test.rs` (bridge contract, renderer, cache isolation, CLI end-to-end). Remaining extraction-side gap (tracked in the §4 row): no host extractor emits function→field access edges yet.
19. **CLOSED — label-constrained reachability in the DSL**: the `reachable via "<pattern>" [incoming]` operator (`label_reachability::{parse_pattern, reachable_targets}`) ships in `analyze query`.
20. **OPEN — daemon-resident GraphSession**: hold the bridged session in the MCP daemon, invalidate on sync events via `reactive::ReactiveDb` *or* `incremental` (pick one). Future: a perf optimization, not a missing capability — every analysis already runs correctly via the snapshot cache; a resident session would only shave the per-invocation snapshot load.
21. **CLOSED — `codegraph analyze diff [--base <snapshot|auto>]`** (working-tree vs base over the snapshot cache): nodes/edges added/removed/changed, complexity deltas for changed functions (the diff run writes a `complexity.json` sidecar so the next diff has before-metrics), newly-introduced cycles, and the impact set of the delta. The cache keeps one rotated `.prev` generation (`auto` = the last cached snapshot before the current fingerprint); explicit `--base` loads a snapshot file or cache dir via `overlay::load_snapshot_bincode`. No base → honest note, exit 0.

## SUPERSEDED / N-A list

SUPERSEDED (codegraph's version is better — one-line justification each):

- **12 language adapters + registry** (`adapter/*`): host extraction covers 19+ languages with 22 node kinds and 6 standalone format extractors; bridge re-projects. Adapters' only residual value is Rust IR lowering until byte ranges land.
- **`builder.rs`**: host orchestrator + SQLite index + `analysis_bridge::build_analysis_graph` is the build path.
- **`call_site.rs`**: host extraction already captures call sites into the edge pipeline.
- **`resolver.rs`**: host `src/resolution/` (import resolution, path aliases, name matcher, ~20 framework resolvers, callback synthesizer) vastly outclasses path-proximity name matching.
- **`content_index.rs`**: SQLite FTS5 + `codegraph query`/MCP search already serve grep/snippet needs with persistence.
- **`framework_routes.rs`**: host framework resolvers emit first-class `route` nodes across ~20 frameworks vs the prototype's handful (verified `src/resolution/frameworks/` registry).
- **`worktree.rs`**: host `sync::worktree::detect_worktree_index_mismatch` implements the same borrowed-index guard, already wired to CLI warnings (verified).
- **`symbols.rs`**: host FTS5 + the established symbol-resolution conventions are the lookup surface; handle-based editing is dormant.
- **`closure.rs`**: host `impact`/`affected` BFS radius + `analyze slice --depth` cover bounded reachability for users.
- *(within rows above)* `GraphSession::{explore, search, node, grep, outline}`: host MCP tools are the validated agent surface — never duplicate.

N/A (jfc-host-specific):

- **`enrichment.rs`**: `LspDataProvider`'s only implementor was jfc's LspClient; dormant trait with no consumer here — revisit only alongside a future `codegraph-lsp` (DESIGN_FUTURE P15).

## Counts per gap status (89 modules)

| status | count | modules |
|---|---|---|
| EXPOSED | 58 | lib, graph, nodes, edges, index, csr, frontier, traversal, bfs_directed, dominators, analysis, communities, complexity, complexity_rules, slicing, cascade, polyglot, overlay, pass, hll, label_reachability, ir, ir_map, cfg, cfg_rules, dataflow, dataflow_rules, points_to, possible_types, monomorphize, taint_v2, taint_naming, traits_hierarchy, co_change, coverage, validation, schema, fingerprint, cache, data_dir, capabilities, predicates, analysis_tools, partial, dsl/{mod, aggregate, plan, provenance, stream}, context/{mod, budget, clustering, dataflow_seed, expansion, heuristics, render, resolver, retrieval_gate} |
| UNEXPOSED | 9 | incremental, reactive, persistence, history, strata, context/measure, kind_specific, formatting, session |
| SUPERSEDED | 21 | adapter/{mod + 12 languages}, builder, call_site, resolver, content_index, framework_routes, worktree, symbols, closure |
| N/A | 1 | enrichment |
