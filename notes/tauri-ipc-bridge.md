# Tauri IPC bridge resolver (upstream #878, issue #1543)

Port of `src/resolution/frameworks/tauri.ts` → `src/resolution/frameworks/tauri.rs`.

## What it fixes
In a Tauri app, `callers`/`impact` on every `#[tauri::command]` came back empty:
the frontend reaches Rust through a runtime IPC hop, so tree-sitter sees no call,
and tauri-specta mangles the name (`get_mcp_port` → `getMcpPort`) so a name guess
misses too. This resolver bridges the JS/TS callsite to the Rust handler.

## Coverage (matches the TS PR)
- Commands: typed `commands.fooBar()` + raw `invoke('foo_bar')` → `#[tauri::command] fn foo_bar`.
- Events: typed `events.fooBar.listen()` + raw `listen('foo-bar')`/`once(...)` → `#[derive(...Event)] struct FooBar`.
- tauri-specta name conversions (snake/camel for commands; Pascal/camel/kebab/snake for events),
  stacked attributes (`#[specta::specta]` + `#[tauri::command]`), bare `Event` derive,
  and `#[tauri_specta(event_name = "...")]` overrides.
- JS/TS callers only (confidence 0.7 commands, 0.6 events, resolvedBy=framework).

## Deviations from TS
1. Per-context `WeakMap` cache → per-instance `Mutex<Option<TauriIndex>>` on the
   resolver struct (same pattern as `react_native.rs`). Registry builds a fresh
   `TauriBridgeResolver::new()` per run.
2. The Rust `regex` crate has no backreferences: the wire-call regex matches any
   of the three quote styles on each side instead of a backref to the opener.
   Wire names never contain quotes, so this is exact in practice.

## Wiring
- Registered in `frameworks/mod.rs` (after Fabric, before CICS — matches the TS slot).
- `resolve()` runs via `detect_frameworks` + `resolver/engine.rs`.
- `extract()` runs via `extraction/orchestrator/parse.rs` (surfaces raw invoke/listen
  wire names as file-attributed `calls` refs).

## Tests
23 in-module unit tests in `tauri.rs` (ported from `__tests__/tauri-ipc-bridge.test.ts`
unit + extract cases). The three TS end-to-end tests that drive the full `CodeGraph`
pipeline through SQLite are covered at the resolver level here
(`raw_invoke_wire_reference_resolves_to_command` chains extract → resolve); a full
pipeline e2e belongs with the CLI/indexer suite.
