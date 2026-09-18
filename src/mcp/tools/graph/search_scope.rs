//! `codegraph_search` over the projects the atlas knows: `projects:
//! "linked"` (this project and the projects it shares code with — Cargo
//! path dependencies, workspaces, npm file deps, go replace, either way),
//! `"all"` (every registered project), or a list of project names.
//!
//! Unlike `projectPaths`, which opens each project as a session project,
//! these indexes are only *read*: opened read-only (creating nothing, never
//! migrated), searched with the same ranking, within the call's deadline
//! and a project cap. Every hit carries its project's root (what
//! `projectPath` takes); projects not searched are listed in
//! `failedProjects` with the reason.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::super::context::ToolHandler;
use super::super::format::{json_len, mcp_output_budget, num_or, rows_within_budget};
use super::super::output::SearchHitOutput;
use super::super::schema::ToolResult;
use crate::atlas::{LinkKind, ProjectStatus};
use crate::error::Result;
use crate::federation::{GraphId, GraphSet};
use crate::types::{NodeKind, SearchOptions, SearchResult};
use crate::utils::clamp;

/// Projects one `linked` or named search reads at most.
const MAX_LINKED: usize = 32;
/// Projects one `all` search reads at most.
const MAX_ALL: usize = 64;
/// Names one call looks up.
const MAX_NAMES: usize = 12;

/// Which registered projects a search covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::mcp::tools) enum ProjectScope {
    Linked,
    All,
    Named(Vec<String>),
}

/// The `projects` argument, when given.
pub(in crate::mcp::tools) fn project_scope(args: &Map<String, Value>) -> Option<ProjectScope> {
    match args.get("projects")? {
        Value::String(scope) => match scope.trim() {
            "linked" => Some(ProjectScope::Linked),
            "all" => Some(ProjectScope::All),
            "" => None,
            name => Some(ProjectScope::Named(vec![name.to_string()])),
        },
        Value::Array(items) => {
            let names: Vec<String> = items
                .iter()
                .filter_map(Value::as_str)
                .map(|name| name.trim().to_string())
                .filter(|name| !name.is_empty())
                .collect();
            (!names.is_empty()).then_some(ProjectScope::Named(names))
        }
        _ => None,
    }
}

/// One project to search.
struct Target {
    root: PathBuf,
    name: String,
}

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_search_scope(
        &self,
        args: &Map<String, Value>,
        scope: ProjectScope,
    ) -> Result<ToolResult> {
        let names = search_names(args);
        if names.is_empty() {
            return Ok(self
                .validate_string(args.get("query"), "query")
                .err()
                .unwrap_or_else(|| self.error_result("query must be a non-empty string")));
        }
        let fed = self.federation_reader();
        let current = self
            .get_code_graph(args.get("projectPath").and_then(Value::as_str))
            .ok()
            .map(|cg| cg.get_project_root().to_path_buf());
        let (targets, mut failed) = scope_targets(&fed, &scope, current.as_deref());
        let limit = clamp(num_or(args, "limit", 10.0), 1.0, 100.0) as usize;
        let kinds = search_kinds(args);
        let batch = names.len() > 1;

        let mut hits: Vec<(f64, Value)> = Vec::new();
        let mut matched: BTreeSet<String> = BTreeSet::new();
        let mut sections = Vec::new();
        for target in &targets {
            if fed.deadline().expired() {
                failed.push(json!({ "project": target.root, "message": "out of time" }));
                continue;
            }
            let graph = match fed.open(&GraphId::project(&target.root)) {
                Ok(graph) => graph,
                Err(reason) => {
                    failed.push(json!({ "project": target.root, "message": reason.as_str() }));
                    continue;
                }
            };
            let mut found = 0;
            for name in &names {
                let results: Vec<SearchResult> = graph
                    .queries()
                    .search_nodes(
                        name,
                        &SearchOptions {
                            limit: Some(limit),
                            kinds: kinds.clone(),
                            ..Default::default()
                        },
                    )
                    .unwrap_or_default();
                if !results.is_empty() {
                    matched.insert(name.clone());
                }
                found += results.len();
                for result in results {
                    let mut hit = SearchHitOutput::from(&result);
                    if batch {
                        hit.matched_query = Some(name.clone());
                    }
                    let mut row = serde_json::to_value(hit)?;
                    row["project"] = Value::String(target.root.to_string_lossy().into_owned());
                    hits.push((result.score, row));
                }
            }
            sections.push(format!(
                "- {} ({}): {found}",
                target.name,
                target.root.display()
            ));
        }
        // One ranking across projects (a stable sort keeps each project's
        // own order among equal scores).
        hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        let results: Vec<Value> = hits.into_iter().map(|(_, row)| row).collect();

        let mut payload = json!({ "schemaVersion": 2, "kind": "search", "results": [] });
        let unmatched: Vec<&String> = names
            .iter()
            .filter(|name| !matched.contains(*name))
            .collect();
        if batch && !unmatched.is_empty() {
            payload["unmatched"] = json!(unmatched);
        }
        if !failed.is_empty() {
            payload["failedProjects"] = Value::Array(failed);
        }
        payload["truncated"] = Value::Bool(true);
        let keep = rows_within_budget(
            mcp_output_budget(),
            json_len(&payload),
            results.iter().map(json_len),
        );
        let truncated = keep < results.len();
        let shown = keep.min(results.len());
        payload["results"] = Value::Array(results.into_iter().take(keep).collect());
        if let (false, Some(object)) = (truncated, payload.as_object_mut()) {
            object.remove("truncated");
        }
        let text = format!(
            "Search across {} project{}: {shown} hits\n\n{}",
            targets.len(),
            if targets.len() == 1 { "" } else { "s" },
            sections.join("\n")
        );
        self.structured_result(&self.truncate_output(&text), &payload)
    }
}

/// `query` plus any `symbols`, deduplicated.
fn search_names(args: &Map<String, Value>) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Some(query) = args.get("query").and_then(Value::as_str) {
        names.push(query.trim().to_string());
    }
    for item in args
        .get("symbols")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(name) = item.as_str() {
            names.push(name.trim().to_string());
        }
    }
    names.retain(|name| !name.is_empty());
    let mut seen = std::collections::HashSet::new();
    names.retain(|name| seen.insert(name.clone()));
    names.truncate(MAX_NAMES);
    names
}

fn search_kinds(args: &Map<String, Value>) -> Option<Vec<NodeKind>> {
    match args.get("kind").and_then(Value::as_str) {
        Some("type") => Some(vec![NodeKind::TypeAlias]),
        Some(kind) if !kind.is_empty() => kind.parse::<NodeKind>().ok().map(|kind| vec![kind]),
        _ => None,
    }
}

/// The projects a scope names, the asking project first; and the names or
/// projects that could not be searched.
fn scope_targets(
    fed: &GraphSet,
    scope: &ProjectScope,
    current: Option<&Path>,
) -> (Vec<Target>, Vec<Value>) {
    let mut failed = Vec::new();
    let Some(atlas) = fed.atlas() else {
        let targets = current
            .map(|root| Target {
                root: root.to_path_buf(),
                name: fed.project_name(root),
            })
            .into_iter()
            .collect();
        failed.push(json!({ "project": "atlas", "message": "no projects registered" }));
        return (targets, failed);
    };
    let current_project = current.and_then(|root| atlas.project_for_path(root).ok().flatten());
    let mut chosen: Vec<crate::atlas::Project> = Vec::new();
    let cap = match scope {
        ProjectScope::All => {
            chosen = atlas
                .projects()
                .unwrap_or_default()
                .into_iter()
                .filter(|project| project.status != ProjectStatus::Missing)
                .collect();
            MAX_ALL
        }
        ProjectScope::Linked => {
            if let Some(project) = &current_project {
                let mut ids: BTreeSet<i64> = BTreeSet::new();
                for link in atlas.links_from(project.id).unwrap_or_default() {
                    if is_code_link(link.kind) && link.is_cross_project() {
                        ids.extend(link.to_project);
                    }
                }
                for link in atlas.links_to(project.id).unwrap_or_default() {
                    if is_code_link(link.kind) {
                        ids.insert(link.from_project);
                    }
                }
                chosen.push(project.clone());
                chosen.extend(
                    ids.into_iter()
                        .filter_map(|id| atlas.project(id).ok().flatten()),
                );
            } else if let Some(root) = current {
                failed.push(json!({
                    "project": root,
                    "message": "not registered in the atlas; only this project was searched"
                }));
            }
            MAX_LINKED
        }
        ProjectScope::Named(names) => {
            for name in names {
                let path = Path::new(name);
                let by_path = path
                    .is_absolute()
                    .then(|| atlas.project_for_path(path).ok().flatten())
                    .flatten();
                let found = match by_path {
                    Some(project) => vec![project],
                    None => atlas.projects_named(name).unwrap_or_default(),
                };
                if found.is_empty() {
                    failed.push(json!({ "project": name, "message": "not a registered project" }));
                }
                chosen.extend(found);
            }
            MAX_LINKED
        }
    };
    // The asking project first, then by name; each root once.
    let current_root = current_project.as_ref().map(|p| p.root.clone());
    chosen.sort_by(|a, b| {
        (Some(&b.root) == current_root.as_ref())
            .cmp(&(Some(&a.root) == current_root.as_ref()))
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.root.cmp(&b.root))
    });
    let mut seen = BTreeSet::new();
    chosen.retain(|project| seen.insert(project.root.clone()));
    let over = chosen.len().saturating_sub(cap);
    if over > 0 {
        failed.push(json!({
            "project": format!("{over} more"),
            "message": format!("over the {cap}-project cap; name them in `projects`")
        }));
    }
    let mut targets: Vec<Target> = chosen
        .into_iter()
        .take(cap)
        .map(|project| Target {
            root: project.root,
            name: project.name,
        })
        .collect();
    if targets.is_empty() {
        targets.extend(current.map(|root| Target {
            root: root.to_path_buf(),
            name: fed.project_name(root),
        }));
    }
    (targets, failed)
}

fn is_code_link(kind: LinkKind) -> bool {
    !matches!(kind, LinkKind::SameRemote | LinkKind::NestedWorkspace)
}
