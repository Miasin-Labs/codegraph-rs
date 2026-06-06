# MCP daemon + proxy port notes

Ported files (owner: mcp-daemon agent):

- `src/mcp/daemon-paths.ts` → `rust/src/mcp/daemon_paths.rs`
- `src/mcp/daemon.ts` → `rust/src/mcp/daemon.rs`
- `src/mcp/proxy.ts` → `rust/src/mcp/proxy.rs`
- `src/mcp/version.ts` → `rust/src/mcp/version.rs`
- Tests: `rust/tests/mcp_daemon_test.rs` (20 tests, all passing) + 10 in-module
  unit tests (`cargo test --lib mcp::`).

No files owned by other agents were modified (`mod.rs` already declared the
modules; untouched).

## Public API surface

### `mcp::version`
- `pub const CODEGRAPH_PACKAGE_VERSION: &str` (TS `CodeGraphPackageVersion`).
  **Deviation:** compile-time `env!("CARGO_PKG_VERSION")` instead of a runtime
  `package.json` read; the `"0.0.0-unknown"` sentinel is unreachable. Same
  rendezvous semantics (same-build always matches, cross-build mismatches).

### `mcp::daemon_paths`
- `pub const HOST_PPID_ENV: &str = "CODEGRAPH_HOST_PPID"` — **moved here** from
  TS `src/extraction/wasm-runtime-flags.ts` (the wasm re-exec machinery is
  Node-only and not ported; the env-var contract lives on, #277). Anyone
  re-execing/spawning an intermediate process must set it to the host pid.
- `pub fn get_daemon_socket_path(project_root: &Path) -> PathBuf` — sha256 of
  the lexically-resolved root (Node `path.resolve` parity via
  `utils::lexical_resolve`), first 16 hex chars; identical hashing to TS so TS
  and Rust daemons share paths. POSIX limit 100 → tmpdir fallback
  `codegraph-<hash>.sock`. On Windows returns `\\.\pipe\codegraph-<hash>`
  (computed for parity even though the Rust daemon doesn't listen there yet).
- `pub fn get_daemon_pid_path(project_root: &Path) -> PathBuf`
- `pub struct DaemonLockInfo { pid: i64, version: String, socket_path: String, started_at: i64 }`
  — serde camelCase; `encode_lock_info` is byte-identical to TS
  `JSON.stringify(info, null, 2) + '\n'` (verified by test).
- `pub fn encode_lock_info(&DaemonLockInfo) -> String`
- `pub fn decode_lock_info(&str) -> Option<DaemonLockInfo>` — faithful TS
  quirk preserved: a bare decimal (`"12345"`) is valid JSON, fails the field
  checks, and decodes to `None`; the legacy plain-pid branch fires only when
  JSON parsing fails (e.g. leading-zero `"00123"` → pid 123, version
  `"unknown"`). Number()-vs-Rust-parse edge: hex literals (`"0x10"`) decode in
  TS but not Rust — judged irrelevant (never written by any version we ship).

### `mcp::daemon`
- `pub const DEFAULT_IDLE_TIMEOUT_MS: u64 = 300_000`
- `pub const MAX_HELLO_LINE_BYTES: usize = 4096`
- `pub struct DaemonHello { codegraph, pid: u32, socket_path, protocol }` —
  serializes camelCase in TS key order:
  `{"codegraph":...,"pid":...,"socketPath":...,"protocol":1}`.
- `pub struct DaemonStartResult { socket_path: PathBuf, lock: DaemonLockInfo }`
- `pub enum AcquireResult { Acquired { pid_path, info }, Taken { existing: Option<DaemonLockInfo>, pid_path } }`
- `pub fn try_acquire_daemon_lock(&Path) -> crate::error::Result<AcquireResult>`
  — temp-file + `fs::hard_link` (atomic + exclusive), no empty-file window
  (must-fix 1). **Deviations:** returns `Result` where TS throws; the temp
  filename gains an in-process counter (`<pidPath>.<pid>.<n>.tmp` vs TS
  `<pidPath>.<pid>.tmp`) so threaded racers inside one process can't collide —
  unobservable (temp always unlinked).
- `pub fn clear_stale_daemon_lock(pid_path: &Path, expected_dead_pid: Option<i64>) -> bool`
  — compare-and-delete semantics identical to TS.
- `pub fn parse_idle_timeout_ms(Option<&str>) -> u64` /
  `pub fn resolve_idle_timeout_ms() -> u64` — reads
  `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`; `0` = never idle-exit; invalid/negative
  → default. (TS keeps the parser private; exposed here for tests.)
- **unix-only** (`#[cfg(unix)]`):
  - `pub trait DaemonSessionFactory: Send + Sync + 'static` — the seam over
    `MCPEngine`/`MCPSession`/`SocketTransport` (stubs when this was written):
    - `fn warm_up(&self, root: &Path)` — default no-op; called once on a
      background thread at start (TS `void engine.ensureInitialized(root)`).
    - `fn serve_connection(&self, stream: UnixStream, root: &Path)` — runs on a
      dedicated thread per connection, **hello already written by the daemon**;
      returning drops the client (refcount decrement).
    - `fn stop_engine(&self)` — default no-op; called once during shutdown.
  - `pub struct DaemonOptions { idle_timeout_ms: Option<u64>, register_signal_handlers: bool }`
    (`Default`: env-resolved timeout, signals **on**). The signal opt-out is a
    test-only deviation — installing process-wide SIGINT/SIGTERM handlers
    inside a test harness would hijack it.
  - `pub struct Daemon` —
    `new(&Path, Arc<dyn DaemonSessionFactory>, DaemonOptions)`,
    `start() -> Result<DaemonStartResult>`, `stop(reason: &str)`,
    `handle_sigterm()` (#662: ignored while clients attached),
    `get_client_count()`, `get_socket_path()`, `is_stopped()`, `wait()`.
    Signals are flag-set in a handler and dispatched from the accept loop
    (25ms poll). **Deviation:** TS `stop()` ends in `process.exit(0)`; Rust
    `stop()` marks stopped and wakes `wait()` — **the bin wiring must
    `process::exit(0)` after `wait()` returns.** All daemon stderr strings are
    byte-identical to TS (`[CodeGraph daemon] Listening on … Idle timeout …ms.`,
    `Shutting down (…; clients=N).`, `Ignoring SIGTERM while N client(s) are
    attached; will exit via idle timeout.`).

### `mcp::proxy`
- Cross-platform: `DEFAULT_PPID_POLL_MS`, `DAEMON_RECONNECT_RETRY_MS` (100),
  `DAEMON_RECONNECT_MAX_RETRIES` (120), `DAEMON_PROXY_PENDING_MAX_LINES`
  (1000), `DAEMON_PROXY_PENDING_MAX_BYTES` (2 MiB),
  `enum ProxyOutcome { Proxied, FallbackNeeded }`, `struct ProxyResult { outcome, reason: Option<String> }`,
  `parse_poll_ms(Option<&str>) -> u64` (`CODEGRAPH_PPID_POLL_MS`; 0 = disabled),
  `parse_host_ppid(Option<&str>) -> Option<u32>` (rejects ≤1 and non-integers).
- **unix-only**:
  - `pub struct DaemonSocket { stream: UnixStream, tail: Vec<u8> }` — `tail` is
    bytes past the hello newline (TS `socket.unshift(tail)`); must be treated
    as already-read daemon output. `run_local_handshake_proxy` and `run_proxy`
    handle it internally.
  - `pub enum HelloConnectResult { Connected(DaemonSocket), VersionMismatch, Unavailable }`
  - `pub fn connect_with_hello(socket_path: &Path, expected_version: &str) -> HelloConnectResult`
    — 3s hello timeout, 4096-byte bound, exact-version check; stderr strings
    identical to TS.
  - `pub fn run_proxy(socket_path: &Path, expected_version: Option<&str>) -> ProxyResult`
    — `None` defaults to `CODEGRAPH_PACKAGE_VERSION`. Returns only on
    fallback; on a successful attach it pipes stdio and **exits the process**
    when either end closes (TS parity — TS's `'proxied'` resolve is also
    unreachable). Note: `run_proxy` is legacy in TS too (index.ts uses only
    `connectWithHello` + `runLocalHandshakeProxy`); ported for completeness.
  - `pub trait LocalToolExecutor: Send { fn execute(&mut self, name, &Value) -> Result<Value, String>; fn stop(&mut self) {} }`
  - `pub struct LocalHandshakeDeps { get_daemon_socket, make_local_executor, root, negotiate_protocol_version, server_info, server_instructions, static_tools }`
  - `pub fn run_local_handshake_proxy(deps: LocalHandshakeDeps) -> !` — never
    returns; exits via stdin-EOF / PPID-watchdog shutdown.
  - `pub fn spawn_ppid_watchdog_with(poll_ms, original_ppid, host_ppid, on_death)` —
    the #277 watchdog core, exposed as the test seam.

## Wiring contract for the MCP-server / CLI agent

The TS `proxy.ts` imported `SERVER_INFO` + `negotiatedProtocolVersion`
(session.ts), `SERVER_INSTRUCTIONS` (server-instructions.ts), `getStaticTools`
(tools.ts), and `MCPEngine` (engine.ts). Those files were stubs when this port
was written, so they are **injected**. The JSON-RPC shapes stay in proxy.rs and
are byte-shape-identical to TS (serde `preserve_order`; envelope key order
`jsonrpc, id, result|error`; an *absent* request id is omitted from the reply
(TS `undefined`), an explicit `null` id is preserved; local tool failure →
`{"code":-32603,"message":…}`; initialize result key order `protocolVersion,
capabilities:{tools:{}}, serverInfo, instructions`; `tools/list` →
`{"tools":[…]}`; `resources/list` → `{"resources":[]}` (#621);
`resources/templates/list` → `{"resourceTemplates":[]}`; `prompts/list` →
`{"prompts":[]}`; `ping` → `{}`).

To reproduce TS `MCPServer.runProxyWithLocalHandshake` (src/mcp/index.ts:390):

- `get_daemon_socket`: closure that (1) probes
  `connect_with_hello(socket_path, CODEGRAPH_PACKAGE_VERSION)` — `Connected` →
  `Some(ds)`; `VersionMismatch` → `None` **definitively** (do NOT spawn/poll;
  TS serves in-process to never run stale-vs-new); `Unavailable` → (2)
  `try_acquire`-aware spawn of the detached daemon (re-invoke the CLI:
  `serve --mcp --path <root>` with env `CODEGRAPH_DAEMON_INTERNAL=1`, detached
  session, stdio → `.codegraph/daemon.log`), then (3) retry
  `connect_with_hello` on an interval until deadline → `Some`/`None`. (TS
  loops `connectWithHello` every 100ms up to ~12s inside `getDaemonSocket`.)
- `make_local_executor`: lazily construct `MCPEngine`, `ensure_initialized(root)`
  (swallow init errors — TS `.catch(() => {/* degraded */})`), then
  `execute(name, args)` → `engine.getToolHandler().execute(...)`; `stop()` →
  `engine.stop()`.
- `negotiate_protocol_version` / `server_info` / `server_instructions` /
  `static_tools`: from `session.rs` / `server_instructions.rs` / `tools.rs`
  once ported.
- Daemon side: implement `DaemonSessionFactory` over the shared engine —
  `warm_up` → `engine.ensure_initialized(root)`; `serve_connection` →
  `MCPSession::new(SocketTransport::new(stream), engine, explicit_project_path
  = root)` and run it to completion; `stop_engine` → `engine.stop()`. After
  `Daemon::start()`, the daemon-mode bin must `daemon.wait()` then
  `process::exit(0)`.
- `CODEGRAPH_NO_DAEMON` / `CODEGRAPH_DAEMON_INTERNAL` parsing and
  `resolveDaemonRoot` (realpath + `find_nearest_codegraph_root`) live in TS
  `src/mcp/index.ts` → `server.rs` (server agent's file), not here.

## Platform gates

Everything socket-shaped (`Daemon`, `DaemonSessionFactory`, `connect_with_hello`,
`run_proxy`, `run_local_handshake_proxy`, `DaemonSocket`, watchdogs) is
`#[cfg(unix)]`. **Windows named pipes are NOT implemented** (std has no
named-pipe listener/client; no windows-sys dependency in Cargo.toml). The
wiring must run Windows in direct in-process mode (as if `CODEGRAPH_NO_DAEMON=1`).
`daemon_paths` still computes the `\\.\pipe\codegraph-<hash>` name so a future
named-pipe implementation (or TS interop) keeps the same rendezvous. Lock
helpers (`try_acquire_daemon_lock` via `fs::hard_link`, `clear_stale_daemon_lock`)
and all parsers compile on both platforms.

## Known edges / deviations (beyond those marked above)

- `Daemon` accept loop polls (25ms) a non-blocking listener so it can observe
  stop + signal flags — TS relies on the event loop. Adds ≤25ms accept/SIGTERM
  latency; irrelevant against the ~600ms daemon spawn.
- In `run_local_handshake_proxy`, a local fallback tool call executes while
  holding the proxy state lock — intentional: mirrors single-threaded JS, where
  nothing else progresses during a local call.
- `attach_daemon_socket`: if `try_clone()` of a freshly-connected daemon socket
  fails (fd exhaustion) during the *reconnect* path, the proxy can park in
  `Connecting` without retrying. Vanishingly rare; noted for completeness.
- Hello-read deviations: none observable — same 3s budget, same
  `> MAX_HELLO_LINE_BYTES` bound, same error strings ("daemon hello not JSON:
  …", "daemon hello missing required fields", "daemon closed connection before
  hello", "timed out waiting for daemon hello", "daemon hello line exceeded
  size limit").
- Env vars honored with exact TS names/semantics:
  `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`, `CODEGRAPH_PPID_POLL_MS`,
  `CODEGRAPH_HOST_PPID`.

## Test split (for the orchestrator / remaining agents)

- `__tests__/mcp-daemon.test.ts` — daemon-side mechanics ported in
  `rust/tests/mcp_daemon_test.rs` (lock arbitration incl. an 8-way concurrent
  race, complete-record atomicity, versioned hello + wire shape, refcounting,
  idle timeout + disarm, SIGTERM-with-clients #662, stale socket/lock
  clearing, version-mismatch fallback, hello-tail preservation, malformed/
  oversized hello rejection, `run_proxy` fallback reasons). The TS suite's
  end-to-end specs (spawning the real `serve --mcp` binary; proxy reconnect
  after daemon SIGKILL #662; `CODEGRAPH_NO_DAEMON=1`) require the CLI +
  `MCPServer` wiring → re-validate in the integration task.
- `__tests__/mcp-ppid-watchdog.test.ts` — the watchdog core is covered here
  with a real SIGKILL'd child via `spawn_ppid_watchdog_with` (host-pid
  channel + reason string). The four-tier process-tree variant needs the
  built binary → integration task. On Linux validation remember `--init` in
  Docker (zombie reaping; see CLAUDE.md).
- `__tests__/mcp-debounce-env.test.ts` — targets `parseDebounceEnv` in
  `src/mcp/engine.ts` → **server agent** (engine.rs).
- `__tests__/mcp-catchup-gate.test.ts` — targets `ToolHandler.setCatchUpGate`
  in `src/mcp/tools.ts` → **server agent** (tools.rs).
