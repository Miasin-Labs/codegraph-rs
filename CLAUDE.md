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
  co-change, the DSL, and the concurrency lint (`analysis/src/concurrency.rs`).

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
  mirror. **10 tools by default** (`DEFAULT_MCP_TOOLS` in
  `registry/filters.rs`: `search, callers, callees, impact, node, explore,
  status, files, history, tests`), listed on every project regardless of size
  — there is no small-repo gating. `arch, xref, paths` are opt-in through the
  `CODEGRAPH_MCP_TOOLS` allowlist (comma-separated short names), for 13 in
  all. Keep `server_instructions.rs` naming exactly the default set. The
  surface is shaped by mined agent behaviour (627k tool calls, 2026-09):
  `search`/`node` take a `symbols` batch (agents otherwise grep `a|b|c`),
  `search` takes `projectPaths` for several indexes in one call, and the
  `node` file view records into and honours the per-session ledger
  (`explore_session`) so a re-read of unchanged lines returns `alreadySent`.
  Any tool that emits source must do the same, and every new structured field
  must be declared in its output schema — the payloads set
  `additionalProperties: false`, and clients reject undeclared fields. The
  inference vulnerability engine (`vuln`/`verify_roles` tools, `analyze vuln`)
  was **deleted (2026-09)** — recover it from git history, don't re-gate it.
  To add one, update the domain-shaped `src/mcp/tools/` tree: (1) route the
  tool in `handlers.rs`/`ToolHandler::execute()`; (2) add the handler in the
  owning domain module (`graph/`, `explore/`, `admin/`, etc.),
  getting the graph via `self.get_code_graph(...)` or bridging with
  `analysis_bridge::build_analysis_graph_cached_with_options`; (3) register
  the schema in `registry/` using the shared schema builders and
  `read_only_annotations()`; (4) keep the registration-shape test
  `tool_definition_json_is_wire_compatible_with_ts` green — it no longer means
  TS parity, but it still guards names and schemas; (5) add a functional test
  in `tests/mcp_tools_test.rs`. Heavy analyses may ALSO ship as
  `codegraph analyze …` CLI subcommands mirrored by `src/analyze/reports/`
  report fns.
- **Time-boxed callers must never do unbounded work.** Claude Code kills a
  `UserPromptSubmit` hook at 30s and MCP clients time out requests; a killed
  transaction rolls back, so inline work on a large index fails on every try.
  The prompt hook checks the schema read-only (`db::database_schema_is_current`)
  and the vocabulary state (`CodeGraph::segment_vocab_state`); MCP
  `get_code_graph` checks the schema before opening a `projectPath` index.
  Anything short of current/complete goes to `sync::background::
  spawn_background_sync` (a detached `codegraph sync`; it only ever launches
  the `codegraph` CLI binary, never the current test/server executable, and
  `CODEGRAPH_NO_BACKGROUND_SYNC=1` keeps work inline). Per-open schema repair
  (`repair_shared_schema_v9`) must stay cheap for the same reason, and
  `rebuild_name_segment_vocab` must stay one transaction that ends by marking
  the vocabulary complete.
- **Recursive AST/graph walkers must call `ensure_sufficient_stack`** (crate
  root fn) at the recursion head — depth is bounded by input, not thread stack;
  a deep input otherwise aborts the process.
- **Per-language behavior lives in static rules tables**, looked up by language
  id: `cfg_rules.rs`, `dataflow_rules.rs`, `concurrency_rules.rs`
  (`for_language(lang)`). Add a language by extending these, not by branching in
  the walkers.
- **SQLite schema is versioned** (`src/db/schema.sql` + `src/db/migrations.rs`,
  currently through v9). A schema change must bump `schema_versions`, add an
  idempotent migration, treat new columns as nullable (backfill on re-index),
  and update count/size pin tests.
 - Language-support additions pin sizes with **count tests** (a regression guard
   on how many nodes/edges a fixture yields).
 - **Extraction is bounded by the pinned tree-sitter grammar.** A grammar that
   can't parse some (usually *nightly/unstable*) syntax misparses that item and
   degrades extraction for it alone. Known, test-pinned Rust cases (all on
   tree-sitter-rust 0.24): `const trait` (no trait node; required methods hoisted
   to bare fns — `rust_const_trait_is_a_known_grammar_limitation`), **trait
   aliases** `pub trait X = A + B;` (no node; upstream #229) and **declarative
   macros 2.0** `pub macro m {…}` (no node; upstream #45) —
   `rust_trait_alias_and_macro2_are_known_grammar_limitations`. (`const impl`,
   `~const` bounds, let-else, GATs, const generics, raw idents, `gen` blocks,
   safe/unsafe `extern`, auto traits, and doc comments all parse fine — don't
   add extractor branches for them.) Other languages: C/C++ macro-confused
   `namespace`/`switch` "functions", Kotlin `fun interface`. The fix is a grammar
   bump, not an extractor branch. Macro **expansion** is also out of scope —
   tree-sitter parses tokens, so symbols *generated* by a `macro_rules!`
   invocation are never extracted (only the macro def + the call site are).
   `NodeKind` now has **25** variants (added `Macro` for `macro_rules!` defs);
   `map_node_kind` (analysis bridge) maps it to `None` — macros aren't callable
   analysis nodes.

## Notable subsystems

- **Concurrency lint** (`analysis/src/concurrency.rs`, per-language rules in
  `concurrency_rules.rs`): flags lossy best-effort sends. Library-only since
  the vuln engine (its sole CLI surface) was deleted.
- **Tool-history flywheel** (`src/history.rs`): a separate, global, redacted
  SQLite DB of agent tool usage (`codegraph history ingest|show`) — never the
  per-project graph schema.
