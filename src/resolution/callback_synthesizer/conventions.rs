//! File-convention synthesis for Delphi forms and SvelteKit routes.

use std::collections::HashSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::edge_meta;
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, EdgeKind, NodeKind, Provenance};

static PASCAL_FORM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(?:dfm|fmx)$").expect("valid regex"));
static SVELTEKIT_PAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(.*\/)(\+(?:page|layout))\.svelte$").expect("valid regex"));

fn reference_edge(source: &str, target: &str, line: u32, metadata: crate::types::Metadata) -> Edge {
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind: EdgeKind::References,
        metadata: Some(metadata),
        line: Some(line),
        column: None,
        provenance: Some(Provenance::Heuristic),
    }
}

/// Link a Delphi `.pas` unit to its basename-matched `.dfm`/`.fmx` form.
pub(super) fn pascal_form_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let all_files: HashSet<String> = ctx.get_all_files().into_iter().collect();
    let mut edges = Vec::new();

    for form_file in &all_files {
        if !PASCAL_FORM_RE.is_match(form_file) {
            continue;
        }
        let pas_file = PASCAL_FORM_RE.replace(form_file, ".pas").into_owned();
        if !all_files.contains(&pas_file) {
            continue;
        }
        let form_node = ctx
            .get_nodes_in_file(form_file)
            .into_iter()
            .find(|node| node.kind == NodeKind::File);
        let unit_node = ctx
            .get_nodes_in_file(&pas_file)
            .into_iter()
            .find(|node| node.kind == NodeKind::File);
        let (Some(form_node), Some(unit_node)) = (form_node, unit_node) else {
            continue;
        };
        edges.push(reference_edge(
            &unit_node.id,
            &form_node.id,
            unit_node.start_line,
            edge_meta(vec![
                ("synthesizedBy", Value::from("pascal-form")),
                ("registeredAt", Value::from(pas_file.as_str())),
            ]),
        ));
    }
    edges
}

/// Link a SvelteKit page/layout component to sibling `load` and `actions` hooks.
pub(super) fn sveltekit_load_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    const LOADER_SUFFIXES: [&str; 4] = [".server.ts", ".server.js", ".ts", ".js"];
    let hook_kinds = [
        NodeKind::Function,
        NodeKind::Method,
        NodeKind::Constant,
        NodeKind::Variable,
    ];
    let all_files: HashSet<String> = ctx.get_all_files().into_iter().collect();
    let mut edges = Vec::new();

    for page_file in &all_files {
        let Some(captures) = SVELTEKIT_PAGE_RE.captures(page_file) else {
            continue;
        };
        let Some(page) = ctx
            .get_nodes_in_file(page_file)
            .into_iter()
            .find(|node| node.kind == NodeKind::Component)
        else {
            continue;
        };
        let dir = captures.get(1).expect("directory capture").as_str();
        let prefix = captures.get(2).expect("route prefix capture").as_str();

        for suffix in LOADER_SUFFIXES {
            let loader_file = format!("{dir}{prefix}{suffix}");
            if !all_files.contains(&loader_file) {
                continue;
            }
            for hook in ctx.get_nodes_in_file(&loader_file) {
                if !hook_kinds.contains(&hook.kind)
                    || (hook.name != "load" && hook.name != "actions")
                {
                    continue;
                }
                edges.push(reference_edge(
                    &page.id,
                    &hook.id,
                    page.start_line,
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("sveltekit-load")),
                        ("via", Value::from(hook.name.as_str())),
                        (
                            "registeredAt",
                            Value::from(format!("{}:{}", loader_file, hook.start_line)),
                        ),
                    ]),
                ));
            }
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolution::types::ImportMapping;
    use crate::types::{Language, Node};

    struct Fixture {
        files: Vec<String>,
        nodes: Vec<Node>,
    }

    impl ResolutionContext for Fixture {
        fn get_nodes_in_file(&self, path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.file_path == path)
                .cloned()
                .collect()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.qualified_name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.kind == kind)
                .cloned()
                .collect()
        }
        fn file_exists(&self, path: &str) -> bool {
            self.files.iter().any(|file| file == path)
        }
        fn read_file(&self, _: &str) -> Option<String> {
            None
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.clone()
        }
        fn get_nodes_by_lower_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|node| node.name.to_lowercase() == name)
                .cloned()
                .collect()
        }
        fn get_import_mappings(&self, _: &str, _: Language) -> Vec<ImportMapping> {
            Vec::new()
        }
    }

    fn node(id: &str, kind: NodeKind, name: &str, file: &str, line: u32) -> Node {
        Node::new(
            id,
            kind,
            name,
            format!("{file}::{name}"),
            file,
            Language::Typescript,
            line,
            line + 5,
        )
    }

    #[test]
    fn pairs_pascal_forms_by_basename() {
        let fixture = Fixture {
            files: vec!["forms/About.pas".into(), "forms/About.dfm".into()],
            nodes: vec![
                node("unit", NodeKind::File, "About.pas", "forms/About.pas", 1),
                node("form", NodeKind::File, "About.dfm", "forms/About.dfm", 1),
            ],
        };
        let edges = pascal_form_edges(&fixture);
        assert_eq!(edges.len(), 1);
        assert_eq!(
            (edges[0].source.as_str(), edges[0].target.as_str()),
            ("unit", "form")
        );
        assert_eq!(edges[0].kind, EdgeKind::References);
    }

    #[test]
    fn links_sveltekit_component_to_load_and_actions() {
        let fixture = Fixture {
            files: vec![
                "src/routes/a/+page.svelte".into(),
                "src/routes/a/+page.server.ts".into(),
            ],
            nodes: vec![
                node(
                    "page",
                    NodeKind::Component,
                    "+page.svelte",
                    "src/routes/a/+page.svelte",
                    1,
                ),
                node(
                    "load",
                    NodeKind::Function,
                    "load",
                    "src/routes/a/+page.server.ts",
                    2,
                ),
                node(
                    "actions",
                    NodeKind::Constant,
                    "actions",
                    "src/routes/a/+page.server.ts",
                    8,
                ),
            ],
        };
        let edges = sveltekit_load_edges(&fixture);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|edge| edge.source == "page"));
        assert!(edges.iter().any(|edge| edge.target == "load"));
        assert!(edges.iter().any(|edge| edge.target == "actions"));
    }
}
