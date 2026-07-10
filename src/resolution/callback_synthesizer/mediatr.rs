//! C# MediatR request and notification dispatch synthesis.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

use super::edges::{edge_meta, synthesized_edge};
use super::source::{enclosing_fn, line_of};
use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::ResolutionContext;
use crate::types::{Edge, Node, NodeKind};

static MEDIATR_HANDLER_BASE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:IRequestHandler|INotificationHandler)\s*<\s*([A-Za-z_]\w*)")
        .expect("valid MediatR handler regex")
});
static MEDIATR_DISPATCH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"([A-Za-z_][\w.]*)\s*\.\s*(?:Send|Publish)\s*\(\s*(new\s+[A-Z]\w*|[A-Za-z_]\w*)")
        .expect("valid MediatR dispatch regex")
});
static MEDIATR_RECEIVER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:mediator|sender|publisher)").expect("valid MediatR receiver regex")
});
static MEDIATR_INLINE_TYPE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^new\s+([A-Z]\w*)").expect("valid MediatR inline type regex"));
static IDENTIFIER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*$").expect("valid identifier regex"));

const MEDIATR_FANOUT_CAP: usize = 80;
const MEDIATR_HANDLER_DECL_LOOKAHEAD: usize = 4;

fn resolve_mediatr_arg_type(
    argument: &str,
    lines: &[&str],
    method_start: u32,
    dispatch_line: u32,
) -> Option<String> {
    if let Some(captures) = MEDIATR_INLINE_TYPE_RE.captures(argument) {
        return captures.get(1).map(|capture| capture.as_str().to_string());
    }
    if !IDENTIFIER_RE.is_match(argument) {
        return None;
    }

    let escaped = regex::escape(argument);
    let assignment = Regex::new(&format!(r"\b{escaped}\b\s*=\s*new\s+([A-Z]\w*)")).ok()?;
    let declaration = Regex::new(&format!(r"\b([A-Z]\w*)\b\s+{escaped}\b")).ok()?;
    let mut declared_type = None;
    let start = method_start.saturating_sub(1) as usize;
    let end = (dispatch_line as usize).min(lines.len());
    for line in lines.iter().take(end).skip(start) {
        if let Some(captures) = assignment.captures(line) {
            return captures.get(1).map(|capture| capture.as_str().to_string());
        }
        if declared_type.is_none() {
            declared_type = declaration
                .captures(line)
                .and_then(|captures| captures.get(1))
                .map(|capture| capture.as_str().to_string());
        }
    }
    declared_type
}

/// Link mediator `Send`/`Publish` sites to matching handler `Handle` methods.
pub(super) fn mediatr_dispatch_edges(ctx: &dyn ResolutionContext) -> Vec<Edge> {
    let mut handlers: HashMap<String, Vec<Node>> = HashMap::new();

    for file in ctx.get_all_files() {
        if !file.ends_with(".cs") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains("IRequestHandler<") && !content.contains("INotificationHandler<") {
            continue;
        }

        let lines: Vec<&str> = content.split('\n').collect();
        let nodes = ctx.get_nodes_in_file(&file);
        for class in nodes.iter().filter(|node| node.kind == NodeKind::Class) {
            let start = class.start_line.saturating_sub(1) as usize;
            let end = (start + MEDIATR_HANDLER_DECL_LOOKAHEAD).min(lines.len());
            let declaration = lines[start..end].join("\n");
            let Some(request_type) = MEDIATR_HANDLER_BASE_RE
                .captures(&declaration)
                .and_then(|captures| captures.get(1))
                .map(|capture| capture.as_str().to_string())
            else {
                continue;
            };
            let Some(handle) = nodes.iter().find(|node| {
                node.kind == NodeKind::Method
                    && node.name == "Handle"
                    && node.start_line >= class.start_line
                    && node.start_line <= class.end_line
            }) else {
                continue;
            };
            handlers
                .entry(request_type)
                .or_default()
                .push(handle.clone());
        }
    }
    if handlers.is_empty() {
        return Vec::new();
    }

    let mut edges = Vec::new();
    let mut seen = HashSet::new();
    for file in ctx.get_all_files() {
        if !file.ends_with(".cs") {
            continue;
        }
        let Some(content) = ctx.read_file(&file) else {
            continue;
        };
        if !content.contains(".Send(") && !content.contains(".Publish(") {
            continue;
        }

        let safe = strip_comments_for_regex(&content, CommentLang::Csharp);
        let lines: Vec<&str> = safe.split('\n').collect();
        let nodes = ctx.get_nodes_in_file(&file);
        let mut added = 0usize;
        for captures in MEDIATR_DISPATCH_RE.captures_iter(&safe) {
            if added >= MEDIATR_FANOUT_CAP {
                break;
            }
            let Some(whole) = captures.get(0) else {
                continue;
            };
            let Some(receiver) = captures.get(1).map(|capture| capture.as_str()) else {
                continue;
            };
            if !MEDIATR_RECEIVER_RE.is_match(receiver) {
                continue;
            }
            let Some(argument) = captures.get(2).map(|capture| capture.as_str()) else {
                continue;
            };
            let line = line_of(&safe, whole.start());
            let Some(dispatcher) = enclosing_fn(&nodes, line) else {
                continue;
            };
            let Some(request_type) =
                resolve_mediatr_arg_type(argument, &lines, dispatcher.start_line, line)
            else {
                continue;
            };
            let Some(targets) = handlers.get(&request_type) else {
                continue;
            };
            for target in targets {
                if added >= MEDIATR_FANOUT_CAP {
                    break;
                }
                if target.id == dispatcher.id
                    || !seen.insert(format!("{}>{}", dispatcher.id, target.id))
                {
                    continue;
                }
                edges.push(synthesized_edge(
                    &dispatcher.id,
                    &target.id,
                    Some(line),
                    edge_meta(vec![
                        ("synthesizedBy", Value::from("mediatr-dispatch")),
                        ("via", Value::from(request_type.as_str())),
                        ("registeredAt", Value::from(format!("{file}:{line}"))),
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
    use crate::types::{Language, NodeKind};

    struct Fixture {
        files: HashMap<String, String>,
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
            self.files.contains_key(path)
        }
        fn read_file(&self, path: &str) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/project"
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.keys().cloned().collect()
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

    fn node(id: &str, kind: NodeKind, name: &str, file: &str, start: u32, end: u32) -> Node {
        Node::new(
            id,
            kind,
            name,
            format!("{file}::{name}"),
            file,
            Language::Csharp,
            start,
            end,
        )
    }

    #[test]
    fn resolves_inline_assignment_and_declared_argument_types() {
        let lines = [
            "void Run(CancelOrderCommand command) {",
            "  command = new ReplacementCommand();",
            "  _mediator.Send(command);",
        ];
        assert_eq!(
            resolve_mediatr_arg_type("new CancelOrderCommand", &lines, 1, 3),
            Some("CancelOrderCommand".into())
        );
        assert_eq!(
            resolve_mediatr_arg_type("command", &lines, 1, 3),
            Some("ReplacementCommand".into())
        );
    }

    #[test]
    fn links_mediator_send_to_handler() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "Handler.cs".into(),
                    "class CancelHandler : IRequestHandler<CancelOrderCommand, bool> {\n  Task<bool> Handle(CancelOrderCommand request) { }\n}\n"
                        .into(),
                ),
                (
                    "Controller.cs".into(),
                    "class Controller {\n  void Cancel() {\n    var command = new CancelOrderCommand();\n    _mediator.Send(command);\n  }\n}\n"
                        .into(),
                ),
            ]),
            nodes: vec![
                node("handler-class", NodeKind::Class, "CancelHandler", "Handler.cs", 1, 3),
                node("handle", NodeKind::Method, "Handle", "Handler.cs", 2, 2),
                node("cancel", NodeKind::Method, "Cancel", "Controller.cs", 2, 5),
            ],
        };

        let edges = mediatr_dispatch_edges(&fixture);
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].source, "cancel");
        assert_eq!(edges[0].target, "handle");
        assert_eq!(
            edges[0].metadata.as_ref().unwrap()["via"],
            "CancelOrderCommand"
        );
    }

    #[test]
    fn ignores_non_mediator_receiver() {
        let fixture = Fixture {
            files: HashMap::from([
                (
                    "Handler.cs".into(),
                    "class Handler : IRequestHandler<Message, bool> {\n  void Handle(Message m) {}\n}\n"
                        .into(),
                ),
                (
                    "Page.cs".into(),
                    "class Page {\n  void Run() { MessagingCenter.Send(new Message()); }\n}\n"
                        .into(),
                ),
            ]),
            nodes: vec![
                node("handler-class", NodeKind::Class, "Handler", "Handler.cs", 1, 3),
                node("handle", NodeKind::Method, "Handle", "Handler.cs", 2, 2),
                node("run", NodeKind::Method, "Run", "Page.cs", 2, 2),
            ],
        };
        assert!(mediatr_dispatch_edges(&fixture).is_empty());
    }
}
