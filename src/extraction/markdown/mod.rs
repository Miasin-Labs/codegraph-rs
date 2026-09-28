//! MarkdownExtractor — documents as graph nodes.
//!
//! A Markdown file yields its File node, one `Section` node per heading
//! (nested by level: `#` contains `##`, spanning up to the next heading of
//! its level or higher), the symbols its tagged code blocks declare
//! (`examples.rs`: doc examples, stamped `language: markdown` so resolution
//! and analysis never see them), and references to what it cites
//! (`refs.rs`): RFCs by number, other documents by link, Rust feature gates.
//! The document-link pass (`links.rs`) turns those into `references` edges
//! after every file is stored.
//!
//! A file named the rust-lang/rfcs way (`text/NNNN-slug.md`) is *RFC NNNN*:
//! a `Module` node named `RFC NNNN` spans the document and holds its
//! sections, and each feature its header declares (`- Feature Name: \`x\``)
//! is a `Constant` node `x` (qualified `…::feature(x)`) that `#![feature(x)]`
//! in code and `feature(x)` in other documents resolve to.
//!
//! Bounded per file ([`Budget`]): headings, parsed code blocks and their
//! bytes, example nodes, references. Every pass is linear in the file; a
//! reference finds its section by binary search over heading lines.

mod examples;
pub mod links;
pub(crate) mod refs;
mod scan;

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use self::examples::{FenceLanguage, Placement};
use self::scan::Heading;
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::resolution::line_index::LineStarts;
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

/// Per-file limits. A document past one keeps what fit and says so in a
/// warning; nothing grows with the product of two sizes.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub max_sections: usize,
    pub max_code_blocks: usize,
    pub max_block_bytes: usize,
    pub max_code_bytes: usize,
    pub max_example_nodes: usize,
    pub max_references: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_sections: 2_000,
            max_code_blocks: 256,
            max_block_bytes: 64 * 1024,
            max_code_bytes: 1024 * 1024,
            max_example_nodes: 4_000,
            max_references: 4_000,
        }
    }
}

/// How much prose a section keeps as its docstring (what search reads).
const SUMMARY_BYTES: usize = 480;
/// How long a heading may be as a node name.
const MAX_NAME_BYTES: usize = 160;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

fn clip(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

pub struct MarkdownExtractor<'a> {
    file_path: String,
    source: &'a str,
    budget: Budget,
}

/// A section being built: its heading, span and node.
struct SectionSpan {
    heading: Heading,
    end_line: u32,
    parent: Option<usize>,
    id: String,
    qualified: String,
}

impl<'a> MarkdownExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str) -> Self {
        Self {
            file_path: file_path.into(),
            source,
            budget: Budget::default(),
        }
    }

    pub fn with_budget(mut self, budget: Budget) -> Self {
        self.budget = budget;
        self
    }

    pub fn extract(self) -> ExtractionResult {
        let started = std::time::Instant::now();
        let mut result = ExtractionResult::default();
        let updated_at = now_ms();
        let source = self.source;
        let path = self.file_path.as_str();
        let scan = scan::scan(source, self.budget.max_sections);
        let starts = LineStarts::new(source);
        let lines: Vec<&str> = source.split('\n').collect();
        let last_line = scan.line_count.max(1);

        if scan.headings.len() >= self.budget.max_sections {
            self.warn(
                &mut result,
                format!(
                    "Markdown document has more than {} headings; later ones are text",
                    self.budget.max_sections
                ),
            );
        }

        // File node.
        let file_id = generate_node_id(path, NodeKind::File, path, 1);
        let file_name = path.rsplit('/').next().unwrap_or(path).to_string();
        let mut file = Node::new(
            file_id.clone(),
            NodeKind::File,
            file_name,
            path,
            path,
            Language::Markdown,
            1,
            last_line,
        );
        file.start_byte = Some(0);
        file.end_byte = Some(source.len() as u32);
        file.updated_at = updated_at;
        result.nodes.push(file);

        // The RFC this file is, if any: a Module spanning the document.
        let rfc = refs::rfc_of_file(path);
        let root_qualified = match rfc {
            Some((number, _)) => format!("{path}::RFC {number}"),
            None => path.to_string(),
        };
        let sections = self.sections(&scan.headings, last_line, &lines, &root_qualified);
        let root_id = match rfc {
            Some((number, slug)) => {
                let name = format!("RFC {number}");
                let id = generate_node_id(path, NodeKind::Module, &name, 1);
                let mut node = Node::new(
                    id.clone(),
                    NodeKind::Module,
                    name.clone(),
                    format!("{path}::{name}"),
                    path,
                    Language::Markdown,
                    1,
                    last_line,
                );
                node.signature = Some(format!("RFC {number}: {slug}"));
                // What the RFC is about: its first section's prose (the
                // header list above it is metadata).
                node.docstring = sections
                    .first()
                    .and_then(|first| {
                        first_paragraph(
                            &lines,
                            first.heading.line + 1,
                            first.end_line,
                            &scan.fences,
                        )
                    })
                    .map(|text| clip(&text, SUMMARY_BYTES).to_string());
                node.start_byte = Some(0);
                node.end_byte = Some(source.len() as u32);
                node.updated_at = updated_at;
                result.nodes.push(node);
                result
                    .edges
                    .push(Edge::new(file_id.clone(), id.clone(), EdgeKind::Contains));
                id
            }
            None => file_id.clone(),
        };

        // Sections, nested by level.
        for section in &sections {
            let parent_id = section
                .parent
                .map_or(root_id.as_str(), |p| sections[p].id.as_str());
            let name = clip(&section.heading.text, MAX_NAME_BYTES).to_string();
            let mut node = Node::new(
                section.id.clone(),
                NodeKind::Section,
                name,
                section.qualified.clone(),
                path,
                Language::Markdown,
                section.heading.line,
                section.end_line,
            );
            node.signature = lines
                .get(section.heading.line as usize - 1)
                .map(|line| clip(line.trim(), MAX_NAME_BYTES * 2).to_string());
            node.docstring = first_paragraph(
                &lines,
                section.heading.line + 1,
                section.end_line,
                &scan.fences,
            )
            .map(|text| clip(&text, SUMMARY_BYTES).to_string());
            node.start_byte = Some(section.heading.byte as u32);
            node.end_byte = Some(
                (starts
                    .line_start(section.end_line + 1)
                    .min(source.len() + 1))
                .saturating_sub(1)
                .max(section.heading.byte) as u32,
            );
            node.updated_at = updated_at;
            result.nodes.push(node);
            result.edges.push(Edge::new(
                parent_id.to_string(),
                section.id.clone(),
                EdgeKind::Contains,
            ));
        }
        let heading_lines: Vec<u32> = sections.iter().map(|s| s.heading.line).collect();
        // The innermost section holding `line` is the last heading at or
        // before it (a section runs until the next heading).
        let owner = |line: u32| -> (&str, &str) {
            match heading_lines.partition_point(|&start| start <= line) {
                0 => (root_id.as_str(), root_qualified.as_str()),
                at => (
                    sections[at - 1].id.as_str(),
                    sections[at - 1].qualified.as_str(),
                ),
            }
        };

        // Features an RFC's header declares.
        if rfc.is_some() {
            let header_end = scan
                .headings
                .first()
                .map_or(source.len(), |h| h.byte)
                .min(8 * 1024);
            for (feature, offset) in refs::declared_features(source, header_end) {
                let line = starts.line_of(offset);
                let qualified = format!("{root_qualified}::feature({feature})");
                let id = generate_node_id(path, NodeKind::Constant, &qualified, line);
                let mut node = Node::new(
                    id.clone(),
                    NodeKind::Constant,
                    feature.clone(),
                    qualified,
                    path,
                    Language::Markdown,
                    line,
                    line,
                );
                node.signature = Some(format!("#![feature({feature})]"));
                node.updated_at = updated_at;
                result.nodes.push(node);
                result
                    .edges
                    .push(Edge::new(root_id.clone(), id, EdgeKind::Contains));
            }
        }

        self.extract_examples(&scan.fences, &owner, &mut result);

        // References, one per (section, target): the first mention.
        let mut found = refs::find_references(source, path, rfc.map(|(n, _)| n));
        found.sort_by_key(|f| f.offset);
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut kept = 0usize;
        for reference in found {
            let line = starts.line_of(reference.offset);
            let (from, _) = owner(line);
            if !seen.insert((from.to_string(), reference.name.clone())) {
                continue;
            }
            if kept == self.budget.max_references {
                self.warn(
                    &mut result,
                    format!(
                        "Markdown document cites more than {} targets; the rest are not linked",
                        self.budget.max_references
                    ),
                );
                break;
            }
            kept += 1;
            let column = (reference.offset - starts.line_start(line).min(reference.offset)) as u32;
            result.unresolved_references.push(UnresolvedReference {
                from_node_id: from.to_string(),
                reference_name: reference.name,
                reference_kind: EdgeKind::References,
                line,
                column,
                file_path: Some(path.to_string()),
                language: Some(Language::Markdown),
                candidates: None,
                metadata: None,
            });
        }

        result.duration_ms = started.elapsed().as_secs_f64() * 1000.0;
        result
    }

    fn warn(&self, result: &mut ExtractionResult, message: String) {
        result.errors.push(ExtractionError {
            message,
            file_path: Some(self.file_path.clone()),
            line: None,
            column: None,
            severity: Severity::Warning,
            code: Some("markdown_budget".to_string()),
        });
    }

    /// Spans, parents, ids and qualified names of the headings.
    fn sections(
        &self,
        headings: &[Heading],
        last_line: u32,
        lines: &[&str],
        root_qualified: &str,
    ) -> Vec<SectionSpan> {
        let path = self.file_path.as_str();
        let mut sections: Vec<SectionSpan> = Vec::with_capacity(headings.len());
        // Indices of the sections still open, outermost first.
        let mut open: Vec<usize> = Vec::new();
        let mut ids: HashSet<String> = HashSet::new();
        for heading in headings {
            while let Some(&top) = open.last() {
                if sections[top].heading.level >= heading.level {
                    sections[top].end_line = heading
                        .line
                        .saturating_sub(1)
                        .max(sections[top].heading.line);
                    open.pop();
                } else {
                    break;
                }
            }
            let parent = open.last().copied();
            let parent_qualified =
                parent.map_or(root_qualified, |p| sections[p].qualified.as_str());
            let name = clip(&heading.text, MAX_NAME_BYTES);
            let qualified = format!("{parent_qualified}::{name}");
            let mut id = generate_node_id(path, NodeKind::Section, name, heading.line);
            if !ids.insert(id.clone()) {
                id = generate_node_id(path, NodeKind::Section, &qualified, heading.line);
                ids.insert(id.clone());
            }
            open.push(sections.len());
            sections.push(SectionSpan {
                heading: heading.clone(),
                end_line: last_line,
                parent,
                id,
                qualified,
            });
        }
        // Trailing blank lines are not part of a section.
        for section in &mut sections {
            while section.end_line > section.heading.line
                && lines
                    .get(section.end_line as usize - 1)
                    .is_none_or(|line| line.trim().is_empty())
            {
                section.end_line -= 1;
            }
        }
        sections
    }

    fn extract_examples<'s>(
        &self,
        fences: &[scan::Fence],
        owner: &dyn Fn(u32) -> (&'s str, &'s str),
        result: &mut ExtractionResult,
    ) {
        let mut taken: HashSet<String> = result.nodes.iter().map(|n| n.id.clone()).collect();
        let mut blocks = 0usize;
        let mut code_bytes = 0usize;
        let mut example_nodes = 0usize;
        let mut exhausted = false;
        for fence in fences {
            let content = &self.source[fence.content.clone()];
            if content.trim().is_empty() || content.len() > self.budget.max_block_bytes {
                continue;
            }
            let language = match examples::fence_language(&fence.info) {
                FenceLanguage::Tagged(language) => language,
                FenceLanguage::Untagged => Language::Rust,
                FenceLanguage::Other => continue,
            };
            if blocks == self.budget.max_code_blocks
                || code_bytes + content.len() > self.budget.max_code_bytes
                || example_nodes >= self.budget.max_example_nodes
            {
                exhausted = true;
                break;
            }
            let code = examples::dedented(content, fence.indent);
            if matches!(
                examples::fence_language(&fence.info),
                FenceLanguage::Untagged
            ) && !examples::untagged_is_rust(&code)
            {
                continue;
            }
            blocks += 1;
            code_bytes += content.len();
            let (parent_id, parent_qualified) = owner(fence.open_line);
            let placement = Placement {
                file_path: &self.file_path,
                parent_id,
                parent_qualified,
            };
            let (nodes, edges) = examples::extract_block(
                fence,
                &code,
                language,
                &placement,
                self.budget.max_example_nodes - example_nodes,
                &mut taken,
            );
            example_nodes += nodes.len();
            result.nodes.extend(nodes);
            result.edges.extend(edges);
        }
        if exhausted {
            self.warn(
                result,
                format!(
                    "Markdown document's code blocks exceed the budget ({} blocks, {} bytes, {} symbols); later blocks are not parsed",
                    self.budget.max_code_blocks,
                    self.budget.max_code_bytes,
                    self.budget.max_example_nodes
                ),
            );
        }
    }
}

/// The first paragraph of prose in lines `from..=to` (1-based): skips
/// blank lines, code fences, tables, HTML and headings; joins wrapped lines.
fn first_paragraph(lines: &[&str], from: u32, to: u32, fences: &[scan::Fence]) -> Option<String> {
    let in_fence = |line: u32| {
        let at = fences.partition_point(|f| f.close_line < line);
        fences
            .get(at)
            .is_some_and(|f| f.open_line <= line && line <= f.close_line)
    };
    let mut text = String::new();
    let mut line = from.max(1);
    while line <= to {
        let raw = lines.get(line as usize - 1).map_or("", |l| l.trim());
        let skip = raw.is_empty()
            || in_fence(line)
            || raw.starts_with('#')
            || raw.starts_with('|')
            || raw.starts_with('<')
            || raw.starts_with("---")
            || (raw.starts_with('[') && raw.contains("]:"));
        if skip {
            if !text.is_empty() {
                break;
            }
        } else {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(raw.trim_start_matches(['>', ' ']));
            if text.len() > SUMMARY_BYTES {
                break;
            }
        }
        line += 1;
    }
    (!text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests;
