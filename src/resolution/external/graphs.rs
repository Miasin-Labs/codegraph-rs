//! Which graphs a project's references may resolve into.
//!
//! * **Dependencies** — the versions `deps/registry.db` records for the
//!   project (its lockfiles), each through its shard when one has been
//!   built (`meta.json` readable by this build). Registry and git sources
//!   only: a path dependency is a linked project.
//! * **Linked projects** — the atlas's `cargo_path_dep` links from this
//!   project into another registered project: the dependency's crate is a
//!   directory of that project, resolved in its own index.
//!
//! Each graph is keyed by the name code uses for its crate. Two versions
//! under one name are resolved to the direct one; with no single direct
//! version the name is left out (never guessed). Discovery reads only the
//! registry, the atlas and `meta.json` files — no graph is opened here.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use super::manifest::LibTarget;
use crate::atlas::{Atlas, LinkKind, canonical_root};
use crate::db::ExternalGraphKind;
use crate::deps::{DepSource, DepsHome, Ecosystem, ShardMeta, dependencies_of_in};

/// Where the machine-wide stores are: the dependency store and the atlas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FederationHome {
    pub deps: DepsHome,
    pub atlas: PathBuf,
}

impl FederationHome {
    /// `codegraph_home()/deps` and `codegraph_home()/atlas.db`.
    pub fn from_env() -> Self {
        FederationHome {
            deps: DepsHome::from_env(),
            atlas: crate::atlas::atlas_path(),
        }
    }

    /// Both under one explicit home directory (tests, tools).
    pub fn at(codegraph_home: &Path) -> Self {
        FederationHome {
            deps: DepsHome::at(codegraph_home.join("deps")),
            atlas: codegraph_home.join("atlas.db"),
        }
    }
}

/// Where one reachable graph's index is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphLocation {
    /// A dependency shard directory (`codegraph.db` + `meta.json`); node
    /// paths are relative to `source_dir`.
    Shard { dir: PathBuf, source_dir: PathBuf },
    /// A linked project's root; the crate is `crate_dir` inside it (`""`
    /// when the crate is the project root).
    Project { root: PathBuf, crate_dir: String },
}

/// One graph a project's references may resolve into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReachableGraph {
    pub kind: ExternalGraphKind,
    /// `crates/serde_json-1.0.150`, or the linked project's canonical root.
    pub key: String,
    /// The name code uses for the crate (`serde_json`, `linkscope`).
    pub krate: String,
    /// The crate's library root, relative to the graph root.
    pub lib_root: String,
    pub location: GraphLocation,
    /// Changes whenever what the graph holds may have changed.
    pub fingerprint: String,
}

/// Why a dependency was left out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Skipped {
    /// No shard built yet (or one this build cannot read).
    pub no_shard: usize,
    /// Several versions under one crate name and no single direct one.
    pub ambiguous: usize,
    /// A linked project whose index is missing.
    pub unindexed_links: usize,
}

/// The graphs one project reaches, by crate name.
#[derive(Debug, Clone, Default)]
pub struct Reach {
    graphs: Vec<ReachableGraph>,
    by_crate: HashMap<String, usize>,
    pub skipped: Skipped,
}

impl Reach {
    pub fn graphs(&self) -> &[ReachableGraph] {
        &self.graphs
    }

    pub fn is_empty(&self) -> bool {
        self.graphs.is_empty()
    }

    /// The graph of the crate code calls `krate`.
    pub fn index_of(&self, krate: &str) -> Option<usize> {
        self.by_crate.get(krate).copied()
    }

    pub fn graph(&self, index: usize) -> &ReachableGraph {
        &self.graphs[index]
    }

    /// Graph key → fingerprint, for change detection between passes.
    pub fn fingerprints(&self) -> BTreeMap<String, String> {
        self.graphs
            .iter()
            .map(|graph| (graph.key.clone(), graph.fingerprint.clone()))
            .collect()
    }

    /// A reach over explicit graphs (tests).
    pub fn from_graphs(graphs: Vec<ReachableGraph>) -> Reach {
        let mut reach = Reach::default();
        for graph in graphs {
            reach.push(graph);
        }
        reach
    }

    fn push(&mut self, graph: ReachableGraph) {
        let index = self.graphs.len();
        self.by_crate.insert(graph.krate.clone(), index);
        self.graphs.push(graph);
    }
}

/// The graphs `project_root` reaches (read-only; nothing is created).
pub fn discover(home: &FederationHome, project_root: &Path) -> Reach {
    let root = canonical_root(project_root);
    let mut reach = Reach::default();
    let mut linked = linked_projects(home, &root, &mut reach.skipped);
    let linked_names: Vec<String> = linked.iter().map(|g| g.krate.clone()).collect();
    for graph in dependency_shards(home, &root, &mut reach.skipped) {
        // A crate the atlas links to a project resolves there.
        if !linked_names.contains(&graph.krate) {
            reach.push(graph);
        }
    }
    for graph in linked.drain(..) {
        reach.push(graph);
    }
    reach
}

/// A dependency version with a readable shard, and whether it is direct.
struct Candidate {
    graph: ReachableGraph,
    direct: bool,
}

fn dependency_shards(
    home: &FederationHome,
    root: &Path,
    skipped: &mut Skipped,
) -> Vec<ReachableGraph> {
    let Ok(dependencies) = dependencies_of_in(&home.deps, root) else {
        return Vec::new();
    };
    let mut by_name: BTreeMap<String, Vec<Candidate>> = BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for dependency in dependencies {
        // Rust first; npm and Go shards join here with their resolvers.
        if dependency.key.ecosystem != Ecosystem::Crates
            || matches!(dependency.source, DepSource::Path { .. })
            || !seen.insert(dependency.key.clone())
        {
            continue;
        }
        let dir = home.deps.shard_dir(&dependency.key);
        let meta = ShardMeta::read(&dir)
            .filter(|meta| meta.is_readable() && meta.state.has_shard())
            .filter(|meta| meta.key() == dependency.key);
        let Some(meta) = meta else {
            skipped.no_shard += 1;
            continue;
        };
        let source_dir = PathBuf::from(&meta.source_dir);
        let lib = LibTarget::read(&source_dir, &dependency.key.name);
        by_name
            .entry(lib.name.clone())
            .or_default()
            .push(Candidate {
                direct: dependency.direct == Some(true),
                graph: ReachableGraph {
                    kind: ExternalGraphKind::Dependency,
                    key: format!(
                        "{}/{}",
                        dependency.key.ecosystem.as_str(),
                        dependency.key.dir_name()
                    ),
                    krate: lib.name,
                    lib_root: lib.root,
                    fingerprint: format!(
                        "{}:{}:{}",
                        meta.built_at_ms, meta.extractor_version, meta.source_fingerprint
                    ),
                    location: GraphLocation::Shard { dir, source_dir },
                },
            });
    }
    let mut graphs = Vec::new();
    for (_, mut candidates) in by_name {
        if candidates.len() > 1 {
            candidates.retain(|candidate| candidate.direct);
        }
        match candidates.len() {
            1 => graphs.extend(candidates.pop().map(|candidate| candidate.graph)),
            _ => skipped.ambiguous += 1,
        }
    }
    graphs
}

/// Crates of other registered projects this project depends on by path.
fn linked_projects(
    home: &FederationHome,
    root: &Path,
    skipped: &mut Skipped,
) -> Vec<ReachableGraph> {
    let Ok(Some(atlas)) = Atlas::open_read_only(&home.atlas) else {
        return Vec::new();
    };
    let Ok(Some(project)) = atlas.project_by_root(root) else {
        return Vec::new();
    };
    let Ok(links) = atlas.links_from(project.id) else {
        return Vec::new();
    };
    let mut graphs: Vec<ReachableGraph> = Vec::new();
    for link in links {
        if link.kind != LinkKind::CargoPathDep || !link.is_cross_project() {
            continue;
        }
        let (Some(to), Some(dependency)) = (link.to_project, link.detail.as_deref()) else {
            continue;
        };
        let Ok(Some(target)) = atlas.project(to) else {
            continue;
        };
        if !crate::db::get_database_path(&target.root).is_file() {
            skipped.unindexed_links += 1;
            continue;
        }
        let Ok(crate_dir) = link.to_path.strip_prefix(&target.root) else {
            continue;
        };
        let crate_dir = crate_dir.to_string_lossy().replace('\\', "/");
        // Code calls the crate by its dependency key, or by the library
        // name its own manifest sets.
        let lib = LibTarget::read(&link.to_path, dependency);
        let krate = lib.name.clone();
        if graphs.iter().any(|graph| graph.krate == krate) {
            continue;
        }
        let lib_root = if crate_dir.is_empty() {
            lib.root
        } else {
            format!("{crate_dir}/{}", lib.root)
        };
        graphs.push(ReachableGraph {
            kind: ExternalGraphKind::Project,
            key: target.root.to_string_lossy().into_owned(),
            krate,
            lib_root,
            fingerprint: format!(
                "{}:{}",
                target.last_indexed_ms.unwrap_or_default(),
                target.node_count.unwrap_or_default()
            ),
            location: GraphLocation::Project {
                root: target.root.clone(),
                crate_dir,
            },
        });
    }
    graphs
}
