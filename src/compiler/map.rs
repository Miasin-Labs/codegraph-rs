//! SCIP occurrences onto the graph's nodes.
//!
//! * **Definitions** map a symbol to the node that defines it: the node of
//!   the document's file whose name is the occurrence's text and whose
//!   range holds it (the innermost such node). A definition no node holds
//!   whose item starts at a macro invocation (`make_fn!(generated_one);`)
//!   is a macro-generated item: it becomes a [`GeneratedNode`].
//! * **Scopes** answer "which node is this position in" for edges the
//!   compiler adds: the innermost function/type/module node, else the file.
//! * **Claims** — what tree-sitter recorded at a position (an edge, an
//!   unresolved reference, an external edge; line + column of the
//!   expression + the name as written) — are matched to the occurrence
//!   that names them: the first occurrence at or after the claim's start
//!   whose text is the claim's last name segment (`a.b().c()` recorded at
//!   `a` is the `c` occurrence), within the claim's scope and a few lines.
//!
//! Every lookup is per file and indexed; nothing here is O(file) per
//! occurrence.

use std::collections::{HashMap, HashSet};

use super::scip::{Document, LOCAL, Occurrence, Range, ScipIndex, SymbolId, SymbolInfo, kind};
use super::source::SourceText;
use super::symbol::{Suffix, Symbol};
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::resolution::types::SCOPE_KINDS;
use crate::types::{Language, Node, NodeKind, Visibility};

/// How far below a claim's line a method call's occurrence may be (a
/// chain written over several lines).
const CLAIM_LOOKAHEAD_LINES: u32 = 24;

/// A position: one-based line (the graph's), zero-based byte column.
pub(crate) type Pos = (u32, u32);

pub(crate) fn start_of(range: &Range) -> Pos {
    (range.start_line + 1, range.start_col)
}

fn node_start(node: &Node) -> Pos {
    (node.start_line, node.start_column)
}

fn node_end(node: &Node) -> Pos {
    (node.end_line, node.end_column)
}

fn node_contains(node: &Node, pos: Pos) -> bool {
    // A node recorded without columns covers its whole lines.
    if node.end_column == 0 && node.start_column == 0 {
        return pos.0 >= node.start_line && pos.0 <= node.end_line;
    }
    pos >= node_start(node) && pos < node_end(node)
}

/// A raw identifier's name: `r#type` → `type`.
pub(crate) fn bare(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

/// The last segment of a reference as written, generic arguments dropped:
/// `self.cache.get` → `get`, `Vec::<u8>::new` → `new`, `a::b` → `b`,
/// `collect::<Vec<_>>` → `collect`.
pub(crate) fn last_segment(name: &str) -> &str {
    let mut depth = 0usize;
    let mut last = (0, 0);
    let mut start = None;
    for (at, c) in name.char_indices() {
        let ident = c.is_alphanumeric() || c == '_' || c == '#';
        if depth == 0 && ident {
            start.get_or_insert(at);
            continue;
        }
        if let Some(from) = start.take() {
            last = (from, at);
        }
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    if let Some(from) = start {
        last = (from, name.len());
    }
    let segment = &name[last.0..last.1];
    bare(if segment.is_empty() { name } else { segment })
}

// =============================================================================
// One document
// =============================================================================

/// A document whose file the index holds unchanged, with its source and
/// the file's nodes.
pub(crate) struct DocView<'a> {
    pub(crate) doc: &'a Document,
    pub(crate) source: SourceText<'a>,
    pub(crate) nodes: &'a [Node],
    /// Occurrence indexes in (line, column) order.
    order: Vec<usize>,
    /// Each occurrence's text (empty when the range is not one line).
    texts: Vec<&'a str>,
    scopes: ScopeIndex,
}

impl<'a> DocView<'a> {
    pub(crate) fn new(doc: &'a Document, source: &'a str, nodes: &'a [Node]) -> Self {
        let source = SourceText::new(source);
        let texts = doc
            .occurrences
            .iter()
            .map(|occurrence| source.slice(&occurrence.range))
            .collect();
        let mut order: Vec<usize> = (0..doc.occurrences.len()).collect();
        order.sort_by_key(|&i| {
            let range = &doc.occurrences[i].range;
            (range.start_line, range.start_col)
        });
        let scopes = ScopeIndex::new(nodes, source.line_count());
        DocView {
            doc,
            source,
            nodes,
            order,
            texts,
            scopes,
        }
    }

    pub(crate) fn path(&self) -> &'a str {
        &self.doc.relative_path
    }

    pub(crate) fn occurrence(&self, index: usize) -> &'a Occurrence {
        &self.doc.occurrences[index]
    }

    pub(crate) fn text(&self, index: usize) -> &'a str {
        bare(self.texts[index])
    }

    /// The innermost scope node (function, type, module, else the file)
    /// holding `pos`.
    pub(crate) fn scope_at(&self, pos: Pos) -> Option<&'a Node> {
        self.scopes.innermost(self.nodes, pos)
    }

    /// The occurrence a claim names (see the module docs). A `method`
    /// claim (`recv.m()`, a dropped receiver) only takes an occurrence
    /// written after a `.`, and skips the calls of that name inside its
    /// own receiver (`self.get_ref().get_ref()` recorded at `self` is the
    /// second `get_ref`); past the claim's own position, a local never
    /// answers it (`a.f(c).c()` recorded at `a` is not the argument `c`).
    /// A claim recorded without a line is looked for through its scope.
    pub(crate) fn find_claim(&self, site: &ClaimSite<'_>) -> Option<usize> {
        let name = last_segment(site.name);
        let target = (site.pos.0.saturating_sub(1), site.pos.1);
        let first = self.order.partition_point(|&i| {
            let range = &self.doc.occurrences[i].range;
            (range.start_line, range.start_col) < target
        });
        let scope_last = site.scope_end_line.saturating_sub(1);
        // Only a method chain spreads over lines (`a\n    .m()` is recorded
        // at `a`); anything else is named on its own line — a later line's
        // occurrence of the name is another reference (rust-analyzer
        // records none for some, like `V::Value`).
        let last_line = if site.whole_scope {
            scope_last
        } else if site.method {
            (target.0 + CLAIM_LOOKAHEAD_LINES).min(scope_last)
        } else {
            target.0
        };
        let mut exact = None;
        let mut skip = site.skip;
        for &i in &self.order[first..] {
            let occurrence = &self.doc.occurrences[i];
            if occurrence.range.start_line > last_line.max(target.0) {
                break;
            }
            if occurrence.is_definition() {
                continue;
            }
            let at = (occurrence.range.start_line, occurrence.range.start_col);
            if at == target && exact.is_none() && self.text(i) == "Self" {
                exact = Some(i);
            }
            let fits = if site.method {
                // The claim sits at the receiver; the method follows a `.`
                // and is called (`self.defer.defer(w)`: not the field).
                occurrence.symbol != LOCAL
                    && self.source.after_dot(&occurrence.range)
                    && self.source.follow(&occurrence.range) == super::source::Follow::Call
            } else {
                at == target || occurrence.symbol != LOCAL
            };
            if fits && self.text(i) == name {
                if skip == 0 {
                    return Some(i);
                }
                skip -= 1;
            }
        }
        // `Self` at the very position names the type the claim recorded
        // by its own name.
        exact
    }
}

/// What tree-sitter recorded, as [`DocView::find_claim`] looks for it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ClaimSite<'n> {
    /// Where it was recorded (the expression's start).
    pub(crate) pos: Pos,
    /// The name as written; its last segment is looked for.
    pub(crate) name: &'n str,
    /// `recv.m()`: the occurrence follows a `.` and is called.
    pub(crate) method: bool,
    /// Same-named method calls inside the receiver, passed over first.
    pub(crate) skip: usize,
    pub(crate) scope_end_line: u32,
    /// Recorded without a line: look through the whole scope.
    pub(crate) whole_scope: bool,
}

#[cfg(test)]
impl<'n> ClaimSite<'n> {
    pub(crate) fn at(pos: Pos, name: &'n str, scope_end_line: u32) -> Self {
        ClaimSite {
            pos,
            name,
            method: false,
            skip: 0,
            scope_end_line,
            whole_scope: false,
        }
    }
}

/// How many calls of `method` a compacted receiver (`self.get_ref()`,
/// `a.b(..).m()`) holds.
pub(crate) fn calls_in_receiver(receiver: &str, method: &str) -> usize {
    if method.is_empty() {
        return 0;
    }
    receiver
        .match_indices(method)
        .filter(|(at, _)| {
            let before = receiver[..*at].ends_with('.');
            let after = &receiver[at + method.len()..];
            before && (after.starts_with('(') || after.starts_with("::<"))
        })
        .count()
}

/// Innermost-scope lookup over one file's nodes: each line records the
/// innermost scope covering it, and a scope's parent is kept so a position
/// outside the line's scope (two items on one line) walks outward.
struct ScopeIndex {
    /// Node indexes of the scopes, by line (zero-based).
    by_line: Vec<Option<u32>>,
    /// Scopes starting or ending on a line: a position there may be in
    /// one of them rather than in the line's innermost.
    edges: HashMap<u32, Vec<u32>>,
    parent: HashMap<u32, u32>,
    file: Option<u32>,
}

impl ScopeIndex {
    fn new(nodes: &[Node], lines: usize) -> Self {
        let mut scopes: Vec<u32> = (0..nodes.len() as u32)
            .filter(|&i| SCOPE_KINDS.contains(&nodes[i as usize].kind))
            .collect();
        let file = (0..nodes.len() as u32).find(|&i| nodes[i as usize].kind == NodeKind::File);
        // Outer before inner: by start, then the longer first.
        scopes.sort_by(|&a, &b| {
            let (a, b) = (&nodes[a as usize], &nodes[b as usize]);
            node_start(a)
                .cmp(&node_start(b))
                .then_with(|| node_end(b).cmp(&node_end(a)))
        });
        let mut parent = HashMap::new();
        let mut stack: Vec<u32> = Vec::new();
        for &scope in &scopes {
            let node = &nodes[scope as usize];
            while let Some(&top) = stack.last() {
                if node_contains(&nodes[top as usize], node_start(node)) {
                    break;
                }
                stack.pop();
            }
            if let Some(&top) = stack.last() {
                parent.insert(scope, top);
            }
            stack.push(scope);
        }
        let mut by_line = vec![None; lines + 1];
        let mut edges: HashMap<u32, Vec<u32>> = HashMap::new();
        // Paint outer scopes first so inner ones overwrite them.
        for &scope in &scopes {
            let node = &nodes[scope as usize];
            let (start, end) = (
                node.start_line.saturating_sub(1),
                node.end_line.saturating_sub(1),
            );
            for line in start..=end.min(lines as u32) {
                by_line[line as usize] = Some(scope);
            }
            edges.entry(start).or_default().push(scope);
            if end != start {
                edges.entry(end).or_default().push(scope);
            }
        }
        ScopeIndex {
            by_line,
            edges,
            parent,
            file,
        }
    }

    fn innermost<'n>(&self, nodes: &'n [Node], pos: Pos) -> Option<&'n Node> {
        let line = pos.0.saturating_sub(1);
        let mut best: Option<u32> = None;
        let mut consider = |candidate: u32| {
            let node = &nodes[candidate as usize];
            if !node_contains(node, pos) {
                return;
            }
            let span = |n: &Node| (n.end_line - n.start_line, n.end_column);
            if best.is_none_or(|b| span(node) < span(&nodes[b as usize])) {
                best = Some(candidate);
            }
        };
        if let Some(scopes) = self.edges.get(&line) {
            for &scope in scopes {
                consider(scope);
            }
        }
        let mut walk = self.by_line.get(line as usize).copied().flatten();
        while let Some(scope) = walk {
            if node_contains(&nodes[scope as usize], pos) {
                consider(scope);
                break;
            }
            walk = self.parent.get(&scope).copied();
        }
        best.or(self.file).map(|i| &nodes[i as usize])
    }
}

// =============================================================================
// Definitions
// =============================================================================

/// A node for an item a macro invocation generates.
#[derive(Debug, Clone)]
pub(crate) struct GeneratedNode {
    pub(crate) node: Node,
    /// The scope holding the invocation (the `contains` edge's source).
    pub(crate) parent: Option<String>,
    /// The macro whose invocation generated it (`make_fn`).
    pub(crate) macro_name: String,
}

/// Why a definition got no node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum Unmapped {
    /// No node of that name holds it (an item the extractor does not
    /// record: an associated type, a local `fn` in a block, …).
    NoNode,
    /// Several nodes fit equally.
    Ambiguous,
}

/// Every definition of the project's documents, mapped.
#[derive(Default)]
pub(crate) struct Definitions {
    /// Symbol → the node ids defining it (one, except `cfg` twins and
    /// items of separate crates sharing a path — every integration test,
    /// bench and example is a crate of its own).
    pub(crate) nodes: HashMap<SymbolId, Vec<String>>,
    /// The file of every node in `nodes`.
    pub(crate) files: HashMap<String, String>,
    pub(crate) generated: Vec<GeneratedNode>,
    /// Symbols a project document defines (mapped or not; stale documents
    /// included), so a reference to one is never taken for a dependency.
    pub(crate) project_symbols: HashSet<SymbolId>,
    /// The packages (name, version) the project's documents define
    /// symbols in. The version matters: a workspace crate may carry a
    /// dependency's name (a `[patch]` stand-in).
    pub(crate) packages: HashSet<(String, String)>,
    /// Defined symbols by (package, version, path below the modules): a
    /// file mounted in two crates under different module names
    /// (`#[path = "fixture.rs"] mod federation;`) is indexed once, so a
    /// reference from the other crate names a symbol with no definition
    /// of its own.
    pub(crate) by_tail: HashMap<(String, String, String), Vec<SymbolId>>,
    /// Unmapped definitions by SCIP kind and reason.
    pub(crate) unmapped: HashMap<(i32, Unmapped), usize>,
    pub(crate) mapped: usize,
}

impl Definitions {
    /// The one node `symbol` is defined by, if exactly one.
    pub(crate) fn node(&self, symbol: SymbolId) -> Option<&str> {
        match self.nodes.get(&symbol)?.as_slice() {
            [one] => Some(one.as_str()),
            _ => None,
        }
    }

    /// The node `symbol` names when referenced from `file`: the only one,
    /// else the one defined in `file`, else the only one in `file`'s
    /// directory (the crate of an integration test or bench).
    pub(crate) fn node_in(&self, symbol: SymbolId, file: &str) -> Option<&str> {
        let ids = self.nodes.get(&symbol)?;
        if let [one] = ids.as_slice() {
            return Some(one.as_str());
        }
        let file_of = |id: &String| self.files.get(id).map_or("", String::as_str);
        let same_file: Vec<&String> = ids.iter().filter(|id| file_of(id) == file).collect();
        if !same_file.is_empty() {
            return match same_file.as_slice() {
                [one] => Some(one.as_str()),
                _ => None,
            };
        }
        let dir = |path: &str| path.rsplit_once('/').map_or("", |(dir, _)| dir).to_string();
        let here = dir(file);
        let same_dir: Vec<&String> = ids.iter().filter(|id| dir(file_of(id)) == here).collect();
        match same_dir.as_slice() {
            [one] => Some(one.as_str()),
            _ => None,
        }
    }
}

/// Symbols' `SymbolInformation`, by symbol.
pub(crate) fn symbol_infos(index: &ScipIndex) -> HashMap<SymbolId, &SymbolInfo> {
    let mut infos = HashMap::new();
    for document in &index.documents {
        for info in &document.symbols {
            if info.symbol != LOCAL {
                infos.entry(info.symbol).or_insert(info);
            }
        }
    }
    infos
}

/// Record the definitions of every document (stale ones only as project
/// symbols); map those of `views` (documents whose file is current) to
/// nodes. `views` are paired with their index in `index.documents`.
pub(crate) fn map_definitions(
    index: &ScipIndex,
    views: &[(usize, DocView<'_>)],
    infos: &HashMap<SymbolId, &SymbolInfo>,
    symbol_ids: &HashMap<&str, SymbolId>,
) -> Definitions {
    let mut definitions = Definitions::default();
    for document in &index.documents {
        for occurrence in &document.occurrences {
            if occurrence.is_definition() && occurrence.symbol != LOCAL {
                definitions.project_symbols.insert(occurrence.symbol);
            }
        }
    }
    for &symbol in &definitions.project_symbols {
        let text = index.symbol(symbol);
        if let Some(parsed) = Symbol::parse(text) {
            if let Some(tail) = symbol_tail(text, &parsed) {
                definitions
                    .by_tail
                    .entry((
                        parsed.package.clone(),
                        parsed.version.clone(),
                        tail.to_string(),
                    ))
                    .or_default()
                    .push(symbol);
            }
            definitions
                .packages
                .insert((parsed.package, parsed.version));
        }
    }
    for (_, view) in views {
        let mut by_name: HashMap<&str, Vec<&Node>> = HashMap::new();
        for node in view.nodes {
            if !matches!(
                node.kind,
                NodeKind::File | NodeKind::Import | NodeKind::Export | NodeKind::Parameter
            ) {
                by_name.entry(bare(&node.name)).or_default().push(node);
            }
        }
        for (occurrence_index, occurrence) in view.doc.occurrences.iter().enumerate() {
            if !occurrence.is_definition() || occurrence.symbol == LOCAL {
                continue;
            }
            let info = infos.get(&occurrence.symbol);
            let scip_kind = info.map_or(0, |info| info.kind);
            let symbol_text = index.symbol(occurrence.symbol);
            // An impl block itself (`impl#[Sq][Shape]`) has no node.
            if symbol_text.ends_with(']') {
                continue;
            }
            // Modules are files or `mod` items the graph already holds as
            // such; crate roots have no node.
            if scip_kind == kind::MODULE {
                let module = by_name
                    .get(view.text(occurrence_index))
                    .and_then(|nodes| nodes.iter().find(|node| node.kind == NodeKind::Module));
                if let Some(node) = module {
                    push_node(&mut definitions, occurrence.symbol, &node.id, view.path());
                }
                continue;
            }
            let pos = start_of(&occurrence.range);
            let name = view.text(occurrence_index);
            let candidates: Vec<&Node> = by_name
                .get(name)
                .map(|nodes| {
                    nodes
                        .iter()
                        .copied()
                        .filter(|node| node.start_line <= pos.0 && pos.0 <= node.end_line)
                        .collect()
                })
                .unwrap_or_default();
            match innermost_candidate(&candidates) {
                Candidate::One(node) => {
                    push_node(&mut definitions, occurrence.symbol, &node.id, view.path())
                }
                Candidate::Tie => {
                    *definitions
                        .unmapped
                        .entry((scip_kind, Unmapped::Ambiguous))
                        .or_default() += 1;
                }
                Candidate::None => {
                    let generated = generated_node(
                        index,
                        symbol_ids,
                        view,
                        occurrence,
                        info.copied(),
                        &definitions,
                    );
                    match generated {
                        Some(generated) => {
                            push_node(
                                &mut definitions,
                                occurrence.symbol,
                                &generated.node.id,
                                view.path(),
                            );
                            definitions.generated.push(generated);
                        }
                        None => {
                            *definitions
                                .unmapped
                                .entry((scip_kind, Unmapped::NoNode))
                                .or_default() += 1;
                        }
                    }
                }
            }
        }
    }
    definitions.mapped =
        definitions.nodes.values().map(Vec::len).sum::<usize>() - definitions.generated.len();
    definitions
}

fn push_node(definitions: &mut Definitions, symbol: SymbolId, id: &str, file: &str) {
    let ids = definitions.nodes.entry(symbol).or_default();
    if !ids.iter().any(|known| known == id) {
        ids.push(id.to_string());
    }
    definitions.files.insert(id.to_string(), file.to_string());
}

enum Candidate<'n> {
    One(&'n Node),
    Tie,
    None,
}

fn innermost_candidate<'n>(candidates: &[&'n Node]) -> Candidate<'n> {
    let span = |node: &Node| (node.end_line - node.start_line, node.end_column);
    let Some(best) = candidates.iter().min_by_key(|node| span(node)) else {
        return Candidate::None;
    };
    let ties = candidates
        .iter()
        .filter(|node| span(node) == span(best) && node.start_line == best.start_line)
        .count();
    if ties > 1 {
        Candidate::Tie
    } else {
        Candidate::One(best)
    }
}

/// The node for a definition a macro invocation generated, when the
/// definition's item starts at one.
fn generated_node(
    index: &ScipIndex,
    symbol_ids: &HashMap<&str, SymbolId>,
    view: &DocView<'_>,
    occurrence: &Occurrence,
    info: Option<&SymbolInfo>,
    definitions: &Definitions,
) -> Option<GeneratedNode> {
    let item = occurrence.enclosing.unwrap_or(occurrence.range);
    if !view
        .source
        .macro_invocation_at(item.start_line, item.start_col)
    {
        return None;
    }
    let info = info?;
    let symbol = Symbol::parse(index.symbol(occurrence.symbol))?;
    let found = symbol.item();
    let node_kind = generated_kind(info.kind, &found)?;
    let line_text = view.source.line(item.start_line);
    let macro_name = line_text
        .get(item.start_col as usize..)
        .and_then(|rest| rest.split('!').next())
        .map(|path| path.rsplit("::").next().unwrap_or(path).trim().to_string())
        .unwrap_or_default();
    let name = if info.display_name.is_empty() {
        found.name.to_string()
    } else {
        info.display_name.clone()
    };
    let qualified_name = match found.owner {
        Some(owner) if node_kind != NodeKind::Function => format!("{owner}::{name}"),
        _ => name.clone(),
    };
    let file = view.path();
    let start_line = item.start_line + 1;
    let id = generate_node_id(
        file,
        node_kind,
        &format!("{qualified_name}#generated"),
        start_line,
    );
    let signature = info.signature.clone();
    let public = signature
        .as_deref()
        .is_some_and(|signature| signature.trim_start().starts_with("pub"));
    let mut node = Node::new(
        id,
        node_kind,
        name,
        qualified_name,
        file,
        Language::Rust,
        start_line,
        item.end_line + 1,
    );
    node.start_column = item.start_col;
    node.end_column = item.end_col;
    node.signature = signature;
    node.docstring = (!info.documentation.is_empty()).then(|| info.documentation.join("\n"));
    node.visibility = Some(if public {
        Visibility::Public
    } else {
        Visibility::Private
    });
    node.is_exported = Some(public);
    // The owner type when the project defines it, else the scope holding
    // the invocation.
    let owner_node = Symbol::text_without_last(index.symbol(occurrence.symbol))
        .filter(|_| found.owner.is_some() && !found.in_impl)
        .and_then(|owner| symbol_ids.get(owner))
        .and_then(|&owner| definitions.node(owner))
        .map(str::to_string);
    let parent =
        owner_node.or_else(|| view.scope_at(start_of(&item)).map(|scope| scope.id.clone()));
    Some(GeneratedNode {
        node,
        parent,
        macro_name,
    })
}

/// The graph kind of a generated item.
fn generated_kind(scip_kind: i32, item: &super::symbol::Item<'_>) -> Option<NodeKind> {
    Some(match scip_kind {
        kind::FUNCTION => NodeKind::Function,
        kind::STATIC_METHOD if !item.in_impl => NodeKind::Function,
        kind::METHOD | kind::STATIC_METHOD | kind::TRAIT_METHOD => NodeKind::Method,
        kind::STRUCT => NodeKind::Struct,
        kind::ENUM => NodeKind::Enum,
        kind::UNION => NodeKind::Union,
        kind::TRAIT => NodeKind::Trait,
        kind::ENUM_MEMBER => NodeKind::EnumMember,
        kind::CONSTANT => NodeKind::Constant,
        kind::STATIC_VARIABLE => NodeKind::Variable,
        kind::MACRO => NodeKind::Macro,
        kind::TYPE_ALIAS if item.suffix == Suffix::Type => NodeKind::TypeAlias,
        _ => return None,
    })
}

// =============================================================================
// Targets
// =============================================================================

/// What an occurrence's symbol names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// A local variable, parameter, closure, type parameter or label.
    Local,
    /// A node of this project.
    Node(String),
    /// A project item no single node stands for (see [`Unmapped`]), or one
    /// defined in a document whose file changed since.
    ProjectUnmapped,
    /// A workspace crate's item with no source to point at: generated by a
    /// `#[derive]` or another procedural macro (or a build script).
    Generated,
    /// An item of a dependency crate.
    Dependency(Symbol),
    /// An item of the standard library.
    Std,
    /// A symbol that does not parse.
    Unknown,
}

/// What `symbol` names when referenced from `file`.
pub(crate) fn target_of(
    index: &ScipIndex,
    definitions: &Definitions,
    symbol: SymbolId,
    file: &str,
) -> Target {
    if symbol == LOCAL {
        return Target::Local;
    }
    if let Some(node) = definitions.node_in(symbol, file) {
        return Target::Node(node.to_string());
    }
    if definitions.project_symbols.contains(&symbol) {
        return Target::ProjectUnmapped;
    }
    let Some(parsed) = Symbol::parse(index.symbol(symbol)) else {
        return Target::Unknown;
    };
    if parsed.is_std() {
        Target::Std
    } else if definitions
        .packages
        .contains(&(parsed.package.clone(), parsed.version.clone()))
    {
        // A workspace crate's item no document defines: the same item seen
        // through another crate's module path, or one a macro generated.
        let text = index.symbol(symbol);
        let mounted = symbol_tail(text, &parsed).and_then(|tail| {
            definitions.by_tail.get(&(
                parsed.package.clone(),
                parsed.version.clone(),
                tail.to_string(),
            ))
        });
        match mounted.map(Vec::as_slice) {
            Some([one]) => match definitions.node_in(*one, file) {
                Some(node) => Target::Node(node.to_string()),
                None => Target::ProjectUnmapped,
            },
            Some(_) => Target::ProjectUnmapped,
            None => Target::Generated,
        }
    } else {
        Target::Dependency(parsed)
    }
}

/// The symbol's text below its module path (`impl#[Sq]area().` of
/// `a/b/impl#[Sq]area().`), when it has one.
fn symbol_tail<'t>(text: &'t str, parsed: &Symbol) -> Option<&'t str> {
    let first = parsed
        .descriptors
        .iter()
        .find(|descriptor| descriptor.suffix != Suffix::Namespace)?;
    text.get(first.at..)
}

// =============================================================================
// Impl blocks
// =============================================================================

/// What an item defined in an `impl` block is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ImplRole {
    /// A member of an inherent `impl Type` block.
    Inherent,
    /// A member of `impl Trait for Type`: the trait's member it implements,
    /// when the trait's occurrence in the header was found.
    Trait(Option<SymbolId>),
}

/// The `impl#[Self][Trait]` prefix of a member symbol and its bracket
/// count, when the symbol is one.
fn impl_prefix(text: &str) -> Option<(&str, usize, &str)> {
    let symbol = Symbol::parse(text)?;
    let at = symbol
        .descriptors
        .iter()
        .position(|d| d.suffix == Suffix::Type && d.name == "impl")?;
    let params = symbol.descriptors[at + 1..]
        .iter()
        .take_while(|d| d.suffix == Suffix::TypeParameter)
        .count();
    let member = symbol.descriptors.get(at + 1 + params)?;
    if params == 0 || at + 1 + params + 1 != symbol.descriptors.len() {
        return None;
    }
    Some((&text[..member.at], params, &text[member.at..]))
}

/// The trait name of an `impl#[Self][Trait]` prefix.
fn impl_trait_name(prefix: &str) -> Option<String> {
    let symbol = Symbol::parse(&format!("{prefix}x()."))?;
    symbol.item().trait_name.map(str::to_string)
}

/// How far back from an impl's first member its trait's occurrence is
/// looked for (the header sits right above the first member).
const HEADER_LOOKBACK: usize = 256;

/// The role of every `impl` member `view` defines.
pub(crate) fn impl_members(
    index: &ScipIndex,
    symbol_ids: &HashMap<&str, SymbolId>,
    view: &DocView<'_>,
) -> Vec<(SymbolId, ImplRole)> {
    let mut traits: HashMap<&str, Option<SymbolId>> = HashMap::new();
    let mut out = Vec::new();
    for (position, &i) in view.order.iter().enumerate() {
        let occurrence = view.occurrence(i);
        if !occurrence.is_definition() || occurrence.symbol == LOCAL {
            continue;
        }
        let text = index.symbol(occurrence.symbol);
        let Some((prefix, params, member)) = impl_prefix(text) else {
            continue;
        };
        if params == 1 {
            out.push((occurrence.symbol, ImplRole::Inherent));
            continue;
        }
        let trait_symbol = *traits.entry(prefix).or_insert_with(|| {
            let name = impl_trait_name(prefix)?;
            view.order[position.saturating_sub(HEADER_LOOKBACK)..position]
                .iter()
                .rev()
                .map(|&j| view.occurrence(j))
                .find(|candidate| {
                    !candidate.is_definition()
                        && candidate.symbol != LOCAL
                        && Symbol::parse(index.symbol(candidate.symbol)).is_some_and(|s| {
                            let item = s.item();
                            item.suffix == Suffix::Type && item.name == name
                        })
                })
                .map(|candidate| candidate.symbol)
        });
        let trait_member = trait_symbol.and_then(|trait_symbol| {
            let wanted = format!("{}{member}", index.symbol(trait_symbol));
            symbol_ids.get(wanted.as_str()).copied()
        });
        out.push((occurrence.symbol, ImplRole::Trait(trait_member)));
    }
    out
}

/// Where an occurrence sits in an `impl … for …` header, read from its line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeaderRole {
    /// The trait (`Shape` in `impl Shape for Sq`); the self type's
    /// occurrence index when found on the line.
    Trait(Option<usize>),
    /// Anything else in the header (the self type, generic bounds).
    Other,
}

/// Whether occurrence `i` is part of an `impl` header, and as what.
pub(crate) fn header_role(view: &DocView<'_>, i: usize) -> Option<HeaderRole> {
    let range = view.occurrence(i).range;
    let line = view.source.line(range.start_line);
    let before = line.get(..range.start_col as usize)?;
    if !has_word(before, "impl") {
        return None;
    }
    let after = line.get(range.end_col as usize..).unwrap_or("");
    let after = skip_generics(after.trim_start());
    if !after.trim_start().starts_with("for ") {
        return Some(HeaderRole::Other);
    }
    // The self type: the first type occurrence after ` for ` on the line.
    let for_at = range.end_col as usize + line[range.end_col as usize..].find(" for ")?;
    let first = view.order.partition_point(|&j| {
        let r = &view.doc.occurrences[j].range;
        (r.start_line, r.start_col) < (range.start_line, for_at as u32)
    });
    let self_occurrence = view.order[first..]
        .iter()
        .copied()
        .take_while(|&j| view.occurrence(j).range.start_line == range.start_line)
        .find(|&j| view.occurrence(j).symbol != LOCAL && !view.occurrence(j).is_definition());
    Some(HeaderRole::Trait(self_occurrence))
}

fn has_word(text: &str, word: &str) -> bool {
    text.match_indices(word).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + word.len()..].chars().next();
        !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !after.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// `<A, B<C>> rest` → ` rest`.
fn skip_generics(text: &str) -> &str {
    let Some(rest) = text.strip_prefix('<') else {
        return text;
    };
    let mut depth = 1usize;
    for (at, c) in rest.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[at + 1..];
                }
            }
            _ => {}
        }
    }
    ""
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::extract_from_source;

    const INDEX: &[u8] = include_bytes!("../../tests/compiler_fixture/index.scip");
    const LIB: &str = include_str!("../../tests/compiler_fixture/src/lib.rs");
    const OTHER: &str = include_str!("../../tests/compiler_fixture/src/other.rs");

    /// The fixture's index and the nodes tree-sitter extracts from it.
    struct Fixture {
        index: ScipIndex,
        lib: Vec<Node>,
        other: Vec<Node>,
    }

    fn fixture() -> Fixture {
        let nodes = |path: &str, source: &str| {
            extract_from_source(path, source, Some(Language::Rust), None).nodes
        };
        Fixture {
            index: ScipIndex::decode(INDEX).unwrap(),
            lib: nodes("src/lib.rs", LIB),
            other: nodes("src/other.rs", OTHER),
        }
    }

    impl Fixture {
        fn doc(&self, path: &str) -> usize {
            self.index
                .documents
                .iter()
                .position(|doc| doc.relative_path == path)
                .unwrap()
        }

        fn views(&self) -> Vec<(usize, DocView<'_>)> {
            let lib = self.doc("src/lib.rs");
            let other = self.doc("src/other.rs");
            vec![
                (
                    lib,
                    DocView::new(&self.index.documents[lib], LIB, &self.lib),
                ),
                (
                    other,
                    DocView::new(&self.index.documents[other], OTHER, &self.other),
                ),
            ]
        }

        fn symbol_ids(&self) -> HashMap<&str, SymbolId> {
            self.index
                .symbols
                .iter()
                .enumerate()
                .skip(1)
                .map(|(id, text)| (text.as_str(), id as SymbolId))
                .collect()
        }

        fn id(&self, suffix: &str) -> SymbolId {
            self.index
                .symbols
                .iter()
                .position(|text| text.ends_with(&format!(" {suffix}")))
                .unwrap_or_else(|| panic!("no symbol {suffix}")) as SymbolId
        }

        fn node_named(&self, qualified: &str) -> &Node {
            self.lib
                .iter()
                .chain(&self.other)
                .find(|node| node.qualified_name == qualified)
                .unwrap_or_else(|| panic!("no node {qualified}"))
        }
    }

    fn definitions(fixture: &Fixture) -> Definitions {
        let views = fixture.views();
        let infos = symbol_infos(&fixture.index);
        map_definitions(&fixture.index, &views, &infos, &fixture.symbol_ids())
    }

    #[test]
    fn the_fixture_index_decodes() {
        let index = ScipIndex::decode(INDEX).unwrap();
        assert_eq!(index.tool_name, "rust-analyzer");
        assert_eq!(index.project_root, "file:///fixture");
        let mut paths: Vec<&str> = index
            .documents
            .iter()
            .map(|doc| doc.relative_path.as_str())
            .collect();
        paths.sort();
        assert_eq!(paths, ["src/lib.rs", "src/other.rs"]);
        assert!(index.documents.iter().all(|doc| doc.position_encoding == 1));
    }

    #[test]
    fn definitions_land_on_the_extracted_nodes() {
        let fixture = fixture();
        let definitions = definitions(&fixture);
        for (symbol, qualified) in [
            ("Shape#", "Shape"),
            ("Shape#area().", "Shape::area"),
            ("impl#[Sq][Shape]area().", "Sq::area"),
            ("impl#[Circle][Shape]area().", "Circle::area"),
            ("Circle#r.", "Circle::r"),
            ("helper().", "helper"),
            ("other/area().", "area"),
            ("make_fn!", "make_fn"),
        ] {
            let id = fixture.id(&format!("0.1.0 {symbol}"));
            assert_eq!(
                definitions.node(id),
                Some(fixture.node_named(qualified).id.as_str()),
                "{symbol}"
            );
        }
        assert!(
            definitions.unmapped.is_empty(),
            "{:?}",
            definitions.unmapped
        );
        assert!(
            definitions
                .packages
                .contains(&("fx".to_string(), "0.1.0".to_string()))
        );
    }

    #[test]
    fn a_macro_generated_item_gets_a_generated_node() {
        let fixture = fixture();
        let definitions = definitions(&fixture);
        assert_eq!(definitions.generated.len(), 1);
        let generated = &definitions.generated[0];
        assert_eq!(generated.node.name, "generated_one");
        assert_eq!(generated.node.kind, NodeKind::Function);
        assert_eq!(generated.node.file_path, "src/lib.rs");
        assert_eq!(
            (generated.node.start_line, generated.node.end_line),
            (28, 28)
        );
        assert_eq!(generated.macro_name, "make_fn");
        assert_eq!(
            generated.node.signature.as_deref(),
            Some("pub fn generated_one() -> u32")
        );
        let file = fixture
            .lib
            .iter()
            .find(|n| n.kind == NodeKind::File)
            .unwrap();
        assert_eq!(generated.parent.as_deref(), Some(file.id.as_str()));
        let id = fixture.id("0.1.0 generated_one().");
        assert_eq!(definitions.node(id), Some(generated.node.id.as_str()));
    }

    fn method<'n>(pos: Pos, name: &'n str, scope_end_line: u32) -> ClaimSite<'n> {
        ClaimSite {
            method: true,
            ..ClaimSite::at(pos, name, scope_end_line)
        }
    }

    #[test]
    fn receivers_count_their_own_calls_of_the_method() {
        assert_eq!(calls_in_receiver("self.get_ref()", "get_ref"), 1);
        assert_eq!(calls_in_receiver("a.b(..).m().m::<T>()", "m"), 2);
        assert_eq!(calls_in_receiver("self.get_refs()", "get_ref"), 0);
        assert_eq!(calls_in_receiver("get_ref()", "get_ref"), 0);
    }

    #[test]
    fn claims_find_the_occurrence_that_names_them() {
        let fixture = fixture();
        let views = fixture.views();
        let (_, lib) = &views[0];
        let symbol = |found: Option<usize>| {
            found.map(|i| fixture.index.symbol(lib.occurrence(i).symbol).to_string())
        };
        // `s.area()` recorded at `s` (35:26): the `area` after it.
        assert!(
            symbol(lib.find_claim(&method((35, 26), "s.area", 36)))
                .unwrap()
                .ends_with(" Shape#area().")
        );
        // `Sq(2.0).area()` recorded at the expression start (49:4).
        assert!(
            symbol(lib.find_claim(&method((49, 4), "area", 50)))
                .unwrap()
                .ends_with(" impl#[Sq][Shape]area().")
        );
        // The constructor at the same position.
        assert!(
            symbol(lib.find_claim(&ClaimSite::at((49, 4), "Sq", 50)))
                .unwrap()
                .ends_with(" Sq#")
        );
        // A qualified path: its last segment.
        assert!(
            symbol(lib.find_claim(&ClaimSite::at((49, 51), "other::area", 50)))
                .unwrap()
                .ends_with(" other/area().")
        );
        // Inside macro arguments.
        assert!(
            symbol(lib.find_claim(&ClaimSite::at((40, 19), "helper", 42)))
                .unwrap()
                .ends_with(" helper().")
        );
        // Nothing of that name within the scope.
        assert_eq!(lib.find_claim(&ClaimSite::at((49, 4), "nowhere", 50)), None);
        // A claim without a line: anywhere in its scope (`uses_macro`).
        assert!(
            symbol(lib.find_claim(&ClaimSite {
                whole_scope: true,
                ..ClaimSite::at((38, 0), "generated_one", 42)
            }))
            .unwrap()
            .ends_with(" generated_one().")
        );
    }

    #[test]
    fn positions_resolve_to_their_innermost_scope() {
        let fixture = fixture();
        let views = fixture.views();
        let (_, lib) = &views[0];
        assert_eq!(lib.scope_at((35, 30)).unwrap().qualified_name, "total");
        assert_eq!(lib.scope_at((12, 8)).unwrap().qualified_name, "Sq::area");
        assert_eq!(lib.scope_at((28, 0)).unwrap().kind, NodeKind::File);
    }

    #[test]
    fn targets_tell_project_std_and_locals_apart() {
        let fixture = fixture();
        let definitions = definitions(&fixture);
        let helper = fixture.id("0.1.0 helper().");
        assert_eq!(
            target_of(&fixture.index, &definitions, helper, "src/lib.rs"),
            Target::Node(fixture.node_named("helper").id.clone())
        );
        assert_eq!(
            target_of(&fixture.index, &definitions, LOCAL, "src/lib.rs"),
            Target::Local
        );
        let boxed = fixture
            .index
            .symbols
            .iter()
            .position(|text| text.ends_with("boxed/Box#"))
            .unwrap() as SymbolId;
        assert_eq!(
            target_of(&fixture.index, &definitions, boxed, "src/lib.rs"),
            Target::Std
        );
    }

    #[test]
    fn impl_members_know_the_trait_method_they_implement() {
        let fixture = fixture();
        let views = fixture.views();
        let (_, lib) = &views[0];
        let ids = fixture.symbol_ids();
        let members = impl_members(&fixture.index, &ids, lib);
        let trait_area = fixture.id("0.1.0 Shape#area().");
        for implementing in ["impl#[Sq][Shape]area().", "impl#[Circle][Shape]area()."] {
            let member = fixture.id(&format!("0.1.0 {implementing}"));
            assert!(
                members.contains(&(member, ImplRole::Trait(Some(trait_area)))),
                "{implementing}: {members:?}"
            );
        }
    }

    #[test]
    fn impl_headers_are_recognized_from_their_line() {
        let fixture = fixture();
        let views = fixture.views();
        let (_, lib) = &views[0];
        let at = |line: u32, col: u32| {
            (0..lib.doc.occurrences.len())
                .find(|&i| {
                    let range = lib.occurrence(i).range;
                    (range.start_line, range.start_col) == (line, col)
                })
                .unwrap()
        };
        // `impl Shape for Sq {` (zero-based line 9).
        let trait_at = at(9, 5);
        let self_at = at(9, 15);
        assert_eq!(
            header_role(lib, trait_at),
            Some(HeaderRole::Trait(Some(self_at)))
        );
        assert_eq!(header_role(lib, self_at), Some(HeaderRole::Other));
        assert_eq!(header_role(lib, at(48, 4)), None);
    }

    #[test]
    fn impl_prefixes() {
        let (prefix, params, member) =
            impl_prefix("rust-analyzer cargo fx 0.1.0 impl#[Sq][Shape]area().").unwrap();
        assert_eq!(prefix, "rust-analyzer cargo fx 0.1.0 impl#[Sq][Shape]");
        assert_eq!(params, 2);
        assert_eq!(member, "area().");
        assert_eq!(impl_trait_name(prefix).as_deref(), Some("Shape"));
        let (_, params, _) = impl_prefix("rust-analyzer cargo fx 0.1.0 a/impl#[Sq]new().").unwrap();
        assert_eq!(params, 1);
        assert!(impl_prefix("rust-analyzer cargo fx 0.1.0 impl#[Sq]").is_none());
        assert!(impl_prefix("rust-analyzer cargo fx 0.1.0 Shape#area().").is_none());
        assert!(has_word("impl<T> ", "impl") && !has_word("simple ", "impl"));
        assert_eq!(skip_generics("<A, B<C>> for X"), " for X");
    }

    #[test]
    fn last_segments() {
        assert_eq!(last_segment("self.cache.get"), "get");
        assert_eq!(last_segment("Vec::<u8>::new"), "new");
        assert_eq!(last_segment("collect::<Vec<_>>"), "collect");
        assert_eq!(last_segment("a::b"), "b");
        assert_eq!(last_segment("helper"), "helper");
        assert_eq!(last_segment("r#type"), "type");
    }
}
