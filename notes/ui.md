# notes/ui.md — UI module port

## Files ported

- `src/ui/glyphs.ts` → `rust/src/ui/glyphs.rs` (faithful, incl. #168 doc comment)
- `src/ui/types.ts` → `rust/src/ui/types.rs`
- `src/ui/shimmer-progress.ts` + `src/ui/shimmer-worker.ts` → `rust/src/ui/shimmer_progress.rs`
  (the Node `worker_threads` worker is folded in as a named `std::thread`
  "shimmer-worker" fed over `crossbeam-channel`)
- `rust/src/ui/mod.rs` re-exports the public surface.

## Public API surface (for the wiring wave — CLI `index`/`sync` commands)

```rust
// glyphs
pub fn supports_unicode() -> bool;
pub struct Glyphs { pub ok/err/info/warn/bar_filled/bar_empty/rail/phase_done/
                    dash/h_line/tree_branch/tree_last/tree_pipe: &'static str,
                    pub spinner: &'static [&'static str] }
pub static UNICODE_GLYPHS: Glyphs;
pub static ASCII_GLYPHS: Glyphs;
pub fn get_glyphs() -> &'static Glyphs;       // cached after first call
pub fn _reset_glyphs_cache();                  // test-only

// shimmer progress
pub struct IndexProgress { pub phase: String, pub current: u64, pub total: u64 } // serde camelCase
pub struct ShimmerProgress;                    // owns the render thread
pub fn create_shimmer_progress() -> ShimmerProgress;
impl ShimmerProgress {
    pub fn on_progress(&mut self, progress: &IndexProgress); // mirrors onProgress
    pub fn stop(self);  // sends Stop, waits ≤2000ms for Stopped ack, joins; detaches on timeout
}

// message types (serde tag = "type": "update" / "finish-phase" / "stop" / "stopped")
pub enum ShimmerWorkerMessage { Update { phase, phase_name, percent: i32, count: u64 }, FinishPhase, Stop }
pub enum ShimmerMainMessage { Stopped }
```

Env vars honored exactly: `CODEGRAPH_ASCII=1`, `CODEGRAPH_UNICODE=1`, `TERM=linux`.

## Deviations / dropped Node-isms

- **Worker thread → std::thread.** `worker_threads` + `postMessage` becomes a
  `std::thread` with two `crossbeam-channel` unbounded channels. The message
  enums keep serde derives matching the TS discriminated-union JSON shape for
  parity, though they only travel in-process now.
- **`fs.writeSync(1, ...)` → locked `stdout().write_all` + flush.** Node needed
  the raw fd-1 syscall to bypass the main-thread event-loop proxy; in Rust the
  render thread's direct write is already a plain syscall. Behavior (animation
  keeps running while the indexing thread is blocked in SQLite) is preserved.
  ANSI mojibake guard (#168) is preserved via the same ASCII glyph fallback.
- **`setInterval(render, 50)` → `recv_timeout` deadline loop** ticking every
  50 ms; messages are handled as they arrive (same as the TS event loop).
- **`worker.terminate()` on stop-timeout → thread detach.** Rust can't kill a
  thread; on a 2 s ack timeout `stop()` drops the JoinHandle. If a
  `ShimmerProgress` is dropped without `stop()`, the channel disconnect ends
  the worker loop (no zombie thread).
- **`n.toLocaleString()` → local `format_number`** with en-US-style comma
  grouping (Node's default locale in the published CLI). Documented inline.
- **`sisteransi` / `fast-string-width`:** both are package.json dependencies
  but neither is imported anywhere in `src/` (verified by grep) — the worker
  already inlines its escape strings (`\r`, `\x1b[K`, `\x1b[0m`, `\x1b[2m`,
  `\x1b[32m`, `\x1b[1m`, `\x1b[38;2;r;g;bm`). Ported those constants verbatim;
  **no string-width calculation exists in the TS UI**, so none was implemented.
- `Math.round` vs `f64::round` (half-away-from-zero vs JS half-up): all
  rounded values here (percent, lerp color channels, bar fill) are
  non-negative, where the two agree.
- TS `glyphs.test.ts` fakes `process.platform` via `Object.defineProperty`;
  Rust can't, so platform cases are `#[cfg(windows)]` / `#[cfg(not(windows))]`
  gated and the darwin+linux "default true" cases collapse into one
  not-windows test (the implementation has no darwin/linux split beyond TERM).
  Env-mutating tests serialize on a test-local mutex (parallel test threads).
- "ASCII and Unicode sets cover the same keys": key parity is enforced by the
  `Glyphs` struct type; the ported test asserts all fields are non-empty in
  both sets instead of comparing `Object.keys`.

## Integrator notes

- `IndexProgress` is declared locally in `shimmer_progress.rs` exactly as
  `shimmer-progress.ts` declares its own local interface. The TS public API
  (`src/index.ts`) re-exports a same-shaped `IndexProgress` from
  `src/extraction` — when the extraction port lands its own `IndexProgress`,
  the wiring wave should unify them (or re-export one).
- `ShimmerProgress::on_progress` takes `&mut self` (it tracks `last_phase`),
  so the CLI's progress callback needs `move`-captured mutable access (e.g.
  wrap in `Mutex` if shared across rayon workers).
- `cargo check` was clean for this module. While running tests, a transient
  compile error existed in `src/db/connection.rs` (other agent's in-flight
  port — `db::migrations` items missing); not caused by ui files.

## Extra assignment — foundation/security test ports

- `rust/tests/foundation_test.rs`: directory-layer cases of
  `__tests__/foundation.test.ts` (init creates dir/db/.gitignore with `*` +
  `!.gitignore`, double-init error matches /already initialized/i,
  isInitialized false→true, validateDirectory valid/invalid, uninitialize
  removes `.codegraph/`). "Initialized" is simulated at the directory layer
  (create_directory + empty `codegraph.db`) because `CodeGraph`/db are stubs.
- `rust/tests/security_test.rs`: FileLock suite (acquire/release with PID
  content, double-acquisition error "locked by another process", stale-lock
  takeover for dead PID 99999999, with_lock value/cleanup, release-on-panic
  via Drop, idempotent release), validateProjectPath sensitive-dir blocking
  (`/` + `/etc` gated `#[cfg(unix)]`; `C:\Windows` case-insensitivity gated
  `#[cfg(windows)]`; tempdir allowed), path-traversal prevention at the
  utils level, symlink resistance gated `#[cfg(unix)]`
  (is_path_within_root_real escape, remove_directory not following a
  symlinked `.codegraph`, list_directory_contents skipping symlinks), and the
  atomic-write tmp+rename simulation.
- Dropped: `withLockAsync` cases (no async runtime — sync `with_lock` covers
  the contract).

### Deferred cases — for later waves (NOT yet ported)

From `foundation.test.ts` → **db wave / public-API wave**:
- Opening Projects (`CodeGraph.openSync`, `getProjectRoot`, /not initialized/i)
- Database (getStats node/edge/file counts, dbSizeBytes, optimize, clear)
- Close/Destroy (destroy alias keeps `.codegraph/`)
- Graph Query Methods (getContext /not found/i; traverse/getCallGraph/
  getTypeHierarchy/findUsages empty results)
- Database Connection suite (initialize/isOpen, getSchemaVersion == 4,
  transaction returns value, open nonexistent → /not found/i)
- Query Builder suite (getNode/getNodesInFile/getOutgoingEdges/getFile/
  getFiles null/empty results)

From `security.test.ts` → **their module waves**:
- Path Traversal Prevention via `CodeGraph.getCode` (public-API wave)
- MCP Input Validation (ToolHandler: non-string/empty query rejection with
  "non-empty string", limit clamping/NaN/negative, output truncation with
  "... (output truncated)", #230 sensitive projectPath via handler →
  "sensitive system directory") (**MCP wave**)
- `isSourceFile` extension matrix (extraction wave)
- JSON.parse error boundaries: malformed `decorators`/`metadata`/`errors`
  columns → undefined, no crash (db wave — QueryBuilder)
- Symlink Cycle Detection in `scanDirectory` (cycle, valid link, broken link)
  (extraction wave)
- The "Session marker symlink resistance" suite mentioned in CLAUDE.md does
  **not** exist in the current `__tests__/` (grep found nothing) — nothing to
  port; flagging so the MCP wave doesn't hunt for it.
