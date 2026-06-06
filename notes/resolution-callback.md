# callback synthesizer port notes

Ported `src/resolution/callback-synthesizer.ts` (1233 ln) →
`rust/src/resolution/callback_synthesizer.rs`. All 14 synthesis channels,
faithful: field-backed observer (callback), closure-collection, EventEmitter,
react-render (setState→render), flutter-build (setState→build), cpp-override,
interface-impl, go-grpc-stub-impl, jsx-render (React JSX child), Vue SFC
template (kebab children + @event handlers + composable destructure),
rn-event-channel (ObjC/Swift/JVM → JS), fabric-native-impl, mybatis-java-xml,
gin-middleware-chain. Same constants (`MAX_CALLBACKS_PER_CHANNEL=40`,
`EVENT_FANOUT_CAP=6`, `MAX_JSX_CHILDREN=30`, `CC_FANOUT_CAP=8`,
`FABRIC_NATIVE_SUFFIXES`), same regexes, same caps/fan-out guards, same
per-channel dedup keys, same channel merge order.

## Public API

```rust
// crate::resolution::callback_synthesizer
pub fn synthesize_callback_edges(
    queries: &db::QueryBuilder,
    ctx: &dyn resolution::types::ResolutionContext,
) -> crate::error::Result<usize>;   // count of merged (deduped) edges inserted
```

Everything else is private. Every synthesized edge is
`kind: Calls, provenance: Some(Provenance::Heuristic)` with metadata written
EXACTLY as TS (same keys, same values, same key order — serde_json is built
with `preserve_order`):

| channel | metadata keys |
|---|---|
| callback | synthesizedBy, via, field, registeredAt |
| closure-collection | synthesizedBy, field, registeredAt |
| event-emitter | synthesizedBy, event, registeredAt (no `line`) |
| react-render / flutter-build | synthesizedBy, via:"setState", registeredAt |
| cpp-override / interface-impl / go-grpc-stub-impl | synthesizedBy, via, registeredAt |
| jsx-render (react + vue kebab) | synthesizedBy, via (no registeredAt) |
| vue-handler | synthesizedBy, event [, via — composable form] |
| rn-event-channel | synthesizedBy, event, registeredAt (no `line`) |
| fabric-native-impl | synthesizedBy, viaSuffix ("(exact)" for the bare name), componentName (no `line`) |
| mybatis-java-xml | synthesizedBy, via:"Class.id", registeredAt |
| gin-middleware-chain | synthesizedBy, via, registeredAt |

`registeredAt` is always `"{filePath}:{line}"` (the wiring site the MCP layer
surfaces) — string-identical to TS.

## Integration needs (resolver.rs owner)

- TS call site is `ReferenceResolver.resolveAllReferences`
  (src/resolution/index.ts:794-801): AFTER all base `calls` edges are
  persisted, call it best-effort and record the count:
  ```rust
  // best-effort — never fail the index on it (TS try/catch → ignore Err)
  match synthesize_callback_edges(&self.queries, &self.context) {
      Ok(n) => { aggregate_stats.by_method.insert("callback-synthesis".into(), n); }
      Err(_) => {} // synthesis is additive and optional; ignore failures
  }
  ```
  The TS fn returns `number` and relies on the caller's try/catch; the Rust
  port returns `Result<usize>` instead of swallowing internally so the caller
  keeps that decision.
- `insert_edges` endpoint-validates against the DB (skips edges whose
  endpoints don't exist) — same as TS.
- **Re-running the synthesizer inserts duplicate rows**: the `edges` table has
  no UNIQUE constraint, so INSERT OR IGNORE never fires. This is TS-parity
  (the pipeline re-extracts files — deleting their edges — before
  re-resolving; synthesis is downstream of that). Don't call it twice in one
  pass.

## Deviations (all mechanical, behavior-identical)

- **JS regex semantics preserved explicitly**: JS `\w`/`\d`/`\b` are
  ASCII-only; Rust's default is Unicode. All identifier classes are written
  `[0-9A-Za-z_]` / `[0-9]` and word boundaries `(?-u:\b)` so non-ASCII (CJK)
  identifiers behave exactly as in TS.
- **`methodAndFunctionNodes` generator → `iterate_nodes_by_kind` visitor**
  (db port's #610-preserving shape): private
  `for_each_method_and_function(queries, FnMut(Node))` streams method then
  function nodes; O(1) memory in node count.
- **JS `Map`/`Set` insertion order** matters here (cap truncation +
  which metadata survives the merged `source>target` dedup), so private
  `OrderedMap<V>`/`OrderedSet` reimplement JS semantics (set-on-existing-key
  keeps position; iteration in first-insertion order). HashMap is used only
  where the TS map is lookup-only (interface-impl overload grouping, go-grpc
  name sets, mybatis javaIndex, fabric class index, vue destructured map).
- **Offsets/lines**: `regex` match offsets are bytes (TS: UTF-16 units); line
  numbers are computed by counting `\n` bytes before the offset, which is
  identical to the TS `slice(0, idx).split('\n').length` for any UTF-8 input.
  `goBalancedArgs` walks bytes (delimiters are ASCII — equivalent to the TS
  UTF-16 walk).
- **TS falsiness**: `sliceLines` returns `None` for 0 bounds and call sites
  treat `None`/empty-string alike (`node_source` helper mirrors
  `const src = content && sliceLines(...); if (!src) continue`). `if (!e.line)`
  → `Some(l) if l > 0`. Empty-file reads short-circuit exactly where TS's
  `if (!content)` did — and do NOT where TS only used `?.`
  (fieldChannel's caller-line read).
- **Dynamic `argRe`** (`new RegExp(\`${reg.node.name}...\`)`) uses
  `regex::escape` on the name — a no-op for every reachable name (registrar
  names match `REGISTRAR_NAME`, ASCII word chars only), but prevents a panic
  path TS didn't have.
- **Error plumbing**: TS QueryBuilder throws → caller catches; Rust channels
  that touch the DB return `Result` and `?` up to the public fn.
- `Node.end_line` is non-optional in `crate::types` so TS's defensive
  `endLine ?? startLine` collapses to `end_line` (the DB always stores it).

## Cross-module deps (read-only)

- `crate::db::QueryBuilder` — iterate_nodes_by_kind / get_nodes_by_kind /
  get_outgoing_edges / get_incoming_edges / get_node_by_id / insert_edges.
- `crate::extraction::generated_detection::is_generated_file` (go-grpc gate).
- `super::strip_comments::{strip_comments_for_regex, CommentLang::Go}` (gin).
- `super::types::ResolutionContext` (object-safe trait — taken as `&dyn`).
- NOT re-exported from `resolution/mod.rs` (mod.rs only declares `pub mod`
  lines; the resolver.rs owner wires the barrel re-exports — add
  `pub use callback_synthesizer::synthesize_callback_edges;` if mirroring the
  TS index.ts surface).

## Tests

- `tests/callback_synthesizer_test.rs` (6, all green):
  - Full port of `__tests__/closure-collection-synthesizer.test.ts` (same
    Swift fixture byte-for-byte, same SQL assertions via `json_extract`).
    The TS test drives `CodeGraph.init` + `indexAll`; that facade isn't
    ported yet, so the fixture inserts the extraction-shaped nodes/edges
    directly (real files in a tempdir + real SQLite, no mocks — the same
    approach `tests/db_test.rs` documented for iterate-nodes-by-kind).
  - Port-validation coverage: field channel (via/field/registeredAt), event
    emitter (named-function handler + registration site), react-render
    (setState gate + non-setState sibling untouched), jsx-child (capitalized
    + resolved gate), merged-pass dedup (vue kebab + @click on the same
    target → 1 edge, first-emitted jsx-render wins).
- In-module `#[cfg(test)]` (7, all green): kebab_to_pascal, slice_lines TS
  semantics, registrar/dispatcher field extraction, closure-collection regex
  shapes (incl. the `$0` digit-guard and the non-invoking forEach gate),
  enclosing_fn tightest-encloser preference, go helper trio, OrderedMap/Set
  insertion-order parity.
- **`__tests__/pr19-improvements.test.ts` contains NO synthesizer cases** —
  checked per assignment: nothing in it references `synthesizeCallbackEdges`
  / `synthesizedBy` / heuristic provenance. The other channels' e2e tests
  (`fabric-view`, `rn-event-channel`, `gin-middleware-chain`,
  `frameworks-integration`) belong to the frameworks/orchestrator owners.

## Verification status

`cargo check`: clean for this file and the test (remaining warnings are in
other agents' in-flight files: frameworks/nestjs.rs, frameworks/rust.rs,
frameworks/swift_objc.rs, path_aliases.rs). `cargo test --lib
resolution::callback_synthesizer` 7/7; `cargo test --test
callback_synthesizer_test` 6/6.
