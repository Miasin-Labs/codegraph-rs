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
  Import/Export/Route/Component nodes; carries Calls/Contains/Implements/
  UsesType/References edges + `signature` in metadata; spans are
  line/column with `byte_range: 0..0` (SQLite stores no byte offsets).
- **Exposed today** (`src/analyze.rs` + `src/bin/codegraph.rs`):
  `codegraph analyze {complexity, communities, dominators, slice, cycles,
  impact, taint}` — verified; these are the *only* analysis-crate entry
  points the host calls (plus `GraphSession::from_snapshot` in the bridge).
- **IN-FLIGHT by other agents** (do not re-propose): (a) rebrand of `JFC_*`
  env vars / cache dirs / tracing targets / fingerprint domain strings
  (visible in working tree: `CODEGRAPH_ANALYSIS_CAP_*`,
  `codegraph-analysis::CodeGraph::*` with a deliberate one-time fingerprint
  break); (b) analysis-graph snapshot cache under `.codegraph/`;
  (c) `analyze query "<dsl>"`; (d) `context --budget --strategy analysis`.

### Honesty constraints that shape every placement decision

1. **No byte ranges in SQLite** → `ir_map::build_ir_map` cannot anchor
   functions → everything IR-backed (points-to, possible-types-via-IR,
   taint_v2, value-level slicing) is blocked until the host schema stores
   byte offsets. The exposed `slice`/`taint` honestly report
   `granularity: "call-graph"`.
2. **ADAPTER_PARITY.md**: even upstream, only the **Rust adapter** populates
   per-function IR/CFG/dataflow/accessed_fields. The bridge populates none.
   The exposed `analyze complexity` sidesteps this by **re-parsing on-disk
   sources with the host grammars** and anchoring by line/col — the proven
   pattern for any source-level analysis over the bridge.
3. **DSL operator reachable ≠ data present.** Once `analyze query` lands,
   operators like `cfg`, `dataflow`, `untested`, `since`, `hot` parse fine
   but return empty over the bridged graph (no per-node cfg/dataflow/
   coverage/revision data). Surfacing them honestly requires the ingestion
   work listed in the close list.

Gap status legend: **EXPOSED** (via analyze/context CLI today) ·
**IN-FLIGHT** (one of the 4 items above) · **UNEXPOSED** (engine has it, no
codegraph surface) · **SUPERSEDED** (codegraph's own version is better) ·
**N/A** (jfc-host-specific).

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
| Read-optimized traversal substrate (`csr.rs`, `frontier.rs`) | CSR snapshot + hybrid sparse/dense frontier (Ligra-style) for fast BFS | Used by `traversal::traverse` → exercised by `analyze dominators`/`slice` | EXPOSED (substrate, ×2) | Internal; once the snapshot cache (in-flight) lands, consider persisting the CSR alongside it so big-repo analyze calls skip rebuild. |
| Typed per-kind metadata accessors (`kind_specific.rs`) | `KindData` projections over serialized `NodeData` | Used by GraphSession/formatting rendering — rides the in-flight `analyze query` output path | IN-FLIGHT (substrate) | Internal rendering helper; nothing to expose. |
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
| Polyglot boundary detection (`polyglot.rs`) | Detects HTTP-route, FFI (`extern "C"`/JNI), WASM-export boundaries; `resolve_cross_language_calls` stitches caller→handler edges across language subgraphs; `PolyglotReport` | Partial overlap only: host has route nodes and a Swift/ObjC bridge resolver, but **no FFI/WASM boundary detection and no cross-language boundary report** | UNEXPOSED | `codegraph analyze boundaries` → `polyglot::{detect_http_routes, detect_ffi_exports, detect_wasm_exports, resolve_cross_language_calls}` over the bridged graph (works today — name/signature-driven). Long term, fold the stitched cross-language edges into the host resolution pipeline as `provenance:'heuristic'` edges so `codegraph_explore` rides them (the dynamic-dispatch-coverage playbook pattern). |

### 4. Incrementality / persistence / caching — 12 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Graph fingerprinting (`fingerprint.rs`) | Order-independent BLAKE3 digest, golden-pinned | Rebrand in working tree (`codegraph-analysis::CodeGraph::*` domains, deliberate one-time fingerprint break) keys the snapshot cache | IN-FLIGHT | Snapshot-cache key under `.codegraph/`. |
| Per-file memo + on-disk cache (`cache.rs`) | Content-fingerprint-keyed memoization, `JFC_GRAPH_CACHE_DIR` | Being rebranded; snapshot cache under `.codegraph/` replaces the ad-hoc cache dir | IN-FLIGHT | `.codegraph/` is the only cache home. |
| Data-dir resolution (`data_dir.rs`) | XDG-cache/workspace-hash dir resolution, `JFC_GRAPH_DATA_DIR` | Being rebranded; host already owns `.codegraph/` resolution | IN-FLIGHT | Host `.codegraph/` discovery is canonical; analysis-side resolution only feeds the snapshot cache. |
| Capability toggles (`capabilities.rs`) | Feature tree with dependency cascading, `JFC_GRAPH_CAP_*` env kill-switches | Env rebrand to `CODEGRAPH_ANALYSIS_CAP_*` confirmed in working tree; **no introspection surface** | IN-FLIGHT (rebrand); introspection UNEXPOSED | Close list: `analyze capabilities` printing the resolved tree + env names (`CapabilityTree::default_tree()` / `from_env()`). |
| Incremental query cache (`incremental.rs`) | Adapton-style memo with read-set tracking | None — every CLI run re-bridges from SQLite | UNEXPOSED | Only pays off in a long-lived process: host the bridged `GraphSession` inside the MCP daemon (`src/mcp/daemon.rs`) and invalidate per file-sync event. Not a CLI surface. |
| Reactive recompute (`reactive.rs`) | Salsa-lite input→output dependency tracking | None | UNEXPOSED | Same placement as `incremental.rs` — daemon-resident session invalidation. Pick ONE of incremental/reactive when that lands; don't ship both. |
| Event-sourced persistence (`persistence.rs`) | Base snapshot + ordered `GraphEvent`s, undo via replay, versioned on-disk schema | Host SQLite is state-based; snapshot cache (in-flight) covers fast reload | UNEXPOSED | Recommend **leave internal / do not expose**: SQLite + the snapshot cache make event-sourcing redundant for codegraph's read-mostly use. Revisit only if undo/edit tooling appears. |
| Query history (`history.rs`) | Recent-query ring buffer for inspection | None | UNEXPOSED | Low priority; only meaningful in a daemon/TUI. If the daemon hosts a session, surface as a debug log, not a tool. |
| Base+branch-diff overlay (`overlay.rs`) | Shared base snapshot + per-branch diff (postcard/bincode), `diff_against_base`/`apply_diff_to_graph` | None; in-flight snapshot cache is single-snapshot only | UNEXPOSED | Team/monorepo feature layered on the snapshot cache: `codegraph snapshot save-base` / auto base+diff under `.codegraph/`. Post-cache follow-up; entry `overlay::{save_base_snapshot, load_base_snapshot, diff_against_base, apply_diff_to_graph}`. |
| Partial struct views (`partial.rs`) | Field-level granularity for context windows (`get_partial_struct`, accessed-field markers) | None — and **blocked**: bridge drops Field/Property nodes (host SQLite *has* them) | UNEXPOSED | Extend `map_node_kind` to optionally carry Field nodes (flag-gated to avoid node explosion), then surface in `context --strategy analysis` output for big structs/classes. Entry: `partial::get_partial_struct`. |
| Worktree mismatch detection (`worktree.rs`) | Refuses to silently borrow a parent checkout's cached index | Host `src/sync/worktree.rs`: `detect_worktree_index_mismatch` + user-facing warnings, already wired into CLI (verified) | SUPERSEDED | Host version is canonical; analysis copy only needs to guard the snapshot cache — reuse the host check, delete the duplicate when convenient. |
| Pass framework (`pass.rs`) | Pre/postcondition `GraphFlag`s + `PassManager` ordering | None (bridge runs no passes) | UNEXPOSED | Internal orchestration only: when coverage/possible-types/routes enrichment passes get exposed, run them through `PassManager` inside `analysis_bridge`/`analyze.rs`. No user surface. |

### 5. Traversal & graph algorithms — 9 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| BFS/DFS traversal (`traversal.rs`, `bfs_directed.rs`) | Depth-bounded, cycle-detected traversal; direction-optimised push/pull BFS over CSR | Drives `analyze dominators`/`slice`; host also has its own `src/graph/traversal.rs` for impact/affected | EXPOSED (×2) | As-is. |
| Transitive closure (`closure.rs`) | Bounded depth-N reachability fixpoints | Host `codegraph impact`/`affected` (BFS radius) + `analyze slice --depth` cover the user-facing need | SUPERSEDED | Use host impact/affected; keep closure internal for future Datalog work. |
| Approximate reachability (`hll.rs`) | HyperLogLog per-node reachability estimates for >100k-node graphs | None | UNEXPOSED | `analyze stats --estimate-reachability` (or fold into a future `analyze stats`): `hll::approximate_reachability(graph)`; gate behind node-count threshold so small repos get exact numbers. |
| Label-constrained reachability (`label_reachability.rs`) | RLC: reachable via edge-label-sequence patterns (`Calls+ Implements?`) | None — verified NOT wired into the DSL (lib.rs declaration only); DSL `via`/`intermediate` constrain nodes, not edge labels | UNEXPOSED | Wire as a DSL path-pattern extension (`paths ... where edges match "<pattern>"`) once `analyze query` lands, or `analyze reachable <from> --edge-pattern "<p>"`. Entry: `label_reachability::{reachable_targets, reachable}`. |
| Dominators (`dominators.rs`) | Generic dominator trees | `analyze dominators` | EXPOSED | As-is (note: recomputes per node because `graph.inner()` is pub(crate) — expose a whole-tree API in the analysis crate to lift the `--top` cap). |
| Whole-graph algorithm suite (`analysis.rs`) | SCC, dominator chains, taint paths, k-shortest paths, **centrality (PageRank)**, **critical/articulation nodes**, **bridge edges**, components, toposort, transitive reduction, **parallel edit groups/coloring**, **maximal cliques**, all-pairs distances, **Dot export** | EXPOSED **partially**: `cycles` (SCC + break suggestions), `dominators` (chains), `taint` (paths). Centrality, critical nodes, bridges, cliques, parallel groups, distances, toposort, Dot export have **no surface** | EXPOSED (partial) | Close list: `analyze centrality`, `analyze critical`, `analyze export --format dot`. All pure graph algorithms — full fidelity over the bridge today, zero extraction work. |
| Community detection (`communities.rs`) | Louvain modularity | `analyze communities` | EXPOSED | As-is. |
| Stratified negation (`strata.rs`) | Rule stratification for negation-safe rule queries | None | UNEXPOSED | Leave internal — its only consumer is the not-yet-built Datalog engine (DESIGN_FUTURE P13). No surface until that exists. |

### 6. Intraprocedural analysis (IR / CFG / dataflow / complexity) — 9 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Language-agnostic IR (`ir.rs`, `ir_map.rs`) | Adapters lower ASTs once; `build_ir_map` builds the interprocedural `DataflowOracle` for value-level slicing/taint | None — **blocked**: `build_ir_map` anchors by byte span; bridge spans are `0..0` | UNEXPOSED (×2) | The big unlock: add byte offsets to the host SQLite schema (extraction already has them at parse time), then lower IR in a bridge post-pass via host-grammar re-parse. Everything in section 7 marked "blocked on IR" lights up at once. |
| Control-flow graphs (`cfg.rs`, `cfg_rules.rs`) | Per-function basic blocks + typed edges; rules for rust/ts/js/python/go/java/c/cpp/php (+ more) — but only the Rust *adapter* populates `node.cfg` upstream (ADAPTER_PARITY) | None over the bridge; DSL `cfg` operator will parse (in-flight) but return empty | UNEXPOSED (×2) | `analyze cfg <symbol>` using the proven complexity pattern: re-parse the on-disk file with host grammars, anchor by line/col, call `cfg::build_cfg`. Honesty-gate to languages with `CfgRules::for_language` rules; report skips. |
| Per-function dataflow (`dataflow.rs`, `dataflow_rules.rs`) | Param flows, returns, assignments, arg flows, mutation detection (Rust-extraction-only upstream) | None; DSL `dataflow` operator empty over bridge | UNEXPOSED (×2) | Same re-parse pattern: `analyze dataflow <symbol>` → `dataflow::extract_dataflow`; gate by `dataflow_rules` coverage. |
| Complexity metrics (`complexity.rs`, `complexity_rules.rs`) | Cyclomatic/cognitive/nesting/Halstead/LOC/MI, 12+ language rule tables | `analyze complexity [--top N]` (re-parse pattern, skipped-language breakdown) | EXPOSED (×2) | As-is — this is the reference implementation of the re-parse pattern. |
| Precondition predicates (`predicates.rs`) | Backward control-flow predicate extraction (`extract_predicates` needs source + byte_pos) powering the DSL `preconditions` renderer | DSL `preconditions` (call-graph backward BFS) rides the in-flight `analyze query`; source-level predicate enrichment degrades over the bridge (byte_pos 0) | UNEXPOSED (source-level; call-graph flavor IN-FLIGHT) | After byte ranges land, the DSL renderer's `extract_predicates` enrichment starts working automatically; no separate surface needed. |

### 7. Interprocedural analysis — 11 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Points-to / alias analysis (`points_to.rs`) | Andersen-style field-sensitive, flow-insensitive points-to over IR; `PointsToTable::{pts_of, may_alias}` | None — blocked on IR (byte ranges) | UNEXPOSED | Post-IR: `analyze alias <fn> <var-a> <var-b>` / points-to backing for value-level slices. Entry: `points_to` over `ir_map::build_ir_map`. Don't expose before IR — partial coverage is worse than none. |
| Possible-types propagation (`possible_types.rs`) | Concrete subtype sets flowing into/out of each function (`propagate_possible_types`); DSL `possible_types` operator exists | Works over the bridge **today** (UsesType/Implements edges carried); operator becomes reachable via in-flight `analyze query`, but nothing runs the propagation pass first | UNEXPOSED (pass wiring) | Run `propagate_possible_types` as an enrichment pass in the bridge (via `pass.rs`) before DSL evaluation, and/or `analyze types <symbol>`. |
| Monomorphization detection (`monomorphize.rs`) | Generic defs + callsites supplying concrete type args (`find_instantiations`, `annotate`) — signature/metadata-driven | None; bridge carries `signature` metadata so it works today | UNEXPOSED | `analyze generics [symbol]` → `monomorphize::find_instantiations`. |
| Program slicing (`slicing.rs`) | Forward/backward slices over a `DataflowOracle` | `analyze slice` at call-graph granularity (`CallGraphOracle`); value-level blocked on IR | EXPOSED (call-graph) | Keep; upgrade oracle to `ir_map` post-byte-ranges — report's `granularity` field flips to value-level automatically. |
| Value-level taint (`taint_v2.rs`) | Interprocedural source→sink with sanitizer awareness over IR | `analyze taint` uses graph-path `analysis::taint_paths` instead (honest `granularity: "call-graph"` note) | UNEXPOSED (blocked on IR) | Post-IR: swap into `analyze taint` behind the same report shape. |
| Name-based taint inference (`taint_naming.rs`) | Bimodal source/sink classification from identifier names (`classify_name`, `flow_priority`) | None | UNEXPOSED | `analyze taint --suggest` (no source/sink args): rank candidate source/sink pairs by `flow_priority`. Works today — pure name analysis. |
| Trait/type hierarchy analyses (`traits_hierarchy.rs`) | Implementation hierarchies, trait-dispatch call edges, type-based clustering | None directly; DSL `trait_impls`/`dispatch`/`cluster by type` operators ride in-flight `analyze query`; Implements edges carried by bridge → works today | UNEXPOSED (direct surface) | `analyze traits [type]` → `trait_hierarchies()/trait_dispatch_calls()/cluster_by_primary_type()`. Also feed dispatch edges to the host resolution pipeline as heuristic edges (dynamic-dispatch coverage for `codegraph_explore`). |
| Co-change / temporal coupling (`co_change.rs`) | Mines `git log --name-only` (`fetch_git_history`), maps commits to nodes, confidence-scored pairs (`compute_co_changes`, `co_changes_for_nodes`); `GraphSession::co_changes`; DSL `co_changes` op shells git live | None — verified no host git-history mining anywhere; DSL op becomes reachable via in-flight `analyze query` (per-node only) | UNEXPOSED | `analyze co-change [symbol] [--min-support N]` — repo-wide pairs via `fetch_git_history` + `compute_co_changes`, per-symbol via `co_changes_for_nodes`. Works today, zero extraction dependencies. |
| Coverage mapping (`coverage.rs`) | LCOV parsing (`parse_lcov`), annotates function nodes with line/branch coverage (`annotate_graph_from_lcov`, `CoveragePass`); DSL `untested` filter | None — verified; `untested` will return empty over the bridge | UNEXPOSED | `analyze coverage --lcov <path> [--untested]`: parse + annotate the bridged graph, report covered/uncovered functions; run `CoveragePass` before DSL eval when `--lcov` given so `untested` works. |
| Cascade task generation (`cascade.rs`) | `CascadeTask`s grouped by file when a signature change ripples | `analyze impact [--signature]` | EXPOSED | As-is. |
| Virtual edit validation (`validation.rs`) | `VirtualValidator::validate_signature_change(target, arity_before, arity_after)` — pre-commit simulation, per-call-site breakage verdicts; `preview_affected_call_sites` | **Not** exposed — verified distinct from `analyze impact` (impact generates cascade tasks; validation simulates and judges the edit) | UNEXPOSED | `analyze validate <symbol> --params-before N --params-after M` (or `analyze impact --validate`): returns pass/fail + affected-call-site verdicts. Works today over Calls edges. |

### 8. Query DSL — 5 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Pipe DSL engine (`dsl/mod.rs`, `dsl/aggregate.rs`, `dsl/plan.rs`, `dsl/provenance.rs`, `dsl/stream.rs`) | ~30 operators (callers/callees/depth/filter/show, set algebra, path patterns w/ `via`, taint, preconditions, entrypoints, scc, dominators, trait_impls, dispatch, cluster, affected, multi_path, untested, possible_types, co_changes, communities, complexity, cfg, dataflow, hot, since), aggregation/let/quantifiers, plan optimiser, why-provenance, streaming eval | None today; `analyze query "<dsl>"` is in flight | IN-FLIGHT (×5) | `analyze query` over `GraphSession::query`. Document per-operator data requirements (constraint #3 above): `cfg`/`dataflow`/`untested`/`since`/`hot` need the ingestion items in the close list before they return anything. |

### 9. Agent-facing context engine — 10 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| Context builder + supporting machinery (`context/mod.rs`, `budget.rs`, `clustering.rs`, `expansion.rs`, `render.rs`, `resolver.rs`, `dataflow_seed.rs`, `heuristics.rs`, `retrieval_gate.rs`) | `build_context(task, opts)`: intent classification (heuristics), Repoformer-style retrieve/abstain gate, DRACO dataflow seeding, subgraph expansion, file-count-scaled budget, clustering + source merging, markdown render, multi-separator symbol resolution | Host `src/context/` (ContextBuilder) exists as a library module but has **no CLI command yet**; `context --budget --strategy analysis` is in flight and `build_context` uses all nine modules (verified imports) | IN-FLIGHT (×9) | The in-flight `context --strategy analysis` is the surface. Per the TS-side lesson ("adapt the tool to the agent"), do NOT turn the retrieval gate into a new MCP tool — if it ever crosses to MCP, it shapes existing tool *responses* server-side. |
| Retrieval measurement harness (`context/measure.rs`) | Quantified before/after gate-savings measurement | Host has `scripts/agent-eval`-style methodology (TS repo); nothing in Rust | UNEXPOSED | Leave internal — it's an eval harness, not a user capability. Use it from benches/tests when tuning the gate; never a CLI/MCP surface. |

### 10. Output / interop / facade — 4 modules

| capability (modules) | what the prototype does | codegraph equivalent today | gap status | right-way placement |
|---|---|---|---|---|
| CPG report façade (`analysis_tools.rs`) | `program_slice`/`data_dependencies`/`taint_flow` as compact source-annotated, path-capped text reports | `analyze slice`/`taint` emit their own report shapes without source annotation | UNEXPOSED | Add `--source` to `analyze slice`/`taint` rendering via these functions (line-span-based annotation works today; value-level fidelity arrives with IR). |
| Token-budgeted formatting (`formatting.rs`) | `format_query_result` under a token budget; `FormattedOutput` | `GraphSession::query` returns it → rides in-flight `analyze query` | IN-FLIGHT | As the `analyze query` output path. |
| Stable JSON schemas + envelopes (`schema.rs`) | JSON Schemas for QueryResult/EntrypointSummary/ContextResult/FormattedOutput; `Envelope` with payload-kind tagging; `json_schema_for` | `analyze --json` emits stable serde shapes but no schema export, no envelopes | UNEXPOSED | Wrap `analyze query`/`context --json` output in `schema::Envelope`; add `analyze schema <kind>` printing `json_schema_for(kind)` for downstream tooling. |
| High-level facade (`session.rs`) | `GraphSession`: query memoization, edit invalidation, co_changes, context, search/callers/callees/impact/node/explore/grep/outline, overlay open/save | Bridge already builds it (`BridgeResult::into_session`); `analyze query` + `context` (in flight) ride it. Its `explore/search/node/grep/outline` duplicate host MCP tools | IN-FLIGHT | The single analysis entry point for CLI work. Its agent-shaped methods (explore/search/node) are SUPERSEDED by host MCP tools — never expose them as a second agent surface. |

---

## CLOSE LIST — prioritized UNEXPOSED items worth exposing

Tier 1 — works today over the bridge, zero extraction changes:

1. **Expose co-change mining**: `codegraph analyze co-change [symbol] [--min-support N] [--max-commits N]` → `co_change::fetch_git_history` + `compute_co_changes` (repo-wide) / `co_changes_for_nodes` or `GraphSession::co_changes` (per-symbol).
2. **Expose LCOV coverage mapping**: `codegraph analyze coverage --lcov <path> [--untested]` → `coverage::parse_lcov` + `annotate_graph_from_lcov` (run `CoveragePass` pre-DSL so the in-flight `analyze query`'s `untested` operator stops returning empty).
3. **Expose virtual-edit validation**: `codegraph analyze validate <symbol> --params-before N --params-after M` (or `analyze impact --validate`) → `validation::VirtualValidator::{validate_signature_change, preview_affected_call_sites}` — the simulate-and-judge complement to the exposed cascade-task `analyze impact`.
4. **Expose trait/type hierarchy analyses**: `codegraph analyze traits [type]` → `traits_hierarchy::{trait_hierarchies, trait_dispatch_calls, cluster_by_primary_type}` (Implements/UsesType edges already bridged).
5. **Expose centrality + critical nodes**: `codegraph analyze centrality [--top N]` / `analyze critical` → `analysis::{centrality, critical_nodes, bridge_edges}` — pure graph, full fidelity, already compiled in.
6. **Expose Dot/Graphviz export**: `codegraph analyze export --format dot [--symbol X --depth N]` → `analysis::{to_dot, to_dot_subgraph}`.
7. **Expose possible-types**: run `possible_types::propagate_possible_types` as a bridge enrichment pass (through `pass.rs`) + `analyze types <symbol>`; makes the DSL `possible_types` operator real.
8. **Expose monomorphization detection**: `codegraph analyze generics [symbol]` → `monomorphize::find_instantiations` (bridge carries signatures).
9. **Expose taint source/sink suggestion**: `analyze taint --suggest` → `taint_naming::{classify_name, flow_priority}` when source/sink args omitted.
10. **Expose polyglot boundaries**: `codegraph analyze boundaries` → `polyglot::{detect_http_routes, detect_ffi_exports, detect_wasm_exports, resolve_cross_language_calls}`; longer-term, feed stitched cross-language edges into host resolution as heuristic edges.
11. **Expose capability introspection**: `codegraph analyze capabilities` → print `capabilities::CapabilityTree` + the (rebranded) `CODEGRAPH_ANALYSIS_CAP_*` env names.
12. **Expose JSON schemas/envelopes**: wrap `analyze query`/`context --json` in `schema::Envelope`; `analyze schema <kind>` → `schema::json_schema_for`.
13. **Expose HLL reachability estimates**: `analyze stats --estimate-reachability` → `hll::approximate_reachability` (threshold-gated to large graphs).

Tier 2 — needs the complexity-style source re-parse anchor (host grammars, line/col):

14. **Expose CFG introspection**: `codegraph analyze cfg <symbol>` → `cfg::build_cfg` + `cfg_rules::CfgRules::for_language`; honesty-gate to rule-covered languages, report skips (ADAPTER_PARITY limits apply).
15. **Expose per-function dataflow**: `codegraph analyze dataflow <symbol>` → `dataflow::extract_dataflow` + `dataflow_rules`; same gating.
16. **Source-annotated slice/taint reports**: `analyze slice|taint --source` → `analysis_tools::{program_slice, data_dependencies, taint_flow}`.

Tier 3 — blocked on schema/architecture work (do these to unblock, don't expose half-bridged):

17. **Store byte ranges in the SQLite schema** (extraction has them at parse time) → unlocks `ir_map::build_ir_map` → upgrades `analyze slice`/`taint` from call-graph to value-level (`slicing` over `PointsToOracle`, `taint_v2::analyze`, `points_to`, source-level `predicates`) with no surface changes — the reports' `granularity` field flips honestly.
18. **Carry Field nodes through the bridge (flag-gated)** → `partial::get_partial_struct` field-level views in `context --strategy analysis` output.
19. **Label-constrained reachability in the DSL**: extend path patterns with edge-label constraints → `label_reachability::{reachable_targets, reachable}` (currently dead code — verified unused by the DSL).
20. **Daemon-resident GraphSession**: hold the bridged session in the MCP daemon, invalidate on sync events via `reactive::ReactiveDb` *or* `incremental` (pick one) — makes repeated `analyze`/`query` calls skip the full re-bridge.
21. **Overlay base+diff snapshots**: `codegraph snapshot` family on top of the in-flight snapshot cache → `overlay::{save_base_snapshot, diff_against_base, apply_diff_to_graph}` for shared monorepo base indexes.

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
| EXPOSED | 16 | lib, graph, nodes, edges, index, csr, frontier, traversal, bfs_directed, dominators, analysis (partial — see close list 5/6), communities, complexity, complexity_rules, slicing, cascade |
| IN-FLIGHT | 21 | fingerprint, cache, data_dir, capabilities (rebrand; introspection unexposed), kind_specific, formatting, session, dsl/{mod, aggregate, plan, provenance, stream}, context/{mod, budget, clustering, dataflow_seed, expansion, heuristics, render, resolver, retrieval_gate} |
| UNEXPOSED | 30 | polyglot, incremental, reactive, persistence, overlay, partial, pass, history, hll, label_reachability, strata, ir, ir_map, cfg, cfg_rules, dataflow, dataflow_rules, predicates, points_to, possible_types, monomorphize, taint_v2, taint_naming, traits_hierarchy, co_change, coverage, validation, analysis_tools, schema, context/measure |
| SUPERSEDED | 21 | adapter/{mod + 12 languages}, builder, call_site, resolver, content_index, framework_routes, worktree, symbols, closure |
| N/A | 1 | enrichment |
