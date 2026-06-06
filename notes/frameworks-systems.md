# frameworks group 3 port notes — systems ecosystems (go / rust / cargo-workspace / swift / swift-objc)

Ported from `src/resolution/frameworks/{go,rust,cargo-workspace,swift,swift-objc}.ts`
into `rust/src/resolution/frameworks/{go,rust,cargo_workspace,swift,swift_objc}.rs`.

Tests: `rust/tests/frameworks_systems_test.rs` — 33 tests, all green
(`cargo test --test frameworks_systems_test`). `cargo check` clean for these
five files (zero warnings).

## Public API

```rust
// go.rs — TS `goResolver` (name "go", languages [go])
pub struct GoResolver;                       // unit struct, Default
impl FrameworkResolver for GoResolver { ... }

// rust.rs — TS `rustResolver` (name "rust", languages [rust])
pub struct RustResolver { /* workspace-map cache */ }
impl RustResolver { pub fn new() -> Self }   // + Default
impl FrameworkResolver for RustResolver { ... }

// cargo_workspace.rs
pub fn get_cargo_workspace_crate_map(context: &dyn ResolutionContext)
    -> HashMap<String, String>;              // crate-name + underscore alias -> member dir

// swift.rs — TS `swiftUIResolver` / `uikitResolver` / `vaporResolver`
pub struct SwiftUIResolver;                  // name "swiftui", languages [swift]
pub struct UIKitResolver;                    // name "uikit",   languages [swift]
pub struct VaporResolver;                    // name "vapor",   languages [swift]
// all unit structs, Default, impl FrameworkResolver

// swift_objc.rs — TS `swiftObjcBridgeResolver` (name "swift-objc-bridge",
// languages [swift, objc], claims_reference(name) == name.contains(':'))
pub struct SwiftObjcBridgeResolver { /* reverse-bridge map cache */ }
impl SwiftObjcBridgeResolver { pub fn new() -> Self }  // + Default
impl FrameworkResolver for SwiftObjcBridgeResolver { ... }
```

All `extract()` hooks are implemented (TS objects have `extract`), so they
always return `Some(..)` — `Some(FrameworkExtractionResult::default())` for
non-matching extensions (TS returned `{nodes: [], references: []}`).
`post_extract` is not implemented by any of these resolvers (None), matching TS.

Route node shapes/metadata are byte-identical to TS:
- id `route:{filePath}:{line}:{METHOD}:{path}`, kind `route`,
  name `"{METHOD} {path}"`, qualifiedName `{filePath}::route:{path}`,
  start/end line = match line, references with kind `references`, column 0.
- swift component/class ids: `view:` / `app:` / `viewcontroller:` / `uiview:`
  prefixed, qualifiedName `{filePath}::{name}`.
- Confidences preserved exactly: go 0.8/0.8/0.75/0.7; rust 0.8/0.8/0.7 and
  module 0.95 (workspace) / 0.6 (local); swiftui 0.85/0.85/0.7;
  uikit 0.85/0.8/0.85/0.8; vapor 0.85/0.75/0.8; swift-objc bridge 0.6.
  All `resolved_by: ResolvedBy::Framework`.

## Registry integration (for the frameworks/mod.rs / resolver.rs owner)

- TS exports const singletons; here construct one instance per
  `ReferenceResolver`: `Box::new(GoResolver)`, `Box::new(RustResolver::new())`,
  `Box::new(SwiftUIResolver)`, `Box::new(UIKitResolver)`,
  `Box::new(VaporResolver)`, `Box::new(SwiftObjcBridgeResolver::new())`.
- **Create fresh `RustResolver`/`SwiftObjcBridgeResolver` instances per
  resolver lifetime (i.e. per index/rebuild)** — see cache note below.
- These two structs use `RefCell` caches, so they are `!Sync`. The TS
  resolution pipeline is single-threaded; if the Rust wiring ever resolves
  in parallel, swap `RefCell` for `Mutex` (trivial, no API change).

## Deviations (all documented inline too)

1. **WeakMap caches → per-instance `RefCell<HashMap<project_root, …>>`.**
   TS cached the cargo-workspace crate map (rust.ts) and the ObjC
   reverse-bridge map (swift-objc.ts) in module-level
   `WeakMap<ResolutionContext, …>`; `&dyn ResolutionContext` has no object
   identity, so the cache is per-resolver-instance keyed by
   `get_project_root()` (keeps the multi-project-daemon isolation property).
   Invalidate-on-reindex therefore depends on constructing a fresh resolver
   per `ReferenceResolver` (TS invalidated when the context object was
   recreated — same lifecycle if the registry is built per resolver).
2. **picomatch → globset** (cargo_workspace.rs): `GlobBuilder` with
   `literal_separator(true)` reproduces picomatch's `*`-doesn't-cross-`/`
   default. picomatch's `dot: false` is covered by the walk's existing
   `.`-child skip. Difference: a leading-`!` *negated* member pattern is not
   supported by globset (treated literally / build error → expands to no
   matches); real Cargo.tomls use `exclude`, not `!` members. An unparsable
   glob yields no matches instead of throwing.
3. **swift_objc_bridge.rs was an unported stub** owned by another agent at
   port time, so `swift_objc.rs` carries private faithful copies of the two
   bridge-math functions it needs (`bridge_swift_base_names_for_objc_selector`
   ← `swiftBaseNamesForObjcSelector`, `bridge_is_objc_exposed` ←
   `isObjcExposed`, plus `lower_first`). **When `swift_objc_bridge.rs` lands,
   replace the local copies with
   `use crate::resolution::swift_objc_bridge::{swift_base_names_for_objc_selector, is_objc_exposed};`**
   (the candidate-order contract: raw first keyword first, then `init` for
   `initWith*`, then preposition-stripped base, then `setX:`→`x` — order
   matters because resolve() takes the first hit).
4. **Byte vs UTF-16 offsets**: `end_column` and the actix 500-char chain
   window are byte-based (TS used UTF-16 units); the actix window clamps to
   a char boundary. Same policy as extraction-core. Line numbers unaffected.
5. JS truthiness on file reads preserved: empty `go.mod` / `Package.swift` /
   `Cargo.toml` contents are treated as absent (TS `if (!content)`).
   Vapor handler `split('.').pop()` empty-string falsiness preserved.
6. `declaration_source_window` uses `str::lines()` (vs TS `split(/\r?\n/)`):
   identical except a trailing empty line isn't represented — harmless, the
   window is only regex-probed for `@objc`/`@nonobjc`.
7. TS `Set` insertion order in `swiftBaseNamesForObjcSelector` reproduced
   with an order-preserving Vec + contains-check.

## Deferred tests (for the wiring/e2e task)

- `__tests__/gin-middleware-chain.test.ts` — needs `CodeGraph.init`/`indexAll`
  + the `gin-middleware-chain` synthesizer (`callback-synthesizer.ts`,
  resolution-core agent). Re-port once the public API lands: it asserts
  edges where `metadata.synthesizedBy = 'gin-middleware-chain'`, source
  `Next` (method), targets exactly {Logger, Recovery, getUser} (inline
  closures skipped), `metadata.via` = handler name, `metadata.registeredAt`
  matches `app.go:\d+`.
- `__tests__/frameworks-integration.test.ts` "Go gRPC stub→impl synthesis"
  (2 cases) — same e2e dependency; the synthesis under test lives in the
  callback synthesizer, not go.rs.
- `frameworks.test.ts` "FrameworkResolver.extract interface" +
  "getApplicableFrameworks" — belong to the frameworks/mod.rs owner.
- `__tests__/swift-objc-bridge.test.ts` — tests the bridge-math module
  itself; belongs to the `swift_objc_bridge.rs` owner.

## Nothing missing/broken found in done modules

`resolution/types.rs`, `strip_comments.rs`, `crate::types` all sufficed.
The only external dep used beyond those is `globset` + `regex` (both already
in Cargo.toml).
