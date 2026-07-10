# CLI module port notes (`src/bin/codegraph.ts` → `rust/src/bin/codegraph.rs`)

Port of the commander CLI to clap (derive). Tests:
`rust/tests/cli_test.rs` (9, all passing — spawns the real binary via
`CARGO_BIN_EXE_codegraph`, real SQLite, tempdirs, no mocks).

Verification at handoff: `cargo check --all-targets` clean (zero warnings),
`cargo build --release` succeeds, `target/release/codegraph --help` lists all
15 subcommands, full `cargo test` green (417 lib tests + every integration
suite). Manual release-binary smoke: init / status / status --json / sync /
files (tree) / query / callers / callees / impact / affected / unlock /
serve (info) / `serve --mcp` (live JSON-RPC initialize round-trip) /
`install --print-config` (known + unknown id).

Files touched: ONLY `src/bin/codegraph.rs`, `tests/cli_test.rs`, and this
notes file. No foreign module files modified; `Cargo.toml`/`lib.rs` untouched.
The MCP wave's helper binary `src/bin/codegraph-mcp-server.rs` was KEPT (its
notes allowed delete-or-keep; keeping it avoids touching
`tests/mcp_server_test.rs`, which spawns it). Both binaries coexist;
`default-run = "codegraph"` already points at the CLI.

## Subcommand surface (matches the TS `program.command(...)` registrations)

`init [path]`, `uninit [path]`, `index [path]`, `sync [path]`,
`status [path]`, `query <search>`, `files`, `serve`, `unlock [path]`,
`callers <symbol>`, `callees <symbol>`, `impact <symbol>`,
`affected [files...]`, `install`, `uninstall` — declared in TS registration
order so `--help` lists them identically. All flag names, shorts, defaults,
and description strings are verbatim from the TS source.

NOTE: the assignment mentioned a `context` subcommand; the TS CLI's header
comment lists one, but **no `program.command('context')` exists in
`src/bin/codegraph.ts`** (verified by grep — the header comment is stale
upstream). Ported faithfully: no `context` subcommand. `callers`/`callees`/
`impact`/`uninstall` (absent from the assignment list but present in TS) ARE
ported.

## Behavior parity highlights

- **Bare invocation** (`codegraph` with no args) runs the interactive
  installer (TS `process.argv.length === 2` check), error path
  `Installation failed: {msg}` + exit 1.
- **`resolveProjectPath`**: `path.resolve(arg || cwd)` via
  `utils::lexical_resolve`, then walk-up over `ancestors()` checking
  `directory::is_initialized` (codegraph.db required), falling back to the
  original absolute path. `init` uses plain resolve (no walk-up), same as TS.
- **`status --json`** — byte-shape-identical to TS (#329): key order
  `initialized, version, projectPath, indexPath, lastIndexed, fileCount,
  nodeCount, edgeCount, dbSizeBytes, backend, journalMode, nodesByKind,
  languages, pendingChanges{added,modified,removed}, worktreeMismatch`;
  single line (serde_json `preserve_order` + `json!` literal order);
  `backend` is `"native"` (PORTING.md rule 12); `lastIndexed` is
  `new Date(ms).toISOString()` parity via a hand-rolled civil-from-days
  formatter (`YYYY-MM-DDTHH:MM:SS.mmmZ` — no chrono dependency).
  Uninitialized shape `{initialized:false, version, projectPath, indexPath,
  lastIndexed:null}` exact.
- **`serve --mcp`** = `MCPServer::new(resolved_path).start()` (blocks);
  `--path` resolves through `resolveProjectPath` (the Cursor cwd quirk —
  the installer injects `--path` into Cursor's args); `--no-watch` routes
  through the `CODEGRAPH_NO_WATCH=1` env chokepoint exactly like TS line
  1101. The non-`--mcp` info screen (stderr, ANSI, tool list) is
  byte-matched.
- **`init`** runs indexing by default; legacy `-i/--index` accepted as a
  no-op; `-v/--verbose` switches the shimmer renderer for the timestamped
  line logger (`createVerboseProgress` ported: phase lines, 5% step logs,
  scanning `% 1000` logs, `toFixed(1)` elapsed). Shimmer path prints the dim
  rail line first and adapts `extraction::IndexProgress` →
  `ui::IndexProgress` (`phase.as_str()`, u64 casts) per notes/ui.md.
  `offer_watch_fallback` is called where TS calls it (currently a stub in the
  installer module — see notes/installer.md deferred item 2; when its body
  lands, init/already-initialized paths light up automatically).
- **`printIndexResult` / `writeErrorLog`** ported in full: lock-failure
  surfacing before the file-count branches, error-code breakdown with the TS
  `codeLabels` table (insertion-ordered), `.codegraph/errors.log`
  write/cleanup, "The index is fully usable — only the failed files are
  missing." line.
- **`query`** mirrors the MCP generated-file down-rank
  (`extraction::is_generated_file`, stable sort). `--json` =
  `JSON.stringify(results, null, 2)` of the serde `SearchResult` (camelCase,
  absent optionals omitted).
- **`callers`/`callees`/`impact`** port the exact-match filter
  (`name == s || name.endsWith(".s") || name.endsWith("::s")`), the
  top-match fallback, the dedupe-by-node-id, and the `{symbol, callers|
  callees}` / `{symbol, depth, nodeCount, edgeCount, affected}` pretty-JSON
  shapes (`startLine` always emitted; the human `:line` suffix omitted when
  0, matching the TS falsy check).
- **`affected`** ports the default test-file regexes, the custom `--filter`
  glob→regex converter, the BFS over `get_file_dependents` with the
  `depth >= maxDepth` cutoff, `--stdin`, and the `{changedFiles,
  affectedTests, totalDependentsTraversed}` JSON. Exit 0 + optional info on
  empty input, before opening the graph.
- **`install`/`uninstall`** delegate to the installer module
  (`RunInstallerOptions`/`RunUninstallerOptions`); `--print-config` writes
  `target.print_config(loc)` raw to stdout with no fs writes and the exact
  `Unknown target "{id}". Known: {ids}.` error; `--location` validation
  message byte-matched; `--no-permissions`/`--yes` → `auto_allow`
  Some(false)/Some(true)/None exactly as the TS comment prescribes.
- **`uninit`** prompts `⚠ This will permanently delete all CodeGraph data.
  Continue? (y/N)` (yellow, glyph-aware) unless `-f`; runs
  `CodeGraph::open_sync` → `uninitialize()` →
  `sync::remove_git_sync_hook(root, &DEFAULT_SYNC_HOOKS)` with the
  `Removed git {hooks} sync hook(s)` message.
- **`unlock`** removes `.codegraph/codegraph.lock`; "not initialized" path
  returns exit 0 (TS `return`, not `process.exit(1)` — preserved).
- **Exit codes**: command failures exit 1 with the per-command catch message
  (`Failed to index:`, `Search failed:`, `callers failed:` …, byte-matched);
  clap parse errors are intercepted and exit **1** (commander parity — clap's
  default is 2); `--help` exits 0.
- **`--version`/`-V`** intercepted before clap to print the bare version
  string (commander parity; clap would prefix the binary name).
- Helpers ported: `formatNumber` (en-US comma grouping), `formatDuration`,
  `globToRegex` (files) and the affected-filter converter (both with the TS
  escape sets and replacement order), `parseInt` prefix parsing.

## Env vars honored

`CODEGRAPH_NO_WATCH` (set by `serve --no-watch`), and everything
`MCPServer::start()` reads downstream (`CODEGRAPH_NO_DAEMON`,
`CODEGRAPH_DAEMON_INTERNAL`, `CODEGRAPH_MCP_DEBUG`, …) — see
notes/mcp-server.md. No new env vars introduced.

## Deviations from TS (all deliberate, documented)

1. **commander → clap**: help *layout* differs (clap renderer); all
   user-visible description strings are verbatim. Usage-error exit code
   forced to 1 (clap default 2) for commander parity.
2. **@clack/prompts → plain stdout log lines** (same adaptation as the
   installer module): `intro`/`outro` print the bare message;
   `log.success/info/warn/error` print `{green ✓|blue ℹ|yellow ⚠|red ✗} msg`
   using the glyph set (ASCII fallback preserved, #168); `note(body, title)`
   prints `title:` + two-space-indented body. clack's box-drawing rails are
   not reproduced.
3. **Node-only constructs dropped** (N/A in a native binary):
   - `src/bin/node-version-check.ts` (Node 25 block / MIN_NODE_MAJOR banner,
     `CODEGRAPH_ALLOW_UNSAFE_NODE`) — no Node runtime to gate.
   - `relaunchWithWasmRuntimeFlagsIfNeeded` (`--liftoff-only` re-exec) —
     grammars are native crates, no V8/WASM.
   - `src/bin/uninstall.ts` (npm `preuninstall` hook) — npm packaging only.
   - `process.on('uncaughtException'/'unhandledRejection')` — no equivalent
     async error funnel; Rust errors flow through the per-command catch.
   - The lazy `loadCodeGraph()` import shim + its "Failed to load CodeGraph
     modules" banner — modules are statically linked.
4. **`status` human Backend line**: TS hardcodes
   `node:sqlite — built-in (full WAL)`; the port prints
   `native — built-in (full WAL)` (PORTING.md rule 12: report `"native"`).
   `--json` is exactly `"backend":"native"`.
5. **Map-iteration determinism**: `GraphStats` maps are `HashMap` (TS object
   insertion order is unrecoverable), so: `status` human breakdowns sort
   count-desc then key-asc; `status --json` `nodesByKind` keys and
   `languages` are alphabetical; `impact`'s merged `affected` list (TS Map
   BFS insertion order) sorts by (filePath, startLine, name); its human
   group-by-file follows that order. Key NAMES/shape are unchanged.
6. **Numeric-flag parsing**: `parseInt` prefix semantics are ported
   (`"12abc"` → 12), but NaN/negative garbage falls back to the flag default
   (TS would propagate NaN into `slice(0, NaN)` / SQL LIMIT with
   under-defined results). `--max-depth` parse failure behaves like the TS
   NaN (never prunes).
7. **`query --kind` with an unknown kind**: TS feeds the raw string to the
   SQL `kind IN (...)` filter → empty results; `NodeKind` is an enum here, so
   an unparseable kind short-circuits to the same empty result set.
8. `files` tree/flat/grouped sorting uses byte-wise `str::cmp` where TS used
   `localeCompare` (case ordering can differ for mixed-case names);
   `toFixed` is approximated half-away-from-zero (binary-representation edge
   cases differ immeasurably).
9. `cg.destroy()` (deprecated alias) call sites use `close()` — identical
   behavior, avoids the `#[deprecated]` warning.
10. `serve --mcp` blocks inside `start()` (Rust `MCPServer` is blocking; TS
    resolved and let the event loop run) — exit points identical, see
    notes/mcp-server.md.

## Test port map (tests/cli_test.rs — 9 tests)

- `__tests__/status-json.test.ts` (3/3, #329): library
  `get_last_indexed_at` None→recent-ms window; `status --json` uninitialized
  (`initialized:false, version, indexPath ∋ .codegraph, lastIndexed:null`);
  `status --json` indexed (version + indexPath + ISO `lastIndexed`
  round-tripped back into the index window via a days-from-civil parser —
  plus extra wire-shape pins: `backend:"native"`, journalMode,
  pendingChanges/nodesByKind/languages/worktreeMismatch keys).
- End-to-end smoke (assigned): `init` (fixture with `src/util.ts` +
  `src/__tests__/util.test.ts`) → `.codegraph/codegraph.db` exists, "Indexed"
  output → `query --json` finds `add` → `query` human header → `callers
  --json` includes `double` → `affected --quiet` echoes a changed test file →
  `affected --json` shape → `status --json` fileCount ≥ 2 → `uninit -f`
  removes `.codegraph/` → `status --json` back to uninitialized.
- `unlock` (stale lock removed + no-op second run), `--help` lists all 15
  subcommands, `--version` prints the bare version, unknown command exits 1
  (commander parity), `status` human "Not initialized" path.
- N/A (npm packaging / Node runtime — nothing to port, per assignment):
  `__tests__/node-version-check.test.ts`, `npm-sdk.test.ts`,
  `npm-shim.test.ts`, `prepare-release.test.ts`.

## Observed while validating (not CLI issues)

- `tests/import_resolver_test.rs::load_cpp_include_dirs_is_cached_per_project_root`
  failed ONCE during a parallel full-suite run and passes in isolation,
  single-threaded, and on re-runs — pre-existing flake in the resolver
  wave's caching test under full-suite parallel load; my files cannot
  influence it (bin + tests only). Flagging for the integration wave.
- A piped one-shot probe of `serve --mcp` that closes stdin immediately can
  race the response (stdin EOF → exit-on-close, TS parity). Keep stdin open
  until the response arrives (the test suites already do).

## For the integration/packaging wave

- TS reads `package.json` for the version; the port uses
  `env!("CARGO_PKG_VERSION")` (Cargo.toml currently 1.3.1 - keep in sync
  with the npm version at release time).
- Installer Step 6 (`initialize_local_project`) and `offer_watch_fallback`
  are still reduced/stubbed in the installer module (notes/installer.md
  items 1–2). The CLI calls `offer_watch_fallback` at the TS call sites, so
  wiring the stub completes both `codegraph init` and the installer flow.
- The `codegraph-mcp-server` helper binary can be deleted once
  `tests/mcp_server_test.rs` is repointed at `CARGO_BIN_EXE_codegraph`
  (`serve --mcp --path …` arg shape is supported by the CLI, including the
  daemon re-spawn invocation).
