# MCP server architecture

CodeGraph uses `rmcp` 3.1.3 as its only MCP protocol implementation. The old
hand-written JSON-RPC transport and session were removed. Direct stdio,
daemon-backed Unix sockets, and daemon-failure fallback all construct the same
connection-local `CodeGraphService`.

## Ownership boundaries

| Module | Responsibility |
|---|---|
| `src/mcp/service.rs` | Connection-local project resolution, tool refresh, logging subscription, and tool execution orchestration. |
| `src/mcp/service/handler.rs` | `rmcp::ServerHandler`: capabilities, protocol negotiation, tools, logging level, roots change, and custom-method errors. |
| `src/mcp/service/wire.rs` | Typed conversion between CodeGraph tool values and rmcp models; progress forwarding; `spawn_blocking` boundary. |
| `src/mcp/engine.rs` | `!Send` CodeGraph/ToolHandler state. |
| `src/mcp/engine/handle.rs` | Cloneable serialized command handle over the dedicated engine thread. |
| `src/mcp/engine/project.rs` | Project open, watcher startup, and first-call catch-up sync. |
| `src/mcp/server.rs` | Direct/daemon/proxy mode selection only. |

rmcp owns initialization, request IDs, request/notification dispatch,
cancellation tokens, progress tokens, peer requests, response serialization,
and stdio/async-I/O framing. CodeGraph owns only product policy.

## Protocol surface

- Supported versions are `rmcp::model::ProtocolVersion::KNOWN_VERSIONS`; rmcp
  3.1.3 negotiates through `2025-11-25`.
- Capabilities: tools with `listChanged`, plus legacy MCP logging.
- Tool definitions preserve CodeGraph input/output schemas and advertise
  read-only, non-destructive, idempotent, closed-world annotations.
- `tools/list` is dynamic. Opening a project can gate the list to the three
  primary tools and emits `notifications/tools/list_changed` when a previously
  listed surface changes.
- `tools/call` runs blocking graph work on the dedicated engine thread. rmcp's
  request cancellation token is bridged to CodeGraph's cooperative cancel flag.
- `_meta.progressToken` is bridged to rmcp progress notifications during the
  first-call catch-up sync.
- Engine diagnostics remain on stderr and are also mirrored through rmcp
  `notifications/message`; `logging/setLevel` updates the per-connection floor.
- Resources and prompts are not advertised. rmcp's empty-list defaults remain
  probe-friendly for clients that call their list methods anyway.

## Project resolution

Resolution order is:

1. `--path` supplied when the server process starts.
2. `roots/list` when the client advertises the roots capability.
3. The server process working directory.

`notifications/roots/list_changed` re-arms unresolved root discovery. The
deprecated initialize `rootUri` field is not part of rmcp 3.1.3's typed model;
clients must use roots or configure `--path`. Root file URIs are strictly
percent-decoded as UTF-8, including spaces and non-ASCII project names.

## Tool result projection and output bounds

CodeGraph keeps both MCP result fields required by current hosts:

- `structuredContent`: the versioned JSON payload.
- `content[0].text`: compact JSON of that same payload for text-only hosts.

This is intentional compatibility, not a second protocol implementation.
`codegraph_explore` applies its adaptive 13/15/16 KiB budget to the complete
serialized payload. Relationships are capped per kind, additional-file symbol
lists are capped, and lower-value metadata/evidence is shed before serialization
can cross the host's 50 KiB inline limit. `CODEGRAPH_MAX_OUTPUT_CHARS` may lower,
but never raise, the adaptive Explore ceiling.

## Runtime modes

- **Direct:** `rmcp::serve_server(CodeGraphService, rmcp::transport::stdio())`.
- **Daemon connection:** the daemon consumes its private hello line, enters the
  captured Tokio runtime, converts the accepted socket to
  `tokio::net::UnixStream`, then runs the same rmcp server.
- **Proxy:** version-checks the daemon hello and copies bytes between stdio and
  the socket. It never parses MCP.
- **Fallback:** if daemon acquisition times out or the daemon version differs,
  the launcher starts the direct rmcp server; no local proxy handler exists.

## Environment variables

- `CODEGRAPH_NO_DAEMON`
- `CODEGRAPH_DAEMON_INTERNAL`
- `CODEGRAPH_DAEMON_IDLE_TIMEOUT_MS`
- `CODEGRAPH_MCP_DEBUG`
- `CODEGRAPH_WATCH_DEBOUNCE_MS`
- `CODEGRAPH_PPID_POLL_MS`
- `CODEGRAPH_HOST_PPID`
- `CODEGRAPH_NO_WATCH`
- `CODEGRAPH_MAX_OUTPUT_CHARS`

## Verification surfaces

- `tests/mcp_protocol_test.rs`: rmcp negotiation, annotations, errors,
  progress, cancellation, logging, roots refresh, dynamic tool lists, and
  version-mismatch fallback.
- `tests/mcp_server_test.rs`: real spawned direct and daemon processes,
  direct/daemon result equality, project resolution, output projection, and
  watcher/catch-up behavior.
- `tests/mcp_daemon_test.rs`: lock/hello/socket/refcount/idle/watchdog mechanics.
- `tests/mcp_tools_test.rs` and payload unit tests: output schemas and full
  serialized Explore caps.
