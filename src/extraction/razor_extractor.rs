//! ASP.NET Razor and Blazor markup extraction.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::extraction::grammars::is_language_supported;
use crate::extraction::languages;
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
use crate::types::{
    Edge,
    EdgeKind,
    ExtractionError,
    ExtractionResult,
    Language,
    Node,
    NodeKind,
    UnresolvedReference,
};

const BLAZOR_BUILTIN_COMPONENTS: &[&str] = &[
    "Router",
    "Found",
    "NotFound",
    "RouteView",
    "AuthorizeRouteView",
    "LayoutView",
    "CascadingValue",
    "CascadingAuthenticationState",
    "AuthorizeView",
    "Authorized",
    "NotAuthorized",
    "Authorizing",
    "EditForm",
    "DataAnnotationsValidator",
    "ValidationSummary",
    "ValidationMessage",
    "InputText",
    "InputNumber",
    "InputCheckbox",
    "InputSelect",
    "InputDate",
    "InputTextArea",
    "InputRadio",
    "InputRadioGroup",
    "InputFile",
    "PageTitle",
    "HeadContent",
    "HeadOutlet",
    "Virtualize",
    "DynamicComponent",
    "ErrorBoundary",
    "SectionContent",
    "SectionOutlet",
    "FocusOnNavigate",
    "NavLink",
    "Microsoft",
];

static DIRECTIVE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*@(?:model|inherits)\s+([A-Za-z_][\w.]*(?:\s*<[^>]+>)?)").unwrap()
});
static INJECT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s*@inject\s+([A-Za-z_][\w.]*(?:\s*<[^>]+>)?)\s+[A-Za-z_]").unwrap()
});
static TYPEOF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@typeof\(\s*([A-Za-z_][\w.]*)\s*\)").unwrap());
static TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Z][A-Za-z0-9_]*)\b([^>]*)>").unwrap());
static TYPE_ARG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bT[A-Za-z]*\s*=\s*\"([A-Za-z_][\w.]*)\""#).unwrap());
static CODE_BLOCK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"@(?:code|functions)\b\s*\{|@\{").unwrap());
static TYPE_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z_][A-Za-z0-9_.]*").unwrap());

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

struct CodeBlock<'a> {
    content: &'a str,
    line_offset: u32,
}

pub struct RazorExtractor<'a> {
    file_path: String,
    source: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
}

impl<'a> RazorExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str) -> Self {
        Self {
            file_path: file_path.into(),
            source,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
        }
    }

    pub fn extract(mut self) -> ExtractionResult {
        let start = std::time::Instant::now();
        let component = self.create_component_node();
        self.extract_directives(&component.id);
        if self.file_path.to_ascii_lowercase().ends_with(".razor") {
            self.extract_component_tags(&component.id);
        }
        self.process_code_blocks(&component.id);
        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: start.elapsed().as_millis() as f64,
        }
    }

    fn create_component_node(&mut self) -> Node {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let file_name = self
            .file_path
            .rsplit(['/', '\\'])
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.file_path);
        let lower = file_name.to_ascii_lowercase();
        let suffix_len = if lower.ends_with(".razor") {
            ".razor".len()
        } else if lower.ends_with(".cshtml") {
            ".cshtml".len()
        } else {
            0
        };
        let name = &file_name[..file_name.len() - suffix_len];
        let id = generate_node_id(&self.file_path, NodeKind::Component, name, 1);
        let mut node = Node::new(
            id,
            NodeKind::Component,
            name,
            format!("{}::{name}", self.file_path),
            self.file_path.clone(),
            Language::Razor,
            1,
            lines.len().max(1) as u32,
        );
        node.end_column = lines.last().map_or(0, |line| line.len() as u32);
        node.start_byte = Some(0);
        node.end_byte = Some(self.source.len() as u32);
        node.is_exported = Some(true);
        node.updated_at = now_ms();
        self.nodes.push(node.clone());
        node
    }

    fn last_segment(value: &str) -> &str {
        value.rsplit('.').next().unwrap_or(value)
    }

    fn type_names(value: &str) -> Vec<&str> {
        TYPE_TOKEN_RE
            .find_iter(value)
            .map(|matched| Self::last_segment(matched.as_str()))
            .filter(|name| name.chars().next().is_some_and(char::is_uppercase))
            .collect()
    }

    fn push_ref(&mut self, component_id: &str, name: &str, line: u32, column: u32) {
        self.unresolved_references.push(UnresolvedReference {
            from_node_id: component_id.to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::References,
            line,
            column,
            file_path: Some(self.file_path.clone()),
            language: Some(Language::Razor),
            candidates: None,
            metadata: None,
        });
    }

    fn extract_directives(&mut self, component_id: &str) {
        for (index, line) in self.source.split('\n').enumerate() {
            for capture in [DIRECTIVE_RE.captures(line), INJECT_RE.captures(line)]
                .into_iter()
                .flatten()
            {
                if let Some(value) = capture.get(1) {
                    for name in Self::type_names(value.as_str()) {
                        self.push_ref(component_id, name, index as u32 + 1, 0);
                    }
                }
            }
            for capture in TYPEOF_RE.captures_iter(line) {
                let Some(value) = capture.get(1) else {
                    continue;
                };
                let name = Self::last_segment(value.as_str());
                if name.chars().next().is_some_and(char::is_uppercase) {
                    self.push_ref(
                        component_id,
                        name,
                        index as u32 + 1,
                        capture.get(0).map_or(0, |matched| matched.start() as u32),
                    );
                }
            }
        }
    }

    fn extract_component_tags(&mut self, component_id: &str) {
        for (index, line) in self.source.split('\n').enumerate() {
            for capture in TAG_RE.captures_iter(line) {
                let Some(name) = capture.get(1).map(|matched| matched.as_str()) else {
                    continue;
                };
                if BLAZOR_BUILTIN_COMPONENTS.contains(&name) {
                    continue;
                }
                self.push_ref(
                    component_id,
                    name,
                    index as u32 + 1,
                    capture
                        .get(0)
                        .map_or(0, |matched| matched.start() as u32 + 1),
                );
                if let Some(attributes) = capture.get(2) {
                    for type_arg in TYPE_ARG_RE.captures_iter(attributes.as_str()) {
                        let Some(value) = type_arg.get(1) else {
                            continue;
                        };
                        let type_name = Self::last_segment(value.as_str());
                        if type_name.chars().next().is_some_and(char::is_uppercase) {
                            self.push_ref(component_id, type_name, index as u32 + 1, 0);
                        }
                    }
                }
            }
        }
    }

    fn matching_brace(&self, opening: usize) -> Option<usize> {
        let bytes = self.source.as_bytes();
        let mut depth = 0usize;
        let mut index = opening;
        while index < bytes.len() {
            match bytes[index] {
                quote @ (b'\'' | b'"') => {
                    index += 1;
                    while index < bytes.len() && bytes[index] != quote {
                        index += usize::from(bytes[index] == b'\\');
                        index += 1;
                    }
                }
                b'/' if bytes.get(index + 1) == Some(&b'/') => {
                    index += 2;
                    while index < bytes.len() && bytes[index] != b'\n' {
                        index += 1;
                    }
                }
                b'/' if bytes.get(index + 1) == Some(&b'*') => {
                    index += 2;
                    while index + 1 < bytes.len()
                        && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                    {
                        index += 1;
                    }
                    index += usize::from(index < bytes.len());
                }
                b'{' => depth += 1,
                b'}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return Some(index);
                    }
                }
                _ => {}
            }
            index += 1;
        }
        None
    }

    fn code_blocks(&self) -> Vec<CodeBlock<'a>> {
        let mut blocks = Vec::new();
        let mut search_from = 0usize;
        while let Some(matched) = CODE_BLOCK_RE.find_at(self.source, search_from) {
            let Some(relative_open) = self.source[matched.start()..].find('{') else {
                break;
            };
            let opening = matched.start() + relative_open;
            let Some(closing) = self.matching_brace(opening) else {
                search_from = matched.end();
                continue;
            };
            blocks.push(CodeBlock {
                content: &self.source[opening + 1..closing],
                line_offset: self.source[..opening + 1]
                    .bytes()
                    .filter(|byte| *byte == b'\n')
                    .count() as u32,
            });
            search_from = closing + 1;
        }
        blocks
    }

    fn process_code_blocks(&mut self, component_id: &str) {
        if !is_language_supported(Language::Csharp) {
            return;
        }
        for block in self.code_blocks() {
            if block.content.trim().is_empty() {
                continue;
            }
            let wrapped = format!("class __RazorCode__ {{\n{}\n}}", block.content);
            let result = TreeSitterExtractor::new(
                self.file_path.clone(),
                &wrapped,
                Some(Language::Csharp),
                languages::extractor_for(Language::Csharp),
            )
            .extract();
            for mut reference in result.unresolved_references {
                reference.from_node_id = component_id.to_string();
                reference.line = reference
                    .line
                    .saturating_add(block.line_offset)
                    .saturating_sub(1);
                reference.file_path = Some(self.file_path.clone());
                reference.language = Some(Language::Razor);
                self.unresolved_references.push(reference);
            }
        }
    }
}
