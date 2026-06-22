# codegraph-analysis

Graph-based context engine for semantic code analysis. Parses source files with tree-sitter, builds a typed directed graph of code symbols and their relationships, and exposes a pipe-based DSL for querying the graph.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                       GraphSession                           │
│  (facade: owns graph + symbols + events + capabilities)     │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌──────────┐   ┌──────────┐   ┌──────────────────────┐   │
│  │  Builder  │──▶│ CodeGraph │◀──│  DSL Query Engine    │   │
│  └──────────┘   └──────────┘   └──────────────────────┘   │
│       │              │                    │                  │
│       ▼              ▼                    ▼                  │
│  ┌──────────┐   ┌──────────┐   ┌──────────────────────┐   │
│  │ Adapters  │   │Traversal │   │     Formatting       │   │
│  │(tree-sit.)│   │BFS/DFS/  │   │  (token-budgeted)    │   │
│  └──────────┘   │path-find │   └──────────────────────┘   │
│                  └──────────┘                               │
│                       │                                     │
│                       ▼                                     │
│  ┌──────────┐   ┌──────────┐   ┌──────────────────────┐   │
│  │ Analysis  │   │ Cascade  │   │    Persistence       │   │
│  │SCC/dom/PR│   │(sig edit) │   │  (event-sourced)     │   │
│  └──────────┘   └──────────┘   └──────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

## Core Concepts

### Nodes

Exactly **5 node kinds** — `Function`, `Struct`, `Enum`, `Module`, `Trait`. Each node carries a deterministic `NodeId` (content-addressed hash of `file_path + qualified_name + kind`) that remains stable across insertions and removals.

### Edges

7 edge kinds model code relationships:

| Edge Kind        | Source           | Target                  | Semantics                        |
|------------------|------------------|-------------------------|----------------------------------|
| `Calls`          | Function         | Function                | Resolved function call           |
| `UnresolvedCall` | Function         | any                     | Name-only call (needs LSP)       |
| `UsesType`       | Function         | Struct/Enum/Trait       | Type reference                   |
| `References`     | any              | any                     | General reference                |
| `Contains`       | Module/Struct/…  | Function/Struct/…       | Containment hierarchy            |
| `Implements`     | Struct/Enum      | Trait                   | Trait implementation             |
| `ExternalCall`   | Function         | any                     | Call to external crate           |

Edge invariants are enforced at insertion time — invalid source/target kind combinations and non-finite weights are rejected before the graph is mutated.

### Graph Stability

The graph wraps petgraph's `StableDiGraph`. The public API exclusively uses `NodeId` (content-addressed); internal `NodeIndex` slots never leak to downstream crates. This prevents stale-index bugs after node removal/re-addition cycles.

## Modules

| Module | Purpose |
|--------|---------|
| `graph` | Core `CodeGraph` — typed wrapper over `StableDiGraph` with O(1) ID lookup |
| `nodes` | Node types, `NodeId`, `Span`, `Visibility` |
| `edges` | Edge types with per-kind validity rules |
| `builder` | Orchestrates building a graph from files/directories (respects `.gitignore`) |
| `adapter` | `LanguageAdapter` trait + `RustAdapter` (tree-sitter-rust) |
| `dsl` | Pipe-based query DSL: `fn("x") \| callers \| depth 2 \| filter kind=Function` |
| `traversal` | BFS/DFS traversal, shortest path, subgraph extraction |
| `analysis` | SCC, dominators, toposort, page rank, articulation points, bridges, cliques, graph coloring, Floyd-Warshall, Dot export |
| `validation` | Virtual edit simulation — preview affected call sites before committing |
| `cascade` | Generates per-file `CascadeTask`s for signature change propagation |
| `persistence` | Event-sourced graph history with undo, cascade tracing, and versioned on-disk schema |
| `cache` | In-memory memoization (red-green style) keyed on `(path, content_fingerprint)` |
| `fingerprint` | Iteration-order-independent graph digest for cache keys |
| `symbols` | Symbol table: maps handles like `fn:module::name` to `NodeId` + `Span` |
| `enrichment` | LSP enrichment layer — resolves `UnresolvedCall` edges via `LspDataProvider` trait |
| `formatting` | Token-budgeted output rendering for query results |
| `capabilities` | Feature toggles with dependency cascading |
| `partial` | Partial struct views — field-level granularity for context windows |
| `session` | High-level facade (`GraphSession`) that the host application consumes |
| `pass` | Multi-pass analysis pipeline |
| `history` | Graph history tracking |
| `predicates` | Predicate extraction for preconditions analysis |

## Query DSL

The DSL supports the legacy pipe-chain syntax plus the extended expression
grammar used by `codegraph analyze query`: set algebra, path queries,
entrypoints, dominators, trait implementations, reachability patterns, and
aggregation. `analysis/src/dsl/syntax/mod.rs` is the source of truth for the
full grammar and keeps the parser, executor, AST model, and tests split by
role.

Legacy pipe-chain operators:

```text
fn("name")         — select functions by substring match
type("name")       — select structs/enums/traits by substring match
callers            — walk incoming Calls edges (one hop)
callees            — walk outgoing Calls edges (one hop)
depth N            — expand working set N hops outward
filter kind=Kind   — retain only nodes of a given kind
show projection    — control output (fields | signature | body)
taint "var"        — forward BFS over call edges (data-flow proxy)
preconditions      — backward BFS over call edges (control-flow)
```

### Examples

```text
fn("execute_tool") | callees | depth 2
type("Config") | callers
fn("parse") | taint "input" | depth 5
fn("unwrap_unchecked") | preconditions
fn("handle_request") | callers | filter kind=Function | show signature
```

Cycle detection is automatic — mutual recursion terminates and reports `cycles_detected`. Results are capped by `max_nodes` (default 50) and a token budget (default 4000).

## Usage

```rust
use codegraph_analysis::session::GraphSession;

// Index a workspace
let session = GraphSession::from_directory(Path::new("."));

// Run a DSL query
let result = session.query(r#"fn("main") | callees | depth 3"#)?;

// Incremental update after file edit
session.update_file(Path::new("src/lib.rs"), &new_content);

// Validate a signature change before applying
let validation = session.validate_edit(&target_id, 1, 2);

// Generate cascade tasks for sub-agents
let tasks = session.cascade(&target_id, "fn bar(x: i32, y: i32)", "added param y");
```

## Analysis Capabilities

The `analysis` module wraps petgraph's algorithm suite:

- **SCC (Tarjan)** — detect mutual recursion clusters
- **Dominators** — precondition analysis ("what must be true to reach X?")
- **Topological sort** — cascade edit ordering
- **Page rank** — function centrality / importance scoring
- **K-shortest paths** — bounded taint path enumeration
- **Connected components** — independent module detection
- **Articulation points** — critical function identification
- **Bridge edges** — critical edge detection
- **Feedback arc set** — cycle-breaking suggestions
- **Dijkstra** — weighted shortest path
- **Transitive reduction** — display-clean DAG edges
- **Graph coloring** — parallelism analysis
- **Maximal cliques** — module clustering (Bron-Kerbosch)
- **Floyd-Warshall** — all-pairs shortest paths
- **Dot export** — Graphviz visualization

## Persistence

Graph state is event-sourced via `GraphEvent` variants (`NodeAdded`, `NodeRemoved`, `EdgeAdded`, `EdgeRemoved`, `FileReindexed`). Events are wrapped in `VersionedEvent` with a schema tag for forward-compatible on-disk storage. The `EventLog` supports:

- Append-only logging with monotonic IDs
- Undo (pop last event)
- Cascade tracing (follow `parent_event_id` links)
- Snapshot threshold detection

## Dependencies

| Crate | Role |
|-------|------|
| `petgraph` 0.8 | Graph data structure + algorithms |
| `tree-sitter` 0.25.10 (pinned) | Incremental parsing |
| `tree-sitter-rust` 0.24 | Rust grammar |
| `serde` / `serde_json` | Serialization |
| `bincode` 2 | Compact binary serialization |
| `ignore` 0.4 | `.gitignore`-aware file discovery |
| `thiserror` | Error derive |
| `tracing` | Structured diagnostics |

The `tree-sitter` version is pinned because grammar AST shapes are tightly coupled to the parser version — floating minor versions risks graph divergence across builds.

## Testing

```bash
cargo test -p codegraph-analysis
```

Test fixtures live in `tests/fixtures/` and cover:
- Multi-file indexing
- Deep call chains (10-hop taint)
- Mutual recursion (cycle termination)
- Partial struct analysis
- Fingerprint stability across insertion orders
- NodeId stability across remove/re-add cycles
