//! Applying a SCIP index to the graph: every verdict of one pass.
//!
//! Per document whose file is indexed and unchanged since rust-analyzer
//! ran (a changed file's document waits for the next run):
//!
//! 1. What tree-sitter recorded there — resolved edges, unresolved
//!    references, external edges — is matched to the occurrence naming it
//!    ([`DocView::find_claim`]).
//! 2. The occurrence's symbol decides: the same node → **confirmed**
//!    (provenance `scip`); another node → **corrected** (the compiler wins;
//!    the tree-sitter target is kept in the metadata); std, a dependency or
//!    a local → the edge is **refuted** and removed, the reference going
//!    back to `unresolved_refs` (or to an external edge when the
//!    dependency's shard holds the item). An unresolved reference the
//!    compiler resolves becomes an edge (or external edge).
//! 3. Occurrences nothing claimed are **added**: calls, constructions and
//!    type references tree-sitter missed — inside macro invocations, on
//!    generic receivers, through trait objects — as `scip` edges marked
//!    `compilerOnly` (dropped, never restored, when their target file is
//!    re-extracted), and `implements` edges from `impl Trait for Type`.
//! 4. Trait-method → impl-method dispatch edges are checked against what
//!    each impl member implements: the heuristic ones rust-analyzer agrees
//!    with are marked `compilerVerified`, those to a method that implements
//!    another (or no) trait method are removed, and missing ones are added.
//!
//! Everything is written in one transaction at the end
//! ([`QueryBuilder::apply_compiler_changes`]).

use std::collections::{HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::deps::{DependencyTarget, DependencyTargets};
use super::map::{
    self,
    ClaimSite,
    DocView,
    HeaderRole,
    ImplRole,
    Pos,
    Target,
    bare,
    last_segment,
    map_definitions,
    start_of,
    symbol_infos,
};
use super::report::{CompilerReport, StrategyCounts, push_sample};
use super::run::CompilerState;
use super::scip::{LOCAL, ScipIndex, SymbolId};
use super::source::Follow;
use crate::db::{
    CompilerChanges,
    EdgeRow,
    EdgeUpdate,
    ExternalEdge,
    ExternalEdgeRow,
    QueryBuilder,
    UnresolvedRow,
};
use crate::error::Result;
use crate::resolution::external::graphs::Reach;
use crate::types::{
    Edge,
    EdgeKind,
    Language,
    Metadata,
    Node,
    NodeKind,
    Provenance,
    UnresolvedReference,
};

/// Edge metadata key: what the compiler said (`confirmed`, `corrected`,
/// `resolved`, `added`, `generated`).
pub const COMPILER_KEY: &str = "compiler";
/// Edge metadata key: the edge has no tree-sitter reference behind it (so
/// it is dropped, not restored, when its target is re-extracted).
pub const COMPILER_ONLY_KEY: &str = "compilerOnly";
/// `resolvedBy` of an edge the compiler resolved.
pub const RESOLVED_BY_COMPILER: &str = "compiler";

/// Knobs of one pass.
#[derive(Debug, Clone)]
pub struct ApplyOptions {
    pub budget: Duration,
    pub max_open: usize,
    /// Write every verdict as JSON lines here (differential checks).
    pub dump: Option<PathBuf>,
}

/// What an apply works from.
pub(crate) struct ApplyInput<'a> {
    pub(crate) queries: &'a QueryBuilder,
    pub(crate) project_root: &'a Path,
    pub(crate) index: &'a ScipIndex,
    pub(crate) state: Option<&'a CompilerState>,
    pub(crate) reach: &'a Reach,
}

/// One indexed, unchanged document's inputs.
struct Loaded {
    doc: usize,
    source: String,
    nodes: Vec<Node>,
}

/// Apply `input.index` to the graph.
pub(crate) fn apply(input: &ApplyInput<'_>, options: &ApplyOptions) -> Result<CompilerReport> {
    let started = Instant::now();
    let deadline = started + options.budget;
    let queries = input.queries;
    let index = input.index;
    let mut report = CompilerReport {
        tool_version: index.tool_version.clone(),
        complete: true,
        ..CompilerReport::default()
    };
    let previous_generated = queries.get_compiler_generated_ids()?;

    // Documents whose file the index holds unchanged.
    let mut loaded: Vec<Loaded> = Vec::new();
    for (doc, document) in index.documents.iter().enumerate() {
        report.documents.total += 1;
        let path = document.relative_path.as_str();
        let Some(record) = queries.get_file_by_path(path)? else {
            report.documents.unindexed += 1;
            continue;
        };
        if record.language != Language::Rust {
            report.documents.unindexed += 1;
            continue;
        }
        // Only the run's own record says which files its index describes.
        let recorded = input
            .state
            .is_some_and(|state| state.files.get(path) == Some(&record.content_hash));
        let source = std::fs::read(input.project_root.join(path))
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        let Some(source) = source.filter(|source| {
            recorded && crate::utils::sha256_hex(source.as_bytes()) == record.content_hash
        }) else {
            report.documents.stale += 1;
            continue;
        };
        let nodes: Vec<Node> = queries
            .get_nodes_by_file(path)?
            .into_iter()
            .filter(|node| !previous_generated.contains(&node.id))
            .collect();
        loaded.push(Loaded { doc, source, nodes });
    }
    report.documents.applied = loaded.len();
    let documented: HashSet<&str> = index
        .documents
        .iter()
        .map(|document| document.relative_path.as_str())
        .collect();
    report.documents.without_document = super::rust_files(queries)?
        .iter()
        .filter(|path| !documented.contains(path.as_str()))
        .count();

    let views: Vec<(usize, DocView<'_>)> = loaded
        .iter()
        .map(|entry| {
            (
                entry.doc,
                DocView::new(&index.documents[entry.doc], &entry.source, &entry.nodes),
            )
        })
        .collect();
    let symbol_ids: HashMap<&str, SymbolId> = index
        .symbols
        .iter()
        .enumerate()
        .skip(1)
        .map(|(id, text)| (text.as_str(), id as SymbolId))
        .collect();
    let infos = symbol_infos(index);
    let definitions = map_definitions(index, &views, &infos, &symbol_ids);
    report.definitions.mapped = definitions.mapped;
    report.definitions.generated = definitions.generated.len();
    for ((kind, reason), count) in &definitions.unmapped {
        let reason = match reason {
            map::Unmapped::NoNode => "no-node",
            map::Unmapped::Ambiguous => "ambiguous",
        };
        *report
            .definitions
            .unmapped
            .entry(format!("{}:{reason}", scip_kind_name(*kind)))
            .or_default() += count;
    }

    let deps = DependencyTargets::new(input.reach, options.max_open);
    report.dependency_graphs = deps.graphs();
    let mut pass = Pass {
        queries,
        index,
        definitions: &definitions,
        deps: &deps,
        targets: HashMap::new(),
        changes: CompilerChanges::default(),
        report,
        dump: options.dump.as_deref().and_then(Dump::create),
        dispatch: HashSet::new(),
        roles: HashMap::new(),
        generated_ids: definitions
            .generated
            .iter()
            .map(|generated| generated.node.id.clone())
            .collect(),
        nodes: HashMap::new(),
    };

    // Generated nodes: refresh, add, and drop the ones no longer produced.
    for generated in &definitions.generated {
        pass.nodes
            .insert(generated.node.id.clone(), Some(generated.node.clone()));
        pass.changes.insert_generated.push(generated.node.clone());
        if let Some(parent) = &generated.parent {
            let mut contains = Edge::new(parent, &generated.node.id, EdgeKind::Contains);
            contains.provenance = Some(Provenance::Scip);
            contains.metadata = Some(Metadata::from_iter([
                (COMPILER_KEY.to_string(), Value::from("generated")),
                (
                    "macro".to_string(),
                    Value::from(generated.macro_name.clone()),
                ),
            ]));
            pass.changes.edge_inserts.push(contains);
        }
    }
    pass.changes.remove_generated = previous_generated
        .iter()
        .filter(|id| !pass.generated_ids.contains(*id))
        .cloned()
        .collect();
    for (symbol, ids) in &definitions.nodes {
        for id in ids {
            pass.changes.symbols.push((
                id.clone(),
                index.symbol(*symbol).to_string(),
                pass.generated_ids.contains(id),
            ));
        }
    }

    let mut valid_files: HashSet<&str> = HashSet::new();
    for (_, view) in &views {
        valid_files.insert(view.path());
        if Instant::now() >= deadline {
            pass.report.complete = false;
            break;
        }
        pass.file(view, &symbol_ids)?;
    }
    if pass.report.complete {
        pass.dispatch_edges(&valid_files)?;
    }

    let Pass {
        changes,
        mut report,
        mut dump,
        ..
    } = pass;
    queries.apply_compiler_changes(&changes)?;
    if let Some(dump) = dump.as_mut() {
        dump.flush();
    }
    report.opens = deps.opens();
    report.elapsed_ms = started.elapsed().as_millis() as u64;
    Ok(report)
}

/// What an `impl` member node implements.
enum MemberRole {
    /// A member of an inherent impl: no trait method.
    Inherent,
    /// The project trait method (node id) it implements.
    Implements(String),
    /// A method of a std or dependency trait.
    Foreign,
    /// Its trait could not be read.
    Unknown,
}

/// The pass's working state.
struct Pass<'a> {
    queries: &'a QueryBuilder,
    index: &'a ScipIndex,
    definitions: &'a map::Definitions,
    deps: &'a DependencyTargets<'a>,
    targets: HashMap<SymbolId, Target>,
    changes: CompilerChanges,
    report: CompilerReport,
    dump: Option<Dump>,
    /// `(trait method node, impl method node)` pairs rust-analyzer defines.
    dispatch: HashSet<(String, String)>,
    /// What each `impl` member node implements.
    roles: HashMap<String, MemberRole>,
    generated_ids: HashSet<String>,
    /// Nodes looked up by id (targets, for samples and kinds).
    nodes: HashMap<String, Option<Node>>,
}

/// A claim: what tree-sitter recorded at a position.
enum ClaimRow<'r> {
    Edge(&'r EdgeRow),
    Unresolved(&'r UnresolvedRow),
    External(&'r ExternalEdgeRow),
}

/// Where the compiler's answer for a claim points.
enum Answer {
    Node(String),
    External(Box<ExternalEdge>),
    Std,
    /// A derive/proc-macro-generated item: no source, no node.
    Generated,
    Dependency(&'static str),
    Local,
    Unverifiable(&'static str),
}

impl<'a> Pass<'a> {
    /// What `id` names from `file` (cached per symbol unless several
    /// definitions make the answer depend on the file).
    fn target(&mut self, id: SymbolId, file: &str) -> Target {
        if let Some(known) = self.targets.get(&id) {
            return known.clone();
        }
        let target = map::target_of(self.index, self.definitions, id, file);
        if self
            .definitions
            .nodes
            .get(&id)
            .is_none_or(|ids| ids.len() < 2)
        {
            self.targets.insert(id, target.clone());
        }
        target
    }

    fn node(&mut self, id: &str) -> Option<Node> {
        if let Some(known) = self.nodes.get(id) {
            return known.clone();
        }
        let node = self.queries.get_node_by_id(id).ok().flatten();
        self.nodes.insert(id.to_string(), node.clone());
        node
    }

    /// The compiler's answer for `symbol`, a reference of `kind` made by
    /// `source` as `written`.
    #[allow(clippy::too_many_arguments)]
    fn answer(
        &mut self,
        file: &str,
        symbol: SymbolId,
        source: &str,
        kind: EdgeKind,
        written: &str,
        pos: Pos,
    ) -> Answer {
        match self.target(symbol, file) {
            Target::Local => Answer::Local,
            Target::Node(id) => Answer::Node(id),
            Target::Std => Answer::Std,
            Target::Generated => Answer::Generated,
            Target::ProjectUnmapped => Answer::Unverifiable("project-unmapped"),
            Target::Unknown => Answer::Unverifiable("unknown-symbol"),
            Target::Dependency(parsed) => match self.deps.lookup(symbol, &parsed) {
                DependencyTarget::Found { graph, node } => {
                    let kind = match (kind, node.kind) {
                        (EdgeKind::Calls, NodeKind::Struct | NodeKind::Union) => {
                            EdgeKind::Instantiates
                        }
                        (EdgeKind::Instantiates, NodeKind::Function | NodeKind::Method) => {
                            EdgeKind::Calls
                        }
                        (kind, _) => kind,
                    };
                    Answer::External(Box::new(ExternalEdge {
                        source: source.to_string(),
                        kind,
                        target_graph_kind: graph.kind,
                        target_graph_key: graph.key.clone(),
                        target_node_id: node.id.clone(),
                        target_name: node.name.clone(),
                        target_qualified_name: node.qualified_name.clone(),
                        target_kind: node.kind,
                        target_file_path: node.file_path.clone(),
                        target_line: Some(node.start_line),
                        reference_name: written.to_string(),
                        line: Some(pos.0),
                        column: Some(pos.1),
                        confidence: 1.0,
                        resolved_by: RESOLVED_BY_COMPILER.to_string(),
                        metadata: None,
                    }))
                }
                DependencyTarget::NoGraph => Answer::Dependency("dependency-no-shard"),
                DependencyTarget::NotFound => Answer::Dependency("dependency-not-found"),
                DependencyTarget::NotLinked => Answer::Dependency("dependency-not-linked"),
            },
        }
    }

    /// The edge's target is an item declared inside its source's own span
    /// (a `const` or `fn` in a function body).
    fn local_item(&mut self, edge: &Edge) -> bool {
        let (Some(source), Some(target)) = (self.node(&edge.source), self.node(&edge.target))
        else {
            return false;
        };
        source.file_path == target.file_path
            && source.start_line <= target.start_line
            && target.end_line <= source.end_line
    }

    /// A reference of `kind` as an edge to `target`: a call of a struct
    /// constructs it, and the reverse.
    fn edge_kind_for(&mut self, kind: EdgeKind, target: &str) -> EdgeKind {
        match self.node(target).map(|node| node.kind) {
            Some(NodeKind::Struct | NodeKind::Union | NodeKind::Class)
                if kind == EdgeKind::Calls =>
            {
                EdgeKind::Instantiates
            }
            Some(NodeKind::Function | NodeKind::Method) if kind == EdgeKind::Instantiates => {
                EdgeKind::Calls
            }
            _ => kind,
        }
    }

    fn file_of(&mut self, id: &str) -> String {
        self.node(id).map(|node| node.file_path).unwrap_or_default()
    }

    fn describe(&mut self, id: &str) -> String {
        match self.node(id) {
            Some(node) => format!(
                "{} ({}:{})",
                node.qualified_name, node.file_path, node.start_line
            ),
            None => id.to_string(),
        }
    }

    fn file(&mut self, view: &DocView<'_>, symbol_ids: &HashMap<&str, SymbolId>) -> Result<()> {
        let path = view.path();
        let file_nodes: HashMap<&str, &Node> = view
            .nodes
            .iter()
            .map(|node| (node.id.as_str(), node))
            .collect();
        let edges: Vec<EdgeRow> = self
            .queries
            .get_edge_rows_from_file(path)?
            .into_iter()
            .filter(|row| {
                file_nodes.contains_key(row.edge.source.as_str())
                    && row.edge.provenance != Some(Provenance::Heuristic)
                    // `imports` edges tie a file to its own `use` items:
                    // structure, not a reference the compiler resolves.
                    && row.edge.kind != EdgeKind::Imports
            })
            .collect();
        let unresolved = self.queries.get_unresolved_rows_for_file(path)?;
        let external: Vec<ExternalEdgeRow> = self
            .queries
            .get_external_edge_rows_from_file(path)?
            .into_iter()
            .filter(|row| file_nodes.contains_key(row.edge.source.as_str()))
            .collect();
        let target_ids: Vec<String> = edges.iter().map(|row| row.edge.target.clone()).collect();
        for (id, node) in self.queries.get_nodes_by_ids(&target_ids)? {
            self.nodes.insert(id, Some(node));
        }

        let mut claimed: HashSet<usize> = HashSet::new();
        // Existing (source, target, kind, line) — additions never repeat one.
        let mut present: HashSet<(String, String, EdgeKind, u32)> = HashSet::new();
        for row in &edges {
            present.insert((
                row.edge.source.clone(),
                row.edge.target.clone(),
                row.edge.kind,
                row.edge.line.unwrap_or(0),
            ));
        }
        let mut present_external: HashSet<(String, String, u32)> = HashSet::new();
        for row in &external {
            present_external.insert((
                row.edge.source.clone(),
                row.edge.target_node_id.clone(),
                row.edge.line.unwrap_or(0),
            ));
        }

        let claims: Vec<ClaimRow<'_>> = edges
            .iter()
            .map(ClaimRow::Edge)
            .chain(unresolved.iter().map(ClaimRow::Unresolved))
            .chain(external.iter().map(ClaimRow::External))
            .collect();
        for claim in &claims {
            let kind = match claim {
                ClaimRow::Edge(row) => row.edge.kind,
                ClaimRow::Unresolved(row) => row.reference.reference_kind,
                ClaimRow::External(row) => row.edge.kind,
            };
            let (source, written, pos, metadata) = match claim {
                ClaimRow::Edge(row) => {
                    let written = written_of_edge(&row.edge, self.nodes.get(&row.edge.target));
                    (
                        row.edge.source.as_str(),
                        written,
                        (row.edge.line.unwrap_or(0), row.edge.column.unwrap_or(0)),
                        row.edge.metadata.as_ref(),
                    )
                }
                ClaimRow::Unresolved(row) => (
                    row.reference.from_node_id.as_str(),
                    row.reference.reference_name.clone(),
                    (row.reference.line, row.reference.column),
                    row.reference.metadata.as_ref(),
                ),
                ClaimRow::External(row) => (
                    row.edge.source.as_str(),
                    row.edge.reference_name.clone(),
                    (row.edge.line.unwrap_or(0), row.edge.column.unwrap_or(0)),
                    row.edge.metadata.as_ref(),
                ),
            };
            // `recv.m()` (as written, or with its receiver dropped).
            let method = kind == EdgeKind::Calls
                && ((written.contains('.') && !written.contains("::"))
                    || crate::types::receiver_was_dropped(metadata));
            let name = last_segment(&written);
            let skip = crate::types::dropped_receiver_text(metadata)
                .map_or(0, |receiver| map::calls_in_receiver(receiver, name));
            let scope = file_nodes.get(source);
            let scope_end = scope.map_or(pos.0 + 1, |node| node.end_line.max(pos.0));
            // Recorded without a position (a value reference): its scope.
            let whole_scope = pos.0 == 0;
            let at = match (whole_scope, scope) {
                (true, Some(node)) => (node.start_line, node.start_column),
                _ => pos,
            };
            let found = view.find_claim(&ClaimSite {
                pos: at,
                name,
                method,
                skip,
                scope_end_line: scope_end,
                whole_scope,
            });
            if let Some(occurrence) = found {
                claimed.insert(occurrence);
            }
            match claim {
                ClaimRow::Edge(row) => self.edge_claim(view, row, &written, found),
                ClaimRow::Unresolved(row) => self.unresolved_claim(view, row, found),
                ClaimRow::External(row) => self.external_claim(view, row, found),
            }
        }

        // Members of `impl` blocks: what each implements.
        for (member, role) in map::impl_members(self.index, symbol_ids, view) {
            self.report.definitions.impl_members += 1;
            let Some(implementing) = self.definitions.node(member).map(str::to_string) else {
                continue;
            };
            let role = match role {
                ImplRole::Inherent => MemberRole::Inherent,
                ImplRole::Trait(None) => MemberRole::Unknown,
                ImplRole::Trait(Some(declared)) => match self.definitions.node(declared) {
                    Some(declared) => {
                        self.dispatch
                            .insert((declared.to_string(), implementing.clone()));
                        MemberRole::Implements(declared.to_string())
                    }
                    None if self.definitions.project_symbols.contains(&declared) => {
                        MemberRole::Unknown
                    }
                    None => MemberRole::Foreign,
                },
            };
            self.roles.insert(implementing, role);
        }

        // What nobody claimed.
        for (i, occurrence) in view.doc.occurrences.iter().enumerate() {
            if occurrence.is_definition() || occurrence.symbol == LOCAL || claimed.contains(&i) {
                continue;
            }
            match map::header_role(view, i) {
                Some(HeaderRole::Other) => {
                    self.skip("impl-header");
                    continue;
                }
                Some(HeaderRole::Trait(self_occurrence)) => {
                    self.implements(
                        view,
                        i,
                        self_occurrence,
                        &mut present,
                        &mut present_external,
                    );
                    continue;
                }
                None => {}
            }
            let text = view.text(i);
            if text.is_empty() || matches!(text, "Self" | "self" | "super" | "crate") {
                self.skip("self-or-path-keyword");
                continue;
            }
            let range = &occurrence.range;
            if view.source.in_use_item(range.start_line) {
                self.skip("use-item");
                continue;
            }
            if view.source.in_comment_or_attribute(range) {
                self.skip("comment-or-attribute");
                continue;
            }
            let follow = view.source.follow(range);
            if follow == Follow::PathSep {
                self.skip("path-qualifier");
                continue;
            }
            let pos = start_of(range);
            let Some(scope) = view.scope_at(pos) else {
                self.skip("no-scope");
                continue;
            };
            let source = scope.id.clone();
            let in_macro = in_macro_invocation(view, pos);
            match self.answer(
                path,
                occurrence.symbol,
                &source,
                EdgeKind::References,
                text,
                pos,
            ) {
                Answer::Node(target) => {
                    let Some(target_node) = self.node(&target) else {
                        self.skip("target-missing");
                        continue;
                    };
                    let follow = shape(view, range, follow);
                    let Some(kind) = addition_kind(target_node.kind, follow) else {
                        self.skip("field-or-module");
                        continue;
                    };
                    if !present.insert((source.clone(), target.clone(), kind, pos.0)) {
                        self.skip("already-recorded");
                        continue;
                    }
                    let mut edge = added_edge(&source, &target, kind, pos, text);
                    if in_macro {
                        if let Some(metadata) = edge.metadata.as_mut() {
                            metadata.insert("inMacro".to_string(), Value::Bool(true));
                        }
                        self.report.added.in_macros += 1;
                    }
                    self.changes.edge_inserts.push(edge);
                    *self
                        .report
                        .added
                        .edges
                        .entry(kind.as_str().to_string())
                        .or_default() += 1;
                    let text = text.to_string();
                    self.log_added(path, pos, &text, &source, &target, kind);
                }
                Answer::External(mut edge) => {
                    let follow = shape(view, range, follow);
                    let Some(kind) = addition_kind(edge.target_kind, follow) else {
                        self.skip("field-or-module");
                        continue;
                    };
                    if !present_external.insert((
                        source.clone(),
                        edge.target_node_id.clone(),
                        pos.0,
                    )) {
                        self.skip("already-recorded");
                        continue;
                    }
                    edge.kind = kind;
                    edge.metadata = Some(compiler_only_metadata());
                    self.changes.external_inserts.push(*edge);
                    self.report.added.external += 1;
                }
                Answer::Std => self.skip("std"),
                Answer::Generated => self.skip("generated"),
                Answer::Dependency(why) | Answer::Unverifiable(why) => self.skip(why),
                Answer::Local => {}
            }
        }
        Ok(())
    }

    fn skip(&mut self, why: &str) {
        *self
            .report
            .added
            .skipped
            .entry(why.to_string())
            .or_default() += 1;
    }

    /// An `implements` edge for the trait of an `impl Trait for Type`
    /// header tree-sitter did not record, from the self type's node.
    fn implements(
        &mut self,
        view: &DocView<'_>,
        trait_at: usize,
        self_occurrence: Option<usize>,
        present: &mut HashSet<(String, String, EdgeKind, u32)>,
        present_external: &mut HashSet<(String, String, u32)>,
    ) {
        let Some(self_node) = self_occurrence
            .map(|i| view.occurrence(i).symbol)
            .and_then(|symbol| self.definitions.node(symbol))
            .map(str::to_string)
        else {
            self.skip("impl-header");
            return;
        };
        let occurrence = view.occurrence(trait_at);
        let pos = start_of(&occurrence.range);
        let written = view.text(trait_at).to_string();
        match self.answer(
            view.path(),
            occurrence.symbol,
            &self_node,
            EdgeKind::Implements,
            &written,
            pos,
        ) {
            Answer::Node(target) => {
                if present.insert((
                    self_node.clone(),
                    target.clone(),
                    EdgeKind::Implements,
                    pos.0,
                )) {
                    self.changes.edge_inserts.push(added_edge(
                        &self_node,
                        &target,
                        EdgeKind::Implements,
                        pos,
                        &written,
                    ));
                    self.report.implements_added += 1;
                    self.log_added(
                        view.path(),
                        pos,
                        &written,
                        &self_node,
                        &target,
                        EdgeKind::Implements,
                    );
                }
            }
            Answer::External(mut edge) => {
                if present_external.insert((self_node.clone(), edge.target_node_id.clone(), pos.0))
                {
                    edge.metadata = Some(compiler_only_metadata());
                    self.changes.external_inserts.push(*edge);
                    self.report.added.external += 1;
                }
            }
            _ => self.skip("impl-header"),
        }
    }

    fn strategy(&mut self, edge: &Edge) -> &mut StrategyCounts {
        let strategy = edge
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("resolvedBy"))
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        self.report.by_strategy.entry(strategy).or_default()
    }

    fn confirm(&mut self, view: &DocView<'_>, row: &EdgeRow, written: &str, verdict: &str) {
        let edge = &row.edge;
        self.report.edges.confirmed += 1;
        self.strategy(edge).confirmed += 1;
        let mut metadata = edge.metadata.clone().unwrap_or_default();
        metadata.insert(COMPILER_KEY.to_string(), Value::from("confirmed"));
        self.changes.edge_updates.push(EdgeUpdate {
            id: row.id,
            target: edge.target.clone(),
            kind: edge.kind,
            metadata,
            provenance: Some(Provenance::Scip),
        });
        self.log(view.path(), verdict, edge, written, None);
    }

    fn edge_claim(
        &mut self,
        view: &DocView<'_>,
        row: &EdgeRow,
        written: &str,
        found: Option<usize>,
    ) {
        let edge = &row.edge;
        let prior = edge.provenance == Some(Provenance::Scip);
        let Some(found) = found else {
            if prior {
                self.report.edges.prior_kept += 1;
            } else {
                self.report.edges.no_occurrence += 1;
                self.strategy(edge).no_occurrence += 1;
                self.log(view.path(), "no-occurrence", edge, written, None);
            }
            return;
        };
        let occurrence = view.occurrence(found);
        let pos = match edge.line {
            Some(line) => (line, edge.column.unwrap_or(0)),
            None => self
                .node(&edge.source)
                .map_or((1, 0), |node| (node.start_line, node.start_column)),
        };
        if !prior {
            self.report.edges.examined += 1;
        }
        let answer = self.answer(
            view.path(),
            occurrence.symbol,
            &edge.source,
            edge.kind,
            written,
            pos,
        );
        // rust-analyzer never saw tree-sitter's target — code a `cfg` it
        // did not build (`cfg(tokio_taskdump)`'s twin of a function), or a
        // file outside every crate — so it cannot say the edge is wrong:
        // keep it, and add the target the compiler did see.
        let agrees = matches!(&answer, Answer::Node(target) if *target == edge.target);
        if !prior && !agrees && !self.definitions.files.contains_key(&edge.target) {
            self.report.edges.target_not_seen += 1;
            self.strategy(edge).unverifiable += 1;
            if let Answer::Node(target) = answer {
                let kind = self.edge_kind_for(edge.kind, &target);
                let mut added = added_edge(&edge.source, &target, kind, pos, written);
                if let Some(metadata) = added.metadata.as_mut() {
                    metadata.insert("besideTreeSitter".to_string(), Value::Bool(true));
                }
                self.changes.edge_inserts.push(added);
                *self
                    .report
                    .added
                    .edges
                    .entry(kind.as_str().to_string())
                    .or_default() += 1;
            }
            self.log(view.path(), "target-not-seen", edge, written, None);
            return;
        }
        match answer {
            Answer::Node(target) if target == edge.target => {
                if prior {
                    self.report.edges.prior_kept += 1;
                } else {
                    self.confirm(view, row, written, "confirmed");
                }
            }
            Answer::Node(target) => {
                let kind = self.edge_kind_for(edge.kind, &target);
                let mut metadata = edge.metadata.clone().unwrap_or_default();
                if prior {
                    self.report.edges.prior_changed += 1;
                } else {
                    self.report.edges.corrected += 1;
                    self.strategy(edge).corrected += 1;
                    let resolved_by = metadata.get("resolvedBy").cloned();
                    metadata.insert(
                        "treeSitterTarget".to_string(),
                        Value::from(edge.target.clone()),
                    );
                    if let Some(resolved_by) = resolved_by {
                        metadata.insert("treeSitterResolvedBy".to_string(), resolved_by);
                    }
                }
                metadata.insert(COMPILER_KEY.to_string(), Value::from("corrected"));
                metadata.insert("resolvedBy".to_string(), Value::from(RESOLVED_BY_COMPILER));
                metadata.insert("confidence".to_string(), Value::from(1.0));
                if !prior {
                    let from = self.describe(&edge.target);
                    let to = self.describe(&target);
                    let source = self.describe(&edge.source);
                    let strategy = strategy_of(edge);
                    push_sample(&mut self.report.samples.corrected, || {
                        format!(
                            "{}:{} `{written}` in {source}: {from} ⇒ {to} [{strategy}]",
                            view.path(),
                            pos.0
                        )
                    });
                    self.log(view.path(), "corrected", edge, written, Some(&to));
                }
                self.changes.edge_updates.push(EdgeUpdate {
                    id: row.id,
                    target,
                    kind,
                    metadata,
                    provenance: Some(Provenance::Scip),
                });
            }
            Answer::Unverifiable(_) | Answer::Dependency(_) if prior => {
                self.report.edges.prior_kept += 1;
            }
            Answer::Local if self.local_item(edge) => {
                // rust-analyzer names an item declared inside a function
                // body `local N`; tree-sitter's target is that item.
                if prior {
                    self.report.edges.prior_kept += 1;
                } else {
                    self.report.edges.local_items += 1;
                    self.confirm(view, row, written, "confirmed-local-item");
                }
            }
            Answer::Unverifiable(why) => {
                self.report.edges.unverifiable += 1;
                self.strategy(edge).unverifiable += 1;
                let symbol = self.index.symbol(occurrence.symbol).to_string();
                self.log(
                    view.path(),
                    &format!("unverifiable-{why}"),
                    edge,
                    written,
                    Some(&symbol),
                );
            }
            refuted => {
                if prior {
                    // The compiler's own edge no longer holds: drop it.
                    self.report.edges.prior_changed += 1;
                    self.changes.edge_deletes.push(row.id);
                    return;
                }
                let (why, external) = match refuted {
                    Answer::Std => ("std", None),
                    Answer::Generated => ("generated", None),
                    Answer::Local => ("local", None),
                    Answer::Dependency(why) => (why, None),
                    Answer::External(edge) => ("dependency", Some(edge)),
                    Answer::Node(_) | Answer::Unverifiable(_) => return,
                };
                match why {
                    "std" => self.report.edges.refuted_std += 1,
                    "generated" => self.report.edges.refuted_generated += 1,
                    "local" => self.report.edges.refuted_local += 1,
                    _ => self.report.edges.refuted_dependency += 1,
                }
                self.strategy(edge).refuted += 1;
                let from = self.describe(&edge.target);
                let source = self.describe(&edge.source);
                let strategy = strategy_of(edge);
                let symbol = self.index.symbol(occurrence.symbol).to_string();
                push_sample(&mut self.report.samples.refuted, || {
                    format!(
                        "{}:{} `{written}` in {source}: {from} ⇒ {why} {symbol} [{strategy}]",
                        view.path(),
                        pos.0
                    )
                });
                self.log(
                    view.path(),
                    &format!("refuted-{why}"),
                    edge,
                    written,
                    Some(&symbol),
                );
                self.changes.edge_deletes.push(row.id);
                match external {
                    Some(mut external) => {
                        external.metadata = reference_metadata(edge);
                        self.changes.external_inserts.push(*external);
                    }
                    // A local is not a reference to anything to resolve.
                    None if why == "local" => {}
                    None => {
                        let reference_name = written_reference(edge, written);
                        let language = self
                            .node(&edge.source)
                            .map_or(Language::Rust, |node| node.language);
                        self.changes.unresolved_inserts.push(UnresolvedReference {
                            from_node_id: edge.source.clone(),
                            reference_name,
                            reference_kind: match edge.kind {
                                EdgeKind::Instantiates => EdgeKind::Calls,
                                kind => kind,
                            },
                            line: pos.0,
                            column: pos.1,
                            file_path: Some(view.path().to_string()),
                            language: Some(language),
                            candidates: None,
                            metadata: reference_metadata(edge),
                        });
                    }
                }
            }
        }
    }

    fn unresolved_claim(&mut self, view: &DocView<'_>, row: &UnresolvedRow, found: Option<usize>) {
        let reference = &row.reference;
        self.report.unresolved.examined += 1;
        let Some(found) = found else {
            *self
                .report
                .unresolved
                .remaining
                .entry("no-occurrence".to_string())
                .or_default() += 1;
            self.log_reference(view.path(), "unresolved-no-occurrence", reference, None);
            return;
        };
        let occurrence = view.occurrence(found);
        let pos = (reference.line, reference.column);
        let answer = self.answer(
            view.path(),
            occurrence.symbol,
            &reference.from_node_id,
            reference.reference_kind,
            &reference.reference_name,
            pos,
        );
        let remaining = match answer {
            Answer::Node(target) => {
                let kind = match self.node(&target).map(|node| node.kind) {
                    Some(NodeKind::Struct | NodeKind::Union | NodeKind::Class)
                        if reference.reference_kind == EdgeKind::Calls =>
                    {
                        EdgeKind::Instantiates
                    }
                    _ => reference.reference_kind,
                };
                let mut metadata = reference.metadata.clone().unwrap_or_default();
                metadata.insert("confidence".to_string(), Value::from(1.0));
                metadata.insert("resolvedBy".to_string(), Value::from(RESOLVED_BY_COMPILER));
                metadata.insert(COMPILER_KEY.to_string(), Value::from("resolved"));
                let written = &reference.reference_name;
                if written.contains("::") || written.contains('.') {
                    metadata.insert("referenceName".to_string(), Value::from(written.clone()));
                }
                self.changes.edge_inserts.push(Edge {
                    source: reference.from_node_id.clone(),
                    target: target.clone(),
                    kind,
                    metadata: Some(metadata),
                    line: Some(pos.0),
                    column: Some(pos.1),
                    provenance: Some(Provenance::Scip),
                });
                self.changes.unresolved_deletes.push(row.id);
                self.report.unresolved.resolved_in_project += 1;
                let to = self.describe(&target);
                let source = self.describe(&reference.from_node_id);
                push_sample(&mut self.report.samples.resolved, || {
                    format!("{}:{} `{written}` in {source} ⇒ {to}", view.path(), pos.0)
                });
                self.log_reference(view.path(), "resolved", reference, Some(&to));
                return;
            }
            Answer::External(mut edge) => {
                edge.metadata = reference.metadata.clone();
                let to = format!("{}::{}", edge.target_graph_key, edge.target_qualified_name);
                self.changes.external_inserts.push(*edge);
                self.changes.unresolved_deletes.push(row.id);
                self.report.unresolved.resolved_external += 1;
                self.log_reference(view.path(), "resolved-external", reference, Some(&to));
                return;
            }
            Answer::Std => "std",
            Answer::Generated => "generated",
            Answer::Local => "local",
            Answer::Dependency(why) | Answer::Unverifiable(why) => why,
        };
        *self
            .report
            .unresolved
            .remaining
            .entry(remaining.to_string())
            .or_default() += 1;
        let symbol = self.index.symbol(occurrence.symbol).to_string();
        self.log_reference(
            view.path(),
            &format!("unresolved-{remaining}"),
            reference,
            Some(&symbol),
        );
    }

    fn external_claim(&mut self, view: &DocView<'_>, row: &ExternalEdgeRow, found: Option<usize>) {
        let edge = &row.edge;
        let prior = edge.resolved_by == RESOLVED_BY_COMPILER;
        let Some(found) = found else {
            if !prior {
                self.report.external.unverifiable += 1;
            }
            return;
        };
        if !prior {
            self.report.external.examined += 1;
        }
        let occurrence = view.occurrence(found);
        let pos = (edge.line.unwrap_or(0), edge.column.unwrap_or(0));
        match self.answer(
            view.path(),
            occurrence.symbol,
            &edge.source,
            edge.kind,
            &edge.reference_name,
            pos,
        ) {
            Answer::External(new)
                if new.target_graph_key == edge.target_graph_key
                    && new.target_node_id == edge.target_node_id =>
            {
                if !prior {
                    self.report.external.confirmed += 1;
                }
            }
            Answer::External(mut new) => {
                let from = format!("{}::{}", edge.target_graph_key, edge.target_qualified_name);
                let to = format!("{}::{}", new.target_graph_key, new.target_qualified_name);
                if !prior {
                    self.report.external.corrected += 1;
                    self.log_external(view.path(), "external-corrected", edge, &from, &to);
                }
                let mut metadata = edge.metadata.clone().unwrap_or_default();
                metadata.insert("treeSitterTarget".to_string(), Value::from(from));
                new.metadata = Some(metadata);
                new.kind = edge.kind;
                new.reference_name = edge.reference_name.clone();
                self.changes.external_deletes.push(row.id);
                self.changes.external_inserts.push(*new);
            }
            Answer::Node(target) => {
                if !prior {
                    self.report.external.into_project += 1;
                    let from = format!("{}::{}", edge.target_graph_key, edge.target_qualified_name);
                    let to = self.describe(&target);
                    self.log_external(view.path(), "external-into-project", edge, &from, &to);
                }
                let mut metadata = edge.metadata.clone().unwrap_or_default();
                metadata.insert("confidence".to_string(), Value::from(1.0));
                metadata.insert("resolvedBy".to_string(), Value::from(RESOLVED_BY_COMPILER));
                metadata.insert(COMPILER_KEY.to_string(), Value::from("corrected"));
                self.changes.external_deletes.push(row.id);
                self.changes.edge_inserts.push(Edge {
                    source: edge.source.clone(),
                    target,
                    kind: edge.kind,
                    metadata: Some(metadata),
                    line: edge.line,
                    column: edge.column,
                    provenance: Some(Provenance::Scip),
                });
            }
            Answer::Std | Answer::Generated | Answer::Local => {
                if !prior {
                    self.report.external.refuted += 1;
                    let from = format!("{}::{}", edge.target_graph_key, edge.target_qualified_name);
                    let symbol = self.index.symbol(occurrence.symbol).to_string();
                    self.log_external(view.path(), "external-refuted", edge, &from, &symbol);
                }
                self.changes.external_deletes.push(row.id);
                if !is_compiler_only(edge.metadata.as_ref()) {
                    self.changes.unresolved_inserts.push(UnresolvedReference {
                        from_node_id: edge.source.clone(),
                        reference_name: edge.reference_name.clone(),
                        reference_kind: match edge.kind {
                            EdgeKind::Instantiates => EdgeKind::Calls,
                            kind => kind,
                        },
                        line: pos.0,
                        column: pos.1,
                        file_path: Some(view.path().to_string()),
                        language: Some(Language::Rust),
                        candidates: None,
                        metadata: edge.metadata.clone(),
                    });
                }
            }
            Answer::Dependency(_) | Answer::Unverifiable(_) => {
                if !prior {
                    self.report.external.unverifiable += 1;
                }
            }
        }
    }

    /// Compare the heuristic trait → impl dispatch edges with what each
    /// impl member implements.
    fn dispatch_edges(&mut self, valid_files: &HashSet<&str>) -> Result<()> {
        let existing = self.queries.get_interface_dispatch_rows(Language::Rust)?;
        let mut seen: HashSet<(String, String)> = HashSet::new();
        for row in &existing {
            let pair = (row.edge.source.clone(), row.edge.target.clone());
            seen.insert(pair.clone());
            if self.dispatch.contains(&pair) {
                if !is_compiler_verified(row.edge.metadata.as_ref()) {
                    self.report.dispatch.confirmed += 1;
                    let mut metadata = row.edge.metadata.clone().unwrap_or_default();
                    metadata.insert("compilerVerified".to_string(), Value::Bool(true));
                    self.changes.edge_updates.push(EdgeUpdate {
                        id: row.id,
                        target: row.edge.target.clone(),
                        kind: row.edge.kind,
                        metadata,
                        provenance: Some(Provenance::Heuristic),
                    });
                }
                continue;
            }
            // Removed only when rust-analyzer says what the target method
            // implements, and it is not this trait method.
            let wrong = match self.roles.get(&row.edge.target) {
                Some(MemberRole::Inherent | MemberRole::Foreign) => true,
                Some(MemberRole::Implements(declared)) => *declared != row.edge.source,
                Some(MemberRole::Unknown) | None => false,
            };
            if wrong && valid_files.contains(self.file_of(&row.edge.target).as_str()) {
                self.report.dispatch.removed += 1;
                self.changes.edge_deletes.push(row.id);
                let from = self.describe(&row.edge.source);
                let to = self.describe(&row.edge.target);
                if let Some(dump) = self.dump.as_mut() {
                    dump.write(&serde_json::json!({
                        "verdict": "dispatch-removed",
                        "source": from,
                        "to": to,
                    }));
                }
            }
        }
        let mut missing: Vec<(String, String)> = self
            .dispatch
            .iter()
            .filter(|pair| !seen.contains(*pair))
            .cloned()
            .collect();
        missing.sort();
        for (declared, implementing) in missing {
            let Some(declared_node) = self.node(&declared) else {
                continue;
            };
            let Some(implementing_node) = self.node(&implementing) else {
                continue;
            };
            let mut edge = Edge::new(&declared, &implementing, EdgeKind::Calls);
            edge.line = Some(declared_node.start_line);
            edge.provenance = Some(Provenance::Heuristic);
            edge.metadata = Some(Metadata::from_iter([
                ("synthesizedBy".to_string(), Value::from("interface-impl")),
                (
                    "via".to_string(),
                    Value::from(implementing_node.name.clone()),
                ),
                (
                    "registeredAt".to_string(),
                    Value::from(format!(
                        "{}:{}",
                        implementing_node.file_path, implementing_node.start_line
                    )),
                ),
                ("compilerVerified".to_string(), Value::Bool(true)),
            ]));
            self.changes.edge_inserts.push(edge);
            self.report.dispatch.added += 1;
            if let Some(dump) = self.dump.as_mut() {
                dump.write(&serde_json::json!({
                    "verdict": "dispatch-added",
                    "source": declared_node.qualified_name,
                    "sourceFile": declared_node.file_path,
                    "to": format!(
                        "{} ({}:{})",
                        implementing_node.qualified_name,
                        implementing_node.file_path,
                        implementing_node.start_line
                    ),
                }));
            }
        }
        Ok(())
    }

    fn log(&mut self, file: &str, verdict: &str, edge: &Edge, written: &str, to: Option<&str>) {
        let Some(dump) = self.dump.as_mut() else {
            return;
        };
        let record = serde_json::json!({
            "verdict": verdict,
            "file": file,
            "line": edge.line,
            "col": edge.column,
            "kind": edge.kind.as_str(),
            "written": written,
            "source": edge.source,
            "from": edge.target,
            "to": to,
            "strategy": strategy_of(edge),
        });
        dump.write(&record);
    }

    fn log_external(
        &mut self,
        file: &str,
        verdict: &str,
        edge: &ExternalEdge,
        from: &str,
        to: &str,
    ) {
        let Some(dump) = self.dump.as_mut() else {
            return;
        };
        dump.write(&serde_json::json!({
            "verdict": verdict,
            "file": file,
            "line": edge.line,
            "col": edge.column,
            "kind": edge.kind.as_str(),
            "written": edge.reference_name,
            "source": edge.source,
            "from": from,
            "to": to,
            "strategy": edge.resolved_by,
        }));
    }

    fn log_reference(
        &mut self,
        file: &str,
        verdict: &str,
        reference: &UnresolvedReference,
        to: Option<&str>,
    ) {
        let Some(dump) = self.dump.as_mut() else {
            return;
        };
        let record = serde_json::json!({
            "verdict": verdict,
            "file": file,
            "line": reference.line,
            "col": reference.column,
            "kind": reference.reference_kind.as_str(),
            "written": reference.reference_name,
            "source": reference.from_node_id,
            "to": to,
        });
        dump.write(&record);
    }

    fn log_added(
        &mut self,
        file: &str,
        pos: Pos,
        written: &str,
        source: &str,
        target: &str,
        kind: EdgeKind,
    ) {
        if self.report.samples.added.len() >= super::report::SAMPLES_KEPT && self.dump.is_none() {
            return;
        }
        let from = self.describe(source);
        let to = self.describe(target);
        push_sample(&mut self.report.samples.added, || {
            format!(
                "{file}:{} `{written}` {} in {from} ⇒ {to}",
                pos.0,
                kind.as_str()
            )
        });
        if let Some(dump) = self.dump.as_mut() {
            dump.write(&serde_json::json!({
                "verdict": "added",
                "file": file,
                "line": pos.0,
                "col": pos.1,
                "kind": kind.as_str(),
                "written": written,
                "source": from,
                "to": to,
            }));
        }
    }
}

/// Whether `pos` lies inside a macro invocation's arguments on its line
/// (`println!("{}", helper())`, `vec![f()]`).
fn in_macro_invocation(view: &DocView<'_>, pos: Pos) -> bool {
    let line = view.source.line(pos.0.saturating_sub(1));
    let before = line.get(..pos.1 as usize).unwrap_or("");
    before.contains("!(") || before.contains("![") || before.contains("!{")
}

/// What follows an occurrence, for the edge kind: in a pattern nothing is
/// called or constructed (`Some(x) =>`, `let Point { x, .. } = p`).
fn shape(view: &DocView<'_>, range: &super::scip::Range, follow: Follow) -> Follow {
    if matches!(follow, Follow::Call | Follow::Brace) && view.source.in_pattern(range) {
        Follow::Other
    } else {
        follow
    }
}

/// The kind of an edge the compiler adds for an occurrence of a node of
/// `kind`; `None` for what the graph does not link (fields, modules).
/// Kinds follow tree-sitter's: a tuple variant `V(..)` is called, a struct
/// literal or tuple struct `S(..)` instantiated.
fn addition_kind(kind: NodeKind, follow: Follow) -> Option<EdgeKind> {
    Some(match kind {
        NodeKind::Function | NodeKind::Method => {
            if follow == Follow::Call {
                EdgeKind::Calls
            } else {
                EdgeKind::References
            }
        }
        NodeKind::EnumMember => {
            if follow == Follow::Call {
                EdgeKind::Calls
            } else {
                EdgeKind::References
            }
        }
        NodeKind::Struct | NodeKind::Union | NodeKind::Class => {
            if matches!(follow, Follow::Call | Follow::Brace) {
                EdgeKind::Instantiates
            } else {
                EdgeKind::References
            }
        }
        NodeKind::Enum
        | NodeKind::Trait
        | NodeKind::Interface
        | NodeKind::TypeAlias
        | NodeKind::Constant
        | NodeKind::Variable
        | NodeKind::Macro => EdgeKind::References,
        _ => return None,
    })
}

fn added_edge(source: &str, target: &str, kind: EdgeKind, pos: Pos, written: &str) -> Edge {
    let mut metadata = compiler_only_metadata();
    metadata.insert("confidence".to_string(), Value::from(1.0));
    metadata.insert("resolvedBy".to_string(), Value::from(RESOLVED_BY_COMPILER));
    metadata.insert("referenceName".to_string(), Value::from(written));
    Edge {
        source: source.to_string(),
        target: target.to_string(),
        kind,
        metadata: Some(metadata),
        line: Some(pos.0),
        column: Some(pos.1),
        provenance: Some(Provenance::Scip),
    }
}

fn compiler_only_metadata() -> Metadata {
    Metadata::from_iter([
        (COMPILER_KEY.to_string(), Value::from("added")),
        (COMPILER_ONLY_KEY.to_string(), Value::Bool(true)),
    ])
}

/// An edge only the compiler knew (no tree-sitter reference behind it).
pub fn is_compiler_only(metadata: Option<&Metadata>) -> bool {
    metadata
        .and_then(|metadata| metadata.get(COMPILER_ONLY_KEY))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn is_compiler_verified(metadata: Option<&Metadata>) -> bool {
    metadata
        .and_then(|metadata| metadata.get("compilerVerified"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn strategy_of(edge: &Edge) -> String {
    edge.metadata
        .as_ref()
        .and_then(|metadata| {
            metadata
                .get("treeSitterResolvedBy")
                .or_else(|| metadata.get("resolvedBy"))
        })
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string()
}

/// The reference as tree-sitter wrote it: `referenceName` when recorded,
/// else the target's name.
fn written_of_edge(edge: &Edge, target: Option<&Option<Node>>) -> String {
    edge.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("referenceName"))
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .or_else(|| {
            target
                .and_then(Option::as_ref)
                .map(|node| node.name.clone())
        })
        .unwrap_or_default()
}

/// The name a refuted edge's reference is restored under.
fn written_reference(edge: &Edge, written: &str) -> String {
    edge.metadata
        .as_ref()
        .and_then(|metadata| metadata.get("referenceName"))
        .and_then(Value::as_str)
        .map_or_else(|| bare(written).to_string(), str::to_string)
}

/// The reference's own metadata, without resolution bookkeeping.
fn reference_metadata(edge: &Edge) -> Option<Metadata> {
    let mut metadata = edge.metadata.clone()?;
    for key in [
        "confidence",
        "resolvedBy",
        "referenceName",
        COMPILER_KEY,
        "treeSitterTarget",
        "treeSitterResolvedBy",
    ] {
        metadata.remove(key);
    }
    (!metadata.is_empty()).then_some(metadata)
}

fn scip_kind_name(kind: i32) -> String {
    use super::scip::kind as k;
    match kind {
        k::ASSOCIATED_TYPE => "associated-type".into(),
        k::CONSTANT => "constant".into(),
        k::ENUM => "enum".into(),
        k::ENUM_MEMBER => "variant".into(),
        k::FIELD => "field".into(),
        k::FUNCTION => "function".into(),
        k::MACRO => "macro".into(),
        k::METHOD => "method".into(),
        k::MODULE => "module".into(),
        k::STRUCT => "struct".into(),
        k::TRAIT => "trait".into(),
        k::TYPE_ALIAS => "type-alias".into(),
        k::UNION => "union".into(),
        k::TRAIT_METHOD => "trait-method".into(),
        k::STATIC_METHOD => "associated-fn".into(),
        k::STATIC_VARIABLE => "static".into(),
        other => format!("kind-{other}"),
    }
}

/// A JSON-lines log of every verdict.
struct Dump {
    out: std::io::BufWriter<std::fs::File>,
}

impl Dump {
    fn create(path: &Path) -> Option<Dump> {
        let file = std::fs::File::create(path).ok()?;
        Some(Dump {
            out: std::io::BufWriter::new(file),
        })
    }

    fn write(&mut self, record: &Value) {
        let _ = writeln!(self.out, "{record}");
    }

    fn flush(&mut self) {
        let _ = self.out.flush();
    }
}
