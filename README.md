# CodeGraph - native Rust implementation

A complete Rust port of the TypeScript CodeGraph implementation ([colbymchenry/codegraph](https://github.com/colbymchenry/codegraph)):
a local-first code-intelligence library + CLI + MCP server. It parses any supported
codebase with tree-sitter, stores symbols/edges/files in SQLite (FTS5), and exposes
the knowledge graph to AI agents (Claude Code, Cursor, Codex CLI, opencode, …) over
MCP. Per-project data lives in `.codegraph/`; extraction is deterministic — derived
from the AST, never LLM-summarized.

The implementation tracks the TypeScript reference closely: the same algorithms,
constants, node-ID hashing, and JSON wire shapes, with Rust-specific analysis,
decompiler, GPU, Salesforce, and MCP capabilities layered on top.
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
codegraph query|explore|node|files       # inspect symbols, source, and structure
codegraph callers|callees|impact|affected|context
codegraph analyze <cmd>      # analysis engine: complexity, communities, dominators,
                             # slice, cycles, impact (cascade), taint — see below
codegraph install            # wire the MCP server into agents (Claude Code, Cursor, …)
codegraph serve --mcp        # run as an MCP server over stdio
codegraph daemon             # list/stop shared MCP daemons
codegraph telemetry [status|on|off]
codegraph upgrade [version] [--check] [--force]
codegraph version            # also: -v, -V, -version, --version
codegraph uninit [path]      # remove .codegraph/
```

All `CODEGRAPH_*` environment variables keep their exact TS names and semantics
(`CODEGRAPH_NO_DAEMON`, `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`,
`CODEGRAPH_WATCH_DEBOUNCE_MS`, `CODEGRAPH_NO_WATCH`, …).

Project-specific discovery rules live in an optional `codegraph.json` at the
project root. Patterns use gitignore syntax; extension mappings override the
built-in detector:

```json
{
  "extensions": { ".inc": "php", ".component": "typescript" },
  "includeIgnored": ["vendor/embedded-repo/**"],
  "include": ["generated/api/**"],
  "exclude": ["fixtures/**", "dist/**"]
}
```

There is also a small auxiliary binary, `codegraph-mcp-server` (stdio MCP server
only), kept for the integration suite; `codegraph serve --mcp` is the canonical
entry point.

## Supported languages

The indexer recognizes TypeScript, JavaScript, TSX, JSX, ArkTS, Python, Go,
Rust, Java, C, C++, C#, Razor/Blazor, PHP, Ruby, Swift, Kotlin, Dart, Svelte,
Vue, Astro, Liquid, Pascal/Delphi, Scala, Lua, Luau, Objective-C, R, Solidity,
Vyper, Move, Cairo, Sway, Fe, Nix, Salesforce Apex, Bash, HTML, Visualforce,
Aura, YAML, Twig, XML, properties files, CFML, CFScript, embedded CFQuery,
COBOL/copybooks, VB.NET, Erlang/OTP resources, and Terraform/OpenTofu.

Metal (`.metal`) and CUDA (`.cu`, `.cuh`) use the C++ extractor after
line-preserving syntax preprocessing. Razor, Astro, Svelte, Vue, Liquid, and
CFML use dedicated wrappers for their embedded languages. Terraform and
OpenTofu share the HCL grammar and one `terraform` graph language ID.

## Module map (mirrors the TS repo’s `src/`)

| Rust module | TS source | What it is |
|---|---|---|
| `src/lib.rs` + `src/codegraph.rs` | `src/index.ts` | Public API — the `CodeGraph` struct: `init`/`open`/`close`, `index_all`, `sync`, `search_nodes`, `get_callers`/`get_callees`, `get_impact_radius`, `build_context`, `watch`/`unwatch` |
| `src/types.rs`, `src/error.rs`, `src/utils.rs`, `src/directory.rs` | `src/types.ts`, `src/utils.ts` | `Node`/`Edge`/`NodeKind`/`EdgeKind`/`Language`, errors, hashing/path helpers, `.codegraph/` dir management |
| `src/db/` | `src/db/` | `DatabaseConnection`, `QueryBuilder` (prepared statements), `schema.sql` (embedded via `include_str!`), migrations — rusqlite (bundled SQLite + FTS5) |
| `src/extraction/` | `src/extraction/` | `ExtractionOrchestrator`, tree-sitter wrapper/helpers, per-language and embedded-language extractors, generated-file detection |
| `src/resolution/` | `src/resolution/` | `ReferenceResolver` orchestrating imports, receiver/type matching, dynamic-dispatch synthesis, cross-language bridges, and framework resolvers |
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
                                              # schema-v5+ indexes (current: v8)
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
- **Rule-backed source analysis** — `complexity` (and `diff`'s complexity
  deltas) re-parses on-disk sources with the compiled tree-sitter grammars.
  Rules cover Rust, TypeScript/JavaScript, ArkTS, Python, Go, Java, C, C++,
  C#, PHP, Kotlin, Swift, Ruby, R, Solidity, Vyper, Move, Cairo, Sway, Fe, Nix,
  CFML/CFScript/CFQuery, and Erlang. Other languages are counted in the
  report's `skipped` breakdown.
- **Value level needs IR** — `slice`/`taint --value-level` lower the
  engine's per-function dataflow IR by re-parsing on-disk sources anchored
  at the indexed byte offsets (schema-v5+ indexes; re-index pre-v5 projects
  to enable). IR lowering covers Rust, TypeScript/JavaScript, Python, and
  Go; `cfg`/`dataflow` use the same anchor pattern with their own rule
  tables. Without `--value-level` (or on pre-v5 indexes), `slice` and
  `taint` run over a call-graph dataflow oracle and say so in their
  `note`/`granularity` fields — function-level hops along call edges, never
  pretended value tracking.

## Key architectural deviations from the TS implementation

Collected from [`notes/*.md`](notes/); each module's notes file documents its own
deviations exhaustively. The big ones:

- **Tokio orchestration with synchronous kernels.** CLI and MCP binaries own one
  multi-thread runtime. File parsing and reference resolution use bounded
  `spawn_blocking` task sets and restore input order after completion-order
  processing; SQLite persistence remains serialized on its owning thread.
  Watcher and MCP worker threads borrow the same runtime handle, while blocking
  transports retain dedicated `std::thread`s.
- **Single SQLite backend.** The TS dual backend (better-sqlite3 native with a
  node-sqlite3-wasm fallback) collapses to rusqlite with bundled SQLite + FTS5.
  `codegraph status` still reports the backend string as `"native"` for output
  parity — there is no wasm slow path to fall back to.
- **Native tree-sitter grammars instead of wasm.** Grammar crates are compiled in
  (see `Cargo.toml`); `web-tree-sitter`'s async `Parser.init()` and the
  `dist/*.wasm` copy step disappear entirely. No runtime grammar loading.
- **Node-isms dropped.** `MemoryMonitor`, `processInBatches`, event-loop
  `debounce`/`throttle` helpers, and the parse worker-thread pool are gone or
  replaced by their Rust equivalents (the watcher implements its own debounce;
  Tokio's blocking pool runs synchronous parser jobs). The Node 25 hard-exit
  check is irrelevant and not ported.
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

## Verification

The repository's release gates are:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace
cargo test --workspace
```

Tests use temporary directories, real files, and real SQLite databases rather
than mocks. The June 6 test totals and row-level results remain recorded in the
historical parity report; they are not a claim about the current worktree.

## Parity with the TS implementation

The original row-level comparison remains in [`notes/parity.md`](notes/parity.md)
(2026-06-06). It is intentionally preserved as a reproducible historical
snapshot rather than rewritten after every upstream commit.

The July 9 parity wave closes the later upstream gaps that snapshot did not
cover: schema v8 reconciles the incompatible Rust/TypeScript schema-7 lineages
and enforces unique logical edges; indexing records extraction version, state,
and file accounting; resolver snapshots and synthesized-edge writes are bounded;
receiver inference handles scoped locals, typed parameters, and chained calls;
the expanded framework/dynamic-dispatch matrix is registered; `codegraph.json`,
`explore`, `node`, `daemon`, telemetry, upgrade, explicit version aliases, and the
Claude prompt hook are available. The Rust-only analysis/decompiler/GPU surfaces
remain additive rather than parity requirements.
