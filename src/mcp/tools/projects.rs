//! codegraph_projects — the projects view of the federated graph (opt-in):
//! every indexed project on this machine from the atlas, and for one
//! project its code links (Cargo path deps, workspaces, go replace) either
//! way, its dependencies from `deps/registry.db`, and the graphs its code
//! calls into (external edges per dependency or linked project).
//!
//! Read-only and bounded: the atlas, the registry and the project's index
//! are opened read-only (creating nothing), lists are capped, and the
//! per-graph edge counts run under the call's deadline.

use std::collections::HashMap;
use std::path::Path;

use serde::Serialize;
use serde_json::{Map, Value, json};

use super::context::ToolHandler;
use super::format::{json_len, mcp_output_budget, num_or, rows_within_budget};
use super::output::{notices_schema, successes_or_error};
use super::schema::ToolResult;
use crate::atlas::{Atlas, LinkKind, Project, ProjectLink, ProjectStatus};
use crate::db::ExternalGraphKind;
use crate::error::Result;
use crate::federation::{GraphId, GraphSet, dependency_label};
use crate::utils::clamp;

/// Links, and graphs called into, listed for one project.
const SHOW_CAP: usize = 30;

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// One project of the list.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectRow {
    name: String,
    root: String,
    /// Absent when the index is readable at this build's schema.
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    languages: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<u64>,
    /// Code links to other projects / from other projects.
    #[serde(skip_serializing_if = "is_zero")]
    links: usize,
    #[serde(skip_serializing_if = "is_zero")]
    used_by: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectsOutput {
    schema_version: u32,
    kind: &'static str,
    total: usize,
    projects: Vec<ProjectRow>,
    #[serde(skip_serializing_if = "is_false")]
    truncated: bool,
}

/// A code link of the shown project.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LinkRow {
    kind: &'static str,
    /// The other project's name, or the directory when it is not one.
    project: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DependencySummary {
    total: usize,
    direct: usize,
    /// Versions with a readable shard (what queries can follow into).
    with_shard: usize,
}

/// A graph the project's code calls into.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GraphRow {
    graph: String,
    edges: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProjectOutput {
    schema_version: u32,
    kind: &'static str,
    name: String,
    root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remote: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    languages: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    nodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_indexed_ms: Option<i64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    links: Vec<LinkRow>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    used_by: Vec<LinkRow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    dependencies: Option<DependencySummary>,
    /// External edges of its index per graph, most first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    graphs: Vec<GraphRow>,
    #[serde(skip_serializing_if = "is_zero")]
    graphs_omitted: usize,
}

impl ToolHandler {
    pub(in crate::mcp::tools) fn handle_projects(
        &self,
        args: &Map<String, Value>,
    ) -> Result<ToolResult> {
        let fed = self.federation_reader();
        let Some(atlas) = fed.atlas() else {
            return Ok(self.error_result(
                "No projects are registered on this machine yet (no atlas). Indexing a project \
                 registers it; `codegraph projects scan` registers existing indexes.",
            ));
        };
        match args
            .get("project")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|p| !p.is_empty())
        {
            Some(project) => self.show_project(&fed, atlas, project),
            None => self.list_projects(atlas, args),
        }
    }

    fn list_projects(&self, atlas: &Atlas, args: &Map<String, Value>) -> Result<ToolResult> {
        let limit = clamp(num_or(args, "limit", 50.0), 1.0, 500.0) as usize;
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .map(|q| q.trim().to_lowercase())
            .filter(|q| !q.is_empty());
        let projects = atlas.projects().unwrap_or_default();
        let links = atlas.links().unwrap_or_default();
        let mut out_links: HashMap<i64, usize> = HashMap::new();
        let mut in_links: HashMap<i64, usize> = HashMap::new();
        for link in links.iter().filter(|link| is_code_link(link)) {
            *out_links.entry(link.from_project).or_default() += 1;
            if let Some(to) = link.to_project {
                *in_links.entry(to).or_default() += 1;
            }
        }
        let rows: Vec<ProjectRow> = projects
            .iter()
            .filter(|project| {
                query.as_ref().is_none_or(|q| {
                    project.name.to_lowercase().contains(q)
                        || project.root.to_string_lossy().to_lowercase().contains(q)
                })
            })
            .map(|project| ProjectRow {
                name: project.name.clone(),
                root: project.root.to_string_lossy().into_owned(),
                status: status(project),
                languages: languages(project),
                files: project.file_count,
                links: out_links.get(&project.id).copied().unwrap_or(0),
                used_by: in_links.get(&project.id).copied().unwrap_or(0),
            })
            .collect();
        let total = rows.len();
        let mut output = ProjectsOutput {
            schema_version: 1,
            kind: "projects",
            total,
            projects: Vec::new(),
            truncated: true,
        };
        let keep = rows_within_budget(
            mcp_output_budget(),
            json_len(&output),
            rows.iter().take(limit).map(json_len),
        );
        output.projects = rows.into_iter().take(keep).collect();
        output.truncated = output.projects.len() < total;
        let text = output
            .projects
            .iter()
            .map(|row| {
                format!(
                    "- {} ({}){}{}",
                    row.name,
                    row.root,
                    row.status
                        .map(|status| format!(" [{status}]"))
                        .unwrap_or_default(),
                    row.languages
                        .as_deref()
                        .map(|langs| format!(" — {langs}"))
                        .unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.structured_result(
            &format!("{} of {total} projects\n\n{text}", output.projects.len()),
            &output,
        )
    }

    fn show_project(&self, fed: &GraphSet, atlas: &Atlas, arg: &str) -> Result<ToolResult> {
        let project = match self.find_project(atlas, arg) {
            Ok(project) => project,
            Err(message) => return Ok(self.error_result(&message)),
        };
        let names: HashMap<i64, String> = atlas
            .projects()
            .unwrap_or_default()
            .into_iter()
            .map(|p| (p.id, p.name))
            .collect();
        let link_row = |link: &ProjectLink, other: Option<i64>| LinkRow {
            kind: link.kind.as_str(),
            project: other
                .and_then(|id| names.get(&id).cloned())
                .unwrap_or_else(|| link.to_path.to_string_lossy().into_owned()),
            detail: link.detail.clone(),
        };
        let mut links: Vec<LinkRow> = atlas
            .links_from(project.id)
            .unwrap_or_default()
            .iter()
            .filter(|link| is_code_link(link) && !link.is_internal())
            .map(|link| link_row(link, link.to_project))
            .collect();
        dedup_links(&mut links);
        let mut used_by: Vec<LinkRow> = atlas
            .links_to(project.id)
            .unwrap_or_default()
            .iter()
            .filter(|link| is_code_link(link))
            .map(|link| link_row(link, Some(link.from_project)))
            .collect();
        dedup_links(&mut used_by);
        links.truncate(SHOW_CAP);
        used_by.truncate(SHOW_CAP);

        let dependencies = fed.registry().and_then(|registry| {
            let root = project.root.to_string_lossy();
            let deps = registry.dependencies_of(&root).ok()?;
            (!deps.is_empty()).then(|| DependencySummary {
                total: deps.len(),
                direct: deps.iter().filter(|dep| dep.direct == Some(true)).count(),
                with_shard: deps
                    .iter()
                    .filter(|dep| {
                        crate::deps::ShardMeta::read(&fed.home().deps.shard_dir(&dep.key))
                            .is_some_and(|meta| meta.is_readable() && meta.state.has_shard())
                    })
                    .count(),
            })
        });

        let (graphs, graphs_omitted) = graphs_called(fed, &project.root);
        let output = ProjectOutput {
            schema_version: 1,
            kind: "project",
            name: project.name.clone(),
            root: project.root.to_string_lossy().into_owned(),
            status: status(&project),
            branch: project.branch.clone(),
            remote: project.remote.clone(),
            languages: languages(&project),
            files: project.file_count,
            nodes: project.node_count,
            last_indexed_ms: project.last_indexed_ms,
            links,
            used_by,
            dependencies,
            graphs,
            graphs_omitted,
        };
        let text = show_text(&output);
        self.structured_result(&text, &output)
    }

    /// A registered project by path (the nearest registered root at or
    /// above it; `.` is this session's project) or by name.
    fn find_project(&self, atlas: &Atlas, arg: &str) -> std::result::Result<Project, String> {
        let path = if arg == "." {
            self.get_code_graph(None)
                .map(|cg| cg.get_project_root().to_path_buf())
                .ok()
        } else {
            Some(Path::new(arg).to_path_buf()).filter(|path| path.is_absolute() && path.exists())
        };
        if let Some(path) = path {
            if let Ok(Some(project)) = atlas.project_for_path(&path) {
                return Ok(project);
            }
        }
        let mut named = atlas.projects_named(arg).unwrap_or_default();
        match named.len() {
            1 => Ok(named.remove(0)),
            0 => Err(format!(
                "No registered project matches \"{arg}\" (call codegraph_projects without \
                 `project` for the list)"
            )),
            n => Err(format!(
                "\"{arg}\" names {n} projects; pass one of their roots: {}",
                named
                    .iter()
                    .map(|p| p.root.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        }
    }
}

/// Links that are code (what cross-project queries follow), not derived
/// facts about checkouts.
fn is_code_link(link: &ProjectLink) -> bool {
    !matches!(link.kind, LinkKind::SameRemote | LinkKind::NestedWorkspace)
}

fn dedup_links(links: &mut Vec<LinkRow>) {
    let mut seen = std::collections::HashSet::new();
    links.retain(|link| seen.insert((link.kind, link.project.clone())));
}

fn status(project: &Project) -> Option<&'static str> {
    (project.status != ProjectStatus::Ok).then(|| project.status.as_str())
}

fn languages(project: &Project) -> Option<String> {
    let top: Vec<&str> = project
        .languages
        .iter()
        .take(3)
        .map(|lang| lang.language.as_str())
        .collect();
    (!top.is_empty()).then(|| top.join(", "))
}

/// The graphs a project's index has external edges into, most edges first.
fn graphs_called(fed: &GraphSet, root: &Path) -> (Vec<GraphRow>, usize) {
    let Ok(graph) = fed.open(&GraphId::project(root)) else {
        return (Vec::new(), 0);
    };
    if !graph.has_external_edges {
        return (Vec::new(), 0);
    }
    let counts = graph.queries().count_external_edges().unwrap_or_default();
    let omitted = counts.len().saturating_sub(SHOW_CAP);
    let rows = counts
        .into_iter()
        .take(SHOW_CAP)
        .map(|count| GraphRow {
            graph: match count.graph_kind {
                ExternalGraphKind::Dependency => dependency_label(&count.graph_key),
                ExternalGraphKind::Project => fed.project_name(Path::new(&count.graph_key)),
            },
            edges: count.edges,
        })
        .collect();
    (rows, omitted)
}

fn show_text(output: &ProjectOutput) -> String {
    let mut lines = vec![
        format!("## {} — {}", output.name, output.root),
        String::new(),
    ];
    if let Some(status) = output.status {
        lines.push(format!("Status: {status}"));
    }
    if let Some(languages) = &output.languages {
        lines.push(format!(
            "Languages: {languages}{}",
            output
                .files
                .map(|files| format!(" ({files} files)"))
                .unwrap_or_default()
        ));
    }
    let link_list = |rows: &[LinkRow]| {
        rows.iter()
            .map(|row| format!("{} ({})", row.project, row.kind))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if !output.links.is_empty() {
        lines.push(format!("Links to: {}", link_list(&output.links)));
    }
    if !output.used_by.is_empty() {
        lines.push(format!("Used by: {}", link_list(&output.used_by)));
    }
    if let Some(deps) = &output.dependencies {
        lines.push(format!(
            "Dependencies: {} ({} direct, {} with a shard)",
            deps.total, deps.direct, deps.with_shard
        ));
    }
    if !output.graphs.is_empty() {
        lines.push(format!(
            "Calls into: {}{}",
            output
                .graphs
                .iter()
                .map(|row| format!("{} ({})", row.graph, row.edges))
                .collect::<Vec<_>>()
                .join(", "),
            if output.graphs_omitted > 0 {
                format!(", +{} more", output.graphs_omitted)
            } else {
                String::new()
            }
        ));
    }
    lines.join("\n")
}

/// The declared shapes of the list and of one project.
pub(in crate::mcp::tools) fn projects_output_schema() -> Value {
    let link = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "kind": { "type": "string" },
            "project": { "type": "string" },
            "detail": { "type": "string" }
        },
        "required": ["kind", "project"]
    });
    let list = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "projects" },
            "notices": notices_schema(),
            "total": { "type": "integer" },
            "projects": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "name": { "type": "string" },
                    "root": { "type": "string" },
                    "status": { "type": "string" },
                    "languages": { "type": "string" },
                    "files": { "type": "integer" },
                    "links": { "type": "integer" },
                    "usedBy": { "type": "integer" }
                },
                "required": ["name", "root"]
            }},
            "truncated": { "type": "boolean" }
        },
        "required": ["schemaVersion", "kind", "total", "projects"]
    });
    let show = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "schemaVersion": { "type": "integer" },
            "kind": { "const": "project" },
            "notices": notices_schema(),
            "name": { "type": "string" },
            "root": { "type": "string" },
            "status": { "type": "string" },
            "branch": { "type": "string" },
            "remote": { "type": "string" },
            "languages": { "type": "string" },
            "files": { "type": "integer" },
            "nodes": { "type": "integer" },
            "lastIndexedMs": { "type": "integer" },
            "links": { "type": "array", "items": link.clone() },
            "usedBy": { "type": "array", "items": link },
            "dependencies": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "total": { "type": "integer" },
                    "direct": { "type": "integer" },
                    "withShard": { "type": "integer" }
                },
                "required": ["total", "direct", "withShard"]
            },
            "graphs": { "type": "array", "items": {
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "graph": { "type": "string" },
                    "edges": { "type": "integer" }
                },
                "required": ["graph", "edges"]
            }},
            "graphsOmitted": { "type": "integer" }
        },
        "required": ["schemaVersion", "kind", "name", "root"]
    });
    successes_or_error(vec![list, show])
}
