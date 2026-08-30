# MCP daemon and proxy architecture

The daemon is an internal process-sharing optimization. It is not an MCP
transport exposed to clients and it does not implement MCP semantics.

## Process topology

```text
MCP host
  ↕ stdio bytes
launcher / proxy
  ↕ Unix socket bytes
detached daemon
  ↕ rmcp AsyncRead + AsyncWrite
CodeGraphService
  ↕ serialized engine commands
shared EngineHandle
```

The daemon owns one CodeGraph engine, SQLite connection set, watcher, and
catch-up gate. Each accepted client gets a fresh connection-local
`CodeGraphService`, while all services clone the same `EngineHandle`.

## Rendezvous contract

- Socket and PID paths are deterministic per canonical project root.
- Lock acquisition uses an atomic hard-link protocol and records PID, version,
  socket path, and start time.
- The daemon writes one bounded JSON hello line before rmcp sees the socket:
  `codegraph`, `pid`, `socketPath`, and private protocol version `1`.
- The proxy requires an exact CodeGraph package-version match. A mismatch is a
  definitive direct-mode fallback, never a stale-daemon attach.
- Bytes after the hello newline are preserved as the socket tail and forwarded
  before subsequent reads.

The hello and lockfile remain compatible with the pre-rmcp daemon contract;
everything after the hello is rmcp-owned MCP traffic.

## Cold start and fallback

The launcher first probes the existing socket. If unavailable, it spawns the
same executable with:

```text
serve --mcp --path <canonical-root>
CODEGRAPH_DAEMON_INTERNAL=1
```

The child is detached with `setsid`, stdin is null, and stdout/stderr append to
`.codegraph/daemon.log`. The launcher polls for up to 240 × 25 ms. If the
daemon still is not available, direct rmcp mode consumes the original stdio
stream. The proxy does not answer initialize locally and does not buffer or
interpret JSON-RPC; the operating-system stdin pipe is the bounded cold-start
buffer.

Once attached, proxy shutdown follows either stream closing. If the daemon is
killed, the proxy exits and the MCP host restarts the configured server. There
is no second in-proxy MCP implementation or semantic reconnect state machine.

## Async boundary

Daemon accepts occur on plain std threads. `tokio::net::UnixStream::from_std`
must run inside `tokio::runtime::Handle::block_on`; converting before entering
the runtime panics with “there is no reactor running” and leaks the daemon
client refcount. `tests/mcp_server_test.rs::daemon_mode_uses_rmcp_for_initialize_list_and_call`
locks this boundary.

## Lifecycle

- Client refcount disarms the idle timer on attach and re-arms it after the
  final disconnect.
- `SIGTERM` is ignored while clients are attached and honored while idle.
- Shutdown closes client sockets, stops the shared engine, deregisters the
  daemon, removes its socket/registry entry, and releases the verified PID lock
  last so a successor cannot be cleaned up by its predecessor.
- The stdio proxy and direct mode retain the parent-PID watchdog; the detached
  daemon intentionally does not.

## Platform scope

Unix uses the shared daemon. Windows currently runs the fully compliant direct
stdio rmcp server because named-pipe daemon transport is not implemented.

## Tests

`tests/mcp_daemon_test.rs` covers lock races, complete lock records, stale
cleanup, hello validation and tail preservation, version mismatch, client
refcount, idle shutdown, SIGTERM behavior, and parent-PID watchdog behavior.
`tests/mcp_server_test.rs` drives initialize, tools/list, and tools/call through
the real detached daemon and compares the results with direct mode.
