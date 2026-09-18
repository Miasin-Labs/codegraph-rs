//! One external resolution pass over a project's index.
//!
//! 1. Find the reachable graphs ([`discover`]) and compare their
//!    fingerprints with the ones the last complete pass recorded
//!    (`project_metadata` key [`STATE_KEY`]).
//! 2. On a change (a shard built, rebuilt or gone, a lockfile bump, a linked
//!    project re-indexed), in [`ExternalMode::Full`]: turn the external
//!    edges into changed or vanished graphs back into unresolved references,
//!    then examine *every* unresolved Rust reference. Otherwise examine only
//!    the references this index run left unresolved.
//! 3. Resolve each within the time budget; store the edges and drop the
//!    references they resolve, in batches.
//! 4. Record the fingerprints only when a full pass finished — a pass cut
//!    short by its budget is simply redone (it is idempotent: what it
//!    resolved is no longer unresolved).
//!
//! Every graph is opened read-only; nothing outside this project's index is
//! written.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::graphs::{FederationHome, Reach, Skipped, discover};
use super::names::MethodNames;
use super::open::{DEFAULT_MAX_OPEN_GRAPHS, GraphCache, OpenStats};
use super::worker::{Outcome, Resolver, Store, resolve_refs, working_ref};
use crate::db::{ExternalGraphKind, QueryBuilder};
use crate::error::Result;
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::Language;

/// `project_metadata` key holding the last complete pass's graph
/// fingerprints.
pub const STATE_KEY: &str = "external_resolution";
/// Bump when what a pass resolves changes: the next pass is a full one.
pub const EXTERNAL_RESOLUTION_VERSION: u32 = 1;
/// Default budget of a full pass (`CODEGRAPH_EXTERNAL_BUDGET_MS`).
const DEFAULT_FULL_BUDGET: Duration = Duration::from_secs(60);
/// Default budget of an incremental pass.
const DEFAULT_INCREMENTAL_BUDGET: Duration = Duration::from_secs(5);
/// How many distinct misses a report keeps.
pub const MISSES_KEPT: usize = 30;
/// References paged from the index (and stored) at a time in a full pass.
const PAGE: usize = 20_000;
/// Below this many references the method-name pre-filter is skipped:
/// reading every reachable graph's method names (~50 ms for 250 shards)
/// costs more than the inference it saves on a sync's few references.
const FILTER_THRESHOLD: usize = 4_000;

/// How much a pass may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalMode {
    /// Only the references handed in (the watcher's syncs): no full pass,
    /// no restoring, no fingerprints recorded.
    Incremental,
    /// CLI `init`/`index`/`sync` and the background sync a finished
    /// dependency build queues: a full pass whenever the graphs changed.
    Full,
}

/// Which references a pass starts from.
#[derive(Debug, Clone, Copy)]
pub enum ExternalRefs<'a> {
    /// Every unresolved Rust reference (after a full index).
    All,
    /// These (what an incremental resolution left unresolved).
    These(&'a [UnresolvedRef]),
}

/// Knobs of one pass.
#[derive(Debug, Clone)]
pub struct ExternalOptions {
    pub mode: ExternalMode,
    pub budget: Duration,
    pub max_open: usize,
    pub home: FederationHome,
}

impl ExternalOptions {
    /// Defaults from the environment: `CODEGRAPH_EXTERNAL_BUDGET_MS`,
    /// `CODEGRAPH_EXTERNAL_MAX_OPEN`, and the stores under
    /// `codegraph_home()`.
    pub fn from_env(mode: ExternalMode) -> Self {
        let budget = env_number("CODEGRAPH_EXTERNAL_BUDGET_MS")
            .map(Duration::from_millis)
            .unwrap_or(match mode {
                ExternalMode::Full => DEFAULT_FULL_BUDGET,
                ExternalMode::Incremental => DEFAULT_INCREMENTAL_BUDGET,
            });
        ExternalOptions {
            mode,
            budget,
            max_open: env_number("CODEGRAPH_EXTERNAL_MAX_OPEN")
                .map_or(DEFAULT_MAX_OPEN_GRAPHS, |n| n as usize),
            home: FederationHome::from_env(),
        }
    }
}

/// `CODEGRAPH_EXTERNAL=0` (or `false`/`off`) turns the pass off.
pub fn external_resolution_enabled() -> bool {
    !std::env::var("CODEGRAPH_EXTERNAL").is_ok_and(|value| {
        let value = value.trim();
        value == "0" || value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("off")
    })
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

/// What one pass did.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExternalReport {
    pub dependency_graphs: usize,
    pub project_graphs: usize,
    pub skipped: Skipped,
    /// The reachable graphs differ from the last complete pass's.
    pub graphs_changed: bool,
    /// Every unresolved Rust reference was examined.
    pub full: bool,
    /// External edges turned back into unresolved references.
    pub restored: usize,
    pub examined: usize,
    pub into_dependencies: usize,
    pub into_projects: usize,
    pub by: BTreeMap<String, usize>,
    /// References placed in (or typed to) a reachable crate that no single
    /// item there answered, most frequent first (at most [`MISSES_KEPT`]).
    pub misses: Vec<(String, usize)>,
    pub opens: OpenStats,
    /// The pass examined everything it meant to within its budget.
    pub complete: bool,
    pub elapsed_ms: u64,
}

impl ExternalReport {
    pub fn resolved(&self) -> usize {
        self.into_dependencies + self.into_projects
    }
}

/// What the last complete pass saw.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Recorded {
    version: u32,
    graphs: BTreeMap<String, String>,
}

/// `project_metadata` key of a full pass its budget cut short: the graphs
/// it ran against and the last reference row it finished, so the next pass
/// over the same graphs continues instead of starting over (a project too
/// large for one budget still gets through, one pass at a time).
pub const RESUME_KEY: &str = "external_resolution_resume";

#[derive(Debug, Serialize, Deserialize)]
struct Resume {
    graphs: Recorded,
    after: i64,
}

/// Run one pass over the index `queries` of the project at `project_root`,
/// resolving with the project's context `project` ([`plan`], then
/// [`Plan::execute`]).
pub fn run(
    queries: &QueryBuilder,
    project_root: &Path,
    project: &dyn ResolutionContext,
    refs: ExternalRefs<'_>,
    options: &ExternalOptions,
) -> Result<ExternalReport> {
    plan(queries, project_root, refs, options)?.execute(queries, project)
}

/// A pass whose graphs are known and whose stale edges were restored,
/// ready to resolve.
pub struct Plan<'a> {
    reach: Reach,
    current: Recorded,
    report: ExternalReport,
    refs: ExternalRefs<'a>,
    full_mode: bool,
    /// The reference row a cut-short full pass finished (0: from the start).
    resume_after: i64,
    max_open: usize,
    started: Instant,
    deadline: Instant,
}

/// Discover the reachable graphs, compare them with the last complete
/// pass's, and (in [`ExternalMode::Full`], on a change) restore the edges
/// into changed or vanished graphs.
pub fn plan<'a>(
    queries: &QueryBuilder,
    project_root: &Path,
    refs: ExternalRefs<'a>,
    options: &ExternalOptions,
) -> Result<Plan<'a>> {
    let started = Instant::now();
    let deadline = started + options.budget;
    let reach = discover(&options.home, project_root);
    trace("discover", started);
    let mut report = ExternalReport {
        dependency_graphs: count(&reach, ExternalGraphKind::Dependency),
        project_graphs: count(&reach, ExternalGraphKind::Project),
        skipped: reach.skipped,
        complete: true,
        ..ExternalReport::default()
    };
    let current = Recorded {
        version: EXTERNAL_RESOLUTION_VERSION,
        graphs: reach.fingerprints(),
    };
    let previous: Option<Recorded> = queries
        .get_metadata(STATE_KEY)?
        .and_then(|text| serde_json::from_str(&text).ok());
    report.graphs_changed = previous.as_ref() != Some(&current);

    let full_mode = options.mode == ExternalMode::Full;
    // A full pass cut short against these same graphs is continued; its
    // stale edges were restored when it started.
    let resume_after = if full_mode {
        queries
            .get_metadata(RESUME_KEY)?
            .and_then(|text| serde_json::from_str::<Resume>(&text).ok())
            .filter(|resume| resume.graphs == current)
            .map(|resume| resume.after)
    } else {
        None
    };
    if full_mode && report.graphs_changed && resume_after.is_none() {
        let previous = previous.unwrap_or_default();
        let stale: Vec<String> = queries
            .external_edge_graph_keys()?
            .into_iter()
            .filter(|key| {
                previous.version != current.version
                    || current.graphs.get(key) != previous.graphs.get(key)
            })
            .collect();
        report.restored = queries.restore_external_edges(&stale)?;
    }
    report.full = full_mode
        && (report.graphs_changed || resume_after.is_some() || matches!(refs, ExternalRefs::All));
    Ok(Plan {
        reach,
        current,
        report,
        refs,
        full_mode,
        resume_after: resume_after.unwrap_or(0),
        max_open: options.max_open,
        started,
        deadline,
    })
}

impl Plan<'_> {
    /// The pass will examine every unresolved Rust reference (worth a
    /// context over the whole project held in memory, shared by workers).
    pub fn examines_everything(&self) -> bool {
        self.report.full && !self.reach.is_empty()
    }

    /// Resolve on this thread with the project's context `project`, and
    /// store the edges.
    pub fn execute(
        self,
        queries: &QueryBuilder,
        project: &dyn ResolutionContext,
    ) -> Result<ExternalReport> {
        self.run_with(
            queries,
            project,
            &|reach, names, refs, max_open, deadline| {
                vec![resolve_refs(
                    project, reach, names, refs, max_open, deadline,
                )]
            },
        )
    }

    /// [`Self::execute`] over `workers` threads sharing `project` (each
    /// opens the graphs it needs itself; a shard handle is one thread's).
    pub fn execute_parallel(
        self,
        queries: &QueryBuilder,
        project: &(dyn ResolutionContext + Sync),
        workers: usize,
    ) -> Result<ExternalReport> {
        let workers = workers.max(1);
        self.run_with(
            queries,
            project,
            &|reach, names, refs, max_open, deadline| {
                let chunk = refs.len().div_ceil(workers).max(1);
                std::thread::scope(|scope| {
                    let handles: Vec<_> = refs
                        .chunks(chunk)
                        .map(|part| {
                            scope.spawn(move || {
                                resolve_refs(project, reach, names, part, max_open, deadline)
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|handle| handle.join().unwrap_or_else(|_| Outcome::lost()))
                        .collect()
                })
            },
        )
    }

    fn run_with(
        self,
        queries: &QueryBuilder,
        project: &dyn ResolutionContext,
        resolve: &Resolver<'_>,
    ) -> Result<ExternalReport> {
        let Plan {
            reach,
            current,
            mut report,
            refs,
            full_mode,
            resume_after,
            max_open,
            started,
            deadline,
        } = self;
        let mut finished_through = resume_after;
        if !reach.is_empty() {
            let many = report.full
                || matches!(refs, ExternalRefs::These(these) if these.len() > FILTER_THRESHOLD);
            let names = if many {
                let at = Instant::now();
                let cache = GraphCache::new(&reach, max_open);
                let names = MethodNames::read(&cache);
                report.opens.add(cache.stats());
                trace("method names", at);
                names
            } else {
                MethodNames::Unfiltered
            };
            let mut store = Store::default();
            let mut misses: BTreeMap<String, usize> = BTreeMap::new();
            if report.full {
                let mut after = resume_after;
                loop {
                    let page = queries.get_unresolved_references_for_language_after_id(
                        Language::Rust,
                        after,
                        PAGE,
                    )?;
                    if page.refs.is_empty() {
                        break;
                    }
                    after = page.last_id;
                    let refs: Vec<UnresolvedRef> = page
                        .refs
                        .into_iter()
                        .filter_map(|row| working_ref(row, project))
                        .collect();
                    let outcomes = resolve(&reach, &names, &refs, max_open, deadline);
                    store.take(outcomes, &mut report, &mut misses);
                    store.flush(queries)?;
                    if !report.complete {
                        break;
                    }
                    finished_through = after;
                }
            } else if let ExternalRefs::These(these) = refs {
                let rust: Vec<UnresolvedRef> = these
                    .iter()
                    .filter(|reference| reference.language == Language::Rust)
                    .cloned()
                    .collect();
                let outcomes = resolve(&reach, &names, &rust, max_open, deadline);
                store.take(outcomes, &mut report, &mut misses);
                store.flush(queries)?;
            }
            let mut misses: Vec<(String, usize)> = misses.into_iter().collect();
            misses.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            misses.truncate(MISSES_KEPT);
            report.misses = misses;
        }
        if full_mode && report.complete && (report.full || report.graphs_changed) {
            queries.set_metadata(STATE_KEY, &serde_json::to_string(&current)?)?;
            queries.delete_metadata(RESUME_KEY)?;
        } else if report.full && !report.complete {
            let resume = Resume {
                graphs: current,
                after: finished_through,
            };
            queries.set_metadata(RESUME_KEY, &serde_json::to_string(&resume)?)?;
        }
        report.elapsed_ms = started.elapsed().as_millis() as u64;
        Ok(report)
    }
}

fn count(reach: &Reach, kind: ExternalGraphKind) -> usize {
    reach
        .graphs()
        .iter()
        .filter(|graph| graph.kind == kind)
        .count()
}

/// Phase timings on stderr with `CODEGRAPH_EXTERNAL_TRACE=1` (measurement).
pub(crate) fn trace(phase: &str, since: Instant) {
    if std::env::var_os("CODEGRAPH_EXTERNAL_TRACE").is_some() {
        eprintln!("external: {phase} {:?}", since.elapsed());
    }
}
