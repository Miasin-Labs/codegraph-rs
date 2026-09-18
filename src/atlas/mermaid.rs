//! The project graph as a Mermaid flowchart.
//!
//! Nodes are projects (name + home-relative root); checkouts of one
//! repository (a main checkout and its worktrees) share a subgraph. Edges
//! are cross-project links, one per `(from, to, kind)` with a `×n` count
//! when several manifest entries say the same thing; `same_remote` pairs
//! are drawn once, undirected.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::path::Path;

use super::kinds::LinkKind;
use super::model::{Project, ProjectLink};

/// What to draw.
#[derive(Debug, Clone, Default)]
pub struct GraphOptions {
    /// Only the cluster within `depth` links of this project.
    pub focus: Option<i64>,
    pub depth: usize,
    /// Only these kinds (empty: all).
    pub kinds: Vec<LinkKind>,
    /// Also draw projects with no links (unfocused graphs only).
    pub include_isolated: bool,
}

/// Render `projects`/`links` as a `flowchart LR`.
pub fn render(projects: &[Project], links: &[ProjectLink], opts: &GraphOptions) -> String {
    let by_id: BTreeMap<i64, &Project> = projects.iter().map(|p| (p.id, p)).collect();
    // (from, to, kind) → count, cross-project links of the chosen kinds.
    let mut edges: BTreeMap<(i64, i64, LinkKind), usize> = BTreeMap::new();
    for link in links {
        let Some(to) = link.to_project else { continue };
        if !link.is_cross_project()
            || !(opts.kinds.is_empty() || opts.kinds.contains(&link.kind))
            || !by_id.contains_key(&link.from_project)
            || !by_id.contains_key(&to)
        {
            continue;
        }
        if link.kind == LinkKind::SameRemote {
            // Stored both ways; one undirected relation per pair.
            let pair = (link.from_project.min(to), link.from_project.max(to));
            edges.insert((pair.0, pair.1, link.kind), 1);
        } else {
            *edges.entry((link.from_project, to, link.kind)).or_default() += 1;
        }
    }
    let shown: BTreeSet<i64> = match opts.focus {
        Some(focus) => cluster(focus, opts.depth, &edges),
        None if opts.include_isolated => by_id.keys().copied().collect(),
        None => edges.keys().flat_map(|(a, b, _)| [*a, *b]).collect(),
    };
    edges.retain(|(a, b, _), _| shown.contains(a) && shown.contains(b));

    let home = dirs::home_dir();
    let mut out = String::from("flowchart LR\n");
    // Group checkouts of one repository.
    let mut groups: BTreeMap<String, Vec<&Project>> = BTreeMap::new();
    let mut loose: Vec<&Project> = Vec::new();
    for project in shown.iter().filter_map(|id| by_id.get(id)) {
        match project.repo_root.as_ref().filter(|_| project.is_checkout()) {
            Some(repo) => groups
                .entry(repo.to_string_lossy().into_owned())
                .or_default()
                .push(project),
            None => loose.push(project),
        }
    }
    for (index, (repo, members)) in groups.iter().enumerate() {
        if members.len() < 2 {
            loose.extend(members);
            continue;
        }
        let title = Path::new(repo)
            .file_name()
            .map_or_else(|| repo.clone(), |n| n.to_string_lossy().into_owned());
        let _ = writeln!(
            out,
            "  subgraph repo{index}[\"{} (checkouts)\"]",
            escape(&title)
        );
        for project in members {
            let _ = writeln!(out, "    {}", node(project, home.as_deref()));
        }
        out.push_str("  end\n");
    }
    loose.sort_by_key(|p| p.id);
    for project in loose {
        let _ = writeln!(out, "  {}", node(project, home.as_deref()));
    }
    for ((from, to, kind), count) in &edges {
        let label = if *count > 1 {
            format!("{} ×{count}", kind.label())
        } else {
            kind.label().to_owned()
        };
        let arrow = match kind {
            LinkKind::SameRemote => format!("-. {label} .-"),
            LinkKind::NestedWorkspace => format!("-. {label} .->"),
            LinkKind::CargoWorkspaceMember | LinkKind::NpmWorkspace => {
                format!("== {label} ==>")
            }
            _ => format!("-- {label} -->"),
        };
        let _ = writeln!(out, "  p{from} {arrow} p{to}");
    }
    out
}

/// Projects within `depth` links of `focus`, following links both ways.
fn cluster(
    focus: i64,
    depth: usize,
    edges: &BTreeMap<(i64, i64, LinkKind), usize>,
) -> BTreeSet<i64> {
    let mut seen = BTreeSet::from([focus]);
    let mut queue = VecDeque::from([(focus, 0usize)]);
    while let Some((id, d)) = queue.pop_front() {
        if d >= depth {
            continue;
        }
        for (a, b, _) in edges.keys() {
            let next = if *a == id {
                *b
            } else if *b == id {
                *a
            } else {
                continue;
            };
            if seen.insert(next) {
                queue.push_back((next, d + 1));
            }
        }
    }
    seen
}

fn node(project: &Project, home: Option<&Path>) -> String {
    let root = match home.and_then(|h| project.root.strip_prefix(h).ok()) {
        Some(rel) => format!("~/{}", rel.display()),
        None => project.root.display().to_string(),
    };
    format!(
        "p{}[\"{}<br/>{}\"]",
        project.id,
        escape(&project.name),
        escape(&root)
    )
}

/// Mermaid label text: quotes and angle brackets as entities.
fn escape(text: &str) -> String {
    text.replace('"', "#quot;")
        .replace('<', "#lt;")
        .replace('>', "#gt;")
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::atlas::ProjectStatus;

    fn project(id: i64, name: &str, repo: Option<&str>) -> Project {
        let root = PathBuf::from(format!("/w/{name}"));
        Project {
            id,
            root: root.clone(),
            name: name.to_owned(),
            checkout_root: repo.map(|_| root.clone()),
            repo_root: repo.map(PathBuf::from),
            is_worktree: false,
            remote: None,
            branch: None,
            head_commit: None,
            languages: Vec::new(),
            file_count: None,
            node_count: None,
            edge_count: None,
            counts_exact: true,
            index_bytes: None,
            index_schema: None,
            extraction_version: None,
            engine_version: None,
            last_indexed_ms: None,
            last_seen_ms: 0,
            registered_ms: 0,
            status: ProjectStatus::Ok,
        }
    }

    fn link(id: i64, from: i64, to: i64, kind: LinkKind) -> ProjectLink {
        ProjectLink {
            id,
            from_project: from,
            to_project: Some(to),
            to_path: PathBuf::from("/x"),
            kind,
            evidence: None,
            detail: None,
        }
    }

    #[test]
    fn renders_grouped_checkouts_counted_edges_and_focus() {
        let projects = vec![
            project(1, "app", Some("/w/app")),
            project(2, "app@wt", Some("/w/app")),
            project(3, "lib\"x", None),
            project(4, "far", None),
            project(5, "island", None),
        ];
        let links = vec![
            link(1, 1, 3, LinkKind::CargoPathDep),
            link(2, 1, 3, LinkKind::CargoPathDep),
            link(3, 3, 4, LinkKind::GoReplace),
            link(4, 1, 1, LinkKind::CargoWorkspaceMember),
            link(5, 2, 4, LinkKind::SameRemote),
            link(6, 4, 2, LinkKind::SameRemote),
        ];
        let all = render(&projects, &links, &GraphOptions::default());
        assert!(all.starts_with("flowchart LR\n"));
        assert!(all.contains("subgraph repo0[\"app (checkouts)\"]"));
        assert!(all.contains("p3[\"lib#quot;x<br/>/w/lib#quot;x\"]"));
        assert!(all.contains("p1 -- cargo path dep ×2 --> p3"));
        assert!(all.contains("p3 -- go replace --> p4"));
        assert!(all.contains("p2 -. same remote .- p4"));
        assert!(
            !all.contains("p1 == cargo member"),
            "internal links are not drawn"
        );
        assert!(!all.contains("island"));

        let focused = render(
            &projects,
            &links,
            &GraphOptions {
                focus: Some(1),
                depth: 1,
                ..GraphOptions::default()
            },
        );
        assert!(focused.contains("p1 -- cargo path dep ×2 --> p3"));
        assert!(!focused.contains("p4["), "{focused}");
    }
}
