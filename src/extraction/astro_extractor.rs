//! Astro component extraction via TypeScript delegation plus template scans.

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
    Severity,
    UnresolvedReference,
};

const ASTRO_BUILTIN_COMPONENTS: &[&str] = &["Fragment", "Code", "Debug"];

static SCRIPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<script(?:\s[^>]*)?>(?P<content>.*?)</script>").unwrap());
static COVERED_TAG_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<script(?:\s[^>]*)?>.*?</script>|<style(?:\s[^>]*)?>.*?</style>").unwrap()
});
static EXPR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{([^}/][^}]*)\}").unwrap());
static OPEN_EXPR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{([^}/][^}]*)$").unwrap());
static CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)([A-Za-z_$][0-9A-Za-z_$.]*)\s*\(").unwrap());
static COMPONENT_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Z][A-Za-z0-9_$]*)(?-u:\b)").unwrap());

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn count_newlines(value: &str) -> u32 {
    value.bytes().filter(|byte| *byte == b'\n').count() as u32
}

#[derive(Clone, Copy)]
struct ScriptBlock<'a> {
    content: &'a str,
    start_line: u32,
    end_line: u32,
    start_byte: u32,
}

pub struct AstroExtractor<'a> {
    file_path: String,
    source: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
}

impl<'a> AstroExtractor<'a> {
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
        let frontmatter = self.extract_frontmatter();
        if let Some(block) = frontmatter {
            self.process_script_content(block, &component.id, "frontmatter");
        }
        for block in self.extract_script_blocks() {
            self.process_script_content(block, &component.id, "script");
        }
        let covered = self.covered_ranges(frontmatter);
        self.extract_template_calls(&component.id, &covered);
        self.extract_template_components(&component.id, &covered);

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
        let name = file_name.strip_suffix(".astro").unwrap_or(file_name);
        let id = generate_node_id(&self.file_path, NodeKind::Component, name, 1);
        let mut node = Node::new(
            id,
            NodeKind::Component,
            name,
            format!("{}::{name}", self.file_path),
            self.file_path.clone(),
            Language::Astro,
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

    fn extract_frontmatter(&self) -> Option<ScriptBlock<'a>> {
        let mut offset = 0usize;
        let lines: Vec<&str> = self.source.split('\n').collect();
        let mut opening = None;
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                offset += line.len() + 1;
                continue;
            }
            if trimmed == "---" {
                opening = Some((
                    index,
                    offset + line.len() + usize::from(index + 1 < lines.len()),
                ));
            }
            break;
        }
        let (opening_line, content_start) = opening?;
        let mut cursor = content_start;
        for (index, line) in lines.iter().enumerate().skip(opening_line + 1) {
            if line.trim() == "---" {
                let content_end = cursor.saturating_sub(usize::from(index > opening_line + 1));
                return Some(ScriptBlock {
                    content: self.source.get(content_start..content_end)?,
                    start_line: (opening_line + 1) as u32,
                    end_line: index as u32,
                    start_byte: content_start as u32,
                });
            }
            cursor += line.len() + usize::from(index + 1 < lines.len());
        }
        None
    }

    fn extract_script_blocks(&self) -> Vec<ScriptBlock<'a>> {
        SCRIPT_RE
            .captures_iter(self.source)
            .filter_map(|captures| {
                let full = captures.get(0)?;
                let content = captures.name("content")?;
                let opening_end = full.as_str().find('>')? + 1;
                let start_line = count_newlines(&self.source[..full.start()])
                    + count_newlines(&full.as_str()[..opening_end]);
                Some(ScriptBlock {
                    content: content.as_str(),
                    start_line,
                    end_line: start_line + count_newlines(content.as_str()),
                    start_byte: content.start() as u32,
                })
            })
            .collect()
    }

    fn process_script_content(&mut self, block: ScriptBlock<'a>, component_id: &str, label: &str) {
        if !is_language_supported(Language::Typescript) {
            self.errors.push(ExtractionError {
                message: format!(
                    "Parser for typescript not available, cannot parse Astro {label} block"
                ),
                file_path: Some(self.file_path.clone()),
                line: None,
                column: None,
                severity: Severity::Warning,
                code: None,
            });
            return;
        }
        let result = TreeSitterExtractor::new(
            self.file_path.clone(),
            block.content,
            Some(Language::Typescript),
            languages::extractor_for(Language::Typescript),
        )
        .extract();

        for mut node in result.nodes {
            node.start_line += block.start_line;
            node.end_line += block.start_line;
            node.start_byte = node.start_byte.map(|byte| byte + block.start_byte);
            node.end_byte = node.end_byte.map(|byte| byte + block.start_byte);
            node.language = Language::Astro;
            let id = node.id.clone();
            self.nodes.push(node);
            self.edges
                .push(Edge::new(component_id, id, EdgeKind::Contains));
        }
        for mut edge in result.edges {
            if let Some(line) = edge.line.filter(|line| *line != 0) {
                edge.line = Some(line + block.start_line);
            }
            self.edges.push(edge);
        }
        for mut reference in result.unresolved_references {
            reference.line += block.start_line;
            reference.file_path = Some(self.file_path.clone());
            reference.language = Some(Language::Astro);
            self.unresolved_references.push(reference);
        }
        for mut error in result.errors {
            if let Some(line) = error.line.filter(|line| *line != 0) {
                error.line = Some(line + block.start_line);
            }
            self.errors.push(error);
        }
    }

    fn covered_ranges(&self, frontmatter: Option<ScriptBlock<'_>>) -> Vec<(u32, u32)> {
        let mut ranges = Vec::new();
        if let Some(block) = frontmatter {
            ranges.push((block.start_line.saturating_sub(1), block.end_line));
        }
        for matched in COVERED_TAG_RE.find_iter(self.source) {
            let start = count_newlines(&self.source[..matched.start()]);
            ranges.push((start, start + count_newlines(matched.as_str())));
        }
        ranges
    }

    fn is_covered(line: u32, ranges: &[(u32, u32)]) -> bool {
        ranges
            .iter()
            .any(|(start, end)| line >= *start && line <= *end)
    }

    fn extract_template_calls(&mut self, component_id: &str, ranges: &[(u32, u32)]) {
        for (line_index, line) in self.source.split('\n').enumerate() {
            let line_index = line_index as u32;
            if Self::is_covered(line_index, ranges) {
                continue;
            }
            let mut expressions: Vec<(&str, usize)> = EXPR_RE
                .captures_iter(line)
                .filter_map(|capture| {
                    let full = capture.get(0)?;
                    Some((capture.get(1)?.as_str(), full.start()))
                })
                .collect();
            let without_complete = EXPR_RE.replace_all(line, "");
            if let Some(open) = OPEN_EXPR_RE.captures(&without_complete) {
                if let Some(text) = open.get(1) {
                    expressions.push((text.as_str(), line.rfind('{').unwrap_or(0)));
                }
            }
            for (expression, offset) in expressions {
                for call in CALL_RE.captures_iter(expression) {
                    let Some(name) = call.get(1).map(|value| value.as_str()) else {
                        continue;
                    };
                    if matches!(name, "if" | "await" | "function") {
                        continue;
                    }
                    self.unresolved_references.push(UnresolvedReference {
                        from_node_id: component_id.to_string(),
                        reference_name: name.to_string(),
                        reference_kind: EdgeKind::Calls,
                        line: line_index + 1,
                        column: (offset + call.get(0).map_or(0, |m| m.start())) as u32,
                        file_path: Some(self.file_path.clone()),
                        language: Some(Language::Astro),
                        candidates: None,
                        metadata: None,
                    });
                }
            }
        }
    }

    fn extract_template_components(&mut self, component_id: &str, ranges: &[(u32, u32)]) {
        for (line_index, line) in self.source.split('\n').enumerate() {
            let line_index = line_index as u32;
            if Self::is_covered(line_index, ranges) {
                continue;
            }
            for capture in COMPONENT_TAG_RE.captures_iter(line) {
                let Some(name) = capture.get(1).map(|value| value.as_str()) else {
                    continue;
                };
                if ASTRO_BUILTIN_COMPONENTS.contains(&name) {
                    continue;
                }
                self.unresolved_references.push(UnresolvedReference {
                    from_node_id: component_id.to_string(),
                    reference_name: name.to_string(),
                    reference_kind: EdgeKind::References,
                    line: line_index + 1,
                    column: capture
                        .get(0)
                        .map_or(0, |matched| matched.start() as u32 + 1),
                    file_path: Some(self.file_path.clone()),
                    language: Some(Language::Astro),
                    candidates: None,
                    metadata: None,
                });
            }
        }
    }
}
