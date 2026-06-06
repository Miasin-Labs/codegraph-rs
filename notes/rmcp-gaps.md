# rmcp gap matrix — codegraph-rs MCP server vs MCP spec (rmcp 1.7.0 as reference)

Authoritative gap analysis between the hand-rolled MCP server in `src/mcp/` and the MCP
specification as embodied by the official Rust SDK (`/home/cole/RustProjects/forks/rust-sdk`,
workspace 1.7.0). Every claim was verified against both source trees (file:line cited where
load-bearing). TS parent: `/home/cole/WebstormProjects/forks/codegraph/src/mcp/`.

**Constraint honored throughout:** codegraph-rs intentionally does NOT depend on rmcp. The
hand-rolled transport mirrors the TS daemon/proxy architecture (unix-socket daemon, hello
handshake, PPID watchdog) and must keep working byte-compatibly with the TS on-disk/socket
contract (`daemon_paths.rs` hashes, `DaemonHello` key order, lockfile `JSON.stringify` bytes).
rmcp is used here only as the spec reference; adoption is a clearly-marked future option (§5).

Negotiated protocol ceiling: codegraph-rs (and TS) top out at **2025-06-18**; rmcp at
**2025-11-25**. Rows are judged against the versions codegraph actually negotiates
(2024-11-05 / 2025-03-26 / 2025-06-18) unless noted.

`[upstream gap too]` = the TS parent has the same gap; fixing it in Rust exceeds TS and the
divergence must be documented in `notes/parity.md`.

## 1. Gap matrix

### Lifecycle

| Feature | Spec status | rmcp | codegraph-rs (TS parent) | Verdict |
|---|---|---|---|---|
| `initialize` request → `protocolVersion` + `capabilities` + `serverInfo` (+`instructions`) | MUST | `InitializeResult` (`model.rs`), handshake in `service/server.rs` | Full; key order pinned to TS (`session.rs:339-351`); proxy answers identically from statics (`proxy.rs:835-852`) | OK |
| Version negotiation: echo client version if known, else respond with server's version, never error | MUST | `negotiate_protocol_version` (`service/server.rs`) — never errors | `negotiated_protocol_version` (`session.rs`): echoes any of the 3 known strings, else `2025-06-18`; identical rule, identical to TS | OK |
| Protocol 2025-11-25 support | optional | `ProtocolVersion::V_2025_11_25 = LATEST` | Capped at 2025-06-18, in lockstep with TS | SKIP (see §4.3) |
| `notifications/initialized` tolerated, never answered | MUST | Dispatched to `on_initialized` (`InitializedNotificationMethod = "notifications/initialized"`) | Dispatch matches the **bare string `"initialized"`** (`session.rs:249`); the spec method `notifications/initialized` falls to the default arm and is only *accidentally* tolerated (notification ⇒ no `-32601` emitted). Observable behavior is compliant, but the dedicated no-op arm is dead code for every spec client `[upstream gap too]` (TS `session.ts:140` same) | **MUST-FIX** (trivial) |
| `ping` → `{}`, including before `initialize` | MUST | Pre-init pings answered in the handshake loop (`serve_server_with_ct_inner`); `EmptyResult {}` | `session.rs:263-267`: handled at any time (no init gate); proxy forwards/answers in all states | OK |
| Requests arriving before `initialize` | client SHOULD NOT; server behavior unspecified | rmcp aborts setup if first message isn't `initialize`/`ping` | Tolerated; engine init is lazily retried (`retry_init_if_needed`) — strictly more lenient than rmcp, valid | OK |
| `instructions` in initialize result | optional | `InitializeResult.instructions` | `SERVER_INSTRUCTIONS`, byte-identical to TS (tested) | OK |
| `serverInfo` `title`/`description`/`icons`/`websiteUrl` | optional (2025-06-18+) | `Implementation` has all | `{name, version}` only; key order byte-pinned by a proxy-parity test | SKIP (see §4.6) |
| Shutdown semantics | no protocol method | stdin EOF → drain → close | stdin EOF → `process::exit(0)` (stdio); daemon SIGTERM/idle logic | OK |

### JSON-RPC layer

| Feature | Spec status | rmcp | codegraph-rs (TS parent) | Verdict |
|---|---|---|---|---|
| `"jsonrpc": "2.0"` literal enforced | MUST | `JsonRpcVersion2_0` const-string type | `is_valid` check; violators → `-32600` `id:null` (`transport.rs:271`) | OK |
| Ids string-or-int, echoed verbatim; result XOR error | MUST | `RequestId = NumberOrString` (rejects null/float) | Any JSON id echoed verbatim; explicit `id:null` treated as a request (TS `'id' in message` parity) — tolerant superset; result/error exclusivity holds | OK |
| Parse error: `-32700`, unreadable id, keep stream alive | MUST | `id` **omitted** (2025-11-25 §Error Responses), recovery tested | `id: null` (`transport.rs:235-241`), stream continues. `null` is the JSON-RPC 2.0 rule and correct for ≤2025-06-18; revisit only if 2025-11-25 is adopted | OK |
| `-32600` for valid JSON that isn't a JSON-RPC object | MUST | `invalid_request` paths | `"Invalid Request: not a valid JSON-RPC 2.0 message"`, `id:null` | OK |
| Unknown request method → `-32601` (always respond to requests) | MUST | Default `on_custom_request` → `-32601` with method name | **Session**: `-32601 "Method not found: {method}"` (`session.rs:290-299`) — OK. **Proxy degraded mode**: `handle_locally` (`proxy.rs:580-618`) answers ONLY `tools/call` and `ping`; every other request is **silently dropped — no response at all** (host hangs until its own timeout). Non-JSON lines also dropped without `-32700` (`proxy.rs:583`). `[upstream gap too]` (TS `proxy.ts:198-214` identical) | **MUST-FIX** |
| Unknown notification → silently ignore, never respond | MUST | Default `on_custom_notification` no-op; codec-level skip for LSP-ish noise | Default arm ignores id-less messages | OK |
| JSON-RPC batching | MUST in 2024-11-05/2025-03-26; **removed** in 2025-06-18 | **Not supported** (no batch variant; vestigial dead code) — despite echoing the old versions | Not supported: array → one `-32600` `id:null`. Same posture as rmcp; identical to TS | SKIP (see §4.2) |
| stdio framing: one UTF-8 JSON per `\n` line, no embedded newlines, logs to stderr only | MUST | `JsonRpcMessageCodec`; CRLF tolerated; BOM stripped | `BufRead::lines()` + `trim()`; CRLF tolerated; per-line flush; all diagnostics on stderr | OK |
| UTF-8 BOM stripping (RFC 8259 §8.1) | MAY | Strips `\xEF\xBB\xBF` | `trim()` does not strip BOM → first line would `-32700` then recover | SKIP (see §4.8) |
| Server-initiated request ids unique per session | MUST | `AtomicU32Provider` numbers | `"{prefix}-{n}"` strings from `AtomicU64` (`cg-srv`/`cg-sock`) | OK |
| Error object: `code`, `message`, optional `data` | MUST | `ErrorData` | `data` key omitted when `None` (TS `JSON.stringify(undefined)` parity, byte-tested) | OK |

### Tools (the served feature)

| Feature | Spec status | rmcp | codegraph-rs (TS parent) | Verdict |
|---|---|---|---|---|
| Advertise `capabilities.tools` | MUST | `ToolsCapability` | `{"tools": {}}` (`session.rs:346`, `proxy.rs:844`) | OK |
| `tools/list`; tolerate optional `cursor` | MUST (pagination itself optional) | `paginated_result!`; `with_all_items` no-cursor mode is valid | Params ignored entirely (cursor tolerated-by-ignoring); no `nextCursor`; returns full list — spec-valid for 8 tools | OK |
| `tools/call`: unknown tool / bad params → JSON-RPC error; execution failure → in-band `isError:true` | MUST | Router: `-32602 "tool not found"`; `CallToolResult::error` | `-32602 "Missing tool name"` / `"Unknown tool: {name}"`; execution failures in-band `isError:true` — exactly rmcp's split. (Degraded-proxy `-32603` is engine-transport failure only, parity with TS) | OK |
| `inputSchema` root `type:"object"` | MUST | Schema helpers panic otherwise | All 8 tools `InputSchema { schema_type: "object", … }` (`tools.rs:397-412`) | OK |
| `outputSchema` + `structuredContent` | optional | `Json<T>`, `with_output_schema` | None — output is formatter-produced markdown | SKIP (see §4.4) |
| Tool `annotations` (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`) | optional | `ToolAnnotations` (`model/tool.rs`) | **None** (`ToolDefinition` is name/description/inputSchema only). All 8 codegraph tools are read-only/idempotent/closed-world — hosts use these hints for permission UX | **SHOULD-ADD** |
| Tool `title`/`icons`/`_meta` | optional | Present | None | SKIP (see §4.6) |
| `tools.listChanged` capability + `notifications/tools/list_changed` | optional | `enable_tool_list_changed` + `notify_tool_list_changed` | **Not advertised, never emitted** — yet the list IS dynamic: tiny-repo gating (<500 files keeps 3 tools) and the explore-budget suffix appear only after a project opens, and the local-handshake proxy answers every `tools/list` statically **forever** (`proxy.rs:853-856`), so daemon-mode clients never see the gated/budgeted list at all `[upstream gap too]` | **SHOULD-ADD** |
| Tool-level task negotiation (`execution.taskSupport`) | MUST at 2025-11-25; N/A at 2025-06-18 | Enforced (`handler/server.rs`) | N/A (version ceiling 2025-06-18) | SKIP (with §4.3) |

### Notifications & utilities

| Feature | Spec status | rmcp | codegraph-rs (TS parent) | Verdict |
|---|---|---|---|---|
| `notifications/cancelled`: tolerate (incl. late/unknown ids) | MUST | Intercepted in `serve_inner`; per-request `CancellationToken` | Tolerated (default arm, silently ignored) — MUST satisfied | OK |
| `notifications/cancelled`: actually stop processing, drop the response | SHOULD | `local_ct_pool` → `RequestContext.ct`; handlers observe cooperatively | **Never honored** — tool execution is synchronous/serial and uncancellable; a cancelled long `codegraph_explore`/first-call catch-up sync still runs to completion and its response is still sent `[upstream gap too]` | **SHOULD-ADD** |
| `notifications/progress` + `_meta.progressToken` | optional (only when caller sent a token) | Auto-injected tokens (`AtomicU32ProgressTokenProvider`); `Meta::get_progress_token`; `notify_progress` | `params._meta`/`progressToken` never read; `notifications/progress` never emitted. The first `tools/call` can block many seconds on the catch-up sync / initial index with zero feedback `[upstream gap too]` | **SHOULD-ADD** |
| Cancellation notification on our own request timeout | SHOULD | `RequestHandle::await_response` auto-sends `notifications/cancelled` reason `"request timeout"` | `transport.rs request()` timeout (e.g. `roots/list` 5000ms) just abandons the pending entry; no cancellation sent `[upstream gap too]` | **SHOULD-ADD** (minor) |
| `logging` capability, `logging/setLevel`, `notifications/message` | optional (if advertised, `setLevel` must work) | Full; default `set_level` `-32601` means advertising requires overriding | Not advertised; `logging/setLevel` → `-32601` (correct while unadvertised). All diagnostics stderr-only — which the host **cannot see in daemon mode** (daemon stderr → `.codegraph/daemon.log`) `[upstream gap too]` | **SHOULD-ADD** |
| `roots/list` (server→client) consumption | optional | `Peer::list_roots` | Implemented: lazy one-shot `init_from_roots` with 5s timeout, gated on advertised capability — matches rmcp's capability-gating discipline | OK |
| `notifications/roots/list_changed` | optional | `on_roots_list_changed` hook | **Dropped** (default arm); `roots_attempted` one-shot latch means a root added after a failed resolution is never picked up `[upstream gap too]` | **SHOULD-ADD** (minor) |
| Server→client `ping` | optional | Available | Not sent; not needed (PPID watchdog + socket EOF cover liveness) | OK |

### Optional server features (tools-only server)

| Feature | Spec status | rmcp | codegraph-rs (TS parent) | Verdict |
|---|---|---|---|---|
| Resources (`list`/`templates/list`/`read`/`subscribe`/`unsubscribe`/`updated`) | optional | Full models; defaults: empty lists, `read` `-32601` | `resources/list` + `resources/templates/list` answered with empty lists **without advertising the capability** (#621 probe-friendliness for opencode/Codex); `read`/`subscribe` → `-32601`. Same shape as rmcp's defaults | SKIP (see §4.1) |
| Prompts (`list`/`get`) | optional | Full | `prompts/list` → `{"prompts":[]}` (#621); `get` → `-32601` | SKIP (§4.1) |
| `completion/complete` | optional | Default returns empty `CompleteResult` | `-32601` (capability not advertised, so compliant) | SKIP (§4.1) |
| Sampling (`sampling/createMessage`, server→client) | optional (client capability) | Full incl. SEP-1577 gating | Never used — extraction is deterministic-by-design, no LLM in the loop | SKIP (§4.1) |
| Elicitation (`elicitation/create`) | optional (client capability) | Full incl. URL mode | Never used — no interactive flows | SKIP (§4.1) |
| Tasks (SEP-1686) | optional, 2025-11-25 | Full | N/A at 2025-06-18 | SKIP (§4.3) |
| Custom methods / `experimental` / `extensions` capabilities | optional | `CustomRequest` → default `-32601` | Unknown requests → `-32601`; unknown notifications dropped — exactly rmcp's defaults | OK |

### Transports

| Feature | Spec status | rmcp | codegraph-rs (TS parent) | Verdict |
|---|---|---|---|---|
| stdio transport | one-of MUST | `transport-io` | Implemented (reader+dispatcher threads, serial handler — JS-event-loop analog) | OK |
| Streamable HTTP (+ session headers, Origin/Host validation, SSE resume) | optional transport | Full tower service | Not shipped. All four real hosts (Claude Code, Cursor, Codex CLI, opencode) launch codegraph over stdio; the unix-socket daemon is an internal transport behind the stdio proxy, not an MCP-exposed endpoint | SKIP (see §4.7) |
| Legacy HTTP+SSE / WebSocket | deprecated / absent | Not shipped either | Not shipped | OK |
| Windows daemon (named pipe) | not a spec item — TS parity gap | n/a | `#[cfg(unix)]`-gated; Windows always runs direct mode (spec-compliant standalone stdio). Documented-intentional port gap (`notes/mcp-daemon.md`), tracked separately | SKIP (§4.9) |

## 2. MUST-FIX (implement now)

1. **Proxy degraded mode must answer every request** — `src/mcp/proxy.rs::handle_locally`
   (~line 580). Today only `tools/call` and `ping` get responses in `Failed` state; any other
   request (e.g. `logging/setLevel`, a probe, a custom method) is silently dropped and the
   host hangs until its own timeout — violating the JSON-RPC/MCP MUST that every request
   receives exactly one response. Add a default arm: request with an id → `-32601`
   `"Method not found: {method}"`; unparseable line → `-32700` `"Parse error: invalid JSON"`
   with `id:null` (instead of `return`); notifications stay dropped. Mirror rmcp's default
   `ServerHandler::on_custom_request` (`McpError::method_not_found`) and
   `AsyncRwTransport::receive`'s parse-error recovery. Reuse the session's exact error
   strings/wire shape (`transport.rs::ErrorCodes`) for cross-mode consistency.
   `[upstream gap too]` — TS `proxy.ts::handleLocally` has the identical hole; document the
   divergence in `notes/parity.md`.

2. **Recognize the spec method name `notifications/initialized`** —
   `src/mcp/session.rs:249`. The no-op arm matches the bare legacy string `"initialized"`;
   every spec client's `notifications/initialized` falls through to the default arm and is
   only tolerated by accident (notification ⇒ no error emitted). Match both
   `"initialized" | "notifications/initialized"` in the same no-op arm so the handshake hook
   is real (and any future logic attached to it actually fires). Mirror rmcp's
   `InitializedNotificationMethod = "notifications/initialized"` (`model.rs`).
   `[upstream gap too]` — TS `session.ts:140` matches the bare string as well.

## 3. SHOULD-ADD (optional, materially useful for Claude Code / Cursor / Codex / opencode)

Ordered by value-for-effort:

1. **Tool annotations (`readOnlyHint` etc.)** — `src/mcp/tools.rs::ToolDefinition` +
   `tools()`. Add an optional `annotations` field (skip-if-none) and set
   `{"readOnlyHint": true, "idempotentHint": true, "openWorldHint": false}` on all 8 tools
   (and `destructiveHint: false`); hosts use these for permission UX and auto-approval.
   Mirror rmcp `ToolAnnotations` (`model/tool.rs`, camelCase serialization). Static-tools
   path in `proxy.rs` picks this up for free via `get_static_tools()`.

2. **`tools.listChanged`** — advertise `{"tools": {"listChanged": true}}` in
   `src/mcp/session.rs:346` + `proxy.rs:844`; emit `notifications/tools/list_changed`
   (via the existing-but-unused `transport.notify()`) from the session/engine after a
   project open changes the tool surface (tiny-repo gating, budget suffix). In
   `src/mcp/proxy.rs`, once the daemon is `Ready`, forward `tools/list` to the daemon
   instead of answering statically forever — this fixes the real staleness where
   daemon-mode clients never see the gated/budgeted list. Keep the static answer for
   `Connecting`/`Failed` (the cold-start fix is load-bearing). Mirror rmcp
   `ToolsCapability { list_changed }` + `Peer::notify_tool_list_changed`.

3. **Progress notifications for long first calls** — `src/mcp/session.rs::handle_tools_call`:
   read `params._meta.progressToken` (string or integer, per rmcp `Meta::get_progress_token`);
   when present, emit `notifications/progress` (`{progressToken, progress, total?, message}`)
   during the catch-up sync / initial index that can block the first tool call for many
   seconds. Plumb an emit-callback through `engine.rs` (`EngineCommand::Execute`) into the
   sync loop's existing per-file accounting. Mirror rmcp `ProgressNotificationParam`.
   Only emit when a token was sent — never unsolicited.

4. **Honor `notifications/cancelled` for long tool calls** — `src/mcp/session.rs` dispatch +
   `src/mcp/engine.rs`: track in-flight request ids; on `notifications/cancelled` set a
   per-request atomic cancel flag the engine checks between pipeline stages (catch-up sync,
   per-file explore assembly); when fired, stop work and suppress the response (send
   nothing, per spec). Tolerate unknown/late ids exactly as today. Mirror rmcp's
   `local_ct_pool` keyed by `RequestId` + cooperative `RequestContext.ct`
   (`src/service.rs`). Note rmcp's doc rule: `initialize` is never cancellable.

5. **`logging` capability** — `src/mcp/session.rs`: advertise `"logging": {}`, handle
   `logging/setLevel` (`SetLevelRequestParams { level }`, store per-session min-level,
   reply `{}`), and mirror the existing `[CodeGraph MCP]` stderr diagnostics (watcher
   status, auto-sync, catch-up) as `notifications/message`
   (`{level, logger: "codegraph", data}`). Highest value in daemon mode, where stderr goes
   to `.codegraph/daemon.log` and the host sees nothing. Mirror rmcp `LoggingLevel`
   (lowercase serde) + `LoggingMessageNotificationParam`; note rmcp treats an advertised
   `logging` without a working `setLevel` as non-conformant — implement both or neither.

6. **Handle `notifications/roots/list_changed`** — `src/mcp/session.rs` dispatch: when no
   project is resolved yet, reset the `roots_attempted` one-shot latch so the next
   `retry_init_if_needed` re-issues `roots/list` (covers a host adding a workspace folder
   after connect). Mirror rmcp `ServerHandler::on_roots_list_changed`.

7. **Send `notifications/cancelled` when our own request times out** —
   `src/mcp/transport.rs::request()` timeout path (today: `roots/list`, 5000ms): emit
   `notifications/cancelled {requestId, reason: "request timeout"}` before abandoning the
   pending entry. Mirror rmcp `RequestHandle::await_response` / `REQUEST_TIMEOUT_REASON`.

## Implemented divergences (2026-06-06 — all §2 MUST-FIX + all §3 SHOULD-ADD items landed)

Every item below EXCEEDS the TS parent (marked `// EXCEEDS TS:` at each source site;
cross-referenced in `notes/parity.md` "Intentional MCP divergences"). Wire shapes mirror
rmcp model types exactly (field names/casing); rmcp is NOT a dependency.

1. **Proxy degraded mode answers every request** (§2.1) — `proxy.rs::handle_locally`:
   unparseable line → `-32700` `"Parse error: invalid JSON"` with `id:null`; any other
   id-bearing request → `-32601` `"Method not found: {method}"`; valid JSON that isn't a
   JSON-RPC message → `-32600` `id:null`; `tools/list` (pending-drain path) → static list;
   `logging/setLevel` → `{}` ack (the local handshake advertises `logging`); `initialize`
   stays a no-op (already answered by `process_client_line`); notifications and responses
   to server-initiated requests stay dropped. Codes/strings reuse
   `transport.rs::ErrorCodes` byte-for-byte (mirrors rmcp default `on_custom_request` +
   `AsyncRwTransport::receive` recovery).
2. **`notifications/initialized` spec name** (§2.2) — `session.rs` dispatch arm matches
   `"initialized" | "notifications/initialized"` (rmcp `InitializedNotificationMethod`).
3. **Tool annotations** (§3.1) — `tools.rs::ToolDefinition.annotations`
   (skip-if-none) set on all 8 tools to `{readOnlyHint:true, destructiveHint:false,
   idempotentHint:true, openWorldHint:false}`; rmcp `ToolAnnotations` camelCase. The
   proxy's static tools pick it up via `get_static_tools()` for free.
4. **`tools.listChanged`** (§3.2) — `session.rs::server_capabilities()` returns
   `{"logging":{},"tools":{"listChanged":true}}` (used by both the session initialize and
   the proxy's local handshake via `LocalHandshakeDeps.server_capabilities`). The session
   remembers the serialized list it last served and emits param-less
   `notifications/tools/list_changed` after a tool call whose project open/catch-up
   changed it. The proxy forwards `tools/list` to the daemon once `Ready` (static answers
   kept for `Connecting`/`Failed` — cold-start fix preserved) and, after attaching, sends
   one `list_changed` nudge if any `tools/list` had been answered statically.
5. **Progress notifications** (§3.3) — `session.rs::handle_tools_call` reads
   `params._meta.progressToken` (string or integer; floats ignored, per rmcp
   `Meta::get_progress_token`) and builds a `ProgressEmitter` plumbed through
   `EngineCommand::Execute` into the `ToolHandler` `CallContext`; the catch-up-sync gate
   emits `notifications/progress {progressToken, progress, total?, message}` (monotonic
   counter, 100ms throttle, final emission sets `total`). Never emitted without a token.
6. **Cancellation honored** (§3.4) — `notifications/cancelled` is intercepted on the
   transport *reader* thread (`set_notification_interceptor`) because the serial
   dispatcher may be blocked inside the very call being cancelled; the session keeps an
   in-flight map keyed by serialized request id (rmcp `local_ct_pool` analog), sets the
   per-request `Arc<AtomicBool>`, the engine checks it between pipeline stages
   (post-catch-up-sync, pre-dispatch), and the session suppresses the response when set.
   Unknown/late ids tolerated as before; `initialize` is never registered (never
   cancellable). Granularity note: the catch-up `cg.sync()` itself is not abortable
   mid-flight (the orchestrator API has no abort signal on the sync path — owned by the
   extraction module); cancellation takes effect at the next stage boundary.
7. **`logging` capability** (§3.5) — advertised; `logging/setLevel` validates the rmcp
   `LoggingLevel` (lowercase), stores a per-session min rank (default `info`), replies
   `{}` (invalid level → `-32602`). `engine.rs::LogBroadcaster` mirrors every
   `[CodeGraph MCP]` diagnostic (watcher status, auto-sync, catch-up, open failures,
   roots fallbacks) as `notifications/message {level, logger:"codegraph", data}` to each
   subscribed session (weak transport refs, pruned when sessions close); stderr bytes
   unchanged. Both halves implemented (the "both or neither" rule).
8. **`notifications/roots/list_changed`** (§3.6) — while no project is resolved and no
   `--path` pinned one, the notification re-arms the one-shot `roots_attempted` latch
   (plus a `roots_refresh_requested` override for the recorded-cwd-hint case) so the next
   `retry_init_if_needed` re-issues `roots/list`.
9. **Timeout cancellation** (§3.7) — `transport.rs::request()` emits
   `notifications/cancelled {requestId, reason:"request timeout"}` (rmcp
   `CancelledNotificationParam` / `REQUEST_TIMEOUT_REASON`) before abandoning the pending
   entry on its 5000ms (default) timeout.

Tests: `tests/mcp_protocol_test.rs` (spawned real binary over stdio + a planted
wrong-version daemon socket for degraded-proxy coverage) plus transport unit tests
(`request_times_out_with_the_ts_error_string`, `notification_interceptor_consumes_on_the_reader_path`).

## 4. SKIP (deliberately not implementing)

1. **Resources / prompts / completion / sampling / elicitation** — codegraph is a tools-only
   code-intelligence server: no documents to expose (the graph IS the product, served
   through tools), no prompt templates, no prompt/resource arguments to complete, no LLM in
   the loop (extraction is deterministic-by-design), no interactive flows. The #621
   empty-list probe answers stay exactly as-is **without** advertising the capabilities —
   advertising `resources`/`prompts` would obligate `resources/read`/`prompts/get`, and
   rmcp's own defaults likewise serve empty lists without requiring the capability.
2. **JSON-RPC batching** — required only by the 2024-11-05/2025-03-26 specs and removed in
   2025-06-18. rmcp itself does not implement it while still echoing those old versions;
   none of the four real hosts sends batches. Matching rmcp's posture; a batch array gets
   one `-32600`, which keeps the stream alive.
3. **Protocol 2025-11-25 (+ tasks, taskSupport negotiation, error-id omission)** — stay in
   lockstep with the TS parent's 2025-06-18 ceiling. Adopting 2025-11-25 drags in the
   task-negotiation MUSTs for zero benefit (no host requests it today); bump together with
   TS when it moves.
4. **`outputSchema` / `structuredContent`** — tool output is agent-facing markdown produced
   by the formatter (staleness banners, flow sections); structuring it would duplicate the
   formatter and bloat every response. Revisit only if a host starts consuming
   `structuredContent`.
5. **Pagination cursors** — 8 tools, no resources/prompts; returning everything with no
   `nextCursor` is spec-valid (rmcp's `with_all_items` does the same).
6. **`title`/`description`/`icons`/`websiteUrl` on serverInfo and tools** — cosmetic;
   `serverInfo` key order is byte-pinned by the proxy-handshake parity test, so churn here
   costs parity for no host-visible gain.
7. **Streamable HTTP transport** — all four hosts launch codegraph over stdio; the
   unix-socket daemon behind the proxy is an internal transport whose hello/lockfile/socket
   contract must remain byte-compatible with TS. An HTTP listener is a separate product
   decision, not a spec gap.
8. **UTF-8 BOM stripping on stdio** — MAY per RFC 8259 §8.1; the `-32700`-then-recover path
   already keeps the stream alive, and no real host emits a BOM.
9. **Windows named-pipe daemon** — TS-parity gap, not an MCP-spec gap (Windows direct mode
   is a fully compliant stdio server). Documented-intentional and tracked in
   `notes/mcp-daemon.md`; out of scope for this pass.

## 5. Future option (clearly marked, NOT the fix)

**rmcp adoption** — rmcp 1.7.0 covers everything above (cancellation tokens, progress
plumbing, listChanged, logging, 2025-11-25) and its `conformance/` harness could drive
regression testing. It is NOT recommended now because: (a) the hand-rolled transport's
daemon/proxy architecture (hello handshake byte-parity, lockfile `JSON.stringify` bytes,
TS↔Rust socket rendezvous, PPID watchdog, local-handshake cold-start answers) has no rmcp
equivalent and would have to be re-plumbed around rmcp's tokio `Service` model; (b) the
codebase is deliberately thread-based (`!Send` engine, reader/dispatcher threads), and rmcp
is tokio-native; (c) byte-exact TS parity (key order, error strings) is tested and
load-bearing. If revisited: rmcp's `AsyncRwTransport` over the existing unix socket is the
plausible seam, with the hello line consumed before handing the stream to rmcp. A cheaper
intermediate: run rmcp's **conformance-client** scenarios against a thin streamable-HTTP
shim, or port its scenario assertions into `tests/mcp_server_test.rs`.

## 6. Verification notes

- `session.rs:241-301` (dispatch), `session.rs:339-351` (initialize result),
  `proxy.rs:580-618` (`handle_locally`), `proxy.rs:825-873` (`process_client_line`),
  `transport.rs:226-272` (line handling, error codes), `tools.rs:397-412`
  (`ToolDefinition` — zero matches for `annotations`/`outputSchema`) — all read directly.
- TS parity of both MUST-FIX gaps confirmed: `session.ts:140` (`case 'initialized'`),
  `proxy.ts:198-214` (`handleLocally` drops unknown requests).
- Zero occurrences of `cursor`/`progressToken`/`listChanged` in `src/mcp/*.rs` (grep).
- rmcp claims taken from the SDK inventory (model.rs, service/server.rs, async_rw.rs,
  capabilities.rs, handler defaults) — consistent with the cited sources.

## Conformance results

**2026-06-06 — post-gap-fix verification against rmcp model types (rust-sdk fork @ `/home/cole/RustProjects/forks/rust-sdk`).**

The SDK's `conformance/` harness could not be used directly: `conformance-client`
(`conformance/src/bin/client.rs`) takes a server **URL** and drives scenarios exclusively
over `StreamableHttpClientTransport` — there is no stdio / child-process mode, so it cannot
target an external stdio server like `codegraph serve --mcp`. (Its sibling
`conformance-server` is likewise the SDK's own HTTP server for the *official* MCP
conformance framework, not a generic test driver.)

Instead, a hand-driven conformance smoke was run over stdio against the release binary
(`target/release/codegraph serve --mcp --path <fixture>`, fixture = 2-file TS project
indexed via `codegraph init`: 2 files / 6 nodes / 9 edges), and **every response line was
deserialized into the rmcp wire/model types** (`ServerJsonRpcMessage`, `InitializeResult`,
`EmptyObject`, `ListToolsResult`, `CallToolResult`, `JsonRpcError`, `ErrorData`) by a
throwaway Rust validator built against the fork's `crates/rmcp`.

| # | Step | Result | Checked against rmcp type | Notes |
|---|------|--------|---------------------------|-------|
| 1 | `initialize` (2025-06-18) | PASS | `ServerJsonRpcMessage` + `InitializeResult` | negotiated `2025-06-18`; serverInfo `codegraph/0.9.9`; `tools.listChanged=true`; instructions 4736 chars |
| 2 | `notifications/initialized` | PASS | — | no response emitted (correct for a notification) |
| 3 | `ping` | PASS | `EmptyObject` | `{"result":{}}` |
| 4 | `tools/list` | PASS | `ListToolsResult` | 3 tools (`codegraph_search`, `codegraph_node`, `codegraph_explore`); every `inputSchema.type == "object"` |
| 5 | `tools/call` (`codegraph_search`, `{"query":"triple"}`) | PASS | `CallToolResult` | 1 text content item; `isError` omitted on success |
| 6 | unknown method (`foo/bar`) | PASS | `JsonRpcError` | `-32601` METHOD_NOT_FOUND, id echoed |
| 7 | malformed JSON line | PASS | `ErrorData` | `-32700` PARSE_ERROR with `id:null` per JSON-RPC |
| 8 | `notifications/cancelled` (unknown requestId) | PASS | — | silently ignored, no response |
| 9 | `ping` after 6–8 | PASS | `EmptyObject` | server survives parse error + spurious cancel |

Additional observations: zero unsolicited stdout lines during the session; server exits
cleanly (`exit=0`) when stdin closes. **9/9 PASS.**

Same-day full-suite status: `cargo test --all-targets` 1340 passed / 0 failed across 24
test targets; `cargo clippy --all-targets` zero warnings; `cargo fmt --check` clean.
