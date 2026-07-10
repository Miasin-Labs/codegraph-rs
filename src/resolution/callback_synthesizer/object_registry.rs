//! Object-literal registry dispatch synthesis.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{enclosing_fn, line_of};
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node, NodeKind};

const REGISTRY_MIN_ENTRIES: usize = 2;
const REGISTRY_FANOUT_CAP: usize = 40;

static REGISTRY_ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:(?:const|let|var)\s+([A-Za-z_$][0-9A-Za-z_$]*)|((?:this\.)?[A-Za-z_$][0-9A-Za-z_$]*))\s*=\s*\{",
    )
    .expect("valid regex")
});
static REGISTRY_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:(?-u:\b)new\s+)?((?:this\.)?[A-Za-z_$][0-9A-Za-z_$]*)\s*\[\s*([A-Za-z_$][0-9A-Za-z_$.]*)\s*\]\s*(?:\(|\.[A-Za-z_$])",
    )
    .expect("valid regex")
});
static REGISTRY_ENTRY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"^\s*(?:\[[^\]]+\]|["']?[0-9A-Za-z_$]+["']?)\s*:\s*([A-Za-z_$][0-9A-Za-z_$]*)\s*$"#,
    )
    .expect("valid regex")
});
static CHAINED_CALL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\]\s*\([^)]*\)\s*\.\s*([A-Za-z_$][0-9A-Za-z_$]*)").expect("valid regex")
});
static CHAINED_MEMBER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\]\s*\.\s*([A-Za-z_$][0-9A-Za-z_$]*)").expect("valid regex"));

#[derive(Debug)]
struct Dispatch {
    registry: String,
    line: u32,
    chained: Option<String>,
}

#[derive(Debug)]
struct Registry {
    names: Vec<String>,
    line: u32,
}

fn is_registry_source(file: &str) -> bool {
    [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"]
        .iter()
        .any(|extension| file.ends_with(extension))
}

fn comment_language(file: &str) -> CommentLang {
    if file.ends_with(".js")
        || file.ends_with(".jsx")
        || file.ends_with(".mjs")
        || file.ends_with(".cjs")
    {
        CommentLang::Javascript
    } else {
        CommentLang::Typescript
    }
}

fn normalize_registry(reference: &str) -> &str {
    reference.strip_prefix("this.").unwrap_or(reference)
}

/// Return the brace-balanced body after `open`, excluding the outer braces.
fn brace_body(source: &str, open: usize) -> Option<&str> {
    let mut depth = 0i32;
    for (offset, byte) in source.as_bytes()[open..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&source[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Extract depth-zero `key: Identifier` values from an object literal.
fn registry_entry_names(body: &str) -> Vec<String> {
    let mut segments = Vec::new();
    let mut depth = 0i32;
    let mut start = 0usize;
    for (index, byte) in body.bytes().enumerate() {
        match byte {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth -= 1,
            b',' if depth == 0 => {
                segments.push(&body[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    segments.push(&body[start..]);

    let mut names = Vec::new();
    for segment in segments {
        let Some(captures) = REGISTRY_ENTRY_RE.captures(segment) else {
            continue;
        };
        let name = captures.get(1).expect("registry value").as_str();
        if name.len() >= 3 && !names.iter().any(|known| known == name) {
            names.push(name.to_string());
        }
    }
    names
}

fn is_class_entry(name: &str) -> bool {
    matches!(
        name,
        "execute" | "run" | "handle" | "perform" | "process" | "call" | "apply" | "dispatch"
    )
}

fn resolve_registry_handler(
    ctx: &dyn ResolutionContext,
    name: &str,
    chained: Option<&str>,
) -> Option<Node> {
    let candidates = ctx.get_nodes_by_name(name);
    if let Some(function) = candidates
        .iter()
        .find(|candidate| candidate.kind == NodeKind::Function)
    {
        return Some(function.clone());
    }
    if let Some(class) = candidates
        .iter()
        .find(|candidate| matches!(candidate.kind, NodeKind::Class | NodeKind::Struct))
    {
        let methods: Vec<Node> = ctx
            .get_nodes_in_file(&class.file_path)
            .into_iter()
            .filter(|candidate| {
                candidate.kind == NodeKind::Method
                    && candidate.start_line >= class.start_line
                    && candidate.start_line <= class.end_line
            })
            .collect();
        if let Some(wanted) = chained.filter(|method| is_class_entry(method)) {
            if let Some(method) = methods.iter().find(|method| method.name == wanted) {
                return Some(method.clone());
            }
        }
        return methods
            .iter()
            .find(|method| is_class_entry(&method.name))
            .or_else(|| methods.iter().find(|method| method.name == "constructor"))
            .cloned()
            .or_else(|| Some(class.clone()));
    }
    candidates
        .into_iter()
        .find(|candidate| candidate.kind == NodeKind::Method)
}

/// Link a computed registry dispatch to every callable registered in its object literal.
pub(super) fn object_registry_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut edges = Vec::new();
    let mut seen = HashSet::new();

    for file in ctx.get_all_files() {
        if !is_registry_source(&file) {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if content.is_empty() || !content.contains('[') {
            continue;
        }
        let line_count = content.bytes().filter(|byte| *byte == b'\n').count() + 1;
        if content.len() / line_count > 200 {
            continue;
        }
        let safe = strip_comments_for_regex(&content, comment_language(&file));

        let mut dispatches = Vec::new();
        for captures in REGISTRY_DISPATCH_RE.captures_iter(&safe) {
            let matched = captures.get(0).expect("registry dispatch");
            let mut end = (matched.start() + 160).min(safe.len());
            while !safe.is_char_boundary(end) {
                end -= 1;
            }
            let window = &safe[matched.start()..end];
            let chained = CHAINED_CALL_RE
                .captures(window)
                .or_else(|| CHAINED_MEMBER_RE.captures(window))
                .and_then(|capture| capture.get(1))
                .map(|method| method.as_str().to_string());
            dispatches.push(Dispatch {
                registry: captures
                    .get(1)
                    .expect("registry reference")
                    .as_str()
                    .to_string(),
                line: line_of(&safe, matched.start()),
                chained,
            });
        }
        if dispatches.is_empty() {
            continue;
        }
        let referenced: HashSet<String> = dispatches
            .iter()
            .map(|dispatch| normalize_registry(&dispatch.registry).to_string())
            .collect();

        let mut registries: HashMap<String, Registry> = HashMap::new();
        for captures in REGISTRY_ASSIGN_RE.captures_iter(&safe) {
            let matched = captures.get(0).expect("registry assignment");
            let Some(lhs) = captures.get(1).or_else(|| captures.get(2)) else {
                continue;
            };
            let lhs = normalize_registry(lhs.as_str());
            if !referenced.contains(lhs) || registries.contains_key(lhs) {
                continue;
            }
            let Some(body) = brace_body(&safe, matched.end() - 1) else {
                continue;
            };
            let names = registry_entry_names(body);
            if names.len() >= REGISTRY_MIN_ENTRIES {
                registries.insert(
                    lhs.to_string(),
                    Registry {
                        names,
                        line: line_of(&safe, matched.start()),
                    },
                );
            }
        }
        if registries.is_empty() {
            continue;
        }

        let nodes_in_file = ctx.get_nodes_in_file(&file);
        for dispatch in dispatches {
            let Some(registry) = registries.get(normalize_registry(&dispatch.registry)) else {
                continue;
            };
            let Some(dispatcher) = enclosing_fn(&nodes_in_file, dispatch.line) else {
                continue;
            };
            let mut added = 0usize;
            for name in &registry.names {
                if added >= REGISTRY_FANOUT_CAP {
                    break;
                }
                let Some(target) = resolve_registry_handler(ctx, name, dispatch.chained.as_deref())
                else {
                    continue;
                };
                if target.id == dispatcher.id
                    || !seen.insert(format!("{}>{}", dispatcher.id, target.id))
                {
                    continue;
                }
                edges.push(synthesized_edge(
                    &dispatcher.id,
                    &target.id,
                    Some(dispatch.line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("object-registry")),
                        ("via", Value::from(name.as_str())),
                        (
                            "registeredAt",
                            Value::from(format!("{}:{}", file, registry.line)),
                        ),
                    ]),
                ));
                added += 1;
            }
        }
    }
    edges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolution::types::ImportMapping;
    use crate::types::Language;

    struct Fixture {
        source: String,
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
            path == "registry.ts"
        }
        fn read_file(&self, path: &str) -> Option<String> {
            (path == "registry.ts").then(|| self.source.clone())
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            vec!["registry.ts".into()]
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

    fn node(id: &str, kind: NodeKind, name: &str, start: u32, end: u32) -> Node {
        Node::new(
            id,
            kind,
            name,
            format!("registry.ts::{name}:{start}"),
            "registry.ts",
            Language::Typescript,
            start,
            end,
        )
    }

    #[test]
    fn extracts_only_top_level_identifier_entries() {
        assert_eq!(
            registry_entry_names("add: AddCommand, nested: { bad: Wrong }, remove: RemoveCommand"),
            vec!["AddCommand", "RemoveCommand"]
        );
        assert_eq!(
            registry_entry_names("[Cmd.ADD]: AddCommand, 'remove': RemoveCommand"),
            vec!["AddCommand", "RemoveCommand"]
        );
    }

    #[test]
    fn links_dispatcher_to_registered_class_entry_methods() {
        let fixture = Fixture {
            source: "const commands = { add: AddCommand, remove: RemoveCommand };\n\
                     function route(command) { new commands[command]().execute(); }\n\
                     class AddCommand { execute() {} }\n\
                     class RemoveCommand { execute() {} }"
                .into(),
            nodes: vec![
                node("route", NodeKind::Function, "route", 2, 2),
                node("add-class", NodeKind::Class, "AddCommand", 3, 3),
                node("add-exec", NodeKind::Method, "execute", 3, 3),
                node("remove-class", NodeKind::Class, "RemoveCommand", 4, 4),
                node("remove-exec", NodeKind::Method, "execute", 4, 4),
            ],
        };
        let edges = object_registry_edges(&fixture);
        assert_eq!(edges.len(), 2);
        assert!(edges.iter().all(|edge| edge.source == "route"));
        assert!(edges.iter().any(|edge| edge.target == "add-exec"));
        assert!(edges.iter().any(|edge| edge.target == "remove-exec"));
    }

    #[test]
    fn ignores_static_quoted_key_access() {
        assert!(!REGISTRY_DISPATCH_RE.is_match("commands['add']()"));
        assert!(REGISTRY_DISPATCH_RE.is_match("commands[action]()"));
    }
}
