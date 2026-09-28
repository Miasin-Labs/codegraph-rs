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
    /// Directories whose types the crate re-exports wholesale
    /// ([`GraphLocation::Shard`]'s `facade_of`).
    pub(crate) facade_of: Vec<String>,
    /// The graph's read-only connection, for whole-graph queries.
    pub(crate) db: Db,
    /// Resolution over the graph: its nodes, and its sources read from the
    /// graph root (a shard's source directory, the linked project's root).
    pub(crate) context: ResolverContext,
}

impl ForeignGraph {
    fn open(index: usize, graph: &ReachableGraph) -> Option<ForeignGraph> {
        let (queries, root, crate_dir, facade_of) = match &graph.location {
            GraphLocation::Shard {
                dir,
                crate_dir,
                facade_of,
                ..
            } => {
                let handle = ShardHandle::open_dir(dir)?;
                let root = handle.source_dir().to_string_lossy().into_owned();
                (
                    QueryBuilder::new(handle.queries().db().clone()),
                    root,
                    crate_dir.clone(),
                    facade_of.clone(),
                )
            }
            GraphLocation::Project { root, crate_dir } => (
                open_project_index(root)?,
                root.to_string_lossy().into_owned(),
                crate_dir.clone(),
                Vec::new(),
            ),
        };
        Some(ForeignGraph {
            index,
            kind: graph.kind,
            key: graph.key.clone(),
            krate: graph.krate.clone(),
            lib_root: graph.lib_root.clone(),
            crate_dir,
            facade_of,
            db: queries.db().clone(),
            context: ResolverContext::new(root, queries),
        })
    }

    /// `file_path` (relative to the graph root) is one of the crate's.
    pub(crate) fn in_crate(&self, file_path: &str) -> bool {
        in_dir(&self.crate_dir, file_path)
    }

    /// `file_path` is in `scope` of the crate.
    pub(crate) fn in_scope(&self, scope: Scope, file_path: &str) -> bool {
        match scope {
            Scope::Own => self.in_crate(file_path),
            Scope::Facade => self.facade_dir(file_path).is_some(),
        }
    }

    /// The directory (named like its crate) of the crates this one is the
    /// facade of that holds `file_path`.
    pub(crate) fn facade_dir(&self, file_path: &str) -> Option<&str> {
        self.facade_of
            .iter()
            .map(String::as_str)
            .find(|dir| in_dir(dir, file_path))
    }
}

/// Where a lookup in a crate's graph looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scope {
    /// The crate's own files.
    Own,
    /// The files of the crates it is the facade of (the toolchain's `std`
    /// over `core` and `alloc`) — for a type's members, and only once the
    /// crate's own items and its re-exports have no answer.
    Facade,
}

/// `file_path` is inside `dir` (`""`: the whole graph).
fn in_dir(dir: &str, file_path: &str) -> bool {
    dir.is_empty()
        || file_path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
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

/// `(graph index, owner path, method)`.
pub(crate) type MethodKey = (usize, String, String);

/// Lookups already answered this pass: graphs are immutable while it runs,
/// and one project asks the same few thousand questions many times.
#[derive(Default)]
pub(crate) struct Memo {
    pub(crate) paths: RefCell<HashMap<(String, Vec<String>, EdgeKind), Option<Located>>>,
    /// `(graph, owner, method)` → the graph index the method was found in
    /// (another crate's, through a re-export, alias or `Deref`) and it.
    pub(crate) methods: RefCell<HashMap<MethodKey, Option<(usize, Node)>>>,
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
