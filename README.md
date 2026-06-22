# CodeGraph — Rust port

A complete Rust port of the TypeScript CodeGraph implementation ([colbymchenry/codegraph](https://github.com/colbymchenry/codegraph)):
a local-first code-intelligence library + CLI + MCP server. It parses any supported
codebase with tree-sitter, stores symbols/edges/files in SQLite (FTS5), and exposes
the knowledge graph to AI agents (Claude Code, Cursor, Codex CLI, opencode, …) over
MCP. Per-project data lives in `.codegraph/`; extraction is deterministic — derived
from the AST, never LLM-summarized.

The port is **behavior-faithful by construction**: one `.ts` file maps to one `.rs`
file, same algorithms, same constants, same node-ID hashing (sha256 of identical
strings), same JSON wire shapes (camelCase, same key order where it matters).
See [`PORTING.md`](PORTING.md) for the conventions and [`notes/`](notes/) for
per-module porting notes and documented deviations.

## Build, test, run

```bash
cargo build --release      # produces target/release/codegraph
cargo test --all-targets   # full suite (unit + integration), no network needed
cargo clippy --all-targets # warning-clean
cargo fmt --check          # format-clean
```

The single `codegraph` binary serves both the CLI and the MCP server, exactly like
the npm package's `dist/bin/codegraph.js`:

```bash
codegraph init [path]        # initialize .codegraph/ and build the index
codegraph index|sync|status  # maintain the index
codegraph query|files|context|affected   # query the graph
codegraph analyze <cmd>      # analysis engine: complexity, communities, dominators,
                             # slice, cycles, impact (cascade), taint — see below
codegraph install            # wire the MCP server into agents (Claude Code, Cursor, …)
codegraph serve --mcp        # run as an MCP server over stdio
codegraph uninit [path]      # remove .codegraph/
```

All `CODEGRAPH_*` environment variables keep their exact TS names and semantics
(`CODEGRAPH_NO_DAEMON`, `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`,
`CODEGRAPH_WATCH_DEBOUNCE_MS`, `CODEGRAPH_NO_WATCH`, …).

There is also a small auxiliary binary, `codegraph-mcp-server` (stdio MCP server
only), kept for the integration suite; `codegraph serve --mcp` is the canonical
entry point.

## Module map (mirrors the TS repo’s `src/`)

| Rust module | TS source | What it is |
|---|---|---|
| `src/lib.rs` + `src/codegraph.rs` | `src/index.ts` | Public API — the `CodeGraph` struct: `init`/`open`/`close`, `index_all`, `sync`, `search_nodes`, `get_callers`/`get_callees`, `get_impact_radius`, `build_context`, `watch`/`unwatch` |
| `src/types.rs`, `src/error.rs`, `src/utils.rs`, `src/directory.rs` | `src/types.ts`, `src/utils.ts` | `Node`/`Edge`/`NodeKind`/`EdgeKind`/`Language`, errors, hashing/path helpers, `.codegraph/` dir management |
| `src/db/` | `src/db/` | `DatabaseConnection`, `QueryBuilder` (prepared statements), `schema.sql` (embedded via `include_str!`), migrations — rusqlite (bundled SQLite + FTS5) |
| `src/extraction/` | `src/extraction/` | `ExtractionOrchestrator`, tree-sitter wrapper/helpers, 19 per-language extractors under `languages/`, standalone extractors (`svelte`, `vue`, `liquid`, `dfm` for Delphi, `mybatis`, `ida_c`), generated-file detection |
| `src/resolution/` | `src/resolution/` | `ReferenceResolver` orchestrating `import_resolver` (+ `path_aliases`, `workspace_packages`, `go_module`), `name_matcher`, `callback_synthesizer` (dynamic-dispatch edge synthesis), `swift_objc_bridge`, and 20 framework resolvers under `frameworks/` |
| `src/graph/` | `src/graph/` | `GraphTraverser` (BFS/DFS, impact radius, path finding) and `GraphQueryManager` |
| `src/context/` | `src/context/` | `ContextBuilder` + formatter for markdown/JSON output |
| `src/search/` | `src/search/` | FTS5 query parser and ranking helpers |
| `src/sync/` | `src/sync/` | `FileWatcher` (notify-based, debounced, gitignore-aware), watch policy, git hooks, worktree detection |
| `src/mcp/` | `src/mcp/` | MCP server: `session` (protocol state machine), `tools`, `transport`, `engine`, shared `daemon` + `proxy` (#411), `server_instructions` (returned in MCP `initialize`) |
| `src/installer/` | `src/installer/` | `codegraph install`: target registry + one file per agent (`claude`, `cursor`, `codex`, `opencode`, `gemini`, `kiro`, `antigravity`, `hermes`), surgical JSON/JSONC/TOML/YAML config editing |
| `src/bin/codegraph.rs` | `src/bin/codegraph.ts` | CLI (clap; commander in TS) — same subcommands, same flags, plus the Rust-only `analyze` family |
| `src/ui/` | `src/ui/` | Terminal UI: shimmer progress, glyph fallbacks (ASCII/Unicode) |
| `src/analysis_bridge.rs`, `src/analyze/`, `src/analyze_ir.rs` | — (Rust-only) | Bridge from the SQLite index into the `codegraph-analysis` engine + the report runners behind `codegraph analyze`; `src/analyze/reports/` owns the report-family modules |
| `analysis/` | — (Rust-only) | The `codegraph-analysis` crate (migrated jfc-graph): petgraph code graph, query DSL, graph algorithms, IR/CFG/dataflow analyses |

## Analysis engine (`codegraph analyze`)

The full **jfc-graph analysis engine** lives in this workspace as the
[`analysis/`](analysis/) crate (`codegraph-analysis`; see
[`notes/jfc-graph-migration.md`](notes/jfc-graph-migration.md) for the
migration record). `src/analysis_bridge.rs` materializes an analysis graph
from an already-indexed `.codegraph/` SQLite database — no re-parsing, pure
read — and `src/analyze/mod.rs` plus `src/analyze/reports/` run the
engine's public capabilities over it.
The `codegraph analyze` subcommands expose them on the CLI; the same
functions are library API (`codegraph::analyze`). The MCP tool surface is
deliberately untouched.

Every subcommand prints human output by default and a stable camelCase JSON
shape with `--json` (wrapped in a `{"schemaVersion", "kind", "data"}`
envelope). The full command table:

```bash
# Query & graph structure
codegraph analyze query '<dsl>'               # pipe-based graph query DSL (below);
                                              # --explain plan, --why provenance,
                                              # --lcov enables `untested`
codegraph analyze stats [--estimate-reachability]  # node/edge/kind counts; exact or
                                              # HyperLogLog reachability profiling
codegraph analyze communities                 # Louvain call-graph communities
codegraph analyze cycles                      # SCCs: mutual recursion + dependency
                                              # cycles, with break suggestions
codegraph analyze dominators <symbol>         # immediate dominators of everything
                                              # reachable from an entry symbol
codegraph analyze centrality [--top N]        # PageRank: most depended-upon symbols
codegraph analyze critical                    # articulation nodes + bridge edges
                                              # (single points of failure)
codegraph analyze export [-s <symbol> -d N]   # Graphviz DOT export (whole graph or
                                              # a symbol's neighborhood)

# Flow & change analysis
codegraph analyze slice <symbol> [--direction fwd|bwd] [--value-level]
                                              # program slice: call-graph hops, or
                                              # value-level over dataflow IR on
                                              # schema-v5 indexes (byte offsets)
codegraph analyze taint <source> <sink> [--value-level]
                                              # source → sink paths, each hop
                                              # annotated; --suggest ranks candidate
                                              # pairs by identifier naming
codegraph analyze impact <symbol> [-s <sig>]  # signature-edit cascade: direct call
                                              # sites to update, grouped by file
                                              # (distinct from `codegraph impact`,
                                              # which is a BFS blast radius)
codegraph analyze validate <symbol> --params-before N --params-after M
                                              # simulate an arity change before
                                              # making it: per-caller verdicts
codegraph analyze diff [--base <snapshot|auto>]
                                              # working tree vs the last cached
                                              # snapshot: nodes/edges added/removed/
                                              # changed, complexity deltas for
                                              # changed functions, newly-introduced
                                              # cycles, impact set of the delta

# Per-function source analysis (re-parse anchor pattern)
codegraph analyze complexity [--top N]        # cyclomatic/cognitive/nesting/
                                              # Halstead/maintainability
codegraph analyze cfg <symbol>                # control-flow graph: basic blocks +
                                              # typed edges
codegraph analyze dataflow <symbol>           # params, returns, assignments,
                                              # argument flows, mutations

# Types & dispatch
codegraph analyze types <symbol>              # concrete types that can flow
                                              # into/out of a function
codegraph analyze traits [type]               # trait/interface hierarchies,
                                              # dispatch calls, type clusters
codegraph analyze generics [symbol]           # generic definitions + callsite
                                              # instantiations
codegraph analyze boundaries                  # cross-language boundaries: HTTP
                                              # routes, FFI and WASM exports

# History & coverage
codegraph analyze co-change [symbol]          # temporal coupling mined from git log
codegraph analyze coverage --lcov <path> [--untested]
                                              # map LCOV onto functions; also
                                              # enables the DSL `untested` operator

# Introspection
codegraph analyze capabilities                # engine capability toggles + their
                                              # CODEGRAPH_ANALYSIS_CAP_* env vars
codegraph analyze schema <kind>               # JSON Schema for an engine payload
```

### Query DSL (`codegraph analyze query`)

The engine's pipe-based DSL runs directly against the bridged graph. Seed a
node set with `fn("name")`, `type("Name")`, `entrypoints`, `scc`, or `hot N`;
pipe it through `callers`, `callees`, `depth N`, `filter kind=K`, …; combine
with set algebra (`union` / `intersect` / `\`), path patterns, and
aggregations (`count`, `exists`, `group_by`, …):

```bash
codegraph analyze query 'fn("main") | callees | depth 3'     # all main() reaches in ≤3 hops
codegraph analyze query 'path fn("main") -> fn("helper")'    # shortest call path A → B
codegraph analyze query 'scc'                                # mutual-recursion clusters
```

`--explain` prints the optimised query plan without executing; `--why`
attaches per-row provenance (which operators produced each result).

### Graph snapshot cache

Bridging re-reads every node/edge row from SQLite, which dominates wall-clock
on large indexes — so the bridged graph is snapshotted under
`<project>/.codegraph/analysis/` (`graph.bin` + `meta.json`) keyed by a
fingerprint of the index. Repeat `analyze`/`context --strategy analysis`
invocations load the snapshot (a dim `(cached graph)` notice in human output;
`--json` stays pure JSON), and any re-index that changes the store invalidates
it automatically. `--no-cache` forces a rebuild; cache failures always degrade
to a silent rebuild, never wrong answers.

One **previous generation** is kept: a refresh that changes the fingerprint
first rotates the old snapshot to `*.prev`. That rotated generation is what
`codegraph analyze diff --base auto` compares the working tree against
(edit → re-index → `analyze diff` shows exactly what changed). A diff run
also annotates the current snapshot with per-function complexity
(`complexity.json`), so the *next* diff reports full before/after complexity
deltas; with no base snapshot cached, `analyze diff` says so honestly and
exits 0.

### Environment variables (all native — no `JFC_*` compatibility)

- `CODEGRAPH_ANALYSIS_DATA_DIR` — overrides the engine's per-workspace data
  dir. Default resolution: `$XDG_CACHE_HOME/codegraph-analysis/<workspace-hash>/`
  → `$HOME/.cache/codegraph-analysis/<workspace-hash>/` → in-workspace
  `.codegraph-analysis/` fallback.
- `CODEGRAPH_ANALYSIS_CACHE_DIR` — relocates both the engine's per-file parse
  cache (default `$HOME/.cache/codegraph-analysis/v1/`) and the CLI's graph
  snapshot cache (which then lives under `<override>/<workspace-key>/`).
- `CODEGRAPH_ANALYSIS_CAP_*=0|false|off|no` — capability kill-switches
  (`CALL_GRAPH`, `TYPE_USAGE`, `PARTIAL_STRUCT`, `VIRTUAL_VALIDATION`,
  `PERSISTENCE`, `SYMBOL_EDITING`).

### Token-budgeted context (`codegraph context --strategy analysis`)

`codegraph context "<task>" [--budget <tokens>] [--strategy classic|analysis]`:
`classic` (default) is the pre-existing `ContextBuilder` pipeline, unchanged —
`--budget` applies a plain output trim on top. `--strategy analysis` routes
through the engine's context modules over the bridged graph: identifiers from
the task are resolved to symbols (exact, then case-insensitive/prefix
fallback), entry points are dataflow-seeded (DRACO), expansion is
retrieval-gated (Repoformer), and per-file clustered source is rendered to
markdown trimmed to the token budget (~4 chars/token). The report states its
honest capability — e.g. `seeding: "call-graph"` when the index has no
type-flow edges for the resolved symbols — and never fabricates dataflow.

```bash
codegraph context "how does the daemon handshake work" --strategy analysis --budget 2000
```

**What runs at which fidelity** (the reports state this themselves rather
than over-claiming):

- **All indexed languages** — `communities`, `dominators`, `cycles`,
  `impact`, `validate`, `centrality`, `critical`, `export`, `stats`,
  `traits`, `diff`, `co-change`, `coverage`, and the call-graph–granularity
  `slice`/`taint` are pure graph algorithms over the bridged index, so they
  work for every language the indexer supports.
- **12 languages** — `complexity` (and `diff`'s complexity deltas) re-parses
  on-disk sources with the compiled tree-sitter grammars and runs the
  engine's metrics; rules exist for Rust, TypeScript/JavaScript, Python, Go,
  Java, C, C++, C#, PHP, Kotlin, Swift, and Ruby. Functions in other
  languages are counted in the report's `skipped` breakdown.
- **Value level needs IR** — `slice`/`taint --value-level` lower the
  engine's per-function dataflow IR by re-parsing on-disk sources anchored
  at the indexed byte offsets (schema-v5 indexes; re-index pre-v5 projects
  to enable). IR lowering covers Rust, TypeScript/JavaScript, Python, and
  Go; `cfg`/`dataflow` use the same anchor pattern with their own rule
  tables. Without `--value-level` (or on pre-v5 indexes), `slice` and
  `taint` run over a call-graph dataflow oracle and say so in their
  `note`/`granularity` fields — function-level hops along call edges, never
  pretended value tracking.

## Key architectural deviations from the TS implementation

Collected from [`notes/*.md`](notes/); each module's notes file documents its own
deviations exhaustively. The big ones:

- **No async runtime.** TS `async` exists for Node's event loop; rusqlite and
  tree-sitter are synchronous. Everything is plain sync Rust. Parallelism comes
  from `rayon` (parsing), and `std::thread` + `crossbeam-channel` (file watcher,
  daemon, MCP transport — one thread per transport instead of event-loop
  interleaving; behavior-identical).
- **Single SQLite backend.** The TS dual backend (better-sqlite3 native with a
  node-sqlite3-wasm fallback) collapses to rusqlite with bundled SQLite + FTS5.
  `codegraph status` still reports the backend string as `"native"` for output
  parity — there is no wasm slow path to fall back to.
- **Native tree-sitter grammars instead of wasm.** Grammar crates are compiled in
  (see `Cargo.toml`); `web-tree-sitter`'s async `Parser.init()` and the
  `dist/*.wasm` copy step disappear entirely. No runtime grammar loading.
- **Node-isms dropped.** `MemoryMonitor`, `processInBatches`, event-loop
  `debounce`/`throttle` helpers, and the parse worker-thread pool are gone or
  replaced by their natural Rust equivalents (the watcher implements its own
  debounce; rayon replaces the worker pool). The Node 25 hard-exit check is
  irrelevant and not ported.
- **String offsets are UTF-8 bytes, not UTF-16 code units.** Affects only
  column-ish internals on non-ASCII lines; line numbers (what the graph stores)
  are identical.
- **Daemon/proxy socket is Unix-only.** The TS daemon listens on a named pipe on
  Windows via Node's `net`; Rust std has no named-pipe listener, so on Windows the
  MCP server runs in direct (in-process) mode, as if `CODEGRAPH_NO_DAEMON=1`.
  Lockfile arbitration got *stronger* than TS: the pidfile is hard-linked into
  place fully written (no empty-file race window).
- **Errors are `Result`, not exceptions.** Where TS throws, Rust returns
  `crate::error::Result`; user-visible messages are kept identical.
- **JSON output omits absent optionals** (serde `skip_serializing_if`) where TS
  sometimes serializes `null` (e.g. `"visibility": null` in `query --json`). Key
  names, casing, and ordering otherwise match the TS wire format.

## Test suite

`cargo test --workspace --all-targets`: **2,154 tests, 0 failures**:

- 427 unit tests in `src/` (`#[cfg(test)]`, alongside the code they cover)
- 944 integration tests in `tests/` — 24 suites ported from `../__tests__/*.test.ts`
  plus the Rust-only analysis suites (extraction 289+3, frameworks 64/67/33,
  installer targets 80, db 50, codegraph API 42, MCP tools/server/protocol/daemon
  41/32/15/20, resolution 30+29+6, graph 31, sync 26, context 23, security 15,
  analyze CLI 12, analysis bridge 9, CLI 9, foundation 8, git hooks 7, grammars 3)
- 783 tests in the `codegraph-analysis` crate (773 unit + the golden
  `fingerprint_stability` and `node_id_stability` suites)

Same testing philosophy as the TS suite: temp dirs (`tempfile::tempdir()`), real
files, real SQLite — no mocking. Platform-specific behavior is gated with
`#[cfg(unix)]` / `#[cfg(windows)]`. `cargo clippy --all-targets` and
`cargo fmt --check` are clean.

## Parity with the TS implementation

Full report: [`notes/parity.md`](notes/parity.md) (2026-06-06; both arms indexing
an identical 353-file fixture — the full TS+Rust repo tree).

**Headline: nodes, node IDs, and files are byte-identical (9,105 nodes, sha256 ID
set exactly equal); edges differ by 58 rows (0.19%).** Both arms are individually
100% deterministic. Rust indexed the fixture in 1.7s vs 3.2s for TS.

Every divergent edge row was classified:

1. **Ambiguous-name tie-breaks (net 0):** equally-scored same-named candidates,
   different winner (JS Map insertion order vs Rust sort order). Neither is more
   correct.
2. **Batch-boundary duplicate-reference loss (TS −35, Rust −57):** a pre-existing
   *upstream TS bug*, faithfully ported — the post-batch resolved-reference delete
   isn't line-aware, so duplicate refs straddling a 5000-row page boundary are
   dropped. It hits different victims per arm because insertion order differs. The
   only aggregate over 2% (`instantiates` +3.1%) is this bug on the TS side — the
   **Rust count is the correct one**.
3. **Name-matcher receiver-type-inference gap (~33 `calls`, 0.2%):** TS resolves
   `var.member` refs by scanning source for the receiver's type; the Rust port
   leaves them unresolved. The TS edges in this class are wrong-target junk, so
   Rust's conservatism yields a more correct graph — but it is a documented
   divergence from the reference implementation.
4. **Cosmetic:** unresolved-ref *text* differs for some Rust field calls
   (receiver-qualified vs bare member); no edge impact.

Functional spot checks (`query --json`, `affected --json`, `callers --json`,
`--help` surfaces) are identical after normalizing timestamps and TS's serialized
`null`s. Verdict: **within tolerance** — where the arms disagree beyond noise, the
Rust output is the more correct of the two.
