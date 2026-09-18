//! `codegraph projects …` — the atlas CLI — and the registration hook the
//! index-writing commands (`init`, `index`, `sync`) call when they succeed.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::time::Duration;

use codegraph::atlas::mermaid::{self, GraphOptions};
use codegraph::atlas::scan::scan;
use codegraph::atlas::{
    self,
    Atlas,
    LinkKind,
    Project,
    ProjectLink,
    RegisterOutcome,
    ScanOptions,
    register_project,
};
use codegraph::history::atlas_join::ActivitySummary;
use serde::Serialize;
use serde_json::json;

use super::{
    ProjectsCommands,
    bold,
    dim,
    error_msg,
    print_json,
    process,
    resolve_absolute,
    resolve_project_path_quiet,
    success,
    warn,
};

mod activity;
mod render;

use render::{ago, human_bytes, languages_line, short_path, status_label};

type CmdResult = Result<(), String>;

/// Register `root` after a successful index write. Never fails the command:
/// contention or an error is one line on stderr (nothing when `quiet`).
pub(crate) fn register_after_write(root: &Path, quiet: bool) {
    let outcome = register_project(root);
    if let (false, Some(warning)) = (quiet, outcome.warning(root)) {
        eprintln!("[codegraph] {warning}");
    }
}

pub(crate) fn cmd_projects(command: Option<ProjectsCommands>, json: bool) {
    let command = command.unwrap_or(ProjectsCommands::List {
        sort: "name".into(),
        limit: None,
    });
    let result = match command {
        ProjectsCommands::List { sort, limit } => cmd_list(&sort, limit, json),
        ProjectsCommands::Show { project } => cmd_show(&project, json),
        ProjectsCommands::Links { project, all } => cmd_links(&project, all, json),
        ProjectsCommands::Scan {
            roots,
            max_depth,
            budget_secs,
        } => cmd_scan(&roots, max_depth, budget_secs, json),
        ProjectsCommands::Prune { remove } => cmd_prune(remove, json),
        ProjectsCommands::Graph {
            project,
            format,
            depth,
            kinds,
            all,
        } => cmd_graph(project.as_deref(), &format, depth, &kinds, all),
        ProjectsCommands::Register { path, quiet } => cmd_register(path.as_deref(), quiet, json),
    };
    if let Err(message) = result {
        error_msg(&message);
        process::exit(1);
    }
}

fn read_atlas() -> Result<Option<Atlas>, String> {
    atlas::open_read_only().map_err(|e| e.to_string())
}

fn write_atlas() -> Result<Atlas, String> {
    codegraph::directory::ensure_codegraph_home().map_err(|e| e.to_string())?;
    Atlas::open(&atlas::atlas_path()).map_err(|e| e.to_string())
}

fn no_atlas_hint() -> String {
    format!(
        "No projects registered yet ({} does not exist). Index a project, or run \
         \"codegraph projects scan\" to register existing indexes.",
        atlas::atlas_path().display()
    )
}

/// A project by path (the nearest registered root at or above it) or name.
fn resolve(atlas: &Atlas, arg: &str) -> Result<Project, String> {
    let path = resolve_absolute(Some(arg));
    if path.exists() {
        if let Some(project) = atlas.project_for_path(&path).map_err(|e| e.to_string())? {
            return Ok(project);
        }
    }
    let mut named = atlas.projects_named(arg).map_err(|e| e.to_string())?;
    match named.len() {
        1 => Ok(named.remove(0)),
        0 => Err(format!(
            "No registered project matches \"{arg}\" (see \"codegraph projects\")"
        )),
        n => Err(format!(
            "\"{arg}\" names {n} projects; pass one of their paths:\n{}",
            named
                .iter()
                .map(|p| format!("  {}", p.root.display()))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}

/// A listed project with its agent activity (JSON).
#[derive(Serialize)]
struct Listed<'a> {
    #[serde(flatten)]
    project: &'a Project,
    #[serde(flatten)]
    activity: ActivitySummary,
}

fn cmd_list(sort: &str, limit: Option<usize>, json: bool) -> CmdResult {
    if !matches!(sort, "name" | "activity") {
        return Err(format!("Unknown sort \"{sort}\" (one of: name, activity)"));
    }
    let Some(atlas) = read_atlas()? else {
        if json {
            return print_json(&json!({ "atlas": atlas::atlas_path(), "projects": [] }));
        }
        println!("{}", no_atlas_hint());
        return Ok(());
    };
    let projects = atlas.projects().map_err(|e| e.to_string())?;
    let reader = activity::open_reader();
    let (summaries, cut_short) = activity::summaries(reader.as_ref(), &atlas, &projects);
    let mut rows: Vec<Listed<'_>> = projects
        .iter()
        .zip(summaries)
        .map(|(project, activity)| Listed { project, activity })
        .collect();
    if sort == "activity" {
        // Stable: projects without activity keep their name order.
        rows.sort_by_key(|r| std::cmp::Reverse(r.activity.last_activity_ms));
    }
    let total = rows.len();
    rows.truncate(limit.unwrap_or(usize::MAX));
    if json {
        let mut value = json!({ "atlas": atlas::atlas_path(), "projects": rows });
        if cut_short {
            value["activityIncomplete"] = json!(true);
        }
        return print_json(&value);
    }
    let shown = if rows.len() < total {
        format!("{} of {total}", rows.len())
    } else {
        total.to_string()
    };
    println!("{shown} projects in {}\n", short_path(&atlas::atlas_path()));
    let width = rows
        .iter()
        .map(|r| r.project.name.len())
        .max()
        .unwrap_or(0)
        .min(40);
    for row in &rows {
        let p = row.project;
        let counts = match (p.file_count, p.node_count) {
            (Some(files), Some(nodes)) => format!(
                "{} files, {}{} nodes",
                super::format_number(files),
                if p.counts_exact { "" } else { "~" },
                super::format_number(nodes)
            ),
            _ => String::new(),
        };
        println!(
            "  {:<width$}  {}  {}  {}  {}  {}  {}",
            bold(&p.name),
            status_label(p.status),
            languages_line(&p.languages, 2),
            counts,
            p.index_bytes.map(human_bytes).unwrap_or_default(),
            p.last_indexed_ms
                .map(|t| format!("indexed {}", ago(t)))
                .unwrap_or_default(),
            activity::summary_label(&row.activity),
            width = width + 8,
        );
        let mut second = vec![short_path(&p.root)];
        if let Some(remote) = &p.remote {
            second.push(match &p.branch {
                Some(branch) => format!("{remote} ({branch})"),
                None => remote.clone(),
            });
        }
        println!("    {}", dim(&second.join("  ")));
    }
    if cut_short {
        warn("Agent activity is incomplete: the history read hit its time bound");
    }
    Ok(())
}

fn cmd_show(arg: &str, json: bool) -> CmdResult {
    let atlas = read_atlas()?.ok_or_else(no_atlas_hint)?;
    let project = resolve(&atlas, arg)?;
    let err = |e: atlas::AtlasError| e.to_string();
    let worktrees = atlas.worktrees_of(&project).map_err(err)?;
    let clones = atlas.clones_of(&project).map_err(err)?;
    let links_out = atlas.links_from(project.id).map_err(err)?;
    let links_in = atlas.links_to(project.id).map_err(err)?;
    let reader = activity::open_reader();
    let (own_activity, linked_activity) = match &reader {
        Some(reader) => activity::project_activity(reader, &atlas, &project),
        None => (None, Vec::new()),
    };
    if json {
        return print_json(&json!({
            "project": project,
            "worktrees": worktrees,
            "clones": clones,
            "linksOut": links_out,
            "linksIn": links_in,
            "activity": own_activity,
            "linkedActivity": linked_activity,
        }));
    }
    let names = project_names(&atlas)?;
    println!("{}  {}", bold(&project.name), status_label(project.status));
    let row = |label: &str, value: String| {
        if !value.is_empty() {
            println!("  {:<10} {value}", dim(label));
        }
    };
    row("root", short_path(&project.root));
    row("git", git_line(&project));
    row("index", index_line(&project));
    row("contents", contents_line(&project));
    row("languages", languages_line(&project.languages, 8));
    row(
        "indexed",
        project
            .last_indexed_ms
            .map(|t| format!("{} · seen {}", ago(t), ago(project.last_seen_ms)))
            .unwrap_or_default(),
    );
    for (label, group) in [("worktrees", &worktrees), ("clones", &clones)] {
        for (i, other) in group.iter().enumerate() {
            let branch = other.branch.as_deref().unwrap_or("detached");
            row(
                if i == 0 { label } else { "" },
                format!(
                    "{}  {}  ({branch})",
                    other.name,
                    dim(&short_path(&other.root))
                ),
            );
        }
    }
    print_links("links out", &links_out, &names, &project, false, false);
    print_links("links in", &links_in, &names, &project, true, false);
    activity::print_activity(own_activity.as_ref(), reader.is_some());
    activity::print_linked(&linked_activity);
    Ok(())
}

fn cmd_links(arg: &str, all: bool, json: bool) -> CmdResult {
    let atlas = read_atlas()?.ok_or_else(no_atlas_hint)?;
    let project = resolve(&atlas, arg)?;
    let keep = |l: &ProjectLink| all || !l.is_internal();
    let links_out: Vec<ProjectLink> = atlas
        .links_from(project.id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(keep)
        .collect();
    let links_in = atlas.links_to(project.id).map_err(|e| e.to_string())?;
    if json {
        return print_json(&json!({
            "project": { "id": project.id, "name": project.name, "root": project.root },
            "linksOut": links_out,
            "linksIn": links_in,
        }));
    }
    let names = project_names(&atlas)?;
    println!(
        "{}  {}",
        bold(&project.name),
        dim(&short_path(&project.root))
    );
    print_links("links out", &links_out, &names, &project, false, all);
    print_links("links in", &links_in, &names, &project, true, all);
    Ok(())
}

fn cmd_scan(roots: &[String], max_depth: usize, budget_secs: u64, json: bool) -> CmdResult {
    let roots = if roots.is_empty() {
        vec![dirs::home_dir().ok_or("no home directory; pass --root")?]
    } else {
        roots.iter().map(|r| resolve_absolute(Some(r))).collect()
    };
    let mut atlas = write_atlas()?;
    let opts = ScanOptions {
        roots,
        max_depth,
        budget: Duration::from_secs(budget_secs),
        skip: vec![codegraph::directory::codegraph_home()],
    };
    let report = scan(&mut atlas, &opts).map_err(|e| e.to_string())?;
    if json {
        return print_json(&report);
    }
    println!(
        "Scanned {} directories in {}: {} indexes ({} new, {} refreshed) — {} ok, {} stale schema, {} unreadable",
        super::format_number(report.dirs_visited as u64),
        super::format_duration(i64::try_from(report.elapsed_ms).unwrap_or(i64::MAX)),
        report.found.len(),
        report.new,
        report.refreshed,
        report.ok,
        report.stale_schema,
        report.unreadable,
    );
    let projects = atlas.projects().map_err(|e| e.to_string())?;
    let links = atlas.links().map_err(|e| e.to_string())?;
    let mut by_kind: BTreeMap<LinkKind, usize> = BTreeMap::new();
    let mut internal = 0usize;
    for link in &links {
        if link.is_cross_project() {
            *by_kind.entry(link.kind).or_default() += 1;
        } else {
            internal += 1;
        }
    }
    let cross: usize = by_kind.values().sum();
    let kinds = by_kind
        .iter()
        .map(|(k, n)| format!("{} {n}", k.label()))
        .collect::<Vec<_>>()
        .join(", ");
    println!(
        "Atlas: {} projects, {cross} cross-project links ({kinds}), {internal} within a project or unresolved",
        projects.len()
    );
    let mut repos: BTreeMap<String, usize> = BTreeMap::new();
    for project in projects.iter().filter(|p| p.is_checkout()) {
        if let Some(key) = project.repo_key() {
            *repos.entry(key).or_default() += 1;
        }
    }
    let multi = repos.values().filter(|n| **n > 1).count();
    if multi > 0 {
        println!("Repositories with several registered checkouts (worktrees): {multi}");
    }
    for (root, why) in &report.skipped {
        warn(&format!("Skipped {}: {why}", root.display()));
    }
    if report.budget_exhausted {
        warn(&format!(
            "Stopped at the {budget_secs}s budget — rerun (or raise --budget-secs) to finish"
        ));
    }
    Ok(())
}

fn cmd_prune(remove: bool, json: bool) -> CmdResult {
    if read_atlas()?.is_none() {
        return if json {
            print_json(&json!({ "checked": 0, "markedMissing": [], "removed": [] }))
        } else {
            println!("{}", no_atlas_hint());
            Ok(())
        };
    }
    let report = write_atlas()?.prune(remove).map_err(|e| e.to_string())?;
    if json {
        return print_json(&report);
    }
    for root in &report.marked_missing {
        println!("  missing  {root}");
    }
    for root in &report.removed {
        println!("  removed  {root}");
    }
    success(&format!(
        "Checked {} projects: {} newly missing, {} removed",
        report.checked,
        report.marked_missing.len(),
        report.removed.len()
    ));
    Ok(())
}

fn cmd_graph(
    focus: Option<&str>,
    format: &str,
    depth: usize,
    kinds: &[String],
    all: bool,
) -> CmdResult {
    if format != "mermaid" {
        return Err(format!(
            "Unknown graph format \"{format}\" (supported: mermaid)"
        ));
    }
    let kinds = kinds
        .iter()
        .map(|k| {
            LinkKind::parse(k).ok_or_else(|| {
                let valid: Vec<&str> = LinkKind::ALL.iter().map(|k| k.as_str()).collect();
                format!("Unknown link kind \"{k}\" (one of: {})", valid.join(", "))
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let Some(atlas) = read_atlas()? else {
        println!("flowchart LR");
        return Ok(());
    };
    let focus = focus.map(|arg| resolve(&atlas, arg)).transpose()?;
    let projects = atlas.projects().map_err(|e| e.to_string())?;
    let links = atlas.links().map_err(|e| e.to_string())?;
    let opts = GraphOptions {
        focus: focus.map(|p| p.id),
        depth,
        kinds,
        include_isolated: all,
    };
    print!("{}", mermaid::render(&projects, &links, &opts));
    Ok(())
}

fn cmd_register(path: Option<&str>, quiet: bool, json: bool) -> CmdResult {
    let root = resolve_project_path_quiet(path);
    let outcome = register_project(&root);
    if quiet {
        return Ok(());
    }
    if json {
        let value = match &outcome {
            RegisterOutcome::Registered {
                project_id,
                links,
                new,
            } => {
                json!({ "registered": true, "projectId": project_id, "links": links, "new": new, "root": root })
            }
            other => json!({ "registered": false, "reason": format!("{other:?}"), "root": root }),
        };
        return print_json(&value);
    }
    match outcome {
        RegisterOutcome::Registered { links, new, .. } => {
            success(&format!(
                "{} {} ({links} manifest links)",
                if new { "Registered" } else { "Refreshed" },
                root.display()
            ));
            Ok(())
        }
        RegisterOutcome::Disabled => {
            warn("The atlas is disabled (CODEGRAPH_ATLAS=0)");
            Ok(())
        }
        RegisterOutcome::NotIndexed => Err(format!(
            "No CodeGraph index at or above {} (run \"codegraph init\" first)",
            root.display()
        )),
        other => Err(other.warning(&root).unwrap_or_default()),
    }
}

fn project_names(atlas: &Atlas) -> Result<HashMap<i64, Project>, String> {
    Ok(atlas
        .projects()
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|p| (p.id, p))
        .collect())
}

/// Links grouped cross-project first; `incoming` names the source.
fn print_links(
    label: &str,
    links: &[ProjectLink],
    names: &HashMap<i64, Project>,
    project: &Project,
    incoming: bool,
    all: bool,
) {
    let (shown, hidden): (Vec<&ProjectLink>, Vec<&ProjectLink>) = links
        .iter()
        .partition(|l| all || l.is_cross_project() || incoming);
    let unresolved = hidden.iter().filter(|l| l.to_project.is_none()).count();
    let internal = hidden.len() - unresolved;
    let mut summary = format!("{label} ({})", shown.len());
    if internal + unresolved > 0 {
        summary.push_str(&dim(&format!(
            " + {internal} within the project, {unresolved} to unregistered dirs — --all to list"
        )));
    }
    println!("  {summary}");
    for link in shown {
        let other_id = if incoming {
            Some(link.from_project)
        } else {
            link.to_project
        };
        let other = other_id
            .and_then(|id| names.get(&id))
            .map_or_else(|| short_path(&link.to_path), |p| p.name.clone());
        let arrow = if incoming { "←" } else { "→" };
        let evidence_base = if incoming {
            other_id
                .and_then(|id| names.get(&id))
                .map(|p| p.root.as_path())
        } else {
            Some(project.root.as_path())
        };
        let evidence = link.evidence_label(evidence_base).unwrap_or_default();
        let detail = link.detail.as_deref().unwrap_or_default();
        println!(
            "    {:<14} {arrow} {:<28} {}  {}",
            link.kind.label(),
            other,
            dim(&evidence),
            dim(detail)
        );
    }
}

fn git_line(p: &Project) -> String {
    let mut parts = Vec::new();
    if let Some(remote) = &p.remote {
        parts.push(remote.clone());
    }
    match (&p.branch, &p.head_commit) {
        (Some(b), Some(c)) => parts.push(format!("{b} @ {}", &c[..c.len().min(8)])),
        (Some(b), None) => parts.push(b.clone()),
        (None, Some(c)) => parts.push(format!("detached @ {}", &c[..c.len().min(8)])),
        (None, None) => {}
    }
    if p.is_worktree {
        if let Some(repo) = &p.repo_root {
            parts.push(format!("worktree of {}", short_path(repo)));
        }
    } else if p.checkout_root.is_some() && !p.is_checkout() {
        if let Some(checkout) = &p.checkout_root {
            parts.push(format!("inside checkout {}", short_path(checkout)));
        }
    }
    parts.join(" · ")
}

fn index_line(p: &Project) -> String {
    let mut parts = Vec::new();
    if let Some(v) = p.index_schema {
        parts.push(format!("schema v{v}"));
    }
    if let Some(v) = p.extraction_version {
        parts.push(format!("extraction v{v}"));
    }
    if let Some(v) = &p.engine_version {
        parts.push(format!("engine {v}"));
    }
    if let Some(b) = p.index_bytes {
        parts.push(human_bytes(b));
    }
    parts.join(" · ")
}

fn contents_line(p: &Project) -> String {
    let approx = if p.counts_exact { "" } else { "~" };
    let mut parts = Vec::new();
    if let Some(n) = p.file_count {
        parts.push(format!("{} files", super::format_number(n)));
    }
    if let Some(n) = p.node_count {
        parts.push(format!("{approx}{} nodes", super::format_number(n)));
    }
    if let Some(n) = p.edge_count {
        parts.push(format!("{approx}{} edges", super::format_number(n)));
    }
    parts.join(" · ")
}
