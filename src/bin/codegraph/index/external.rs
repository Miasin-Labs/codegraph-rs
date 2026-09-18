//! After `init`/`index`/`sync` has registered the project (atlas) and
//! recorded its lockfiles (deps registry): resolve what in-project
//! resolution left unresolved into the dependency shards and linked
//! projects it reaches (`codegraph::resolution::external`).

use codegraph::resolution::external::{ExternalMode, ExternalOptions, ExternalReport};
use codegraph::{CodeGraph, ExternalScope};

use super::{clack_log_info, format_duration, format_number};

/// Run the external pass on the project `cg` just indexed; failures are
/// reported, never fatal (the index itself is already written).
pub(super) async fn resolve_external_after_write(
    cg: &CodeGraph,
    scope: ExternalScope,
    quiet: bool,
    verbose: bool,
) {
    let options = ExternalOptions::from_env(ExternalMode::Full);
    let outcome = cg.resolve_external(scope, &options).await;
    match outcome {
        Ok(Some(report)) if !quiet => print_report(&report, verbose),
        Err(error) if !quiet => {
            clack_log_info(&format!("External resolution skipped: {error}"));
        }
        _ => {}
    }
}

fn print_report(report: &ExternalReport, verbose: bool) {
    if report.resolved() == 0 && report.restored == 0 && !verbose {
        return;
    }
    let mut line = format!(
        "External: {} references into {} dependencies, {} into linked projects",
        format_number(report.into_dependencies as u64),
        format_number(report.dependency_graphs as u64),
        format_number(report.into_projects as u64),
    );
    if verbose {
        line.push_str(&format!(
            " (examined {}, restored {}, {} graphs opened ({} distinct), {} without a shard, {}{})",
            format_number(report.examined as u64),
            format_number(report.restored as u64),
            report.opens.opened,
            report.opens.distinct,
            report.skipped.no_shard,
            format_duration(report.elapsed_ms as i64),
            if report.complete {
                ""
            } else {
                ", budget reached"
            }
        ));
    }
    clack_log_info(&line);
    if verbose && !report.misses.is_empty() {
        let top: Vec<String> = report
            .misses
            .iter()
            .take(10)
            .map(|(what, count)| format!("{what} ×{count}"))
            .collect();
        clack_log_info(&format!(
            "  unanswered in reachable crates: {}",
            top.join(", ")
        ));
    }
}
