//! The prompt hook's one line about linked projects: when this project
//! has path-dependency links, which of those projects had failures no run
//! fixed, or edits touching the code they share, in the last week.
//!
//! Hook-bounded: two read-only opens that create nothing, a handful of
//! indexed lookups, a short deadline — and nothing at all (no history
//! read) for a project without code links.

use std::path::Path;
use std::time::Duration;

use super::DAY_MS;
use super::linked::{LinkedActivity, LinkedQuery, Relation};
use super::reader::ActivityReader;
use crate::atlas::Atlas;
use crate::history::time::ago;

/// "Recent" for the line.
const WINDOW_MS: i64 = 7 * DAY_MS;
/// Linked projects looked at.
const MAX_LINKED: usize = 8;
/// Hard stop for the whole line (the hook's target is a few ms).
const DEADLINE: Duration = Duration::from_millis(100);
/// Longest command template quoted.
const COMMAND_CHARS: usize = 40;
/// Most bytes the line takes, whatever room the digest leaves.
pub(crate) const LINKED_LINE_MAX: usize = 320;

const HEAD: &str = "Linked projects, last 7d:";
const HINT: &str = " (more: `codegraph history recall failures --related`)";

/// The line for the project at `cwd` (history repository `repo_root`), in
/// at most `room` bytes; `None` when nothing is notable or anything is
/// missing.
pub(crate) fn linked_line(
    history: &Path,
    atlas: &Path,
    cwd: &Path,
    repo_root: &Path,
    room: usize,
) -> Option<String> {
    let atlas = Atlas::open_read_only(atlas).ok()??;
    let project = match atlas.project_for_path(cwd).ok()? {
        Some(p) => p,
        None => atlas.project_for_path(repo_root).ok()??,
    };
    // No code links: done before the history store is even opened.
    if super::linked_projects(&atlas, &project, false)
        .ok()?
        .is_empty()
    {
        return None;
    }
    let reader = ActivityReader::open(history, DEADLINE).ok()??;
    let own = reader.scope(&atlas, &project).ok()?;
    let since = reader.now - WINDOW_MS;
    let query = LinkedQuery {
        failures_since_ms: since,
        edits_since_ms: since,
        max: MAX_LINKED,
        detail: false,
        same_remote: false,
    };
    let linked = reader
        .linked_activity(&atlas, &project, own.as_ref(), query)
        .ok()?;
    render(&linked, reader.now, room.min(LINKED_LINE_MAX))
}

/// `HEAD entry; entry…HINT`, as many notable entries as fit in `room`.
pub(super) fn render(linked: &[LinkedActivity], now: i64, room: usize) -> Option<String> {
    let mut line = HEAD.to_owned();
    let mut shown = 0;
    for entry in linked.iter().filter_map(|l| entry(l, now)) {
        let sep = if shown == 0 { " " } else { "; " };
        if line.len() + sep.len() + entry.len() > room {
            break;
        }
        line.push_str(sep);
        line.push_str(&entry);
        shown += 1;
    }
    if shown == 0 {
        return None;
    }
    if line.len() + HINT.len() <= room {
        line.push_str(HINT);
    }
    Some(line)
}

/// `unlace (depends on this): edited this project 3h ago, 2 open failures `cargo test` E0599`
fn entry(linked: &LinkedActivity, now: i64) -> Option<String> {
    if !linked.is_notable() {
        return None;
    }
    let mut parts = Vec::new();
    if let Some(at) = linked.shared_edit_ms {
        parts.push(match linked.relation {
            Relation::Dependent => format!("edited this project {} ago", ago(at, now)),
            _ => format!("edited {} ago", ago(at, now)),
        });
    }
    if let Some(first) = linked.open_failures.first() {
        let n = linked.open_failures.len();
        let codes = first
            .codes
            .first()
            .map(|c| format!(" {c}"))
            .unwrap_or_default();
        parts.push(format!(
            "{n} open failure{} (`{}`{codes})",
            if n == 1 { "" } else { "s" },
            clip(&first.command, COMMAND_CHARS)
        ));
    }
    Some(format!(
        "{} ({}): {}",
        linked.project,
        linked.relation.label(),
        parts.join(", ")
    ))
}

fn clip(text: &str, chars: usize) -> String {
    match text.char_indices().nth(chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_owned(),
    }
}
