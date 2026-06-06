# extraction-languages port notes

Ported all of `src/extraction/languages/*.ts` → `rust/src/extraction/languages/`
(18 language files + `mod.rs` barrel). Every extractor is a stateless unit
struct implementing `crate::extraction::tree_sitter_types::LanguageExtractor`
per the extraction-core contract (`notes/extraction-core.md`).

## Public API surface (for the wiring wave)

```rust
// languages/mod.rs
pub fn extractor_for(language: Language) -> Option<&'static dyn LanguageExtractor>;

// unit structs, all `pub` and re-exported from mod.rs:
TypescriptExtractor, JavascriptExtractor, PythonExtractor, GoExtractor,
RustExtractor, JavaExtractor, CExtractor, CppExtractor (both in c_cpp.rs),
CsharpExtractor, PhpExtractor, RubyExtractor, SwiftExtractor, KotlinExtractor,
DartExtractor, PascalExtractor, ScalaExtractor, LuaExtractor, LuauExtractor,
ObjcExtractor
```

`extractor_for` mirrors the TS `EXTRACTORS` map exactly: typescript+tsx →
TypescriptExtractor; javascript+jsx → JavascriptExtractor; c/cpp split;
21 mapped languages total; everything else (svelte, vue, liquid, yaml, twig,
xml, properties, unknown) → `None`.

`mod.rs` also hosts two `pub(crate)` shared helpers used across language
files (copies of the wrapper's private ones, per the core notes):
`named_children(node) -> Vec<SyntaxNode>` and
`find_named_child(node, kind) -> Option<SyntaxNode>`.

## Faithfulness

All node-type tables, field names, kind mappings, visibility/async/static
defaults, ImportOutcome tri-state semantics (`Declined` wherever the TS hook
returns `null`), and hook-presence semantics (`Some(bool)` for present TS
hooks) are 1:1 with the TS source. Deviations below are all driven by
native-grammar differences (native crates vs the wasm grammars TS loads) and
were validated against what `__tests__/extraction.test.ts` actually asserts.

### Kotlin — tree-sitter-kotlin-ng vs fwcd grammar (DELIBERATE ADAPTATIONS)

grammars.rs uses `tree-sitter-kotlin-ng`, which renames several nodes vs the
fwcd grammar the TS extractor targets. Adapted so the TS suite's
user-visible expectations still hold:

- `import_types()` is `["import_header", "import"]` — kotlin-ng calls the
  node `import` (fwcd: `import_header`); both listed.
- `extract_import` prefers a `qualified_identifier` child (kotlin-ng wraps
  the dotted path; the alias of `import a.B as C` is a separate trailing
  `identifier`) and falls back to `identifier`. Reproduces the TS-suite
  expectations: simple → `java.io.IOException`, aliased → path without alias,
  wildcard → path without `.*`.
- `extract_package` accepts `qualified_identifier` then `identifier`.
- `classify_class_node` additionally scans `modifiers > class_modifier` text
  for `enum`/`interface` — kotlin-ng nests the `enum` keyword there
  (`enum class Level` → `(class_declaration (modifiers (class_modifier)) …)`),
  while plain `interface Foo` still has a direct anonymous `interface` child.
- `name_field()` stays `"simple_identifier"` (TS value). kotlin-ng has a real
  `name:` field with node type `identifier`; names resolve via the core's
  first-identifier-child fallback, which the core port already handles.
- The `fun interface` misparse workaround (visit_node + resolve_body ERROR
  handling) is ported verbatim but is **dead code on kotlin-ng** — the native
  grammar parses `fun interface` cleanly (class_declaration classified
  Interface by the keyword scan), which satisfies the TS tests' intent
  without the workaround. Left in place for fidelity/harmlessness.
- kotlin-ng quirk: `class C { fun f() {} }` (no trailing semicolon/newline
  variations) can produce a `(MISSING _class_member_semi)` node; it doesn't
  affect extraction.

### Dart — native-grammar node renames (DELIBERATE ADAPTATIONS)

- `class_types()` is `["class_definition", "class_declaration"]` — the native
  tree-sitter-dart 0.2 grammar names the node `class_declaration` (TS-era
  wasm grammar: `class_definition`); both listed (superset).
- TS sets `callTypes: []` because the wasm-era grammar represented calls as
  identifier+`selector` pairs, captured via `extractBareCall`. The native
  grammar produces real `(call_expression function: …)` nodes, so
  `call_types()` is `["call_expression"]` and Dart calls keep being captured
  (TS-on-wasm did capture them); `extract_bare_call` is ported verbatim and
  retained, but is dead code on this grammar.
- Class methods nest as `class_body > class_member > method_declaration
  (signature: method_signature, body: function_body)`; the unclaimed wrappers
  are traversed through, and the signature/body sibling relationship the TS
  `resolveBody` depends on still holds inside `method_declaration`.

### Scala — tree-sitter-scala 0.26 path fields (BEHAVIOR NOTE)

`import scala.collection.mutable` parses as
`(import_declaration path: (identifier) ×3)` — one `path:` field **per
segment**. `child_by_field_name("path")` returns the first, so import module
names are the **root segment** (`scala`), not the dotted path. This is what
the TS hook produces on the same tree (webTS `childForFieldName` also returns
the first); the TS suite only asserts import **count** for Scala. If
dotted-path parity with other languages is wanted later, join all `path`
children — deliberately NOT done (faithful port).

### Python / Rust — isAsync hooks are grammar-blind (TS parity, NOT fixed)

- python.ts: `isAsync = previousSibling?.type === 'async'` — tree-sitter-python
  puts `async` INSIDE `function_definition`, so this returns false for
  `async def`. Ported verbatim; same outcome as TS. TS suite doesn't assert it.
- rust.ts: `isAsync` scans direct children for `async` — tree-sitter-rust
  nests it under `function_modifiers`, so `async fn` → false. Ported verbatim.
  TS suite doesn't assert it. (Fixing both is a 3-line change each; flag for
  upstream rather than silently diverging.)

### Ruby — bare `private` is an `identifier`, not a `call` (TS parity)

`getVisibility` only recognizes `call`-typed preceding siblings (e.g.
`private :foo`); a bare `private` line is a plain `identifier` in
tree-sitter-ruby, so methods after it stay `public`. Ported verbatim; same as
TS on the same grammar. TS suite doesn't assert Ruby visibility. (A bare
`private` identifier IS picked up by `extract_bare_call` as a call named
"private" — also TS-parity.)

### ObjC

`- (void)m;` declarations inside `@interface` are `method_declaration`, which
is NOT in `method_types()` (TS parity — only `method_definition` in an
`@implementation` is extracted; the @interface emits the class node and
`@implementation` reuses it via the visit_node hook — verified single class
node in the smoke test).

`isStatic` TS regex `/^\s*\+/` is implemented as
`text.trim_start().starts_with('+')` (equivalent).

### Other Rust-idiom translations (no behavior change)

- TS `??`-style "hook returned null" → `ImportOutcome::Declined` everywhere a
  TS `extractImport` returns null; languages with no hook (Go, Pascal) keep
  the default `NotHandled`.
- C/C++ `extractImport` was duplicated verbatim in TS; shared here as a
  private `extract_include_import` fn (byte-identical behavior). Same for
  `resolve_typedef_kind` (C/C++/ObjC share the same body in TS).
- BFS helpers (`extractCppQualifiedMethodName`, lua `findDescendant`) use
  `VecDeque` for the TS array-shift queues.
- Luau's TS object spread (`...luaExtractor`) is explicit delegation to a
  `static LUA: LuaExtractor`; only `type_alias_types`, `is_exported`,
  `get_signature` are overridden (exactly the keys the TS literal sets);
  `get_receiver_type`/`visit_node` delegate (= inherited via spread).
- Lua `require` signature `.slice(0, 100)` → `chars().take(100)` (chars vs
  UTF-16 units; same caveat already noted by the core port).
- Go `isExported` `charCodeAt(0) ∈ [65,90]` → `first byte is_ascii_uppercase()`.
- Regexes (`RECEIVER_RE` in go.rs) are `LazyLock<Regex>` statics with the
  exact TS patterns.

## Tests (all passing)

- **26 in-module `#[cfg(test)]` tests** (one+ per language file): each runs a
  representative snippet end-to-end through `TreeSitterExtractor`, asserting
  node kinds, names, qualified names, visibility/async/static, imports, and
  hook-specific behavior (PHP trait-use implements refs + class constants,
  Ruby module scoping + bare calls, Lua/Luau require forms, Go/Kotlin
  receivers + Go generic-receiver regex (#583), ObjC selector building +
  @implementation class reuse, C/C++ typedef-kind resolution + misparse
  filter, Scala val/var field-vs-constant, mod.rs EXTRACTORS-map parity).
  Run: `cargo test --lib extraction::languages`
- **`tests/extraction_languages_test.rs`** (3 tests): drives all 21 mapped
  languages through the public `extractor_for` + `TreeSitterExtractor`
  surface — per-language symbol/kind round-trips, per-language import
  extraction, and call-reference recording. Run:
  `cargo test --test extraction_languages_test`
- Test-snippet learnings encoded there: a Rust `struct S;` (no body) is
  skipped as a forward declaration (TS parity); a Pascal implementation-only
  `defProc` creates no node — the interface `declProc` is the node and the
  definition only contributes calls (TS parity).

## For the integrator

- The orchestrator's `extract_from_source` should call
  `languages::extractor_for(lang)` and hand the result to
  `TreeSitterExtractor::new` (per extraction-core notes).
- When porting `__tests__/extraction.test.ts`, expect these grammar-driven
  differences (all detailed above): Scala import names are root segments;
  Python/Rust isAsync false; Ruby bare-`private` visibility public. Kotlin
  tests should pass as-is thanks to the kotlin-ng adaptations, except
  fun-interface tests pass via clean parses rather than the misparse path.
- Nothing here required changes to shared files; no missing dependencies.
