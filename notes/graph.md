# graph module port notes

Ported from `src/graph/{traversal,queries,index}.ts` into
`rust/src/graph/{traversal,queries,mod}.rs`. Tests:
`__tests__/graph.test.ts` (all cases) → `rust/tests/graph_test.rs`
(31 tests, all passing).

## Verification status (IMPORTANT)

At port time `cargo check` on the shared crate was broken by an in-flight
file owned by another agent — `src/resolution/path_aliases.rs:405` has an
unterminated raw string (`r#"…{"#/*": …"#`; the inner `"#` terminates the
raw string early, E0758 unterminated block comment). **Not mine to fix.**
I validated my module in a full copy of the crate at `/tmp/cg-graph-check`
with only that one line patched (`r#"…"#` → `r##"…"##`): there,
`cargo check --lib` is clean (zero warnings in graph files),
`cargo clippy --lib --tests` has zero findings in `src/graph/` /
`tests/graph_test.rs`, and `cargo test --test graph_test` passes 31/31.
Once the resolution agent fixes their raw string, `cargo test --test
graph_test` should pass in-repo unchanged.

## Public API (for the wiring wave / `CodeGraph` facade)

```rust
// graph::traversal
pub struct GraphTraverser;                       // TS GraphTraverser
impl GraphTraverser {
    pub fn new(queries: Rc<QueryBuilder>) -> Self;
    pub fn traverse_bfs(&self, start_id: &str, options: &TraversalOptions) -> Result<Subgraph>;
    pub fn traverse_dfs(&self, start_id: &str, options: &TraversalOptions) -> Result<Subgraph>;
    pub fn get_callers(&self, node_id: &str, max_depth: u32) -> Result<Vec<NodeRef>>;  // TS default 1
    pub fn get_callees(&self, node_id: &str, max_depth: u32) -> Result<Vec<NodeRef>>;  // TS default 1
    pub fn get_call_graph(&self, node_id: &str, depth: u32) -> Result<Subgraph>;       // TS default 2
    pub fn get_type_hierarchy(&self, node_id: &str) -> Result<Subgraph>;
    pub fn find_usages(&self, node_id: &str) -> Result<Vec<NodeRef>>;
    pub fn get_impact_radius(&self, node_id: &str, max_depth: u32) -> Result<Subgraph>; // TS default 3
    pub fn find_path(&self, from_id: &str, to_id: &str, edge_kinds: &[EdgeKind])
        -> Result<Option<Vec<PathStep>>>;        // TS default edge_kinds: [] (= all)
    pub fn get_ancestors(&self, node_id: &str) -> Result<Vec<Node>>;
    pub fn get_children(&self, node_id: &str) -> Result<Vec<Node>>;
}
pub struct PathStep { pub node: Node, pub edge: Option<Edge> } // serde camelCase; edge: None → JSON null (TS parity)

// graph::queries
pub struct GraphQueryManager;                    // TS GraphQueryManager
impl GraphQueryManager {
    pub fn new(queries: Rc<QueryBuilder>) -> Self;   // builds its own GraphTraverser from the Rc
    pub fn get_context(&self, node_id: &str) -> Result<Context>;  // Err "Node not found: <id>" (CodeGraphError::Other)
    pub fn get_file_dependencies(&self, file_path: &str) -> Result<Vec<String>>;
    pub fn get_file_dependents(&self, file_path: &str) -> Result<Vec<String>>;
    pub fn get_exported_symbols(&self, file_path: &str) -> Result<Vec<Node>>;
    pub fn find_by_qualified_name(&self, pattern: &str) -> Result<Vec<Node>>; // * and ? globs
    pub fn get_module_structure(&self) -> Result<HashMap<String, Vec<String>>>;
    pub fn find_circular_dependencies(&self) -> Result<Vec<Vec<String>>>;
    pub fn get_node_metrics(&self, node_id: &str) -> Result<NodeMetrics>;
    pub fn find_dead_code(&self, kinds: Option<&[NodeKind]>) -> Result<Vec<Node>>; // None → [Function, Method, Class]
    pub fn get_filtered_subgraph<F: Fn(&Node) -> bool>(&self, filter: F, include_edges: bool) -> Result<Subgraph>; // TS default include_edges: true
    pub fn get_traverser(&self) -> &GraphTraverser;
}
pub struct NodeMetrics { incoming_edge_count, outgoing_edge_count, call_count,
                         caller_count, child_count, depth: usize }  // serde camelCase

// graph (mod.rs, mirrors index.ts re-exports)
pub use queries::{GraphQueryManager, NodeMetrics};
pub use traversal::{GraphTraverser, PathStep};
```

Uses `crate::types::{Subgraph, TraversalOptions, Direction, Context, NodeRef}`
throughout — no new graph-shaped types beyond `PathStep`/`NodeMetrics` (both
mirror inline TS shapes).

## Deviations from TS (deliberate)

1. **Ownership:** TS passes one `QueryBuilder` instance by reference into both
   classes. Rust uses `Rc<QueryBuilder>` (Db is already `Rc<Connection>`-based
   and `!Send`, see notes/db.md — single-threaded confinement is unchanged).
   The facade should create `Rc::new(QueryBuilder::new(conn.get_db()?))` and
   hand clones of the `Rc` to `GraphTraverser::new` / `GraphQueryManager::new`.
2. **Errors:** every method returns `crate::error::Result` (TS lets DB throws
   propagate). `get_context` on a missing node → `CodeGraphError::Other` with
   the exact TS message `Node not found: <id>`.
3. **Default arguments** (TS optional params) become explicit parameters; the
   TS defaults are documented per method above and in rustdoc — the facade/MCP
   wiring must apply them (`get_callers(id, 1)`, `get_call_graph(id, 2)`,
   `get_impact_radius(id, 3)`, `find_path(a, b, &[])`,
   `find_dead_code(None)`, `get_filtered_subgraph(f, true)`,
   `traverse_*(id, &TraversalOptions::default())`).
4. **`maxDepth: Infinity` default** → `TraversalOptions.max_depth: None`
   (resolved internally; `limit` default 1000, `direction` Outgoing,
   `include_start` true — same as TS `DEFAULT_OPTIONS`).
5. **Order-sensitive collections:**
   - `Subgraph.nodes` is `HashMap<String, Node>` (crate::types, shared) — TS
     `Map` preserves insertion order, HashMap doesn't. No TS consumer relies
     on nodes-map order (edges Vec keeps traversal order), but flagging for
     the context/MCP owners.
   - `get_module_structure` returns `HashMap<String, Vec<String>>` — TS Map
     key order (insertion) is lost; the per-directory file Vec keeps order.
   - `get_file_dependencies`/`get_file_dependents` preserve TS `Set` insertion
     order via Vec + seen-HashSet (matters for `find_circular_dependencies`
     determinism).
6. **`find_by_qualified_name`** uses the `regex` crate with the same
   escape-set/glob translation as TS (`*`→`.*`, `?`→`.`); JS vs Rust regex
   semantics are identical for these fully-escaped patterns. Regex build
   errors map to `CodeGraphError::Other` (TS would throw from `new RegExp`).
7. **BFS edge-priority sort:** `sort_by_key` (stable) replicates JS stable
   `Array.sort` with the contains(0)/calls(1)/other(2) priority.

## TS quirks preserved verbatim (do NOT "fix" without a TS-side change)

- **`get_type_hierarchy` never returns descendants:** in TS,
  `getTypeAncestors` and `getTypeDescendants` share one `visited` set and
  ancestors runs first, marking the focal node visited — so the descendants
  walk returns immediately. Preserved; locked in by
  `type_hierarchy_preserves_ts_quirk_descendants_not_traversed` in
  `tests/graph_test.rs`. (The TS test suite never asserted descendants.)
- `find_usages` does NOT filter `contains` edges (a file "uses" everything it
  contains) — only `get_impact_radius` (#536) and `get_context` exclude them.
- BFS `limit` is checked at dequeue time, so the node map can overshoot the
  limit by one step's fan-out — same as TS.
- `get_impact_radius` recurses into container children **at the same depth**
  (they're part of the same symbol), and excludes incoming `contains` (#536).
- `get_ancestors` follows only the **first** containing edge per level.
- `get_context` is intentionally N+1 per edge (TS is too; `get_node_by_id`
  hits the QueryBuilder LRU cache).

## Tests ported (`tests/graph_test.rs`, 31 passing)

Every case from `__tests__/graph.test.ts`. The TS suite builds its graph by
indexing real TS files through the `CodeGraph` facade (not yet ported), so the
fixture is built directly via `QueryBuilder` (real SQLite in a tempdir, no
mocks) with the node/edge shapes extraction+resolution produce for the TS
fixture files — including the batched-BFS chain case (`root/middleA/middleB/
leaf`) and the #536 sibling-drag regression. Where the TS test only asserted
`Array.isArray(...)` (trivially true in Rust), the Rust test asserts the
actual expected content of the deterministic fixture. Extra coverage beyond
the TS suite: missing-start-node subgraph, DFS smoke, type-hierarchy quirk
lock-in, a real circular-dependency positive case, exported symbols,
qualified-name globs (`*`/`?`), module structure, filtered subgraph.

## Integration needs

- Facade (`src/index.ts` port): construct
  `Rc<QueryBuilder>` once; `traverse()` maps to `traverse_bfs` (the TS facade
  only exposes BFS); apply the TS default arguments listed above.
- MCP `codegraph_impact` → `get_impact_radius(id, 3)`; callers/callees tools
  → `get_callers/get_callees(id, 1)` unless the tool passes a depth.
- JSON wire shapes: `Subgraph`/`Context`/`NodeRef` come from `crate::types`
  (camelCase). `PathStep` serializes `edge: null` for the first hop like TS.
