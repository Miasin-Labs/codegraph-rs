//! `recall --related`: the same question asked of every project the atlas
//! links to this one, each answer labelled with the project's name.
//!
//! A path is a path of *this* project, so a linked project answers with its
//! sessions that touched it (agents in a dependent editing the shared
//! crate); `last`, `failures` and `symbol:` are asked of the linked
//! project's own history; co-change has no cross-project meaning.

use std::path::Path;

use rusqlite::Connection;

use super::linked::linked_projects;
use super::scope::HistoryScope;
use super::{newest, repo_id};
use crate::atlas::Atlas;
use crate::history::deadline::{Deadline, is_interrupt};
use crate::history::memory::{About, Queries, RecallReport, RecallRequest, RelatedRecall};
use crate::history::store::HistoryError;

/// Linked projects asked.
const MAX_RELATED: usize = 6;
/// Rows per linked project (the answer budget trims further).
const ROWS_PER_PROJECT: usize = 3;

/// Where a recall runs.
pub(crate) struct RecallContext<'a> {
    pub conn: &'a Connection,
    pub atlas: &'a Path,
    /// The directory recalled about (resolved to its atlas project).
    pub project_root: &'a Path,
    /// Its history repository root (worktrees folded).
    pub repo_root: &'a Path,
    pub now: i64,
}

/// Add linked projects' matching activity to `report`. Past the deadline
/// the groups gathered so far stay (the answer is marked truncated), and
/// this project's own answer is never lost to a slow linked one.
pub(crate) fn add_related(
    ctx: &RecallContext<'_>,
    req: &RecallRequest,
    report: &mut RecallReport,
    deadline: &Deadline,
) -> Result<(), HistoryError> {
    match gather(ctx, req, report, deadline) {
        Err(HistoryError::Sqlite(e)) if is_interrupt(&e) => {
            report.truncated = true;
            append_note(report, "linked projects timed out");
            Ok(())
        }
        other => other,
    }
}

fn gather(
    ctx: &RecallContext<'_>,
    req: &RecallRequest,
    report: &mut RecallReport,
    deadline: &Deadline,
) -> Result<(), HistoryError> {
    let Some(atlas) = Atlas::open_read_only(ctx.atlas)? else {
        append_note(report, "no atlas yet, so no linked projects");
        return Ok(());
    };
    let project = match atlas.project_for_path(ctx.project_root)? {
        Some(p) => Some(p),
        None => atlas.project_for_path(ctx.repo_root)?,
    };
    let Some(project) = project else {
        append_note(
            report,
            "this project is not in the atlas (`codegraph projects register`)",
        );
        return Ok(());
    };
    let own = HistoryScope::resolve(ctx.conn, &atlas, &project)?;
    let here = repo_id(ctx.conn, ctx.repo_root)?;
    let mut asked = 0usize;
    for linked in linked_projects(&atlas, &project, true)? {
        if asked >= MAX_RELATED {
            break;
        }
        if deadline.expired() {
            report.truncated = true;
            append_note(report, "linked projects timed out");
            break;
        }
        let Some(scope) = HistoryScope::resolve(ctx.conn, &atlas, &linked.project)? else {
            continue;
        };
        if own.as_ref().is_some_and(|o| o.overlaps(&scope)) {
            continue;
        }
        asked += 1;
        let mut group = RelatedRecall {
            project: linked.project.name.clone(),
            relation: linked.relation,
            episodes: Vec::new(),
            symbols: Vec::new(),
            failures: Vec::new(),
        };
        answer(ctx, &scope, here, req, &mut group)?;
        if !(group.episodes.is_empty() && group.symbols.is_empty() && group.failures.is_empty()) {
            report.related.push(group);
        }
    }
    if report.related.is_empty() {
        append_note(report, "no linked project has matching activity");
    }
    Ok(())
}

/// One linked project's answer to `req`.
fn answer(
    ctx: &RecallContext<'_>,
    scope: &HistoryScope,
    here: Option<i64>,
    req: &RecallRequest,
    group: &mut RelatedRecall,
) -> Result<(), HistoryError> {
    let limit = req.limit.clamp(1, ROWS_PER_PROJECT);
    let queries = |repo: i64| Queries {
        conn: ctx.conn,
        repo,
        probe: None,
        since: req.since_ms.unwrap_or(0),
        limit: limit as i64,
        now: ctx.now,
    };
    let nested = (!scope.prefix.is_empty()).then_some(scope.prefix.as_str());
    let mut episodes = Vec::new();
    let mut failures = Vec::new();
    for repo in scope.ids() {
        let q = queries(repo);
        match &req.about {
            About::Last => episodes.extend(q.episodes(nested)?),
            About::Path(path) => {
                if let Some(here) = here {
                    let path = match Path::new(path).strip_prefix(ctx.repo_root) {
                        Ok(rel) => rel.to_string_lossy().into_owned(),
                        Err(_) => path.clone(),
                    };
                    episodes.extend(q.episodes_touching(here, &path)?);
                }
            }
            About::Symbol(name) => {
                if group.symbols.is_empty() {
                    let (symbols, found) = q.symbol(name)?;
                    group.symbols = symbols;
                    episodes.extend(found);
                }
            }
            About::Failures => failures.extend(q.failures(false)?),
            About::Cochange(_) => {}
        }
    }
    group.episodes = newest(episodes, |e| e.started_ms, limit);
    group.failures = newest(failures, |f| f.ts_ms, limit);
    Ok(())
}

fn append_note(report: &mut RecallReport, text: &str) {
    report.note = Some(match report.note.take() {
        Some(note) => format!("{note}; {text}"),
        None => text.to_owned(),
    });
}
