# resolution stitch port notes (resolver / resolution mod / frameworks registry)

Ported `src/resolution/index.ts` (945 ln) → `rust/src/resolution/resolver.rs`,
`src/resolution/frameworks/index.ts` → `rust/src/resolution/frameworks/mod.rs`,
and wired `rust/src/resolution/mod.rs` re-exports. This is the resolution
integrator layer: batch-load → built-in filter → pre-filter → JVM-import →
frameworks → import → name-match → edge creation/promotion → persistence →
callback synthesis, with the same pass ordering, batching (5000), LRU usage,
constants, env var, and logDebug messages as TS.

Verification: whole-crate `cargo check --all-targets` clean (zero warnings,
zero errors). `cargo test` fully green crate-wide (367 lib tests + all
integration suites). My tests: `cargo test --test resolution_test` 30/30;
`cargo test --lib resolution::resolver` 4/4.

## Public API

```rust
// crate::resolution (mod.rs mirrors index.ts: `export * from './types'`
// + the class + createResolver; leaf fns are NOT re-exported, same as TS)
pub use types::*;
pub use resolver::{create_resolver, ReferenceResolver, ResolverContext};

// crate::resolution::resolver
pub struct ResolverContext;            // production ResolutionContext impl (TS createContext() literal)
impl ResolutionContext for ResolverContext { /* all 16 methods incl. the 6 TS-optional ones */ }

pub struct ReferenceResolver;
impl ReferenceResolver {
    pub fn new(project_root: impl Into<String>, queries: QueryBuilder) -> Self;
    pub fn initialize(&mut self);                       // detect frameworks + clearCaches
    pub fn context(&self) -> &dyn ResolutionContext;    // see deviations
    pub fn run_post_extract(&self) -> usize;
    pub fn warm_caches(&self);
    pub fn clear_caches(&self);
    pub fn resolve_all(&self, refs: &[UnresolvedReference],
                       on_progress: Option<&mut dyn FnMut(usize, usize)>) -> ResolutionResult;
    pub fn resolve_one(&self, r: &UnresolvedRef) -> Option<ResolvedRef>;
    pub fn create_edges(&self, resolved: &[ResolvedRef]) -> Vec<Edge>;
    pub fn resolve_and_persist(&self, refs: &[UnresolvedReference],
                               on_progress: Option<&mut dyn FnMut(usize, usize)>) -> Result<ResolutionResult>;
    pub fn resolve_and_persist_batched(&self,
                               on_progress: Option<&mut dyn FnMut(usize, usize)>,
                               batch_size: Option<usize>) -> Result<ResolutionResult>; // None = 5000
    pub fn get_detected_frameworks(&self) -> Vec<String>;
}
pub fn create_resolver(project_root: impl Into<String>, queries: QueryBuilder) -> ReferenceResolver;

// crate::resolution::frameworks (mod.rs)
pub fn get_all_framework_resolvers() -> Vec<Box<dyn FrameworkResolver>>;
pub fn get_framework_resolver(name: &str) -> Option<Box<dyn FrameworkResolver>>;
pub fn detect_frameworks(context: &dyn ResolutionContext) -> Vec<Box<dyn FrameworkResolver>>;
pub fn get_applicable_frameworks<'a>(detected: &'a [Box<dyn FrameworkResolver>],
                                     language: Language) -> Vec<&'a dyn FrameworkResolver>;
// + re-exports of all 23 resolver types and laravel::FACADE_MAPPINGS
```

## Wiring notes for the public-API (CodeGraph facade) owner

- Construct: `create_resolver(project_root, QueryBuilder::new(conn.get_db()?))`
  — the resolver OWNS its QueryBuilder (TS held a reference); `Db` is a cheap
  `Rc` clone so the facade keeps its own QueryBuilder on the same connection.
- `resolveReferences()` (TS facade) maps to
  `resolve_and_persist(&queries.get_unresolved_references()?, None)`;
  `indexAll`'s resolution pass maps to
  `resolve_and_persist_batched(progress, None)` — that's the production path,
  and it runs the callback-synthesis pass at the end (recorded under
  `stats.by_method["callback-synthesis"]`, even when 0 — TS parity).
- `run_post_extract()` after every indexAll/sync (NestJS RouterModule).
- Recreate the resolver when tsconfig/go.mod/workspaces change — the alias /
  go-module / workspace caches are `OnceCell` (TS: lazy + "immutable for the
  resolver's lifetime").
- `ReferenceResolver` is intentionally `!Send`/`!Sync` (RefCell caches +
  Rc-backed Db) — single-threaded like the TS pipeline. Confine to one thread.
- `CODEGRAPH_RESOLVER_CACHE_SIZE` honored exactly (parseInt-prefix semantics,
  >0 check, default 5000; content cache = max(64, limit/5)).

## Deviations (each documented inline too)

1. **Cache ownership**: the 7 LRU caches + knownNames/knownFiles live in
   `ResolverContext` (the TS context closures captured the resolver's private
   fields; Rust borrowck wants them with the trait impl). Observable behavior
   identical, incl. TS truthiness quirks: a cached EMPTY mappings/re-exports
   array IS returned (`if (cached) return cached` — arrays are truthy);
   `!content` treats `Some("")` as missing.
2. **Frameworks registry is a constructor, not a mutable global**: TS kept
   module-level singletons; several Rust resolvers carry per-instance caches
   (`RustResolver`, `SwiftObjcBridgeResolver`, `ReactNativeBridgeResolver`)
   whose notes require fresh instances per resolver lifetime. So
   `detect_frameworks`/`get_all_framework_resolvers` build fresh boxes in the
   exact TS registration order (pinned by test).
   `registerFrameworkResolver` was NOT ported — it mutated the global list
   and has zero usages in TS src/ or tests; callers can push onto the Vec
   they own. TS's try/catch→false around `detect()` is dropped: Rust
   resolvers signal failure by returning false, not by throwing.
3. **Error plumbing**: `resolve_and_persist[_batched]` return
   `crate::error::Result<ResolutionResult>` (the TS methods threw/rejected on
   DB errors). Context trait methods stay infallible and log-and-swallow DB
   errors to empty results (per the resolution-types contract). One guarded
   difference: a failed `warm_caches()` query leaves the pre-filter DISABLED
   (`None` ⇒ `has_any_possible_match` returns true) instead of propagating —
   strictly safer (no refs wrongly filtered); TS would have aborted the run.
4. **`context()` accessor added** (TS kept `context` private): the wiring
   layer and tests need the production context for the synthesizer and
   framework hooks; TS passed the same object to those collaborators anyway.
5. **`resolveAndPersistBatched`'s `await setImmediate` dropped** — no event
   loop; progress callback still fires per batch.
6. **`run_post_extract` catch granularity**: TS wrapped
   `fw.postExtract() + updateNode loop` in one try/catch (a throw aborts the
   rest of that framework's updates). Rust `post_extract` can't throw; an
   `update_node` Err logs the same `"Framework '{name}' postExtract failed"`
   debug message and breaks to the next framework.
7. **JS semantics preserved explicitly**: `Number.parseInt` prefix parsing
   for the env var; `ref.filePath || …` / `ref.language || …` (empty-string
   falsy; `'unknown'` truthy and kept); `floor((i/total)*100)` progress
   throttle with final `(total,total)` report; strict-`>` first-wins
   candidate reduce; byMethod keyed by the kebab-case `resolvedBy` strings.
   Built-in sets are byte-identical to TS (cardinalities pinned by unit
   test: JS 28, hooks 10, py 23/13/45, go 67/40, pascal 15+87, c 137, cpp 25).
8. `read_file` uses lossy UTF-8 decoding (`from_utf8_lossy`) to match Node's
   `readFileSync(_, 'utf-8')` replacement behavior rather than erroring.
9. `capitalize_first` is scalar-based (JS `charAt(0)` is a UTF-16 unit) —
   differs only for astral-plane first characters in identifiers.

## Test port map (tests/resolution_test.rs — 30 tests)

The TS suites drive `CodeGraph.init` + `indexAll`; extraction
orchestrator/languages were IN FLIGHT (off-limits), so fixtures insert the
extraction-shaped nodes/file records the TS extractors produce (same id /
qualified-name schemes) over REAL files + REAL SQLite, then run the REAL
production pipeline via `create_resolver().resolve_and_persist_batched()`.
The resolution machinery under test (import maps from real file content,
alias/workspace/go.mod loaders against the real fs, barrel chases, name
matching, edge promotion, persistence, unresolved-row cleanup) is all
production code end-to-end.

- resolution.test.ts "Framework Detection" (4) → mock-context ports + a
  Rust-added registration-order pin.
- frameworks.test.ts "getApplicableFrameworks" (2) → ported with fake
  resolvers (py/js/any), order-asserted.
- "Integration Tests" (9): react detection from a real project; import-based
  call edge; Python calls→instantiates promotion (incl. no-duplicate-calls
  assert); Go cross-package via go.mod #388 (disambiguates same-name pkga vs
  pkgb); Go aliased import + fmt stdlib filter #388; TS type_alias member
  method receiver-overlap #359 (RecorderHandle::stop wins, class look-alike
  gets 0 callers); Java import disambiguation #314 (field-signature receiver
  inference + imported-FQN choice — service convert, not dao); C# #381
  references-kind resolution (≥4/≥3 incoming references); Go stdlib stays
  external.
- "tsconfig path aliases" (2): alias-mapped file wins over same-named legacy
  duplicate; graceful no-tsconfig fallback.
- "re-export chain following" (7): 3-hop wildcard→named→declaration chase;
  renamed re-export (pre-filter import escape); svelte default re-export
  component preference #629; bare `.`/`./` directory imports #629; workspace
  `@scope/pkg/sub` barrel #629; Vue SFC script barrel #629; Vue template
  component through default re-export barrel #629.
- "C/C++ Import Resolution" e2e include case: file→file imports edge via the
  heuristic include-dir scan; `<vector>` produces no file edge.
- object-literal-methods.test.ts (resolution describe): store-action callers
  across files (destructured + chained getState + in-store sibling).
- pr19-improvements.test.ts: "Resolution Warm Caches" (+ asserts the
  callback-synthesis byMethod key and unresolved-row cleanup) and
  "Best-Candidate Resolution" (resolveOne exercised through the built-in
  short-circuit; the TS test only reflected on the prototype).
- In-module resolver.rs tests (4): parseInt-prefix parity, capitalize_first,
  the JS-family barrel-language regex, built-in set cardinalities.

### NOT ported here (and why / who)

- resolution.test.ts "Name Matcher" (4) + kind-bias (2) → already ported by
  the name-matcher agent (notes/resolution-match.md).
- "Import Resolver" (4), "JVM FQN" (9), C/C++ direct cases → import agent
  (tests/import_resolver_test.rs).
- "React Framework Resolver" (2) → frameworks-js agent
  (tests/frameworks_js_test.rs).
- object-literal-methods.test.ts extraction describe (5 cases driving
  `extractFromSource`) and pr19's grammar/extraction/MCP/CLI/db suites →
  extraction-orchestrator / MCP / CLI owners.
- The TS assertions that are pure extraction output (`stats.fileCount`,
  node existence after indexing) → re-assert in the facade's e2e suite once
  `CodeGraph.indexAll` lands. Same for the deferred end-to-end suites listed
  in notes/frameworks-backend.md (Drupal e2e, frameworks-integration Java
  e2e) and notes/frameworks-systems.md (gin-middleware-chain, go-grpc) —
  they need real multi-language extraction, not just resolution.

## Leaf-module observations (read-only audit, nothing broken)

- All leaf APIs matched their notes files exactly; no bugs surfaced through
  the pipeline tests — no foreign files were modified.
- `swift_objc.rs` still carries its documented private copies of two
  bridge-math functions; `swift_objc_bridge.rs` has since landed, so its
  owner (or a cleanup pass) can swap to
  `use crate::resolution::swift_objc_bridge::{…}` as their notes anticipate.
  Behavior-identical today; left untouched (not my file, no bug).
- `callback_synthesizer::synthesize_callback_edges` is wired exactly as its
  notes prescribe (after batched persistence, best-effort, count into
  `by_method["callback-synthesis"]`, Err ignored). Reminder from its notes
  stands: do not call the batched resolver twice in one indexing pass without
  re-extracting first, or synthesized edges duplicate (TS parity).
