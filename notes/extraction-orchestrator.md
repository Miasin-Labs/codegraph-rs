# extraction-orchestrator port notes (final extraction layer + module stitch)

Ported files:

- `src/extraction/orchestrator.rs` ← `src/extraction/index.ts` (1469 ln) **plus**
  the `extractFromSource` dispatcher from the bottom of
  `src/extraction/tree-sitter.ts` (deferred to this layer by
  `notes/extraction-core.md` because the standalone extractors were stubs then).
- `src/extraction/mod.rs` — stitched: re-exports mirror `src/extraction/index.ts`
  (orchestrator surface + grammar fns) plus each TS file's own exports
  (extractors, `LanguageExtractor` contract types, `generate_node_id`,
  `is_generated_file`).
- `tests/extraction_test.rs` ← `__tests__/extraction.test.ts` (4670 ln) +
  the extraction half of `__tests__/object-literal-methods.test.ts`.
  **289 tests, all green** (`cargo test --test extraction_test`). Plus 6
  in-module unit tests in orchestrator.rs (`cargo test --lib extraction::orchestrator`).
  `cargo check --all-targets`: zero warnings/errors crate-wide at handoff.

## Public API surface (for the wiring wave / `CodeGraph`)

```rust
// crate::extraction (all re-exported from mod.rs)

pub fn extract_from_source(
    file_path: &str, source: &str,
    language: Option<Language>,            // None ⇒ detect_language(path, Some(source))
    framework_names: Option<&[String]>,    // None/empty ⇒ no framework pass
) -> ExtractionResult;

pub fn hash_content(content: &str) -> String;              // sha256 hex (Node parity)
pub fn build_default_ignore(root_dir: &Path) -> ignore::gitignore::Gitignore;
pub fn scan_directory(root_dir: &Path,
    on_progress: Option<&mut dyn FnMut(usize, &str)>) -> Vec<String>;

pub enum IndexPhase { Scanning, Parsing, Storing, Resolving }  // serde "scanning"...
pub struct IndexProgress { pub phase: IndexPhase, pub current: usize,
    pub total: usize, pub current_file: Option<String> }       // serde camelCase
pub struct IndexResult { pub success: bool, pub files_indexed/skipped/errored: usize,
    pub nodes_created: usize, pub edges_created: usize,
    pub errors: Vec<ExtractionError>, pub duration_ms: i64 }   // serde camelCase
pub struct SyncResult { files_checked/added/modified/removed, nodes_updated: usize,
    duration_ms: i64, changed_file_paths: Option<Vec<String>>,
    changed_node_names: Option<Vec<String>> }                  // Options omitted when None (TS parity)
pub struct ChangedFiles { added, modified, removed: Vec<String> }
pub struct ReconcileResult { files_removed: usize, removed_node_names: Vec<String> }
pub struct FileStats { size: u64, modified_at_ms: i64 }        // fs.Stats subset
impl FileStats { pub fn from_metadata(&fs::Metadata) -> Self }

pub struct ExtractionOrchestrator<'a> { /* root_dir, &QueryBuilder, RefCell<framework names> */ }
impl<'a> ExtractionOrchestrator<'a> {
    pub fn new(root_dir: impl Into<PathBuf>, queries: &'a QueryBuilder) -> Self;
    pub fn index_all(&self, on_progress: Option<&dyn Fn(&IndexProgress)>,
        signal: Option<&AtomicBool>, _verbose: bool) -> Result<IndexResult>;
    pub fn index_files(&self, file_paths: &[String]) -> Result<IndexResult>;
    pub fn index_file(&self, relative_path: &str) -> Result<ExtractionResult>;
    pub fn index_file_with_content(&self, relative_path: &str, content: &str,
        stats: &FileStats) -> Result<ExtractionResult>;
    pub fn sync(&self, on_progress: Option<&dyn Fn(&IndexProgress)>) -> Result<SyncResult>;
    pub fn get_changed_files(&self) -> Result<ChangedFiles>;
    pub fn reconcile_removed_files(&self) -> Result<ReconcileResult>;
    pub fn reset_detected_frameworks(&self);
}
```

Signature mappings the wiring wave must know:

- **TS `AbortSignal` → `Option<&AtomicBool>`** (`true` = aborted; checked at
  batch boundaries and per stored file — same observable partial-result shape:
  `success:false`, `"Aborted"` error prepended).
- **TS methods are infallible-typed but can throw (DB errors)** → Rust methods
  return `crate::error::Result<_>`. `index_file` still returns read/path errors
  *inside* `ExtractionResult.errors` (read_error / path_traversal /
  size_exceeded codes, exact TS messages); `Err(_)` only for DB failures.
- `verbose` kept for parity but unused — every TS verbose log was a
  worker-lifecycle message (spawn/recycle/timeout) with no native equivalent.
- Methods take `&self` (TS field mutation → `RefCell` for the framework-name
  cache). `ExtractionOrchestrator` is NOT `Sync` (holds `&QueryBuilder` → `Rc`).

## Parallelism (parse-worker.ts replacement)

`index_all` processes files in `FILE_IO_BATCH_SIZE = 10` chunks: each chunk is
read + size-checked + parsed via `rayon par_iter` (a free function — the rayon
closure must not capture the orchestrator because the DB handle is `!Sync`),
then results are stored **sequentially on the calling thread** (same
main-thread-stores invariant as TS). Dropped as N/A-native, with TS counter
semantics preserved:

- worker spawn/recycle (`WORKER_RECYCLE_INTERVAL`), parse timeout
  (`PARSE_TIMEOUT_MS` + per-100KB scaling), `rejectAllPending`;
- the WASM memory-corruption **retry pass** (fresh-worker retry, then
  comment-stripped retry) — no WASM heap exists; the `parse_error` catch branch
  around `requestParse` is unreachable natively (extract never throws);
- `scanDirectoryAsync` (event-loop yield variant) — `scan_directory` covers it;
- per-file progress events: emitted in the sequential store loop (report →
  handle → on size-skip a second no-file event), same event count/values as TS,
  but parse happens before the report instead of after (batch pipelining).

## File discovery parity notes

- git path: `git rev-parse --show-toplevel` → parent-repo `check-ignore` guard →
  `git ls-files -z -c --recurse-submodules` + `-o --exclude-standard`, recursing
  into embedded repos (#193/#147/#541 behaviors all covered by tests). git runs
  via a spawn + poll helper with the TS timeouts (5s/30s); kill on deadline.
  The TS 50 MB `maxBuffer` cap is not enforced (unbounded read) — deviation,
  larger outputs now succeed instead of throwing.
- TS compared `path.resolve(gitRoot) !== path.resolve(rootDir)`; Rust uses
  `fs::canonicalize` on both (falls back to the raw path). Strictly *fewer*
  false mismatches (macOS `/var`→`/private/var`); end behavior identical
  because a false mismatch only triggered the check-ignore probe.
- npm `ignore` pkg → `ignore::gitignore::{GitignoreBuilder, Gitignore}`.
  `.ignores(p)` parity is `matched_path_or_any_parents(p, is_dir)` (ancestor
  dirs considered; negations re-include). Defaults seeded via `add_line`, then
  root `.gitignore` + `.codegraphignore` merged so negation can override a
  default (`!vendor/` test-verified). Walk fallback keeps per-directory scoped
  matchers exactly like TS (first matcher that ignores wins; nested negation
  cannot rescue a shallower matcher's ignore — same as TS).
- File reads use `String::from_utf8_lossy` everywhere (Node `'utf-8'` reads
  never throw on invalid bytes — lossy-replace matches).
- Insertion-order preserved where TS used `Set` iteration order
  (git file list, removed/changed node names).

## Other deviations (all behavior-argued)

- `mtimeMs` floor: both sides stored as integer ms (`as_millis()` truncates =
  `Math.floor`).
- io error messages inside `Failed to read file: {msg}` use Rust's io::Error
  Display, not Node's `ENOENT: no such file...` strings (only the prefix is
  asserted anywhere).
- `store_extraction_result` stores the file record's `errors` BEFORE the
  caller fills missing `filePath`s — matching the TS serialization-time order
  (DB errors are pre-fill in TS too).
- Framework `extract()` hooks are infallible in Rust, so the TS
  `Framework extractor '{name}' failed: {err}` catch-warning is unreachable and
  dropped (no `catch_unwind`).
- TS `detectFrameworks` try/catch→false per resolver: Rust `detect()` cannot
  throw; direct call (the canonical registry in `resolution/frameworks/mod.rs`
  documents the same).
- Framework resolvers are constructed fresh per `extract_from_source` call /
  per detection (canonical registry behavior) — the TS singletons' WeakMap
  caches keyed per-context had the same cold-start property per run.

## Cross-module integration done here

- Uses the canonical `resolution::frameworks::{get_all_framework_resolvers,
  get_applicable_frameworks, detect_frameworks}` registry (landed mid-port; my
  earlier local copy was removed).
- `DetectionContext` (private): filesystem-backed `ResolutionContext` for
  pre-index framework detection — graph queries return empty; implements
  `file_exists`/`read_file` (via `validate_path_within_root`) and
  `list_directories` (monorepo manifest probing), exactly the TS shape.
- `FrameworkExtractionResult.references` (`resolution::types::UnresolvedRef`,
  required path/lang) → `types::UnresolvedReference` (optional path/lang)
  conversion lives here (`unresolved_ref_to_reference`, fills `Some(..)`).
- Svelte/Vue extractors get `languages::extractor_for` injected as the
  `ScriptExtractorLookup` (per notes/extraction-svelte-vue.md).
- No other agents' files needed fixing — the full TS extraction suite passed
  against the language/standalone ports on the first complete run.

## Tests ported / deferred

`tests/extraction_test.rs` (289): every `describe` of `extraction.test.ts` —
language detection/support, IDA C (incl. the oversized-dump `indexFiles` case,
run against orchestrator+QueryBuilder), TS/arrow/type-alias/exported-var/file
nodes, Python, Go, Rust, Java (incl. `$anon@` classes), C#, PHP, Swift, Kotlin
(incl. fun-interface misparse cases), Dart, all Import sub-describes (TS/JS,
Python, Rust, Go, Swift, Kotlin, Java, C#, PHP, Ruby + modules, C/C++, Dart,
Liquid), Pascal/Delphi (incl. UAuth/UTypes fixtures), DFM/FMX (incl. MainForm
fixture), **Full Indexing** (via orchestrator + QueryBuilder in place of
`CodeGraph.initSync` — re-verify through the public API once it lands), Path
Normalization, Directory Exclusion, Git Submodules, Nested non-submodule
repos (real `git` invocations, like TS), Scala, Vue (incl. the two
dispatcher-dependent #425 cases the svelte-vue port deferred),
Instantiates+Decorates, Lua, Luau, ObjC, issue regressions (#528/#583/#366/#556),
object-literal extraction (5).

Deferred to the public-API/resolution wiring wave (need `CodeGraph` +
`ReferenceResolver`):
- `extraction.test.ts` "should resolve IDA sub callers and callees after indexAll"
- `object-literal-methods.test.ts` "object-literal method resolution (end-to-end)"

## For the integrator (CodeGraph / CLI / MCP)

- `CodeGraph.indexAll/sync/indexFiles/indexFile/getChangedFiles` map 1:1 onto
  the orchestrator methods above; `scanDirectory` and `extractFromSource` are
  module-level. `src/ui` has its own `IndexProgress { phase: String, current,
  total: u64 }` — adapt at the CLI boundary (`phase.as_str()`).
- `index_all` resets framework detection each run (TS parity); the watcher's
  single-file path (`index_file_with_content`) detects lazily and caches —
  call `reset_detected_frameworks()` when manifests change if you want
  re-detection without a new orchestrator.
- Wire-parity: `IndexResult`/`SyncResult`/`IndexProgress` serde-camelCase with
  optional fields omitted — safe to serialize directly for `--json`/MCP.
