# name matching port notes (name_matcher / swift_objc_bridge)

Ported from `src/resolution/{name-matcher,swift-objc-bridge}.ts` into
`rust/src/resolution/{name_matcher,swift_objc_bridge}.rs`. Scoring weights,
confidence constants, thresholds, strategy ordering, and tiebreaks are
ported EXACTLY (retrieval-quality-critical — see CLAUDE.md). Builds against
`resolution/types.rs` (`UnresolvedRef`, `ResolvedRef`, `ResolvedBy`,
`ResolutionContext`, `ImportMapping`) — no other resolution module needed.

Tests: in-module `#[cfg(test)]`.
- `name_matcher`: the 6 matchReference-targeting cases from
  `__tests__/resolution.test.ts` ("Name Matcher" + "Name Matcher: kind bias
  for new ref kinds" describes) — `__tests__/symbol-lookup.test.ts` does NOT
  target matchReference (it tests the search layer), so nothing came from
  there — plus 7 Rust-side tests covering strategies the TS suite only
  exercises end-to-end (file-path 0.95/0.85/0.7 ladder, partial
  qualified-name suffix 0.85, fuzzy 0.5/0.3 + kind filter, C++ receiver
  inference incl. header fallback + keyword/`xor` rejection, Java field
  signature inference, #314 preferred-FQN disambiguation, capitalized
  receiver, splitCamelCase parity).
- `swift_objc_bridge`: all 34 cases of `__tests__/swift-objc-bridge.test.ts`
  ported 1:1.
All pass in the real crate: `cargo test --lib name_matcher` 16/16,
`cargo test --lib swift_objc_bridge` 34/34; whole-crate `cargo check` clean
at finish time.

## Public API (rust/src/resolution/name_matcher.rs)

```rust
use crate::resolution::types::{ResolutionContext, ResolvedRef, UnresolvedRef};

// All take the ref by reference and clone it into ResolvedRef.original.
pub fn match_reference(reference: &UnresolvedRef, context: &dyn ResolutionContext) -> Option<ResolvedRef>;
// Individual strategies (exported in TS; resolver.ts may call them directly):
pub fn match_by_file_path(...)      -> Option<ResolvedRef>;  // 0.95 exact / 0.85 suffix / 0.7 singleton, FilePath
pub fn match_by_qualified_name(...) -> Option<ResolvedRef>;  // 0.95 unique QN / 0.85 suffix, QualifiedName
pub fn match_method_call(...)       -> Option<ResolvedRef>;  // 0.9 cpp/java typed, 0.85 S1, 0.8 S2, 0.7/0.65 S3
pub fn match_by_exact_name(...)     -> Option<ResolvedRef>;  // 0.9 / 0.5 cross-lang / 0.7|0.4 by proximity≥30
pub fn match_fuzzy(...)             -> Option<ResolvedRef>;  // 0.5 / 0.3 cross-lang, Fuzzy
```

`match_reference` strategy order (same as TS): file-path → qualified-name →
method-call → exact-name → fuzzy.

Private helpers ported 1:1: `resolve_method_on_type` (with #314
`preferred_fqn: Option<&str>` Java/Kotlin import disambiguation, `.kt`/`.java`
extension choice), `infer_cpp_receiver_type` (declarator scan up from call
line, then `.h`/`.hpp`/`.hxx` sibling-header fallback),
`infer_java_field_receiver_type` (enclosing class → field `signature`
"<Type> <name>" parse), `normalize_cpp_type_name` + the 29-token
`CPP_NON_TYPE_TOKENS` set, `split_camel_case`, `compute_path_proximity`
(15/segment capped 80), `find_best_match` (f64 score; 100 same-file, +prox,
+50/−80 language, +25 calls→fn/method, +25 instantiates→class/struct/iface,
+25/+15 decorates, +10 exported, +max(0, 20−lineDist/10); first-wins strict
`>` tiebreak; bestScore starts at −1 so an all-negative field can return None).

## Public API (rust/src/resolution/swift_objc_bridge.rs)

```rust
pub fn objc_selector_for_swift_method(base_name: &str, external_labels: &[Option<&str>],
                                      explicit_objc_name: Option<&str>) -> Option<String>;
pub fn objc_selector_for_swift_init(external_labels: &[Option<&str>], internal_names: &[&str],
                                    explicit_objc_name: Option<&str>) -> Option<String>;
pub struct ObjcAccessors { pub getter: String, pub setter: String }   // TS inline {getter,setter}
pub fn objc_accessors_for_swift_property(swift_name: &str,
                                         explicit_objc_name: Option<&str>) -> Option<ObjcAccessors>;
pub fn swift_base_names_for_objc_selector(selector: &str) -> Vec<String>; // insertion-ordered, deduped (TS Set)
pub fn detect_explicit_objc_name(source_slice: &str) -> Option<String>;
pub fn is_objc_exposed(source_slice: &str) -> bool;
```

For the `frameworks/swift_objc.rs` owner: TS `(string | null)[]` external
labels → `&[Option<&str>]` (`None` ≙ `null`; `Some("_")`/`Some("")` are also
treated as unlabeled, mirroring the TS `=== '_' || === ''` checks).
`internal_names` is `&[&str]` (TS `string[]`). JS truthiness on
`explicitObjcName` is preserved: `Some("")` does NOT short-circuit the
method/init rules; but in `objc_accessors_for_swift_property` TS used `??`
(nullish), so `Some("")` IS taken as the getter there — both preserved.

## Deviations (all behavior-neutral, documented inline)

- **Lookahead → consuming group** in the C++ declarator regex: TS
  `(?=[;=,)\[{(]|$)` is unsupported by the `regex` crate; replaced with
  `(?:[;=,)\[{(]|$)`. Equivalent here because only the FIRST match's capture
  group 1 is consumed (leftmost start position and group-1 span are identical
  with or without consuming the terminator).
- **JS `\w` → `[0-9A-Za-z_]`** in ported patterns (the regex crate's `\w` is
  Unicode-aware; JS's is ASCII). `\b` left as-is (Unicode vs ASCII word
  boundary differs only around non-ASCII identifiers — negligible for code).
- `CPP_NON_TYPE_TOKENS` is a 28-element const array + an explicit `"xor"`
  check in `is_cpp_non_type_token` (29 tokens total, exactly the TS set).
- JS `signature.lastIndexOf(name)` returning −1 made `slice(0, -1)` drop the
  signature's last char; mirrored explicitly (char-boundary-safe).
- `split(/\r?\n/)` mirrored via split('\n') + strip trailing '\r' (a lone
  `\r` is NOT a separator in either).
- TS `Set` insertion order in `swift_base_names_for_objc_selector` →
  `Vec<String>` with contains-check dedupe (order preserved: raw keyword
  first, then `init`, then preposition-stripped base, then setter property).
- Strategy-3 word-overlap score is `i64` (TS number arithmetic is integral
  there); `find_best_match` score is `f64` (TS `distance / 10` is float).
- `splitCamelCase`'s `w.length > 1` filter uses `chars().count() > 1`
  (UTF-16 vs scalar count differs only for astral-plane chars).

## Integration needs

- `resolution/mod.rs` already declares both modules; the `resolver.rs`
  owner should add `pub use name_matcher::match_reference;` (etc.) when
  wiring the module's `index.ts`-style re-exports.
- `match_method_call`'s Java/Kotlin branch calls
  `context.get_import_mappings(file_path, language)` and matches
  `ImportMapping.local_name` / reads `.source` as the preferred FQN — the
  production `ResolutionContext` impl must populate those (TS parity).
- The C++ branch calls `context.read_file` + `context.file_exists` with
  PROJECT-RELATIVE paths (same strings as `Node.file_path`) — the production
  context must resolve them against the project root.

## Blockers / validation record

**RESOLVED at finish time:** the foreign in-flight breakage below was fixed
by its owners before I finished; the final run in the REAL repo is green
(`cargo check` clean; `cargo test --lib name_matcher` 16/16;
`cargo test --lib swift_objc_bridge` 34/34). Interim record kept for the
orchestrator:

During the port the whole-crate `cargo check` failed in OTHER agents'
in-flight files (none in mine):

1. `src/resolution/path_aliases.rs:405` — E0758 unterminated block comment:
   `r#"…{"#/*": …}"#` terminates the raw string at the embedded `"#`,
   leaving a dangling `/*`. **Owner fix: `r##"…"##` delimiters.** This is a
   LEX error, so it breaks every build of the crate.
2. `src/resolution/import_resolver.rs:16` — E0603: imports
   `path_aliases::relative_lexical`, which is private. **Owner fix: `pub fn`.**
3. `src/resolution/callback_synthesizer.rs:586,1451` — E0277:
   `OrderedMap<String>: Default` not satisfied (its `entry_or_default`
   requires `V: Default` where V is the OrderedMap itself).
   **Owner fix: `impl<V> Default for OrderedMap<V>`.**

Since I can't edit those files, my two files were validated two ways:
(a) isolated scratch crate (real `types.rs` + `resolution/types.rs` + my two
files): `cargo test` 60/60 green, clippy clean for my files;
(b) full clone of the crate at `/tmp/cg-full-copy` with ONLY the three
foreign bugs above patched: same 16/16 + 34/34, zero errors/warnings
attributed to my files in the real-crate compile. Superseded by the final
green run in the real repo (above).
