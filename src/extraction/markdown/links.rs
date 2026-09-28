//! The document-link pass: turns the `rfc:`, `doc:` and `feature:`
//! references documents (and Rust `#![feature]` attributes) record into
//! `references` edges, after every changed file is stored and before
//! general resolution (which never sees Markdown nodes, so it could not
//! resolve these, and must not try).
//!
//! - `rfc:N` → the `RFC N` document node(s).
//! - `feature:x` → the `x` feature node(s) RFC headers declare.
//! - `doc:path` → that document's File node; `#L12` → the innermost section
//!   holding line 12; `#some-heading` → the section with that anchor.
//!
//! A document never links to itself. Every edge records the reference
//! name, so re-indexing the target restores the reference (see
//! `orchestrator/reconcile.rs`) and the next pass links it again. Lookups
//! are indexed and memoized per pass; the pass is bounded by
//! [`MAX_REFERENCES`] rows per prefix and [`DEADLINE`].

use std::collections::HashMap;
use std::time::{Duration, Instant};

use super::refs::{DOC_PREFIX, FEATURE_PREFIX, RFC_PREFIX, decode_document_link};
use super::scan::heading_slug;
use crate::db::{QueryBuilder, ResolvedRefKey};
use crate::error::Result;
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::types::{Edge, EdgeKind, Language, Metadata, Node, NodeKind, UnresolvedReference};

/// Most references read per prefix in one pass.
const MAX_REFERENCES: usize = 500_000;
/// Wall-clock bound on one pass; what is left waits for the next.
const DEADLINE: Duration = Duration::from_secs(20);
/// Most targets one reference links to (a feature several RFCs declare).
const MAX_TARGETS: usize = 8;

/// Which references a pass examines.
#[derive(Debug, Clone, Copy)]
pub enum DocLinkScope<'a> {
    /// Every document reference (full index, or a document changed).
    All,
    /// Only references recorded in these files (a code-only sync: no
    /// document target changed, so only new references can link).
    Files(&'a [String]),
}

impl<'a> DocLinkScope<'a> {
    /// The scope for a sync that changed `paths`: everything when any of
    /// them is a document (a target may have appeared), else those files.
    pub fn for_changed(paths: &'a [String]) -> Self {
        let document = paths.iter().any(|path| {
            let lower = path.to_ascii_lowercase();
            lower.ends_with(".md") || lower.ends_with(".markdown") || lower.ends_with(".mdown")
        });
        if document {
            DocLinkScope::All
        } else {
            DocLinkScope::Files(paths)
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DocLinkStats {
    pub examined: usize,
    pub resolved: usize,
    pub edges: usize,
}

/// A target: its id and file.
#[derive(Clone)]
struct Target {
    id: String,
    file_path: String,
}

/// Sections of one document, sorted by start line.
struct DocumentSections {
    file: Option<Target>,
    /// Loaded the first time an anchor into the document is asked for.
    sections: Option<Vec<Node>>,
}

struct Pass<'q> {
    queries: &'q QueryBuilder,
    targets: HashMap<String, Vec<Target>>,
    documents: HashMap<String, DocumentSections>,
}

fn is_document_reference(name: &str) -> bool {
    name.starts_with(RFC_PREFIX) || name.starts_with(DOC_PREFIX) || name.starts_with(FEATURE_PREFIX)
}

/// Link the document references in `scope`. Never fails the index: a
/// query error ends the pass early and is returned.
pub fn resolve_document_links(
    queries: &QueryBuilder,
    scope: DocLinkScope<'_>,
) -> Result<DocLinkStats> {
    let started = Instant::now();
    let refs: Vec<UnresolvedReference> = match scope {
        DocLinkScope::All => {
            let mut refs = Vec::new();
            for prefix in [RFC_PREFIX, DOC_PREFIX, FEATURE_PREFIX] {
                refs.extend(queries.get_unresolved_references_with_prefix(prefix, MAX_REFERENCES)?);
            }
            refs
        }
        DocLinkScope::Files(paths) => queries
            .get_unresolved_references_by_files(paths)?
            .into_iter()
            .filter(|r| is_document_reference(&r.reference_name))
            .collect(),
    };
    let mut pass = Pass {
        queries,
        targets: HashMap::new(),
        documents: HashMap::new(),
    };
    let mut stats = DocLinkStats::default();
    let mut edges = Vec::new();
    let mut keys = Vec::new();
    for reference in &refs {
        if started.elapsed() > DEADLINE {
            break;
        }
        stats.examined += 1;
        let from_file = reference.file_path.as_deref().unwrap_or("");
        let targets: Vec<Target> = pass
            .targets_of(&reference.reference_name)?
            .into_iter()
            .filter(|target| target.file_path != from_file)
            .collect();
        if targets.is_empty() {
            continue;
        }
        stats.resolved += 1;
        for target in targets {
            let mut metadata = Metadata::new();
            metadata.insert(
                "referenceName".to_string(),
                serde_json::Value::String(reference.reference_name.clone()),
            );
            metadata.insert(
                "resolvedBy".to_string(),
                serde_json::Value::String("document-link".to_string()),
            );
            let mut edge = Edge::new(
                reference.from_node_id.clone(),
                target.id,
                EdgeKind::References,
            );
            edge.line = Some(reference.line);
            edge.column = Some(reference.column);
            edge.metadata = Some(metadata);
            edges.push(edge);
        }
        keys.push(ResolvedRefKey {
            from_node_id: reference.from_node_id.clone(),
            reference_name: reference.reference_name.clone(),
            reference_kind: reference.reference_kind.as_str().to_string(),
            line: reference.line,
            column: reference.column,
        });
    }
    stats.edges = edges.len();
    queries.insert_edges(&edges)?;
    queries.delete_specific_resolved_references(&keys)?;
    Ok(stats)
}

impl Pass<'_> {
    fn targets_of(&mut self, name: &str) -> Result<Vec<Target>> {
        if let Some(found) = self.targets.get(name) {
            return Ok(found.clone());
        }
        let found = if let Some(number) = name.strip_prefix(RFC_PREFIX) {
            self.named(&format!("RFC {number}"), |node| {
                node.kind == NodeKind::Module
            })?
        } else if let Some(feature) = name.strip_prefix(FEATURE_PREFIX) {
            let suffix = format!("::feature({feature})");
            self.named(feature, |node| {
                node.kind == NodeKind::Constant && node.qualified_name.ends_with(&suffix)
            })?
        } else if let Some((path, anchor)) = decode_document_link(name) {
            self.document(&path, anchor.as_deref())?
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        self.targets.insert(name.to_string(), found.clone());
        Ok(found)
    }

    /// Markdown nodes named `name` that `keep` accepts.
    fn named(&self, name: &str, keep: impl Fn(&Node) -> bool) -> Result<Vec<Target>> {
        let mut nodes: Vec<Node> = self
            .queries
            .get_nodes_by_name(name)?
            .into_iter()
            .filter(|node| node.language == Language::Markdown && keep(node))
            .collect();
        nodes.sort_by(|a, b| a.file_path.cmp(&b.file_path));
        Ok(nodes
            .into_iter()
            .take(MAX_TARGETS)
            .map(|node| Target {
                id: node.id,
                file_path: node.file_path,
            })
            .collect())
    }

    /// The document at `path`, or its section `anchor` names.
    fn document(&mut self, path: &str, anchor: Option<&str>) -> Result<Option<Target>> {
        if !self.documents.contains_key(path) {
            let file_id = generate_node_id(path, NodeKind::File, path, 1);
            let file = self.queries.get_node_by_id(&file_id)?.map(|node| Target {
                id: node.id,
                file_path: node.file_path,
            });
            self.documents.insert(
                path.to_string(),
                DocumentSections {
                    file,
                    sections: None,
                },
            );
        }
        let queries = self.queries;
        let document = self.documents.get_mut(path).expect("inserted above");
        let Some(file) = document.file.clone() else {
            return Ok(None);
        };
        let Some(anchor) = anchor else {
            return Ok(Some(file));
        };
        if document.sections.is_none() {
            let mut sections: Vec<Node> = queries
                .get_nodes_by_file(path)?
                .into_iter()
                .filter(|node| node.kind == NodeKind::Section)
                .collect();
            sections.sort_by_key(|node| node.start_line);
            document.sections = Some(sections);
        }
        let sections = document.sections.as_deref().unwrap_or_default();
        Ok(Some(section_for_anchor(sections, anchor).map_or(
            file,
            |node| Target {
                id: node.id.clone(),
                file_path: node.file_path.clone(),
            },
        )))
    }
}

/// The section an anchor names: `L12`/`L12-L20` → the innermost section
/// holding line 12; else the section whose GitHub slug is the anchor.
fn section_for_anchor<'n>(sections: &'n [Node], anchor: &str) -> Option<&'n Node> {
    let line = anchor
        .strip_prefix('L')
        .and_then(|rest| rest.split('-').next())
        .and_then(|digits| digits.parse::<u32>().ok());
    if let Some(line) = line {
        // Sections are sorted by start; the innermost holding `line` is the
        // last one starting at or before it that still spans it.
        let upto = sections.partition_point(|node| node.start_line <= line);
        return sections[..upto]
            .iter()
            .rev()
            .find(|node| node.end_line >= line);
    }
    let anchor = anchor.to_lowercase();
    sections
        .iter()
        .find(|node| heading_slug(&node.name) == anchor)
}
