# MCP server core port notes (transport / session / engine / server / instructions)

Ported files (owner: mcp-server agent):

- `src/mcp/server-instructions.ts` → `rust/src/mcp/server_instructions.rs`
- `src/mcp/transport.ts` → `rust/src/mcp/transport.rs`
- `src/mcp/session.ts` → `rust/src/mcp/session.rs`
- `src/mcp/engine.ts` → `rust/src/mcp/engine.rs`
- `src/mcp/index.ts` → `rust/src/mcp/server.rs` (+ `mcp/mod.rs` re-exports)
- Helper binary: `rust/src/bin/codegraph-mcp-server.rs` (see "Helper binary")
- Tests: `rust/tests/mcp_server_test.rs` (32, all passing) + 21 in-module unit
  tests (`cargo test --lib mcp::transport mcp::session mcp::engine mcp::server
  mcp::server_instructions`).

Verification at handoff: `cargo check --all-targets` clean (zero warnings
crate-wide), full `cargo test` green (417 lib tests + every integration
suite, 0 failed), `tests/mcp_server_test.rs` re-run 4× with no flakes.

## Foreign-file changes (recorded per the porting rules)

1. `src/codegraph.rs` — added `pub use crate::mcp::MCPServer;` to the
   re-export block and removed the "(port in flight)" comment. This exact
   change was *invited* by notes/codegraph-api.md ("Left for later waves …
   matching TS would be `pub use crate::mcp::MCPServer` at the entry").
   `lib.rs`'s existing `pub use codegraph::*` now surfaces `MCPServer` at the
   crate root, mirroring TS `src/index.ts`'s `export { MCPServer }`.
2. `src/bin/codegraph-mcp-server.rs` — NEW file (auto-discovered bin; no
   Cargo.toml edit needed — explicit `[[bin]]` + autobins coexist on edition
   2021). See "Helper binary" below. `src/bin/codegraph.rs` (CLI agent's
   file) was NOT touched.

No other foreign files modified.

## Public API surface

### `mcp::server_instructions`
- `pub const SERVER_INSTRUCTIONS: &str` — **byte-identical** to the TS
  template literal (single source of truth for agent-facing guidance, issue
  #529). Pinned by `tests/mcp_server_test.rs::
  server_instructions_are_byte_identical_to_the_ts_source`, which extracts
  the template from `../src/mcp/server-instructions.ts` and compares
  byte-for-byte (skips silently if the TS tree isn't checked out).

### `mcp::transport`
```rust
pub struct ErrorCodes;            // associated consts PARSE_ERROR -32700,
                                  // INVALID_REQUEST -32600, METHOD_NOT_FOUND -32601,
                                  // INVALID_PARAMS -32602, INTERNAL_ERROR -32603
pub struct IncomingMessage { pub id: Option<Value>, pub method: String, pub params: Option<Value> }
    // id None = key absent (notification); Some(Value::Null) = explicit null id
    // (TS `'id' in message`). is_request() = id.is_some().
pub type MessageHandler = Box<dyn FnMut(IncomingMessage) -> Result<(), String> + Send>;
pub trait JsonRpcTransport: Send + Sync {
    fn start(&self, handler: MessageHandler);   // idempotent
    fn stop(&self);
    fn send(&self, response: &Value);
    fn notify(&self, method: &str, params: Option<Value>);
    fn request(&self, method, params, timeout_ms: Option<u64>) -> Result<Value, String>;
        // None = TS default 5000; Err string == TS
        // `Timed out after {ms}ms waiting for "{method}" response`
    fn send_result(&self, id: &Value, result: Value);
    fn send_error(&self, id: &Value, code: i64, message: &str, data: Option<Value>);
        // pass Value::Null for the TS `null` id; `data: None` omits the key
}
pub struct StdioTransportOptions { pub exit_on_close: bool /*true*/, pub on_close: Option<Box<dyn FnOnce()+Send>> }
pub struct StdioTransport;        // ::new(opts); + wait_until_closed()
#[cfg(unix)] pub struct SocketTransport; // ::new(UnixStream) / ::with_prefix(stream, "cg-sock"),
                                  // on_close(Box<FnOnce>), write_raw(line), wait_until_closed()
```
- Wire shapes byte-identical to TS `JSON.stringify` key order (serde_json
  `preserve_order`): responses `{"jsonrpc","id","result"}` /
  `{"jsonrpc","id","error":{"code","message"[,"data"]}}`, notifications
  `{"jsonrpc","method"[,"params"]}`, server-initiated requests
  `{"jsonrpc","id","method"[,"params"]}` with ids `cg-srv-<n>` /
  `cg-sock-<n>`. Parse-error / invalid-request / internal-error strings
  byte-matched. Unit tests pin all of these.
- **Threading deviation (behavior-identical):** each transport runs a
  *reader* thread (parses lines; routes responses to pending
  server-initiated requests directly) + a *dispatcher* thread (runs the
  handler serially, like the JS event loop). This is what makes
  `roots/list`-mid-`tools/call` deadlock-free: the blocked handler's
  response bypasses the dispatch queue. Response-vs-later-request relative
  ordering can theoretically differ from TS; nothing observes it.
- `StdioTransport.stop()` cannot interrupt a blocking stdin read (TS closed
  readline); it flags stopped + rejects pending. Irrelevant in practice —
  direct mode exits the process on stdin EOF (`exit_on_close`, TS parity).
- TS `SocketTransport.stop()` does NOT fire close handlers (stopped flag set
  before destroy) — faithfully replicated.

### `mcp::session`
```rust
pub fn server_info() -> Value;             // {"name":"codegraph","version":<CARGO_PKG_VERSION>} (TS SERVER_INFO)
pub const PROTOCOL_VERSION: &str = "2025-06-18";
pub fn negotiated_protocol_version(client_version: Option<&Value>) -> String;
pub struct MCPSessionOptions { pub explicit_project_path: Option<String> }
pub struct MCPSession;                     // ::new(Arc<dyn JsonRpcTransport>, EngineHandle, opts)
                                           // start() / stop() / get_transport()
```
- Methods handled: initialize, initialized (no-op), tools/list, tools/call,
  ping, resources/list + resources/templates/list + prompts/list (#621 empty
  lists, not -32601), default → `Method not found: {method}`.
- initialize result key order `protocolVersion, capabilities:{tools:{}},
  serverInfo, instructions`; response sent BEFORE engine init (#172);
  rootUri > workspaceFolders[0] > --path (#196); roots/list one-shot with
  5000ms timeout and the exact TS fallback stderr strings.
- `file_uri_to_path`: hand-rolled WHATWG-ish parse (no `url` crate in the
  dependency set): `scheme://[host]/path` → percent-decoded pathname →
  win32 `/C:` strip → `lexical_resolve` (TS `path.resolve`). Fallback for
  unparseable URIs replicates TS `uri.replace(/^file:\/\/\/?/, '')`.
  Deviation: percent-decoding is lenient (malformed `%` left verbatim) where
  TS `decodeURIComponent` throws into the same fallback — coincides for real
  client URIs.
- Deviation (edge): a truthy non-string `params.name` in tools/call passes
  the TS falsy-guard and fails lookup; the "Unknown tool: X" stringification
  uses a JS-template-literal approximation (numbers/bools exact, objects →
  `[object Object]`).

### `mcp::engine`
```rust
pub struct MCPEngineOptions { pub watch: bool }      // Default: true
pub struct MCPEngine;                                // !Send — engine-thread confined
    // new(opts), set_project_path_hint, get_project_path, get_tool_handler,
    // has_default_code_graph, ensure_initialized(&str), retry_initialize_sync(&str), stop()
pub fn parse_debounce_env(raw: Option<&str>) -> Option<u64>;  // CODEGRAPH_WATCH_DEBOUNCE_MS, #403
pub struct EngineHandle;                             // Send+Sync+Clone — THE Rust-only seam
    // spawn(opts), set_project_path_hint, ensure_initialized_async(&str) -> Receiver<()>,
    // ensure_initialized(&str) /*blocking*/, retry_initialize_sync, has_default_code_graph,
    // get_project_path, get_tools, execute(name, Value) -> ToolResult, stop()
```
- **EngineHandle** is the Rust adaptation of "one engine, many sessions":
  `CodeGraph`/`ToolHandler` are `!Send`, so a dedicated engine thread owns
  the `MCPEngine` and processes commands strictly serially (the JS event
  loop's semantics). `ensure_initialized_async` = TS background
  `ensureInitialized` promise; the session stores the receiver as its
  `resolvePromise`.
- All engine stderr strings byte-matched to TS (`Failed to open project at
  …`, `File watcher disabled — … codegraph init\` to refresh.`, `File
  watcher debounce: …ms (CODEGRAPH_WATCH_DEBOUNCE_MS)`, `Auto-synced N
  file(s) in Mms`, `Auto-sync error: …`, `File watcher active — graph will
  auto-sync on changes`, `File watcher unavailable on this platform — …`,
  `Caught up N file(s) changed since last run`, `Catch-up sync failed: …`).
- **Catch-up gate deviation:** TS starts `cg.sync()` immediately
  (background) and gates the first tool call on its promise. The sync port
  pushes a one-shot *closure* (runs `cg.sync()` + logging) via
  `ToolHandler::set_catch_up_gate` — the reconcile work happens *at the
  first `execute()`* instead of at open. Same observable contract (first
  tool call never sees deleted-file rows; later calls don't re-wait —
  test-pinned); the one difference: if NO tool call ever happens the
  catch-up never runs (TS ran it anyway). The gate closure must not panic
  (it would propagate out of `execute`) — it swallows/logs its own errors.
- `parse_debounce_env` ports the TS Number()-semantics for every tested
  input incl. `1e3` → 1000; the 5 TS describe cases live as in-module unit
  tests (= the whole of `__tests__/mcp-debounce-env.test.ts`).

### `mcp::server`
```rust
pub struct MCPServer;   // ::new(Option<impl Into<String>>) — TS `new MCPServer(path?)`
    // start(&self) -> crate::error::Result<()>  — BLOCKS for the server's lifetime
    // stop(&self)                                — direct-mode stop + process::exit(0)
```
- `start()` decision order is TS-exact: `CODEGRAPH_DAEMON_INTERNAL` (truthy:
  set, ≠"0", ≠"false" case-insensitive) → daemon process;
  `CODEGRAPH_NO_DAEMON` → direct; no `.codegraph/` reachable
  (`resolve_daemon_root`: explicit `--path` ?? cwd → `find_nearest_codegraph_root`
  → canonicalize) → direct; else proxy-with-local-handshake.
- **Blocking deviation:** TS `start()` resolved and Node's event loop kept
  the process alive; Rust `start()` blocks (direct: signal/watchdog poll
  loop; daemon: `Daemon::wait()` then `process::exit(0)` per
  notes/mcp-daemon.md's wiring contract). Exit points are identical: stdin
  EOF (StdioTransport exit-on-close), SIGINT/SIGTERM → `stop()` → exit(0),
  PPID watchdog (#277, via `proxy::spawn_ppid_watchdog_with` + reused
  `parse_poll_ms`/`parse_host_ppid`/`HOST_PPID_ENV`; exact TS log line
  `[CodeGraph MCP] Parent process exited ({reason}); shutting down.`).
- `CODEGRAPH_MCP_DEBUG` gates the `Direct mode: {reason}.` stderr line (TS
  truthiness: any non-empty value).
- Daemon mode (unix): TS-exact lock arbitration loop (5 × 100ms takeover
  retries; `Another daemon (pid N) already holds the lock; exiting.` /
  `Could not acquire the daemon lock; exiting.`), then
  `Daemon::new(root, Arc<EngineSessionFactory>, DaemonOptions::default())`.
  `EngineSessionFactory` implements `DaemonSessionFactory` over a shared
  `EngineHandle`: `warm_up` → blocking `ensure_initialized` (runs on the
  daemon's background thread, TS `void ensureInitialized`);
  `serve_connection` → `SocketTransport` + `MCPSession` (explicit path =
  root) run to `wait_until_closed()`; `stop_engine` → `engine.stop()`.
- Proxy mode (unix): `run_proxy_with_local_handshake` wires
  `proxy::LocalHandshakeDeps` exactly as notes/mcp-daemon.md prescribed:
  probe `connect_with_hello(socket, CODEGRAPH_PACKAGE_VERSION)`;
  `VersionMismatch` → None (definitive, no polling); `Unavailable` →
  `spawn_detached_daemon` then 240 × 25ms reconnect polls; local executor =
  lazily-spawned `EngineHandle` with backgrounded `ensure_initialized`
  (init errors swallowed, TS `.catch(() => {})`), `execute` →
  `serde_json::to_value(ToolResult)`.
- `spawn_detached_daemon`: re-invokes `current_exe()` with
  `serve --mcp --path <root>`, env `CODEGRAPH_DAEMON_INTERNAL=1`, `setsid`
  via `pre_exec` (TS `detached: true`), stdout/stderr →
  `.codegraph/daemon.log` (append; falls back to null), stdin null. The TS
  "cannot resolve CLI script path to spawn the daemon" failure string is
  kept for the `current_exe()` error. Deviation: the Child handle is leaked
  (TS `unref()`); a loser daemon that exits stays a zombie until the proxy
  exits (Node auto-reaped via SIGCHLD) — harmless, noted.
- **Windows gate:** proxy/daemon modes are `#[cfg(unix)]` (no named-pipe
  listener — pre-existing gap per notes/mcp-daemon.md). On Windows `start()`
  always runs direct mode ("daemon mode unavailable on this platform"); the
  PPID watchdog is also unix-only (std has no ppid; TS `process.ppid` worked
  on Windows — flagging as a Windows-validation-pass item).

### `mcp::mod` re-exports (mirrors TS `src/mcp/index.ts` exports)
`MCPServer`, `StdioTransport`, `tools`, `ToolHandler`,
`#[cfg(unix)] Daemon`, `CODEGRAPH_PACKAGE_VERSION` — plus all submodules
remain `pub mod`.

## Helper binary (`codegraph-mcp-server`)

`rust/src/bin/codegraph-mcp-server.rs` is a ~40-line `main` that constructs
`MCPServer` the way the TS CLI's `serve --mcp` action does (`--path/-p` →
path.resolve parity via `lexical_resolve`; `--no-watch` →
`CODEGRAPH_NO_WATCH=1`, the same env chokepoint as TS
`src/bin/codegraph.ts:1101`; stray `serve`/`--mcp` tokens accepted+ignored
so the daemon's re-spawn invocation `<exe> serve --mcp --path <root>` works
against it too). It exists because the integration tests must spawn a real
stdio server (TS suites spawn `dist/bin/codegraph.js serve --mcp`) and the
clap CLI (`src/bin/codegraph.rs`, owned by the in-flight CLI wave) is still
a stub.

**CLI-wave handoff:** implement `serve --mcp` as
`MCPServer::new(resolved_path).start()` (exactly the helper's body), then
either delete `codegraph-mcp-server.rs` and point
`tests/mcp_server_test.rs`'s `spawn_server` at
`env!("CARGO_BIN_EXE_codegraph")`, or keep the helper — both binaries
coexist fine.

## Env vars honored (exact TS names/semantics)

`CODEGRAPH_NO_DAEMON`, `CODEGRAPH_DAEMON_INTERNAL`, `CODEGRAPH_MCP_DEBUG`,
`CODEGRAPH_WATCH_DEBOUNCE_MS` (#403), `CODEGRAPH_PPID_POLL_MS` +
`CODEGRAPH_HOST_PPID` (#277, parsed by the proxy module's shared fns),
`CODEGRAPH_NO_WATCH` (via `watch_disabled_reason`, routed by the
`--no-watch` flag).

## Test port map (tests/mcp_server_test.rs — 32 tests)

- `mcp-initialize.test.ts` (5/5): fast initialize without `.codegraph`,
  2025-11-25→2025-06-18 negotiation, 2024-11-05 negotiation-down,
  **response-before-watcher-log ordering** (#172; seq-tagged stdout/stderr
  reader threads = TS `tagStreams`), resources/prompts empty-list probes
  (#621). All spawn the helper binary with `CODEGRAPH_NO_DAEMON=1` (direct
  mode, like the TS suite).
- `mcp-roots.test.ts` (3/3): roots/list resolution with server-initiated
  string id (#196), actionable no-roots error (asserts the byte-matched "No
  CodeGraph project is loaded" + `projectPath` + `--path` + searched-dir
  basename, and that roots/list was never sent), explicit rootUri short-
  circuit. Spawned WITHOUT `NO_DAEMON` — cwd has no `.codegraph`, so the
  server takes the direct path organically (TS parity).
- `mcp-staleness-banner.test.ts` (5/5): banner / footer / drain-after-sync /
  status "### Pending sync:" / no-watcher-empty. Uses
  `sync::emit_watch_event_for_tests` (the TS `__emitWatchEventForTests`
  seam) under `NODE_ENV=test` (ENV_LOCK write + restoring guards), inert
  watcher, real debounce, registry key = `cg.get_project_root()`.
- `mcp-catchup-gate.test.ts` (5/5): Promise gate → one-shot closure gate
  (deviation 3 of notes/mcp-tools.md): awaited-before-first-call,
  one-shot (run count stays 1), deleted-file reconcile via a real
  `cg.sync()` gate, converge-to-0-files (`get_stats().file_count == 0`),
  failing gate doesn't break dispatch.
- `security.test.ts` "MCP Input Validation" (13/13): non-string/empty query
  rejections ("non-empty string"), valid query, limit clamp/NaN/negative,
  non-string symbol rejections across callers/callees/impact/node, explore
  missing query, **truncation** ("... (output truncated)") — the TS test
  fakes `searchNodes` with 3000 rows; no mocks here, so a real 150-function
  fixture with 120-char names drives the top-100 formatted results past the
  15000-char cap (same code path), #230 sensitive projectPath
  (`#[cfg(unix)]` /etc, `#[cfg(windows)]` C:\Windows — gated like
  `it.runIf`).
- Plus the SERVER_INSTRUCTIONS byte-parity test (see above).
- `mcp-debounce-env.test.ts` (5/5) lives as in-module unit tests in
  `engine.rs`; transport wire-shape/timeout/dispatch unit tests (9) in
  `transport.rs`; negotiation/server-info/uri unit tests (5) in
  `session.rs`; env-truthiness + resolve_daemon_root (2) in `server.rs`.
- Env discipline: shared `ENV_LOCK` RwLock + `EnvVarGuard` (same pattern as
  sync_test.rs/mcp_tools_test.rs); spawned children get
  CODEGRAPH_*/NODE_ENV/VITEST pinned via `Command::env_remove`/`env` so
  parallel in-process env mutation can't leak into them.

## Deferred / for later waves

- **E2e daemon + proxy specs** (`mcp-daemon.test.ts`'s spawning-the-real-
  binary describes: proxy reconnect after daemon SIGKILL #662,
  `CODEGRAPH_NO_DAEMON=1` end-to-end, four-tier `mcp-ppid-watchdog`
  process-tree case): already flagged "integration task" by
  notes/mcp-daemon.md; the wiring they exercise now exists
  (`EngineSessionFactory`, `run_proxy_with_local_handshake`,
  `spawn_detached_daemon`) — the helper binary supports the daemon re-spawn
  arg shape, so the integration wave can drive them.
- `concurrent-locking.test.ts` describe 3 (ToolHandler reuse spy) and
  `worktree-detection.test.ts` describe 2 — flagged for the MCP/integration
  wave by notes/codegraph-api.md; NOT in this task's assigned test list, so
  still open.
- **Task-description mismatch, resolved:** the assignment said session.ts
  includes "session marker files — the security.test.ts symlink-resistance
  behavior". Current TS `session.ts` (305 ln, read in full) has NO marker
  files, and notes/ui.md already confirmed the "Session marker symlink
  resistance" suite does not exist in `__tests__/` (grep found nothing).
  Nothing was ported; nothing exists to port. (CLAUDE.md's mention refers to
  a Windows-only failure of a suite that has since left the tree.)
- Windows: direct mode only (no named pipes — inherited gap), no PPID
  watchdog (no ppid API). Validate on the real VM per CLAUDE.md before
  claiming Windows support for `serve --mcp`.
