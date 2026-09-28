//! `codegraph compiler-sync` and the compiler layer's part of `index`/`sync`
//! (`codegraph::compiler`): rust-analyzer's SCIP index verifies, corrects
//! and extends the Rust graph. CLI only.

use std::path::{Path, PathBuf};

use codegraph::CodeGraph;
use codegraph::compiler::{
    CompilerOptions,
    CompilerOutcome,
    CompilerReport,
    RunStatus,
    compiler_layer_allowed,
    spawn_background_compiler_sync,
};
use codegraph::sync::background::BackgroundSync;

use super::{
    OpenOptions,
    clack_intro,
    clack_log_info,
    clack_log_success,
    clack_log_warn,
    clack_outro,
    error_msg,
    format_duration,
    format_number,
    is_initialized,
    process,
    refuse_foreign_index,
    resolve_writable_project_path,
};

/// Flags of `codegraph compiler-sync`.
pub(crate) struct CompilerSyncArgs {
    pub(crate) path: Option<String>,
    pub(crate) quiet: bool,
    pub(crate) verbose: bool,
    pub(crate) json: bool,
    pub(crate) force: bool,
    pub(crate) cached: bool,
    pub(crate) background: bool,
    pub(crate) dump: Option<String>,
}

/// codegraph compiler-sync [path]
pub(crate) async fn cmd_compiler_sync(args: CompilerSyncArgs) {
    let project_path = resolve_writable_project_path(args.path.as_deref())
        .unwrap_or_else(|m| refuse_foreign_index(&m, args.quiet));
    if !is_initialized(&project_path) {
        if !args.quiet {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
        }
        process::exit(1);
    }
    if !compiler_layer_allowed() {
        if !args.quiet {
            clack_log_info("The compiler layer is off (CODEGRAPH_COMPILER=0)");
        }
        return;
    }
    if args.background {
        let started = spawn_background_compiler_sync(&project_path);
        if !args.quiet {
            clack_log_info(&background_message(started));
        }
        return;
    }
    let cg = match CodeGraph::open(&project_path, &OpenOptions::default()) {
        Ok(cg) => cg,
        Err(error) => {
            if !args.quiet {
                error_msg(&format!("Failed to open the index: {error}"));
            }
            process::exit(1);
        }
    };
    let mut options = CompilerOptions::from_env();
    options.force_run = args.force;
    options.dump = args.dump.map(PathBuf::from);
    if !args.quiet && !args.json {
        clack_intro("Compiler layer (rust-analyzer)");
        if !args.cached {
            clack_log_info(
                "Running rust-analyzer scip (bounded; a large workspace takes a minute)",
            );
        }
    }
    let outcome = cg.compiler_sync(&options, !args.cached).await;
    cg.close();
    match outcome {
        Ok(outcome) if args.json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&outcome).unwrap_or_default()
            );
        }
        Ok(outcome) if !args.quiet => {
            print_outcome(&outcome, &project_path, args.verbose);
            clack_outro("Done");
        }
        Ok(_) => {}
        Err(error) => {
            if !args.quiet {
                error_msg(&format!("Compiler layer failed: {error}"));
            }
            process::exit(1);
        }
    }
}

/// Which write the compiler layer follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AfterWrite {
    /// `index --compiler`: run rust-analyzer and apply (turns the layer on).
    Explicit,
    /// `index`: files were re-extracted (or not).
    Index { reindexed: bool },
    /// `sync` (git hooks included): files changed (or not).
    Sync { changed: bool },
}

/// After `index`/`sync`. With `--compiler`, run the layer (rust-analyzer
/// included). Otherwise only when the layer is on for the project: an
/// `index` that re-extracted files re-applies the cached index to the
/// files it still describes (a `--force` rebuild gets the layer back),
/// and whenever the sources moved on since rust-analyzer ran a detached
/// `compiler-sync` refreshes it. A `sync` never applies inline — its
/// changed files are exactly the ones the cached index no longer
/// describes — so a git hook's sync stays cheap. Failures are reported,
/// never fatal (the index itself is written).
pub(crate) async fn after_write(
    cg: &CodeGraph,
    project_path: &Path,
    write: AfterWrite,
    quiet: bool,
    verbose: bool,
) {
    let explicit = write == AfterWrite::Explicit;
    if !compiler_layer_allowed() || !(explicit || cg.compiler_layer_enabled()) {
        return;
    }
    let stale = match write {
        AfterWrite::Sync { changed } => changed,
        AfterWrite::Index { reindexed: false } => cg.compiler_index_stale(),
        AfterWrite::Explicit | AfterWrite::Index { reindexed: true } => {
            let options = CompilerOptions::from_env();
            if explicit && !quiet {
                clack_log_info("Compiler layer: running rust-analyzer scip");
            }
            match cg.compiler_sync(&options, explicit).await {
                Ok(outcome) => {
                    if !quiet {
                        print_outcome(&outcome, project_path, verbose);
                    }
                    !explicit && outcome.stale
                }
                Err(error) => {
                    if !quiet {
                        clack_log_warn(&format!("Compiler layer skipped: {error}"));
                    }
                    false
                }
            }
        }
    };
    if stale {
        let started = spawn_background_compiler_sync(project_path);
        if !quiet && verbose {
            clack_log_info(&background_message(started));
        }
    }
}

fn background_message(started: BackgroundSync) -> String {
    match started {
        BackgroundSync::Started => "Compiler layer: rust-analyzer started in the background".into(),
        BackgroundSync::AlreadyRunning => "Compiler layer: a run is already in progress".into(),
        BackgroundSync::Disabled => {
            "Compiler layer: background runs are off (CODEGRAPH_NO_BACKGROUND_SYNC)".into()
        }
        BackgroundSync::Unavailable => "Compiler layer: could not start a background run".into(),
    }
}

fn print_outcome(outcome: &CompilerOutcome, project_path: &Path, verbose: bool) {
    match &outcome.run {
        RunStatus::Ran | RunStatus::Cached => {}
        RunStatus::Missing => clack_log_warn(
            "rust-analyzer is not installed: the Rust graph stays tree-sitter only \
             (`rustup component add rust-analyzer`)",
        ),
        RunStatus::TimedOut => clack_log_warn(
            "rust-analyzer ran out of time (CODEGRAPH_COMPILER_BUDGET_MS); \
             the previous compiler index (if any) was applied",
        ),
        RunStatus::Failed(why) => clack_log_warn(&format!(
            "rust-analyzer failed; the previous compiler index (if any) was applied. \
             See {}:\n{why}",
            codegraph::compiler::compiler_dir(project_path)
                .join("rust-analyzer.log")
                .display()
        )),
        RunStatus::Busy => clack_log_info("Compiler layer: another run is in progress"),
    }
    if outcome.locked {
        clack_log_warn("Compiler layer: the index is locked by another process; not applied");
    }
    let Some(report) = &outcome.report else {
        return;
    };
    print_report(report, verbose);
    if outcome.stale {
        clack_log_info(
            "Compiler layer: sources changed since rust-analyzer ran; changed files keep \
             their tree-sitter edges until the next `codegraph compiler-sync`",
        );
    }
}

fn print_report(report: &CompilerReport, verbose: bool) {
    let refuted = report.edges.refuted_std
        + report.edges.refuted_generated
        + report.edges.refuted_dependency
        + report.edges.refuted_local;
    let added: usize = report.added.edges.values().sum();
    clack_log_success(&format!(
        "Compiler: {} edges confirmed, {} corrected, {} refuted; {} references resolved; \
         {} edges + {} external edges added; {} generated items",
        format_number(report.edges.confirmed as u64),
        format_number(report.edges.corrected as u64),
        format_number(refuted as u64),
        format_number(
            (report.unresolved.resolved_in_project + report.unresolved.resolved_external) as u64
        ),
        format_number((added + report.implements_added + report.dispatch.added) as u64),
        format_number(report.added.external as u64),
        format_number(report.definitions.generated as u64),
    ));
    if report.documents.stale > 0 || !report.complete {
        clack_log_info(&format!(
            "  {} of {} files applied ({} changed since the run){}",
            report.documents.applied,
            report.documents.total,
            report.documents.stale,
            if report.complete {
                ""
            } else {
                "; budget reached"
            }
        ));
    }
    if !verbose {
        return;
    }
    clack_log_info(&format!(
        "  documents {:?}; definitions mapped {}, unmapped {:?}",
        report.documents, report.definitions.mapped, report.definitions.unmapped
    ));
    clack_log_info(&format!("  edges {:?}", report.edges));
    for (strategy, counts) in &report.by_strategy {
        clack_log_info(&format!(
            "  {strategy}: {} confirmed, {} corrected, {} refuted, {} unverifiable, {} unmatched{}",
            counts.confirmed,
            counts.corrected,
            counts.refuted,
            counts.unverifiable,
            counts.no_occurrence,
            counts
                .precision()
                .map(|p| format!(" (agreement {:.1}%)", p * 100.0))
                .unwrap_or_default()
        ));
    }
    clack_log_info(&format!("  unresolved {:?}", report.unresolved));
    clack_log_info(&format!("  external {:?}", report.external));
    clack_log_info(&format!("  added {:?}", report.added));
    clack_log_info(&format!("  dispatch {:?}", report.dispatch));
    for (title, samples) in [
        ("corrected", &report.samples.corrected),
        ("refuted", &report.samples.refuted),
        ("resolved", &report.samples.resolved),
        ("added", &report.samples.added),
    ] {
        if samples.is_empty() {
            continue;
        }
        clack_log_info(&format!("  {title}:"));
        for sample in samples {
            clack_log_info(&format!("    {sample}"));
        }
    }
    clack_log_info(&format!(
        "  {} in {}",
        report.tool_version,
        format_duration(report.elapsed_ms as i64)
    ));
}
