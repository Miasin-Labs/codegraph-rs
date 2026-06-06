# resolution foundation port notes (types / lru_cache / strip_comments)

Ported from `src/resolution/{types,lru-cache,strip-comments}.ts` into
`rust/src/resolution/{types,lru_cache,strip_comments}.rs`. This is the
contract every other resolution file builds against — read this before
porting `import_resolver.rs`, `name_matcher.rs`, `resolver.rs`,
`path_aliases.rs`, `go_module.rs`, `workspace_packages.rs`,
`callback_synthesizer.rs`, or anything under `frameworks/`.

Tests: in-module `#[cfg(test)]` — all 17 cases of
`__tests__/strip-comments.test.ts` (+1 Rust-specific multibyte case),
7 LRU unit tests, 5 contract tests. All pass (validated in an isolated
scratch crate because `extraction/languages` was mid-port and broke the
whole-crate build at verification time — see Blockers at the bottom).

## Public API (rust/src/resolution/types.rs)

```rust
// Working unresolved ref — file_path/language are REQUIRED here, unlike the
// persisted crate::types::UnresolvedReference where they're Option<_>
// denormalizations. The resolver (TS resolveReferences, index.ts:478) fills
// them from the source node before strategies run; that mapping is the
// resolver agent's job (no From impl is provided — it needs node lookups).
pub struct UnresolvedRef {
    pub from_node_id: String,
    pub reference_name: String,
    pub reference_kind: crate::types::EdgeKind,
    pub line: u32,
    pub column: u32,
    pub file_path: String,
    pub language: crate::types::Language,
    pub candidates: Option<Vec<String>>,
}

// TS string union → enum. as_str()/Display/FromStr give the exact TS strings
// ("exact-match", "import", "qualified-name", "framework", "fuzzy",
// "instance-method", "file-path"); serde kebab-case matches. Use
// `r.resolved_by.as_str()` as the byMethod stats key.
pub enum ResolvedBy { ExactMatch, Import, QualifiedName, Framework, Fuzzy, InstanceMethod, FilePath }
pub const RESOLVED_BY_METHODS: [ResolvedBy; 7];

pub struct ResolvedRef {
    pub original: UnresolvedRef,   // owned (TS held a reference; clone/move in)
    pub target_node_id: String,
    pub confidence: f64,           // 0–1
    pub resolved_by: ResolvedBy,
}

pub struct ResolutionStats { pub total: usize, pub resolved: usize, pub unresolved: usize,
                             pub by_method: HashMap<String, usize> }  // TS inline stats object
pub struct ResolutionResult { pub resolved: Vec<ResolvedRef>, pub unresolved: Vec<UnresolvedRef>,
                              pub stats: ResolutionStats }

// Object-safe trait; framework resolvers receive `&dyn ResolutionContext`.
pub trait ResolutionContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node>;
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node>;
    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node>;
    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node>;
    fn file_exists(&self, file_path: &str) -> bool;
    fn read_file(&self, file_path: &str) -> Option<String>;
    fn get_project_root(&self) -> &str;
    fn get_all_files(&self) -> Vec<String>;
    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node>;
    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping>;
    // TS-optional methods → defaults matching the absent-method observable
    // behavior at the TS call sites (None / empty Vec). Production resolver
    // overrides all six.
    fn get_project_aliases(&self) -> Option<&AliasMap> { None }
    fn get_go_module(&self) -> Option<&GoModule> { None }
    fn get_workspace_packages(&self) -> Option<&WorkspacePackages> { None }
    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> { Vec::new() }
    fn list_directories(&self, relative_path: &str) -> Vec<String> { Vec::new() }
    fn get_cpp_include_dirs(&self) -> Vec<String> { Vec::new() }
}

pub struct FrameworkExtractionResult { pub nodes: Vec<Node>, pub references: Vec<UnresolvedRef> }

// Object-safe; registry holds Vec<Box<dyn FrameworkResolver>>. All &self —
// every TS framework resolver is stateless (verified: zero `this.` usage
// under src/resolution/frameworks/). Caches ⇒ RefCell/Cell.
pub trait FrameworkResolver {
    fn name(&self) -> &str;
    fn languages(&self) -> Option<&[Language]> { None }       // None = all languages
    fn detect(&self, context: &dyn ResolutionContext) -> bool;
    fn resolve(&self, reference: &UnresolvedRef, context: &dyn ResolutionContext) -> Option<ResolvedRef>;
    fn claims_reference(&self, name: &str) -> bool { false }  // TS `resolver.claimsReference?.(name)` falsy
    // None = hook NOT implemented (TS method absent) — orchestrator may use
    // this to skip work; Some(empty) = hook ran, found nothing.
    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> { None }
    fn post_extract(&self, context: &dyn ResolutionContext) -> Option<Vec<Node>> { None }
}

pub struct ImportMapping {
    pub local_name: String, pub exported_name: String, pub source: String,
    pub is_default: bool, pub is_namespace: bool, pub resolved_path: Option<String>,
}

pub enum ReExport {                                   // serde: internally tagged on "kind"
    Named { exported_name: String, original_name: String, source: String },  // kind:"named"
    Wildcard { source: String },                                             // kind:"wildcard"
}
impl ReExport { pub fn source(&self) -> &str; }       // convenience, common to both
```

All structs/enums derive Debug/Clone/PartialEq + Serialize/Deserialize with
camelCase field renames (wire parity); `candidates`/`resolved_path` use
`skip_serializing_if` like the TS optionals.

## DELIBERATE DEVIATION — sibling data types are defined in types.rs

The TS `ResolutionContext` referenced `AliasMap` (path-aliases.ts),
`GoModule` (go-module.ts), and `WorkspacePackages` (workspace-packages.ts)
via type-only imports. Those Rust files were stubs when this contract had to
compile, so the DATA TYPES are defined in `resolution/types.rs`:

```rust
pub struct AliasPattern { pub prefix: String, pub suffix: String,
                          pub has_wildcard: bool, pub replacements: Vec<String> }
pub struct AliasMap     { pub base_url: PathBuf, pub patterns: Vec<AliasPattern> }
pub struct GoModule     { pub module_path: String, pub root_dir: PathBuf }
pub struct WorkspacePackages { pub by_name: HashMap<String, String> }
```

**Owners of `path_aliases.rs` / `go_module.rs` / `workspace_packages.rs`:
do NOT re-define these.** Add `pub use super::types::{AliasMap, AliasPattern};`
(etc.) at the top of your file and implement only the loader/apply functions
(`load_project_aliases`, `apply_aliases`, `load_go_module`,
`load_workspace_packages`, `resolve_workspace_import`) against these shapes.

- `base_url`/`root_dir` are `PathBuf` (TS held absolute-path strings);
  pattern/replacement/module strings stay `String` (import specifiers, not
  fs paths).
- `WorkspacePackages.by_name` is a `HashMap` (TS used insertion-ordered
  `Map`): the loader's first-declaration-wins is enforced at insert time
  (`contains_key` check, mirroring TS `!byName.has(...)`), and the only
  iterating consumer (`resolveWorkspaceImport`) picks the LONGEST matching
  name, which is order-independent — two distinct equal-length names can't
  both prefix-match the same import.

## Other mapping decisions (for the five downstream agents)

- `ResolutionContext` methods are infallible like TS. Impls backed by
  `db::QueryBuilder` (whose methods return `Result`) should log-and-swallow
  to empty results (`log_warn`/`log_debug` from `crate::error`), not panic.
- `get_project_root()` returns `&str` (impls store the root). The lazy
  per-instance caches behind `Option<&AliasMap>` etc. work with
  `OnceCell<Option<AliasMap>>` + `get_or_init(...).as_ref()` under `&self`.
- `get_import_mappings`/`get_re_exports` return OWNED `Vec`s (TS returned the
  cached array by reference; returning `&[_]` from an LRU is borrow-hostile —
  clone out of the cache).
- `Node['kind']` → `crate::types::NodeKind`; reference kinds →
  `crate::types::EdgeKind`; languages → `crate::types::Language`.

## LRUCache (rust/src/resolution/lru_cache.rs)

```rust
pub struct LRUCache<K: Eq + Hash + Clone, V>;
impl LRUCache<K, V> {
    pub fn new(max: usize) -> Self;          // panics on 0 with the exact TS message:
                                             // "LRUCache max must be a positive finite number, got 0"
    pub fn len(&self) -> usize;              // TS `size` getter
    pub fn is_empty(&self) -> bool;
    pub fn get(&mut self, key: &K) -> Option<&V>;  // refreshes recency ⇒ &mut self
    pub fn has(&self, key: &K) -> bool;            // no recency refresh (TS Map.has)
    pub fn set(&mut self, key: K, value: V);       // evicts LRU when full
    pub fn clear(&mut self);
}
```

- JS's insertion-ordered `Map` doesn't exist in std; implemented as
  `HashMap<K,(stamp,V)>` + `BTreeMap<stamp,K>` (monotonic u64 stamp).
  O(log n) per op, identical eviction order to the TS delete+reinsert dance.
  No new dependency (per assignment).
- `get` takes `&mut self` (recency refresh is mutation). A resolver exposing
  caches through `&self` context methods should wrap them in `RefCell`.
- The TS "stored undefined" defensive branch is unrepresentable and dropped.

## strip_comments (rust/src/resolution/strip_comments.rs)

```rust
pub enum CommentLang { Python, Javascript, Typescript, Php, Ruby, Java, Csharp, Swift, Go, Rust }
pub fn strip_comments_for_regex(content: &str, lang: CommentLang) -> String;
```

- Every language branch ported exactly (python triple-quote/docstring, ruby
  `=begin/=end` with at-line-start tracking, C-style with
  `allow_single_quote_strings` = js|ts only and multi-line backtick
  template literals for ALL C-style langs (faithful), php `// # /* */` +
  backtick-as-string, go raw backtick strings kept intact + rune literals,
  rust NESTED block comments + char/lifetime skip).
- Rust-port note: the scanner walks BYTES (all delimiters are ASCII; UTF-8
  multi-byte sequences never contain ASCII bytes, so this is exactly
  equivalent to the TS UTF-16 walk). Blanking preserves BYTE length —
  `regex` crate match offsets are byte offsets, so offset→line mapping
  against the original source stays valid (the TS version preserved UTF-16
  length for the same reason). Output is always valid UTF-8 (blanked ranges
  begin/end on ASCII delimiters).
- TS `CommentLang` default fall-through (returns content unchanged) is
  unreachable with a Rust enum and dropped.
- Framework extractors should map `crate::types::Language` →
  `CommentLang` themselves (TS did the same — `CommentLang` is narrower
  than `Language`; e.g. tsx/jsx → Typescript/Javascript branch).

## Integration needs / blockers

- `rust/src/resolution/mod.rs` currently only declares `pub mod` lines; it
  does not re-export the public surface the way TS `resolution/index.ts`
  does. The `resolver.rs` owner (who owns the module's index.ts analog)
  should add `pub use types::*;` etc. when wiring. Everything is reachable
  today at `crate::resolution::types::…`, `crate::resolution::lru_cache::LRUCache`,
  `crate::resolution::strip_comments::{strip_comments_for_regex, CommentLang}`.
- At verification time the WHOLE-crate `cargo check` failed only in
  `src/extraction/languages/mod.rs` (E0432, another agent mid-port — 10
  missing `*Extractor` re-exports). My three files were therefore validated
  in an isolated scratch crate (foundation `src/types.rs` + these three
  files + serde): `cargo test` 36/36 green, zero warnings. Re-run
  `cargo test resolution::` once extraction/languages lands.
