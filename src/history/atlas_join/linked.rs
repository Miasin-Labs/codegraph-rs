//! Projects the atlas links to a project — path dependencies either way,
//! workspace members, separate clones of its remote — and what agents did
//! in them, one [`LinkedActivity`] each.

use std::path::PathBuf;

use serde::Serialize;

use super::reader::{ActivityReader, ActivitySummary};
use super::scope::HistoryScope;
use crate::atlas::{Atlas, AtlasError, LinkKind, Project};
use crate::history::memory::{EpisodeRow, FailureRow};
use crate::history::store::HistoryError;

/// Links through which one project's code uses another's.
const CODE_LINKS: [LinkKind; 5] = [
    LinkKind::CargoPathDep,
    LinkKind::CargoWorkspaceMember,
    LinkKind::NpmWorkspace,
    LinkKind::NpmFileDep,
    LinkKind::GoReplace,
];

/// How a linked project relates to the one asked about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Relation {
    /// This project uses it (a path dependency, workspace member, go
    /// `replace`).
    Dependency,
    /// It uses this project.
    Dependent,
    /// A separate clone of the same remote.
    SameRemote,
}

impl Relation {
    /// Short human label.
    pub fn label(self) -> &'static str {
        match self {
            Self::Dependency => "dependency",
            Self::Dependent => "depends on this",
            Self::SameRemote => "same remote",
        }
    }
}

/// A project linked to another, with how.
#[derive(Debug, Clone)]
pub struct LinkedProject {
    pub project: Project,
    pub relation: Relation,
    /// The link that relates them (the first, when several do).
    pub kind: LinkKind,
}

/// The projects linked to `project`, dependencies first, then dependents,
/// then (with `same_remote`) clones; each once, by name within a group.
/// Other checkouts of the same repository are not "linked" — they share
/// its history.
pub fn linked_projects(
    atlas: &Atlas,
    project: &Project,
    same_remote: bool,
) -> Result<Vec<LinkedProject>, AtlasError> {
    let relation = |kind: LinkKind, incoming: bool| {
        if CODE_LINKS.contains(&kind) {
            Some(if incoming {
                Relation::Dependent
            } else {
                Relation::Dependency
            })
        } else {
            (same_remote && kind == LinkKind::SameRemote).then_some(Relation::SameRemote)
        }
    };
    let mut found: Vec<(Relation, i64, LinkKind)> = Vec::new();
    for link in atlas.links_from(project.id)? {
        if let (true, Some(to), Some(rel)) = (
            link.is_cross_project(),
            link.to_project,
            relation(link.kind, false),
        ) {
            found.push((rel, to, link.kind));
        }
    }
    for link in atlas.links_to(project.id)? {
        if let Some(rel) = relation(link.kind, true) {
            found.push((rel, link.from_project, link.kind));
        }
    }
    found.sort_by_key(|(rel, ..)| *rel);
    let mut out: Vec<LinkedProject> = Vec::new();
    for (relation, id, kind) in found {
        if out.iter().any(|l| l.project.id == id) {
            continue;
        }
        let Some(other) = atlas.project(id)? else {
            continue;
        };
        if project.repo_root.is_some() && other.repo_root == project.repo_root {
            continue;
        }
        out.push(LinkedProject {
            project: other,
            relation,
            kind,
        });
    }
    out.sort_by(|a, b| {
        a.relation
            .cmp(&b.relation)
            .then_with(|| a.project.name.cmp(&b.project.name))
    });
    Ok(out)
}

/// What to gather about linked projects.
#[derive(Debug, Clone, Copy)]
pub struct LinkedQuery {
    /// Failures (not fixed since) newer than this (epoch ms).
    pub failures_since_ms: i64,
    /// Shared-code edits newer than this (epoch ms).
    pub edits_since_ms: i64,
    /// Linked projects to look at.
    pub max: usize,
    /// Also the summary and last episode (the projects view); the prompt
    /// hook only needs failures and edits.
    pub detail: bool,
    /// Also separate clones of the same remote.
    pub same_remote: bool,
}

/// What agents did in one linked project.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedActivity {
    /// Its atlas name.
    pub project: String,
    pub root: PathBuf,
    pub relation: Relation,
    pub link: LinkKind,
    /// Whether any agent session was recorded there.
    pub recorded: bool,
    #[serde(flatten)]
    pub summary: ActivitySummary,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_episode: Option<EpisodeRow>,
    /// Build/test failures no later run fixed, newest first.
    pub open_failures: Vec<FailureRow>,
    /// Latest edit of the code the two share (epoch ms): the linked
    /// project's own files for a dependency; this project's files, by the
    /// linked project's sessions, for a dependent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_edit_ms: Option<i64>,
}

impl LinkedActivity {
    /// Failures or shared-code edits worth a line.
    pub fn is_notable(&self) -> bool {
        !self.open_failures.is_empty() || self.shared_edit_ms.is_some()
    }
}

impl ActivityReader {
    /// Activity of the projects linked to `project` (whose own scope is
    /// `own`; linked projects reading the same history are skipped). Stops
    /// early, with what it has, when the deadline passes.
    pub fn linked_activity(
        &self,
        atlas: &Atlas,
        project: &Project,
        own: Option<&HistoryScope>,
        query: LinkedQuery,
    ) -> Result<Vec<LinkedActivity>, HistoryError> {
        let mut out = Vec::new();
        for linked in linked_projects(atlas, project, query.same_remote)?
            .into_iter()
            .take(query.max)
        {
            if self.expired() {
                break;
            }
            let scope = self.scope(atlas, &linked.project)?;
            if let (Some(scope), Some(own)) = (&scope, own) {
                if scope.overlaps(own) {
                    continue;
                }
            }
            let mut row = LinkedActivity {
                project: linked.project.name.clone(),
                root: linked.project.root.clone(),
                relation: linked.relation,
                link: linked.kind,
                recorded: scope.is_some(),
                summary: ActivitySummary::default(),
                last_episode: None,
                open_failures: Vec::new(),
                shared_edit_ms: None,
            };
            if let Some(scope) = &scope {
                self.fill(&mut row, scope, own, query)?;
            }
            out.push(row);
        }
        Ok(out)
    }

    fn fill(
        &self,
        row: &mut LinkedActivity,
        scope: &HistoryScope,
        own: Option<&HistoryScope>,
        query: LinkedQuery,
    ) -> Result<(), HistoryError> {
        if query.detail {
            row.summary = self.summary(scope)?;
            row.last_episode = self.episodes(scope, 0, 1, None)?.pop();
        }
        row.open_failures = self.failures(scope, query.failures_since_ms, true, 3)?;
        row.shared_edit_ms = match (row.relation, own) {
            (Relation::Dependency, _) => self.last_edit(scope, None, query.edits_since_ms)?,
            (Relation::Dependent, Some(own)) => {
                self.last_edit(own, Some(scope), query.edits_since_ms)?
            }
            _ => None,
        };
        Ok(())
    }

    /// Latest edit since `since` of a file in `of` — by any session, or by
    /// the sessions of `by` only.
    fn last_edit(
        &self,
        of: &HistoryScope,
        by: Option<&HistoryScope>,
        since: i64,
    ) -> Result<Option<i64>, HistoryError> {
        let mut latest = None;
        for repo in of.ids() {
            let q = self.queries(repo, since, 1, None);
            match by {
                None => latest = latest.max(q.last_edit(&of.prefix, None)?),
                Some(by) => {
                    for editor in by.ids() {
                        latest = latest.max(q.last_edit(&of.prefix, Some(editor))?);
                    }
                }
            }
        }
        Ok(latest)
    }
}
