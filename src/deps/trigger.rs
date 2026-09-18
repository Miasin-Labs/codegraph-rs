//! After a project is indexed: record its dependencies (cheap — lockfile
//! parsing, skipped when the lockfiles are unchanged) and, when shards are
//! missing, start ONE detached, budgeted `codegraph deps build`.
//!
//! Only the `codegraph index`/`init`/`sync` CLI commands call this — never
//! the MCP server or the prompt hook, which must not do unbounded work.
//! The build itself always runs in a separate process, launched like
//! [`crate::sync::background::spawn_background_sync`]: only the `codegraph`
//! CLI binary, in its own process group, and never when
//! `CODEGRAPH_NO_BACKGROUND_SYNC=1`.

use std::path::Path;
use std::process::{Command, Stdio};

use super::error::DepsResult;
use super::locate::SourceRoots;
use super::project::{canonical_root, record_project};
use super::registry::{PendingScope, RecordSummary, Registry};
use super::store::{DepsHome, StoreLock};
use crate::sync::background::{BackgroundSync, background_disabled, cli_binary};

/// Default wall-clock budget of a background build.
pub const DEFAULT_BACKGROUND_BUDGET_MS: u64 = 300_000;

/// What the post-index hook did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriggerOutcome {
    /// `CODEGRAPH_DEPS=0`.
    Disabled,
    /// The project has no lockfiles (and was never recorded): nothing written.
    NoLockfiles,
    /// Recorded (or confirmed unchanged); `pending` shards to build, and
    /// what became of the background build request.
    Recorded {
        unchanged: bool,
        summary: RecordSummary,
        pending: usize,
        build: Option<BackgroundSync>,
    },
}

/// `CODEGRAPH_DEPS=0` (or `false`/`off`) turns dependency recording off.
pub fn deps_enabled() -> bool {
    !std::env::var("CODEGRAPH_DEPS").is_ok_and(|v| {
        let v = v.trim();
        v == "0" || v.eq_ignore_ascii_case("false") || v.eq_ignore_ascii_case("off")
    })
}

/// The hook `codegraph index`/`init`/`sync` run after a successful index
/// of `project_root`. Never fails the caller: errors come back for the
/// caller to ignore or log.
pub fn after_project_indexed(project_root: &Path) -> DepsResult<TriggerOutcome> {
    if !deps_enabled() {
        return Ok(TriggerOutcome::Disabled);
    }
    after_project_indexed_in(
        &DepsHome::from_env(),
        &SourceRoots::from_env(),
        project_root,
        &|root| spawn_background_build(&DepsHome::from_env(), root),
    )
}

/// [`after_project_indexed`] against an explicit store, source caches and
/// build spawner.
pub fn after_project_indexed_in(
    home: &DepsHome,
    roots: &SourceRoots,
    project_root: &Path,
    spawn: &dyn Fn(&Path) -> BackgroundSync,
) -> DepsResult<TriggerOutcome> {
    let root = canonical_root(project_root);
    let known = home.registry_path().is_file();
    if !known && super::lockfile::discover(Path::new(&root)).is_empty() {
        return Ok(TriggerOutcome::NoLockfiles);
    }
    home.ensure()?;
    let mut registry = Registry::open(&home.registry_path())?;
    let report = record_project(&mut registry, Path::new(&root), roots, false, now_ms())?;
    if report.lockfiles.is_empty()
        && report.summary == RecordSummary::default()
        && !report.unchanged
    {
        return Ok(TriggerOutcome::NoLockfiles);
    }
    let pending = registry.pending(PendingScope::Project(&root), false)?.len();
    let build = (pending > 0).then(|| spawn(Path::new(&root)));
    Ok(TriggerOutcome::Recorded {
        unchanged: report.unchanged,
        summary: report.summary,
        pending,
        build,
    })
}

/// Start `codegraph deps build --project <root> --background` detached.
pub fn spawn_background_build(home: &DepsHome, project_root: &Path) -> BackgroundSync {
    if background_disabled() {
        return BackgroundSync::Disabled;
    }
    if StoreLock::is_held(&home.builder_lock_path()) {
        return BackgroundSync::AlreadyRunning;
    }
    let Some(exe) = cli_binary() else {
        return BackgroundSync::Unavailable;
    };
    let budget = std::env::var("CODEGRAPH_DEPS_BUDGET_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_BACKGROUND_BUDGET_MS);
    let mut command = Command::new(exe);
    command
        .args(["deps", "build", "--background", "--quiet", "--budget-ms"])
        .arg(budget.to_string());
    // A project's code calls its direct dependencies' APIs; transitive ones
    // matter only to chains typed through dependency return types. Build the
    // direct set in the background (~3.5 GiB for every crate version here vs
    // ~8.5 GiB for all) unless `CODEGRAPH_DEPS_ALL=1`; `deps build --all`
    // builds the rest on demand.
    if !env_flag("CODEGRAPH_DEPS_ALL") {
        command.arg("--direct-only");
    }
    command
        .arg("--project")
        .arg(project_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }
    if command.spawn().is_ok() {
        BackgroundSync::Started
    } else {
        BackgroundSync::Unavailable
    }
}

/// Most projects one finished build queues a re-resolving sync for.
pub const MAX_RERESOLVE_PROJECTS: usize = 8;

/// After a build published `built` shards: start a detached `codegraph
/// sync` for each indexed project that uses one of them (at most
/// [`MAX_RERESOLVE_PROJECTS`], `first` — the project that asked for the
/// build — ahead of the rest). The sync notices the new shards and
/// re-resolves the project's dependency-bound references into them
/// ([`crate::resolution::external`]); it is budgeted, and a no-op when the
/// graphs did not change. Never inline, never when
/// `CODEGRAPH_NO_BACKGROUND_SYNC=1`.
pub fn queue_reresolution(
    registry: &super::registry::Registry,
    built: &[super::model::DepKey],
    first: Option<&str>,
) -> Vec<(String, BackgroundSync)> {
    queue_reresolution_with(registry, built, first, &|root| {
        crate::sync::background::spawn_background_sync(root)
    })
}

/// [`queue_reresolution`] with an explicit sync spawner (tests).
pub fn queue_reresolution_with(
    registry: &super::registry::Registry,
    built: &[super::model::DepKey],
    first: Option<&str>,
    spawn: &dyn Fn(&Path) -> BackgroundSync,
) -> Vec<(String, BackgroundSync)> {
    let mut roots: Vec<String> = Vec::new();
    for key in built {
        for root in registry.users_of(key).unwrap_or_default() {
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
    }
    if let Some(first) = first {
        if let Some(at) = roots.iter().position(|root| root == first) {
            let root = roots.remove(at);
            roots.insert(0, root);
        }
    }
    roots
        .into_iter()
        .filter(|root| crate::db::get_database_path(Path::new(root)).is_file())
        .take(MAX_RERESOLVE_PROJECTS)
        .map(|root| {
            let outcome = spawn(Path::new(&root));
            (root, outcome)
        })
        .collect()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes"))
}
