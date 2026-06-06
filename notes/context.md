# context module port notes

Ported from `src/context/{index,formatter,markers}.ts` into
`rust/src/context/{builder,formatter,markers,mod}.rs`. Tests:
`__tests__/context.test.ts` + `__tests__/context-ranking.test.ts` →
`rust/tests/context_test.rs` (23 tests) plus 9 in-module unit tests
(`cargo test --lib context::`). All passing; `cargo check` and
`cargo clippy --lib --tests` are clean for these files.

## Public API (for the wiring wave / `CodeGraph` facade)

```rust
// context::builder
pub struct ContextBuilder;                       // TS ContextBuilder
impl ContextBuilder {
    pub fn new(project_root: impl Into<PathBuf>,
               queries: Rc<QueryBuilder>,
               traverser: GraphTraverser) -> Self;
    /// TS buildContext — returns the FORMATTED string (markdown default, or JSON).
    pub fn build_context(&self, input: &TaskInput, options: &BuildContextOptions)
        -> Result<String>;
    /// Structured variant (the TS `TaskContext` raw-return path).
    pub fn build_task_context(&self, input: &TaskInput, options: &BuildContextOptions)
        -> Result<TaskContext>;
    pub fn find_relevant_context(&self, query: &str, options: &FindRelevantContextOptions)
        -> Result<Subgraph>;                     // confidence: Some(High|Low); None for empty query
    pub fn get_code(&self, node_id: &str) -> Result<Option<String>>;
}
pub fn create_context_builder(project_root, queries, traverser) -> ContextBuilder;

// context::formatter
pub fn format_context_as_markdown(context: &TaskContext) -> String;
pub fn format_context_as_json(context: &TaskContext) -> String;     // JSON.stringify(..., null, 2) parity
pub fn format_subgraph_tree(subgraph: &Subgraph, entry_points: &[Node]) -> String;
pub fn format_bytes(bytes: u64) -> String;

// context::markers
pub const LOW_CONFIDENCE_MARKER: &str = "### ⚠️ Low-confidence match";
```

`mod.rs` re-exports mirror `index.ts`: `ContextBuilder`, `create_context_builder`,
`format_context_as_markdown`, `format_context_as_json`, `LOW_CONFIDENCE_MARKER`
(plus `format_subgraph_tree`/`format_bytes`, exported by formatter.ts).

All defaults match TS: BuildContextOptions → maxNodes 20, maxCodeBlocks 5,
maxCodeBlockSize 1500, includeCode true, format markdown, searchLimit 3,
traversalDepth 1, minScore 0.3. FindRelevantContextOptions → searchLimit 3,
traversalDepth 1, maxNodes 20, minScore 0.3, edgeKinds [], nodeKinds
HIGH_VALUE (the 14-kind list excluding imports/exports/files). `None` option
fields take the default (TS spread-merge semantics).

## Deviations from TS (deliberate)

1. **Return type of `build_context`:** TS returns `TaskContext | string`; the
   `format` enum has only markdown/json so the raw-object path is unreachable —
   `build_context` returns `Result<String>` and `build_task_context` exposes
   the struct for callers (MCP) that want it.
2. **Node-map ordering.** `crate::types::Subgraph.nodes` is a `HashMap`; the
   TS algorithm depends on `Map` insertion order in several places. Handling:
   - The builder keeps a private insertion-ordered map (`OrderedNodes`)
     through the whole `findRelevantContext` pipeline, so every
     order-sensitive step (trim/fill, per-file diversity cap eviction,
     non-production cap eviction, code-block selection) behaves exactly like
     TS.
   - Subgraphs returned by the graph layer (BFS / type hierarchy) also lose
     their Map order; the builder reconstructs discovery order as
     roots → edge endpoints in edge order → leftovers (deterministic sort).
     This approximates TS BFS-insertion order; it only matters for which
     hierarchy nodes win the `maxNodes/4` budget on overflow.
   - The **formatter** orders nodes as: entry points (in entry-point order)
     first, then remaining sorted by (filePath, startLine, name, id). The TS
     output orders the remainder by Map insertion; structure/format is
     byte-identical, the *selection of the 10 "Related Symbols"* and the JSON
     `nodes` array order can differ from a TS run when >10 non-entry nodes
     exist. Same approach in `format_subgraph_tree`'s "Other relevant
     symbols".
3. **Errors:** TS lets DB/traverser throws propagate from
   `findRelevantContext` except the spots it try/catches; mirrored exactly —
   exact-name lookup and text-search failures are caught + `log_debug`'d,
   `get_dominant_file` errors are swallowed, everything else propagates as
   `crate::error::Result`.
4. **UTF-16 string semantics** preserved where scores/cuts depend on JS
   `.length` (brevity bonuses, `length < 3` guards, code-block truncation
   `slice(0, maxBlockSize)`, total_code_size stat). Truncation clamps to a
   char boundary (JS can split a surrogate pair; Rust can't represent that).
5. **JS-truthiness** preserved: empty-string code blocks are skipped
   (`if (code)`), `node.signature`/`startLine` falsy checks, metadata
   `m?.synthesizedBy` / `m.via || 'child'` truthiness via a `js_truthy`
   helper; `String(value)` via `js_string` (objects stringify as JSON, not
   `[object Object]` — only reachable if a synthesizer writes a non-scalar
   `via`/`event`, which none do).
6. **`fs.readFileSync(utf-8)`** → `fs::read_to_string`: invalid-UTF-8 files
   return an error → logged → `None` (TS would lossily decode). Same
   `validate_path_within_root` gate before any read.
7. **JSON formatter** uses serde structs to lock the exact TS
   `serializeNode`/`serializeEdge` key order (`id, kind, name, qualifiedName,
   filePath, language, startLine, endLine, signature, docstring, visibility,
   isExported, isAsync, isStatic`; note signature BEFORE docstring, unlike
   `types::Node`) and omits absent optionals like `JSON.stringify` omits
   `undefined`. Pretty-printing matches `JSON.stringify(x, null, 2)`.
8. **`formatBytes`** takes `u64`; `{:.1}` vs JS `toFixed(1)` differ only in
   half-way rounding ties (banker's vs round-half-away), invisible for real
   byte counts.
9. `Array.sort()` on related-files is byte-wise in Rust vs UTF-16-unit-wise
   in JS — identical for all BMP/ASCII paths.

## TS quirks preserved verbatim (do NOT "fix")

- Step 2b prefix search and the 5b/5c LIKE channels propagate DB errors (no
  try/catch in TS) while steps 2/3 swallow them.
- The hierarchy pass-1 inner loop adds nodes **without** checking the
  `maxNodes/4` budget (only the outer per-entry-point loop checks); pass 2
  checks per node. Pass-1 edge merge does NOT verify endpoints are in the
  node set; pass 2 does.
- Call-paths DFS budget is `budget-- <= 0` (checked-then-decremented), shared
  across all starts; chains need ≥3 nodes and ≥2 roots; kept chains are
  deduped by `join('>')` substring containment; max 3 chains.
- The dominant-file boost fires when `edgeCount >= 3 * nextEdgeCount`
  (trivially true when nextEdgeCount is 0).
- Non-production eviction keeps the FIRST `max(3, ceil(maxNodes*0.15))`
  test-file nodes in insertion order (not by score) and also drops evicted
  ids from `roots`.
- `extractSymbolsFromQuery` regexes use the JS ASCII `\b` → `(?-u:\b)`;
  snake_case pattern keeps the TS `/i` flag; the 153-entry commonWords list
  was diffed element-by-element against the TS source.

## Integration needs

- Facade (`src/index.ts` port): construct via
  `create_context_builder(project_root, Rc::clone(&queries), GraphTraverser::new(Rc::clone(&queries)))`;
  `CodeGraph::get_code` → `builder.get_code`, `findRelevantContext` /
  `buildContext` map 1:1. Pass `&Default::default()` for omitted options.
- MCP `tools.ts` port: `codegraph_explore` glue calls `find_relevant_context`
  and detects `LOW_CONFIDENCE_MARKER` in build_context markdown — import the
  marker from `crate::context` (leaf module, no heavy deps).
- `Subgraph.confidence` is `Some(High|Low)` from `find_relevant_context`
  (only `None` for the empty-query early return), matching TS.
- No changes needed in shared files; no blockers found in dependencies
  (graph/db/search/extraction::generated_detection all sufficient).
