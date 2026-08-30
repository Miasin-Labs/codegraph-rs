# rmcp migration record

## Status

CodeGraph now depends on the official Rust MCP SDK:

```toml
rmcp = { version = "3.1.3", default-features = false, features = ["server", "transport-io"] }
```

The former `src/mcp/session.rs` and `src/mcp/transport.rs` implementations were
removed. `CodeGraphService` is the sole MCP handler in direct mode, daemon mode,
and daemon-failure fallback. The proxy is a version-checked byte pipe only.

## Why migration was preferable

The old server duplicated lifecycle negotiation, JSON-RPC dispatch, request
IDs, server-initiated roots requests, cancellation tracking, progress, logging,
and response serialization. Keeping those synchronized with MCP releases was a
permanent compatibility burden. rmcp now owns those semantics and provides one
typed `ServerHandler` boundary.

The daemon did not block migration because it is not part of MCP. rmcp accepts
the daemon's `tokio::net::UnixStream` after CodeGraph consumes its private hello
line, exactly as it accepts stdio.

## Responsibility matrix

| Concern | Owner after migration |
|---|---|
| Protocol negotiation and known versions | rmcp |
| JSON-RPC message model and dispatch | rmcp |
| stdio and async stream framing | rmcp |
| request cancellation token lifecycle | rmcp |
| progress and peer notifications | rmcp |
| roots request IDs/timeouts | rmcp plus CodeGraph's 5-second policy timeout |
| tool definitions and execution | CodeGraph |
| dynamic tool-list policy | CodeGraph |
| daemon lock, hello, idle lifecycle | CodeGraph |
| host-compatible result projection | CodeGraph |
| complete Explore payload budget | CodeGraph |

## CodeGraph policies retained on top of rmcp

1. All tools are read-only, non-destructive, idempotent, and closed-world.
2. Tool lists are project-sensitive and emit `tools/list_changed` after a
   previously observed surface changes.
3. Graph work remains serialized through `EngineHandle` because CodeGraph and
   ToolHandler are intentionally `!Send`.
4. rmcp cancellation is bridged to the existing cooperative graph cancel flag.
5. Catch-up sync emits progress only when the caller supplied a progress token.
6. Engine logs remain on stderr and are mirrored through MCP logging for daemon
   clients.
7. `structuredContent` is mirrored as compact JSON in text content for hosts
   that only consume text.
8. Explore's 13/15/16 KiB adaptive budget applies to the full serialized
   payload, including relationships and additional-file metadata.

## Deliberate boundaries

- CodeGraph remains a tools-only stdio server; Streamable HTTP is not needed by
  the supported local hosts.
- Windows uses direct stdio until a named-pipe daemon transport is implemented.
- rmcp 3.1.3 does not expose the deprecated initialize `rootUri` field. Use the
  roots capability or pass `--path`; roots file URIs are strictly decoded by
  the CodeGraph adapter.
- Legacy logging and roots are deprecated in newer MCP drafts but remain
  enabled for clients negotiating the 2025 protocol family.
- Malformed stdio lines follow rmcp's transport behavior: they are skipped and
  the stream remains usable. CodeGraph does not add a second parser around it.
- The daemon hello is private CodeGraph framing and is consumed before rmcp;
  it is not an MCP extension.

## Output-size root cause and resolution

Before migration, Explore's adaptive budget constrained rendered source only.
Structured relationships and additional-file symbols were uncapped, allowing a
96+ KiB text projection and roughly 200 KiB wire response despite a nominal
16 KiB source budget. OpenCode truncates MCP text at 50 KiB / 2,000 lines, which
caused the rare CodeGraph-to-Read/Grep fallback.

The payload builder now:

- omits relationships/additional files for tiers whose human budget disables
  those sections;
- caps relationships per edge kind;
- caps symbols per additional file;
- serializes the whole payload against the adaptive ceiling;
- sheds additional files, relationships, whole source chunks, whole literal
  lines, continuation hints, and omission detail in that priority order;
- preserves all emitted source/literal evidence verbatim and marks `trimmed`.

On the 272-file rust-sdk index, a broad `maxFiles:20` Explore call now produces
about 10.6 KiB of text and 10.6 KiB of structured JSON, keeping the full rmcp
response well below OpenCode's host cap.

## Verification map

- `tests/mcp_protocol_test.rs`: negotiation, annotations, output schemas,
  errors, cancellation, progress, logging, roots refresh, tool-list change,
  and direct fallback.
- `tests/mcp_server_test.rs`: real direct/daemon process equality and project
  resolution.
- `tests/mcp_daemon_test.rs`: private daemon transport mechanics.
- `src/mcp/tools/explore/payload/tests.rs`: full serialized metadata cap.
- `tests/mcp_tools_test.rs`: Explore schemas, omissions, source integrity, and
  configured/adaptive budgets.
