//! Agent activity in `codegraph projects` — the history × atlas join
//! ([`codegraph::history::atlas_join`]): read-only, bounded, and simply
//! absent when there is no history store.

use std::time::Duration;

use codegraph::atlas::{Atlas, Project};
use codegraph::history::atlas_join::{
    ActivityReader,
    ActivitySummary,
    LinkedActivity,
    LinkedQuery,
    ProjectActivity,
    Relation,
};
use codegraph::history::memory::{EpisodeRow, FailureRow};
use codegraph::history::{default_history_path, now_ms};

use super::super::dim;
use super::render::ago;

/// Bound on one command's history reads.
const DEADLINE: Duration = Duration::from_secs(1);
const DAY_MS: i64 = 86_400_000;
/// Linked projects shown by `show`.
const LINKED_SHOWN: usize = 8;
/// Files named per episode by `show`.
const FILES_SHOWN: usize = 3;

/// The history store, read-only (`None`: none yet, or unreadable).
pub(super) fn open_reader() -> Option<ActivityReader> {
    ActivityReader::open(&default_history_path(), DEADLINE)
        .ok()
        .flatten()
}

/// Each project's activity summary (default: nothing recorded) and
/// whether the deadline cut the pass short.
pub(super) fn summaries(
    reader: Option<&ActivityReader>,
    atlas: &Atlas,
    projects: &[Project],
) -> (Vec<ActivitySummary>, bool) {
    let mut out = vec![ActivitySummary::default(); projects.len()];
    let Some(reader) = reader else {
        return (out, false);
    };
    for (slot, project) in out.iter_mut().zip(projects) {
        if reader.expired() {
            return (out, true);
        }
        if let Ok(Some(scope)) = reader.scope(atlas, project) {
            if let Ok(summary) = reader.summary(&scope) {
                *slot = summary;
            }
        }
    }
    let expired = reader.expired();
    (out, expired)
}

/// `project`'s activity and its linked projects'.
pub(super) fn project_activity(
    reader: &ActivityReader,
    atlas: &Atlas,
    project: &Project,
) -> (Option<ProjectActivity>, Vec<LinkedActivity>) {
    let scope = reader.scope(atlas, project).ok().flatten();
    let own = scope.as_ref().and_then(|s| reader.activity(s).ok());
    let now = now_ms();
    let query = LinkedQuery {
        failures_since_ms: now - 14 * DAY_MS,
        edits_since_ms: now - 30 * DAY_MS,
        max: LINKED_SHOWN,
        detail: true,
        same_remote: true,
    };
    let linked = reader
        .linked_activity(atlas, project, scope.as_ref(), query)
        .unwrap_or_default();
    (own, linked)
}

/// `active 2h ago · 5 sessions/30d` (empty when nothing was recorded).
pub(super) fn summary_label(summary: &ActivitySummary) -> String {
    let mut parts = Vec::new();
    if let Some(at) = summary.last_activity_ms {
        parts.push(format!("active {}", ago(at)));
    }
    if summary.sessions_30d > 0 {
        parts.push(format!("{} sessions/30d", summary.sessions_30d));
    }
    parts.join(" · ")
}

/// The "Recent agent activity" section of `show`.
pub(super) fn print_activity(activity: Option<&ProjectActivity>, reader_open: bool) {
    let Some(activity) = activity else {
        let why = if reader_open {
            "no recorded agent sessions"
        } else {
            "no agent history (run `codegraph history ingest`)"
        };
        println!("  Recent agent activity  {}", dim(why));
        return;
    };
    println!(
        "  Recent agent activity  {}",
        dim(&summary_label(&activity.summary))
    );
    for e in &activity.episodes {
        println!("    {}", episode_line(e));
    }
    if !activity.hot_files.is_empty() {
        let hot: Vec<String> = activity
            .hot_files
            .iter()
            .map(|h| format!("{} ({})", h.path, h.episodes))
            .collect();
        println!("    {} {}", dim("hot files, 30d:"), hot.join(", "));
    }
    if !activity.open_failures.is_empty() {
        let open: Vec<String> = activity.open_failures.iter().map(failure_text).collect();
        println!("    {} {}", dim("not fixed since:"), open.join("; "));
    }
}

/// One line per linked project.
pub(super) fn print_linked(linked: &[LinkedActivity]) {
    if linked.is_empty() {
        return;
    }
    println!("  Linked projects' agent activity");
    let width = linked
        .iter()
        .map(|l| l.project.len())
        .max()
        .unwrap_or(0)
        .min(28);
    for l in linked {
        println!(
            "    {:<width$}  {}  {}",
            l.project,
            dim(&format!("({})", l.relation.label())),
            linked_text(l)
        );
    }
}

fn linked_text(l: &LinkedActivity) -> String {
    if !l.recorded {
        return dim("no recorded agent sessions");
    }
    let mut parts = Vec::new();
    let summary = summary_label(&l.summary);
    if !summary.is_empty() {
        parts.push(summary);
    }
    if let Some(e) = &l.last_episode {
        parts.push(format!("last: {} ({} calls)", e.outcome, e.calls));
    }
    if let Some(first) = l.open_failures.first() {
        parts.push(format!(
            "{} open failure(s): {}",
            l.open_failures.len(),
            failure_text(first)
        ));
    }
    if let Some(at) = l.shared_edit_ms {
        parts.push(match l.relation {
            Relation::Dependent => format!("edited this project {}", ago(at)),
            _ => format!("edited {}", ago(at)),
        });
    }
    parts.join(" · ")
}

fn episode_line(e: &EpisodeRow) -> String {
    let mut line = format!(
        "{} ago · {} · {} calls · {}",
        e.ago, e.source, e.calls, e.outcome
    );
    let files: Vec<String> = e
        .files
        .iter()
        .take(FILES_SHOWN)
        .map(|f| {
            let mark = if f.unchanged == Some(true) {
                " (unchanged)"
            } else {
                ""
            };
            format!("{} {}{mark}", f.op, f.path)
        })
        .collect();
    if !files.is_empty() {
        line.push_str(": ");
        line.push_str(&files.join(", "));
    }
    let more = e.files.len().saturating_sub(FILES_SHOWN) + e.more_files;
    if more > 0 {
        line.push_str(&format!(" (+{more})"));
    }
    line
}

fn failure_text(f: &FailureRow) -> String {
    let codes = if f.codes.is_empty() {
        String::new()
    } else {
        format!(" {}", f.codes.join(","))
    };
    format!("`{}`{codes} {} ago ({}×)", f.command, f.ago, f.repeats)
}
