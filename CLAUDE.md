# codegraph-rs

Code-intelligence engine: a tree-sitter symbol/call/type graph over a workspace,
stored in SQLite, queried through a pipe-based DSL and exposed as an MCP server
and a `codegraph` CLI.

## Workspace layout

Two crates (`[workspace] members = ["analysis"]`):

- **`codegraph-rs`** (root, lib name `codegraph`, edition 2021) — extraction
  (tree-sitter, 12+ languages), SQLite persistence (`src/db/`), resolution,
  the MCP server, the CLI, and the bridge to the analysis crate.
  - bins: `src/bin/codegraph.rs` (CLI), `src/bin/codegraph-mcp-server.rs` (MCP).
- **`codegraph-analysis`** (`analysis/`, edition 2024) — the pure analysis
  library: CFG, dataflow, points-to, slicing, dominators, taint, complexity,
  co-change, the DSL, and the inference-based vuln engine (`analysis/src/vuln/`).

The root crate depends on `codegraph-analysis` (path dep).

## Build / test / lint (these are the CI gates — match them exactly)

CI (`.github/workflows/ci.yml`) runs, and a change must pass:

```
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # a single warning fails CI
cargo build --workspace
cargo test --workspace
```

- Format with **nightly**: `cargo +nightly fmt --all` (shaped by repo
  `rustfmt.toml` + global `~/.rustfmt.toml`).
- For fast local iteration, scope to a crate: `cargo test -p codegraph-analysis`,
  `cargo test -p codegraph-rs --lib`, `cargo build --bin codegraph`.
- GPU code is behind the `gpu` feature (cudarc); default builds are CPU-only.

## Architecture seams

- **`src/analysis_bridge.rs`** materializes a `codegraph_analysis::graph::CodeGraph`
  from the SQLite index (`build_analysis_graph(&QueryBuilder)`), mapping the 22
  codegraph node kinds → 5 analysis kinds and the edge kinds. Snapshotted under
  `.codegraph/analysis/` keyed by an index fingerprint. Every `codegraph analyze`
  subcommand goes through this; no re-parsing of source.
- **MCP engine** (`src/mcp/`) holds an `Rc<crate::codegraph::CodeGraph>` (the
  DB-backed graph). Note there are TWO `CodeGraph` types: `crate::codegraph::CodeGraph`
  (root, SQLite-backed) and `codegraph_analysis::graph::CodeGraph` (in-memory,
  analysis); the bridge converts the former to the latter.

## Conventions & invariants (non-obvious — read before editing)

- **MCP tools are extensible.** The old "frozen at 8 for TS wire parity" rule is
  **RETIRED (2026-06)** — the Rust server is the source of truth now, not a TS
  mirror. Currently **13 tools**: the original 8 (`search, callers, callees,
  impact, node, explore, status, files`) plus `vuln, verify_roles, arch, xref,
  paths`. (`verify_roles` is the "model proposes, graph proves" boundary: it runs
  agent-supplied predicate-role proposals through `vuln::classify::GraphVerifier`
  and emits only graph-corroborated findings tagged `InferenceOrigin::Llm`.)
  To add one, all in `src/mcp/tools.rs`: (1) a dispatch arm in
  `ToolHandler::execute()`; (2) a `handle_x(&self, args) -> Result<ToolResult>`
  handler (get the graph via `self.get_code_graph(...)`; for the analysis graph,
  bridge with `analysis_bridge::build_analysis_graph_cached_with_options`);
  (3) a `ToolDefinition` in `tools()` (props via `prop`/`prop_default`/
  `project_path_property`, `annotations: read_only_annotations()`); (4) **bump the
  count + name list in `tool_definition_json_is_wire_compatible_with_ts`** — it no
  longer means TS parity, but it still guards registration shape, so keep it green;
  (5) a functional test in `tests/mcp_tools_test.rs`. Heavy analyses may ALSO ship
  as `codegraph analyze …` CLI subcommands mirrored by `src/analyze.rs` report fns.
- **Recursive AST/graph walkers must call `ensure_sufficient_stack`** (crate
  root fn) at the recursion head — depth is bounded by input, not thread stack;
  a deep input otherwise aborts the process.
- **Per-language behavior lives in static rules tables**, looked up by language
  id: `cfg_rules.rs`, `dataflow_rules.rs`, `concurrency_rules.rs`
  (`for_language(lang)`). Add a language by extending these, not by branching in
  the walkers.
- **SQLite schema is versioned** (`src/db/schema.sql` + `src/db/migrations.rs`,
  currently through v6). A schema change must bump `schema_versions`, add an
  idempotent migration, treat new columns as nullable (backfill on re-index),
  and update count/size pin tests.
- Language-support additions pin sizes with **count tests** (a regression guard
  on how many nodes/edges a fixture yields).

## Notable subsystems

- **Vuln engine** (`analysis/src/vuln/`): inference-based, rule-free bug finding —
  templates (`MissingDominatorCheck`, `ReachesWithoutSanitizer`, …) instantiated
  with *inferred* predicate sets (frequency mining, taint seeds, fix-history,
  LLM-verified), fused in a confidence-weighted `LearnedStore`. Run via
  `codegraph analyze vuln`. Concurrency lint in `analysis/src/concurrency.rs`.
- **Tool-history flywheel** (`src/history.rs`): a separate, global, redacted
  SQLite DB of agent tool usage (`codegraph history ingest|show`) — never the
  per-project graph schema.
