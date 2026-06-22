//! Context Builder
//!
//! Builds rich context for tasks by combining FTS search with graph traversal.
//! Outputs structured context ready to inject into Claude.
//!
//! Ported from `src/context/index.ts`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::json;

use super::formatter::{format_context_as_json, format_context_as_markdown};
use super::markers::LOW_CONFIDENCE_MARKER;
use crate::db::QueryBuilder;
use crate::error::{Result, log_debug};
use crate::graph::GraphTraverser;
use crate::search::{
    extract_search_terms,
    extract_search_terms_opts,
    get_stem_variants,
    is_distinctive_identifier,
    is_test_file,
    score_path_relevance,
};
use crate::types::{
    BuildContextOptions,
    CodeBlock,
    Confidence,
    ContextFormat,
    Direction,
    Edge,
    EdgeKind,
    FindRelevantContextOptions,
    Node,
    NodeKind,
    SearchResult,
    Subgraph,
    TaskContext,
    TaskContextStats,
    TaskInput,
    TraversalOptions,
};
use crate::utils::validate_path_within_root;

// =============================================================================
// Symbol extraction from natural-language queries
// =============================================================================

// JS `\b` is the ASCII word boundary — `(?-u:\b)` in the regex crate.
static CAMEL_CASE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)([A-Z][a-z]+(?:[A-Z][a-z]*)*|[a-z]+(?:[A-Z][a-z]*)+)(?-u:\b)").unwrap()
});
static SNAKE_CASE_RE: LazyLock<Regex> = LazyLock::new(|| {
    // The TS pattern has the /i flag.
    Regex::new(r"(?-u:\b)((?i:[a-z][a-z0-9]*(?:_[a-z0-9]+)+))(?-u:\b)").unwrap()
});
static SCREAMING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)([A-Z][A-Z0-9]*(?:_[A-Z0-9]+)+)(?-u:\b)").unwrap());
static ACRONYM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)([A-Z]{2,})(?-u:\b)").unwrap());
static DOT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?-u:\b)([a-zA-Z][a-zA-Z0-9]*(?:\.[a-zA-Z][a-zA-Z0-9]*)+)(?-u:\b)").unwrap()
});
static LOWERCASE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)([a-z][a-z0-9]{2,})(?-u:\b)").unwrap());

/// Common English words that aren't likely symbol names.
const COMMON_WORDS: [&str; 153] = [
    "the",
    "and",
    "for",
    "with",
    "from",
    "this",
    "that",
    "have",
    "been",
    "will",
    "would",
    "could",
    "should",
    "does",
    "done",
    "make",
    "made",
    "use",
    "used",
    "using",
    "work",
    "works",
    "find",
    "found",
    "show",
    "call",
    "called",
    "calling",
    "get",
    "set",
    "add",
    "all",
    "any",
    "how",
    "what",
    "when",
    "where",
    "which",
    "who",
    "why",
    "not",
    "but",
    "are",
    "was",
    "were",
    "has",
    "had",
    "its",
    "can",
    "did",
    "may",
    "also",
    "into",
    "than",
    "then",
    "them",
    "each",
    "other",
    "some",
    "such",
    "only",
    "same",
    "about",
    "after",
    "before",
    "between",
    "through",
    "during",
    "without",
    "again",
    "further",
    "once",
    "here",
    "there",
    "both",
    "just",
    "more",
    "most",
    "very",
    "being",
    "having",
    "doing",
    "system",
    "need",
    "needs",
    "want",
    "wants",
    "like",
    "look",
    "change",
    "changes",
    "changed",
    "changing",
    // Common English nouns/verbs that match thousands of unrelated code symbols
    "layer",
    "handle",
    "handles",
    "handling",
    "incoming",
    "outgoing",
    "data",
    "flow",
    "flows",
    "level",
    "levels",
    "request",
    "requests",
    "response",
    "responses",
    "implement",
    "implements",
    "implementation",
    "interface",
    "interfaces",
    "class",
    "classes",
    "method",
    "methods",
    "trigger",
    "triggers",
    "affected",
    "affect",
    "affects",
    "else",
    "code",
    "failing",
    "failed",
    "silently",
    "decide",
    "decides",
    "return",
    "returns",
    "returned",
    "take",
    "takes",
    "taken",
    "check",
    "checks",
    "checked",
    "create",
    "creates",
    "created",
    "read",
    "reads",
    "write",
    "writes",
    "written",
    "start",
    "starts",
    "stop",
    "stops",
    "run",
    "runs",
    "running",
];

/// Extract likely symbol names from a natural language query
///
/// Identifies potential code symbols using patterns:
/// - CamelCase: UserService, signInWithGoogle
/// - snake_case: user_service, sign_in
/// - SCREAMING_SNAKE: MAX_RETRIES
/// - dot.notation: app.isPackaged (extracts both sides)
/// - Single words that look like identifiers (no spaces, not common English words)
fn extract_symbols_from_query(query: &str) -> Vec<String> {
    let mut symbols: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let add = |symbols: &mut Vec<String>, seen: &mut HashSet<String>, s: &str| {
        if seen.insert(s.to_string()) {
            symbols.push(s.to_string());
        }
    };

    // Extract CamelCase identifiers (2+ chars, starts with letter)
    for m in CAMEL_CASE_RE.captures_iter(query) {
        let s = &m[1];
        if s.encode_utf16().count() >= 2 {
            add(&mut symbols, &mut seen, s);
        }
    }

    // Extract snake_case identifiers
    for m in SNAKE_CASE_RE.captures_iter(query) {
        let s = &m[1];
        if s.encode_utf16().count() >= 3 {
            add(&mut symbols, &mut seen, s);
        }
    }

    // Extract SCREAMING_SNAKE_CASE
    for m in SCREAMING_RE.captures_iter(query) {
        add(&mut symbols, &mut seen, &m[1]);
    }

    // Extract ALL_CAPS acronyms (2+ chars, e.g., REST, HTTP, LRU, API)
    for m in ACRONYM_RE.captures_iter(query) {
        add(&mut symbols, &mut seen, &m[1]);
    }

    // Extract dot.notation and split into parts (e.g., "app.isPackaged" -> ["app", "isPackaged"])
    for m in DOT_RE.captures_iter(query) {
        let full = m[1].to_string();
        add(&mut symbols, &mut seen, &full);
        for part in full.split('.') {
            if part.encode_utf16().count() >= 2 {
                add(&mut symbols, &mut seen, part);
            }
        }
    }

    // Extract plain lowercase identifiers (3+ chars, not already matched)
    // Catches symbol names like "undo", "redo", "history", "render", "parse"
    for m in LOWERCASE_RE.captures_iter(query) {
        add(&mut symbols, &mut seen, &m[1]);
    }

    // Filter out common English words that aren't likely symbol names
    static COMMON: LazyLock<HashSet<&'static str>> =
        LazyLock::new(|| COMMON_WORDS.iter().copied().collect());
    symbols
        .into_iter()
        .filter(|s| !COMMON.contains(s.to_lowercase().as_str()))
        .collect()
}

// =============================================================================
// Default options
// =============================================================================

/// Default options for context building
///
/// Tuned for minimal context usage while still providing useful results:
/// - Fewer nodes and code blocks by default
/// - Smaller code block size limit
/// - Shallower traversal
struct ResolvedBuildOptions {
    max_nodes: usize,           // 20 — reduced from 50; most tasks don't need 50 symbols
    max_code_blocks: usize,     // 5 — reduced from 10; only show most relevant code
    max_code_block_size: usize, // 1500 — reduced from 2000
    include_code: bool,
    format: ContextFormat,
    search_limit: usize,  // 3 — reduced from 5; fewer entry points
    traversal_depth: u32, // 1 — reduced from 2; shallower graph expansion
    min_score: f64,
}

fn resolve_build_options(options: &BuildContextOptions) -> ResolvedBuildOptions {
    ResolvedBuildOptions {
        max_nodes: options.max_nodes.unwrap_or(20),
        max_code_blocks: options.max_code_blocks.unwrap_or(5),
        max_code_block_size: options.max_code_block_size.unwrap_or(1500),
        include_code: options.include_code.unwrap_or(true),
        format: options.format.unwrap_or(ContextFormat::Markdown),
        search_limit: options.search_limit.unwrap_or(3),
        traversal_depth: options.traversal_depth.unwrap_or(1),
        min_score: options.min_score.unwrap_or(0.3),
    }
}

/// Node kinds that provide high information value in context results.
/// Imports/exports are excluded because they have near-zero information density -
/// they tell you something exists, not how it works.
const HIGH_VALUE_NODE_KINDS: [NodeKind; 14] = [
    NodeKind::Function,
    NodeKind::Method,
    NodeKind::Class,
    NodeKind::Interface,
    NodeKind::TypeAlias,
    NodeKind::Struct,
    NodeKind::Trait,
    NodeKind::Component,
    NodeKind::Route,
    NodeKind::Variable,
    NodeKind::Constant,
    NodeKind::Enum,
    NodeKind::Module,
    NodeKind::Namespace,
];

/// Default options for finding relevant context
struct ResolvedFindOptions {
    search_limit: usize,
    traversal_depth: u32,
    max_nodes: usize,
    min_score: f64,
    edge_kinds: Vec<EdgeKind>,
    node_kinds: Vec<NodeKind>,
}

fn resolve_find_options(options: &FindRelevantContextOptions) -> ResolvedFindOptions {
    ResolvedFindOptions {
        search_limit: options.search_limit.unwrap_or(3),
        traversal_depth: options.traversal_depth.unwrap_or(1),
        max_nodes: options.max_nodes.unwrap_or(20),
        min_score: options.min_score.unwrap_or(0.3),
        edge_kinds: options.edge_kinds.clone().unwrap_or_default(),
        // Filter out imports/exports by default
        node_kinds: options
            .node_kinds
            .clone()
            .unwrap_or_else(|| HIGH_VALUE_NODE_KINDS.to_vec()),
    }
}

// =============================================================================
// Small helpers (JS semantics)
// =============================================================================

/// JS `String.prototype.length` (UTF-16 code units).
fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// JS `str.slice(0, n)` over UTF-16 units, clamped to a char boundary.
fn utf16_slice_prefix(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut units = 0usize;
    for c in s.chars() {
        let len = c.len_utf16();
        if units + len > n {
            break;
        }
        units += len;
        out.push(c);
    }
    out
}

/// `sym.charAt(0).toUpperCase() + sym.slice(1).toLowerCase()`
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => {
            let mut out: String = c.to_uppercase().collect();
            out.push_str(&chars.as_str().to_lowercase());
            out
        }
    }
}

/// Node.js `path.dirname` for the forward-slash paths stored in the DB.
fn posix_dirname(p: &str) -> &str {
    match p.rfind('/') {
        None => ".",
        Some(0) => "/",
        Some(i) => &p[..i],
    }
}

/// JS truthiness for a JSON value (used for edge metadata checks).
fn js_truthy(v: Option<&serde_json::Value>) -> bool {
    match v {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(b)) => *b,
        Some(serde_json::Value::String(s)) => !s.is_empty(),
        Some(serde_json::Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(_) => true, // arrays/objects are truthy
    }
}

/// JS `String(value)` for the JSON values that appear in edge metadata.
fn js_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Null => "null".to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Number(n) => n.to_string(),
        other => other.to_string(),
    }
}

/// Stable descending sort by score (JS `sort((a, b) => b.score - a.score)`).
fn sort_by_score_desc(results: &mut [SearchResult]) {
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
}

fn edge_exists(edges: &[Edge], edge: &Edge) -> bool {
    edges
        .iter()
        .any(|e| e.source == edge.source && e.target == edge.target && e.kind == edge.kind)
}

/// Insertion-ordered node map — stands in for the TS `Map<string, Node>`,
/// whose insertion order the algorithm depends on (`Subgraph.nodes` is a
/// plain `HashMap` and cannot carry it).
#[derive(Default)]
struct OrderedNodes {
    map: HashMap<String, Node>,
    order: Vec<String>,
}

impl OrderedNodes {
    fn contains(&self, id: &str) -> bool {
        self.map.contains_key(id)
    }

    fn get(&self, id: &str) -> Option<&Node> {
        self.map.get(id)
    }

    fn insert(&mut self, node: Node) {
        if !self.map.contains_key(&node.id) {
            self.order.push(node.id.clone());
            self.map.insert(node.id.clone(), node);
        }
    }

    fn remove(&mut self, id: &str) {
        if self.map.remove(id).is_some() {
            if let Some(pos) = self.order.iter().position(|x| x == id) {
                self.order.remove(pos);
            }
        }
    }

    fn len(&self) -> usize {
        self.map.len()
    }

    fn ids(&self) -> Vec<String> {
        self.order.clone()
    }

    fn iter(&self) -> impl Iterator<Item = &Node> {
        self.order.iter().filter_map(|id| self.map.get(id))
    }
}

/// Reconstruct a deterministic insertion order for a `Subgraph` returned by
/// the graph layer (whose `nodes` HashMap dropped the TS Map order): roots
/// first, then edge endpoints in edge order (BFS discovery order), then any
/// stragglers sorted deterministically.
fn ordered_ids_from_subgraph(subgraph: &Subgraph) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for id in &subgraph.roots {
        if subgraph.nodes.contains_key(id) && seen.insert(id.as_str()) {
            out.push(id.clone());
        }
    }
    for e in &subgraph.edges {
        for id in [&e.source, &e.target] {
            if subgraph.nodes.contains_key(id) && seen.insert(id.as_str()) {
                out.push(id.clone());
            }
        }
    }
    let mut rest: Vec<&Node> = subgraph
        .nodes
        .values()
        .filter(|n| !seen.contains(n.id.as_str()))
        .collect();
    rest.sort_by(|a, b| {
        a.file_path
            .cmp(&b.file_path)
            .then(a.start_line.cmp(&b.start_line))
            .then(a.name.cmp(&b.name))
            .then(a.id.cmp(&b.id))
    });
    out.extend(rest.into_iter().map(|n| n.id.clone()));
    out
}

// =============================================================================
// Context Builder
// =============================================================================

/// Context Builder
///
/// Coordinates semantic search and graph traversal to build
/// comprehensive context for tasks.
pub struct ContextBuilder {
    project_root: PathBuf,
    queries: Rc<QueryBuilder>,
    traverser: GraphTraverser,
}

impl ContextBuilder {
    pub fn new(
        project_root: impl Into<PathBuf>,
        queries: Rc<QueryBuilder>,
        traverser: GraphTraverser,
    ) -> Self {
        ContextBuilder {
            project_root: project_root.into(),
            queries,
            traverser,
        }
    }

    /// Build context for a task
    ///
    /// Pipeline:
    /// 1. Parse task input (string or {title, description})
    /// 2. Run semantic search to find entry points
    /// 3. Expand graph around entry points
    /// 4. Extract code blocks for key nodes
    /// 5. Format output for Claude
    ///
    /// Returns the formatted string (markdown or JSON, per
    /// `options.format`; default markdown). The TS `TaskContext` raw-object
    /// return path is unreachable with the two-variant format enum — use
    /// [`ContextBuilder::build_task_context`] for the structured form.
    pub fn build_context(
        &self,
        input: &TaskInput,
        options: &BuildContextOptions,
    ) -> Result<String> {
        let opts = resolve_build_options(options);
        let (context, _node_order) = self.build_task_context_inner(input, &opts)?;

        // Return formatted output
        match opts.format {
            ContextFormat::Markdown => {
                let mut out = format_context_as_markdown(&context);
                out.push_str(&self.build_call_paths_section(&context.subgraph));
                if context.subgraph.confidence == Some(Confidence::Low) {
                    out.push_str(&self.build_low_confidence_note(&context.entry_points));
                }
                Ok(out)
            }
            ContextFormat::Json => Ok(format_context_as_json(&context)),
        }
    }

    /// Structured (unformatted) variant of [`ContextBuilder::build_context`]
    /// — the TS `buildContext` returns the raw `TaskContext` when no format
    /// is given; Rust callers that want the struct use this.
    pub fn build_task_context(
        &self,
        input: &TaskInput,
        options: &BuildContextOptions,
    ) -> Result<TaskContext> {
        let opts = resolve_build_options(options);
        Ok(self.build_task_context_inner(input, &opts)?.0)
    }

    fn build_task_context_inner(
        &self,
        input: &TaskInput,
        opts: &ResolvedBuildOptions,
    ) -> Result<(TaskContext, Vec<String>)> {
        // Parse input (JS: `${title}${description ? `: ${description}` : ''}`)
        let query = match input {
            TaskInput::Text(s) => s.clone(),
            TaskInput::Titled { title, description } => match description.as_deref() {
                Some(d) if !d.is_empty() => format!("{title}: {d}"),
                _ => title.clone(),
            },
        };

        // Find relevant context (semantic search + graph expansion)
        let (subgraph, node_order) = self.find_relevant_context_inner(
            &query,
            &ResolvedFindOptions {
                search_limit: opts.search_limit,
                traversal_depth: opts.traversal_depth,
                max_nodes: opts.max_nodes,
                min_score: opts.min_score,
                edge_kinds: Vec::new(),
                node_kinds: HIGH_VALUE_NODE_KINDS.to_vec(),
            },
        )?;

        // Get entry points (nodes from semantic search)
        let entry_points = Self::get_entry_points(&subgraph);

        // Extract code blocks for key nodes
        let code_blocks = if opts.include_code {
            self.extract_code_blocks(
                &subgraph,
                &node_order,
                opts.max_code_blocks,
                opts.max_code_block_size,
            )
        } else {
            Vec::new()
        };

        // Get related files
        let related_files = Self::get_related_files(&subgraph);

        // Generate summary
        let summary = Self::generate_summary(&query, &subgraph, &entry_points);

        // Calculate stats
        let stats = TaskContextStats {
            node_count: subgraph.nodes.len(),
            edge_count: subgraph.edges.len(),
            file_count: related_files.len(),
            code_block_count: code_blocks.len(),
            total_code_size: code_blocks.iter().map(|b| utf16_len(&b.content)).sum(),
        };

        let context = TaskContext {
            query,
            subgraph,
            entry_points,
            code_blocks,
            related_files,
            summary,
            stats,
        };

        Ok((context, node_order))
    }

    /// Honest handoff appended when retrieval confidence is low (the query matched
    /// mostly common words). Instead of the usual "this covers the surface" framing
    /// — which, when wrong, sends the agent off to Read/Grep — it admits the
    /// uncertainty and routes the agent to the precise tools (explore with real
    /// symbol names, search, or files to browse the closest areas we *did* surface).
    fn build_low_confidence_note(&self, entry_points: &[Node]) -> String {
        let mut dirs: Vec<&str> = Vec::new();
        let mut seen: HashSet<&str> = HashSet::new();
        for n in entry_points {
            let dir = match n.file_path.rfind('/') {
                Some(slash) if slash > 0 => &n.file_path[..slash],
                _ => n.file_path.as_str(),
            };
            if seen.insert(dir) {
                dirs.push(dir);
            }
            if dirs.len() >= 4 {
                break;
            }
        }
        let dir_line = if !dirs.is_empty() {
            format!(
                "\n- `codegraph_files` a likely area: {}",
                dirs.iter()
                    .map(|d| format!("`{d}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            String::new()
        };
        format!(
            "\n\n{LOW_CONFIDENCE_MARKER}\n\n\
             This query matched mostly on common words, so the entry points above may \
             be off-target — treat them as a starting point, not a complete answer. \
             For a reliable result:\n\
             - `codegraph_explore` with the **exact symbol names** you are after \
             (class / function / method names), or\n\
             - `codegraph_search <name>` for one specific symbol\
             {dir_line}\n\nDo not assume the list above is comprehensive."
        )
    }

    /// Surface short call-paths among the symbols this context already found,
    /// derived in-memory from the subgraph's `calls` edges (no extra queries).
    ///
    /// This bakes the value of path-finding INTO the always-loaded `context` tool.
    /// Agents reliably read context's output but do NOT discover/adopt a standalone
    /// trace tool (in deferred-MCP harnesses they only ToolSearch-select tools they
    /// already know). Delivering the flow here means "how does X reach Y" is
    /// answered without the agent needing to find, load, or choose a new tool.
    /// Chains stop where the static call graph ends (e.g. dynamic dispatch) — that
    /// truncation is honest, and the agent can codegraph_node the last hop to bridge.
    fn build_call_paths_section(&self, subgraph: &Subgraph) -> String {
        // Adjacency (insertion-ordered, mirrors the TS Map)
        let mut adj_order: Vec<&str> = Vec::new();
        let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
        for e in &subgraph.edges {
            if e.kind != EdgeKind::Calls {
                continue;
            }
            if !subgraph.nodes.contains_key(&e.source) || !subgraph.nodes.contains_key(&e.target) {
                continue;
            }
            match adj.get_mut(e.source.as_str()) {
                Some(list) => list.push(e.target.as_str()),
                None => {
                    adj_order.push(e.source.as_str());
                    adj.insert(e.source.as_str(), vec![e.target.as_str()]);
                }
            }
        }
        if adj.is_empty() {
            return String::new();
        }

        const MAX_HOPS: usize = 6;
        let mut chains: Vec<Vec<String>> = Vec::new();
        let mut budget: i64 = 2000; // bound DFS work on dense subgraphs

        fn dfs(
            adj: &HashMap<&str, Vec<&str>>,
            id: &str,
            path: &mut Vec<String>,
            seen: &mut HashSet<String>,
            chains: &mut Vec<Vec<String>>,
            budget: &mut i64,
        ) {
            let old = *budget;
            *budget -= 1;
            if old <= 0 {
                return;
            }
            let next: Vec<&str> = adj
                .get(id)
                .map(|l| l.iter().filter(|t| !seen.contains(**t)).copied().collect())
                .unwrap_or_default();
            if next.is_empty() || path.len() >= MAX_HOPS {
                if path.len() >= 3 {
                    chains.push(path.clone()); // >=3 nodes = a real flow, not a single call
                }
                return;
            }
            for t in next {
                seen.insert(t.to_string());
                path.push(t.to_string());
                dfs(adj, t, path, seen, chains, budget);
                path.pop();
                seen.remove(t);
            }
        }

        let starts: Vec<&str> = if !subgraph.roots.is_empty() {
            subgraph
                .roots
                .iter()
                .map(|s| s.as_str())
                .filter(|id| adj.contains_key(id))
                .take(5)
                .collect()
        } else {
            adj_order.iter().copied().take(5).collect()
        };
        for s in starts {
            let mut path = vec![s.to_string()];
            let mut seen: HashSet<String> = HashSet::new();
            seen.insert(s.to_string());
            dfs(&adj, s, &mut path, &mut seen, &mut chains, &mut budget);
        }
        if chains.is_empty() {
            return String::new();
        }

        // Keep only chains that connect TWO OR MORE query-relevant symbols (roots).
        // A chain from a root into an arbitrary callee (render → onMagicFrameGenerate)
        // is structurally valid but tangential to the question; requiring ≥2 roots
        // keeps the chain anchored to what the user actually asked about. Rank by
        // #roots then length, and drop any that are a sub-path of a longer kept chain.
        let root_set: HashSet<&str> = subgraph.roots.iter().map(|s| s.as_str()).collect();
        let root_count =
            |c: &[String]| c.iter().filter(|id| root_set.contains(id.as_str())).count();
        let mut relevant: Vec<Vec<String>> =
            chains.into_iter().filter(|c| root_count(c) >= 2).collect();
        relevant.sort_by(|a, b| {
            root_count(b)
                .cmp(&root_count(a))
                .then(b.len().cmp(&a.len()))
        });
        let mut kept: Vec<Vec<String>> = Vec::new();
        for c in relevant {
            let key = c.join(">");
            if kept.iter().any(|k| k.join(">").contains(&key)) {
                continue;
            }
            kept.push(c);
            if kept.len() >= 3 {
                break;
            }
        }
        if kept.is_empty() {
            return String::new();
        }
        let name = |id: &str| -> String {
            subgraph
                .nodes
                .get(id)
                .map(|n| n.name.clone())
                .unwrap_or_else(|| id.to_string())
        };

        // Synthesized (dynamic-dispatch) hops are real `calls` edges but invisible to
        // static parsing — mark them inline so the agent sees WHERE the callback was
        // wired up (`registered @file:line`) instead of grepping for it. Keyed by
        // "source>target".
        let mut synth_by_pair: HashMap<String, String> = HashMap::new();
        for e in &subgraph.edges {
            if e.kind != EdgeKind::Calls
                || e.provenance != Some(crate::types::Provenance::Heuristic)
            {
                continue;
            }
            let Some(m) = e.metadata.as_ref() else {
                continue;
            };
            if !js_truthy(m.get("synthesizedBy")) {
                continue;
            }
            let at = match m.get("registeredAt") {
                Some(serde_json::Value::String(s)) => format!(" @{s}"),
                _ => String::new(),
            };
            let synthesized_by = m.get("synthesizedBy").and_then(|v| v.as_str());
            let label = match synthesized_by {
                Some("callback") => {
                    let via = if js_truthy(m.get("via")) {
                        format!("`{}`", js_string(m.get("via").unwrap()))
                    } else {
                        "registrar".to_string()
                    };
                    format!("callback via {via}{at}")
                }
                Some("react-render") => format!("React re-render via setState{at}"),
                Some("jsx-render") => {
                    let via = if js_truthy(m.get("via")) {
                        js_string(m.get("via").unwrap())
                    } else {
                        "child".to_string()
                    };
                    format!("renders <{via}>")
                }
                Some("vue-handler") => {
                    let event = if js_truthy(m.get("event")) {
                        js_string(m.get("event").unwrap())
                    } else {
                        "event".to_string()
                    };
                    format!("Vue @{event} handler")
                }
                _ => {
                    let event = if js_truthy(m.get("event")) {
                        format!("`{}`", js_string(m.get("event").unwrap()))
                    } else {
                        String::new()
                    };
                    format!("event {event}{at}")
                }
            };
            synth_by_pair.insert(format!("{}>{}", e.source, e.target), label);
        }
        let render_chain = |c: &[String]| -> String {
            let mut s = name(&c[0]);
            for i in 1..c.len() {
                match synth_by_pair.get(&format!("{}>{}", c[i - 1], c[i])) {
                    Some(synth) => {
                        s.push_str(&format!(" →[{synth}] {}", name(&c[i])));
                    }
                    None => {
                        s.push_str(&format!(" → {}", name(&c[i])));
                    }
                }
            }
            s
        };
        let has_synth = kept.iter().any(|c| {
            (1..c.len()).any(|i| synth_by_pair.contains_key(&format!("{}>{}", c[i - 1], c[i])))
        });
        let mut lines: Vec<String> = vec![
            String::new(),
            "## Call paths".to_string(),
            String::new(),
            "Execution flow among the key symbols (traced through the call graph):".to_string(),
            String::new(),
        ];
        for c in &kept {
            lines.push(format!("- {}", render_chain(c)));
        }
        lines.push(String::new());
        lines.push(if has_synth {
            "_Hops marked `[callback/event …]` are dynamic dispatch bridged by codegraph (with the registration site); the rest are direct calls. codegraph_node any symbol for its body._".to_string()
        } else {
            "_codegraph_node any symbol above for its source + its own callers/callees._".to_string()
        });
        format!("\n{}\n", lines.join("\n"))
    }

    /// Find relevant subgraph for a query
    ///
    /// Uses hybrid search combining exact symbol lookup with semantic search:
    /// 1. Extract potential symbol names from query
    /// 2. Look up exact matches for those symbols (high confidence)
    /// 3. Use semantic search for concept matching
    /// 4. Merge results, prioritizing exact matches
    /// 5. Traverse graph from entry points
    pub fn find_relevant_context(
        &self,
        query: &str,
        options: &FindRelevantContextOptions,
    ) -> Result<Subgraph> {
        let opts = resolve_find_options(options);
        Ok(self.find_relevant_context_inner(query, &opts)?.0)
    }

    #[allow(clippy::too_many_lines)]
    fn find_relevant_context_inner(
        &self,
        query: &str,
        opts: &ResolvedFindOptions,
    ) -> Result<(Subgraph, Vec<String>)> {
        // Start with empty subgraph
        let mut nodes = OrderedNodes::default();
        let mut edges: Vec<Edge> = Vec::new();
        let mut roots: Vec<String> = Vec::new();

        // Handle empty query - return empty subgraph
        if query.trim().is_empty() {
            return Ok((Subgraph::default(), Vec::new()));
        }

        let kinds_filter = || -> Option<Vec<NodeKind>> {
            if opts.node_kinds.is_empty() {
                None
            } else {
                Some(opts.node_kinds.clone())
            }
        };

        // === HYBRID SEARCH ===

        // Step 1: Extract potential symbol names from query
        let symbols_from_query = extract_symbols_from_query(query);
        log_debug(
            "Extracted symbols from query",
            Some(&json!({ "query": query, "symbols": symbols_from_query })),
        );

        // Step 2: Look up exact matches for extracted symbols
        let mut exact_matches: Vec<SearchResult> = Vec::new();
        if !symbols_from_query.is_empty() {
            // Get more results so we can apply co-location boosting before trimming
            match self.queries.find_nodes_by_exact_name(
                &symbols_from_query,
                &crate::types::SearchOptions {
                    limit: Some(opts.search_limit * 5),
                    kinds: kinds_filter(),
                    ..Default::default()
                },
            ) {
                Ok(results) => {
                    exact_matches = results;

                    // Co-location boost: when multiple extracted symbols appear in the same file,
                    // those results are much more likely to be what the user is looking for.
                    // E.g., "scrapeLoop" + "run" both in scrape/scrape.go → boost both.
                    if exact_matches.len() > 1 {
                        // Build a map of files → how many distinct symbol names matched in that file
                        let mut file_symbol_counts: HashMap<&str, HashSet<String>> = HashMap::new();
                        for r in &exact_matches {
                            file_symbol_counts
                                .entry(r.node.file_path.as_str())
                                .or_default()
                                .insert(r.node.name.to_lowercase());
                        }
                        // Boost results in files where multiple query symbols co-occur
                        let boosts: Vec<f64> = exact_matches
                            .iter()
                            .map(|r| {
                                let symbol_count = file_symbol_counts
                                    .get(r.node.file_path.as_str())
                                    .map(|s| s.len())
                                    .unwrap_or(1);
                                if symbol_count > 1 {
                                    (symbol_count - 1) as f64 * 20.0
                                } else {
                                    0.0
                                }
                            })
                            .collect();
                        for (r, boost) in exact_matches.iter_mut().zip(boosts) {
                            r.score += boost;
                        }
                        sort_by_score_desc(&mut exact_matches);
                    }

                    // Trim back to reasonable size
                    exact_matches.truncate(opts.search_limit * 2);
                    log_debug(
                        "Exact symbol matches",
                        Some(&json!({ "count": exact_matches.len() })),
                    );
                }
                Err(error) => {
                    log_debug(
                        "Exact symbol lookup failed",
                        Some(&json!({ "error": error.to_string() })),
                    );
                }
            }
        }

        // Step 2b: Search for extracted symbols as definition (class/interface) prefixes.
        // When the user writes "REST", "bulk", or "allocation", they usually mean classes
        // like RestController, BulkRequest, AllocationService — not nodes named exactly that.
        // Also tries stem variants: "caching" → "cache" finds Cache, CacheBuilder.
        let definition_kinds: Vec<NodeKind> = vec![
            NodeKind::Class,
            NodeKind::Interface,
            NodeKind::Struct,
            NodeKind::Trait,
            NodeKind::Protocol,
            NodeKind::Enum,
            NodeKind::TypeAlias,
        ];
        if !symbols_from_query.is_empty() {
            // Expand symbols with stem variants for broader definition matching
            let mut expanded_symbols: Vec<String> = Vec::new();
            let mut expanded_seen: HashSet<String> = HashSet::new();
            for sym in &symbols_from_query {
                if expanded_seen.insert(sym.clone()) {
                    expanded_symbols.push(sym.clone());
                }
            }
            for sym in &symbols_from_query {
                for variant in get_stem_variants(sym) {
                    if expanded_seen.insert(variant.clone()) {
                        expanded_symbols.push(variant);
                    }
                }
            }
            for sym in &expanded_symbols {
                // Title-case the symbol: "REST" → "Rest", "bulk" → "Bulk", "allocation" → "Allocation"
                let title_cased = title_case(sym);
                if &title_cased == sym {
                    continue; // already title-case (e.g., "Engine") — handled by exact match
                }
                // Fetch more results since popular prefixes have many matches
                let prefix_results = self.queries.search_nodes(
                    &title_cased,
                    &crate::types::SearchOptions {
                        limit: Some(30),
                        kinds: Some(definition_kinds.clone()),
                        ..Default::default()
                    },
                )?;
                let mut matched: Vec<SearchResult> = Vec::new();
                for r in prefix_results {
                    if r.node
                        .name
                        .to_lowercase()
                        .starts_with(&title_cased.to_lowercase())
                    {
                        // Favor shorter names: "AllocationService" (18 chars) over
                        // "AllocationBalancingRoundMetrics" (31 chars). Core classes tend
                        // to have concise names; test/helper classes are verbose.
                        let brevity_bonus = (10.0
                            - (utf16_len(&r.node.name) as f64 - utf16_len(&title_cased) as f64)
                                / 3.0)
                            .max(0.0);
                        matched.push(SearchResult {
                            score: r.score + 15.0 + brevity_bonus,
                            ..r
                        });
                    }
                }
                sort_by_score_desc(&mut matched);
                for r in matched.into_iter().take(opts.search_limit) {
                    let existing = exact_matches.iter().any(|e| e.node.id == r.node.id);
                    if !existing {
                        exact_matches.push(r);
                    }
                }
            }
            sort_by_score_desc(&mut exact_matches);
            exact_matches.truncate(opts.search_limit * 3);
        }

        // Step 3: Run text search for natural language term matching
        // This catches file-name and node-name matches that semantic search may miss,
        // which is critical for template-heavy codebases (e.g., Liquid/Shopify themes)
        // where file names are the primary identifiers.
        let mut text_results: Vec<SearchResult> = Vec::new();
        let text_search = || -> Result<Vec<SearchResult>> {
            let mut out: Vec<SearchResult> = Vec::new();
            let search_terms = extract_search_terms(query);
            if !search_terms.is_empty() {
                // Search each term individually to get broader coverage,
                // then boost results that match multiple terms
                let mut term_order: Vec<String> = Vec::new();
                let mut term_results_map: HashMap<String, (SearchResult, usize)> = HashMap::new();
                // When no explicit kind filter is set, exclude imports — they flood FTS
                // results with qualified name matches (e.g., "REST" matches 445K import paths)
                // but are almost never what exploration queries want.
                let search_kinds: Vec<NodeKind> = if !opts.node_kinds.is_empty() {
                    opts.node_kinds.clone()
                } else {
                    vec![
                        NodeKind::File,
                        NodeKind::Module,
                        NodeKind::Class,
                        NodeKind::Struct,
                        NodeKind::Interface,
                        NodeKind::Trait,
                        NodeKind::Protocol,
                        NodeKind::Function,
                        NodeKind::Method,
                        NodeKind::Property,
                        NodeKind::Field,
                        NodeKind::Variable,
                        NodeKind::Constant,
                        NodeKind::Enum,
                        NodeKind::EnumMember,
                        NodeKind::TypeAlias,
                        NodeKind::Namespace,
                        NodeKind::Export,
                        NodeKind::Route,
                        NodeKind::Component,
                    ]
                };
                for term in &search_terms {
                    let term_results = self.queries.search_nodes(
                        term,
                        &crate::types::SearchOptions {
                            limit: Some(opts.search_limit * 2),
                            kinds: Some(search_kinds.clone()),
                            ..Default::default()
                        },
                    )?;
                    for r in term_results {
                        match term_results_map.get_mut(&r.node.id) {
                            Some((existing, term_hits)) => {
                                *term_hits += 1;
                                existing.score = existing.score.max(r.score);
                            }
                            None => {
                                term_order.push(r.node.id.clone());
                                term_results_map.insert(r.node.id.clone(), (r, 1));
                            }
                        }
                    }
                }
                // Boost results matching multiple terms and sort
                for id in &term_order {
                    if let Some((result, term_hits)) = term_results_map.remove(id) {
                        out.push(SearchResult {
                            score: result.score + (term_hits - 1) as f64 * 5.0,
                            ..result
                        });
                    }
                }
                sort_by_score_desc(&mut out);
                out.truncate(opts.search_limit * 2);
            }
            Ok(out)
        };
        match text_search() {
            Ok(results) => {
                text_results = results;
                log_debug(
                    "Text search results",
                    Some(&json!({ "count": text_results.len() })),
                );
            }
            Err(error) => {
                log_debug(
                    "Text search failed",
                    Some(&json!({ "query": query, "error": error.to_string() })),
                );
            }
        }

        // Step 4: Merge results, taking the max score when duplicates appear
        // across search channels. Exact matches may have lower scores than FTS
        // results for the same node — use the best score from any channel.
        let mut result_index: HashMap<String, usize> = HashMap::new();
        let mut search_results: Vec<SearchResult> = Vec::new();

        // Add exact matches first
        for result in &exact_matches {
            match result_index.get(&result.node.id) {
                Some(&i) => {
                    search_results[i].score = search_results[i].score.max(result.score);
                }
                None => {
                    result_index.insert(result.node.id.clone(), search_results.len());
                    search_results.push(result.clone());
                }
            }
        }

        // Add text search results, upgrading scores for duplicates
        for result in text_results {
            match result_index.get(&result.node.id) {
                Some(&i) => {
                    search_results[i].score = search_results[i].score.max(result.score);
                }
                None => {
                    result_index.insert(result.node.id.clone(), search_results.len());
                    search_results.push(result);
                }
            }
        }

        let query_lower = query.to_lowercase();
        let is_test_query = query_lower.contains("test") || query_lower.contains("spec");

        // Deprioritize test files early so they don't take multi-term boost slots
        if !is_test_query {
            for result in &mut search_results {
                if is_test_file(&result.node.file_path) {
                    result.score *= 0.3;
                }
            }
        }

        // Iter7 — Core-directory boost. On projects with one file that holds
        // the dense majority of internal call edges (e.g. sinatra's
        // `lib/sinatra/base.rb` at 85% of all in-file edges), the agent's
        // task usually asks about the framework's core. Without this boost,
        // ranking favors small focused extension files (e.g. text search
        // picks `sinatra-contrib/lib/sinatra/multi_route.rb`'s 10-line
        // `route` method over `base.rb`'s `route!` because the extension
        // file's `route` matches the query verbatim AND the file is small,
        // dwarfing the longer name `route!` in a 1500-line file). Boost
        // results that share a directory prefix with the dominant file's
        // directory so the core file's siblings outrank sibling-package
        // extensions.
        if let Ok(Some(dominant)) = self.queries.get_dominant_file() {
            if dominant.edge_count >= 3 * dominant.next_edge_count {
                // Take the directory of the dominant file (everything up to the
                // last slash). For `lib/sinatra/base.rb` → `lib/sinatra/`.
                if let Some(slash) = dominant.file_path.rfind('/') {
                    if slash > 0 {
                        let core_dir = &dominant.file_path[..slash + 1];
                        for result in &mut search_results {
                            if result.node.file_path.starts_with(core_dir) {
                                result.score += 25.0;
                            }
                        }
                    }
                }
            }
        }
        // (SQL failure — fall through, scoring works without the boost)

        // Step 5a: Multi-term co-occurrence re-ranking (applied BEFORE truncation).
        // For multi-word queries like "search execution from request to shard",
        // nodes matching 2+ query terms in their name or path are far more relevant
        // than nodes matching just one generic term. Without this, "ExecutionUtils"
        // (matches only "execution") fills budget slots meant for "ShardSearchRequest"
        // (matches "shard" + "search" + "request").
        let query_terms_for_boost = extract_search_terms(query);
        if query_terms_for_boost.len() >= 2 {
            // Group terms that are substrings of each other (stem variants of the same
            // root word). "indexed", "indexe", "index" should count as ONE concept match,
            // not three. Without this, stem variants inflate matchCount and give false
            // multi-term boosts to symbols matching one root word multiple times.
            let mut term_groups: Vec<Vec<String>> = Vec::new();
            let mut sorted = query_terms_for_boost.clone();
            sorted.sort_by_key(|t| std::cmp::Reverse(utf16_len(t)));
            let mut assigned: HashSet<String> = HashSet::new();
            for term in &sorted {
                if assigned.contains(term) {
                    continue;
                }
                let mut group = vec![term.clone()];
                assigned.insert(term.clone());
                for other in &sorted {
                    if assigned.contains(other) {
                        continue;
                    }
                    if term.contains(other.as_str()) || other.contains(term.as_str()) {
                        group.push(other.clone());
                        assigned.insert(other.clone());
                    }
                }
                term_groups.push(group);
            }

            // Build a set of exact-match node IDs so we can exempt them from dampening.
            // When the query is "LiveEditMode DevServerPreview", these are specific
            // symbols the user asked for — dampening them because they only match 1
            // term group is counter-productive.
            let exact_match_ids: HashSet<&str> =
                exact_matches.iter().map(|r| r.node.id.as_str()).collect();

            // ...but only exempt exact matches the user *named as an identifier*
            // (camelCase/snake_case/acronym). A plain dictionary word that happens to
            // exact-match an unrelated symbol — query "flat object" → a constant named
            // FLAT — must NOT be exempt, or the +exact-name bonus floats it to the top
            // of a prose query with zero corroboration from any other term. Classify by
            // the QUERY token (what the user typed), not the matched symbol's name.
            let distinctive_tokens: HashSet<String> = symbols_from_query
                .iter()
                .filter(|s| is_distinctive_identifier(s))
                .map(|s| s.to_lowercase())
                .collect();
            let distinctive_exact_match_ids: HashSet<&str> = exact_matches
                .iter()
                .filter(|r| distinctive_tokens.contains(&r.node.name.to_lowercase()))
                .map(|r| r.node.id.as_str())
                .collect();

            for result in &mut search_results {
                // Check term matches in name (substring) and path DIRECTORIES (exact).
                // Directory segments must match exactly — "search" matches directory
                // "search/" but NOT "elasticsearch/". The class name is checked
                // separately via substring match on the node name.
                let name_lower = result.node.name.to_lowercase();
                let dir_lower = posix_dirname(&result.node.file_path).to_lowercase();
                let dir_segments: Vec<&str> = dir_lower.split('/').collect();
                let mut match_count = 0usize;
                for group in &term_groups {
                    let group_matches = group.iter().any(|term| {
                        let in_name = name_lower.contains(term.as_str());
                        let in_dir = dir_segments.iter().any(|seg| seg == term);
                        in_name || in_dir
                    });
                    if group_matches {
                        match_count += 1;
                    }
                }
                if match_count >= 2 {
                    // Multiplicative boost — 2 terms → 2x, 3 terms → 2.5x
                    result.score *= 1.0 + match_count as f64 * 0.5;
                } else if distinctive_exact_match_ids.contains(result.node.id.as_str()) {
                    // Exact match on a distinctive identifier the user explicitly named —
                    // keep full score (e.g. "LiveEditMode DevServerPreview").
                } else if exact_match_ids.contains(result.node.id.as_str()) {
                    // Exact match on a COMMON word (e.g. "flat" → FLAT): high-scoring noise
                    // inflated by the +exact-name bonus, corroborated by no other query
                    // term. Demote hard so corroborated matches win.
                    result.score *= 0.3;
                } else {
                    // Mild dampen for generic single-term matches — they might be generic
                    // but could also be the right result (e.g., "Protocol" class for an IPC query).
                    result.score *= 0.6;
                }
            }
            sort_by_score_desc(&mut search_results);
        }

        // Step 5b: CamelCase-boundary matching via LIKE query.
        // FTS can't find "Search" inside "TransportSearchAction" (one FTS token).
        // LIKE reliably finds these substring matches. Results are appended with
        // guaranteed slots so they don't compete with higher-scoring prefix matches.
        if !symbols_from_query.is_empty() {
            let camel_definition_kinds = definition_kinds.clone();
            let mut camel_searched_terms: HashSet<String> = HashSet::new();
            let mut search_id_set: HashSet<String> =
                search_results.iter().map(|r| r.node.id.clone()).collect();
            // Track per-node term hits for multi-term boosting
            let mut camel_order: Vec<String> = Vec::new();
            let mut camel_node_terms: HashMap<String, (SearchResult, usize)> = HashMap::new();
            let max_camel_per_term = opts.search_limit.div_ceil(2);

            for sym in &symbols_from_query {
                let title_cased = title_case(sym);
                if utf16_len(&title_cased) < 3 {
                    continue;
                }
                let term_key = title_cased.to_lowercase();
                if !camel_searched_terms.insert(term_key) {
                    continue;
                }

                // Fetch a bounded batch — popular terms like "Search" in Elasticsearch
                // can have many substring matches, so the query layer caps and sorts a
                // candidate window before path-relevance scoring picks the best ones.
                let like_results = self.queries.find_nodes_by_name_substring(
                    &title_cased,
                    &crate::types::SearchOptions {
                        limit: Some(200),
                        kinds: Some(camel_definition_kinds.clone()),
                        ..Default::default()
                    },
                    true, // excludePrefix
                )?;

                // Filter to CamelCase boundaries, score by path relevance, and take top N
                let mut term_candidates: Vec<SearchResult> = Vec::new();
                for r in like_results {
                    let name = &r.node.name;
                    let Some(idx) = name.find(&title_cased) else {
                        continue;
                    };
                    if idx == 0 {
                        continue;
                    }
                    // Accept CamelCase boundary (lowercase before match) OR
                    // acronym boundary (uppercase before match, e.g., RPCProtocol)
                    if !name.as_bytes()[idx - 1].is_ascii_alphabetic() {
                        continue;
                    }
                    if search_id_set.contains(&r.node.id) {
                        continue;
                    }
                    if is_test_file(&r.node.file_path) && !is_test_query {
                        continue;
                    }

                    let path_score = score_path_relevance(&r.node.file_path, query) as f64;
                    let brevity_bonus = (6.0
                        - (utf16_len(name) as f64 - utf16_len(&title_cased) as f64) / 4.0)
                        .max(0.0);
                    term_candidates.push(SearchResult {
                        node: r.node,
                        score: 8.0 + brevity_bonus + path_score,
                        highlights: None,
                    });
                }
                sort_by_score_desc(&mut term_candidates);

                // Widen the per-term pool for accumulation so multi-term co-occurrences
                // can be discovered. A class matching 3 query terms at CamelCase boundaries
                // is far more relevant than one matching just 1, but it needs to survive
                // the per-term cut for EACH term to accumulate its count.
                let accum_per_term = max_camel_per_term * 4;
                for r in term_candidates.into_iter().take(accum_per_term) {
                    match camel_node_terms.get_mut(&r.node.id) {
                        Some((_, term_count)) => {
                            *term_count += 1;
                        }
                        None => {
                            camel_order.push(r.node.id.clone());
                            camel_node_terms.insert(r.node.id.clone(), (r, 1));
                        }
                    }
                }
            }

            // Append CamelCase matches with multi-term boost.
            // These are structurally important (class names containing query terms at
            // CamelCase boundaries) but score much lower than FTS results. Scale their
            // scores up so multi-term CamelCase matches can compete with FTS results.
            let mut camel_results: Vec<SearchResult> = Vec::new();
            for id in &camel_order {
                if let Some((mut result, term_count)) = camel_node_terms.remove(id) {
                    // Multi-term CamelCase matches are extremely relevant — a class matching
                    // 3+ query terms in its name (e.g., ExtensionHostProcess) is almost
                    // certainly what the user wants. Scale aggressively.
                    result.score =
                        result.score * (1.0 + term_count as f64) + (term_count as f64 - 1.0) * 30.0;
                    camel_results.push(result);
                }
            }
            sort_by_score_desc(&mut camel_results);
            let max_camel_total = opts.search_limit;
            for r in camel_results.into_iter().take(max_camel_total) {
                search_id_set.insert(r.node.id.clone());
                search_results.push(r);
            }

            // Step 5c: Compound term matching — find classes whose name contains 2+
            // query terms at ANY position (not just CamelCase boundaries).
            // The CamelCase step above requires idx > 0, which misses classes that
            // START with a query term (e.g., "SearchShardsRequest" starts with "Search").
            // For multi-word queries, a class matching multiple query terms in its name
            // is almost certainly relevant regardless of position.
            if symbols_from_query.len() >= 2 {
                // Collect ALL LIKE results per term (reusing findNodesByNameSubstring)
                // but without the CamelCase boundary or prefix exclusion filters.
                let mut compound_order: Vec<String> = Vec::new();
                let mut compound_term_map: HashMap<String, (Node, HashSet<String>)> =
                    HashMap::new();
                for sym in &symbols_from_query {
                    let title_cased = title_case(sym);
                    if utf16_len(&title_cased) < 3 {
                        continue;
                    }

                    let like_results = self.queries.find_nodes_by_name_substring(
                        &title_cased,
                        &crate::types::SearchOptions {
                            limit: Some(200),
                            kinds: Some(camel_definition_kinds.clone()),
                            ..Default::default()
                        },
                        false, // excludePrefix
                    )?;

                    for r in like_results {
                        if search_id_set.contains(&r.node.id) {
                            continue;
                        }
                        if is_test_file(&r.node.file_path) && !is_test_query {
                            continue;
                        }
                        match compound_term_map.get_mut(&r.node.id) {
                            Some((_, terms)) => {
                                terms.insert(title_cased.clone());
                            }
                            None => {
                                compound_order.push(r.node.id.clone());
                                let mut terms = HashSet::new();
                                terms.insert(title_cased.clone());
                                compound_term_map.insert(r.node.id.clone(), (r.node, terms));
                            }
                        }
                    }
                }

                // Keep only nodes matching 2+ distinct terms
                let mut compound_results: Vec<SearchResult> = Vec::new();
                for id in &compound_order {
                    if let Some((node, terms)) = compound_term_map.remove(id) {
                        if terms.len() >= 2 {
                            let path_score = score_path_relevance(&node.file_path, query) as f64;
                            let brevity_bonus = (6.0 - utf16_len(&node.name) as f64 / 8.0).max(0.0);
                            compound_results.push(SearchResult {
                                score: 10.0
                                    + (terms.len() as f64 - 1.0) * 20.0
                                    + path_score
                                    + brevity_bonus,
                                node,
                                highlights: None,
                            });
                        }
                    }
                }
                sort_by_score_desc(&mut compound_results);
                let max_compound = opts.search_limit.div_ceil(2);
                for r in compound_results.into_iter().take(max_compound) {
                    search_id_set.insert(r.node.id.clone());
                    search_results.push(r);
                }
            }
        }

        // Final sort and truncation — all search channels (exact, text, CamelCase,
        // compound) have now contributed. Sort by score so multi-term matches from
        // later steps can outrank dampened single-term matches from earlier steps.
        sort_by_score_desc(&mut search_results);
        search_results.truncate(opts.search_limit * 3);

        // Filter by minimum score
        let filtered: Vec<SearchResult> = search_results
            .into_iter()
            .filter(|r| r.score >= opts.min_score)
            .collect();

        // Resolve imports/exports to their actual definitions
        // If someone searches "terminal" and finds `import { TerminalPanel }`,
        // they want the TerminalPanel class, not the import statement
        let mut filtered_results = self.resolve_imports_to_definitions(filtered)?;

        // Cap entry points so traversal budget isn't spread too thin.
        // With 36 entry points and maxNodes=120, each gets only 3 nodes — useless.
        // Cap to searchLimit so each entry point gets a meaningful traversal budget.
        if filtered_results.len() > opts.search_limit {
            filtered_results.truncate(opts.search_limit);
        }

        // Confidence signal for the honest-handoff footer (consumed in buildContext).
        // A multi-term prose query that resolves only to isolated common-word matches
        // — no entry point corroborated by 2+ distinct query terms, and none a
        // distinctive identifier the user explicitly named — is LOW confidence: the
        // results are best-effort, not a located answer, so the agent should be told
        // to drill in with explore/trace rather than trust the list as comprehensive.
        // Single-keyword and symbol-name queries are exempt (their single match IS the
        // answer), so the handoff never fires on them.
        let mut confidence = Confidence::High;
        let conf_terms: Vec<String> = extract_search_terms_opts(query, false)
            .into_iter()
            .filter(|t| utf16_len(t) >= 3)
            .collect();
        if conf_terms.len() >= 2 && !filtered_results.is_empty() {
            let distinctive: HashSet<String> = symbols_from_query
                .iter()
                .filter(|s| is_distinctive_identifier(s))
                .map(|s| s.to_lowercase())
                .collect();
            let any_strong = filtered_results.iter().any(|r| {
                if distinctive.contains(&r.node.name.to_lowercase()) {
                    return true;
                }
                let name_lower = r.node.name.to_lowercase();
                let dir_lower = posix_dirname(&r.node.file_path).to_lowercase();
                let dir_segs: Vec<&str> = dir_lower.split('/').collect();
                let mut hits = 0usize;
                for t in &conf_terms {
                    if name_lower.contains(t.as_str()) || dir_segs.contains(&t.as_str()) {
                        hits += 1;
                        if hits >= 2 {
                            return true;
                        }
                    }
                }
                false
            });
            if !any_strong {
                confidence = Confidence::Low;
            }
        }

        // Add entry points to subgraph
        for result in &filtered_results {
            nodes.insert(result.node.clone());
            roots.push(result.node.id.clone());
        }

        // Expand type hierarchy for class/interface entry points.
        // BFS often exhausts its per-entry-point budget on contained methods
        // before reaching extends/implements neighbors. This dedicated step
        // ensures subclasses and superclasses always appear in results.
        // Budget: up to maxNodes/4 hierarchy nodes to avoid flooding.
        let type_hierarchy_kinds: HashSet<NodeKind> = [
            NodeKind::Class,
            NodeKind::Interface,
            NodeKind::Struct,
            NodeKind::Trait,
            NodeKind::Protocol,
        ]
        .into_iter()
        .collect();
        let max_hierarchy_nodes = opts.max_nodes.div_ceil(4);
        let mut hierarchy_nodes_added = 0usize;
        for result in &filtered_results {
            if hierarchy_nodes_added >= max_hierarchy_nodes {
                break;
            }
            if type_hierarchy_kinds.contains(&result.node.kind) {
                let hierarchy = self.traverser.get_type_hierarchy(&result.node.id)?;
                for id in ordered_ids_from_subgraph(&hierarchy) {
                    if !nodes.contains(&id) {
                        if let Some(node) = hierarchy.nodes.get(&id) {
                            nodes.insert(node.clone());
                            hierarchy_nodes_added += 1;
                        }
                    }
                }
                for edge in &hierarchy.edges {
                    if !edge_exists(&edges, edge) {
                        edges.push(edge.clone());
                    }
                }
            }
        }

        // Pass 2: expand hierarchy of newly-discovered parent types to find siblings.
        // E.g., InternalEngine → Engine (parent, from pass 1) → ReadOnlyEngine (sibling).
        if hierarchy_nodes_added > 0 {
            let pass2_candidates: Vec<String> = nodes
                .iter()
                .filter(|n| type_hierarchy_kinds.contains(&n.kind) && !roots.contains(&n.id))
                .map(|n| n.id.clone())
                .collect();
            for candidate in pass2_candidates {
                if hierarchy_nodes_added >= max_hierarchy_nodes {
                    break;
                }
                let sibling_hierarchy = self.traverser.get_type_hierarchy(&candidate)?;
                for id in ordered_ids_from_subgraph(&sibling_hierarchy) {
                    if !nodes.contains(&id) && hierarchy_nodes_added < max_hierarchy_nodes {
                        if let Some(node) = sibling_hierarchy.nodes.get(&id) {
                            nodes.insert(node.clone());
                            hierarchy_nodes_added += 1;
                        }
                    }
                }
                for edge in &sibling_hierarchy.edges {
                    if nodes.contains(&edge.source)
                        && nodes.contains(&edge.target)
                        && !edge_exists(&edges, edge)
                    {
                        edges.push(edge.clone());
                    }
                }
            }
        }

        // Traverse from each entry point
        for result in &filtered_results {
            let traversal_result = self.traverser.traverse_bfs(
                &result.node.id,
                &TraversalOptions {
                    max_depth: Some(opts.traversal_depth),
                    edge_kinds: if opts.edge_kinds.is_empty() {
                        None
                    } else {
                        Some(opts.edge_kinds.clone())
                    },
                    node_kinds: kinds_filter(),
                    direction: Some(Direction::Both),
                    limit: Some(opts.max_nodes.div_ceil(filtered_results.len().max(1))),
                    include_start: None,
                },
            )?;

            // Merge nodes
            for id in ordered_ids_from_subgraph(&traversal_result) {
                if !nodes.contains(&id) {
                    if let Some(node) = traversal_result.nodes.get(&id) {
                        nodes.insert(node.clone());
                    }
                }
            }

            // Merge edges (avoid duplicates)
            for edge in &traversal_result.edges {
                if !edge_exists(&edges, edge) {
                    edges.push(edge.clone());
                }
            }
        }

        // Trim to max nodes if needed
        let mut final_nodes;
        let mut final_edges;
        if nodes.len() > opts.max_nodes {
            // Prioritize entry points and their direct neighbors
            let mut priority_ids: Vec<String> = Vec::new();
            let mut priority_set: HashSet<String> = HashSet::new();
            for id in &roots {
                if priority_set.insert(id.clone()) {
                    priority_ids.push(id.clone());
                }
            }
            for edge in &edges {
                if priority_set.contains(&edge.source) && priority_set.insert(edge.target.clone()) {
                    priority_ids.push(edge.target.clone());
                }
                if priority_set.contains(&edge.target) && priority_set.insert(edge.source.clone()) {
                    priority_ids.push(edge.source.clone());
                }
            }

            // Keep priority nodes, then fill remaining slots
            final_nodes = OrderedNodes::default();
            for id in &priority_ids {
                if let Some(node) = nodes.get(id) {
                    if final_nodes.len() < opts.max_nodes {
                        final_nodes.insert(node.clone());
                    }
                }
            }

            // Fill remaining from other nodes
            for node in nodes.iter() {
                if final_nodes.len() >= opts.max_nodes {
                    break;
                }
                if !final_nodes.contains(&node.id) {
                    final_nodes.insert(node.clone());
                }
            }

            // Filter edges to only include kept nodes
            final_edges = edges
                .iter()
                .filter(|e| final_nodes.contains(&e.source) && final_nodes.contains(&e.target))
                .cloned()
                .collect::<Vec<Edge>>();
        } else {
            final_nodes = nodes;
            final_edges = edges;
        }

        // Per-file diversity cap: prevent any single file from monopolizing the
        // node budget. When BFS traverses from a method, it follows `contains`
        // to the parent class, then back down to all sibling methods. With
        // multiple entry points in the same class, one file can consume 30-40%
        // of maxNodes. Cap each file to ~20% to ensure cross-file diversity.
        let max_per_file = 5usize.max((opts.max_nodes as f64 * 0.2).ceil() as usize);
        let mut file_order: Vec<String> = Vec::new();
        let mut file_counts: HashMap<String, Vec<String>> = HashMap::new();
        for node in final_nodes.iter() {
            match file_counts.get_mut(&node.file_path) {
                Some(ids) => ids.push(node.id.clone()),
                None => {
                    file_order.push(node.file_path.clone());
                    file_counts.insert(node.file_path.clone(), vec![node.id.clone()]);
                }
            }
        }
        let root_set: HashSet<&str> = roots.iter().map(|s| s.as_str()).collect();
        let kind_priority = |kind: NodeKind| -> i32 {
            match kind {
                NodeKind::Class
                | NodeKind::Interface
                | NodeKind::Struct
                | NodeKind::Trait
                | NodeKind::Protocol
                | NodeKind::Enum => 3,
                NodeKind::Method | NodeKind::Function => 1,
                _ => 0,
            }
        };
        for file in &file_order {
            let node_ids = file_counts.get_mut(file).expect("file bucket");
            if node_ids.len() <= max_per_file {
                continue;
            }
            // Sort: entry points first, then classes/interfaces, then others
            node_ids.sort_by_key(|id| {
                let root_bonus = if root_set.contains(id.as_str()) {
                    10
                } else {
                    0
                };
                let kb = final_nodes
                    .get(id)
                    .map(|n| kind_priority(n.kind))
                    .unwrap_or(0);
                -(root_bonus + kb)
            });
            // Remove excess nodes (keep the highest-priority ones)
            for id in &node_ids[max_per_file..] {
                final_nodes.remove(id);
            }
        }
        // Non-production node cap: limit test/sample/integration/example files to
        // at most 15% of the budget. Many codebases have dozens of near-identical
        // test implementations (e.g., 6 Guard classes in integration tests) that
        // individually survive score dampening but collectively flood the result.
        // Test entry points are NOT exempt — they should be evicted too.
        if !is_test_query {
            let max_non_prod = 3usize.max((opts.max_nodes as f64 * 0.15).ceil() as usize);
            let non_prod_ids: Vec<String> = final_nodes
                .iter()
                .filter(|n| is_test_file(&n.file_path))
                .map(|n| n.id.clone())
                .collect();
            if non_prod_ids.len() > max_non_prod {
                for id in &non_prod_ids[max_non_prod..] {
                    final_nodes.remove(id);
                    // Also remove from roots — test file entry points shouldn't anchor results
                    if let Some(root_idx) = roots.iter().position(|r| r == id) {
                        roots.remove(root_idx);
                    }
                }
            }
        }

        // Re-filter edges after per-file and non-production caps
        final_edges.retain(|e| final_nodes.contains(&e.source) && final_nodes.contains(&e.target));

        // Edge recovery: BFS with many entry points leaves most nodes disconnected.
        // Discover edges between already-selected nodes to recover connectivity.
        let recovery_kinds: [EdgeKind; 5] = [
            EdgeKind::Calls,
            EdgeKind::Extends,
            EdgeKind::Implements,
            EdgeKind::References,
            EdgeKind::Overrides,
        ];
        let recovered_edges = self
            .queries
            .find_edges_between_nodes(&final_nodes.ids(), Some(&recovery_kinds))?;
        let mut existing_edge_keys: HashSet<String> = final_edges
            .iter()
            .map(|e| format!("{}:{}:{}", e.source, e.target, e.kind))
            .collect();
        for edge in recovered_edges {
            let key = format!("{}:{}:{}", edge.source, edge.target, edge.kind);
            if existing_edge_keys.insert(key) {
                final_edges.push(edge);
            }
        }

        let node_order = final_nodes.ids();
        Ok((
            Subgraph {
                nodes: final_nodes.map,
                edges: final_edges,
                roots,
                confidence: Some(confidence),
            },
            node_order,
        ))
    }

    /// Get the source code for a node
    ///
    /// Reads the file and extracts the code between startLine and endLine.
    pub fn get_code(&self, node_id: &str) -> Result<Option<String>> {
        let Some(node) = self.queries.get_node_by_id(node_id)? else {
            return Ok(None);
        };
        Ok(self.extract_node_code(&node))
    }

    /// Extract code from a node's source file
    fn extract_node_code(&self, node: &Node) -> Option<String> {
        let file_path = validate_path_within_root(&self.project_root, &node.file_path)?;

        if !file_path.exists() {
            return None;
        }

        match fs::read_to_string(&file_path) {
            Ok(content) => {
                let lines: Vec<&str> = content.split('\n').collect();

                // Extract lines (1-indexed to 0-indexed)
                let start_idx = (node.start_line.saturating_sub(1) as usize).min(lines.len());
                let end_idx = (node.end_line as usize).min(lines.len());

                if start_idx >= end_idx {
                    // JS `lines.slice(start, end)` with start >= end → []
                    return Some(String::new());
                }
                Some(lines[start_idx..end_idx].join("\n"))
            }
            Err(error) => {
                log_debug(
                    "Failed to extract code from node",
                    Some(&json!({
                        "nodeId": node.id,
                        "filePath": node.file_path,
                        "error": error.to_string(),
                    })),
                );
                None
            }
        }
    }

    /// Get entry points from a subgraph (the root nodes)
    fn get_entry_points(subgraph: &Subgraph) -> Vec<Node> {
        subgraph
            .roots
            .iter()
            .filter_map(|id| subgraph.nodes.get(id).cloned())
            .collect()
    }

    /// Extract code blocks for key nodes in the subgraph
    fn extract_code_blocks(
        &self,
        subgraph: &Subgraph,
        node_order: &[String],
        max_blocks: usize,
        max_block_size: usize,
    ) -> Vec<CodeBlock> {
        let mut blocks: Vec<CodeBlock> = Vec::new();

        // Prioritize entry points, then functions/methods
        let mut priority_nodes: Vec<&Node> = Vec::new();

        // First: entry points
        for id in &subgraph.roots {
            if let Some(node) = subgraph.nodes.get(id) {
                priority_nodes.push(node);
            }
        }

        // Then: functions and methods
        for id in node_order {
            let Some(node) = subgraph.nodes.get(id) else {
                continue;
            };
            if !subgraph.roots.contains(&node.id)
                && (node.kind == NodeKind::Function || node.kind == NodeKind::Method)
            {
                priority_nodes.push(node);
            }
        }

        // Then: classes
        for id in node_order {
            let Some(node) = subgraph.nodes.get(id) else {
                continue;
            };
            if !subgraph.roots.contains(&node.id) && node.kind == NodeKind::Class {
                priority_nodes.push(node);
            }
        }

        // Extract code for priority nodes
        for node in priority_nodes {
            if blocks.len() >= max_blocks {
                break;
            }

            let Some(code) = self.extract_node_code(node) else {
                continue;
            };
            if code.is_empty() {
                continue; // JS truthiness: '' is skipped
            }

            // Truncate if too long. Language-neutral marker (no `//` — not a
            // comment in Python, Ruby, etc.); this renders inside a fenced
            // source block whose language varies.
            let truncated = if utf16_len(&code) > max_block_size {
                format!(
                    "{}\n... (truncated) ...",
                    utf16_slice_prefix(&code, max_block_size)
                )
            } else {
                code
            };

            blocks.push(CodeBlock {
                content: truncated,
                file_path: node.file_path.clone(),
                start_line: node.start_line,
                end_line: node.end_line,
                language: node.language,
                node: Some(node.clone()),
            });
        }

        blocks
    }

    /// Get unique files from a subgraph
    fn get_related_files(subgraph: &Subgraph) -> Vec<String> {
        let mut files: Vec<String> = subgraph
            .nodes
            .values()
            .map(|n| n.file_path.clone())
            .collect::<HashSet<String>>()
            .into_iter()
            .collect();
        files.sort();
        files
    }

    /// Generate a summary of the context
    fn generate_summary(_query: &str, subgraph: &Subgraph, entry_points: &[Node]) -> String {
        let node_count = subgraph.nodes.len();
        let edge_count = subgraph.edges.len();
        let files = Self::get_related_files(subgraph);

        let entry_point_names = entry_points
            .iter()
            .take(3)
            .map(|n| n.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");

        let remaining = if entry_points.len() > 3 {
            format!(" and {} more", entry_points.len() - 3)
        } else {
            String::new()
        };

        format!(
            "Found {node_count} relevant code symbols across {} files. \
             Key entry points: {entry_point_names}{remaining}. \
             {edge_count} relationships identified.",
            files.len()
        )
    }

    /// Resolve import/export nodes to their actual definitions
    ///
    /// When search returns `import { TerminalPanel }`, users want the TerminalPanel
    /// class definition, not the import statement. This follows the `imports` edge
    /// to find and return the actual definition instead.
    fn resolve_imports_to_definitions(
        &self,
        results: Vec<SearchResult>,
    ) -> Result<Vec<SearchResult>> {
        let mut resolved: Vec<SearchResult> = Vec::new();
        let mut seen_ids: HashSet<String> = HashSet::new();

        for result in results {
            let node = &result.node;
            let score = result.score;

            // If it's not an import/export, keep it as-is
            if node.kind != NodeKind::Import && node.kind != NodeKind::Export {
                if seen_ids.insert(node.id.clone()) {
                    resolved.push(result);
                }
                continue;
            }

            // For imports/exports, try to find what they reference
            // Imports have outgoing 'imports' edges to the definition
            // Exports have outgoing 'exports' edges to the definition
            let edge_kind = if node.kind == NodeKind::Import {
                EdgeKind::Imports
            } else {
                EdgeKind::Exports
            };
            let outgoing_edges =
                self.queries
                    .get_outgoing_edges(&node.id, Some(&[edge_kind]), None)?;

            let mut found_definition = false;
            for edge in outgoing_edges {
                if let Some(target_node) = self.queries.get_node_by_id(&edge.target)? {
                    if seen_ids.insert(target_node.id.clone()) {
                        // Found the definition - use it instead of the import
                        log_debug(
                            "Resolved import to definition",
                            Some(&json!({
                                "import": node.name,
                                "definition": target_node.name,
                                "kind": target_node.kind,
                            })),
                        );
                        resolved.push(SearchResult {
                            node: target_node,
                            score, // Preserve the original score
                            highlights: None,
                        });
                        found_definition = true;
                    }
                }
            }

            // If we couldn't resolve the import, skip it (it's low-value on its own)
            if !found_definition {
                log_debug(
                    "Skipping unresolved import",
                    Some(&json!({ "name": node.name, "file": node.file_path })),
                );
            }
        }

        Ok(resolved)
    }
}

/// Create a context builder
pub fn create_context_builder(
    project_root: impl Into<PathBuf>,
    queries: Rc<QueryBuilder>,
    traverser: GraphTraverser,
) -> ContextBuilder {
    ContextBuilder::new(project_root, queries, traverser)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_camel_case_snake_case_and_acronyms() {
        let symbols = extract_symbols_from_query(
            "How does UserService call sign_in after MAX_RETRIES with the REST api?",
        );
        assert!(symbols.contains(&"UserService".to_string()));
        assert!(symbols.contains(&"sign_in".to_string()));
        assert!(symbols.contains(&"MAX_RETRIES".to_string()));
        assert!(symbols.contains(&"REST".to_string()));
        assert!(symbols.contains(&"api".to_string()));
        // Common words filtered out
        assert!(!symbols.contains(&"does".to_string()));
        assert!(!symbols.contains(&"call".to_string()));
        assert!(!symbols.contains(&"with".to_string()));
        assert!(!symbols.contains(&"the".to_string()));
        assert!(!symbols.contains(&"after".to_string()));
    }

    #[test]
    fn extracts_dot_notation_full_and_parts() {
        let symbols = extract_symbols_from_query("why is app.isPackaged false");
        assert!(symbols.contains(&"app.isPackaged".to_string()));
        assert!(symbols.contains(&"app".to_string()));
        assert!(symbols.contains(&"isPackaged".to_string()));
    }

    #[test]
    fn filters_common_words_case_insensitively() {
        let symbols = extract_symbols_from_query("Request handling FLOW data");
        assert!(!symbols.contains(&"Request".to_string()));
        assert!(!symbols.contains(&"handling".to_string()));
        assert!(!symbols.contains(&"FLOW".to_string()));
        assert!(!symbols.contains(&"data".to_string()));
    }

    #[test]
    fn title_case_matches_js() {
        assert_eq!(title_case("REST"), "Rest");
        assert_eq!(title_case("bulk"), "Bulk");
        assert_eq!(title_case("allocation"), "Allocation");
        assert_eq!(title_case("Engine"), "Engine");
    }

    #[test]
    fn posix_dirname_matches_node() {
        assert_eq!(posix_dirname("src/a/b.ts"), "src/a");
        assert_eq!(posix_dirname("b.ts"), ".");
        assert_eq!(posix_dirname("/b.ts"), "/");
    }
}
