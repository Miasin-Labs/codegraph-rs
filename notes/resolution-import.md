# import resolution port notes (import_resolver / path_aliases / workspace_packages / go_module)

Ported from `src/resolution/{import-resolver,path-aliases,workspace-packages,go-module}.ts`
into `rust/src/resolution/{import_resolver,path_aliases,workspace_packages,go_module}.rs`.
Read together with `notes/resolution-types.md` — the shared data types
(`AliasMap`/`AliasPattern`, `GoModule`, `WorkspacePackages`) are DEFINED in
`resolution/types.rs` and only **re-exported** (`pub use super::types::…`)
from my files, per that contract.

Tests: 37 in-module `#[cfg(test)]` cases (import_resolver 9, path_aliases 11,
workspace_packages 10, go_module 7) + `rust/tests/import_resolver_test.rs`
(29 cases). All pass in the real crate: `cargo test --test import_resolver_test`
and `cargo test --lib resolution::`. `cargo check` clean for these files
(zero warnings; the only whole-crate warning at verification time was an
`unused_mut` in `callback_synthesizer.rs`, another agent's in-flight file).

## Public API

```rust
// rust/src/resolution/import_resolver.rs  (mirrors every TS export)
pub fn resolve_import_path(import_path: &str, from_file: &str,
                           language: Language, context: &dyn ResolutionContext) -> Option<String>;
pub fn resolve_via_import(reference: &UnresolvedRef, context: &dyn ResolutionContext) -> Option<ResolvedRef>;
pub fn resolve_jvm_import(reference: &UnresolvedRef, context: &dyn ResolutionContext) -> Option<ResolvedRef>;
pub fn extract_import_mappings(_file_path: &str, content: &str, language: Language) -> Vec<ImportMapping>;
pub fn extract_re_exports(content: &str, language: Language) -> Vec<ReExport>;
pub fn load_cpp_include_dirs(project_root: &str) -> Vec<String>;     // cached per root
pub fn clear_cpp_include_dir_cache();
pub fn clear_import_mapping_cache();   // clears BOTH caches, like TS

// rust/src/resolution/path_aliases.rs
pub use super::types::{AliasMap, AliasPattern};
pub fn load_project_aliases(project_root: &str) -> Option<AliasMap>;
pub fn apply_aliases(import_path: &str, aliases: &AliasMap, project_root: &str) -> Vec<String>;
pub(crate) fn relative_lexical(from: &Path, to: &Path) -> String;    // Node path.relative, lexical

// rust/src/resolution/workspace_packages.rs
pub use super::types::WorkspacePackages;
pub fn load_workspace_packages(project_root: &str) -> Option<WorkspacePackages>;
pub fn resolve_workspace_import(import_path: &str, ws: &WorkspacePackages) -> Option<String>;

// rust/src/resolution/go_module.rs
pub use super::types::GoModule;
pub fn load_go_module(project_root: &str) -> Option<GoModule>;
```

`resolve_via_import` / `resolve_jvm_import` take `&UnresolvedRef` and CLONE it
into `ResolvedRef.original` (TS held a reference). All language branches are
ported: TS/JS/tsx/jsx relative + aliased imports, svelte/vue SFC `<script>`
ES6 imports (#629), Python, Go single+block imports and cross-package
qualified calls via go.mod (#388), Java/Kotlin FQN imports + static imports
(#314), PHP `use`, C/C++ `#include` (file→file edges, compile_commands.json
`-I`/`-isystem` parsing, heuristic include-dir probing), and barrel/re-export
chain following (`findExportedSymbol`, depth cap 8, cycle-safe; component-
node preference for default re-exports of `.svelte`/`.vue`, #629/#657).

## Faithfulness notes / mappings

- **TS→null ↔ Rust→None** throughout; confidences (0.9 import, 0.95 JVM),
  `REEXPORT_MAX_DEPTH = 8`, the full `C_CPP_STDLIB_HEADERS` set, fallback
  alias table order (`@/`, `~/`, `@src/`, `src/`, `@app/`, `app/`), and the
  compile_commands.json candidate list are byte-identical.
- **Node `path` semantics** are reimplemented lexically: private
  `posix_dirname`/`join_posix`/`normalize_segments`/`relative_posix` in
  import_resolver.rs (string, '/'-separated, keeps leading `..` so an
  import escaping the root fails `fileExists`, like TS) and
  `relative_lexical` in path_aliases.rs (component-based, shared with the
  compile-db `-I` normalization; on different Windows drive roots it
  returns `to` as-is, mirroring Node). `path.resolve`'s cwd-dependence is
  NOT reproduced — both sides of every relative computation derive from
  the same `project_root` string, so results are identical for any root
  (TS direct-call tests use `projectRoot: ''`; validated both '' and
  absolute tempdir roots).
- **JS regex `\w` is ASCII** (no `u` flag) — every TS `\w` became
  `[0-9A-Za-z_]` (`const W`); regexes are `LazyLock<Regex>` statics.
  JS `String.replace(str, …)` replaces FIRST occurrence → `replacen(…, 1)`
  (alias `*` fill; namespace member-name strip). `replace(/\./g)` etc.
  (global) → `replace_all`/`str::replace`.
- **JS truthiness** preserved at every site: empty `want.memberName` falls
  through to the exported-name branch; `!context.readFile(...)` treats
  `Some("")` as falsy; compile-db `directory: ""` falls back to
  `project_root`; `arguments: []` is used as-is (no fallback to `command`);
  `n.isExported` truthy → `is_exported == Some(true)`.
- **Module-level TS caches → process-global statics** behind `Mutex`
  (`CPP_INCLUDE_DIR_CACHE` keyed by project root; `IMPORT_MAPPING_CACHE` is
  vestigial — the TS original declared but never populated it; kept so
  `clear_import_mapping_cache()` mirrors the TS export, which clears both).
- **tsconfig JSONC**: ported the TS `stripJsonc` state machine verbatim
  (string-aware comment strip + trailing-comma regex) and parse with
  `serde_json` — did NOT switch to the `jsonc-parser` crate, so tolerance
  is bit-identical to TS. `compilerOptions.paths` order is preserved
  (serde_json `preserve_order`), then the same stable specificity sort.
  A tsconfig whose top level is a JSON *array* passes the read (TS
  `typeof [] === 'object'`) and stops the tsconfig→jsconfig fallthrough.
- **compile_commands.json** parsed via typed serde (`directory?`,
  `command?`, `arguments?: Vec<String>`); a type-mismatched db fails the
  whole parse → `None` → heuristic fallback, approximating the TS
  throw-inside-try → `return null` path.
- Deliberate micro-deviation: a non-string `compilerOptions.baseUrl`
  (e.g. a number) is treated as missing (`'.'`); TS would have thrown an
  uncaught TypeError in `path.resolve`. Strictly more robust, same result
  for every valid config.
- `extension_resolution(language)` is a `fn → &'static [&'static str]`
  (TS record lookup; unlisted languages → empty slice).
- `is_external_import` stays private (TS didn't export it) and takes
  `Option<&dyn ResolutionContext>` mirroring the TS optional param.

## Test split vs the resolver-stitch agent (__tests__/resolution.test.ts)

I took the suites that call these functions DIRECTLY with a stub context
(ported in `rust/tests/import_resolver_test.rs`):
- "Import Resolver" — all 4 cases.
- "JVM FQN Import Resolution" — all 9 cases.
- "C/C++ Import Resolution" — all 11 direct cases (+1 Rust-added test
  pinning the per-root cache/clear behavior), EXCEPT
  `connects #include to the real header file via include-dir scan (end-to-end)`
  which drives `CodeGraph.init` → **resolver-stitch agent**.

Left for other agents (full pipeline / other modules): "Name Matcher" (4) and
"Name Matcher: kind bias" (2) → name-matcher owner; "Framework Detection" (4)
and "React Framework Resolver" (2) → frameworks owner; "Integration Tests"
(9), "tsconfig path aliases" (2), "re-export chain following" (7) → all
`CodeGraph.init`-driven → resolver-stitch/public-API owner. (The
loaders/appliers those pipeline cases exercise are unit-covered here:
aliases, barrels, workspace subpaths, go.mod.)

## Integration needs (for resolver.rs / public-API wiring)

- The production `ResolutionContext` impl must override the defaulted trait
  methods and memoise per instance: `get_project_aliases` ←
  `load_project_aliases`, `get_go_module` ← `load_go_module`,
  `get_workspace_packages` ← `load_workspace_packages`,
  `get_cpp_include_dirs` ← `load_cpp_include_dirs`, `get_re_exports` ←
  `extract_re_exports` (LRU-cached in TS), `get_import_mappings` ←
  `extract_import_mappings` (LRU-cached in TS — the cache lives in the
  resolver, NOT in this module).
- Call `clear_import_mapping_cache()` (which also clears the cpp include-dir
  cache) wherever the TS resolver does between indexing runs.
- `resolution/mod.rs` still only declares `pub mod` lines; the resolver.rs
  owner should add the `index.ts`-style re-exports when wiring.
