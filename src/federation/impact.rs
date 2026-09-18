//! A blast radius that crosses into the projects using a graph.
//!
//! The changed items are one graph's (the symbol and what depends on it
//! there). Each project using that graph contributes its *entries* — its
//! symbols with an external edge into any changed item — and then their
//! own dependents inside that project up to `depth - 1` more levels (the
//! crossing counts as the first). Bounded by the project cap, a per-project
//! cap on symbols, and the request's deadline; a project whose index cannot
//! answer is skipped with its reason.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use super::graph::{GraphId, OpenGraph};
use super::set::GraphSet;
use super::users::{
    Caller,
    ProjectRef,
    SkipReason,
    SkippedProject,
    UserRead,
    distinct_callers,
    is_test,
};
use crate::db::{ExternalEdge, ExternalTarget};
use crate::types::Node;

/// Edges read from one dependent project at most.
const MAX_EDGES_PER_PROJECT: usize = 1_000;

/// One project's share of the blast radius.
#[derive(Debug, Clone)]
pub struct ProjectImpact {
    pub project: ProjectRef,
    /// Its symbols that reference a changed item directly.
    pub entries: Vec<Caller>,
    /// What depends on those entries inside the project (not repeating
    /// them): code before tests, then by file and line.
    pub affected: Vec<Node>,
    /// Symbols left out by the per-project cap.
    pub omitted: usize,
    /// Its last external pass was cut short (edges may be missing).
    pub partial: bool,
}

/// The cross-project part of a blast radius.
#[derive(Debug, Clone, Default)]
pub struct CrossImpact {
    pub groups: Vec<ProjectImpact>,
    /// Projects read that reference none of the changed items.
    pub unaffected: Vec<ProjectRef>,
    pub skipped: Vec<SkippedProject>,
}

impl CrossImpact {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() && self.skipped.is_empty()
    }

    /// Symbols affected across every project.
    pub fn total(&self) -> usize {
        self.groups
            .iter()
            .map(|group| group.entries.len() + group.affected.len() + group.omitted)
            .sum()
    }
}

impl GraphSet {
    /// The part of `changed`'s blast radius (items of graph `id`) that lies
    /// in the projects using the graph, `first` read first.
    pub fn impact_across(
        &self,
        id: &GraphId,
        changed: &[Node],
        first: Option<&Path>,
        depth: u32,
        per_project: usize,
    ) -> CrossImpact {
        let mut out = CrossImpact::default();
        if changed.is_empty() {
            return out;
        }
        let identities: Vec<ExternalTarget<'_>> = changed.iter().map(ExternalTarget::of).collect();
        let names: HashMap<&str, &str> = changed
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
                UserRead::Edges { edges, .. } if edges.is_empty() => out.unaffected.push(user),
                UserRead::Edges {
                    graph,
                    edges,
                    partial,
                } => out.groups.push(self.project_impact(
                    &graph,
                    user,
                    &edges,
                    &names,
                    depth,
                    per_project,
                    partial,
                )),
            }
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn project_impact(
        &self,
        graph: &OpenGraph,
        project: ProjectRef,
        edges: &[ExternalEdge],
        names: &HashMap<&str, &str>,
        depth: u32,
        per_project: usize,
        partial: bool,
    ) -> ProjectImpact {
        let per_project = per_project.max(1);
        let mut entries = distinct_callers(graph, edges, names).unwrap_or_default();
        let mut omitted = entries.len().saturating_sub(per_project);
        entries.truncate(per_project);

        // Their dependents inside the project, while room and time last.
        let mut affected: Vec<Node> = Vec::new();
        let mut known: HashSet<String> = entries.iter().map(|e| e.node.id.clone()).collect();
        let room = per_project.saturating_sub(entries.len());
        if depth > 1 {
            let traverser = graph.traverser();
            for entry in &entries {
                if self.deadline().expired() {
                    break;
                }
                let Ok(radius) = traverser.get_impact_radius(&entry.node.id, depth - 1) else {
                    break;
                };
                for (id, node) in radius.nodes {
                    if known.insert(id) {
                        affected.push(node);
                    }
                }
            }
        }
        affected.sort_by(|a, b| {
            is_test(a)
                .cmp(&is_test(b))
                .then_with(|| a.file_path.cmp(&b.file_path))
                .then(a.start_line.cmp(&b.start_line))
                .then_with(|| a.name.cmp(&b.name))
        });
        if affected.len() > room {
            omitted += affected.len() - room;
            affected.truncate(room);
        }
        ProjectImpact {
            project,
            entries,
            affected,
            omitted,
            partial,
        }
    }
}
