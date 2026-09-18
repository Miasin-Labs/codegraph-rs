//! The resolving half of a pass: workers that resolve slices of
//! references with their own graph handles, and the store that writes what
//! they found (edges in, resolved references out) in one transaction.

use std::collections::{BTreeMap, HashSet};
use std::time::Instant;

use super::context::ExternalContext;
use super::declarations::Declarations;
use super::graphs::Reach;
use super::names::MethodNames;
use super::open::{GraphCache, OpenStats};
use super::pass::ExternalReport;
use super::rust::{self, Attempt, ExternalResolvedBy, Resolved};
use crate::db::{ExternalEdge, ExternalGraphKind, QueryBuilder, ResolvedRefKey};
use crate::error::Result;
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{EdgeKind, Language, NodeKind, UnresolvedReference};

/// Resolve a slice of references, however many threads it takes.
pub(crate) type Resolver<'a> =
    dyn Fn(&Reach, &MethodNames, &[UnresolvedRef], usize, Instant) -> Vec<Outcome> + 'a;

/// What one worker resolved.
pub(crate) struct Outcome {
    resolved: Vec<Resolution>,
    misses: BTreeMap<String, usize>,
    examined: usize,
    complete: bool,
    opens: OpenStats,
}

impl Outcome {
    /// A worker that died: nothing it resolved is kept, and the pass is
    /// not complete (the references it held are examined again).
    pub(super) fn lost() -> Outcome {
        Outcome {
            resolved: Vec::new(),
            misses: BTreeMap::new(),
            examined: 0,
            complete: false,
            opens: OpenStats::default(),
        }
    }
}

/// One reference resolved into another graph, as plain data.
struct Resolution {
    edge: ExternalEdge,
    key: ResolvedRefKey,
    by: ExternalResolvedBy,
}

/// Resolve `refs` on this thread, with its own graph handles.
pub(super) fn resolve_refs(
    project: &dyn ResolutionContext,
    reach: &Reach,
    names: &MethodNames,
    refs: &[UnresolvedRef],
    max_open: usize,
    deadline: Instant,
) -> Outcome {
    let cache = GraphCache::new(reach, max_open);
    let declarations = Declarations::new(&cache);
    let context = ExternalContext {
        project,
        declarations: &declarations,
    };
    let mut outcome = Outcome {
        resolved: Vec::new(),
        misses: BTreeMap::new(),
        examined: 0,
        complete: true,
        opens: OpenStats::default(),
    };
    for reference in refs {
        if Instant::now() >= deadline {
            outcome.complete = false;
            break;
        }
        outcome.examined += 1;
        match rust::resolve(reference, &context, names) {
            Attempt::Resolved(resolved) => {
                outcome.resolved.push(Resolution::of(reference, *resolved));
            }
            Attempt::Missed(what) => *outcome.misses.entry(what).or_default() += 1,
            Attempt::NotOurs => {}
        }
    }
    outcome.opens = cache.stats();
    outcome
}

impl Resolution {
    fn of(reference: &UnresolvedRef, resolved: Resolved) -> Resolution {
        let Resolved { found, by } = resolved;
        let node = found.node;
        let kind = match (reference.reference_kind, node.kind) {
            (EdgeKind::Calls, NodeKind::Struct) => EdgeKind::Instantiates,
            (kind, _) => kind,
        };
        Resolution {
            key: ResolvedRefKey {
                from_node_id: reference.from_node_id.clone(),
                reference_name: reference.reference_name.clone(),
                reference_kind: reference.reference_kind.as_str().to_string(),
                line: reference.line,
                column: reference.column,
            },
            edge: ExternalEdge {
                source: reference.from_node_id.clone(),
                kind,
                target_graph_kind: found.graph.kind,
                target_graph_key: found.graph.key.clone(),
                target_node_id: node.id,
                target_name: node.name,
                target_qualified_name: node.qualified_name,
                target_kind: node.kind,
                target_file_path: node.file_path,
                target_line: Some(node.start_line),
                reference_name: reference.reference_name.clone(),
                line: Some(reference.line),
                column: Some(reference.column),
                confidence: found.confidence,
                resolved_by: by.as_str().to_string(),
                metadata: reference.metadata.clone(),
            },
            by,
        }
    }
}

/// A stored unresolved row as the resolvers take it.
pub(super) fn working_ref(
    row: UnresolvedReference,
    project: &dyn ResolutionContext,
) -> Option<UnresolvedRef> {
    let file_path = match row.file_path {
        Some(path) if !path.is_empty() => path,
        _ => project.get_node_by_id(&row.from_node_id)?.file_path,
    };
    Some(UnresolvedRef {
        from_node_id: row.from_node_id,
        reference_name: row.reference_name,
        reference_kind: row.reference_kind,
        line: row.line,
        column: row.column,
        file_path,
        language: row.language.unwrap_or(Language::Rust),
        candidates: row.candidates,
        metadata: row.metadata,
    })
}

/// Edges waiting to be stored, and the references they resolve.
#[derive(Default)]
pub(super) struct Store {
    edges: Vec<ExternalEdge>,
    keys: Vec<ResolvedRefKey>,
    seen: HashSet<(String, String, u32, u32)>,
}

impl Store {
    pub(super) fn take(
        &mut self,
        outcomes: Vec<Outcome>,
        report: &mut ExternalReport,
        misses: &mut BTreeMap<String, usize>,
    ) {
        for outcome in outcomes {
            for (what, count) in outcome.misses {
                *misses.entry(what).or_default() += count;
            }
            report.examined += outcome.examined;
            report.complete &= outcome.complete;
            report.opens.add(outcome.opens);
            for resolution in outcome.resolved {
                let key = &resolution.key;
                let identity = (
                    key.from_node_id.clone(),
                    key.reference_name.clone(),
                    key.line,
                    key.column,
                );
                if !self.seen.insert(identity) {
                    continue;
                }
                match resolution.edge.target_graph_kind {
                    ExternalGraphKind::Dependency => report.into_dependencies += 1,
                    ExternalGraphKind::Project => report.into_projects += 1,
                }
                *report
                    .by
                    .entry(resolution.by.as_str().to_string())
                    .or_default() += 1;
                self.edges.push(resolution.edge);
                self.keys.push(resolution.key);
            }
        }
    }

    pub(super) fn flush(&mut self, queries: &QueryBuilder) -> Result<()> {
        if self.edges.is_empty() {
            return Ok(());
        }
        queries.db().transaction(|| {
            queries.insert_external_edges(&self.edges)?;
            queries.delete_specific_resolved_references(&self.keys)
        })?;
        self.edges.clear();
        self.keys.clear();
        Ok(())
    }
}
