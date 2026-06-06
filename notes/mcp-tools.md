# MCP tools module port notes (ToolHandler)

Ported `src/mcp/tools.ts` (3264 ln — the largest TS file) → `rust/src/mcp/tools.rs`
(~2,660 ln). Tests: `rust/tests/mcp_tools_test.rs` (41, all passing) + 10 in-module
unit tests. Verification at handoff: `cargo check --all-targets` clean (zero
warnings crate-wide), full `cargo test` green (1,261 tests / 23 binaries, 0 failed).

Files touched: ONLY `src/mcp/tools.rs`, `tests/mcp_tools_test.rs`, and this notes
file. `mcp/mod.rs` already declared `pub mod tools;` — no edit needed. No foreign
module files modified; `Cargo.toml`/`lib.rs` untouched.

## Public API surface (`crate::mcp::tools`)

```rust
// Budget functions — tier tables copied digit-for-digit from TS.
pub fn get_explore_budget(file_count: u64) -> u32;            // <500→1, <5000→2, <15000→3, <25000→4, ≥25000→5
pub struct ExploreOutputBudget { pub max_output_chars, default_max_files,
    max_chars_per_file, gap_threshold: i64, max_symbols_in_file_header,
    max_edges_per_relationship_kind, include_relationships,
    include_additional_files, include_completeness_signal, include_budget_note,
    exclude_low_value_files }
pub fn get_explore_output_budget(file_count: u64) -> ExploreOutputBudget;
    // tiers: <150 → 13000/4/3800/7 · <500 → 18000/5/3800/8 · <5000 → 24000/8/6500/12
    //        <15000 → 24000/8/7000/15 · ≥15000 → 24000/8/7000/15
    // MONOTONIC INVARIANT (max_chars_per_file never shrinks at a larger tier)
    // is pinned by an in-module unit test. TS `getExploreOutputBudget(Infinity)`
    // fallback (stats failure) → call with u64::MAX.

// Staleness formatting (#403) — exported like TS.
pub fn format_stale_banner(stale: &[PendingFile]) -> String;
pub fn format_stale_footer(stale: &[PendingFile]) -> String;

// Tool definitions. Serde output is byte-shape-identical to TS:
// { name, description, inputSchema: { type, properties{ type, description,
//   enum?, default? }, required? } } — properties/keys in TS literal order
// (serde_json `preserve_order` keeps Map insertion order).
pub struct ToolDefinition { pub name, description: String, pub input_schema: InputSchema }
pub struct InputSchema { pub schema_type /*"type"*/, pub properties: serde_json::Map<String,Value>, pub required: Option<Vec<String>> }
pub fn tools() -> Vec<ToolDefinition>;          // all 8, TS array order
pub fn get_static_tools() -> Vec<ToolDefinition>; // CODEGRAPH_MCP_TOOLS-filtered (proxy tools/list)

// Results — serializes { content: [{type:"text", text}], isError? } (isError
// omitted on success, exactly like TS).
pub struct ToolResult { pub content: Vec<ToolContent>, pub is_error: Option<bool> }
pub struct ToolContent { pub content_type /*"type"*/, pub text: String }
impl ToolResult { pub fn text(&self) -> &str }  // convenience: first text content

pub struct ToolHandler;  // !Send/!Sync (holds Rc<CodeGraph>)
impl ToolHandler {
    pub fn new(cg: Option<Rc<CodeGraph>>) -> ToolHandler;
    pub fn set_default_code_graph(&self, cg: Rc<CodeGraph>);
    pub fn set_catch_up_gate(&self, gate: Option<Box<dyn FnOnce()>>); // see deviation 3
    pub fn set_default_project_hint(&self, searched_path: impl Into<String>);
    pub fn has_default_code_graph(&self) -> bool;
    pub fn get_tools(&self) -> Vec<ToolDefinition>;  // allowlist + tiny-repo gating + dynamic explore budget description
    pub fn execute(&self, tool_name: &str, args: &serde_json::Value) -> ToolResult; // sync (TS async)
    pub fn close_all(&self);
}
```

Tools implemented (all): `codegraph_explore` (flow-from-named-symbols, glue
nodes, named-symbol seeding, RWR graph relevance, relevance gate, adaptive
sibling skeletonization / focused view / on-spine god-file, whole-file rule,
clustering + per-file budget, blast radius, hard 25K inline ceiling),
`codegraph_node` (file/line disambiguation, container outline,
ALL-overloads-in-one-call with 12000-char body budget / HARD_CAP 16 /
LIST_CAP 20, caller/callee trail), `codegraph_search`, `codegraph_callers`,
`codegraph_callees`, `codegraph_impact`, `codegraph_status`, `codegraph_files`
(tree/flat/grouped, #426 path normalization, glob).

Env vars (exact TS names/semantics): `CODEGRAPH_MCP_TOOLS` (allowlist, short or
`codegraph_`-prefixed names, filtered in get_tools/get_static_tools AND enforced
on execute), `CODEGRAPH_EXPLORE_LINENUMS` (`"0"` disables cat-n numbering),
`CODEGRAPH_ADAPTIVE_EXPLORE` (`"0"`/`"false"` disables skeletonization).

All steering strings ("do NOT Read", "treat as already Read", skeleton/focused
tags, completeness/budget notes, truncation messages) copied verbatim — explore
output never tells the agent to use Read.

## Deviations from TS (all argued, none behavior-tested-against)

1. **Iteration-order determinism.** TS `Map`/object iteration preserves
   insertion order; Rust `Subgraph.nodes` is a `HashMap` (types.rs — not mine
   to change). Everywhere the TS code iterates `subgraph.nodes`, the port uses
   a private `OrderedNodeMap` seeded **roots-first (in `roots` order), then
   (file_path, start_line, name, id)**. Consequences: deterministic output, but
   *tie* ordering (file order when every sort key ties, "Not shown above" list
   order, impact by-file grouping order) can differ from a TS run. `handle_status`
   sorts nodes-by-kind / languages keys alphabetically (TS shows SQL GROUP BY
   order, which SQLite returns sorted anyway).
2. **Backend line in `codegraph_status`.** TS hardcodes
   `**Backend:** node:sqlite (Node built-in) — full WAL + FTS5`. That statement
   is false for the Rust build; PORTING.md rule 12 mandates the reported backend
   string `"native"`. Port emits
   `**Backend:** native (rusqlite bundled SQLite) — full WAL + FTS5`
   (same line shape, backend token from `cg.get_backend().as_str()`).
   **Flag for the orchestrator: if strict byte parity is preferred over rule 12,
   it's a one-line change.**
3. **Catch-up gate** (TS `setCatchUpGate(Promise|null)`): sync port is a
   one-shot `Box<dyn FnOnce()>` run (and cleared) at the top of the next
   `execute()`. The engine should hand a closure that blocks until its
   catch-up sync finished and swallows/logs its own errors (TS handler
   swallowed rejections; a panic inside the closure here will propagate —
   don't panic in it).
4. **`execute` is sync** (no event loop). Signature takes `&serde_json::Value`
   (non-object → `{}`).
5. **Numeric arg coercion** uses one helper (`Number(x) || default` semantics:
   missing/0/NaN/non-numeric → default, numeric strings parse). TS had two
   subtly different forms (`Number(args.limit) || 10` vs `(args.limit as
   number) || 20`) — the second would pass a *string* through untouched in JS;
   that JS-only type-leak is not replicated.
6. **`kind` filter in search:** TS forwards any string into the SQL filter
   (unknown kind like `"type"` → 0 rows → "No results found"). Rust `NodeKind`
   can't represent unknown kinds, so an unparseable kind short-circuits to the
   identical `No results found for "<query>"` response.
7. **Char counting:** budget thresholds use byte length (`str::len`) where TS
   uses UTF-16 `String.length`; identical for ASCII source, ±a few chars on
   non-ASCII. Truncation cuts are char-boundary-safe (`floor_char_boundary`).
   Input-validation messages (`got N`) use `chars().count()`.
8. **`localeCompare`** in files formatting approximated as case-insensitive
   compare with byte-order tiebreak (`locale_cmp`) — "a" < "B" like ICU, no ICU
   dependency.
9. **CALLABLE sets:** TS includes `'constructor'`, which is not a `NodeKind`
   in either implementation (dead-letter) — omitted from the Rust matches!().
10. **`synthEdgeNote`** ports the 7 TS cases (callback, event-emitter,
    react-render, jsx-render, vue-handler, interface-impl, closure-collection).
    The Rust synthesizer also emits `flutter-build`, which TS has no case for —
    faithfully falls through to the bare edge-kind tag, same as TS would.
11. **Project cache holds `Rc<CodeGraph>`** keyed by both the original
    projectPath and the resolved root (TS parity). `close_all()` may call
    `close()` twice on an instance cached under two keys — `close()` is
    idempotent. The default instance is never cached/closed by the handler
    (server owns it), and the #238 reuse contract is implemented:
    a projectPath resolving to the default project root returns the default
    instance instead of opening a second connection.

## Wiring contract for the MCP server/engine waves

- Construct: `ToolHandler::new(None)` answers `tools/list` statically (or use
  `get_static_tools()` from the proxy before any project opens); after lazy
  open call `set_default_code_graph(Rc<CodeGraph>)`.
- `set_default_project_hint(cwd_or_search_path)` before first execute so the
  "No CodeGraph project is loaded" error (string byte-matched to TS) names the
  searched directory.
- Tool-call dispatch: `handler.execute(name, &params.arguments)` →
  `serde_json::to_value(&result)` is the exact TS wire shape (content array,
  optional `isError`). `tools/list` → `serde_json::to_value(handler.get_tools())`
  (inputSchema key order preserved).
- Worktree-mismatch (#155) and staleness (#403) wrappers are applied inside
  `execute` exactly as TS (status excluded from the banner wrappers, embeds its
  own sections).
- Deferred to the server/daemon waves (per notes/codegraph-api.md):
  `concurrent-locking.test.ts` describe 3 (ToolHandler reuse spy / concurrent
  tool calls — the reuse logic itself is implemented + the same-project paths
  are exercised by the staleness/status preference code) and
  `worktree-detection.test.ts` describe 2 (mismatch on hot read tools +
  detection caching against a real git worktree fixture).
- `MCPServer` re-export at crate root is still the server owner's job
  (notes/codegraph-api.md "Left for later waves").

## Test port map (tests/mcp_tools_test.rs — 41 tests)

- `explore-output-budget.test.ts` → 10 pure-fn budget tests + 7 e2e tests
  (fat `Session` fixture: under-cap output, meta-text gating, relationships-or-
  source, cat-n line numbers default ON, `CODEGRAPH_EXPLORE_LINENUMS=0` off,
  language-neutral gap markers, envelope filter keeps method bodies).
- `adaptive-explore-sizing.test.ts` → 8 tests on the OkHttp-in-miniature
  fixture (fixture sanity ≥3/<3 implements split; off-spine siblings
  skeletonize with bodies elided; on-spine exemplar full; distinct step full;
  `CODEGRAPH_ADAPTIVE_EXPLORE=0` escape hatch; named-callable spare
  (RealCall fix); base+subclasses family file collapses to `· focused` with
  the named base body kept; shared polymorphic method does not spare).
- `explore-blast-radius.test.ts` → 2 tests (dependents + covering tests listed;
  leaf with no dependents omitted).
- `mcp-tool-allowlist.test.ts` → 6 tests + 1 extra (`get_static_tools` honors
  the allowlist — the TS suite covers this via proxy tests).
- `mcp-files-path-normalization.test.ts` → 7 tests (the 7 root-ish variants
  collapsed into one parameterized test; subdir prefix, leading `/`, leading
  `./`, trailing `/`, backslashes, sibling-prefix non-bleed (#426)).
- In-module unit tests (10): both budget tier tables digit-for-digit, the
  monotonic `max_chars_per_file` invariant, glob→regex, `toLocaleString`
  grouping, last-qualifier-part, cat-n numbering, token extraction, plus two
  wire-shape pins: ToolDefinition JSON (camelCase `inputSchema`, TS property
  order, enum/default placement, `required` omission on status/files) and
  ToolResult JSON (`isError` omitted on success, present `true` on error).
- Env discipline: `static ENV_LOCK: RwLock<()>` (same pattern as
  sync_test.rs) — env-mutating tests take the write lock with a restoring
  `EnvVarGuard`; default-env tests take the read lock.

## Notes for reviewers / later waves

- TS `initSync(dir, { config: { include: ['**/*.ts'] } })` in the fixtures has
  no Rust equivalent (`init_sync(root)` takes no config); the default
  include/ignore set indexes the same fixture files (verified: `tests/` dirs
  and `*.test.ts` are NOT in `DEFAULT_IGNORE_DIRS`), so behavior matches.
- `handle_explore`'s `sortedFiles.slice(filesIncluded)` quirk for the
  "Not shown above" list is ported as-is (TS assumes the first N sorted files
  were the N included ones, which is not exactly true when `continue`s
  happened — bug-for-bug parity).
- The `seen_edges` set in `handle_impact` is write-only (TS `mergedEdges` is
  built but `formatImpact` never reads edges) — kept for parity, marked by
  usage only.
