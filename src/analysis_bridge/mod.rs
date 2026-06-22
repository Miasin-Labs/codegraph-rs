//! Bridge from the SQLite knowledge graph into `codegraph-analysis`.
//!
//! Reads an already-indexed codegraph database through [`QueryBuilder`] and
//! materializes a [`codegraph_analysis::graph::CodeGraph`] so every analysis
//! the engine offers (communities, dominators, slicing, taint, cascade, DSL
//! queries, token-budgeted context, ...) can run over a codegraph index
//! WITHOUT re-parsing any source.
//!
//! ## Node-kind mapping (codegraph 22 kinds -> analysis 5 kinds)
//!
//! | codegraph                          | analysis   |
//! |------------------------------------|------------|
//! | `function`, `method`               | `Function` |
//! | `class`, `struct`                  | `Struct`   |
//! | `enum`                             | `Enum`     |
//! | `file`, `module`, `namespace`      | `Module`   |
//! | `trait`, `interface`, `protocol`   | `Trait`    |
//! | everything else                    | skipped    |
//!
//! Skipped kinds are not dropped on the floor wholesale - the information
//! that matters to analyses is folded into metadata on the surviving nodes
//! (the analysis crate's documented well-known metadata keys):
//!
//! - `field`/`property` children of a mapped `Struct`/`Enum`/`Trait`
//!   (via `contains` edges) populate the parent's `fields` JSON array.
//! - `enum_member` children of a mapped `Enum` populate `variants`.
//! - `references`/`type_of` edges from a mapped `Function` to a skipped
//!   `field`/`property` populate the function's `accessed_fields` array.
//!
//! ## Edge-kind mapping
//!
//! | codegraph                  | analysis                                              |
//! |----------------------------|-------------------------------------------------------|
//! | `calls`                    | `Calls` (Function->Function only; else skipped)       |
//! | `contains`                 | `Contains` (source Module/Struct/Enum/Trait; else skipped) |
//! | `implements`               | `Implements` (Struct/Enum->Trait; else skipped)       |
//! | `extends`                  | `Implements` when Struct/Enum->Trait, else `References`|
//! | `references`, `imports`, `exports`, `instantiates`, `type_of`, `returns`, `overrides`, `decorates` | `UsesType` when Function->Struct/Enum/Trait, else `References` |
//!
//! Unresolved codegraph references (the `unresolved_refs` table) whose
//! `reference_kind` is `calls` and whose source maps to a `Function` become
//! `UnresolvedCall(name)` edges pointing at a deterministic placeholder
//! `Function` node under the [`UNRESOLVED_FILE`] pseudo-path - the same
//! shape the analysis crate's own adapters and LSP-enrichment layer use.
//!
//! ## Field carrying (flag-gated, off by default)
//!
//! [`BridgeOptions::include_fields`] (env: [`ANALYSIS_FIELDS_ENV`] -
//! `CODEGRAPH_ANALYSIS_FIELDS=1`) additionally carries the index's
//! `field`/`property` nodes through the bridge using the analysis engine's
//! **typed partial-struct metadata contract**
//! ([`codegraph_analysis::partial::set_struct_fields`] /
//! [`codegraph_analysis::partial::set_accessed_fields`]):
//!
//! - every `field`/`property` child of a mapped `Struct` (via `contains`
//!   edges) is registered as a typed `FieldInfo` (name, best-effort type
//!   from the row's signature, visibility) under the engine's
//!   `fields` key - replacing the legacy JSON name-array fold for that
//!   struct;
//! - every `references`/`type_of` edge from a mapped `Function` to a
//!   skipped `field`/`property` registers the engine's comma-separated
//!   `accessed_fields` annotation - replacing the JSON-array fold for
//!   that function.
//!
//! That is what lights up `partial::get_partial_struct` (field-level
//! struct views in `context --strategy analysis`) over bridged data.
//!
//! **Node-count tradeoff** (why this is off by default): fields ride node
//! *metadata*, never nodes - the analysis graph's node count is identical
//! with the flag on or off, deliberately avoiding the node explosion that
//! per-field nodes would cause (field rows commonly outnumber
//! struct/class rows 5:1+ on real codebases). The cost is metadata
//! payload: per-field type strings on every struct and accessed-field
//! lists on every function grow the in-memory graph, the on-disk snapshot,
//! and the graph fingerprint surface. Most analyses never read field data,
//! so the default stays lean. Field names that would corrupt the engine's
//! encoding (containing `;`/`:`/`,`) are skipped and counted
//! ([`BridgeStats::fields_skipped_invalid`]) - never registered mangled.
//!
//! The snapshot-cache envelope records the flag: a cached graph built
//! without fields is **never** served to a with-fields request (or vice
//! versa) - the mismatch is a cache miss and the graph is re-bridged.
//!
//! Every row that cannot be represented under the analysis graph's edge
//! invariants is **skipped, counted, and logged** ([`BridgeStats`]) - never
//! inserted in a shape the engine rejects, and never a panic.
//!
//! ## Determinism
//!
//! Analysis `NodeId`s are content-addressed (`file_path + qualified_name +
//! kind`), all inputs are read in a stable sort order, and metadata arrays
//! are sorted before serialization - so bridging the same index twice (or
//! after a re-index of unchanged sources) yields the identical fingerprint
//! ([`codegraph_analysis::fingerprint::Fingerprintable`]).
//!
//! ## On-disk snapshot cache
//!
//! Bridging re-reads every node/edge/unresolved-ref row, which on large
//! indexes dominates `codegraph analyze` wall-clock. [`build_analysis_graph_cached`]
//! persists the bridged graph under `<project>/.codegraph/analysis/`:
//!
//! - `graph.bin` - the analysis engine's own postcard snapshot
//!   ([`codegraph_analysis::overlay::save_snapshot_bincode`], versioned by
//!   `OVERLAY_SCHEMA_VERSION`).
//! - `meta.json` - host-side envelope: cache schema version, the host crate
//!   version, the **index fingerprint** the snapshot was built from, the
//!   codegraph-id -> analysis-id map, and the [`BridgeStats`].
//!
//! The index fingerprint ([`compute_index_fingerprint`]) is a cheap BLAKE3
//! digest of the SQLite store's row counts, max rowids, `max(updated_at)`,
//! and every file's `(path, content_hash)` pair - any re-index that changes
//! the store changes the fingerprint and invalidates the snapshot. All cache
//! failures (missing, corrupt, schema/version/fingerprint mismatch) degrade
//! to a silent rebuild; the cache is never load-bearing for correctness.
//!
//! One **previous generation** is kept: a store whose fingerprint differs
//! from the cached one first rotates `graph.bin`/`meta.json` (and the
//! optional `complexity.json` sidecar `analyze diff` writes) to `.prev`.
//! That rotated generation is what `codegraph analyze diff --base auto`
//! compares the working tree against ([`load_auto_base_snapshot`]).
//!
//! `CODEGRAPH_ANALYSIS_CACHE_DIR` (the analysis engine's post-rebrand cache
//! env var) overrides the location: the snapshot then lives under
//! `<override>/<workspace-key>/`, where the key is a stable 16-hex digest of
//! the project root so multiple projects can share one override directory.
//! The default location needs no such key - `.codegraph/` is per-project by
//! construction, and its `.gitignore` (`*` + `!.gitignore`) already keeps
//! the cache out of user repositories.
//!
//! [`QueryBuilder`]: crate::db::QueryBuilder

mod builder;
mod cache;
mod mapping;
mod options;
mod result;
mod rows;
mod sidecar;
mod snapshot;
mod stats;

#[cfg(test)]
mod tests;

pub use builder::{build_analysis_graph, build_analysis_graph_with_options};
pub use cache::{
    ANALYSIS_CACHE_DIR_ENV,
    CachedBridge,
    analysis_cache_dir,
    build_analysis_graph_cached,
    build_analysis_graph_cached_with_options,
    compute_index_fingerprint,
};
pub use mapping::{map_edge_kind, map_node_kind};
pub use options::{ANALYSIS_FIELDS_ENV, BridgeOptions};
pub use result::BridgeResult;
pub use sidecar::{StoredComplexity, store_complexity_sidecar};
pub use snapshot::{
    BaseGeneration,
    BaseSnapshot,
    load_auto_base_snapshot,
    load_explicit_base_snapshot,
};
pub use stats::BridgeStats;

/// Pseudo file path used for placeholder nodes that anchor
/// `UnresolvedCall` edges. Deterministic by construction so rebuilds
/// produce identical placeholder `NodeId`s.
pub const UNRESOLVED_FILE: &str = "<unresolved>";
