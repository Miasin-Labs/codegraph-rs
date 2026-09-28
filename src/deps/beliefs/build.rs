//! `codegraph deps beliefs build`: observe the cargo cache's crates and
//! mine their beliefs.
//!
//! 1. The toolchain shards (`std`/`core`/`alloc` from local `rust-src`).
//! 2. The population: one version per cached crate, in a fixed
//!    pseudo-random order (a sample when the crate budget cuts it short).
//! 3. Per crate not yet observed with the same inputs: its shard and its
//!    direct dependencies' shards (built from the cache into the shared
//!    dependency store, like `deps build`), a private copy of its shard
//!    database, a read-only resolution of its unresolved references into
//!    those shards and the toolchain's, and the observation walk. The copy
//!    is deleted; the observations are cached under `obs/`.
//! 4. Mining every cached observation of the population into
//!    `beliefs.json`.
//!
//! Bounded by a crate budget (new observations per run) and a time budget
//! (no new crate is started once it is spent); each crate's resolution has
//! its own deadline. A run that stops early still mines what is cached, and
//! the next run continues where it stopped.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::model::{BELIEFS_FORMAT, BeliefSet, CrateObservations, OBSERVE_VERSION};
use super::population::{self, CacheCrate};
use super::toolchain::{self, Toolchain};
use super::{BELIEFS_FILE, beliefs_dir, create_private_dir, read_observations, write_json};
use crate::analyze::bugs::Project;
use crate::analyze::bugs::ecosystem::mine::{MineOptions, mine};
use crate::analyze::bugs::ecosystem::{self as eco};
use crate::codegraph::CodeGraph;
use crate::deps::locate::SourceRoots;
use crate::deps::scope::ShardLimits;
use crate::deps::shard::{BuildOutcome, BuildRequest, ShardHandle, ShardMeta, build_shard};
use crate::deps::store::StoreLock;
use crate::deps::{DepKey, DepSource, DepsHome, Ecosystem};
use crate::extraction::EXTRACTION_VERSION;
use crate::resolution::external::{Reach, ReachableGraph};

/// Knobs of one build.
#[derive(Debug, Clone)]
pub struct BuildOptions {
    /// Crates newly observed this run (0: no limit).
    pub max_crates: usize,
    /// No crate is started after this long (`None`: no limit).
    pub budget: Option<Duration>,
    /// The read-only resolution of one crate.
    pub crate_budget: Duration,
    pub jobs: usize,
    /// Observe only these crates (by name); empty: the whole cache.
    pub only: Vec<String>,
    pub limits: ShardLimits,
    pub mine: MineOptions,
    /// Observe again even crates whose cached observations are current.
    pub force: bool,
}

impl Default for BuildOptions {
    fn default() -> Self {
        BuildOptions {
            max_crates: 400,
            budget: Some(Duration::from_secs(30 * 60)),
            crate_budget: Duration::from_secs(20),
            jobs: std::thread::available_parallelism()
                .map_or(2, usize::from)
                .clamp(1, 8),
            only: Vec::new(),
            limits: ShardLimits::default(),
            mine: MineOptions::default(),
            force: false,
        }
    }
}

/// What one build did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildReport {
    pub toolchain: Option<String>,
    /// Crates in the cache (one version each).
    pub population: usize,
    pub observed: usize,
    pub cached: usize,
    /// Crates left for a later run (crate or time budget).
    pub remaining: usize,
    /// `name@version` → why it could not be observed.
    pub failed: BTreeMap<String, String>,
    /// Observations cut short by the per-crate resolution budget.
    pub partial: usize,
    pub crates_mined: usize,
    pub apis: usize,
    pub sites: usize,
    pub beliefs: usize,
    pub by_rule: BTreeMap<String, usize>,
    pub artifact: String,
    pub elapsed_ms: u64,
}

/// Progress, one line per crate.
#[derive(Debug, Clone)]
pub enum Progress {
    Toolchain(Option<String>),
    Observed {
        krate: String,
        sites: usize,
        ms: u64,
    },
    Failed {
        krate: String,
        error: String,
    },
    Mined {
        crates: usize,
        beliefs: usize,
    },
}

/// Run one build over the cache `roots` name, into `home`'s store.
pub fn build(
    home: &DepsHome,
    roots: &SourceRoots,
    options: &BuildOptions,
    on_progress: &(dyn Fn(&Progress) + Sync),
) -> Result<BuildReport, String> {
    let started = Instant::now();
    let dir = beliefs_dir(home);
    create_private_dir(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let Some(_lock) = StoreLock::try_acquire(&dir.join("build.lock")).map_err(|e| e.to_string())?
    else {
        return Err("another `deps beliefs build` is running".to_string());
    };
    let mut report = BuildReport::default();

    // 1. The toolchain.
    let found = Toolchain::locate();
    let toolchain_graphs = match &found {
        Some(toolchain) => block_on(toolchain::ensure(
            &toolchain::toolchain_home(&dir),
            toolchain,
            options.limits.time_ms.max(300_000),
        ))?,
        None => Vec::new(),
    };
    report.toolchain = found.as_ref().map(|t| t.rustc.clone());
    on_progress(&Progress::Toolchain(report.toolchain.clone()));

    // 2. The population.
    let registry_dirs = roots.cargo_registry_dirs();
    let every = population::scan(registry_dirs);
    let versions = population::versions_by_name(&every);
    let mut users = population::newest_per_name(&every);
    if !options.only.is_empty() {
        users.retain(|user| options.only.contains(&user.name));
    }
    users.sort_by_key(|user| (sample_order(&user.name), user.name.clone()));
    report.population = users.len();

    // 3. Observe what is not cached.
    let plans: Vec<Plan> = users
        .iter()
        .map(|user| Plan::of(user, &versions, found.as_ref()))
        .collect();
    let mut todo: Vec<usize> = Vec::new();
    for (index, plan) in plans.iter().enumerate() {
        let cached = !options.force
            && read_observations(
                &dir,
                &plan.user.name,
                &plan.user.version,
                Some(&plan.fingerprint),
            )
            .is_some();
        if cached {
            report.cached += 1;
        } else if options.max_crates == 0 || todo.len() < options.max_crates {
            todo.push(index);
        } else {
            report.remaining += 1;
        }
    }
    let next = AtomicUsize::new(0);
    let outcomes: Mutex<Vec<Outcome>> = Mutex::new(Vec::new());
    let jobs = options.jobs.max(1).min(todo.len().max(1));
    std::thread::scope(|scope| {
        for worker in 0..jobs {
            let (next, outcomes, todo, plans) = (&next, &outcomes, &todo, &plans);
            let (dir, toolchain_graphs) = (&dir, &toolchain_graphs);
            std::thread::Builder::new()
                .name(format!("beliefs-{worker}"))
                .stack_size(16 * 1024 * 1024)
                .spawn_scoped(scope, move || {
                    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        return;
                    };
                    loop {
                        if options
                            .budget
                            .is_some_and(|budget| started.elapsed() >= budget)
                        {
                            return;
                        }
                        let at = next.fetch_add(1, Ordering::SeqCst);
                        let Some(&index) = todo.get(at) else {
                            return;
                        };
                        let plan = &plans[index];
                        let label = format!("{}@{}", plan.user.name, plan.user.version);
                        let began = Instant::now();
                        let result = runtime.block_on(observe_crate(
                            home,
                            dir,
                            plan,
                            toolchain_graphs,
                            options,
                            worker,
                        ));
                        let result = result.and_then(|obs| {
                            let sites = obs.observations.sites.len();
                            let path =
                                super::observations_path(dir, &plan.user.name, &plan.user.version);
                            write_json(&path, &obs.observations)
                                .map_err(|e| format!("write {}: {e}", path.display()))?;
                            Ok((sites, obs.partial))
                        });
                        match &result {
                            Ok((sites, _)) => on_progress(&Progress::Observed {
                                krate: label.clone(),
                                sites: *sites,
                                ms: began.elapsed().as_millis() as u64,
                            }),
                            Err(error) => on_progress(&Progress::Failed {
                                krate: label.clone(),
                                error: error.clone(),
                            }),
                        }
                        if let Ok(mut all) = outcomes.lock() {
                            all.push((label, result));
                        }
                    }
                })
                .ok();
        }
    });
    let outcomes = outcomes.into_inner().unwrap_or_default();
    report.remaining += todo.len().saturating_sub(outcomes.len());
    for (label, result) in outcomes {
        match result {
            Ok((_, partial)) => {
                report.observed += 1;
                report.partial += usize::from(partial);
            }
            Err(error) => {
                report.failed.insert(label, error);
            }
        }
    }

    // 4. Mine.
    let observations: Vec<CrateObservations> = users
        .iter()
        .filter_map(|user| read_observations(&dir, &user.name, &user.version, None))
        .collect();
    let beliefs = mine(&observations, &eco::is_status, &options.mine);
    let mut apis = std::collections::HashSet::new();
    for obs in &observations {
        apis.extend(obs.apis.iter().map(String::as_str));
    }
    let set = BeliefSet {
        format: BELIEFS_FORMAT,
        built_at_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64),
        toolchain: report.toolchain.clone(),
        crates: observations.len(),
        apis: apis.len(),
        sites: observations.iter().map(|obs| obs.sites.len()).sum(),
        beliefs,
    };
    let artifact = dir.join(BELIEFS_FILE);
    write_json(&artifact, &set).map_err(|e| format!("write {}: {e}", artifact.display()))?;
    report.crates_mined = set.crates;
    report.apis = set.apis;
    report.sites = set.sites;
    report.beliefs = set.beliefs.len();
    for belief in &set.beliefs {
        *report
            .by_rule
            .entry(belief.kind.rule().to_string())
            .or_default() += 1;
    }
    on_progress(&Progress::Mined {
        crates: set.crates,
        beliefs: set.beliefs.len(),
    });
    report.artifact = artifact.to_string_lossy().into_owned();
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// A crate's label and its observation's (site count, cut short), or why
/// it failed.
type Outcome = (String, Result<(usize, bool), String>);

/// One crate to observe: its dependencies as the cache resolves them, and
/// the fingerprint of everything its observations depend on.
struct Plan<'a> {
    user: &'a CacheCrate,
    deps: Vec<(DepKey, PathBuf)>,
    fingerprint: String,
}

impl<'a> Plan<'a> {
    fn of(
        user: &'a CacheCrate,
        versions: &std::collections::HashMap<String, Vec<(String, PathBuf)>>,
        toolchain: Option<&Toolchain>,
    ) -> Plan<'a> {
        let mut deps: Vec<(DepKey, PathBuf)> = user
            .deps
            .iter()
            .filter_map(|dep| {
                let available = versions.get(&dep.package)?;
                let (version, dir) = population::choose_version(&dep.req, available)?;
                Some((
                    DepKey::new(Ecosystem::Crates, dep.package.clone(), version.clone()),
                    dir.clone(),
                ))
            })
            .collect();
        deps.sort_by_key(|dep| dep.0.dir_name());
        let mut parts = vec![
            format!("observe{OBSERVE_VERSION}"),
            format!("extract{EXTRACTION_VERSION}"),
            toolchain.map_or("no-toolchain".to_string(), |t| t.version.clone()),
        ];
        parts.extend(deps.iter().map(|(key, _)| key.dir_name()));
        Plan {
            user,
            deps,
            fingerprint: parts.join(","),
        }
    }
}

/// A crate's observations and whether its resolution was cut short.
struct Observation {
    observations: CrateObservations,
    partial: bool,
}

async fn observe_crate(
    home: &DepsHome,
    dir: &Path,
    plan: &Plan<'_>,
    toolchain_graphs: &[ReachableGraph],
    options: &BuildOptions,
    worker: usize,
) -> Result<Observation, String> {
    let user = plan.user;
    let key = DepKey::new(Ecosystem::Crates, user.name.clone(), user.version.clone());
    let meta = ensure_shard(home, &key, &user.dir, options.limits)
        .await
        .ok_or_else(|| "no shard (no library sources, or the build failed)".to_string())?;
    let mut graphs: Vec<ReachableGraph> = toolchain_graphs.to_vec();
    for (dep, source) in &plan.deps {
        if let Some(dep_meta) = ensure_shard(home, dep, source, options.limits).await {
            graphs.push(ReachableGraph::shard(dep, &dep_meta, home.shard_dir(dep)));
        }
    }

    let work = dir.join("work").join(format!(
        "{}-{}-{worker}",
        key.dir_name(),
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&work);
    create_private_dir(&work).map_err(|e| format!("create {}: {e}", work.display()))?;
    let result = observe_copy(home, &key, &meta, &work, &graphs, plan, options);
    let _ = std::fs::remove_dir_all(&work);
    result
}

fn observe_copy(
    home: &DepsHome,
    key: &DepKey,
    meta: &ShardMeta,
    work: &Path,
    graphs: &[ReachableGraph],
    plan: &Plan<'_>,
    options: &BuildOptions,
) -> Result<Observation, String> {
    let database = crate::db::DATABASE_FILENAME;
    std::fs::copy(home.shard_dir(key).join(database), work.join(database))
        .map_err(|e| format!("copy shard: {e}"))?;
    let source_dir = PathBuf::from(&meta.source_dir);
    let cg = CodeGraph::open_detached(&source_dir, work).map_err(|e| e.to_string())?;
    let result = (|| {
        let reach = Reach::from_graphs(graphs.to_vec());
        let (edges, complete) = cg
            .external_edges_read_only(&reach, options.crate_budget)
            .map_err(|e| e.to_string())?;
        let sites = eco::edge_sites(&cg, &edges)?;
        let mut project = Project::load(&cg, &source_dir)?;
        let observed = eco::observe(&mut project, &sites);
        // Each API's declared return, from the graph it resolved into.
        let mut target: std::collections::HashMap<String, (&str, &str)> =
            std::collections::HashMap::new();
        for edge in &edges {
            if let Some(api) = eco::graph_api(&edge.target_graph_key, &edge.target_qualified_name) {
                target
                    .entry(api)
                    .or_insert((edge.target_graph_key.as_str(), edge.target_node_id.as_str()));
            }
        }
        let mut handles: std::collections::HashMap<&str, Option<ShardHandle>> =
            std::collections::HashMap::new();
        let returns: Vec<Option<String>> = observed
            .apis
            .iter()
            .map(|api| {
                let (graph_key, node_id) = target.get(api)?;
                let handle = handles.entry(graph_key).or_insert_with(|| {
                    graphs
                        .iter()
                        .find(|graph| graph.key == *graph_key)
                        .and_then(|graph| match &graph.location {
                            crate::resolution::external::GraphLocation::Shard { dir, .. } => {
                                ShardHandle::open_dir(dir)
                            }
                            crate::resolution::external::GraphLocation::Project { .. } => None,
                        })
                });
                let node = handle.as_ref()?.queries().get_node_by_id(node_id).ok()??;
                eco::declared_value(node.signature.as_deref())
            })
            .collect();
        let sites: Vec<_> = observed.sites.into_iter().map(|site| site.obs).collect();
        Ok(Observation {
            observations: CrateObservations {
                version_of_observer: OBSERVE_VERSION,
                krate: plan.user.name.clone(),
                version: plan.user.version.clone(),
                fingerprint: plan.fingerprint.clone(),
                apis: observed.apis,
                returns,
                sites,
            },
            partial: !complete,
        })
    })();
    cg.close();
    result
}

/// The shard of `key` built from `source` (or the one already there);
/// waits for another builder holding its lock.
async fn ensure_shard(
    home: &DepsHome,
    key: &DepKey,
    source: &Path,
    limits: ShardLimits,
) -> Option<ShardMeta> {
    for _ in 0..600 {
        let outcome = build_shard(
            home,
            &BuildRequest {
                key,
                source: &DepSource::Registry,
                source_dir: source,
                limits,
                force: false,
            },
        )
        .await;
        match outcome {
            BuildOutcome::Built(meta) | BuildOutcome::UpToDate(meta) => return Some(meta),
            BuildOutcome::NoSources | BuildOutcome::Failed(_) => return None,
            BuildOutcome::Locked => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    None
}

fn block_on<F: std::future::Future>(future: F) -> Result<F::Output, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    Ok(runtime.block_on(future))
}

/// A fixed pseudo-random order of crate names (FNV-1a): the crate budget
/// then takes a sample of the cache rather than its alphabetical head.
fn sample_order(name: &str) -> u64 {
    let mut hasher = Fnv(0xcbf2_9ce4_8422_2325);
    name.hash(&mut hasher);
    hasher.finish()
}

struct Fnv(u64);

impl Hasher for Fnv {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
        }
    }
}
