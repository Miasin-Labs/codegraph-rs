//! The graphs one request reads, opened on first need.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use super::FederationOptions;
use super::deadline::Deadline;
use super::follow::Unavailable;
use super::graph::{GraphId, OpenGraph, dependency_label};
use crate::atlas::Atlas;
use crate::db::{
    CURRENT_SCHEMA_VERSION,
    Db,
    ExternalGraphKind,
    MIN_READABLE_SCHEMA_VERSION,
    QueryBuilder,
};
use crate::deps::{Registry, ShardHandle};
use crate::resolution::external::{FederationHome, Reach, discover};
use crate::resolution::lru_cache::LRUCache;

/// First schema whose indexes hold `external_edges`.
pub(crate) const EXTERNAL_EDGES_SCHEMA: u32 = 10;

/// How many graphs a request opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OpenCounts {
    pub opened: usize,
    pub failed: usize,
}

/// The graphs of one request: read-only, cached, at most `max_open` open.
pub struct GraphSet {
    options: FederationOptions,
    deadline: Deadline,
    open: RefCell<LRUCache<GraphId, Rc<OpenGraph>>>,
    failed: RefCell<HashMap<GraphId, Unavailable>>,
    atlas: OnceCell<Option<Atlas>>,
    registry: OnceCell<Option<Registry>>,
    reach: RefCell<HashMap<PathBuf, Rc<Reach>>>,
    counts: Cell<OpenCounts>,
}

impl std::fmt::Debug for GraphSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GraphSet")
            .field("deadline", &self.deadline)
            .field("counts", &self.counts.get())
            .finish_non_exhaustive()
    }
}

impl GraphSet {
    /// A set for one request; its deadline starts now.
    pub fn new(options: FederationOptions) -> Self {
        let max_open = options.max_open.max(1);
        Self {
            deadline: Deadline::new(options.deadline),
            open: RefCell::new(LRUCache::new(max_open)),
            failed: RefCell::new(HashMap::new()),
            atlas: OnceCell::new(),
            registry: OnceCell::new(),
            reach: RefCell::new(HashMap::new()),
            counts: Cell::new(OpenCounts::default()),
            options,
        }
    }

    pub fn options(&self) -> &FederationOptions {
        &self.options
    }

    pub fn home(&self) -> &FederationHome {
        &self.options.home
    }

    pub fn deadline(&self) -> &Deadline {
        &self.deadline
    }

    pub fn counts(&self) -> OpenCounts {
        self.counts.get()
    }

    /// The atlas, read-only (`None` when there is none).
    pub fn atlas(&self) -> Option<&Atlas> {
        self.atlas
            .get_or_init(|| {
                Atlas::open_read_only(&self.options.home.atlas)
                    .ok()
                    .flatten()
            })
            .as_ref()
    }

    /// The dependency registry, read-only (`None` when there is none).
    pub fn registry(&self) -> Option<&Registry> {
        self.registry
            .get_or_init(|| {
                Registry::open_read_only(&self.options.home.deps.registry_path())
                    .ok()
                    .flatten()
            })
            .as_ref()
    }

    /// The graphs `project_root` reaches (phase 2's discovery; read-only,
    /// no graph opened), once per request.
    pub fn reach(&self, project_root: &Path) -> Rc<Reach> {
        let key = crate::atlas::canonical_root(project_root);
        if let Some(reach) = self.reach.borrow().get(&key) {
            return Rc::clone(reach);
        }
        let reach = Rc::new(discover(&self.options.home, &key));
        self.reach.borrow_mut().insert(key, Rc::clone(&reach));
        reach
    }

    /// How answers name a graph, without opening it: a shard's
    /// `name@version`, a project's atlas name (else its directory name).
    pub fn label_of(&self, id: &GraphId) -> String {
        if let Some(graph) = self.open.borrow_mut().get(id) {
            return graph.label.clone();
        }
        match id.kind {
            ExternalGraphKind::Dependency => dependency_label(&id.key),
            ExternalGraphKind::Project => self.project_name(Path::new(&id.key)),
        }
    }

    /// A registered project's display name, else its directory name.
    pub fn project_name(&self, root: &Path) -> String {
        self.atlas()
            .and_then(|atlas| atlas.project_by_root(root).ok().flatten())
            .map(|project| project.name)
            .unwrap_or_else(|| {
                root.file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.to_string_lossy().into_owned())
            })
    }

    /// The graph `id`, opened read-only on first need.
    pub fn open(&self, id: &GraphId) -> Result<Rc<OpenGraph>, Unavailable> {
        if let Some(graph) = self.open.borrow_mut().get(id) {
            return Ok(Rc::clone(graph));
        }
        if let Some(reason) = self.failed.borrow().get(id) {
            return Err(*reason);
        }
        if self.deadline.expired() {
            return Err(Unavailable::Deadline);
        }
        let mut counts = self.counts.get();
        let opened = match id.kind {
            ExternalGraphKind::Dependency => self.open_shard(id),
            ExternalGraphKind::Project => self.open_project(id),
        };
        let graph = match opened {
            Ok(graph) => Rc::new(graph),
            Err(reason) => {
                counts.failed += 1;
                self.counts.set(counts);
                self.failed.borrow_mut().insert(id.clone(), reason);
                return Err(reason);
            }
        };
        counts.opened += 1;
        self.counts.set(counts);
        self.deadline.watch(graph.queries().db().conn());
        self.open.borrow_mut().set(id.clone(), Rc::clone(&graph));
        Ok(graph)
    }

    fn open_shard(&self, id: &GraphId) -> Result<OpenGraph, Unavailable> {
        let dir = id
            .shard_dir(&self.options.home.deps)
            .ok_or(Unavailable::Missing)?;
        if !dir.join(crate::deps::store::META_FILE).is_file() {
            return Err(Unavailable::Missing);
        }
        let handle = ShardHandle::open_dir(&dir).ok_or(Unavailable::Unreadable)?;
        let meta = handle.meta();
        Ok(OpenGraph::new(
            id.clone(),
            format!("{}@{}", meta.name, meta.version),
            handle.source_dir().to_path_buf(),
            Some(meta.key()),
            false,
            QueryBuilder::new(handle.queries().db().clone()),
        ))
    }

    fn open_project(&self, id: &GraphId) -> Result<OpenGraph, Unavailable> {
        let root = PathBuf::from(&id.key);
        let (queries, schema) = open_project_index(&root)?;
        Ok(OpenGraph::new(
            id.clone(),
            self.project_name(&root),
            root,
            None,
            schema >= EXTERNAL_EDGES_SCHEMA,
            queries,
        ))
    }
}

/// A project's index, read-only and creating nothing, with its schema
/// version — when this build reads that schema.
pub(crate) fn open_project_index(root: &Path) -> Result<(QueryBuilder, u32), Unavailable> {
    let path = crate::db::get_database_path(root);
    if !path.is_file() {
        return Err(Unavailable::Missing);
    }
    let conn = crate::atlas::ro::open_read_only(&path, Duration::from_millis(250))
        .map_err(|_| Unavailable::Unreadable)?;
    let version: u32 = conn
        .query_row("SELECT MAX(version) FROM schema_versions", [], |row| {
            row.get::<_, Option<u32>>(0)
        })
        .ok()
        .flatten()
        .ok_or(Unavailable::Unreadable)?;
    if !(MIN_READABLE_SCHEMA_VERSION..=CURRENT_SCHEMA_VERSION).contains(&version) {
        return Err(Unavailable::Unreadable);
    }
    conn.set_prepared_statement_cache_capacity(32);
    Ok((QueryBuilder::new(Db::new(conn)), version))
}
