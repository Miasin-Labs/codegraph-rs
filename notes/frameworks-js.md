# frameworks group 1 — JS/TS ecosystem port notes

Ported from `src/resolution/frameworks/{express,nestjs,react,react-native,
fabric,expo-modules,svelte,vue}.ts` into
`rust/src/resolution/frameworks/{express,nestjs,react,react_native,fabric,
expo_modules,svelte,vue}.rs`. All implement
`crate::resolution::types::FrameworkResolver` (object-safe; see
notes/resolution-types.md). `frameworks/mod.rs` untouched (stitched by the
registry/index owner).

Tests: `rust/tests/frameworks_js_test.rs` — 67 passing
(`cargo test --test frameworks_js_test`). Whole-crate `cargo check` clean at
hand-off time.

## Public API (what the registry owner needs)

```rust
use crate::resolution::frameworks::express::ExpressResolver;          // name() = "express"
use crate::resolution::frameworks::nestjs::NestjsResolver;            // "nestjs"
use crate::resolution::frameworks::react::ReactResolver;              // "react"
use crate::resolution::frameworks::react_native::ReactNativeBridgeResolver; // "react-native-bridge"
use crate::resolution::frameworks::fabric::FabricViewResolver;        // "fabric-view"
use crate::resolution::frameworks::expo_modules::ExpoModulesResolver; // "expo-modules"
use crate::resolution::frameworks::svelte::SvelteResolver;            // "svelte"
use crate::resolution::frameworks::vue::VueResolver;                  // "vue"
```

- Seven are stateless unit structs — construct as `ExpressResolver` etc.
- `ReactNativeBridgeResolver` carries a cache: construct via
  `ReactNativeBridgeResolver::new()` (or `Default`). **See deviation below.**
- `languages()` (TS `languages?` field):
  - express/nestjs/react → `[Javascript, Typescript]` (nestjs declares
    `[Typescript, Javascript]` — order preserved from TS)
  - react-native-bridge → `[Javascript, Typescript, Tsx, Jsx]`
  - fabric-view → `[Typescript, Tsx, Objc, Java, Kotlin]`
  - expo-modules → `[Swift, Kotlin]`
  - svelte → `[Svelte]`
  - vue → `None` (TS object had no `languages` → applies to all languages)
- Hooks implemented per TS file:
  - `extract()`: ALL EIGHT return `Some(...)` (TS implemented `extract` on
    all of them). express/nestjs return `Some(empty)` for non-JS/TS file
    extensions (`/\.(m?js|tsx?|cjs)$/` gate) — same observable as TS.
  - `post_extract()`: ONLY `NestjsResolver` (RouterModule prefixing).
    Returns `Some(updates)`; `Some(vec![])` when nothing to do.
  - `claims_reference()`: react-native-bridge overrides it (returns false,
    with the TS comment); everyone else uses the trait default.

## Node/edge shape parity (kept byte-identical to TS)

- Route node ids: `route:{file}:{line}:{METHOD}:{path}` (express/nestjs),
  `route:{file}:{line}:{path}` (react-router), `route:{file}:{path}:{line}`
  (Next.js pages — note the swapped order, faithful to TS),
  `route:{file}:{path}:1` (svelte/vue file-based routes),
  `middleware:{file}:{name}:1`, `component:{file}:{name}:{line}`,
  `hook:{file}:{name}:{line}`, `fabric-component:{file}:{name}:{line}`,
  `fabric-prop:{file}:{name}:{line}`,
  `expo-module:{file}:{module}:{method}:{line}`.
- Route node names (`"GET /users"`, `"QUERY cat"`, `"WS chat:message"`,
  `"MESSAGE sum"`, `"EVENT user.created"`), qualified names
  (`{file}::{METHOD}:{path}`, `{file}::route:{path}`,
  `{file}::{Module}.{method}`, `{file}::NativeProps.{prop}`), docstrings and
  signatures (fabric/expo) are exact TS strings.
- References: express inline-arrow bodies emit `EdgeKind::Calls` from the
  route node; all named-handler references are `EdgeKind::References`.
- nestjs `post_extract` preserves `id` and `qualified_name` on updates
  (edge integrity + idempotency), mutates only `name`/`updated_at`.

There are **no sha-based ids in these eight files** — the task brief
mentioned react-render synthesis sha ids, but in the TS tree that lives in
`src/resolution/callback-synthesizer.ts` (`react-render`, `jsx-render`,
`fabric-native-impl`, `rn-event-channel` synthesizers), owned by the
resolution-core agent. `react.ts` itself has no `createHash` usage
(verified by grep). Nothing here calls `crate::utils::sha256_hex`.

## Deviations (all local, behavior-preserving unless noted)

1. **react_native cache scope.** TS cached built bridge maps in a
   per-context `WeakMap`. Rust has no weak-keyed map over `&dyn` contexts, so
   the cache is per-resolver-instance (`Mutex<Option<RnMaps>>`, built lazily
   on first `resolve()`). **Registry owner: construct a fresh
   `ReactNativeBridgeResolver` per resolution run / context** (the TS
   registry effectively had one context per resolver lifetime, so this is
   equivalent). `Mutex` (not `RefCell`) so the type stays Send+Sync.
2. **Insertion-ordered maps in nestjs post_extract.** TS `Map` →
   `Vec<(String, String)>` with linear `contains` checks (first-write-wins
   preserved at insert; module-file counts are small). Update order
   therefore matches TS insertion order.
3. **Byte vs UTF-16 offsets.** Line numbers count `\n` before a byte offset
   (identical to TS). Columns/`end_column` and the fixed-size scan windows
   (react: 500/400/300 chars; fabric: 80) are byte-based, clamped to char
   boundaries; TS used UTF-16 code units. Differs only when non-ASCII
   precedes a match on the same line / inside a window.
4. **Regex translation.** JS `\w`/`[\w$]` → explicit `[A-Za-z0-9_]`/
   `[A-Za-z0-9_$]` (Rust `\w` is Unicode); `/i` → `(?i)`; `[\s\S]*?` →
   `(?s)` + `.*?`; sticky `/y` regexes (nestjs methodNameAfter etc.) →
   `^`-anchored regexes applied to `&safe[i..]` slices; `g`-flag
   lastIndex-resumption loops → `captures_at`/`find_at` position loops.
   nestjs's per-call dynamic field regexes (`parseStringField(obj, name)`)
   are pre-compiled statics for the four fixed field names used.
5. **package.json dep checks.** `JSON.parse` + `{...deps, ...devDeps}` +
   JS truthiness → serde_json + a merged map + a `dep_truthy` helper that
   mirrors JS truthiness (empty-string version is falsy, etc.). nestjs only
   checks key prefixes (`@nestjs/`), as in TS.
6. **fabric detect / listDirectories.** TS guarded
   `if (!context.listDirectories) return false;` (optional method). The Rust
   trait always has `list_directories` with an empty-vec default, which is
   observably identical (no dirs → no subpackage probe).
7. `match_delim`/`read_args`/`matching_close`/`split_top_level_objects`
   walk bytes; all delimiters are ASCII so this is exactly equivalent to the
   TS UTF-16 walks (same argument as strip_comments — see
   notes/resolution-types.md).

## Test porting map

Into `rust/tests/frameworks_js_test.rs` (with a `TestContext` mock
implementing `ResolutionContext` over an in-memory node list + file map,
mirroring the TS inline mock contexts):

- `__tests__/frameworks.test.ts`: all expressResolver.extract (3),
  nestjs extract HTTP (5) / GraphQL (3) / microservices+WS (4) /
  detect (3) / resolve (2) / postExtract (8), react React-Router (5),
  svelte smoke (1), and the express + nestjs commented-route regressions (2).
- `__tests__/expo-modules.test.ts`: the 4 unit extract cases.
- `__tests__/fabric-view.test.ts`: the 3 unit extract cases.
- `__tests__/react-native-bridge.test.ts`: all 15 cases (fully unit —
  the TS mock context ported as `TestContext`).
- Plus resolution.test.ts's react component/hook resolve cases and a few
  Rust-side unit cases for svelte/vue resolve patterns and vue route
  extraction (not in the TS files verbatim; marked in the test file).

### Deferred cases (need modules owned by other agents)

- `__tests__/rn-event-channel.test.ts` — entirely end-to-end through
  `CodeGraph.init` + the callback synthesizer's `rn-event-channel` channel.
  Belongs with callback_synthesizer / public-API wiring.
- `__tests__/expo-modules.test.ts` "end-to-end — JS caller → native
  AsyncFunction" and `__tests__/fabric-view.test.ts` "end-to-end: JSX
  consumer → Fabric component → native class" — need the extraction
  orchestrator + resolver pipeline + `fabric-native-impl` / `jsx-render`
  synthesizers and DB assertions.
- `__tests__/frameworks.test.ts` `getApplicableFrameworks` +
  `__tests__/resolution.test.ts` `detectFrameworks`/`getAllFrameworkResolvers`
  — live in TS `frameworks/index.ts`, i.e. the stitched `frameworks/mod.rs`.
  When porting `get_applicable_frameworks`, note the TS semantics: a resolver
  with `languages == None` applies to every language; filtering is
  `languages.contains(lang) || languages.is_none()`.

## Integration needs

- `frameworks/mod.rs` owner: re-export the eight types above and register
  them in the TS `index.ts` `FRAMEWORK_RESOLVERS` order (verified):
  `[laravel, drupal, express, nestjs, react, svelte, vue, django, flask,
  fastapi, rails, spring, play, go, rust, aspnet, swiftUI, uikit, vapor,
  swiftObjcBridge, reactNativeBridge, expoModules, fabricView]`.
  TS `getApplicableFrameworks` semantics: a resolver whose `languages()` is
  `None` applies to every language, otherwise `languages.contains(lang)`.
  `detectFrameworks` wraps each `detect()` in try/catch→false (Rust impls
  don't panic, but keep the guard if wrapping fallible contexts).
- The orchestrator calling `extract()` must treat `Some(empty)` as "ran,
  found nothing" (express/nestjs return that for non-JS files) and `None`
  as "hook not implemented" (never returned by these eight).
- `post_extract` (nestjs) expects `get_nodes_by_name` / `get_nodes_in_file`
  to return the *current* DB state, and the orchestrator to persist each
  returned node via `update_node` keeping ids stable.
