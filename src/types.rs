//! CodeGraph Type Definitions
//!
//! Core types for the semantic knowledge graph system.
//! Ported from `src/types.ts`. Serde renames keep JSON output camelCase
//! so MCP/CLI JSON responses stay wire-compatible with the TS implementation.

use std::collections::HashMap;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

// =============================================================================
// Union Types
// =============================================================================

/// Types of nodes in the knowledge graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeKind {
    File,
    Module,
    Class,
    Struct,
    Interface,
    Trait,
    Protocol,
    Function,
    Method,
    Property,
    Field,
    Variable,
    Constant,
    Enum,
    EnumMember,
    TypeAlias,
    Namespace,
    Parameter,
    Import,
    Export,
    Route,
    Component,
    /// A named global data symbol in decompiled binary output — IDA's
    /// `off_`/`dword_`/`qword_`/`byte_`/`stru_`/`unk_`/`asc_`/`*_vtable` etc.
    DataSymbol,
    /// A string / format literal in decompiled binary output.
    StringLiteral,
    /// A macro definition (Rust `macro_rules!`, C `#define`-style, etc.).
    /// Macro *expansion* is out of scope — this is the definition symbol only,
    /// so the analysis bridge maps it to `None` (it is not callable code).
    Macro,
}

/// Runtime-iterable list of all node kinds (mirrors `NODE_KINDS` in TS).
pub const NODE_KINDS: [NodeKind; 25] = [
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
    NodeKind::Parameter,
    NodeKind::Import,
    NodeKind::Export,
    NodeKind::Route,
    NodeKind::Component,
    NodeKind::DataSymbol,
    NodeKind::StringLiteral,
    NodeKind::Macro,
];

impl NodeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            NodeKind::File => "file",
            NodeKind::Module => "module",
            NodeKind::Class => "class",
            NodeKind::Struct => "struct",
            NodeKind::Interface => "interface",
            NodeKind::Trait => "trait",
            NodeKind::Protocol => "protocol",
            NodeKind::Function => "function",
            NodeKind::Method => "method",
            NodeKind::Property => "property",
            NodeKind::Field => "field",
            NodeKind::Variable => "variable",
            NodeKind::Constant => "constant",
            NodeKind::Enum => "enum",
            NodeKind::EnumMember => "enum_member",
            NodeKind::TypeAlias => "type_alias",
            NodeKind::Namespace => "namespace",
            NodeKind::Parameter => "parameter",
            NodeKind::Import => "import",
            NodeKind::Export => "export",
            NodeKind::Route => "route",
            NodeKind::Component => "component",
            NodeKind::DataSymbol => "data_symbol",
            NodeKind::StringLiteral => "string_literal",
            NodeKind::Macro => "macro",
        }
    }
}

impl fmt::Display for NodeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for NodeKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        NODE_KINDS
            .iter()
            .find(|k| k.as_str() == s)
            .copied()
            .ok_or_else(|| format!("unknown node kind: {s}"))
    }
}

/// Types of edges (relationships) between nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EdgeKind {
    /// Parent contains child (file→class, class→method)
    Contains,
    /// Function/method calls another
    Calls,
    /// File imports from another
    Imports,
    /// File exports a symbol
    Exports,
    /// Class/interface extends another
    Extends,
    /// Class implements interface
    Implements,
    /// Generic reference to another symbol
    References,
    /// Variable/parameter has type
    TypeOf,
    /// Function returns type
    Returns,
    /// Creates instance of class
    Instantiates,
    /// Method overrides parent method
    Overrides,
    /// Decorator applied to symbol
    Decorates,
    /// Symbol reads a data symbol (decompiled binary: load from `dword_`/…)
    Reads,
    /// Symbol writes a data symbol (decompiled binary: store to `qword_`/…)
    Writes,
    /// Thunk/trampoline forwards to its real target (alias, not a real call)
    Aliases,
}

/// Runtime-iterable list of all edge kinds.
pub const EDGE_KINDS: [EdgeKind; 15] = [
    EdgeKind::Contains,
    EdgeKind::Calls,
    EdgeKind::Imports,
    EdgeKind::Exports,
    EdgeKind::Extends,
    EdgeKind::Implements,
    EdgeKind::References,
    EdgeKind::TypeOf,
    EdgeKind::Returns,
    EdgeKind::Instantiates,
    EdgeKind::Overrides,
    EdgeKind::Decorates,
    EdgeKind::Reads,
    EdgeKind::Writes,
    EdgeKind::Aliases,
];

impl EdgeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            EdgeKind::Contains => "contains",
            EdgeKind::Calls => "calls",
            EdgeKind::Imports => "imports",
            EdgeKind::Exports => "exports",
            EdgeKind::Extends => "extends",
            EdgeKind::Implements => "implements",
            EdgeKind::References => "references",
            EdgeKind::TypeOf => "type_of",
            EdgeKind::Returns => "returns",
            EdgeKind::Instantiates => "instantiates",
            EdgeKind::Overrides => "overrides",
            EdgeKind::Decorates => "decorates",
            EdgeKind::Reads => "reads",
            EdgeKind::Writes => "writes",
            EdgeKind::Aliases => "aliases",
        }
    }
}

impl fmt::Display for EdgeKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for EdgeKind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        EDGE_KINDS
            .iter()
            .find(|k| k.as_str() == s)
            .copied()
            .ok_or_else(|| format!("unknown edge kind: {s}"))
    }
}

/// Supported programming languages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Typescript,
    Javascript,
    Tsx,
    Jsx,
    Arkts,
    Python,
    Go,
    Rust,
    Java,
    C,
    Cpp,
    Csharp,
    Razor,
    Php,
    Ruby,
    Swift,
    Kotlin,
    Dart,
    Svelte,
    Vue,
    Astro,
    Liquid,
    Pascal,
    Scala,
    Lua,
    Luau,
    Objc,
    R,
    Solidity,
    Nix,
    Apex,
    Bash,
    Html,
    Visualforce,
    Aura,
    Yaml,
    Twig,
    Xml,
    Properties,
    Cfml,
    Cfscript,
    Cfquery,
    Cobol,
    Vbnet,
    Erlang,
    Terraform,
    Unknown,
}

/// Runtime-iterable list of all languages (mirrors `LANGUAGES` in TS).
pub const LANGUAGES: [Language; 47] = [
    Language::Typescript,
    Language::Javascript,
    Language::Tsx,
    Language::Jsx,
    Language::Arkts,
    Language::Python,
    Language::Go,
    Language::Rust,
    Language::Java,
    Language::C,
    Language::Cpp,
    Language::Csharp,
    Language::Razor,
    Language::Php,
    Language::Ruby,
    Language::Swift,
    Language::Kotlin,
    Language::Dart,
    Language::Svelte,
    Language::Vue,
    Language::Astro,
    Language::Liquid,
    Language::Pascal,
    Language::Scala,
    Language::Lua,
    Language::Luau,
    Language::Objc,
    Language::R,
    Language::Solidity,
    Language::Nix,
    Language::Apex,
    Language::Bash,
    Language::Html,
    Language::Visualforce,
    Language::Aura,
    Language::Yaml,
    Language::Twig,
    Language::Xml,
    Language::Properties,
    Language::Cfml,
    Language::Cfscript,
    Language::Cfquery,
    Language::Cobol,
    Language::Vbnet,
    Language::Erlang,
    Language::Terraform,
    Language::Unknown,
];

impl Language {
    pub fn as_str(&self) -> &'static str {
        match self {
            Language::Typescript => "typescript",
            Language::Javascript => "javascript",
            Language::Tsx => "tsx",
            Language::Jsx => "jsx",
            Language::Arkts => "arkts",
            Language::Python => "python",
            Language::Go => "go",
            Language::Rust => "rust",
            Language::Java => "java",
            Language::C => "c",
            Language::Cpp => "cpp",
            Language::Csharp => "csharp",
            Language::Razor => "razor",
            Language::Php => "php",
            Language::Ruby => "ruby",
            Language::Swift => "swift",
            Language::Kotlin => "kotlin",
            Language::Dart => "dart",
            Language::Svelte => "svelte",
            Language::Vue => "vue",
            Language::Astro => "astro",
            Language::Liquid => "liquid",
            Language::Pascal => "pascal",
            Language::Scala => "scala",
            Language::Lua => "lua",
            Language::Luau => "luau",
            Language::Objc => "objc",
            Language::R => "r",
            Language::Solidity => "solidity",
            Language::Nix => "nix",
            Language::Apex => "apex",
            Language::Bash => "bash",
            Language::Html => "html",
            Language::Visualforce => "visualforce",
            Language::Aura => "aura",
            Language::Yaml => "yaml",
            Language::Twig => "twig",
            Language::Xml => "xml",
            Language::Properties => "properties",
            Language::Cfml => "cfml",
            Language::Cfscript => "cfscript",
            Language::Cfquery => "cfquery",
            Language::Cobol => "cobol",
            Language::Vbnet => "vbnet",
            Language::Erlang => "erlang",
            Language::Terraform => "terraform",
            Language::Unknown => "unknown",
        }
    }
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Language {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        LANGUAGES
            .iter()
            .find(|l| l.as_str() == s)
            .copied()
            .ok_or_else(|| format!("unknown language: {s}"))
    }
}

/// Visibility modifier on a node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    Private,
    Protected,
    Internal,
}

/// How an edge was created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Provenance {
    #[serde(rename = "tree-sitter")]
    TreeSitter,
    #[serde(rename = "scip")]
    Scip,
    #[serde(rename = "heuristic")]
    Heuristic,
}

impl Provenance {
    pub fn as_str(&self) -> &'static str {
        match self {
            Provenance::TreeSitter => "tree-sitter",
            Provenance::Scip => "scip",
            Provenance::Heuristic => "heuristic",
        }
    }
}

impl FromStr for Provenance {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tree-sitter" => Ok(Provenance::TreeSitter),
            "scip" => Ok(Provenance::Scip),
            "heuristic" => Ok(Provenance::Heuristic),
            other => Err(format!("unknown provenance: {other}")),
        }
    }
}

// =============================================================================
// Core Graph Types
// =============================================================================

/// A node in the knowledge graph representing a code symbol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Node {
    /// Unique identifier (hash of file path + qualified name)
    pub id: String,
    /// Type of code element
    pub kind: NodeKind,
    /// Simple name (e.g., "calculateTotal")
    pub name: String,
    /// Fully qualified name (e.g., "src/utils.ts::MathHelper.calculateTotal")
    pub qualified_name: String,
    /// File path relative to project root
    pub file_path: String,
    /// Programming language
    pub language: Language,
    /// Starting line number (1-indexed)
    pub start_line: u32,
    /// Ending line number (1-indexed)
    pub end_line: u32,
    /// Starting column (0-indexed)
    pub start_column: u32,
    /// Ending column (0-indexed)
    pub end_column: u32,
    /// Starting byte offset in the source file (tree-sitter `start_byte()`).
    ///
    /// `None` for rows indexed before schema v5 and for extractors that do
    /// not track byte offsets (some standalone non-tree-sitter extractors,
    /// synthetic nodes such as routes). `skip_serializing_if` keeps the JSON
    /// wire shape unchanged when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_byte: Option<u32>,
    /// Ending byte offset (exclusive) in the source file (tree-sitter
    /// `end_byte()`). Present iff [`Self::start_byte`] is present.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_byte: Option<u32>,
    /// Virtual address of the symbol in the binary, when known. Populated from
    /// decompiled IDA/Hex-Rays output (the `// Address:` header or the
    /// `sub_<HEX>` name); `None` for source-language nodes. This is the
    /// binary-level join key for manifests and cross-decompiler parity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<u64>,
    /// Size in bytes of the symbol in the binary, when known (IDA `// Size:` /
    /// `// Function size:`). Pairs with [`Self::address`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<u32>,
    /// Documentation string if present
    #[serde(skip_serializing_if = "Option::is_none")]
    pub docstring: Option<String>,
    /// Function/method signature
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    /// Normalized function/method result type used for receiver inference.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>,
    /// Visibility modifier
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<Visibility>,
    /// Whether symbol is exported
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_exported: Option<bool>,
    /// Whether symbol is async
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_async: Option<bool>,
    /// Whether symbol is static
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_static: Option<bool>,
    /// Whether symbol is abstract
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_abstract: Option<bool>,
    /// Decorators/annotations applied
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decorators: Option<Vec<String>>,
    /// Generic type parameters
    #[serde(skip_serializing_if = "Option::is_none")]
    pub type_parameters: Option<Vec<String>>,
    /// When the node was last updated (epoch milliseconds)
    pub updated_at: i64,
}

impl Node {
    /// Minimal constructor with required fields; optional fields default to None.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: impl Into<String>,
        kind: NodeKind,
        name: impl Into<String>,
        qualified_name: impl Into<String>,
        file_path: impl Into<String>,
        language: Language,
        start_line: u32,
        end_line: u32,
    ) -> Self {
        Node {
            id: id.into(),
            kind,
            name: name.into(),
            qualified_name: qualified_name.into(),
            file_path: file_path.into(),
            language,
            start_line,
            end_line,
            start_column: 0,
            end_column: 0,
            start_byte: None,
            end_byte: None,
            address: None,
            size: None,
            docstring: None,
            signature: None,
            return_type: None,
            visibility: None,
            is_exported: None,
            is_async: None,
            is_static: None,
            is_abstract: None,
            decorators: None,
            type_parameters: None,
            updated_at: 0,
        }
    }

    /// The node's byte range in its source file, when both offsets are
    /// known and well-formed (`start <= end`). `None` for rows indexed
    /// before schema v5 or by extractors that don't track byte offsets.
    pub fn byte_range(&self) -> Option<std::ops::Range<usize>> {
        match (self.start_byte, self.end_byte) {
            (Some(s), Some(e)) if s <= e => Some(s as usize..e as usize),
            _ => None,
        }
    }
}

/// Arbitrary JSON metadata attached to an edge.
pub type Metadata = serde_json::Map<String, serde_json::Value>;

/// An edge representing a relationship between two nodes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Edge {
    /// Source node ID
    pub source: String,
    /// Target node ID
    pub target: String,
    /// Type of relationship
    pub kind: EdgeKind,
    /// Additional context about the relationship
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
    /// Line number where relationship occurs (e.g., call site)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column number where relationship occurs
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// How this edge was created
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<Provenance>,
}

impl Edge {
    pub fn new(source: impl Into<String>, target: impl Into<String>, kind: EdgeKind) -> Self {
        Edge {
            source: source.into(),
            target: target.into(),
            kind,
            metadata: None,
            line: None,
            column: None,
            provenance: None,
        }
    }
}

/// Metadata about a tracked file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileRecord {
    /// File path relative to project root
    pub path: String,
    /// Content hash for change detection
    pub content_hash: String,
    /// Detected language
    pub language: Language,
    /// File size in bytes
    pub size: u64,
    /// Last modification timestamp (epoch milliseconds)
    pub modified_at: i64,
    /// When last indexed (epoch milliseconds)
    pub indexed_at: i64,
    /// Number of nodes extracted
    pub node_count: u32,
    /// Any extraction errors
    #[serde(skip_serializing_if = "Option::is_none")]
    pub errors: Option<Vec<ExtractionError>>,
}

// =============================================================================
// Extraction Types
// =============================================================================

/// Result from parsing a source file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionResult {
    /// Extracted nodes
    pub nodes: Vec<Node>,
    /// Extracted edges
    pub edges: Vec<Edge>,
    /// References that couldn't be resolved yet
    pub unresolved_references: Vec<UnresolvedReference>,
    /// Any errors during extraction
    pub errors: Vec<ExtractionError>,
    /// Extraction duration in milliseconds
    pub duration_ms: f64,
}

/// Error severity for extraction errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

/// Error during code extraction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtractionError {
    /// Error message
    pub message: String,
    /// File path where the error occurred
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// Line number if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Column number if available
    #[serde(skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    /// Error severity
    pub severity: Severity,
    /// Error code for categorization
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

/// A reference that couldn't be resolved during extraction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnresolvedReference {
    /// ID of the node containing the reference
    pub from_node_id: String,
    /// Name being referenced
    pub reference_name: String,
    /// Type of reference (call, type, import, etc.)
    pub reference_kind: EdgeKind,
    /// Location of the reference
    pub line: u32,
    pub column: u32,
    /// File path where reference occurs (denormalized for performance)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    /// Language of the source file (denormalized for performance)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<Language>,
    /// Possible qualified names it might resolve to
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Metadata>,
}

// =============================================================================
// Query Types
// =============================================================================

/// Retrieval confidence for context-style queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    High,
    Low,
}

/// A subgraph containing a subset of the knowledge graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subgraph {
    /// Nodes in this subgraph, keyed by node ID
    pub nodes: HashMap<String, Node>,
    /// Edges in this subgraph
    pub edges: Vec<Edge>,
    /// Root node IDs (entry points)
    pub roots: Vec<String>,
    /// Retrieval confidence for context-style queries. `Low` means the query
    /// resolved only to isolated common-word matches — callers should surface
    /// an honest handoff rather than present results as comprehensive.
    /// `None` for graph traversals that don't run the search-ranking path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Confidence>,
}

/// Direction of traversal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Outgoing,
    Incoming,
    Both,
}

/// Options for graph traversal.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TraversalOptions {
    /// Maximum depth to traverse (default: unlimited)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u32>,
    /// Edge types to follow (default: all)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_kinds: Option<Vec<EdgeKind>>,
    /// Node types to include (default: all)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_kinds: Option<Vec<NodeKind>>,
    /// Direction of traversal
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<Direction>,
    /// Maximum nodes to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Whether to include the starting node
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_start: Option<bool>,
}

/// Options for searching the graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOptions {
    /// Node types to search
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kinds: Option<Vec<NodeKind>>,
    /// Languages to include
    #[serde(skip_serializing_if = "Option::is_none")]
    pub languages: Option<Vec<Language>>,
    /// File path patterns to include
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_patterns: Option<Vec<String>>,
    /// File path patterns to exclude
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exclude_patterns: Option<Vec<String>>,
    /// Maximum results to return
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    /// Offset for pagination
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<usize>,
    /// Whether search is case-sensitive
    #[serde(skip_serializing_if = "Option::is_none")]
    pub case_sensitive: Option<bool>,
}

/// A search result with relevance scoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    /// Matching node
    pub node: Node,
    /// Relevance score (0-1)
    pub score: f64,
    /// Matched text snippets for highlighting
    #[serde(skip_serializing_if = "Option::is_none")]
    pub highlights: Option<Vec<String>>,
}

// =============================================================================
// Context Types
// =============================================================================

/// A node + edge pair used for incoming/outgoing reference lists.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeRef {
    pub node: Node,
    pub edge: Edge,
}

/// Context information for code understanding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Context {
    /// Primary node being examined
    pub focal: Node,
    /// Nodes containing the focal node (file, class, etc.)
    pub ancestors: Vec<Node>,
    /// Nodes directly contained by focal node
    pub children: Vec<Node>,
    /// Incoming references (who calls/uses this)
    pub incoming_refs: Vec<NodeRef>,
    /// Outgoing references (what this calls/uses)
    pub outgoing_refs: Vec<NodeRef>,
    /// Related type information
    pub types: Vec<Node>,
    /// Relevant imports
    pub imports: Vec<Node>,
}

/// A block of code with context.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeBlock {
    /// The code content
    pub content: String,
    /// File path
    pub file_path: String,
    /// Starting line
    pub start_line: u32,
    /// Ending line
    pub end_line: u32,
    /// Language for syntax highlighting
    pub language: Language,
    /// Associated node if extracted
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<Node>,
}

// =============================================================================
// Database Types
// =============================================================================

/// Database schema version info.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchemaVersion {
    /// Current schema version
    pub version: u32,
    /// When schema was created/updated (epoch milliseconds)
    pub applied_at: i64,
    /// Description of this version
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Statistics about the knowledge graph.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GraphStats {
    /// Total number of nodes
    pub node_count: u64,
    /// Total number of edges
    pub edge_count: u64,
    /// Number of tracked files
    pub file_count: u64,
    /// Node counts by kind
    pub nodes_by_kind: HashMap<String, u64>,
    /// Edge counts by kind
    pub edges_by_kind: HashMap<String, u64>,
    /// File counts by language
    pub files_by_language: HashMap<String, u64>,
    /// Database size in bytes
    pub db_size_bytes: u64,
    /// Last update timestamp (epoch milliseconds)
    pub last_updated: i64,
}

// =============================================================================
// Task Context Types (for buildContext)
// =============================================================================

/// Input for building task context (string or {title, description}).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum TaskInput {
    Text(String),
    Titled {
        title: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

impl TaskInput {
    /// Flatten to the query string ("title: description" form for Titled).
    pub fn as_query(&self) -> String {
        match self {
            TaskInput::Text(s) => s.clone(),
            TaskInput::Titled { title, description } => match description {
                Some(d) => format!("{title}: {d}"),
                None => title.clone(),
            },
        }
    }
}

impl From<&str> for TaskInput {
    fn from(s: &str) -> Self {
        TaskInput::Text(s.to_string())
    }
}

/// Output format for buildContext.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContextFormat {
    Markdown,
    Json,
}

/// Options for building task context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildContextOptions {
    /// Maximum number of nodes to include (default: 50)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<usize>,
    /// Maximum number of code blocks to include (default: 10)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_code_blocks: Option<usize>,
    /// Maximum characters per code block (default: 2000)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_code_block_size: Option<usize>,
    /// Whether to include code blocks (default: true)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_code: Option<bool>,
    /// Output format (default: markdown)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<ContextFormat>,
    /// Number of semantic search results (default: 5)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_limit: Option<usize>,
    /// Graph traversal depth from entry points (default: 2)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traversal_depth: Option<u32>,
    /// Minimum semantic similarity score (default: 0.3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_score: Option<f64>,
}

/// Statistics about a built context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskContextStats {
    /// Number of nodes included
    pub node_count: usize,
    /// Number of edges included
    pub edge_count: usize,
    /// Number of files touched
    pub file_count: usize,
    /// Number of code blocks included
    pub code_block_count: usize,
    /// Total characters in code blocks
    pub total_code_size: usize,
}

/// Full context for a task, ready for an AI agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskContext {
    /// The original query/task
    pub query: String,
    /// Subgraph of relevant nodes and edges
    pub subgraph: Subgraph,
    /// Entry point nodes (from semantic search)
    pub entry_points: Vec<Node>,
    /// Code blocks extracted from key nodes
    pub code_blocks: Vec<CodeBlock>,
    /// Files involved in this context
    pub related_files: Vec<String>,
    /// Brief summary of the context
    pub summary: String,
    /// Statistics about the context
    pub stats: TaskContextStats,
}

/// Options for finding relevant context.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindRelevantContextOptions {
    /// Number of semantic search results (default: 5)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub search_limit: Option<usize>,
    /// Graph traversal depth (default: 2)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub traversal_depth: Option<u32>,
    /// Maximum nodes in result (default: 50)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_nodes: Option<usize>,
    /// Minimum semantic similarity score (default: 0.3)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_score: Option<f64>,
    /// Edge types to follow in traversal
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edge_kinds: Option<Vec<EdgeKind>>,
    /// Node types to include
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_kinds: Option<Vec<NodeKind>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_round_trips_as_snake_case() {
        for kind in NODE_KINDS {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            let back: NodeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
            assert_eq!(kind.as_str().parse::<NodeKind>().unwrap(), kind);
        }
    }

    #[test]
    fn edge_kind_round_trips() {
        for kind in EDGE_KINDS {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, format!("\"{}\"", kind.as_str()));
            let back: EdgeKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn language_round_trips() {
        for lang in LANGUAGES {
            let json = serde_json::to_string(&lang).unwrap();
            assert_eq!(json, format!("\"{}\"", lang.as_str()));
            let back: Language = serde_json::from_str(&json).unwrap();
            assert_eq!(back, lang);
        }
    }

    #[test]
    fn node_serializes_camel_case() {
        let node = Node::new(
            "abc",
            NodeKind::Function,
            "f",
            "src/a.ts::f",
            "src/a.ts",
            Language::Typescript,
            1,
            2,
        );
        let v = serde_json::to_value(&node).unwrap();
        assert_eq!(v["qualifiedName"], "src/a.ts::f");
        assert_eq!(v["filePath"], "src/a.ts");
        assert_eq!(v["startLine"], 1);
        assert!(v.get("docstring").is_none());
    }

    #[test]
    fn provenance_uses_hyphenated_tree_sitter() {
        assert_eq!(
            serde_json::to_string(&Provenance::TreeSitter).unwrap(),
            "\"tree-sitter\""
        );
    }
}
