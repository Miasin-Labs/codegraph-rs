# Engine-side close work (gap-matrix #18, #19, dominators perf)

Date: 2026-06-06. Companion to `notes/prototype-gap-matrix.md`. Scope:
`analysis/src/**` + `analysis/tests/**` only — the host package (`src/`) is
untouched. This note documents the **new public APIs precisely** so the
Tier-1 CLI agent (and any future host work) can consume them without reading
the engine source.

Verified at time of writing: `cargo test -p codegraph-analysis` → 797 lib +
6 fingerprint_stability + 4 node_id_stability, 0 failed; `cargo clippy -p
codegraph-analysis --all-targets` zero warnings; `cargo fmt -p
codegraph-analysis -- --check` clean; `cargo check -p codegraph-rs --lib`
still green (all changes are additive; one signature-stable rewrite, see
§3).

---

## 1. (#19) Label-constrained reachability is now a DSL operator

`label_reachability` was verified dead code (declared in `lib.rs`, no
callers). It is now wired into the DSL as a **pipe-chain postfix operator**,
plus a public string-pattern parser hosts can reuse for CLI flags.

### DSL surface (rides `GraphSession::query` / `analyze query` for free)

```
reachable := 'reachable' 'via' STRING ( 'incoming' | 'outgoing' )?
```

- Pattern string: whitespace-separated atoms, each an edge label with an
  optional repetition suffix — `Calls` (exactly one), `Calls+` (one or
  more), `Calls*` (zero or more), `any` / `any+` / `any*` (wildcard label).
  Valid labels: `Calls`, `UnresolvedCall`, `UsesType`, `References`,
  `Contains`, `Implements`, `ExternalCall`, `Extends`, `Returns`, `TypeOf`,
  `any`. Payload-carrying kinds (`UnresolvedCall`, `ExternalCall`) match by
  discriminant, i.e. any payload.
- Semantics: replaces the working set with every node reachable via a path
  whose **edge-label sequence** matches the whole pattern (product BFS over
  `(node, NFA-state)` — cycle-safe). Distinct from `depth N`
  (label-agnostic) and from path-query `via K` (≥1 edge of kind K anywhere).
  Seeds survive only if the pattern matches the zero-length path.
- Direction: default `outgoing`; `incoming` answers "who reaches me via
  this sequence". Unknown direction idents are a parse error.
- Honesty: an unseeded `reachable via` returns an empty node set **with a
  metadata line saying so** (`…empty working set — seed with fn("...")…`);
  a seeded run pushes `reachable via "<pattern>" <direction> seeds=N
  reached=M`. Typo'd edge labels fail at **parse** time with position +
  the valid-label list.
- Examples:
  - `fn("handler") | reachable via "Calls+"` — transitive callees only.
  - `fn("render") | reachable via "Calls+" incoming` — transitive callers.
  - `type("S") | reachable via "UsesType" incoming` — functions using S.
  - `(fn("a") | reachable via "Calls+") diff fn("b")` — composes with set
    algebra / `PipeFrom` like any other op.

### New engine API (`analysis/src/label_reachability.rs`)

```rust
pub fn parse_pattern(input: &str) -> Result<Vec<PatternAtom>, PatternParseError>
pub fn format_pattern(pattern: &[PatternAtom]) -> String   // round-trips parse_pattern
pub struct PatternParseError { pub atom: String, pub reason: String } // thiserror Display
```

`PatternAtom` / `EdgeMatcher` now derive `PartialEq, Eq` (needed by
`DslOp`). The pre-existing entry points are unchanged:
`reachable_targets(graph, source, pattern, direction) -> Vec<NodeId>`
(sorted) and `reachable(graph, source, target, pattern, direction) -> bool`
(short-circuits). If the Tier-1 agent ships `analyze reachable <symbol>
--edge-pattern "<p>" [--incoming]`, use `parse_pattern` + these two —
do not re-implement label parsing.

`DslOp::ReachableVia { pattern: Vec<PatternAtom>, direction:
ClosureDirection }` is the AST shape (`crate::closure::ClosureDirection::
{Outgoing, Incoming}`). The plan optimiser deliberately does **not** hoist
filters past it (not in `can_swap_filter_with`) — filtering before/after a
reachability expansion is not order-independent.

## 2. (#18) Field metadata through the bridge — partial structs over bridged data

The bridge drops host field/property nodes, so `partial::get_partial_struct`
had no data to render. The model already carries the data as **node
metadata** (no new node kind needed — `partial.rs` reads two metadata keys);
what was missing was a public, typed registration surface. That now exists
at three levels.

### Metadata contract (`analysis/src/partial.rs`)

- `pub const STRUCT_FIELDS_KEY: &str = "fields"` — on a `Struct` node;
  value encoded as `name:type:pub;name:type:priv;…`. The **first `:`** ends
  the name, a **trailing `:pub` / `:priv`** carries visibility, everything
  between is the type — so qualified types (`std::path::PathBuf`) and
  generic types with commas survive. (The decoder previously truncated a
  qualified type at its first `:`; fixed + pinned by tests. Legacy 2-part
  `name:type` entries still parse, visibility defaults private.)
- `pub const ACCESSED_FIELDS_KEY: &str = "accessed_fields"` — on a
  `Function` node; comma-separated field names.

### Typed graph-level API (use this from the bridge, pre-session)

```rust
pub fn set_struct_fields(graph: &mut CodeGraph, struct_id: &NodeId,
                         fields: &[FieldInfo]) -> Result<(), PartialStructError>
pub fn set_accessed_fields(graph: &mut CodeGraph, fn_id: &NodeId,
                           accessed: &[String]) -> Result<(), PartialStructError>
pub fn encode_fields_metadata(fields: &[FieldInfo]) -> Result<String, PartialStructError>
pub fn parse_fields_metadata(raw: &str) -> Vec<FieldInfo>      // now public
pub fn try_get_partial_struct(graph: &CodeGraph, struct_id: &NodeId,
                              accessing_fn: &NodeId) -> Result<PartialView, PartialStructError>
```

- `FieldInfo { name, type_str, is_public }` and `PartialView` now derive
  `PartialEq, Eq`.
- Validation (graph untouched on error): field names non-empty and free of
  `;` `:` `,`; types must not contain `;`. Errors:
  `PartialStructError::{NodeNotFound, WrongKind{id,expected,got},
  InvalidFieldName, InvalidFieldType, NoFieldData, Disabled}` (thiserror).
- `set_struct_fields` requires a `Struct` node and **overwrites** any prior
  list; `set_accessed_fields` requires a `Function` node; an **empty slice
  removes** the annotation (absent data ≠ "accesses zero fields").
- Both route through `CodeGraph::update_node_metadata`, so the graph
  revision bumps (`since N` queries see it) and the metadata-key index stays
  in lockstep. (Fixed a latent bug there: a closure that *removed* a key
  left a stale index bucket because eviction used the post-mutation node;
  eviction now uses a pre-mutation snapshot. Pinned by
  `graph::tests::update_node_metadata_evicts_removed_keys_robust`.)
- `try_get_partial_struct` is `get_partial_struct` with precise errors —
  CLI/context renderers should use it to print an **honest capability note**
  ("no field data registered for struct …") instead of an empty section.
  `get_partial_struct`'s `Option` shape is unchanged.

### Flag-gated session API (`analysis/src/session.rs`)

```rust
impl GraphSession {
    pub fn register_struct_fields(&mut self, struct_id: &NodeId,
        fields: &[partial::FieldInfo]) -> Result<(), partial::PartialStructError>
    pub fn register_accessed_fields(&mut self, fn_id: &NodeId,
        accessed: &[String]) -> Result<(), partial::PartialStructError>
    pub fn partial_struct(&self, struct_id: &NodeId, accessing_fn: &NodeId)
        -> Result<partial::PartialView, partial::PartialStructError>
}
```

All three are gated on the **pre-existing** `Capability::PartialStruct`
(env kill-switch `CODEGRAPH_ANALYSIS_CAP_PARTIAL_STRUCT=0`) and return
`PartialStructError::Disabled` rather than silently no-oping. The register
methods also invalidate the session's memoised-query cache for the touched
node. Host bridges that build a `CodeGraph` before `from_snapshot` can use
the graph-level setters directly (no capability gate at that level — gating
is a session/render concern).

NodeIds are the keys throughout — the host maps its own ids via
`BridgeResult::id_map` exactly as `analyze` symbol resolution already does.

## 3. Dominators: batch API — compute the tree once

`analyze dominators` recomputed the dominator tree **per reported node**
because `analysis::dominator_chain(graph, root, target)` was the only
NodeId-level entry (and `CodeGraph::inner()` is intentionally
`pub(crate)` — it stays that way). New public batch handle in
`analysis/src/analysis.rs`:

```rust
pub fn dominator_tree(graph: &CodeGraph, root: &NodeId) -> Option<DominatorTree>
    // None iff root not in graph. One Cooper-Harvey-Kennedy pass + O(V)
    // NodeIndex→NodeId translation.

pub struct DominatorTree { /* opaque: root + idom map, NodeId-keyed */ }
impl DominatorTree {
    pub fn root(&self) -> &NodeId
    pub fn immediate_dominator(&self, node: &NodeId) -> Option<&NodeId> // None: root/unreachable
    pub fn chain(&self, node: &NodeId) -> Vec<NodeId>   // idom outward to root; empty: root/unreachable
    pub fn chain_report(&self, node: &NodeId) -> DominatorChain // same shape dominator_chain returns
    pub fn dominates(&self, a: &NodeId, b: &NodeId) -> bool     // reflexive
    pub fn idoms(&self) -> impl Iterator<Item = (&NodeId, &NodeId)> // whole tree, unspecified order
    pub fn len(&self) -> usize        // reachable nodes excluding root
    pub fn is_empty(&self) -> bool
}
```

- **Host migration (lifts the `--top` cap honestly):** in `analyze
  dominators`, replace the per-node `dominator_chain` loop with one
  `dominator_tree(&graph, &root)` + `tree.chain_report(&node)` per reachable
  node. Equivalence is pinned by
  `analysis::tests::dominator_tree_batch_matches_per_node_chains_normal`.
- `dominator_chain` keeps its exact signature and `None` semantics
  (missing root **or missing target** → `None`); it is now a documented
  one-shot convenience implemented over `dominator_tree`.
- The tree is a **snapshot**: it does not observe graph mutations made after
  it is built (pinned by `dominator_tree_linear_chain_and_snapshot_robust`).
- `crate::dominators::Dominators` (the generic petgraph-trait version)
  is unchanged; `DominatorTree` is the NodeId-keyed public face.

## 4. Tests added (all in-module `#[cfg(test)]`)

- `label_reachability`: pattern parse/format round-trip, parse-driven
  reachability, bad-atom rejection (named offending atom), discriminant
  matching for payload kinds. (+4)
- `dsl`: parse shape incl. direction keyword; bad-input parse errors
  (pattern, missing `via`, unknown direction); label-sequence semantics
  (excludes `UsesType` leaks, excludes seed under `+`); `incoming`; honest
  empty-seed metadata; composition with set algebra + regression guard that
  path-query `via Calls` still parses. (+5)
- `partial`: encode/parse round-trip with qualified + generic types; legacy
  format compatibility; end-to-end register→view on bridge-like bare nodes;
  overwrite-not-append; full validation matrix + clearing semantics. (+4)
- `session`: registration + view + capability gate (`Disabled` from all
  three methods), hermetic w.r.t. `CAP_*` env. (+1)
- `graph`: `update_node_metadata` removed-key index eviction + revision
  stamping + missing-node contract. (+1)
- `analysis`: batch≡per-node equivalence, dominates/boundaries (orphan,
  ghost root, ghost target), linear-chain + snapshot semantics. (+3)

## 5. Notes for the Tier-1 CLI agent

- `analyze query` gets `reachable via` for free (it parses inside both the
  legacy pipe grammar and the extended expression grammar). If you document
  operators, copy the grammar line + pattern syntax from §1.
- A dedicated `analyze reachable <from> --edge-pattern "<p>" [--incoming]`
  (gap-matrix wording) is now trivial: resolve the symbol as `analyze`
  already does, `label_reachability::parse_pattern`, then
  `reachable_targets`. Print the `format_pattern` echo in the report so
  `--json` consumers see the normalized pattern.
- `analyze dominators`: switch to `dominator_tree` per §3; keep the
  report shape (`DominatorChain` unchanged) — only the recompute goes away.
- Partial structs: a `context --strategy analysis` renderer (or `analyze
  struct <S> --as-seen-by <fn>`) should call `GraphSession::partial_struct`
  and print the error string verbatim on `Err` — every variant is a
  one-line honest capability note (`Disabled` names the env var;
  `NoFieldData` says registration hasn't happened). Never render an empty
  field list for an `Err`.
