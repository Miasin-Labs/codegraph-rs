//! Tree-sitter Extraction Types
//!
//! Defines the [`LanguageExtractor`] trait and related types used by
//! the core `TreeSitterExtractor` and per-language extraction configs.
//! Extracted to a leaf module to avoid circular imports.
//!
//! Ported from `src/extraction/tree-sitter-types.ts`. The TS
//! `LanguageExtractor` interface (a config object with arrays + optional
//! hooks) becomes a trait: the array/scalar config properties are required
//! methods, the optional hooks are default-implemented methods returning
//! "absent" values (`None` / `false` / empty slices), so a per-language
//! extractor only overrides what it actually provides — exactly mirroring
//! which keys the TS object literal sets.
//!
//! Deviation from TS (documented in `notes/extraction-core.md`): hooks that
//! in TS take only `(node)` — `getVisibility`, `isAsync`, `isStatic`,
//! `isConst`, `classifyClassNode` — also receive `source: &str` here,
//! because native tree-sitter nodes cannot produce text without the source
//! (web-tree-sitter's `node.text` carried it implicitly).

use crate::types::{Node, NodeKind, UnresolvedReference, Visibility};

/// Alias matching the TS sources' `SyntaxNode` import from web-tree-sitter.
/// Per-language extractors should use this name for parity with the TS files.
pub type SyntaxNode<'tree> = tree_sitter::Node<'tree>;

/// Information returned by a language's `extract_import` hook.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportInfo {
    /// The module/package name being imported
    pub module_name: String,
    /// Full import statement text for display
    pub signature: String,
    /// If true, the hook already created unresolved references itself
    /// (TS optional `handledRefs` — defaults to false).
    pub handled_refs: bool,
}

impl ImportInfo {
    pub fn new(module_name: impl Into<String>, signature: impl Into<String>) -> Self {
        ImportInfo {
            module_name: module_name.into(),
            signature: signature.into(),
            handled_refs: false,
        }
    }
}

/// Tri-state result of [`LanguageExtractor::extract_import`].
///
/// The TS core distinguishes hook *presence* from a hook returning `null`:
/// a present hook that returns `null` ("I didn't handle this") suppresses
/// the generic fallback import node, while a language without the hook gets
/// the generic fallback. Rust traits can't observe overriding, so the
/// distinction is made explicit:
///
/// - [`ImportOutcome::NotHandled`] — the default: language has no
///   `extract_import` hook. Multi-import inline handlers AND the generic
///   fallback may run.
/// - [`ImportOutcome::Declined`] — hook exists but declined this node
///   (TS hook returned `null`). Multi-import inline handlers may run, but
///   the generic fallback must NOT.
/// - [`ImportOutcome::Info`] — hook produced import info.
#[derive(Debug, Clone, Default)]
pub enum ImportOutcome {
    #[default]
    NotHandled,
    Declined,
    Info(ImportInfo),
}

/// Information about a single variable within a declaration.
/// Returned by a language's `extract_variables` hook.
#[derive(Debug, Clone)]
pub struct VariableInfo<'tree> {
    /// Variable name
    pub name: String,
    /// Node kind: `variable` or `constant`
    pub kind: NodeKind,
    /// Optional signature string
    pub signature: Option<String>,
    /// If set, this declarator is actually a function and should be extracted as such
    pub delegate_to_function: Option<SyntaxNode<'tree>>,
    /// The AST node to use for positioning (may differ from the declaration node)
    pub position_node: Option<SyntaxNode<'tree>>,
}

/// Classification returned by [`LanguageExtractor::classify_class_node`]
/// (TS string-literal union `'class' | 'struct' | 'enum' | 'interface' | 'trait'`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ClassLikeKind {
    #[default]
    Class,
    Struct,
    Enum,
    Interface,
    Trait,
}

/// Optional per-node attributes passed to `create_node` — the Rust shape of
/// the TS `extra?: Partial<Node>` parameter. `None` fields are left unset on
/// the created node (TS spread of `undefined` values).
#[derive(Debug, Clone, Default)]
pub struct NodeExtra {
    pub docstring: Option<String>,
    pub signature: Option<String>,
    pub visibility: Option<Visibility>,
    pub is_exported: Option<bool>,
    pub is_async: Option<bool>,
    pub is_static: Option<bool>,
    pub is_abstract: Option<bool>,
    /// Overrides the qualified name built from the scope stack
    /// (TS `extra.qualifiedName`, used for receiver methods and
    /// type-alias members).
    pub qualified_name: Option<String>,
}

/// Context object passed to language hooks that need to call back into the
/// core extractor. Provides a controlled API surface — hooks can create
/// nodes, visit children, and add references without accessing the full
/// `TreeSitterExtractor` internals. The core extractor implements this trait.
pub trait ExtractorContext {
    /// Create a node and add it to the extraction result.
    /// Returns a clone of the created node (`None` when the name is empty).
    fn create_node(
        &mut self,
        kind: NodeKind,
        name: &str,
        node: SyntaxNode<'_>,
        extra: NodeExtra,
    ) -> Option<Node>;
    /// Visit a child node (dispatches through the standard visit_node logic)
    fn visit_node(&mut self, node: SyntaxNode<'_>);
    /// Visit a function body to extract calls
    fn visit_function_body(&mut self, body: SyntaxNode<'_>, function_id: &str);
    /// Add an unresolved reference
    fn add_unresolved_reference(&mut self, reference: UnresolvedReference);
    /// Push a node ID onto the scope stack (for containment/qualified name building)
    fn push_scope(&mut self, node_id: String);
    /// Pop the last node ID from the scope stack
    fn pop_scope(&mut self);
    /// Current file path
    fn file_path(&self) -> &str;
    /// Current source text
    fn source(&self) -> &str;
    /// Stack of parent node IDs (current scope)
    fn node_stack(&self) -> &[String];
    /// All nodes extracted so far
    fn nodes(&self) -> &[Node];
}

/// Language-specific extraction configuration.
///
/// Each supported language provides an implementation of this trait that
/// configures which AST node types to look for and how to extract
/// language-specific details like signatures, visibility, and imports.
///
/// Implementations are stateless (typically unit structs) and registered as
/// `&'static dyn LanguageExtractor` in `languages/mod.rs`.
pub trait LanguageExtractor: Send + Sync {
    // --- Node type mappings ---

    /// Node types that represent functions
    fn function_types(&self) -> &[&str];
    /// Node types that represent classes
    fn class_types(&self) -> &[&str];
    /// Node types that represent methods
    fn method_types(&self) -> &[&str];
    /// Node types that represent interfaces/protocols/traits
    fn interface_types(&self) -> &[&str];
    /// Node types that represent structs
    fn struct_types(&self) -> &[&str];
    /// Node types that represent enums
    fn enum_types(&self) -> &[&str];
    /// Node types that represent enum members/cases (e.g. Swift: `enum_entry`, Rust: `enum_variant`)
    fn enum_member_types(&self) -> &[&str] {
        &[]
    }
    /// Node types that represent type aliases (e.g. `type X = ...`)
    fn type_alias_types(&self) -> &[&str];
    /// Node types that represent imports
    fn import_types(&self) -> &[&str];
    /// Node types that represent function calls
    fn call_types(&self) -> &[&str];
    /// Node types that represent variable declarations (const, let, var, etc.)
    fn variable_types(&self) -> &[&str];
    /// Node types that represent class fields (extracted as `field` kind inside class bodies)
    fn field_types(&self) -> &[&str] {
        &[]
    }
    /// Node types that represent class properties (extracted as `property` kind inside class bodies)
    fn property_types(&self) -> &[&str] {
        &[]
    }

    // --- Field name mappings ---

    /// Field name for identifier/name
    fn name_field(&self) -> &str;
    /// Field name for body
    fn body_field(&self) -> &str;
    /// Field name for parameters
    fn params_field(&self) -> &str;
    /// Field name for return type
    fn return_field(&self) -> Option<&str> {
        None
    }

    // --- Existing hooks ---

    /// Override symbol name extraction (e.g. ObjC multi-part selectors).
    fn resolve_name(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<String> {
        None
    }

    /// Extract property name when the generic name walk fails (e.g. ObjC @property).
    fn extract_property_name(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<String> {
        None
    }

    /// Extract signature from node
    fn get_signature(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<String> {
        None
    }
    /// Extract visibility from node
    fn get_visibility(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<Visibility> {
        None
    }
    /// Check if node is exported. `None` means the language has no notion
    /// of exports (TS hook absent) — the node's `isExported` stays unset.
    fn is_exported(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        None
    }
    /// Check if node is async
    fn is_async(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        None
    }
    /// Check if node is static
    fn is_static(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        None
    }
    /// Check if variable declaration is a constant (const vs let/var)
    fn is_const(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        None
    }

    // --- New config properties ---

    /// Additional node types to treat as class declarations (e.g. Dart: `mixin_declaration`)
    fn extra_class_node_types(&self) -> &[&str] {
        &[]
    }
    /// Whether methods can be top-level without enclosing class (Go: true)
    fn methods_are_top_level(&self) -> bool {
        false
    }
    /// NodeKind to use for interface-like declarations (Rust: `trait`). Default: `interface`
    fn interface_kind(&self) -> NodeKind {
        NodeKind::Interface
    }

    // --- New hooks ---

    /// Custom node visitor. Return true if the node was fully handled (skip default dispatch).
    /// Used by languages with fundamentally different AST structures (e.g. ObjC, Scala).
    fn visit_node(&self, _node: SyntaxNode<'_>, _ctx: &mut dyn ExtractorContext) -> bool {
        false
    }

    /// Classify a class_declaration node when the grammar reuses one node type
    /// for multiple concepts (e.g. Swift uses class_declaration for classes, structs, and enums).
    fn classify_class_node(&self, _node: SyntaxNode<'_>, _source: &str) -> ClassLikeKind {
        ClassLikeKind::Class
    }

    /// Resolve the body node for a function/method/class when it's not a child field.
    /// (e.g. Dart puts function_body as a sibling, not a child.)
    /// Returning `None` falls back to `child_by_field_name(body_field)`.
    fn resolve_body<'t>(&self, _node: SyntaxNode<'t>, _body_field: &str) -> Option<SyntaxNode<'t>> {
        None
    }

    /// Extract import information from an import node. See [`ImportOutcome`]
    /// for the tri-state semantics replacing the TS hook-presence check.
    fn extract_import(&self, _node: SyntaxNode<'_>, _source: &str) -> ImportOutcome {
        ImportOutcome::NotHandled
    }

    /// Extract variable declarations from a variable declaration node.
    /// Returns info about each declared variable, allowing the core to create nodes.
    fn extract_variables<'t>(&self, _node: SyntaxNode<'t>, _source: &str) -> Vec<VariableInfo<'t>> {
        Vec::new()
    }

    /// Extract receiver/owner type name from a method declaration.
    /// Used by Go to get the struct receiver (e.g., "scrapeLoop" from
    /// `func (sl *scrapeLoop) run()`). When present, the receiver type is
    /// included in the qualified name for better searchability.
    fn get_receiver_type(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<String> {
        None
    }

    /// Resolve the actual node kind for a type alias declaration.
    /// Used by Go where `type_spec` is the named declaration wrapper for
    /// structs/interfaces. Returns `struct`, `interface`, etc. to override
    /// the default `type_alias` kind, or `None` to keep it as a type alias.
    fn resolve_type_alias_kind(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<NodeKind> {
        None
    }

    /// Check if a function/method name is a misparse artifact that should be
    /// skipped. Used by C/C++ where macros cause tree-sitter to misparse
    /// namespace blocks as function_definitions. When this returns true, the
    /// function node is NOT created, but the body is still visited for calls
    /// and structural nodes (classes, structs, enums).
    fn is_misparsed_function(&self, _name: &str, _node: SyntaxNode<'_>) -> bool {
        false
    }

    /// Detect bare method calls that don't use call expression syntax.
    /// Used by Ruby where `reset` (no parens, no receiver) is a method call
    /// but tree-sitter parses it as a plain `identifier` node. Returns the
    /// callee name if this node is a bare call, or `None` if not.
    fn extract_bare_call(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<String> {
        None
    }

    /// Node types representing a file-level package/namespace declaration
    /// (e.g. Kotlin `package_header`, Java `package_declaration`). When set,
    /// the core wraps every top-level declaration in an implicit `namespace`
    /// node carrying the FQN, so cross-file import resolution can match by
    /// qualified name instead of filename (Kotlin filename ≠ class name).
    fn package_types(&self) -> &[&str] {
        &[]
    }

    /// Extract the dotted package name from a package declaration node.
    fn extract_package(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<String> {
        None
    }
}
