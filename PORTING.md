# CodeGraph TypeScript → Rust porting conventions

> **STATUS: CURRENT THROUGH 1.3.1 - 2026-07-09.** The original 2026-06-06
> row-level comparison and full test run remain historical evidence in
> [`notes/parity.md`](notes/parity.md). The 1.3.1 parity wave reconciles schema
> v8, bounded indexing, current language/framework/dispatch coverage, receiver
> inference, project config, operator commands, and the Claude prompt hook.
> See [`README.md`](README.md) for the current surface and verification status.
> The conventions below remain the maintenance contract.

Read this BEFORE porting any module. The TS source of truth is `src/**/*.ts` in a checkout of colbymchenry/codegraph (originally `../src` when this crate lived inside that repo)
(relative to this `rust/` directory). Tests live in `../__tests__/`.

## Ground rules

1. **Mirror the TS file structure.** One `.ts` file → one `.rs` file with the
   snake_case name (`import-resolver.ts` → `import_resolver.rs`,
   `tree-sitter-helpers.ts` → `tree_sitter_helpers.rs`). Each module directory
   has a `mod.rs` that declares its files and re-exports the public surface
   (mirroring the TS `index.ts` re-exports).
2. **Port faithfully, not creatively.** Same algorithms, same constants, same
   edge-case handling, same error messages where user-visible. Keep TS doc
   comments as `///` rustdoc. Where Rust idiom forces a deviation (ownership,
   no exceptions), keep behavior identical.
3. **Naming:** camelCase → snake_case for fns/vars, PascalCase stays for
   types. TS classes → Rust structs with `impl` blocks. TS interfaces used as
   contracts (e.g. `AgentTarget`, language extractor specs) → Rust traits.
4. **Shared foundation (already ported — use, don't duplicate):**
   - `crate::types` — `Node`, `Edge`, `NodeKind`, `EdgeKind`, `Language`,
     `ExtractionResult`, `UnresolvedReference`, `Subgraph`, etc.
     All serde output is camelCase for JSON wire-parity with TS.
   - `crate::error` — `CodeGraphError`, `Result`, `log_warn`/`log_debug`/`log_error`.
   - `crate::utils` — `sha256_hex`, `normalize_path`, `clamp`,
     `validate_path_within_root`, `is_path_within_root[_real]`,
     `validate_project_path`, `FileLock`, `is_process_alive`, `lexical_resolve`.
   - `crate::directory` — `.codegraph/` dir management.
5. **One Tokio runtime at process boundaries.** CLI and MCP binaries create a
   multi-thread Tokio runtime. Parsing and reference-resolution APIs are async,
   but their synchronous tree-sitter and CPU kernels run in bounded
   `spawn_blocking` `JoinSet`s; SQLite writes stay serialized on the owning
   thread. Watcher and MCP worker threads borrow the process runtime handle
   instead of constructing nested runtimes. Other blocking transports may use
   dedicated `std::thread`s.
6. **Timestamps** are epoch milliseconds as `i64` (`Date.now()` parity):
   `std::time::SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64`.
7. **JSON wire-parity matters.** MCP tool responses, `--json` CLI output, and
   anything an agent parses must match the TS shape (camelCase keys, same
   field names, same omission of absent optionals). Use the serde-derived
   types from `crate::types` wherever possible.
8. **DO NOT edit:** `Cargo.toml`, `src/lib.rs`, `src/types.rs`, `src/error.rs`,
   `src/utils.rs`, `src/directory.rs`, or any module directory you don't own.
   If you're blocked on a missing dependency or a needed change to a shared
   file, write it to `notes/<your-module>.md` and work around it locally
   (e.g. a private helper) so you still compile.
9. **Tests:** port the matching `__tests__/<area>.test.ts` cases as
   `#[cfg(test)]` unit tests in-module, plus `tests/<area>_test.rs` for
   integration-style suites. Use `tempfile::tempdir()` (mirrors `mkdtempSync`),
   real files, real SQLite — **no mocking**, same as the TS suite.
   Platform-specific behavior: gate with `#[cfg(unix)]` / `#[cfg(windows)]`.
10. **Verify before finishing:** `cargo check` (and `cargo test -p codegraph
    <your tests>` if integration allows) must pass for the whole crate from
    `rust/`. Concurrent cargo invocations serialize on the target-dir lock —
    if it blocks, it's another port agent compiling; just wait.
11. **Env vars** (`CODEGRAPH_*`) keep their exact names and semantics.
12. **Node-only constructs:** drop `MemoryMonitor`/`processInBatches`/
    `debounce`/`throttle`-style event-loop helpers unless the consumer module
    actually needs them (the watcher implements its own debounce). The
    better-sqlite3 vs node-sqlite3-wasm dual backend collapses to rusqlite —
    keep the *reported* backend string in `status` output as `"native"`.

## tree-sitter specifics

- Grammars are native crates (see Cargo.toml), NOT wasm. `web-tree-sitter`'s
  async `Parser.init()` disappears; `tree_sitter::Parser::new()` +
  `set_language(&LANG.into())` instead.
- Modern grammar crates expose `LANGUAGE: LanguageFn`; convert with
  `tree_sitter::Language::new(crate_name::LANGUAGE)` or `.into()`.
  Older crates (e.g. tree-sitter-dart 0.2) may export `fn language() ->
  Language` tied to an older tree-sitter version — if the types clash, go
  through the raw pointer: declare the `extern "C" fn tree_sitter_dart()`
  symbol and `LanguageFn::from_raw`. Note it in `notes/`.
- `typescript` and `tsx` are two languages in `tree-sitter-typescript`
  (`LANGUAGE_TYPESCRIPT`, `LANGUAGE_TSX`); `javascript` covers `jsx`.
  `tree-sitter-php` exposes `LANGUAGE_PHP` (with embedded HTML) — use that.
- Node walking: TS code uses `node.namedChildren`, `childForFieldName`,
  `descendantsOfType`. Rust equivalents: `named_children(&mut cursor)`,
  `child_by_field_name`, manual cursor walks. Positions: tree-sitter rows are
  0-based; the TS extractors store 1-based `startLine` — preserve the +1.

## SQLite specifics

- `rusqlite` with `bundled` — verify FTS5 is available in a test
  (`CREATE VIRTUAL TABLE … USING fts5(…)`); if the bundled build lacks it,
  note in `notes/db.md` (Cargo.toml owner will switch features).
- Reuse `src/db/schema.sql` (copied verbatim into `rust/src/db/schema.sql`,
  embedded via `include_str!`). Keep prepared statements; use transactions
  for batch inserts like the TS QueryBuilder does.

## Done definition for a port task

- All files of the module ported; `mod.rs` re-exports match `index.ts`.
- `cargo check` clean (warnings OK if pre-existing elsewhere).
- Ported tests pass (`cargo test`).
- `notes/<module>.md` written: anything deferred, deviations, integration
  needs for the wiring task (public-API/CLI/MCP).
