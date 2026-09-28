//! The compiler layer: rust-analyzer's view of a Rust project, laid over
//! the tree-sitter graph.
//!
//! Tree-sitter cannot see macro-generated items or calls, and resolution
//! approximates trait dispatch, generics and method lookup. rust-analyzer
//! knows the answer: `rust-analyzer scip` writes a SCIP index of every
//! definition and reference, macro expansions included. This module runs
//! it ([`run`], bounded, cached per fingerprint), reads the index
//! ([`scip`], a hand-rolled protobuf reader), maps its occurrences onto
//! the graph's nodes ([`map`]) and applies the verdicts ([`apply`]):
//! tree-sitter's edges are confirmed (provenance `scip`) or corrected —
//! the compiler wins — references it left unresolved are resolved, and
//! what it missed is added, dependency items becoming external edges into
//! the dependency shards so federation follows them. Items a macro
//! generates get nodes of their own (`compiler_symbols.generated`).
//!
//! It is opt-in and CLI-only: `codegraph compiler-sync` or `codegraph index
//! --compiler` turn it on for a project (`project_metadata.compiler_layer`);
//! from then on the CLI's `index`/`sync` re-apply the cached index to the
//! files it still describes and start a detached `compiler-sync` when the
//! sources moved on. Never the MCP server, the watcher or the prompt hook.
//! Without rust-analyzer, or when it fails or runs out of time, the graph
//! is today's, and the CLI says so.

mod apply;
mod deps;
mod map;
pub mod report;
pub mod run;
pub mod scip;
mod source;
pub mod symbol;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

pub use apply::{
    ApplyOptions,
    COMPILER_KEY,
    COMPILER_ONLY_KEY,
    RESOLVED_BY_COMPILER,
    is_compiler_only,
};
pub use report::CompilerReport;
pub use run::{
    CompilerState,
    RunStatus,
    compiler_dir,
    rust_analyzer_program,
    rust_analyzer_version,
};
use serde::{Deserialize, Serialize};

use crate::db::QueryBuilder;
use crate::error::{CodeGraphError, Result};
use crate::resolution::external::graphs::{FederationHome, discover};
use crate::sync::background::BackgroundSync;
use crate::types::Language;

/// `project_metadata` key: the layer is on for this project.
pub const ENABLED_KEY: &str = "compiler_layer";
/// `project_metadata` key: the last pass's summary ([`LayerSummary`]).
pub const SUMMARY_KEY: &str = "compiler_summary";
/// Default budget of applying an index.
const DEFAULT_APPLY_BUDGET: Duration = Duration::from_secs(120);

/// `CODEGRAPH_COMPILER=0` (or `false`/`off`) turns the layer off.
pub fn compiler_layer_allowed() -> bool {
    !std::env::var("CODEGRAPH_COMPILER").is_ok_and(|value| {
        let value = value.trim();
        value == "0" || value.eq_ignore_ascii_case("false") || value.eq_ignore_ascii_case("off")
    })
}

fn env_ms(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_millis)
}

/// Knobs of one compiler sync.
#[derive(Debug, Clone)]
pub struct CompilerOptions {
    /// Budget of the rust-analyzer run (`CODEGRAPH_COMPILER_BUDGET_MS`).
    pub run_budget: Duration,
    /// Budget of applying the index (`CODEGRAPH_COMPILER_APPLY_BUDGET_MS`).
    pub apply_budget: Duration,
    /// Run rust-analyzer even when the cached index is current.
    pub force_run: bool,
    /// Write every verdict here as JSON lines.
    pub dump: Option<PathBuf>,
    pub home: FederationHome,
    pub max_open: usize,
    /// The rust-analyzer executable (`CODEGRAPH_RUST_ANALYZER`, else the
    /// one on `PATH`).
    pub rust_analyzer: PathBuf,
}

impl CompilerOptions {
    pub fn from_env() -> Self {
        CompilerOptions {
            run_budget: env_ms("CODEGRAPH_COMPILER_BUDGET_MS").unwrap_or(run::DEFAULT_BUDGET),
            apply_budget: env_ms("CODEGRAPH_COMPILER_APPLY_BUDGET_MS")
                .unwrap_or(DEFAULT_APPLY_BUDGET),
            force_run: false,
            dump: None,
            home: FederationHome::from_env(),
            max_open: std::env::var("CODEGRAPH_EXTERNAL_MAX_OPEN")
                .ok()
                .and_then(|value| value.trim().parse().ok())
                .unwrap_or(crate::resolution::external::open::DEFAULT_MAX_OPEN_GRAPHS),
            rust_analyzer: run::rust_analyzer_program(),
        }
    }
}

/// What a compiler sync did.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompilerOutcome {
    /// The rust-analyzer run (or why there was none).
    pub run: RunStatus,
    /// The pass over the graph, when an index was applied.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub report: Option<CompilerReport>,
    /// The applied index predates the sources (a new run is due).
    pub stale: bool,
    /// The index was not applied because another process held the
    /// project's write lock.
    pub locked: bool,
}

/// The last pass, as `status` shows it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LayerSummary {
    pub applied_at_ms: i64,
    pub tool_version: String,
    pub documents_applied: usize,
    pub documents_stale: usize,
    pub confirmed: usize,
    pub corrected: usize,
    pub refuted: usize,
    pub resolved: usize,
    pub added: usize,
    pub generated: usize,
    pub complete: bool,
}

impl LayerSummary {
    fn of(report: &CompilerReport) -> Self {
        LayerSummary {
            applied_at_ms: crate::db::connection::now_ms(),
            tool_version: report.tool_version.clone(),
            documents_applied: report.documents.applied,
            documents_stale: report.documents.stale,
            confirmed: report.edges.confirmed,
            corrected: report.edges.corrected,
            refuted: report.edges.refuted_std
                + report.edges.refuted_generated
                + report.edges.refuted_dependency
                + report.edges.refuted_local,
            resolved: report.unresolved.resolved_in_project + report.unresolved.resolved_external,
            added: report.added.edges.values().sum::<usize>()
                + report.added.external
                + report.implements_added
                + report.dispatch.added,
            generated: report.definitions.generated,
            complete: report.complete,
        }
    }

    /// The summary of the last pass, if the layer ever ran.
    pub fn read(queries: &QueryBuilder) -> Option<LayerSummary> {
        let text = queries.get_metadata(SUMMARY_KEY).ok()??;
        serde_json::from_str(&text).ok()
    }
}

/// Whether the layer is on for the project `queries` indexes.
pub fn layer_enabled(queries: &QueryBuilder) -> bool {
    compiler_layer_allowed()
        && queries
            .get_metadata(ENABLED_KEY)
            .ok()
            .flatten()
            .is_some_and(|value| value == "1")
}

/// The project's Rust files as the index knows them.
pub fn rust_files(queries: &QueryBuilder) -> Result<Vec<String>> {
    Ok(queries
        .get_all_files()?
        .into_iter()
        .filter(|file| file.language == Language::Rust && file.path.ends_with(".rs"))
        .map(|file| file.path)
        .collect())
}

/// Bring the cached SCIP index up to date — run rust-analyzer when the
/// inputs changed (or `force`), within `budget`. Holds no lock on the
/// graph: the run takes a minute on a large workspace.
pub fn refresh_index(
    project_root: &Path,
    rust_files: &[String],
    options: &CompilerOptions,
) -> RunStatus {
    let Some((program, tool_version)) =
        run::usable_rust_analyzer(&options.rust_analyzer, project_root)
    else {
        return RunStatus::Missing;
    };
    let inputs = run::hash_inputs(project_root, rust_files, &tool_version);
    run::ensure_index(
        &program,
        project_root,
        &inputs,
        &tool_version,
        rust_files,
        options.run_budget,
        options.force_run,
    )
}

/// Whether the cached index was made for other inputs than the project's
/// current ones (cheap enough for a CLI sync: hashes the Rust files).
pub fn index_is_stale(project_root: &Path, rust_files: &[String]) -> bool {
    let Some(state) = CompilerState::read(project_root) else {
        return true;
    };
    let inputs = run::hash_inputs(project_root, rust_files, &state.tool_version);
    inputs.fingerprint != state.fingerprint
}

/// Apply the cached index to the graph. The caller holds the project's
/// write lock. `Ok(None)` when there is no cached index.
pub fn apply_cached(
    queries: &QueryBuilder,
    project_root: &Path,
    options: &CompilerOptions,
) -> Result<Option<CompilerReport>> {
    let path = run::index_path(project_root);
    let Ok(bytes) = std::fs::read(&path) else {
        return Ok(None);
    };
    let index = scip::ScipIndex::decode(&bytes)
        .map_err(|error| CodeGraphError::other(format!("{}: {error}", path.display())))?;
    drop(bytes);
    let state = CompilerState::read(project_root);
    let reach = discover(&options.home, project_root);
    let input = apply::ApplyInput {
        queries,
        project_root,
        index: &index,
        state: state.as_ref(),
        reach: &reach,
    };
    let report = apply::apply(
        &input,
        &ApplyOptions {
            budget: options.apply_budget,
            max_open: options.max_open,
            dump: options.dump.clone(),
        },
    )?;
    queries.set_metadata(
        SUMMARY_KEY,
        &serde_json::to_string(&LayerSummary::of(&report))?,
    )?;
    Ok(Some(report))
}

/// Start `codegraph compiler-sync --quiet <root>` detached (the CLI's
/// `sync` does when the layer is on and the sources moved on).
pub fn spawn_background_compiler_sync(project_root: &Path) -> BackgroundSync {
    if crate::sync::background::background_disabled() {
        return BackgroundSync::Disabled;
    }
    if crate::deps::store::StoreLock::is_held(&run::lock_path(project_root)) {
        return BackgroundSync::AlreadyRunning;
    }
    let Some(exe) = crate::sync::background::cli_binary() else {
        return BackgroundSync::Unavailable;
    };
    let mut command = Command::new(exe);
    command
        .args(["compiler-sync", "--quiet"])
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
