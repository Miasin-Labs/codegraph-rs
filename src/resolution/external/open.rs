//! Opening reachable graphs: read-only, cached per pass, a bounded number
//! at a time.
//!
//! A shard opens `mode=ro&immutable=1` ([`ShardHandle`]); a linked
//! project's index opens through [`crate::atlas::ro`] (`mode=ro` beside a
//! live writer's `-wal`/`-shm`, else `immutable`). Neither creates a file.
//! At most `max_open` graphs stay open (least recently used closed first);
//! a graph that fails to open is not retried within the pass.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use super::graphs::{GraphLocation, Reach, ReachableGraph};
use crate::db::{
    CURRENT_SCHEMA_VERSION,
    Db,
    ExternalGraphKind,
    MIN_READABLE_SCHEMA_VERSION,
    QueryBuilder,
};
use crate::deps::ShardHandle;
use crate::resolution::ResolverContext;
use crate::resolution::lru_cache::LRUCache;
use crate::types::{EdgeKind, Node};

/// Default for how many graphs one pass keeps open.
pub const DEFAULT_MAX_OPEN_GRAPHS: usize = 32;

/// One reachable graph, open.
pub(crate) struct ForeignGraph {
    /// Its position in the pass's [`Reach`].
    pub(crate) index: usize,
    pub(crate) kind: ExternalGraphKind,
    pub(crate) key: String,
    pub(crate) krate: String,
    /// The crate's library root, relative to the graph root.
    pub(crate) lib_root: String,
    /// Where the crate's files are in the graph (`""`: the whole graph is
    /// the crate, as in a shard).
    pub(crate) crate_dir: String,
    /// The graph's read-only connection, for whole-graph queries.
    pub(crate) db: Db,
    /// Resolution over the graph: its nodes, and its sources read from the
    /// graph root (a shard's source directory, the linked project's root).
    pub(crate) context: ResolverContext,
}

impl ForeignGraph {
    fn open(index: usize, graph: &ReachableGraph) -> Option<ForeignGraph> {
        let (queries, root, crate_dir) = match &graph.location {
            GraphLocation::Shard { dir, .. } => {
                let handle = ShardHandle::open_dir(dir)?;
                let root = handle.source_dir().to_string_lossy().into_owned();
                (
                    QueryBuilder::new(handle.queries().db().clone()),
                    root,
                    String::new(),
                )
            }
            GraphLocation::Project { root, crate_dir } => (
                open_project_index(root)?,
                root.to_string_lossy().into_owned(),
                crate_dir.clone(),
            ),
        };
        Some(ForeignGraph {
            index,
            kind: graph.kind,
            key: graph.key.clone(),
            krate: graph.krate.clone(),
            lib_root: graph.lib_root.clone(),
            crate_dir,
            db: queries.db().clone(),
            context: ResolverContext::new(root, queries),
        })
    }

    /// `file_path` (relative to the graph root) is one of the crate's.
    pub(crate) fn in_crate(&self, file_path: &str) -> bool {
        self.crate_dir.is_empty()
            || file_path
                .strip_prefix(self.crate_dir.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

/// A linked project's index, read-only, if its schema is one this build
/// reads.
fn open_project_index(root: &std::path::Path) -> Option<QueryBuilder> {
    let path = crate::db::get_database_path(root);
    if !path.is_file() {
        return None;
    }
    let conn = crate::atlas::ro::open_read_only(&path, Duration::from_millis(500)).ok()?;
    let version: u32 = conn
        .query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
            row.get::<_, Option<u32>>(0)
        })
        .ok()
        .flatten()?;
    if !(MIN_READABLE_SCHEMA_VERSION..=CURRENT_SCHEMA_VERSION).contains(&version) {
        return None;
    }
    conn.set_prepared_statement_cache_capacity(64);
    Some(QueryBuilder::new(Db::new(conn)))
}

/// How many graphs a pass opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenStats {
    /// Opens (a graph closed by the LRU and needed again counts twice).
    pub opened: usize,
    /// Distinct graphs opened.
    pub distinct: usize,
    /// Graphs closed to stay within the bound.
    pub evicted: usize,
    /// Graphs that could not be opened (skipped for the rest of the pass).
    pub failed: usize,
}

impl OpenStats {
    /// Fold another pass's (or worker's) counts in.
    pub fn add(&mut self, other: OpenStats) {
        self.opened += other.opened;
        self.distinct += other.distinct;
        self.evicted += other.evicted;
        self.failed += other.failed;
    }
}

/// An item a path lookup found, by graph index (the graph itself may have
/// been closed since).
#[derive(Debug, Clone)]
pub(crate) struct Located {
    pub(crate) graph: usize,
    pub(crate) node: Node,
    pub(crate) confidence: f64,
    pub(crate) reexported: bool,
}

/// Lookups already answered this pass: graphs are immutable while it runs,
/// and one project asks the same few thousand questions many times.
#[derive(Default)]
pub(crate) struct Memo {
    pub(crate) paths: RefCell<HashMap<(String, Vec<String>, EdgeKind), Option<Located>>>,
    pub(crate) methods: RefCell<HashMap<(usize, String, String), Option<Node>>>,
    pub(crate) fields: RefCell<HashMap<(usize, String, String), Option<Node>>>,
    pub(crate) types: RefCell<HashMap<(usize, String), bool>>,
    pub(crate) visible: RefCell<HashMap<(usize, String), bool>>,
}

/// The reachable graphs of one pass, opened on first need.
pub(crate) struct GraphCache<'r> {
    reach: &'r Reach,
    pub(crate) memo: Memo,
    max_open: usize,
    open: RefCell<LRUCache<usize, Rc<ForeignGraph>>>,
    failed: RefCell<HashSet<usize>>,
    ever_opened: RefCell<HashSet<usize>>,
    stats: Cell<OpenStats>,
}

impl<'r> GraphCache<'r> {
    pub(crate) fn new(reach: &'r Reach, max_open: usize) -> Self {
        let max_open = max_open.max(1);
        GraphCache {
            reach,
            memo: Memo::default(),
            max_open,
            open: RefCell::new(LRUCache::new(max_open)),
            failed: RefCell::new(HashSet::new()),
            ever_opened: RefCell::new(HashSet::new()),
            stats: Cell::new(OpenStats::default()),
        }
    }

    pub(crate) fn reach(&self) -> &'r Reach {
        self.reach
    }

    pub(crate) fn stats(&self) -> OpenStats {
        self.stats.get()
    }

    /// The crate code calls `krate` has a reachable graph.
    pub(crate) fn knows(&self, krate: &str) -> bool {
        self.reach.index_of(krate).is_some()
    }

    /// The graph of the crate code calls `krate`, opened if need be.
    pub(crate) fn get(&self, krate: &str) -> Option<Rc<ForeignGraph>> {
        self.get_index(self.reach.index_of(krate)?)
    }

    pub(crate) fn get_index(&self, index: usize) -> Option<Rc<ForeignGraph>> {
        if let Some(graph) = self.open.borrow_mut().get(&index) {
            return Some(Rc::clone(graph));
        }
        if self.failed.borrow().contains(&index) {
            return None;
        }
        let mut stats = self.stats.get();
        let Some(graph) = ForeignGraph::open(index, self.reach.graph(index)) else {
            stats.failed += 1;
            self.stats.set(stats);
            self.failed.borrow_mut().insert(index);
            return None;
        };
        stats.opened += 1;
        if self.ever_opened.borrow_mut().insert(index) {
            stats.distinct += 1;
        }
        let graph = Rc::new(graph);
        let mut open = self.open.borrow_mut();
        if open.len() >= self.max_open {
            stats.evicted += 1;
        }
        open.set(index, Rc::clone(&graph));
        self.stats.set(stats);
        Some(graph)
    }
}
