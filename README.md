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
| `src/bin/codegraph.rs` | `src/bin/codegraph.ts` | CLI (clap; commander in TS) — same subcommands, same flags |
| `src/ui/` | `src/ui/` | Terminal UI: shimmer progress, glyph fallbacks (ASCII/Unicode) |

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

`cargo test --all-targets`: **1,324 tests, 0 failures** (25 binaries):

- 417 unit tests in `src/` (`#[cfg(test)]`, alongside the code they cover)
- 907 integration tests in `tests/` — 22 suites ported from `../__tests__/*.test.ts`
  (extraction 289+3, frameworks 64/67/33, installer targets 80, db 49,
  codegraph API 42, MCP tools/server/daemon 41/32/20, resolution 30+29+6,
  graph 31, sync 26, context 23, security 15, CLI 9, foundation 8, git hooks 7,
  grammars 3)

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
