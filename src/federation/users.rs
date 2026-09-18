//! "Who calls X" across projects: every project that uses X's graph is
//! asked for the external edges it recorded into X — a dependency
//! version's users from `deps/registry.db`, a project's dependents from
//! the atlas's `cargo_path_dep` links. Nothing is re-resolved; a project
//! whose index cannot hold the answer (none, older than external edges,
//! never resolved externally) is skipped and says why.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use super::follow::Unavailable;
use super::graph::{GraphId, OpenGraph};
use super::set::GraphSet;
use crate::atlas::LinkKind;
use crate::db::{ExternalEdge, ExternalGraphKind, ExternalTarget};
use crate::resolution::external::STATE_KEY;
use crate::resolution::external::pass::RESUME_KEY;
use crate::search::{is_test_file, is_test_symbol};
use crate::types::{EdgeKind, Node};

/// Edges read from one project at most (its callers are the distinct
/// sources among them).
const MAX_EDGES_PER_PROJECT: usize = 400;

/// A project by its canonical root and display name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRef {
    pub root: PathBuf,
    pub name: String,
}

/// One symbol of a project that calls into another graph.
#[derive(Debug, Clone)]
pub struct Caller {
    pub node: Node,
    /// Where the call is.
    pub line: Option<u32>,
    /// The item of the other graph it reaches (qualified name).
    pub target: String,
}

/// One project's callers.
#[derive(Debug, Clone)]
pub struct ProjectCallers {
    pub project: ProjectRef,
    pub callers: Vec<Caller>,
    /// Distinct callers left out by the per-project cap.
    pub omitted: usize,
    /// Its last external pass was cut short by its budget (edges may be
    /// missing until the next one).
    pub partial: bool,
}

/// Why a project using the graph was not read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SkipReason {
    /// Registered, but no index there.
    NoIndex,
    /// Its index predates external edges (schema < 10).
    OlderIndex,
    /// Its index never ran the external pass.
    NotResolved,
    Unreadable,
    /// The request's budget ran out first.
    Deadline,
    /// More projects than one answer reads.
    Cap,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoIndex => "no_index",
            Self::OlderIndex => "older_index",
            Self::NotResolved => "not_resolved",
            Self::Unreadable => "unreadable",
            Self::Deadline => "deadline",
            Self::Cap => "cap",
        }
    }

    pub fn describe(self) -> &'static str {
        match self {
            Self::NoIndex => "no index",
            Self::OlderIndex => "index predates external edges; re-index it",
            Self::NotResolved => "external resolution has not run; run `codegraph sync` there",
            Self::Unreadable => "index unreadable",
            Self::Deadline => "out of time",
            Self::Cap => "over the project cap",
        }
    }
}

#[derive(Debug, Clone)]
pub struct SkippedProject {
    pub project: ProjectRef,
    pub reason: SkipReason,
}

/// Callers of one graph's items across the projects that use it.
#[derive(Debug, Clone, Default)]
pub struct CrossCallers {
    /// Projects with callers, in the order they were read.
    pub groups: Vec<ProjectCallers>,
    /// Projects read that call none of the items.
    pub without_callers: Vec<ProjectRef>,
    pub skipped: Vec<SkippedProject>,
}

impl CrossCallers {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.skipped.is_empty()
    }
}

/// What reading one user project gave.
pub(crate) enum UserRead {
    Edges {
        graph: Rc<OpenGraph>,
        edges: Vec<ExternalEdge>,
        partial: bool,
    },
    Skipped(SkipReason),
}

impl GraphSet {
    /// The projects that use graph `id`: the registry's users of a
    /// dependency version, the atlas projects linking to a project by a
    /// Cargo path dependency. `first` (the asking project) leads when it is
    /// one of them; the rest follow by name.
    pub fn users_of(&self, id: &GraphId, first: Option<&Path>) -> Vec<ProjectRef> {
        let roots: BTreeSet<PathBuf> = match id.kind {
            ExternalGraphKind::Dependency => self.dependency_users(id),
            ExternalGraphKind::Project => self.project_dependents(Path::new(&id.key)),
        };
        let first = first.map(crate::atlas::canonical_root);
        let mut users: Vec<ProjectRef> = roots
            .into_iter()
            .map(|root| ProjectRef {
                name: self.project_name(&root),
                root,
            })
            .collect();
        users.sort_by(|a, b| {
            let a_first = Some(&a.root) == first.as_ref();
            let b_first = Some(&b.root) == first.as_ref();
            b_first
                .cmp(&a_first)
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.root.cmp(&b.root))
        });
        users
    }

    fn dependency_users(&self, id: &GraphId) -> BTreeSet<PathBuf> {
        let key = match self.open(id) {
            Ok(graph) => graph.dependency.clone(),
            Err(_) => None,
        };
        let (Some(key), Some(registry)) = (key, self.registry()) else {
            return BTreeSet::new();
        };
        registry
            .users_of(&key)
            .unwrap_or_default()
            .into_iter()
            .map(PathBuf::from)
            .collect()
    }

    fn project_dependents(&self, root: &Path) -> BTreeSet<PathBuf> {
        let Some(atlas) = self.atlas() else {
            return BTreeSet::new();
        };
        let Ok(Some(project)) = atlas.project_by_root(root) else {
            return BTreeSet::new();
        };
        let links = atlas.links_to(project.id).unwrap_or_default();
        let from: BTreeSet<i64> = links
            .iter()
            .filter(|link| link.kind == LinkKind::CargoPathDep && link.is_cross_project())
            .map(|link| link.from_project)
            .collect();
        from.into_iter()
            .filter_map(|id| atlas.project(id).ok().flatten())
            .map(|project| project.root)
            .collect()
    }

    /// Who calls `targets` (items of graph `id`) across every project that
    /// uses the graph, `first` read first: at most `per_project` callers a
    /// project, at most `max_projects` projects, within the deadline.
    pub fn callers_across(
        &self,
        id: &GraphId,
        targets: &[Node],
        first: Option<&Path>,
        per_project: usize,
    ) -> CrossCallers {
        let mut out = CrossCallers::default();
        if targets.is_empty() {
            return out;
        }
        let identities: Vec<ExternalTarget<'_>> = targets.iter().map(ExternalTarget::of).collect();
        let names: std::collections::HashMap<&str, &str> = targets
            .iter()
            .map(|node| (node.id.as_str(), node.qualified_name.as_str()))
            .collect();
        for (index, user) in self.users_of(id, first).into_iter().enumerate() {
            if index >= self.options().max_projects {
                out.skipped.push(SkippedProject {
                    project: user,
                    reason: SkipReason::Cap,
                });
                continue;
            }
            match self.read_user(&user.root, id, &identities, MAX_EDGES_PER_PROJECT) {
                UserRead::Skipped(reason) => out.skipped.push(SkippedProject {
                    project: user,
                    reason,
                }),
                UserRead::Edges { edges, .. } if edges.is_empty() => {
                    out.without_callers.push(user);
                }
                UserRead::Edges {
                    graph,
                    edges,
                    partial,
                } => match callers_of(&graph, &user, &edges, &names, per_project, partial) {
                    Some(group) => out.groups.push(group),
                    None => out.skipped.push(SkippedProject {
                        project: user,
                        reason: SkipReason::Unreadable,
                    }),
                },
            }
        }
        out
    }

    /// Who in the project at `root` calls `targets` (items of graph `id`),
    /// at most `limit`; `None` when it calls none of them or cannot say.
    pub fn callers_in(
        &self,
        root: &Path,
        id: &GraphId,
        targets: &[Node],
        limit: usize,
    ) -> Option<ProjectCallers> {
        let identities: Vec<ExternalTarget<'_>> = targets.iter().map(ExternalTarget::of).collect();
        let names: std::collections::HashMap<&str, &str> = targets
            .iter()
            .map(|node| (node.id.as_str(), node.qualified_name.as_str()))
            .collect();
        let UserRead::Edges {
            graph,
            edges,
            partial,
        } = self.read_user(root, id, &identities, MAX_EDGES_PER_PROJECT)
        else {
            return None;
        };
        if edges.is_empty() {
            return None;
        }
        let project = ProjectRef {
            root: graph.root.clone(),
            name: graph.label.clone(),
        };
        callers_of(&graph, &project, &edges, &names, limit, partial)
    }

    /// The edges one user project recorded into `targets` of graph `id`.
    pub(crate) fn read_user(
        &self,
        root: &Path,
        id: &GraphId,
        targets: &[ExternalTarget<'_>],
        limit: usize,
    ) -> UserRead {
        if self.deadline().expired() {
            return UserRead::Skipped(SkipReason::Deadline);
        }
        let graph = match self.open(&GraphId::project(root)) {
            Ok(graph) => graph,
            Err(Unavailable::Missing) => return UserRead::Skipped(SkipReason::NoIndex),
            Err(Unavailable::Unreadable) => return UserRead::Skipped(SkipReason::Unreadable),
            Err(Unavailable::Deadline) => return UserRead::Skipped(SkipReason::Deadline),
        };
        if !graph.has_external_edges {
            return UserRead::Skipped(SkipReason::OlderIndex);
        }
        let queries = graph.queries();
        let completed = queries.get_metadata(STATE_KEY).ok().flatten().is_some();
        let resumable = queries.get_metadata(RESUME_KEY).ok().flatten().is_some();
        let edges = match queries.get_external_edges_into_targets(&id.key, targets, limit) {
            Ok(edges) => edges,
            Err(error) if super::deadline::is_interrupt(&error) => {
                return UserRead::Skipped(SkipReason::Deadline);
            }
            Err(_) => return UserRead::Skipped(SkipReason::Unreadable),
        };
        if edges.is_empty() && !completed && !resumable {
            return UserRead::Skipped(SkipReason::NotResolved);
        }
        UserRead::Edges {
            graph,
            edges,
            partial: !completed,
        }
    }
}

/// The distinct calling symbols of `edges`, capped.
fn callers_of(
    graph: &OpenGraph,
    project: &ProjectRef,
    edges: &[ExternalEdge],
    names: &std::collections::HashMap<&str, &str>,
    per_project: usize,
    partial: bool,
) -> Option<ProjectCallers> {
    let mut callers = distinct_callers(graph, edges, names)?;
    let omitted = callers.len().saturating_sub(per_project);
    callers.truncate(per_project);
    Some(ProjectCallers {
        project: project.clone(),
        callers,
        omitted,
        partial,
    })
}

/// The distinct symbols `edges` come from — code before tests, then by
/// file and line — each with the item it reaches (`names`: target id →
/// qualified name, else the name the edge recorded).
pub(crate) fn distinct_callers(
    graph: &OpenGraph,
    edges: &[ExternalEdge],
    names: &std::collections::HashMap<&str, &str>,
) -> Option<Vec<Caller>> {
    let mut seen = HashSet::new();
    let sources: Vec<&ExternalEdge> = edges
        .iter()
        .filter(|edge| edge.kind != EdgeKind::Contains && seen.insert(edge.source.as_str()))
        .collect();
    let ids: Vec<String> = sources.iter().map(|edge| edge.source.clone()).collect();
    let nodes = graph.queries().get_nodes_by_ids(&ids).ok()?;
    let mut callers: Vec<Caller> = sources
        .iter()
        .filter_map(|edge| {
            Some(Caller {
                node: nodes.get(&edge.source)?.clone(),
                line: edge.line,
                target: names
                    .get(edge.target_node_id.as_str())
                    .map_or(edge.target_qualified_name.clone(), |name| name.to_string()),
            })
        })
        .collect();
    callers.sort_by(|a, b| {
        is_test(&a.node)
            .cmp(&is_test(&b.node))
            .then_with(|| a.node.file_path.cmp(&b.node.file_path))
            .then(a.node.start_line.cmp(&b.node.start_line))
    });
    Some(callers)
}

/// Test code (by file or by an inline `mod tests`), listed after the code
/// it tests.
pub(crate) fn is_test(node: &Node) -> bool {
    is_test_file(&node.file_path) || is_test_symbol(&node.file_path, &node.qualified_name)
}
