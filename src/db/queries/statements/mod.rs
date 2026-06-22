//! Database Queries
//!
//! Prepared statements for CRUD operations on the knowledge graph.
//! Ported from `src/db/queries.ts`.
//!
//! NOTE on shared helpers: like the TS file (which imports `kindBonus`/
//! `nameMatchBonus`/`scorePathRelevance` from `../search/query-utils`,
//! `parseQuery`/`boundedEditDistance` from `../search/query-parser`, and
//! `isGeneratedFile` from `../extraction/generated-detection`), this file
//! pulls those from `crate::search` and
//! `crate::extraction::generated_detection`. `is_low_value_file` and the
//! FTS-operator filter are defined inline in the TS file, so they live
//! here too (see the "Inline helpers" section near the bottom).

mod cache;
mod edges;
mod files;
mod models;
mod nodes;
mod rows;
mod search;
mod stats;
mod unresolved;

use std::cell::RefCell;

use cache::NodeLru;
pub use models::*;

use crate::db::connection::Db;

pub(super) const SQLITE_PARAM_CHUNK_SIZE: usize = 500;
const MAX_CACHE_SIZE: usize = 1000;

/// Query builder for the knowledge graph database.
pub struct QueryBuilder {
    db: Db,
    // Node cache for frequently accessed nodes (LRU-style, max 1000 entries)
    node_cache: RefCell<NodeLru>,
}

impl QueryBuilder {
    pub fn new(db: Db) -> Self {
        QueryBuilder {
            db,
            node_cache: RefCell::new(NodeLru::new(MAX_CACHE_SIZE)),
        }
    }

    /// Borrow the underlying shared handle (for callers that need raw SQL).
    pub fn db(&self) -> &Db {
        &self.db
    }
}
