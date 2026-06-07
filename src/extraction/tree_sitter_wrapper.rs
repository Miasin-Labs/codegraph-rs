//! Tree-sitter Parser Wrapper
//!
//! Handles parsing source code and extracting structural information.
//!
//! Ported from `src/extraction/tree-sitter.ts` (the `TreeSitterExtractor`
//! class plus its private helpers). The bottom-of-file `extractFromSource`
//! dispatcher is NOT here — it routes to the standalone extractors
//! (svelte/vue/liquid/mybatis/dfm/ida) and framework resolvers owned by
//! other modules; see `notes/extraction-core.md` for the wiring contract.
//!
//! Native deviations from the TS original (web-tree-sitter):
//! - No WASM memory management: the TS `tree.delete()` / "memory access out
//!   of bounds" re-throw / `this.source = ''` GC-pressure dance disappears —
//!   native trees free on drop.
//! - Positions: tree-sitter rows are 0-based; stored `startLine` is 1-based
//!   (the `+1` is preserved everywhere the TS applies it — including the one
//!   place it deliberately does NOT, the anonymous-class `extends` ref).
//! - Text indices are byte offsets into UTF-8 (TS used UTF-16 code-unit
//!   indices); identical for ASCII sources.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::extraction::grammars::{create_parser, detect_language, is_language_supported};
use crate::extraction::tree_sitter_helpers::{
    generate_node_id,
    get_child_by_field,
    get_node_text,
    get_preceding_docstring,
};
use crate::extraction::tree_sitter_types::{
    ClassLikeKind,
    ExtractorContext,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
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

/// Epoch milliseconds (`Date.now()` parity).
fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Grow the stack before another level of AST descent.
///
/// Real-world ASTs (LLVM, generated sources) nest deeply enough to blow the
/// 2 MiB default stack of the rayon workers extraction runs on. Every
/// recursive tree walker calls this at its head so recursion depth is bounded
/// by input size, never by thread stack. Mirrors rustc's
/// `ensure_sufficient_stack`.
fn ensure_sufficient_stack<R>(f: impl FnOnce() -> R) -> R {
    /// Remaining-stack threshold that triggers a new segment. Must exceed the
    /// deepest guard-free run of frames (one visit level) with margin.
    const RED_ZONE: usize = 128 * 1024;
    /// Each new segment's size — large enough that segment switches stay rare
    /// even on pathologically nested files.
    const STACK_GROW: usize = 8 * 1024 * 1024;
    stacker::maybe_grow(RED_ZONE, STACK_GROW, f)
}

/// Collect a node's named children into a Vec (the TS `namedChildren` array).
fn named_children<'t>(node: SyntaxNode<'t>) -> Vec<SyntaxNode<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// First named child with the given kind (TS `namedChildren.find(c => c.type === kind)`).
fn find_named_child<'t>(node: SyntaxNode<'t>, kind: &str) -> Option<SyntaxNode<'t>> {
    named_children(node).into_iter().find(|c| c.kind() == kind)
}

/// `path.basename(p)` parity for forward-slash paths.
fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Strip a namespace qualifier: keep the trailing identifier after the
/// rightmost `.` or `::`, then drop ONE leading `:`/`.` left by the `::`
/// split (TS `slice(lastDot + 1).replace(/^[:.]/, '')`).
fn strip_qualifier(name: &str) -> String {
    let last_dot = name.rfind('.').map(|i| i as i64).unwrap_or(-1);
    let last_colons = name.rfind("::").map(|i| i as i64).unwrap_or(-1);
    let last = last_dot.max(last_colons);
    if last >= 0 {
        let mut out = name[(last as usize + 1)..].to_string();
        if out.starts_with(':') || out.starts_with('.') {
            out.remove(0);
        }
        out
    } else {
        name.to_string()
    }
}

/// `= <first 100 chars of initializer>[...]` signature for variables.
fn init_signature(value_text: &str) -> String {
    let truncated: String = value_text.chars().take(100).collect();
    let long = value_text.chars().nth(99).is_some();
    format!("= {}{}", truncated, if long { "..." } else { "" })
}

/// Extract the name from a node based on language
fn extract_name(node: SyntaxNode<'_>, source: &str, extractor: &dyn LanguageExtractor) -> String {
    if let Some(hook_name) = extractor.resolve_name(node, source) {
        if !hook_name.is_empty() {
            return hook_name;
        }
    }

    // Try field name first
    if let Some(name_node) = get_child_by_field(node, extractor.name_field()) {
        // Unwrap pointer_declarator(s) for C/C++ pointer return types
        let mut resolved = name_node;
        while resolved.kind() == "pointer_declarator" {
            let inner =
                get_child_by_field(resolved, "declarator").or_else(|| resolved.named_child(0));
            match inner {
                Some(i) => resolved = i,
                None => break,
            }
        }
        // Handle complex declarators (C/C++)
        if resolved.kind() == "function_declarator" || resolved.kind() == "declarator" {
            let inner_name =
                get_child_by_field(resolved, "declarator").or_else(|| resolved.named_child(0));
            return match inner_name {
                Some(n) => get_node_text(n, source).to_string(),
                None => get_node_text(resolved, source).to_string(),
            };
        }
        // Lua: `function t.f()` / `function t:m()` — the name node is a dot/method
        // index expression; the simple name is the trailing field/method (the table
        // receiver is captured separately via get_receiver_type).
        if resolved.kind() == "dot_index_expression" {
            if let Some(field) = get_child_by_field(resolved, "field") {
                return get_node_text(field, source).to_string();
            }
        }
        if resolved.kind() == "method_index_expression" {
            if let Some(method) = get_child_by_field(resolved, "method") {
                return get_node_text(method, source).to_string();
            }
        }
        return get_node_text(resolved, source).to_string();
    }

    // For Dart method_signature, look inside inner signature types
    if node.kind() == "method_signature" {
        for child in named_children(node) {
            if matches!(
                child.kind(),
                "function_signature"
                    | "getter_signature"
                    | "setter_signature"
                    | "constructor_signature"
                    | "factory_constructor_signature"
            ) {
                // Find identifier inside the inner signature
                for inner in named_children(child) {
                    if inner.kind() == "identifier" {
                        return get_node_text(inner, source).to_string();
                    }
                }
            }
        }
    }

    // Arrow/function expressions get their name from the parent variable_declarator,
    // not from identifiers in their body. Without this, single-expression arrow
    // functions like `const fn = () => someIdentifier` get named "someIdentifier"
    // instead of "fn", because the fallback below finds the body identifier.
    if node.kind() == "arrow_function" || node.kind() == "function_expression" {
        return "<anonymous>".to_string();
    }

    // Fall back to first identifier child
    for child in named_children(node) {
        if matches!(
            child.kind(),
            "identifier" | "type_identifier" | "simple_identifier" | "constant"
        ) {
            return get_node_text(child, source).to_string();
        }
    }

    "<anonymous>".to_string()
}

/// Tree-sitter node kinds that represent constructor invocations
/// (`new Foo()` and friends). Used by extract_instantiation to emit
/// an `instantiates` reference targeting the class name.
const INSTANTIATION_KINDS: &[&str] = &[
    "new_expression",               // typescript / javascript / tsx / jsx
    "object_creation_expression",   // java / c#
    "instance_creation_expression", // some grammars
];

/// Languages that support type annotations (TypeScript, etc.)
fn is_type_annotation_language(language: Language) -> bool {
    matches!(
        language,
        Language::Typescript
            | Language::Tsx
            | Language::Dart
            | Language::Kotlin
            | Language::Swift
            | Language::Rust
            | Language::Go
            | Language::Java
            | Language::Csharp
    )
}

/// Built-in/primitive type names that shouldn't create references
const BUILTIN_TYPES: &[&str] = &[
    "string",
    "number",
    "boolean",
    "void",
    "null",
    "undefined",
    "never",
    "any",
    "unknown",
    "object",
    "symbol",
    "bigint",
    "true",
    "false",
    // Rust
    "str",
    "bool",
    "i8",
    "i16",
    "i32",
    "i64",
    "i128",
    "isize",
    "u8",
    "u16",
    "u32",
    "u64",
    "u128",
    "usize",
    "f32",
    "f64",
    "char",
    // Java/C#
    "int",
    "long",
    "short",
    "byte",
    "float",
    "double",
    // Go
    "int8",
    "int16",
    "int32",
    "int64",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "float32",
    "float64",
    "complex64",
    "complex128",
    "rune",
    "error",
];

/// TreeSitterExtractor - Main extraction class
pub struct TreeSitterExtractor<'a> {
    file_path: String,
    language: Language,
    source: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
    extractor: Option<&'a dyn LanguageExtractor>,
    /// Stack of parent node IDs
    node_stack: Vec<String>,
    /// lookup key → node ID for Pascal defProc lookup
    method_index: Option<HashMap<String, String>>,
}

impl<'a> TreeSitterExtractor<'a> {
    /// Create an extractor. `language: None` detects from the file path +
    /// source (TS constructor's `language || detectLanguage(...)`).
    ///
    /// `extractor` is the per-language config — the TS constructor looked it
    /// up in `EXTRACTORS[language]`; here the caller passes it in (typically
    /// `languages::extractor_for(language)`), keeping this module a leaf.
    pub fn new(
        file_path: impl Into<String>,
        source: &'a str,
        language: Option<Language>,
        extractor: Option<&'a dyn LanguageExtractor>,
    ) -> Self {
        let file_path = file_path.into();
        let language = language.unwrap_or_else(|| detect_language(&file_path, Some(source)));
        TreeSitterExtractor {
            file_path,
            language,
            source,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
            extractor,
            node_stack: Vec::new(),
            method_index: None,
        }
    }

    /// The language this extractor will parse as.
    pub fn language(&self) -> Language {
        self.language
    }

    /// Parse and extract from the source code
    pub fn extract(mut self) -> ExtractionResult {
        let start_time = now_ms();

        if !is_language_supported(self.language) {
            return ExtractionResult {
                nodes: vec![],
                edges: vec![],
                unresolved_references: vec![],
                errors: vec![ExtractionError {
                    message: format!("Unsupported language: {}", self.language),
                    file_path: Some(self.file_path.clone()),
                    line: None,
                    column: None,
                    severity: Severity::Error,
                    code: Some("unsupported_language".to_string()),
                }],
                duration_ms: (now_ms() - start_time) as f64,
            };
        }

        let Some(mut parser) = create_parser(self.language) else {
            return ExtractionResult {
                nodes: vec![],
                edges: vec![],
                unresolved_references: vec![],
                errors: vec![ExtractionError {
                    message: format!("Failed to get parser for language: {}", self.language),
                    file_path: Some(self.file_path.clone()),
                    line: None,
                    column: None,
                    severity: Severity::Error,
                    code: Some("parser_error".to_string()),
                }],
                duration_ms: (now_ms() - start_time) as f64,
            };
        };

        match parser.parse(self.source, None) {
            Some(tree) => {
                // Create file node representing the source file
                let file_node = Node {
                    id: format!("file:{}", self.file_path),
                    kind: NodeKind::File,
                    name: basename(&self.file_path),
                    qualified_name: self.file_path.clone(),
                    file_path: self.file_path.clone(),
                    language: self.language,
                    start_line: 1,
                    end_line: self.source.split('\n').count() as u32,
                    start_column: 0,
                    end_column: 0,
                    // The file node spans the whole source by definition.
                    start_byte: Some(0),
                    end_byte: Some(self.source.len() as u32),
                    docstring: None,
                    signature: None,
                    visibility: None,
                    is_exported: Some(false),
                    is_async: None,
                    is_static: None,
                    is_abstract: None,
                    decorators: None,
                    type_parameters: None,
                    updated_at: now_ms(),
                };
                let file_id = file_node.id.clone();
                self.nodes.push(file_node);

                // Push file node onto stack so top-level declarations get contains edges
                self.node_stack.push(file_id);

                // File-level package declaration (Kotlin/Java). Creates an implicit
                // `namespace` node wrapping every top-level declaration so their
                // qualifiedName carries the FQN — required for cross-file import
                // resolution on JVM languages where filename ≠ class name.
                let package_node_id = self.extract_file_package(tree.root_node());
                if let Some(ref pkg_id) = package_node_id {
                    self.node_stack.push(pkg_id.clone());
                }

                self.visit_node(tree.root_node());

                if package_node_id.is_some() {
                    self.node_stack.pop();
                }
                self.node_stack.pop();
            }
            None => {
                self.errors.push(ExtractionError {
                    message: "Parse error: Parser returned null tree".to_string(),
                    file_path: Some(self.file_path.clone()),
                    line: None,
                    column: None,
                    severity: Severity::Error,
                    code: Some("parse_error".to_string()),
                });
            }
        }

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: (now_ms() - start_time) as f64,
        }
    }

    /// Visit a node and extract information.
    ///
    /// Recursion guard: all `self.visit_node(child)` sites and the
    /// `ExtractorContext` hook funnel through here, so growing the stack at
    /// this head covers the whole `extract_*` family.
    fn visit_node(&mut self, node: SyntaxNode<'_>) {
        ensure_sufficient_stack(|| self.visit_node_inner(node));
    }

    fn visit_node_inner(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        let node_type = node.kind();
        let mut skip_children = false;

        // Language-specific custom visitor hook
        if ext.visit_node(node, self) {
            return;
        }

        // Pascal-specific AST handling
        if self.language == Language::Pascal && self.visit_pascal_node(node) {
            return;
        }

        // Check for function declarations
        // For Python/Ruby, function_definition inside a class should be treated as method
        if ext.function_types().contains(&node_type) {
            if self.is_inside_class_like_node() && ext.method_types().contains(&node_type) {
                // Inside a class - treat as method
                self.extract_method(node);
            } else {
                self.extract_function(node, None);
            }
            skip_children = true; // extract fn/method visits children via visit_function_body
        }
        // Check for class declarations
        else if ext.class_types().contains(&node_type) {
            // Some languages reuse class_declaration for structs/enums (e.g. Swift)
            match ext.classify_class_node(node, self.source) {
                ClassLikeKind::Struct => self.extract_struct(node),
                ClassLikeKind::Enum => self.extract_enum(node),
                ClassLikeKind::Interface => self.extract_interface(node),
                ClassLikeKind::Trait => self.extract_class(node, NodeKind::Trait),
                ClassLikeKind::Class => self.extract_class(node, NodeKind::Class),
            }
            skip_children = true; // extract_class visits body children
        }
        // Extra class node types (e.g. Dart mixin_declaration, extension_declaration)
        else if ext.extra_class_node_types().contains(&node_type) {
            self.extract_class(node, NodeKind::Class);
            skip_children = true;
        }
        // Check for method declarations (only if not already handled by functionTypes)
        else if ext.method_types().contains(&node_type) {
            self.extract_method(node);
            skip_children = true;
        }
        // Check for interface/protocol/trait declarations
        else if ext.interface_types().contains(&node_type) {
            self.extract_interface(node);
            skip_children = true;
        }
        // Check for struct declarations
        else if ext.struct_types().contains(&node_type) {
            self.extract_struct(node);
            skip_children = true;
        }
        // Check for enum declarations
        else if ext.enum_types().contains(&node_type) {
            self.extract_enum(node);
            skip_children = true;
        }
        // Check for type alias declarations (e.g. `type X = ...` in TypeScript)
        // For Go, type_spec wraps struct/interface definitions — resolve_type_alias_kind
        // detects these and extract_type_alias creates the correct node kind.
        else if ext.type_alias_types().contains(&node_type) {
            skip_children = self.extract_type_alias(node);
        }
        // Check for class properties (e.g. C# property_declaration)
        else if ext.property_types().contains(&node_type) && self.is_inside_class_like_node() {
            self.extract_property(node);
            skip_children = true;
        }
        // Check for class fields (e.g. Java field_declaration, C# field_declaration)
        else if ext.field_types().contains(&node_type) && self.is_inside_class_like_node() {
            self.extract_field(node);
            skip_children = true;
        }
        // Check for variable declarations (const, let, var, etc.)
        // Only extract top-level variables (not inside functions/methods)
        else if ext.variable_types().contains(&node_type) && !self.is_inside_class_like_node() {
            self.extract_variable(node);
            skip_children = true; // extract_variable handles children
        }
        // `export_statement` itself is not extracted — the walker descends
        // into children, where the inner declaration is dispatched to its
        // own extractor. `is_exported` walks the parent chain, so the
        // exported flag is preserved automatically.
        // Check for imports
        else if ext.import_types().contains(&node_type) {
            self.extract_import(node);
        }
        // Check for function calls
        else if ext.call_types().contains(&node_type) {
            self.extract_call(node);
        }
        // `new Foo(...)` / `Foo::new(...)` / object_creation_expression —
        // produce an `instantiates` reference. Children still walked so
        // nested calls inside the constructor args (`new Foo(bar())`) get
        // their own `calls` refs.
        else if INSTANTIATION_KINDS.contains(&node_type) {
            self.extract_instantiation(node);
            // Java/C# `new T(...) { ... }` — anonymous class with body. Without
            // extracting it as a class node + its methods, the interface→impl
            // synthesizer (Phase 5.5) can't bridge T's abstract methods to the
            // anonymous overrides.
            if let Some(anon_body) = self.find_anonymous_class_body(node) {
                self.extract_anonymous_class(node, anon_body);
                skip_children = true;
            }
        }
        // Rust: `impl Trait for Type { ... }` — creates implements edge from Type to Trait
        else if node_type == "impl_item" {
            self.extract_rust_impl_item(node);
        }
        // TypeScript interface members: property_signature (`foo: T`) and
        // method_signature (`foo(arg: A): R`) both carry type annotations the
        // interface walker would otherwise drop. Extract them as `references`
        // edges from the interface so resolvers can wire callers/impact for
        // types that only appear in interface members.
        else if (node_type == "property_signature" || node_type == "method_signature")
            && self.is_inside_class_like_node()
            && is_type_annotation_language(self.language)
        {
            if let Some(parent_id) = self.node_stack.last().cloned() {
                self.extract_type_annotations(node, &parent_id);
            }
            // don't skip_children — nested signatures still need traversal
        }

        // Visit children (unless the extract method already visited them)
        if !skip_children {
            for child in named_children(node) {
                self.visit_node(child);
            }
        }
    }

    /// Create a Node object
    fn create_node(
        &mut self,
        kind: NodeKind,
        name: &str,
        node: SyntaxNode<'_>,
        extra: NodeExtra,
    ) -> Option<Node> {
        // Skip nodes with empty/missing names — they are not meaningful symbols
        // and would cause FK violations when edges reference them (see issue #42)
        if name.is_empty() {
            return None;
        }

        let start_line = node.start_position().row as u32 + 1;
        let id = generate_node_id(&self.file_path, kind, name, start_line);

        // Some grammars (e.g. Dart) model a function/method body as a *sibling* of
        // the signature node, so the declaration node's own range is just the
        // signature line. Extend end_line to the resolved body when it sits beyond
        // the node so the node spans its body. Guarded to only ever extend.
        // end_byte is extended in lockstep so the byte range covers the body too.
        let mut end_line = node.end_position().row as u32 + 1;
        let mut end_byte = node.end_byte() as u32;
        if matches!(kind, NodeKind::Function | NodeKind::Method) {
            if let Some(ext) = self.extractor {
                if let Some(body) = ext.resolve_body(node, ext.body_field()) {
                    let body_end = body.end_position().row as u32 + 1;
                    if body_end > end_line {
                        end_line = body_end;
                    }
                    let body_end_byte = body.end_byte() as u32;
                    if body_end_byte > end_byte {
                        end_byte = body_end_byte;
                    }
                }
            }
        }

        let qualified_name = extra
            .qualified_name
            .unwrap_or_else(|| self.build_qualified_name(name));

        let new_node = Node {
            id: id.clone(),
            kind,
            name: name.to_string(),
            qualified_name,
            file_path: self.file_path.clone(),
            language: self.language,
            start_line,
            end_line,
            start_column: node.start_position().column as u32,
            end_column: node.end_position().column as u32,
            start_byte: Some(node.start_byte() as u32),
            end_byte: Some(end_byte),
            docstring: extra.docstring,
            signature: extra.signature,
            visibility: extra.visibility,
            is_exported: extra.is_exported,
            is_async: extra.is_async,
            is_static: extra.is_static,
            is_abstract: extra.is_abstract,
            decorators: None,
            type_parameters: None,
            updated_at: now_ms(),
        };

        self.nodes.push(new_node.clone());

        // Add containment edge from parent
        if let Some(parent_id) = self.node_stack.last() {
            self.edges
                .push(Edge::new(parent_id.clone(), id, EdgeKind::Contains));
        }

        Some(new_node)
    }

    /// Find first named child whose type is in the given list.
    /// Used to locate inner type nodes (e.g. enum_specifier inside a typedef).
    fn find_child_by_types<'t>(
        &self,
        node: SyntaxNode<'t>,
        types: &[&str],
    ) -> Option<SyntaxNode<'t>> {
        named_children(node)
            .into_iter()
            .find(|c| types.contains(&c.kind()))
    }

    /// Find a `package_types` child under the root, create a `namespace` node
    /// for it, and return its id so the caller can scope top-level
    /// declarations underneath. Returns None when no package header is
    /// present (script files, .kts without a package).
    fn extract_file_package(&mut self, root_node: SyntaxNode<'_>) -> Option<String> {
        let ext = self.extractor?;
        let types = ext.package_types();
        if types.is_empty() {
            return None;
        }

        let pkg_node = named_children(root_node)
            .into_iter()
            .find(|c| types.contains(&c.kind()))?;

        let pkg_name = ext.extract_package(pkg_node, self.source)?;
        if pkg_name.is_empty() {
            return None;
        }

        let ns = self.create_node(
            NodeKind::Namespace,
            &pkg_name,
            pkg_node,
            NodeExtra::default(),
        );
        ns.map(|n| n.id)
    }

    /// Build qualified name from node stack
    fn build_qualified_name(&self, name: &str) -> String {
        // Build a qualified name from the semantic hierarchy only (no file path).
        // The file path is stored separately in file_path and pollutes FTS if included here.
        let mut parts: Vec<&str> = Vec::new();
        for node_id in &self.node_stack {
            if let Some(node) = self.nodes.iter().find(|n| &n.id == node_id) {
                if node.kind != NodeKind::File {
                    parts.push(&node.name);
                }
            }
        }
        parts.push(name);
        parts.join("::")
    }

    /// Check if the current node stack indicates we are inside a class-like node
    /// (class, struct, interface, trait). File nodes do not count as class-like.
    fn is_inside_class_like_node(&self) -> bool {
        let Some(parent_id) = self.node_stack.last() else {
            return false;
        };
        let Some(parent_node) = self.nodes.iter().find(|n| &n.id == parent_id) else {
            return false;
        };
        matches!(
            parent_node.kind,
            NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Interface
                | NodeKind::Trait
                | NodeKind::Enum
                | NodeKind::Module
        )
    }

    /// Extract a function
    fn extract_function(&mut self, node: SyntaxNode<'_>, name_override: Option<&str>) {
        let Some(ext) = self.extractor else { return };

        // If the language provides get_receiver_type and this function has a receiver
        // (e.g., Rust function_item inside an impl block), extract as method instead
        if ext
            .get_receiver_type(node, self.source)
            .is_some_and(|r| !r.is_empty())
        {
            self.extract_method(node);
            return;
        }

        // name_override is supplied only for explicitly-named anonymous functions the
        // caller resolved itself (e.g. arrow values of exported-const object members
        // — SvelteKit actions). Inline-object arrows reached by the general walker
        // get no override, so they still fall through to the <anonymous> skip below.
        let mut name = match name_override {
            Some(n) => n.to_string(),
            None => extract_name(node, self.source, ext),
        };
        // For arrow functions and function expressions assigned to variables,
        // resolve the name from the parent variable_declarator.
        // e.g. `export const useAuth = () => { ... }` — the arrow_function node
        // has no `name` field; the name lives on the variable_declarator.
        if name_override.is_none()
            && name == "<anonymous>"
            && (node.kind() == "arrow_function" || node.kind() == "function_expression")
        {
            if let Some(parent) = node.parent() {
                if parent.kind() == "variable_declarator" {
                    if let Some(var_name) = get_child_by_field(parent, "name") {
                        name = get_node_text(var_name, self.source).to_string();
                    }
                }
            }
        }
        if name == "<anonymous>" {
            // Don't emit a node for the anonymous wrapper itself, but still visit its
            // body: AMD/RequireJS and CommonJS module wrappers (`define([], function(){…})`,
            // `(function(){…})()`) hold named inner functions and calls that would
            // otherwise be lost — the dispatcher set skip_children, so nothing else
            // descends into this subtree. (#528)
            let body = ext
                .resolve_body(node, ext.body_field())
                .or_else(|| get_child_by_field(node, ext.body_field()));
            if let Some(body) = body {
                self.visit_function_body(body, "");
            }
            return;
        }

        // Check for misparse artifacts (e.g. C++ macros causing "namespace detail" functions)
        // Skip the node but still visit the body for calls and structural nodes
        if ext.is_misparsed_function(&name, node) {
            let body = ext
                .resolve_body(node, ext.body_field())
                .or_else(|| get_child_by_field(node, ext.body_field()));
            if let Some(body) = body {
                self.visit_function_body(body, "");
            }
            return;
        }

        let docstring = get_preceding_docstring(node, self.source);
        let signature = ext.get_signature(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_exported = ext.is_exported(node, self.source);
        let is_async = ext.is_async(node, self.source);
        let is_static = ext.is_static(node, self.source);

        let Some(func_node) = self.create_node(
            NodeKind::Function,
            &name,
            node,
            NodeExtra {
                docstring,
                signature,
                visibility,
                is_exported,
                is_async,
                is_static,
                ..Default::default()
            },
        ) else {
            return;
        };

        // Extract type annotations (parameter types and return type)
        self.extract_type_annotations(node, &func_node.id);

        // Extract decorators applied to the function (rare in JS/TS but
        // present in Python `@decorator def f():` and Java/Kotlin
        // annotations on free functions).
        self.extract_decorators_for(node, &func_node.id);

        // Push to stack and visit body
        self.node_stack.push(func_node.id.clone());
        let body = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()));
        if let Some(body) = body {
            self.visit_function_body(body, &func_node.id);
        }
        self.node_stack.pop();
    }

    /// Extract a class
    fn extract_class(&mut self, node: SyntaxNode<'_>, kind: NodeKind) {
        let Some(ext) = self.extractor else { return };

        let name = extract_name(node, self.source, ext);
        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        let Some(class_node) = self.create_node(
            kind,
            &name,
            node,
            NodeExtra {
                docstring,
                visibility,
                is_exported,
                ..Default::default()
            },
        ) else {
            return;
        };

        // Extract extends/implements
        self.extract_inheritance(node, &class_node.id);

        // Extract decorators applied to the class (`@Foo class X {}`).
        self.extract_decorators_for(node, &class_node.id);

        // Push to stack and visit body
        self.node_stack.push(class_node.id.clone());
        let body = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()))
            .unwrap_or(node);

        // Visit all children for methods and properties
        for child in named_children(body) {
            self.visit_node(child);
        }
        self.node_stack.pop();
    }

    /// Extract a method
    fn extract_method(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        // For languages with receiver types (Go, Rust), include receiver in qualified name
        // so FTS can match "scrapeLoop.run" → qualified_name "...::scrapeLoop::run"
        let receiver_type = ext
            .get_receiver_type(node, self.source)
            .filter(|r| !r.is_empty());

        // For most languages, only extract as method if inside a class-like node
        // Languages with methods_are_top_level (e.g. Go) always treat them as methods
        // Languages with get_receiver_type (e.g. Rust) extract as method when receiver is found
        if !self.is_inside_class_like_node()
            && !ext.methods_are_top_level()
            && receiver_type.is_none()
        {
            // Skip method_definition nodes inside object literals (getters/setters/methods
            // in inline objects). These are ephemeral and create noise (e.g., Svelte context
            // objects: `ctx.set({ get view() { ... } })`).
            if node
                .parent()
                .is_some_and(|p| p.kind() == "object" || p.kind() == "object_expression")
            {
                let body = ext
                    .resolve_body(node, ext.body_field())
                    .or_else(|| get_child_by_field(node, ext.body_field()));
                if let Some(body) = body {
                    self.visit_function_body(body, "");
                }
                return;
            }
            // Not inside a class-like node and no receiver type, treat as function
            self.extract_function(node, None);
            return;
        }

        let name = extract_name(node, self.source, ext);

        // Check for misparse artifacts (e.g. C++ "switch" inside macro-confused class body)
        if ext.is_misparsed_function(&name, node) {
            let body = ext
                .resolve_body(node, ext.body_field())
                .or_else(|| get_child_by_field(node, ext.body_field()));
            if let Some(body) = body {
                self.visit_function_body(body, "");
            }
            return;
        }

        let docstring = get_preceding_docstring(node, self.source);
        let signature = ext.get_signature(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_async = ext.is_async(node, self.source);
        let is_static = ext.is_static(node, self.source);
        let qualified_name = receiver_type.as_ref().map(|r| format!("{}::{}", r, name));

        let Some(method_node) = self.create_node(
            NodeKind::Method,
            &name,
            node,
            NodeExtra {
                docstring,
                signature,
                visibility,
                is_async,
                is_static,
                qualified_name,
                ..Default::default()
            },
        ) else {
            return;
        };

        // For methods with a receiver type but no class-like parent on the stack
        // (e.g., Rust impl blocks), add a contains edge from the owning struct/trait
        if let Some(ref receiver) = receiver_type {
            if !self.is_inside_class_like_node() {
                let owner_id = self
                    .nodes
                    .iter()
                    .find(|n| {
                        n.name == *receiver
                            && n.file_path == self.file_path
                            && matches!(
                                n.kind,
                                NodeKind::Struct
                                    | NodeKind::Class
                                    | NodeKind::Enum
                                    | NodeKind::Trait
                            )
                    })
                    .map(|n| n.id.clone());
                if let Some(owner_id) = owner_id {
                    self.edges.push(Edge::new(
                        owner_id,
                        method_node.id.clone(),
                        EdgeKind::Contains,
                    ));
                }
            }
        }

        // Extract type annotations (parameter types and return type)
        self.extract_type_annotations(node, &method_node.id);

        // Extract decorators (`@Get('/list') list() {}`).
        self.extract_decorators_for(node, &method_node.id);

        // Push to stack and visit body
        self.node_stack.push(method_node.id.clone());
        let body = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()));
        if let Some(body) = body {
            self.visit_function_body(body, &method_node.id);
        }
        self.node_stack.pop();
    }

    /// Extract an interface/protocol/trait
    fn extract_interface(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        let name = extract_name(node, self.source, ext);
        let docstring = get_preceding_docstring(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        let kind = ext.interface_kind();

        let Some(interface_node) = self.create_node(
            kind,
            &name,
            node,
            NodeExtra {
                docstring,
                is_exported,
                ..Default::default()
            },
        ) else {
            return;
        };

        // Extract extends (interface inheritance)
        self.extract_inheritance(node, &interface_node.id);

        // Visit body children for interface methods and nested types
        self.node_stack.push(interface_node.id.clone());
        let body = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()))
            .unwrap_or(node);
        for child in named_children(body) {
            self.visit_node(child);
        }
        self.node_stack.pop();
    }

    /// Extract a struct
    fn extract_struct(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        // Skip forward declarations and type references (no body = not a definition)
        let Some(body) = get_child_by_field(node, ext.body_field()) else {
            return;
        };

        let name = extract_name(node, self.source, ext);
        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        let Some(struct_node) = self.create_node(
            NodeKind::Struct,
            &name,
            node,
            NodeExtra {
                docstring,
                visibility,
                is_exported,
                ..Default::default()
            },
        ) else {
            return;
        };

        // Extract inheritance (e.g. Swift: struct HTTPMethod: RawRepresentable)
        self.extract_inheritance(node, &struct_node.id);

        // Push to stack for field extraction
        self.node_stack.push(struct_node.id.clone());
        for child in named_children(body) {
            self.visit_node(child);
        }
        self.node_stack.pop();
    }

    /// Extract an enum
    fn extract_enum(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        // Skip forward declarations and type references (no body = not a definition)
        let Some(body) = ext
            .resolve_body(node, ext.body_field())
            .or_else(|| get_child_by_field(node, ext.body_field()))
        else {
            return;
        };

        let name = extract_name(node, self.source, ext);
        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        let Some(enum_node) = self.create_node(
            NodeKind::Enum,
            &name,
            node,
            NodeExtra {
                docstring,
                visibility,
                is_exported,
                ..Default::default()
            },
        ) else {
            return;
        };

        // Extract inheritance (e.g. Swift: enum AFError: Error)
        self.extract_inheritance(node, &enum_node.id);

        // Push to stack and visit body children (enum members, nested types, methods)
        self.node_stack.push(enum_node.id.clone());

        for child in named_children(body) {
            if ext.enum_member_types().contains(&child.kind()) {
                self.extract_enum_members(child);
            } else {
                self.visit_node(child);
            }
        }
        self.node_stack.pop();
    }

    /// Extract enum member names from an enum member node.
    /// Handles multi-case declarations (Swift: `case put, delete`) and single-case patterns.
    fn extract_enum_members(&mut self, node: SyntaxNode<'_>) {
        // Try field-based name first (e.g. Rust enum_variant has a 'name' field)
        if let Some(name_node) = get_child_by_field(node, "name") {
            let name = get_node_text(name_node, self.source).to_string();
            self.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
            return;
        }

        // Check for identifier-like children (Swift: simple_identifier, TS: property_identifier)
        let mut found = false;
        for child in named_children(node) {
            if matches!(
                child.kind(),
                "simple_identifier" | "identifier" | "property_identifier"
            ) {
                let name = get_node_text(child, self.source).to_string();
                self.create_node(NodeKind::EnumMember, &name, child, NodeExtra::default());
                found = true;
            }
        }

        // If the node itself IS the identifier (e.g. TS property_identifier directly in enum body)
        if !found && node.named_child_count() == 0 {
            let name = get_node_text(node, self.source).to_string();
            self.create_node(NodeKind::EnumMember, &name, node, NodeExtra::default());
        }
    }

    /// Extract a class property declaration (e.g. C# `public string Name { get; set; }`).
    /// Extracts as 'property' kind node inside the owning class.
    fn extract_property(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_static = ext.is_static(node, self.source).unwrap_or(false);

        let hook_name = ext.extract_property_name(node, self.source);
        let name = match hook_name {
            Some(h) => h,
            None => {
                let name_node = get_child_by_field(node, "name").or_else(|| {
                    named_children(node)
                        .into_iter()
                        .find(|c| c.kind() == "identifier")
                });
                match name_node {
                    Some(n) => get_node_text(n, self.source).to_string(),
                    None => return,
                }
            }
        };
        if name.is_empty() {
            return;
        }

        // Get property type from the type child (first named child that isn't modifier or identifier)
        let type_node = named_children(node).into_iter().find(|c| {
            !matches!(
                c.kind(),
                "modifier"
                    | "modifiers"
                    | "identifier"
                    | "accessor_list"
                    | "accessors"
                    | "equals_value_clause"
            )
        });
        let type_text = type_node.map(|t| get_node_text(t, self.source));
        let signature = match type_text {
            Some(t) => format!("{} {}", t, name),
            None => name.clone(),
        };

        let prop_node = self.create_node(
            NodeKind::Property,
            &name,
            node,
            NodeExtra {
                docstring,
                signature: Some(signature),
                visibility,
                is_static: Some(is_static),
                ..Default::default()
            },
        );

        // `@Inject() private svc: Foo` and similar — capture the
        // decorator->target relationship for class properties too.
        if let Some(prop_node) = prop_node {
            self.extract_decorators_for(node, &prop_node.id);
            // Emit `references` edges from the property to types named in its
            // type annotation (#381). The generic walker handles TS-style
            // `type_annotation` children; the C# branch walks the `type` field.
            self.extract_type_annotations(node, &prop_node.id);
        }
    }

    /// Extract a class field declaration (e.g. Java field_declaration, C# field_declaration).
    /// Extracts each declarator as a 'field' kind node inside the owning class.
    fn extract_field(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        let docstring = get_preceding_docstring(node, self.source);
        let visibility = ext.get_visibility(node, self.source);
        let is_static = ext.is_static(node, self.source).unwrap_or(false);

        // Java field_declaration: "private final String name = value;" → variable_declarator(s) are direct children
        // C# field_declaration: wraps in variable_declaration → variable_declarator(s)
        let mut declarators: Vec<SyntaxNode<'_>> = named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        // C#: look inside variable_declaration wrapper
        if declarators.is_empty() {
            if let Some(var_decl) = find_named_child(node, "variable_declaration") {
                declarators = named_children(var_decl)
                    .into_iter()
                    .filter(|c| c.kind() == "variable_declarator")
                    .collect();
            }
        }

        // PHP property_declaration: property_element → variable_name → name
        if declarators.is_empty() {
            let prop_elements: Vec<SyntaxNode<'_>> = named_children(node)
                .into_iter()
                .filter(|c| c.kind() == "property_element")
                .collect();
            if !prop_elements.is_empty() {
                // Get type annotation if present (e.g. "string", "int", "?Foo")
                let type_node = named_children(node).into_iter().find(|c| {
                    !matches!(
                        c.kind(),
                        "visibility_modifier"
                            | "static_modifier"
                            | "readonly_modifier"
                            | "property_element"
                            | "var_modifier"
                    )
                });
                let type_text = type_node.map(|t| get_node_text(t, self.source).to_string());

                for elem in prop_elements {
                    let var_name = find_named_child(elem, "variable_name");
                    let name_node = var_name.and_then(|v| find_named_child(v, "name"));
                    let Some(name_node) = name_node else { continue };
                    let name = get_node_text(name_node, self.source).to_string();
                    let signature = match &type_text {
                        Some(t) => format!("{} ${}", t, name),
                        None => format!("${}", name),
                    };
                    self.create_node(
                        NodeKind::Field,
                        &name,
                        elem,
                        NodeExtra {
                            docstring: docstring.clone(),
                            signature: Some(signature),
                            visibility,
                            is_static: Some(is_static),
                            ..Default::default()
                        },
                    );
                }
                return;
            }
        }

        if !declarators.is_empty() {
            // Get field type from the type child
            // Java: type is a direct child of field_declaration
            // C#: type is inside variable_declaration wrapper
            let var_decl = find_named_child(node, "variable_declaration");
            let type_search_node = var_decl.unwrap_or(node);
            let type_node = named_children(type_search_node).into_iter().find(|c| {
                !matches!(
                    c.kind(),
                    "modifiers"
                        | "modifier"
                        | "variable_declarator"
                        | "variable_declaration"
                        | "marker_annotation"
                        | "annotation"
                )
            });
            let type_text = type_node.map(|t| get_node_text(t, self.source).to_string());

            for decl in declarators {
                let name_node = get_child_by_field(decl, "name").or_else(|| {
                    named_children(decl)
                        .into_iter()
                        .find(|c| c.kind() == "identifier")
                });
                let Some(name_node) = name_node else { continue };
                let name = get_node_text(name_node, self.source).to_string();
                let signature = match &type_text {
                    Some(t) => format!("{} {}", t, name),
                    None => name.clone(),
                };
                let field_node = self.create_node(
                    NodeKind::Field,
                    &name,
                    decl,
                    NodeExtra {
                        docstring: docstring.clone(),
                        signature: Some(signature),
                        visibility,
                        is_static: Some(is_static),
                        ..Default::default()
                    },
                );
                // Java/Kotlin annotations / TS field decorators sit on the
                // outer field_declaration, not on the individual declarator.
                if let Some(field_node) = field_node {
                    self.extract_decorators_for(node, &field_node.id);
                    // Same as properties: emit `references` to the field's annotated
                    // type. The outer `field_declaration` is the right scope to
                    // search from (#381).
                    self.extract_type_annotations(node, &field_node.id);
                }
            }
        } else {
            // Fallback: try to find an identifier child directly
            let name_node = get_child_by_field(node, "name").or_else(|| {
                named_children(node)
                    .into_iter()
                    .find(|c| c.kind() == "identifier")
            });
            if let Some(name_node) = name_node {
                let name = get_node_text(name_node, self.source).to_string();
                self.create_node(
                    NodeKind::Field,
                    &name,
                    node,
                    NodeExtra {
                        docstring,
                        visibility,
                        is_static: Some(is_static),
                        ..Default::default()
                    },
                );
            }
        }
    }

    /// Extract function-valued properties of an object literal as named function
    /// nodes (named by their property key). Shared by the two object-of-functions
    /// shapes in extract_variable: the object as a direct const value, and the
    /// object returned by a store-initializer call. Handles both `key: () => {}` /
    /// `key: function() {}` pairs and method shorthand `key() {}`.
    fn extract_object_literal_functions(&mut self, obj: SyntaxNode<'_>) {
        for member in named_children(obj) {
            if member.kind() == "pair" {
                let key = get_child_by_field(member, "key");
                let value = get_child_by_field(member, "value");
                if let (Some(key), Some(value)) = (key, value) {
                    if value.kind() == "arrow_function" || value.kind() == "function_expression" {
                        let name = self.object_key_name(key);
                        self.extract_function(value, Some(&name));
                    }
                }
            } else if member.kind() == "method_definition" {
                // Method shorthand: `{ fetchUser() {...} }`. extract_method deliberately
                // skips object-literal methods, so route through extract_function with an
                // explicit name (method_definition exposes a `body` field, so resolve_body
                // falls through to it and the node spans the full method).
                if let Some(key) = get_child_by_field(member, "name") {
                    let name = self.object_key_name(key);
                    self.extract_function(member, Some(&name));
                }
            }
        }
    }

    /// Property-key text with surrounding quotes stripped (`'foo'` → `foo`).
    fn object_key_name(&self, key: SyntaxNode<'_>) -> String {
        let mut text = get_node_text(key, self.source).to_string();
        if text.starts_with('\'') || text.starts_with('"') || text.starts_with('`') {
            text.remove(0);
        }
        if text.ends_with('\'') || text.ends_with('"') || text.ends_with('`') {
            text.pop();
        }
        text
    }

    /// Given a `call_expression` initializer (`create((set, get) => ({...}))`),
    /// find the object literal RETURNED by a function argument — descending through
    /// nested call_expression arguments so middleware wrappers are unwrapped
    /// (`create(persist((set, get) => ({...}), {...}))`, devtools, immer,
    /// subscribeWithSelector). Returns None when no such object is found — the
    /// common case for ordinary call initializers — so this stays cheap and silent
    /// rather than guessing. Keyed purely on AST shape; no library names.
    fn find_initializer_returned_object<'t>(
        &self,
        call_node: SyntaxNode<'t>,
        depth: u32,
    ) -> Option<SyntaxNode<'t>> {
        if depth > 4 {
            return None;
        }
        let args = get_child_by_field(call_node, "arguments")?;
        for arg in named_children(args) {
            if arg.kind() == "arrow_function" || arg.kind() == "function_expression" {
                if let Some(obj) = self.function_returned_object(arg) {
                    return Some(obj);
                }
            } else if arg.kind() == "call_expression" {
                if let Some(obj) = self.find_initializer_returned_object(arg, depth + 1) {
                    return Some(obj);
                }
            }
        }
        None
    }

    /// The object literal a function expression returns — either the `=> ({...})`
    /// arrow form (a parenthesized_expression wrapping an object) or a
    /// `=> { return {...} }` block. Returns None for any other body shape.
    fn function_returned_object<'t>(&self, fn_node: SyntaxNode<'t>) -> Option<SyntaxNode<'t>> {
        let body = get_child_by_field(fn_node, "body")?;

        // Recursion guard: `(((...)))` parenthesis towers nest as deep as the
        // source allows, and this descends them off the guarded `visit_node`
        // path (reached via `extract_variable` → `find_initializer_returned_object`),
        // so without a guard a pathological initializer overflows the worker
        // stack — the same failure class as the unguarded walkers above.
        fn as_object<'t>(n: SyntaxNode<'t>) -> Option<SyntaxNode<'t>> {
            ensure_sufficient_stack(|| {
                if n.kind() == "object" || n.kind() == "object_expression" {
                    return Some(n);
                }
                if n.kind() == "parenthesized_expression" {
                    for inner in named_children(n) {
                        if let Some(obj) = as_object(inner) {
                            return Some(obj);
                        }
                    }
                }
                None
            })
        }

        // `(set, get) => ({...})` — body is the (parenthesized) object directly.
        if let Some(direct) = as_object(body) {
            return Some(direct);
        }
        // `(set, get) => { return {...} }` — scan top-level return statements.
        if body.kind() == "statement_block" {
            for stmt in named_children(body) {
                if stmt.kind() != "return_statement" {
                    continue;
                }
                for child in named_children(stmt) {
                    if let Some(obj) = as_object(child) {
                        return Some(obj);
                    }
                }
            }
        }
        None
    }

    /// Extract a variable declaration (const, let, var, etc.)
    ///
    /// Extracts top-level and module-level variable declarations.
    /// Captures the variable name and first 100 chars of initializer in signature for searchability.
    fn extract_variable(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        // Different languages have different variable declaration structures
        // TypeScript/JavaScript: lexical_declaration contains variable_declarator children
        // Python: assignment has left (identifier) and right (value)
        // Go: var_declaration, short_var_declaration, const_declaration

        let is_const = ext.is_const(node, self.source).unwrap_or(false);
        let kind = if is_const {
            NodeKind::Constant
        } else {
            NodeKind::Variable
        };
        let docstring = get_preceding_docstring(node, self.source);
        let is_exported = ext.is_exported(node, self.source).unwrap_or(false);

        // Extract variable declarators based on language
        if matches!(
            self.language,
            Language::Typescript | Language::Javascript | Language::Tsx | Language::Jsx
        ) {
            // Handle lexical_declaration and variable_declaration
            // These contain one or more variable_declarator children
            for child in named_children(node) {
                if child.kind() != "variable_declarator" {
                    continue;
                }
                let name_node = get_child_by_field(child, "name");
                let value_node = get_child_by_field(child, "value");

                let Some(name_node) = name_node else { continue };
                // Skip destructured patterns (e.g., `let { x, y } = $props()` in Svelte)
                // These produce ugly multi-line names like "{ class: className }"
                if name_node.kind() == "object_pattern" || name_node.kind() == "array_pattern" {
                    continue;
                }
                let name = get_node_text(name_node, self.source).to_string();
                // Arrow functions / function expressions: extract as function instead of variable
                if let Some(value) = value_node {
                    if value.kind() == "arrow_function" || value.kind() == "function_expression" {
                        self.extract_function(value, None);
                        continue;
                    }
                }

                // Capture first 100 chars of initializer for context (stored in signature for searchability)
                let init_sig = value_node.map(|v| init_signature(get_node_text(v, self.source)));

                let var_node = self.create_node(
                    kind,
                    &name,
                    child,
                    NodeExtra {
                        docstring: docstring.clone(),
                        signature: init_sig,
                        is_exported: Some(is_exported),
                        ..Default::default()
                    },
                );

                // Extract type annotation references (e.g., const x: ITextModel = ...)
                if let Some(ref var_node) = var_node {
                    self.extract_variable_type_annotation(child, &var_node.id);
                }

                // Exported const object-of-functions — extract each function-valued
                // property as a function named by its key + walk its body so its
                // calls are captured. Two shapes, both keyed on AST shape (not on any
                // library name):
                //   `export const actions = { default: async () => {} }` — object is
                //     the DIRECT value (SvelteKit form actions / handler maps / route
                //     tables).
                //   `export const useStore = create((set, get) => ({ fetchUser:
                //     async () => {} }))` — object is RETURNED by an initializer call,
                //     possibly through middleware wrappers (persist/devtools/immer).
                //     Covers Zustand/Redux/Pinia/MobX stores generically.
                // Scoped to EXPORTED consts to exclude inline-object noise
                // (`ctx.set({...})`) the object-method skip deliberately avoids.
                let object_of_fns = match value_node {
                    Some(v) if v.kind() == "object" || v.kind() == "object_expression" => Some(v),
                    Some(v) if v.kind() == "call_expression" => {
                        self.find_initializer_returned_object(v, 0)
                    }
                    _ => None,
                };
                let extract_object_methods = is_exported && object_of_fns.is_some();

                // Visit the initializer body for calls — EXCEPT object literals (their
                // function-valued properties are extracted below) and the store-factory
                // call whose returned object we extract method-by-method below (walking
                // the whole call would re-visit those method arrows and mis-attribute
                // their inner calls to the file/module scope).
                if let Some(value) = value_node {
                    if value.kind() != "object"
                        && value.kind() != "object_expression"
                        && !(extract_object_methods && value.kind() == "call_expression")
                    {
                        self.visit_function_body(value, "");
                    }
                }

                if extract_object_methods {
                    if let Some(obj) = object_of_fns {
                        self.extract_object_literal_functions(obj);
                    }
                }
            }
        } else if matches!(self.language, Language::Python | Language::Ruby) {
            // Python/Ruby assignment: left = right
            let left = get_child_by_field(node, "left").or_else(|| node.named_child(0));
            let right = get_child_by_field(node, "right").or_else(|| node.named_child(1));

            if let Some(left) = left {
                if left.kind() == "identifier" {
                    let name = get_node_text(left, self.source).to_string();
                    // Python constants are usually UPPER_CASE
                    let init_sig = right.map(|r| init_signature(get_node_text(r, self.source)));

                    self.create_node(
                        kind,
                        &name,
                        node,
                        NodeExtra {
                            docstring,
                            signature: init_sig,
                            ..Default::default()
                        },
                    );
                }
            }
        } else if self.language == Language::Go {
            // Go: var_declaration, short_var_declaration, const_declaration
            // These can have multiple identifiers on the left
            let specs: Vec<SyntaxNode<'_>> = named_children(node)
                .into_iter()
                .filter(|c| c.kind() == "var_spec" || c.kind() == "const_spec")
                .collect();

            for spec in specs {
                let name_node = spec.named_child(0);
                if let Some(name_node) = name_node {
                    if name_node.kind() == "identifier" {
                        let name = get_node_text(name_node, self.source).to_string();
                        let value_node = if spec.named_child_count() > 1 {
                            spec.named_child(spec.named_child_count() as u32 - 1)
                        } else {
                            None
                        };
                        let init_sig =
                            value_node.map(|v| init_signature(get_node_text(v, self.source)));
                        let spec_kind = if node.kind() == "const_declaration" {
                            NodeKind::Constant
                        } else {
                            NodeKind::Variable
                        };

                        self.create_node(
                            spec_kind,
                            &name,
                            spec,
                            NodeExtra {
                                docstring: docstring.clone(),
                                signature: init_sig,
                                ..Default::default()
                            },
                        );
                    }
                }
            }

            // Handle short_var_declaration (:=)
            if node.kind() == "short_var_declaration" {
                let left = get_child_by_field(node, "left");
                let right = get_child_by_field(node, "right");

                if let Some(left) = left {
                    // Can be expression_list with multiple identifiers
                    let identifiers: Vec<SyntaxNode<'_>> = if left.kind() == "expression_list" {
                        named_children(left)
                            .into_iter()
                            .filter(|c| c.kind() == "identifier")
                            .collect()
                    } else {
                        vec![left]
                    };

                    for id in identifiers {
                        let name = get_node_text(id, self.source).to_string();
                        let init_sig = right.map(|r| init_signature(get_node_text(r, self.source)));

                        self.create_node(
                            NodeKind::Variable,
                            &name,
                            node,
                            NodeExtra {
                                docstring: docstring.clone(),
                                signature: init_sig,
                                ..Default::default()
                            },
                        );
                    }
                }
            }
        } else if matches!(self.language, Language::Lua | Language::Luau) {
            // Lua/Luau: variable_declaration → assignment_statement → variable_list
            //      (name: identifier...) = expression_list. `local x, y = 1, 2`
            //      declares multiple names; only plain identifiers are locals.
            let assign = find_named_child(node, "assignment_statement").unwrap_or(node);
            let var_list = find_named_child(assign, "variable_list");
            let expr_list = find_named_child(assign, "expression_list");
            let values: Vec<SyntaxNode<'_>> = expr_list.map(named_children).unwrap_or_default();
            let names: Vec<SyntaxNode<'_>> = var_list
                .map(|v| {
                    named_children(v)
                        .into_iter()
                        .filter(|c| c.kind() == "identifier")
                        .collect()
                })
                .unwrap_or_default();
            for (i, name_node) in names.into_iter().enumerate() {
                let name = get_node_text(name_node, self.source).to_string();
                if name.is_empty() {
                    continue;
                }
                let init_sig = values
                    .get(i)
                    .map(|v| init_signature(get_node_text(*v, self.source)));
                self.create_node(
                    kind,
                    &name,
                    name_node,
                    NodeExtra {
                        docstring: docstring.clone(),
                        signature: init_sig,
                        is_exported: Some(is_exported),
                        ..Default::default()
                    },
                );
            }
        } else {
            // Generic fallback for other languages
            // Try to find identifier children
            for child in named_children(node) {
                if child.kind() == "identifier" || child.kind() == "variable_declarator" {
                    let name = if child.kind() == "identifier" {
                        get_node_text(child, self.source).to_string()
                    } else {
                        extract_name(child, self.source, ext)
                    };

                    if !name.is_empty() && name != "<anonymous>" {
                        self.create_node(
                            kind,
                            &name,
                            child,
                            NodeExtra {
                                docstring: docstring.clone(),
                                is_exported: Some(is_exported),
                                ..Default::default()
                            },
                        );
                    }
                }
            }
        }
    }

    /// Extract a type alias (e.g. `export type X = ...` in TypeScript).
    /// For languages like Go, resolve_type_alias_kind detects when the type_spec
    /// wraps a struct or interface definition and creates the correct node kind.
    /// Returns true if children should be skipped (struct/interface handled body visiting).
    fn extract_type_alias(&mut self, node: SyntaxNode<'_>) -> bool {
        let Some(ext) = self.extractor else {
            return false;
        };

        let name = extract_name(node, self.source, ext);
        if name == "<anonymous>" {
            return false;
        }
        let docstring = get_preceding_docstring(node, self.source);
        let is_exported = ext.is_exported(node, self.source);

        // Check if this type alias is actually a struct or interface definition
        // (e.g. Go: `type Foo struct { ... }` is a type_spec wrapping struct_type)
        let resolved_kind = ext.resolve_type_alias_kind(node, self.source);

        if resolved_kind == Some(NodeKind::Struct) {
            let Some(struct_node) = self.create_node(
                NodeKind::Struct,
                &name,
                node,
                NodeExtra {
                    docstring,
                    is_exported,
                    ..Default::default()
                },
            ) else {
                return true;
            };
            // Visit body children for field extraction
            self.node_stack.push(struct_node.id.clone());
            // Try Go-style 'type' field first, then find inner struct child (C typedef struct)
            let type_child = get_child_by_field(node, "type")
                .or_else(|| self.find_child_by_types(node, ext.struct_types()));
            if let Some(type_child) = type_child {
                // Extract struct embedding (e.g. Go: `type DB struct { *Head; Queryable }`)
                self.extract_inheritance(type_child, &struct_node.id);
                let body = get_child_by_field(type_child, ext.body_field()).unwrap_or(type_child);
                for child in named_children(body) {
                    self.visit_node(child);
                }
            }
            self.node_stack.pop();
            return true;
        }

        if resolved_kind == Some(NodeKind::Enum) {
            let Some(enum_node) = self.create_node(
                NodeKind::Enum,
                &name,
                node,
                NodeExtra {
                    docstring,
                    is_exported,
                    ..Default::default()
                },
            ) else {
                return true;
            };
            self.node_stack.push(enum_node.id.clone());
            // Find the inner enum type child (e.g. C: typedef enum { ... } name)
            let inner_enum = self.find_child_by_types(node, ext.enum_types());
            if let Some(inner_enum) = inner_enum {
                self.extract_inheritance(inner_enum, &enum_node.id);
                let body = ext
                    .resolve_body(inner_enum, ext.body_field())
                    .or_else(|| get_child_by_field(inner_enum, ext.body_field()));
                if let Some(body) = body {
                    for child in named_children(body) {
                        if ext.enum_member_types().contains(&child.kind()) {
                            self.extract_enum_members(child);
                        } else {
                            self.visit_node(child);
                        }
                    }
                }
            }
            self.node_stack.pop();
            return true;
        }

        if resolved_kind == Some(NodeKind::Interface) {
            let kind = ext.interface_kind();
            let Some(interface_node) = self.create_node(
                kind,
                &name,
                node,
                NodeExtra {
                    docstring,
                    is_exported,
                    ..Default::default()
                },
            ) else {
                return true;
            };
            // Extract interface inheritance from the inner type node
            if let Some(type_child) = get_child_by_field(node, "type") {
                self.extract_inheritance(type_child, &interface_node.id);
            }
            return true;
        }

        let type_alias_node = self.create_node(
            NodeKind::TypeAlias,
            &name,
            node,
            NodeExtra {
                docstring,
                is_exported,
                ..Default::default()
            },
        );

        // Extract type references from the alias value (e.g., `type X = ITextModel | null`)
        if let Some(type_alias_node) = type_alias_node {
            if is_type_annotation_language(self.language) {
                // The value is everything after the `=`, which is typically the last named child
                // In tree-sitter TS: type_alias_declaration has name + value children
                if let Some(value) = get_child_by_field(node, "value") {
                    self.extract_type_refs_from_subtree(value, &type_alias_node.id);
                    // `type X = { foo: T; bar(): T }` — make the members first-class
                    // property/method nodes under the type alias so `recorder.stop()`
                    // can attach the call edge to `RecorderHandle.stop` instead of
                    // an unrelated class method picked by path-proximity (#359).
                    if matches!(self.language, Language::Typescript | Language::Tsx) {
                        self.extract_ts_type_alias_members(value, &type_alias_node);
                    }
                }
            }
        }
        false
    }

    /// Surface the members of a TypeScript `type X = { ... }` (or intersection
    /// thereof) as `property` / `method` nodes under the type-alias node. Only
    /// walks the immediate object_type / intersection operands so anonymous
    /// nested object types inside generic arguments (`Promise<{ ok: true }>`)
    /// don't produce phantom members.
    fn extract_ts_type_alias_members(&mut self, value: SyntaxNode<'_>, type_alias_node: &Node) {
        let mut object_types: Vec<SyntaxNode<'_>> = Vec::new();
        if value.kind() == "object_type" {
            object_types.push(value);
        } else if value.kind() == "intersection_type" {
            for op in named_children(value) {
                if op.kind() == "object_type" {
                    object_types.push(op);
                }
            }
        } else {
            return;
        }

        self.node_stack.push(type_alias_node.id.clone());
        for obj_type in object_types {
            for child in named_children(obj_type) {
                if child.kind() != "property_signature" && child.kind() != "method_signature" {
                    continue;
                }

                let member_name = get_child_by_field(child, "name")
                    .map(|n| get_node_text(n, self.source).to_string())
                    .unwrap_or_default();
                if member_name.is_empty() {
                    continue;
                }

                // `foo: () => T` and `foo(): T` are functionally a method on the
                // type contract. Treat the property_signature with a function-typed
                // annotation as a method too so call sites can resolve to it.
                // (Mirrors the TS chained ternary; the two Method arms are
                // deliberately distinct cases.)
                #[allow(clippy::if_same_then_else)]
                let member_kind = if child.kind() == "method_signature" {
                    NodeKind::Method
                } else if self.is_ts_function_typed_property(child) {
                    NodeKind::Method
                } else {
                    NodeKind::Property
                };

                let docstring = get_preceding_docstring(child, self.source);
                let signature = get_node_text(child, self.source).to_string();
                self.create_node(
                    member_kind,
                    &member_name,
                    child,
                    NodeExtra {
                        docstring,
                        signature: Some(signature),
                        qualified_name: Some(format!("{}::{}", type_alias_node.name, member_name)),
                        ..Default::default()
                    },
                );

                // Emit `references` edges from the type alias to types named in the
                // member's signature, matching the interface-member behavior added in
                // #432. We attach refs to the type-alias parent (consistent with
                // interface property_signature treatment).
                self.extract_type_annotations(child, &type_alias_node.id);
            }
        }
        self.node_stack.pop();
    }

    /// `foo: () => T` → property_signature whose type_annotation contains a
    /// `function_type`. Treat that as a method-shaped contract member, since
    /// the call site `obj.foo()` has identical semantics to `bar(): T`.
    fn is_ts_function_typed_property(&self, property_signature: SyntaxNode<'_>) -> bool {
        let Some(type_anno) = get_child_by_field(property_signature, "type") else {
            return false;
        };
        named_children(type_anno)
            .into_iter()
            .any(|inner| inner.kind() == "function_type")
    }

    /// Extract an import
    ///
    /// Creates an import node with the full import statement stored in signature for searchability.
    /// Also creates unresolved references for resolution purposes.
    fn extract_import(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };

        let import_text = get_node_text(node, self.source).trim().to_string();

        // Try language-specific hook first
        let mut hook_declined = false;
        match ext.extract_import(node, self.source) {
            ImportOutcome::Info(info) => {
                self.create_node(
                    NodeKind::Import,
                    &info.module_name,
                    node,
                    NodeExtra {
                        signature: Some(info.signature),
                        ..Default::default()
                    },
                );
                // Create unresolved reference unless the hook handled it
                if !info.handled_refs && !info.module_name.is_empty() {
                    if let Some(parent_id) = self.node_stack.last().cloned() {
                        self.unresolved_references.push(UnresolvedReference {
                            from_node_id: parent_id,
                            reference_name: info.module_name,
                            reference_kind: EdgeKind::Imports,
                            line: node.start_position().row as u32 + 1,
                            column: node.start_position().column as u32,
                            file_path: None,
                            language: None,
                            candidates: None,
                        });
                    }
                }
                return;
            }
            // Hook returned null — fall through to multi-import inline handlers only
            // (hook declining means "I didn't handle this" for multi-import cases,
            // NOT "use generic fallback" — the hook already declined)
            ImportOutcome::Declined => hook_declined = true,
            ImportOutcome::NotHandled => {}
        }

        // Multi-import cases that create multiple nodes (can't be expressed with single-return hook)

        // Python import_statement: import os, sys (creates one import per module)
        if self.language == Language::Python && node.kind() == "import_statement" {
            for child in named_children(node) {
                if child.kind() == "dotted_name" {
                    let name = get_node_text(child, self.source).to_string();
                    self.create_node(
                        NodeKind::Import,
                        &name,
                        node,
                        NodeExtra {
                            signature: Some(import_text.clone()),
                            ..Default::default()
                        },
                    );
                } else if child.kind() == "aliased_import" {
                    if let Some(dotted_name) = find_named_child(child, "dotted_name") {
                        let name = get_node_text(dotted_name, self.source).to_string();
                        self.create_node(
                            NodeKind::Import,
                            &name,
                            node,
                            NodeExtra {
                                signature: Some(import_text.clone()),
                                ..Default::default()
                            },
                        );
                    }
                }
            }
            return;
        }

        // Go imports: single or grouped (creates one import per spec)
        if self.language == Language::Go {
            let parent_id = self.node_stack.last().cloned();

            if let Some(import_spec_list) = find_named_child(node, "import_spec_list") {
                let specs: Vec<SyntaxNode<'_>> = named_children(import_spec_list)
                    .into_iter()
                    .filter(|c| c.kind() == "import_spec")
                    .collect();
                for spec in specs {
                    self.extract_go_import_spec(spec, parent_id.as_deref());
                }
            } else if let Some(import_spec) = find_named_child(node, "import_spec") {
                self.extract_go_import_spec(import_spec, parent_id.as_deref());
            }
            return;
        }

        // PHP grouped imports: use X\{A, B} (creates one import per item)
        if self.language == Language::Php {
            let namespace_prefix = find_named_child(node, "namespace_name");
            let use_group = find_named_child(node, "namespace_use_group");
            if let (Some(namespace_prefix), Some(use_group)) = (namespace_prefix, use_group) {
                let prefix = get_node_text(namespace_prefix, self.source).to_string();
                let use_clauses: Vec<SyntaxNode<'_>> = named_children(use_group)
                    .into_iter()
                    .filter(|c| {
                        c.kind() == "namespace_use_group_clause"
                            || c.kind() == "namespace_use_clause"
                    })
                    .collect();
                for clause in use_clauses {
                    let ns_name = find_named_child(clause, "namespace_name");
                    let name = match ns_name {
                        Some(ns) => find_named_child(ns, "name"),
                        None => find_named_child(clause, "name"),
                    };
                    if let Some(name) = name {
                        let full_path = format!("{}\\{}", prefix, get_node_text(name, self.source));
                        self.create_node(
                            NodeKind::Import,
                            &full_path,
                            node,
                            NodeExtra {
                                signature: Some(import_text.clone()),
                                ..Default::default()
                            },
                        );
                    }
                }
                return;
            }
        }

        // If a hook exists but returned null, it intentionally declined this node — don't create fallback
        if hook_declined {
            return;
        }

        // Generic fallback for languages without hooks
        self.create_node(
            NodeKind::Import,
            &import_text.clone(),
            node,
            NodeExtra {
                signature: Some(import_text),
                ..Default::default()
            },
        );
    }

    /// One Go import spec → import node + unresolved `imports` reference.
    fn extract_go_import_spec(&mut self, spec: SyntaxNode<'_>, parent_id: Option<&str>) {
        let Some(string_literal) = find_named_child(spec, "interpreted_string_literal") else {
            return;
        };
        let import_path = get_node_text(string_literal, self.source).replace(['\'', '"'], "");
        if import_path.is_empty() {
            return;
        }
        let signature = get_node_text(spec, self.source).trim().to_string();
        self.create_node(
            NodeKind::Import,
            &import_path,
            spec,
            NodeExtra {
                signature: Some(signature),
                ..Default::default()
            },
        );
        // Create unresolved reference so the resolver can create imports edges
        if let Some(parent_id) = parent_id {
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: parent_id.to_string(),
                reference_name: import_path,
                reference_kind: EdgeKind::Imports,
                line: spec.start_position().row as u32 + 1,
                column: spec.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
            });
        }
    }

    /// Extract a function call
    fn extract_call(&mut self, node: SyntaxNode<'_>) {
        let Some(caller_id) = self.node_stack.last().cloned() else {
            return;
        };

        // Get the function/method being called
        let mut callee_name = String::new();

        // Java/Kotlin method_invocation has 'object' + 'name' fields instead of 'function'
        // PHP member_call_expression has 'object' + 'name', scoped_call_expression has 'scope' + 'name'
        let name_field = get_child_by_field(node, "name");
        let object_field =
            get_child_by_field(node, "object").or_else(|| get_child_by_field(node, "scope"));
        let node_type = node.kind();

        let is_receiver_call = name_field.is_some()
            && object_field.is_some()
            && matches!(
                node_type,
                "method_invocation" | "member_call_expression" | "scoped_call_expression"
            );

        if is_receiver_call {
            let name_field = name_field.unwrap();
            let object_field = object_field.unwrap();
            // Method call with explicit receiver: receiver.method() / $receiver->method() / ClassName::method()
            let method_name = get_node_text(name_field, self.source).to_string();
            // Java `this.userbo.toLogin2()` parses as method_invocation(object=field_access(this, userbo)).
            // Without unwrapping, receiver_name is `this.userbo` and the name-matcher's
            // single-dot receiver regex fails. Pull out the immediate field after `this.`
            // so the receiver is the field name (`userbo`), which the resolver can then
            // look up in the enclosing class's field declarations.
            let mut receiver_name: String = if object_field.kind() == "field_access" {
                let inner = get_child_by_field(object_field, "object");
                let fld = get_child_by_field(object_field, "field");
                match (inner, fld) {
                    (Some(inner), Some(fld))
                        if inner.kind() == "this" || inner.kind() == "this_expression" =>
                    {
                        get_node_text(fld, self.source).to_string()
                    }
                    _ => get_node_text(object_field, self.source).to_string(),
                }
            } else {
                get_node_text(object_field, self.source).to_string()
            };
            // Strip PHP $ prefix from variable names
            if let Some(stripped) = receiver_name.strip_prefix('$') {
                receiver_name = stripped.to_string();
            }

            if !method_name.is_empty() {
                // Skip self/this/parent/static receivers — they don't aid resolution
                const SKIP_RECEIVERS: &[&str] =
                    &["self", "this", "cls", "super", "parent", "static"];
                if SKIP_RECEIVERS.contains(&receiver_name.as_str()) {
                    callee_name = method_name;
                } else {
                    callee_name = format!("{}.{}", receiver_name, method_name);
                }
            }
        } else if node_type == "message_expression" {
            // ObjC message expressions emit one `method` field child per selector
            // keyword: `[obj a:1 b:2 c:3]` has three `method=identifier` siblings.
            // Joining them with `:` reconstructs the full selector and matches the
            // multi-part selector names produced by the ObjC method_definition
            // extractor. Without this join, multi-keyword call sites only emitted
            // the first keyword and never resolved to their target methods.
            let mut method_keywords: Vec<String> = Vec::new();
            for i in 0..node.named_child_count() as u32 {
                if node.field_name_for_named_child(i) == Some("method") {
                    if let Some(kw) = node.named_child(i) {
                        method_keywords.push(get_node_text(kw, self.source).to_string());
                    }
                }
            }
            if !method_keywords.is_empty() {
                let method_name: String = if method_keywords.len() == 1 {
                    method_keywords[0].clone()
                } else {
                    method_keywords
                        .iter()
                        .map(|k| format!("{}:", k))
                        .collect::<Vec<_>>()
                        .join("")
                };
                let receiver_field = get_child_by_field(node, "receiver");
                const SKIP_RECEIVERS: &[&str] = &["self", "super"];
                match receiver_field {
                    Some(receiver) if receiver.kind() != "message_expression" => {
                        let receiver_name = get_node_text(receiver, self.source);
                        if !receiver_name.is_empty() && !SKIP_RECEIVERS.contains(&receiver_name) {
                            callee_name = format!("{}.{}", receiver_name, method_name);
                        } else {
                            callee_name = method_name;
                        }
                    }
                    _ => {
                        callee_name = method_name;
                    }
                }
            }
        } else {
            let func = get_child_by_field(node, "function").or_else(|| node.named_child(0));

            if let Some(func) = func {
                if matches!(
                    func.kind(),
                    "member_expression"
                        | "attribute"
                        | "selector_expression"
                        | "navigation_expression"
                        | "field_expression"
                ) {
                    // Method call: obj.method() or obj.field.method()
                    // Go uses selector_expression with 'field', JS/TS uses member_expression with 'property'
                    // Kotlin uses navigation_expression with navigation_suffix > simple_identifier
                    // C/C++ use field_expression for both `obj.method()` and `ptr->method()`
                    let mut property = get_child_by_field(func, "property")
                        .or_else(|| get_child_by_field(func, "field"));
                    if property.is_none() {
                        let child1 = func.named_child(1);
                        // Kotlin: navigation_suffix wraps the method name — extract simple_identifier from it
                        property = match child1 {
                            Some(c1) if c1.kind() == "navigation_suffix" => {
                                Some(find_named_child(c1, "simple_identifier").unwrap_or(c1))
                            }
                            other => other,
                        };
                    }
                    if let Some(property) = property {
                        let method_name = get_node_text(property, self.source).to_string();
                        // Include receiver name for qualified resolution (e.g., console.print → "console.print")
                        // This helps the resolver distinguish method calls from bare function calls.
                        // Skip self/this/cls as they don't aid resolution
                        let receiver = get_child_by_field(func, "object")
                            .or_else(|| get_child_by_field(func, "operand"))
                            .or_else(|| get_child_by_field(func, "argument"))
                            .or_else(|| func.named_child(0));
                        const SKIP_RECEIVERS: &[&str] = &["self", "this", "cls", "super"];
                        match receiver {
                            Some(receiver)
                                if matches!(
                                    receiver.kind(),
                                    "identifier" | "simple_identifier" | "field_identifier"
                                ) =>
                            {
                                let receiver_name = get_node_text(receiver, self.source);
                                if !SKIP_RECEIVERS.contains(&receiver_name) {
                                    callee_name = format!("{}.{}", receiver_name, method_name);
                                } else {
                                    callee_name = method_name;
                                }
                            }
                            _ => {
                                callee_name = method_name;
                            }
                        }
                    }
                } else if func.kind() == "scoped_identifier"
                    || func.kind() == "scoped_call_expression"
                {
                    // Scoped call: Module::function()
                    callee_name = get_node_text(func, self.source).to_string();
                } else {
                    callee_name = get_node_text(func, self.source).to_string();
                }
            }
        }

        if !callee_name.is_empty() {
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: caller_id,
                reference_name: callee_name,
                reference_kind: EdgeKind::Calls,
                line: node.start_position().row as u32 + 1,
                column: node.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
            });
        }
    }

    /// `new Foo(...)` / `Foo::new(...)` / object_creation_expression —
    /// emit an `instantiates` reference to the class name. The resolver
    /// then links it to the class node, producing the `instantiates`
    /// edge that powers "what creates instances of X" queries.
    ///
    /// Children are still walked so nested calls inside the constructor
    /// arguments (`new Foo(bar())`) get their own `calls` references.
    fn extract_instantiation(&mut self, node: SyntaxNode<'_>) {
        let Some(from_id) = self.node_stack.last().cloned() else {
            return;
        };

        // The class name is in the `constructor`/`type`/first-named-child
        // depending on grammar.
        let ctor = get_child_by_field(node, "constructor")
            .or_else(|| get_child_by_field(node, "type"))
            .or_else(|| get_child_by_field(node, "name"))
            .or_else(|| node.named_child(0));
        let Some(ctor) = ctor else { return };

        let mut class_name = get_node_text(ctor, self.source).to_string();
        // Strip type-argument suffix first: `new Map<K, V>()` would
        // otherwise produce class_name 'Map<K, V>' (the constructor
        // field is a `generic_type` node) and resolution would fail
        // because no class is named with the angle-bracket suffix.
        if let Some(lt_idx) = class_name.find('<') {
            if lt_idx > 0 {
                class_name.truncate(lt_idx);
            }
        }
        // For namespaced/qualified constructors (`new ns.Foo()`,
        // `new ns::Foo()`) keep the trailing identifier — that's what
        // matches a class node in the index.
        class_name = strip_qualifier(&class_name);
        class_name = class_name.trim().to_string();

        if !class_name.is_empty() {
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: from_id,
                reference_name: class_name,
                reference_kind: EdgeKind::Instantiates,
                line: node.start_position().row as u32 + 1,
                column: node.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
            });
        }
    }

    /// Find a `class_body` child of an `object_creation_expression` — the
    /// marker for an anonymous class (`new T() { ... }`). Returns the body
    /// node so the caller can walk it as the anon class's members.
    fn find_anonymous_class_body<'t>(&self, node: SyntaxNode<'t>) -> Option<SyntaxNode<'t>> {
        named_children(node)
            .into_iter()
            // Java: `class_body`. C# uses the same node kind.
            .find(|c| c.kind() == "class_body" || c.kind() == "declaration_list")
    }

    /// Extract a Java/C# anonymous class — `new T() { ...members }`. Emits a
    /// `class` node named `<T$anon@line>`, an `extends` reference to T (so
    /// Phase 5.5 interface-impl can bridge), and walks the body so its
    /// `method_declaration` members become method nodes under the anon class.
    ///
    /// Why this matters: without anon-class extraction, the overrides inside
    /// a lambda-returned `new T() { @Override int foo(){...} }` are not nodes,
    /// so a call through T.foo (the abstract parent method) has no static
    /// target — the agent has to Read the file to find the implementation.
    fn extract_anonymous_class(&mut self, node: SyntaxNode<'_>, body: SyntaxNode<'_>) {
        if self.extractor.is_none() {
            return;
        }

        // The instantiated type sits in the same field/position that
        // extract_instantiation reads from. Use the same lookup so the anon
        // class's `extends` target matches the `instantiates` edge.
        let type_node = get_child_by_field(node, "constructor")
            .or_else(|| get_child_by_field(node, "type"))
            .or_else(|| get_child_by_field(node, "name"))
            .or_else(|| node.named_child(0));
        let mut type_name = type_node
            .map(|t| get_node_text(t, self.source).to_string())
            .unwrap_or_else(|| "Object".to_string());
        if let Some(lt_idx) = type_name.find('<') {
            if lt_idx > 0 {
                type_name.truncate(lt_idx);
            }
        }
        type_name = strip_qualifier(&type_name);
        type_name = type_name.trim().to_string();
        if type_name.is_empty() {
            type_name = "Object".to_string();
        }

        let anon_name = format!("<{}$anon@{}>", type_name, node.start_position().row + 1);
        let Some(class_node) =
            self.create_node(NodeKind::Class, &anon_name, node, NodeExtra::default())
        else {
            return;
        };

        // The anonymous class implicitly extends/implements the named type.
        // We can't tell at extraction time whether T is a class or an interface,
        // so emit `extends`. Resolution will still bind T to whatever it is, and
        // Phase 5.5 (which already handles both `extends` and `implements`) will
        // bridge T's methods to the override names found in the anon body.
        // (TS quirk preserved: this reference's line is NOT +1-adjusted.)
        self.unresolved_references.push(UnresolvedReference {
            from_node_id: class_node.id.clone(),
            reference_name: type_name,
            reference_kind: EdgeKind::Extends,
            line: type_node
                .map(|t| t.start_position().row)
                .unwrap_or_else(|| node.start_position().row) as u32,
            column: type_node
                .map(|t| t.start_position().column)
                .unwrap_or_else(|| node.start_position().column) as u32,
            file_path: None,
            language: None,
            candidates: None,
        });

        // Walk the body's children so method_declaration nodes inside become
        // method nodes scoped to the anon class.
        self.node_stack.push(class_node.id);
        for child in named_children(body) {
            self.visit_node(child);
        }
        self.node_stack.pop();
    }

    /// Consider one node as a decorator/annotation attached to `decorated_id`
    /// (the TS `consider` closure inside extractDecoratorsFor).
    fn consider_decorator(&mut self, n: SyntaxNode<'_>, decorated_id: &str) {
        // `marker_annotation` is Java's grammar for arg-less annotations
        // (`@Override`, `@Deprecated`); without including it, every
        // such Java annotation would be silently skipped.
        if !matches!(n.kind(), "decorator" | "annotation" | "marker_annotation") {
            return;
        }
        // Find the leading identifier: skip the `@` punct, unwrap
        // a call_expression if the decorator is invoked with args.
        let mut target: Option<SyntaxNode<'_>> = None;
        for child in named_children(n) {
            if child.kind() == "call_expression" {
                let f = get_child_by_field(child, "function").or_else(|| child.named_child(0));
                if let Some(f) = f {
                    target = Some(f);
                }
                if target.is_some() {
                    break;
                }
            }
            if matches!(
                child.kind(),
                "identifier" | "member_expression" | "scoped_identifier" | "navigation_expression"
            ) {
                target = Some(child);
                break;
            }
        }
        let Some(target) = target else { return };
        let name = strip_qualifier(get_node_text(target, self.source));
        if name.is_empty() {
            return;
        }
        self.unresolved_references.push(UnresolvedReference {
            from_node_id: decorated_id.to_string(),
            reference_name: name,
            reference_kind: EdgeKind::Decorates,
            line: n.start_position().row as u32 + 1,
            column: n.start_position().column as u32,
            file_path: None,
            language: None,
            candidates: None,
        });
    }

    /// Scan `decl_node` and its preceding siblings (within the parent's
    /// named children) for decorator nodes, emitting a `decorates`
    /// reference from `decorated_id` to each decorator's function name.
    ///
    /// Why preceding siblings: in TypeScript, `@Foo class Bar {}` parses
    /// as an `export_statement` (or top-level wrapper) with the
    /// `decorator` as a child *before* the `class_declaration` — so the
    /// decorator isn't a child of the class itself. For methods/
    /// properties, the decorator IS a direct child of the declaration,
    /// so we also scan decl_node's named children.
    ///
    /// Idempotent across grammars: if neither location yields decorators
    /// (most non-decorator-using languages), the function is a no-op.
    fn extract_decorators_for(&mut self, decl_node: SyntaxNode<'_>, decorated_id: &str) {
        // 1. Decorators that are direct children of the declaration
        //    (method/property style, also some grammars for class).
        for child in named_children(decl_node) {
            self.consider_decorator(child, decorated_id);
        }

        // 2. Decorators that are PRECEDING siblings of the declaration
        //    inside the parent's children (TypeScript class style).
        //    Walk BACKWARDS from the declaration and stop at the first
        //    non-decorator sibling — without that stop, decorators
        //    belonging to an EARLIER unrelated declaration leak in
        //    (e.g. `@A class Foo {} @B class Bar {}` would otherwise
        //    attribute @A to Bar).
        //
        //    Note on identity: matching is by start byte (the TS web
        //    bindings return fresh wrapper objects from navigation, so
        //    the original matched on startIndex; kept for parity).
        if let Some(parent) = decl_node.parent() {
            let decl_start = decl_node.start_byte();
            let siblings = named_children(parent);
            let decl_idx = siblings.iter().position(|s| s.start_byte() == decl_start);
            if let Some(decl_idx) = decl_idx {
                if decl_idx > 0 {
                    for j in (0..decl_idx).rev() {
                        let sibling = siblings[j];
                        if !matches!(
                            sibling.kind(),
                            "decorator" | "annotation" | "marker_annotation"
                        ) {
                            break; // non-decorator separator → stop consuming
                        }
                        self.consider_decorator(sibling, decorated_id);
                    }
                }
            }
        }
    }

    /// Visit function body and extract calls (and structural nodes).
    ///
    /// In addition to call expressions, this also detects class/struct/enum
    /// definitions inside function bodies. This handles two cases:
    ///   1. Local class/struct/enum definitions (valid in C++, Java, etc.)
    ///   2. C++ macro misparsing — macros like NLOHMANN_JSON_NAMESPACE_BEGIN cause
    ///      tree-sitter to interpret the namespace block as a function_definition,
    ///      hiding real class/struct/enum nodes inside the "function body".
    fn visit_function_body(&mut self, body: SyntaxNode<'_>, _function_id: &str) {
        if self.extractor.is_none() {
            return;
        }
        self.visit_for_calls_and_structure(body);
    }

    /// The TS `visitForCallsAndStructure` inner closure.
    ///
    /// Recursion guard: this is a second walker independent of `visit_node` —
    /// it descends whole function bodies, whose statement nesting is exactly
    /// where real-world depth blowups live (the llvm-project overflow
    /// recursed here).
    fn visit_for_calls_and_structure(&mut self, node: SyntaxNode<'_>) {
        ensure_sufficient_stack(|| self.visit_for_calls_and_structure_inner(node));
    }

    fn visit_for_calls_and_structure_inner(&mut self, node: SyntaxNode<'_>) {
        let Some(ext) = self.extractor else { return };
        let node_type = node.kind();

        if ext.call_types().contains(&node_type) {
            self.extract_call(node);
        } else if INSTANTIATION_KINDS.contains(&node_type) {
            // `new Foo()` inside a function body — emit an `instantiates`
            // reference. Without this branch the body walker only knew
            // about `call_expression`, so constructor invocations
            // produced no graph edges at all.
            self.extract_instantiation(node);
            // Anonymous class with body: `new T() { ... }` (Java/C#). Extract as
            // a class so interface-impl synthesis (Phase 5.5) can bridge T's
            // methods to the overrides — same rationale as in visit_node.
            if let Some(anon_body) = self.find_anonymous_class_body(node) {
                self.extract_anonymous_class(node, anon_body);
                return;
            }
        } else if let Some(callee_name) = ext.extract_bare_call(node, self.source) {
            if !callee_name.is_empty() {
                if let Some(caller_id) = self.node_stack.last().cloned() {
                    self.unresolved_references.push(UnresolvedReference {
                        from_node_id: caller_id,
                        reference_name: callee_name,
                        reference_kind: EdgeKind::Calls,
                        line: node.start_position().row as u32 + 1,
                        column: node.start_position().column as u32,
                        file_path: None,
                        language: None,
                        candidates: None,
                    });
                }
            }
        }

        // Nested NAMED functions inside a body — function declarations and named
        // function expressions like `.on('mount', function onmount(){})` — become
        // their own nodes so the graph can link to them (callback handlers, local
        // helpers). Anonymous arrows/expressions fall through to the default
        // recursion below, keeping their inner calls attributed to the enclosing
        // function: this bounds the new nodes to NAMED functions only (no explosion,
        // no lost edges). extract_function walks the nested body itself, so we return.
        if ext.function_types().contains(&node_type) {
            let nested_name = extract_name(node, self.source, ext);
            if !nested_name.is_empty() && nested_name != "<anonymous>" {
                self.extract_function(node, None);
                return;
            }
        }

        // Extract structural nodes found inside function bodies.
        // Each extract method visits its own children, so we return after extracting.
        if ext.class_types().contains(&node_type) {
            match ext.classify_class_node(node, self.source) {
                ClassLikeKind::Struct => self.extract_struct(node),
                ClassLikeKind::Enum => self.extract_enum(node),
                ClassLikeKind::Interface => self.extract_interface(node),
                ClassLikeKind::Trait => self.extract_class(node, NodeKind::Trait),
                ClassLikeKind::Class => self.extract_class(node, NodeKind::Class),
            }
            return;
        }
        if ext.struct_types().contains(&node_type) {
            self.extract_struct(node);
            return;
        }
        if ext.enum_types().contains(&node_type) {
            self.extract_enum(node);
            return;
        }
        if ext.interface_types().contains(&node_type) {
            self.extract_interface(node);
            return;
        }

        for child in named_children(node) {
            self.visit_for_calls_and_structure(child);
        }
    }

    /// Push an unresolved reference (shared shorthand for inheritance refs).
    fn push_ref(
        &mut self,
        from_node_id: &str,
        reference_name: String,
        reference_kind: EdgeKind,
        pos_node: SyntaxNode<'_>,
    ) {
        self.unresolved_references.push(UnresolvedReference {
            from_node_id: from_node_id.to_string(),
            reference_name,
            reference_kind,
            line: pos_node.start_position().row as u32 + 1,
            column: pos_node.start_position().column as u32,
            file_path: None,
            language: None,
            candidates: None,
        });
    }

    /// Extract inheritance relationships
    fn extract_inheritance(&mut self, node: SyntaxNode<'_>, class_id: &str) {
        // Objective-C @interface MyClass : NSObject <ProtoA, ProtoB>
        if node.kind() == "class_interface" {
            if let Some(superclass) = get_child_by_field(node, "superclass") {
                let name = get_node_text(superclass, self.source).to_string();
                self.push_ref(class_id, name, EdgeKind::Extends, superclass);
            }
            for arg_list in named_children(node) {
                if arg_list.kind() != "parameterized_arguments" {
                    continue;
                }
                for type_name in named_children(arg_list) {
                    let type_id = named_children(type_name)
                        .into_iter()
                        .find(|c| c.kind() == "type_identifier" || c.kind() == "identifier");
                    let Some(type_id) = type_id else { continue };
                    let protocol_name = get_node_text(type_id, self.source).to_string();
                    self.push_ref(class_id, protocol_name, EdgeKind::Implements, type_id);
                }
            }
            return;
        }

        // Look for extends/implements clauses
        for child in named_children(node) {
            if matches!(
                child.kind(),
                "extends_clause" | "superclass" | "base_clause" /* PHP class extends */
                | "extends_interfaces" // Java interface extends
            ) {
                // Extract parent class/interface names
                // Java uses type_list wrapper: superclass -> type_identifier,
                // extends_interfaces -> type_list -> type_identifier
                let type_list = find_named_child(child, "type_list");
                let targets: Vec<Option<SyntaxNode<'_>>> = match type_list {
                    Some(tl) => named_children(tl).into_iter().map(Some).collect(),
                    None => vec![child.named_child(0)],
                };
                for target in targets.into_iter().flatten() {
                    let name = get_node_text(target, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Extends, target);
                }
            }

            // C++ base classes: `class Derived : public Base, private Other` →
            // base_class_clause holds access specifiers + base type(s). Emit an extends
            // ref per base type (skip the public/private/protected keywords).
            if child.kind() == "base_class_clause" {
                for t in named_children(child) {
                    if matches!(
                        t.kind(),
                        "type_identifier" | "qualified_identifier" | "template_type"
                    ) {
                        let name = get_node_text(t, self.source).to_string();
                        self.push_ref(class_id, name, EdgeKind::Extends, t);
                    }
                }
            }

            if matches!(
                child.kind(),
                "implements_clause" | "class_interface_clause"
                | "super_interfaces" // Java class implements
                | "interfaces" // Dart
            ) {
                // Extract implemented interfaces
                // Java uses type_list wrapper: super_interfaces -> type_list -> type_identifier
                let type_list = find_named_child(child, "type_list");
                let targets: Vec<SyntaxNode<'_>> = match type_list {
                    Some(tl) => named_children(tl),
                    None => named_children(child),
                };
                for iface in targets {
                    let name = get_node_text(iface, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Implements, iface);
                }
            }

            // Python superclass list: `class Flask(Scaffold, Mixin):`
            // argument_list contains identifier children for each parent class
            if child.kind() == "argument_list" && node.kind() == "class_definition" {
                for arg in named_children(child) {
                    if arg.kind() == "identifier" || arg.kind() == "attribute" {
                        let name = get_node_text(arg, self.source).to_string();
                        self.push_ref(class_id, name, EdgeKind::Extends, arg);
                    }
                }
            }

            // Go interface embedding: `type Querier interface { LabelQuerier; ... }`
            // constraint_elem wraps the embedded interface type identifier
            if child.kind() == "constraint_elem" {
                if let Some(type_id) = find_named_child(child, "type_identifier") {
                    let name = get_node_text(type_id, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Extends, type_id);
                }
            }

            // Go struct embedding: field_declaration without field_identifier
            // e.g. `type DB struct { *Head; Queryable }` — no field name means embedded type
            if child.kind() == "field_declaration" {
                let has_field_identifier = named_children(child)
                    .into_iter()
                    .any(|c| c.kind() == "field_identifier");
                if !has_field_identifier {
                    if let Some(type_id) = find_named_child(child, "type_identifier") {
                        let name = get_node_text(type_id, self.source).to_string();
                        self.push_ref(class_id, name, EdgeKind::Extends, type_id);
                    }
                }
            }

            // Rust trait supertraits: `trait SubTrait: SuperTrait + Display { ... }`
            // trait_bounds contains type_identifier, generic_type, or higher_ranked_trait_bound children
            if child.kind() == "trait_bounds" {
                for bound in named_children(child) {
                    let mut type_name: Option<String> = None;
                    let mut pos_node: Option<SyntaxNode<'_>> = None;

                    if bound.kind() == "type_identifier" {
                        type_name = Some(get_node_text(bound, self.source).to_string());
                        pos_node = Some(bound);
                    } else if bound.kind() == "generic_type" {
                        // e.g. `Deserialize<'de>`
                        if let Some(inner) = find_named_child(bound, "type_identifier") {
                            type_name = Some(get_node_text(inner, self.source).to_string());
                            pos_node = Some(inner);
                        }
                    } else if bound.kind() == "higher_ranked_trait_bound" {
                        // e.g. `for<'de> Deserialize<'de>`
                        let generic = find_named_child(bound, "generic_type");
                        let type_id = generic
                            .and_then(|g| find_named_child(g, "type_identifier"))
                            .or_else(|| find_named_child(bound, "type_identifier"));
                        if let Some(type_id) = type_id {
                            type_name = Some(get_node_text(type_id, self.source).to_string());
                            pos_node = Some(type_id);
                        }
                    }

                    if let (Some(type_name), Some(pos_node)) = (type_name, pos_node) {
                        self.push_ref(class_id, type_name, EdgeKind::Extends, pos_node);
                    }
                }
            }

            // C#: `class Movie : BaseItem, IPlugin` → base_list with identifier children
            // base_list combines both base class and interfaces in a single colon-separated list.
            // We emit all as 'extends' since the syntax doesn't distinguish them.
            if child.kind() == "base_list" {
                for base_type in named_children(child) {
                    // For generic base types like `ClientBase<T>`, extract just the type name
                    let name = if base_type.kind() == "generic_name" {
                        let inner = named_children(base_type)
                            .into_iter()
                            .find(|c| c.kind() == "identifier")
                            .unwrap_or(base_type);
                        get_node_text(inner, self.source).to_string()
                    } else {
                        get_node_text(base_type, self.source).to_string()
                    };
                    self.push_ref(class_id, name, EdgeKind::Extends, base_type);
                }
            }

            // Kotlin: `class Foo : Bar, Baz` → delegation_specifier > user_type > type_identifier
            // Also handles `class Foo : Bar()` → delegation_specifier > constructor_invocation > user_type
            if child.kind() == "delegation_specifier" {
                let user_type = find_named_child(child, "user_type");
                let constructor_invocation = find_named_child(child, "constructor_invocation");
                let target = user_type.or(constructor_invocation);
                if let Some(target) = target {
                    let type_id = if target.kind() == "user_type" {
                        find_named_child(target, "type_identifier").unwrap_or(target)
                    } else {
                        let inner_user_type = find_named_child(target, "user_type");
                        inner_user_type
                            .and_then(|ut| find_named_child(ut, "type_identifier"))
                            .or(inner_user_type)
                            .unwrap_or(target)
                    };
                    let name = get_node_text(type_id, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Extends, type_id);
                }
            }

            // Swift: inheritance_specifier > user_type > type_identifier
            // Used for class inheritance, protocol conformance, and protocol inheritance
            if child.kind() == "inheritance_specifier" {
                let user_type = find_named_child(child, "user_type");
                let type_id = user_type.and_then(|ut| find_named_child(ut, "type_identifier"));
                if let Some(type_id) = type_id {
                    let name = get_node_text(type_id, self.source).to_string();
                    self.push_ref(class_id, name, EdgeKind::Extends, type_id);
                }
            }

            // JavaScript class_heritage has bare identifier without extends_clause wrapper
            // e.g. `class Foo extends Bar {}` → class_heritage → identifier("Bar")
            if (child.kind() == "identifier" || child.kind() == "type_identifier")
                && node.kind() == "class_heritage"
            {
                let name = get_node_text(child, self.source).to_string();
                self.push_ref(class_id, name, EdgeKind::Extends, child);
            }

            // Recurse into container nodes (e.g. field_declaration_list in Go structs,
            // class_heritage in TypeScript which wraps extends_clause/implements_clause)
            if child.kind() == "field_declaration_list" || child.kind() == "class_heritage" {
                self.extract_inheritance(child, class_id);
            }
        }
    }

    /// Rust `impl Trait for Type` — creates an implements edge from Type to Trait.
    /// For plain `impl Type { ... }` (no trait), no inheritance edge is needed.
    fn extract_rust_impl_item(&mut self, node: SyntaxNode<'_>) {
        // Check if this is `impl Trait for Type` by looking for a `for` keyword
        let mut has_for = false;
        for i in 0..node.child_count() as u32 {
            if let Some(c) = node.child(i) {
                if c.kind() == "for" && !c.is_named() {
                    has_for = true;
                    break;
                }
            }
        }
        if !has_for {
            return;
        }

        // In `impl Trait for Type`, the type_identifiers are:
        // first = Trait name, last = implementing Type name
        // Also handle generic types like `impl<T> Trait for MyStruct<T>`
        let type_idents: Vec<SyntaxNode<'_>> = named_children(node)
            .into_iter()
            .filter(|c| {
                matches!(
                    c.kind(),
                    "type_identifier" | "generic_type" | "scoped_type_identifier"
                )
            })
            .collect();
        if type_idents.len() < 2 {
            return;
        }

        let trait_node = type_idents[0];
        let type_node = type_idents[type_idents.len() - 1];

        // Get the trait name (handle scoped paths like std::fmt::Display)
        let trait_name = get_node_text(trait_node, self.source).to_string();

        // Get the implementing type name (extract inner type_identifier for generics)
        let type_name = if type_node.kind() == "generic_type" {
            match find_named_child(type_node, "type_identifier") {
                Some(inner) => get_node_text(inner, self.source).to_string(),
                None => get_node_text(type_node, self.source).to_string(),
            }
        } else {
            get_node_text(type_node, self.source).to_string()
        };

        // Find the struct/type node for the implementing type
        if let Some(type_node_id) = self.find_node_by_name(&type_name) {
            self.push_ref(&type_node_id, trait_name, EdgeKind::Implements, trait_node);
        }
    }

    /// Find a previously-extracted node by name (used for back-references like impl blocks)
    fn find_node_by_name(&self, name: &str) -> Option<String> {
        self.nodes
            .iter()
            .find(|n| {
                n.name == name
                    && matches!(n.kind, NodeKind::Struct | NodeKind::Enum | NodeKind::Class)
            })
            .map(|n| n.id.clone())
    }

    /// Extract type references from type annotations on a function/method/field node.
    /// Creates 'references' edges for parameter types, return types, and field types.
    fn extract_type_annotations(&mut self, node: SyntaxNode<'_>, node_id: &str) {
        let Some(ext) = self.extractor else { return };
        if !is_type_annotation_language(self.language) {
            return;
        }

        // C# tree-sitter doesn't produce `type_identifier` leaves — it uses
        // `identifier`, `predefined_type`, `qualified_name`, `generic_name`,
        // etc. — so the generic walker below emits zero references for it.
        // Dispatch to a C#-aware path that only walks type-position subtrees
        // (the `type` field of a parameter/method/property/field), so
        // parameter NAMES never accidentally surface as type refs (#381).
        if self.language == Language::Csharp {
            self.extract_csharp_type_refs(node, node_id);
            return;
        }

        // Extract parameter type annotations
        let params_field = ext.params_field();
        let params_field = if params_field.is_empty() {
            "parameters"
        } else {
            params_field
        };
        if let Some(params) = get_child_by_field(node, params_field) {
            self.extract_type_refs_from_subtree(params, node_id);
        }

        // Extract return type annotation
        let return_field = match ext.return_field() {
            Some(f) if !f.is_empty() => f,
            _ => "return_type",
        };
        if let Some(return_type) = get_child_by_field(node, return_field) {
            self.extract_type_refs_from_subtree(return_type, node_id);
        }

        // Extract direct type annotation (for class fields like `model: ITextModel`)
        if let Some(type_annotation) = find_named_child(node, "type_annotation") {
            self.extract_type_refs_from_subtree(type_annotation, node_id);
        }
    }

    /// Extract C# type references from a node that owns a type position —
    /// a method/constructor declaration, a property declaration, or a
    /// field declaration (which wraps `variable_declaration → type`).
    ///
    /// Walks ONLY into known type fields, so parameter names like
    /// `request` in `Build(UserDto request)` are never mis-emitted as
    /// type references. Closes #381.
    fn extract_csharp_type_refs(&mut self, node: SyntaxNode<'_>, node_id: &str) {
        // Return type / property type — the field is named `type`.
        if let Some(direct_type) = get_child_by_field(node, "type") {
            self.walk_csharp_type_position(direct_type, node_id);
        }

        // Field declarations wrap declarators in a `variable_declaration`
        // whose `type` field carries the type. The outer `field_declaration`
        // has no `type` field of its own, so the call above is a no-op here
        // and we descend one level.
        if let Some(var_decl) = find_named_child(node, "variable_declaration") {
            if let Some(vd_type) = get_child_by_field(var_decl, "type") {
                self.walk_csharp_type_position(vd_type, node_id);
            }
        }

        // Method / constructor parameters. The field name on
        // `method_declaration` is `parameters`; it points at a
        // `parameter_list` whose `parameter` children each have their own
        // `type` field. Walking ONLY the type field skips parameter NAMES,
        // which would otherwise mis-emit as type references.
        if let Some(params) = get_child_by_field(node, "parameters") {
            for child in named_children(params) {
                if child.kind() != "parameter" {
                    continue;
                }
                if let Some(param_type) = get_child_by_field(child, "type") {
                    self.walk_csharp_type_position(param_type, node_id);
                }
            }
        }
    }

    /// Walk a C# subtree that is KNOWN to be in a type position
    /// (return type, parameter type, property type, field type, generic
    /// argument). Identifiers here are type names, not parameter names.
    fn walk_csharp_type_position(&mut self, node: SyntaxNode<'_>, from_node_id: &str) {
        // Recursion guard — generated generic types nest arbitrarily deep.
        ensure_sufficient_stack(|| self.walk_csharp_type_position_inner(node, from_node_id));
    }

    fn walk_csharp_type_position_inner(&mut self, node: SyntaxNode<'_>, from_node_id: &str) {
        // `predefined_type` is int/string/bool/etc. — never a project ref.
        if node.kind() == "predefined_type" {
            return;
        }

        // Bare type name: `Foo` in `Foo bar`, or the `Foo` inside `List<Foo>`.
        if node.kind() == "identifier" {
            let name = get_node_text(node, self.source).to_string();
            if !name.is_empty() && !BUILTIN_TYPES.contains(&name.as_str()) {
                self.push_ref(from_node_id, name, EdgeKind::References, node);
            }
            return;
        }

        // `Namespace.Foo` → the rightmost identifier is the type. Emit the
        // trailing simple name as the reference.
        if node.kind() == "qualified_name" {
            let text = get_node_text(node, self.source);
            let last = text.split('.').next_back().unwrap_or(text).to_string();
            if !last.is_empty() && !BUILTIN_TYPES.contains(&last.as_str()) {
                self.push_ref(from_node_id, last, EdgeKind::References, node);
            }
            return;
        }

        // `(int Code, Foo Payload)` — tuple element has BOTH a `type` and a
        // `name` field; descending into all named children would mis-emit
        // the element name (`Code`, `Payload`) as a type ref. Walk only the
        // type field.
        if node.kind() == "tuple_element" {
            if let Some(t) = get_child_by_field(node, "type") {
                self.walk_csharp_type_position(t, from_node_id);
            }
            return;
        }

        // Composite type nodes — recurse into named children. Covers
        // `generic_name` (head identifier + `type_argument_list`),
        // `nullable_type`, `array_type`, `pointer_type`, `tuple_type`,
        // `ref_type`, and any newer wrapping shapes the grammar adds.
        // Identifiers reached here are all type-positional (parameter/field
        // names are gated out before we descend).
        for child in named_children(node) {
            self.walk_csharp_type_position(child, from_node_id);
        }
    }

    /// Extract type references from a variable's type annotation.
    fn extract_variable_type_annotation(&mut self, node: SyntaxNode<'_>, node_id: &str) {
        if !is_type_annotation_language(self.language) {
            return;
        }

        // Find type_annotation child (covers TS `: Type`, Rust `: Type`, etc.)
        if let Some(type_annotation) = find_named_child(node, "type_annotation") {
            self.extract_type_refs_from_subtree(type_annotation, node_id);
        }
    }

    /// Recursively walk a subtree and extract all type_identifier references.
    /// Handles unions, intersections, generics, arrays, etc.
    fn extract_type_refs_from_subtree(&mut self, node: SyntaxNode<'_>, from_node_id: &str) {
        // Recursion guard — generated type expressions nest arbitrarily deep.
        ensure_sufficient_stack(|| {
            if node.kind() == "type_identifier" {
                let type_name = get_node_text(node, self.source).to_string();
                if !type_name.is_empty() && !BUILTIN_TYPES.contains(&type_name.as_str()) {
                    self.push_ref(from_node_id, type_name, EdgeKind::References, node);
                }
                return; // type_identifier is a leaf
            }

            // Recurse into children (handles union_type, intersection_type, generic_type, etc.)
            for child in named_children(node) {
                self.extract_type_refs_from_subtree(child, from_node_id);
            }
        });
    }

    /// Handle Pascal-specific AST structures.
    /// Returns true if the node was fully handled and children should be skipped.
    fn visit_pascal_node(&mut self, node: SyntaxNode<'_>) -> bool {
        let node_type = node.kind();

        // Unit/Program/Library → module node
        if matches!(node_type, "unit" | "program" | "library") {
            let module_name_node = find_named_child(node, "moduleName");
            let name = module_name_node
                .map(|n| get_node_text(n, self.source).to_string())
                .unwrap_or_default();
            // Fallback to filename without extension if module name is empty
            let module_name = if name.is_empty() {
                let base = basename(&self.file_path);
                match base.rfind('.') {
                    Some(dot) if dot > 0 => base[..dot].to_string(),
                    _ => base,
                }
            } else {
                name
            };
            self.create_node(NodeKind::Module, &module_name, node, NodeExtra::default());
            // Continue visiting children (interface/implementation sections)
            for child in named_children(node) {
                self.visit_node(child);
            }
            return true;
        }

        // declType wraps declClass/declIntf/declEnum/type-alias
        // The name lives on declType, the inner node determines the kind
        if node_type == "declType" {
            self.extract_pascal_decl_type(node);
            return true;
        }

        // declUses → import nodes for each unit name
        if node_type == "declUses" {
            self.extract_pascal_uses(node);
            return true;
        }

        // declConsts → container; visit children for individual declConst
        if node_type == "declConsts" {
            for child in named_children(node) {
                if child.kind() == "declConst" {
                    self.extract_pascal_const(child);
                }
            }
            return true;
        }

        // declConst at top level (outside declConsts)
        if node_type == "declConst" {
            self.extract_pascal_const(node);
            return true;
        }

        // declTypes → container for type declarations
        if node_type == "declTypes" {
            for child in named_children(node) {
                self.visit_node(child);
            }
            return true;
        }

        // declVars → container for variable declarations
        if node_type == "declVars" {
            for child in named_children(node) {
                if child.kind() == "declVar" {
                    if let Some(name_node) = get_child_by_field(child, "name") {
                        let name = get_node_text(name_node, self.source).to_string();
                        self.create_node(NodeKind::Variable, &name, child, NodeExtra::default());
                    }
                }
            }
            return true;
        }

        // defProc in implementation section → extract calls but don't create duplicate nodes
        if node_type == "defProc" {
            self.extract_pascal_def_proc(node);
            return true;
        }

        // declProp → property node
        if node_type == "declProp" {
            if let Some(name_node) = get_child_by_field(node, "name") {
                let name = get_node_text(name_node, self.source).to_string();
                let visibility = self
                    .extractor
                    .and_then(|e| e.get_visibility(node, self.source));
                self.create_node(
                    NodeKind::Property,
                    &name,
                    node,
                    NodeExtra {
                        visibility,
                        ..Default::default()
                    },
                );
            }
            return true;
        }

        // declField → field node
        if node_type == "declField" {
            if let Some(name_node) = get_child_by_field(node, "name") {
                let name = get_node_text(name_node, self.source).to_string();
                let visibility = self
                    .extractor
                    .and_then(|e| e.get_visibility(node, self.source));
                self.create_node(
                    NodeKind::Field,
                    &name,
                    node,
                    NodeExtra {
                        visibility,
                        ..Default::default()
                    },
                );
            }
            return true;
        }

        // declSection → visit children (propagates visibility via get_visibility)
        if node_type == "declSection" {
            for child in named_children(node) {
                self.visit_node(child);
            }
            return true;
        }

        // exprCall → extract function call reference
        if node_type == "exprCall" {
            self.extract_pascal_call(node);
            return true;
        }

        // interface/implementation sections → visit children
        if node_type == "interface" || node_type == "implementation" {
            for child in named_children(node) {
                self.visit_node(child);
            }
            return true;
        }

        // block (begin..end) → visit for calls
        if node_type == "block" {
            self.visit_pascal_block(node);
            return true;
        }

        false
    }

    /// Extract a Pascal declType node (class, interface, enum, or type alias)
    fn extract_pascal_decl_type(&mut self, node: SyntaxNode<'_>) {
        let Some(name_node) = get_child_by_field(node, "name") else {
            return;
        };
        let name = get_node_text(name_node, self.source).to_string();

        // Find the inner type declaration
        let decl_class = find_named_child(node, "declClass");
        let decl_intf = find_named_child(node, "declIntf");
        let type_child = find_named_child(node, "type");

        if let Some(decl_class) = decl_class {
            if let Some(class_node) =
                self.create_node(NodeKind::Class, &name, node, NodeExtra::default())
            {
                // Extract inheritance from typeref children of declClass
                self.extract_pascal_inheritance(decl_class, &class_node.id);
                // Visit class body
                self.node_stack.push(class_node.id);
                for child in named_children(decl_class) {
                    self.visit_node(child);
                }
                self.node_stack.pop();
            }
        } else if let Some(decl_intf) = decl_intf {
            if let Some(iface_node) =
                self.create_node(NodeKind::Interface, &name, node, NodeExtra::default())
            {
                // Visit interface members
                self.node_stack.push(iface_node.id);
                for child in named_children(decl_intf) {
                    self.visit_node(child);
                }
                self.node_stack.pop();
            }
        } else if let Some(type_child) = type_child {
            // Check if it contains a declEnum
            if let Some(decl_enum) = find_named_child(type_child, "declEnum") {
                if let Some(enum_node) =
                    self.create_node(NodeKind::Enum, &name, node, NodeExtra::default())
                {
                    // Extract enum members
                    self.node_stack.push(enum_node.id);
                    for child in named_children(decl_enum) {
                        if child.kind() == "declEnumValue" {
                            if let Some(member_name) = get_child_by_field(child, "name") {
                                let member = get_node_text(member_name, self.source).to_string();
                                self.create_node(
                                    NodeKind::EnumMember,
                                    &member,
                                    child,
                                    NodeExtra::default(),
                                );
                            }
                        }
                    }
                    self.node_stack.pop();
                }
            } else {
                // Simple type alias: type TFoo = string / type TFoo = Integer
                self.create_node(NodeKind::TypeAlias, &name, node, NodeExtra::default());
            }
        } else {
            // Fallback: could be a forward declaration or simple alias
            self.create_node(NodeKind::TypeAlias, &name, node, NodeExtra::default());
        }
    }

    /// Extract Pascal uses clause into individual import nodes
    fn extract_pascal_uses(&mut self, node: SyntaxNode<'_>) {
        let import_text = get_node_text(node, self.source).trim().to_string();
        for child in named_children(node) {
            if child.kind() == "moduleName" {
                let unit_name = get_node_text(child, self.source).to_string();
                self.create_node(
                    NodeKind::Import,
                    &unit_name,
                    child,
                    NodeExtra {
                        signature: Some(import_text.clone()),
                        ..Default::default()
                    },
                );
                // Create unresolved reference for resolution
                if let Some(parent_id) = self.node_stack.last().cloned() {
                    self.unresolved_references.push(UnresolvedReference {
                        from_node_id: parent_id,
                        reference_name: unit_name,
                        reference_kind: EdgeKind::Imports,
                        line: child.start_position().row as u32 + 1,
                        column: child.start_position().column as u32,
                        file_path: None,
                        language: None,
                        candidates: None,
                    });
                }
            }
        }
    }

    /// Extract a Pascal constant declaration
    fn extract_pascal_const(&mut self, node: SyntaxNode<'_>) {
        let Some(name_node) = get_child_by_field(node, "name") else {
            return;
        };
        let name = get_node_text(name_node, self.source).to_string();
        let default_value = find_named_child(node, "defaultValue");
        let sig = default_value.map(|d| get_node_text(d, self.source).to_string());
        self.create_node(
            NodeKind::Constant,
            &name,
            node,
            NodeExtra {
                signature: sig,
                ..Default::default()
            },
        );
    }

    /// Extract Pascal inheritance (extends/implements) from declClass typeref children
    fn extract_pascal_inheritance(&mut self, decl_class: SyntaxNode<'_>, class_id: &str) {
        let typerefs: Vec<SyntaxNode<'_>> = named_children(decl_class)
            .into_iter()
            .filter(|c| c.kind() == "typeref")
            .collect();
        for (i, type_ref) in typerefs.into_iter().enumerate() {
            let name = get_node_text(type_ref, self.source).to_string();
            let kind = if i == 0 {
                EdgeKind::Extends
            } else {
                EdgeKind::Implements
            };
            self.push_ref(class_id, name, kind, type_ref);
        }
    }

    /// Extract calls and resolve method context from a Pascal defProc (implementation body).
    /// Does not create a new node — the declaration was already captured from the interface section.
    fn extract_pascal_def_proc(&mut self, node: SyntaxNode<'_>) {
        // Find the matching declaration node by name to use as call parent
        let Some(decl_proc) = find_named_child(node, "declProc") else {
            return;
        };

        let Some(name_node) = get_child_by_field(decl_proc, "name") else {
            return;
        };
        let full_name = get_node_text(name_node, self.source).trim().to_string();
        // full_name is like "TAuthService.Create"
        let short_name = full_name
            .rsplit('.')
            .next()
            .unwrap_or(&full_name)
            .to_string();
        let full_name_key = full_name.to_lowercase();
        let short_name_key = short_name.to_lowercase();

        // Build method index on first use (O(n) once, then O(1) per lookup)
        if self.method_index.is_none() {
            let mut index: HashMap<String, String> = HashMap::new();
            for n in &self.nodes {
                if n.kind == NodeKind::Method || n.kind == NodeKind::Function {
                    let name_key = n.name.to_lowercase();
                    // Keep first seen short-name mapping to avoid silently overwriting earlier entries.
                    index.entry(name_key).or_insert_with(|| n.id.clone());

                    // For Pascal methods, also index qualified forms (e.g. TAuthService.Create).
                    if n.kind == NodeKind::Method {
                        let qualified_parts: Vec<&str> = n.qualified_name.split("::").collect();
                        if qualified_parts.len() >= 2 {
                            // Create suffix keys so both "Module.Class.Method" and "Class.Method" can resolve.
                            for i in 0..qualified_parts.len() - 1 {
                                let scoped_name = qualified_parts[i..].join(".").to_lowercase();
                                index.insert(scoped_name, n.id.clone());
                            }
                        }
                    }
                }
            }
            self.method_index = Some(index);
        }

        let index = self.method_index.as_ref().expect("method index built");
        let parent_id = index
            .get(&full_name_key)
            .or_else(|| index.get(&short_name_key))
            .cloned()
            .or_else(|| self.node_stack.last().cloned());
        let Some(parent_id) = parent_id else { return };

        // Visit the block for calls
        if let Some(block) = find_named_child(node, "block") {
            self.node_stack.push(parent_id);
            self.visit_pascal_block(block);
            self.node_stack.pop();
        }
    }

    /// Extract function calls from a Pascal expression
    fn extract_pascal_call(&mut self, node: SyntaxNode<'_>) {
        let Some(caller_id) = self.node_stack.last().cloned() else {
            return;
        };

        // Get the callee name — first child is typically the identifier or exprDot
        let Some(first_child) = node.named_child(0) else {
            return;
        };

        let mut callee_name = String::new();
        if first_child.kind() == "exprDot" {
            // Qualified call: Obj.Method(...)
            let identifiers: Vec<String> = named_children(first_child)
                .into_iter()
                .filter(|c| c.kind() == "identifier")
                .map(|c| get_node_text(c, self.source).to_string())
                .collect();
            if !identifiers.is_empty() {
                callee_name = identifiers.join(".");
            }
        } else if first_child.kind() == "identifier" {
            callee_name = get_node_text(first_child, self.source).to_string();
        }

        if !callee_name.is_empty() {
            self.unresolved_references.push(UnresolvedReference {
                from_node_id: caller_id,
                reference_name: callee_name,
                reference_kind: EdgeKind::Calls,
                line: node.start_position().row as u32 + 1,
                column: node.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
            });
        }

        // Also visit arguments for nested calls
        if let Some(args) = find_named_child(node, "exprArgs") {
            self.visit_pascal_block(args);
        }
    }

    /// Recursively visit a Pascal block/statement tree for call expressions
    fn visit_pascal_block(&mut self, node: SyntaxNode<'_>) {
        // Recursion guard — statement trees nest arbitrarily deep.
        ensure_sufficient_stack(|| {
            for child in named_children(node) {
                if child.kind() == "exprCall" {
                    self.extract_pascal_call(child);
                } else if child.kind() == "exprDot" {
                    // Check if exprDot contains an exprCall
                    for grandchild in named_children(child) {
                        if grandchild.kind() == "exprCall" {
                            self.extract_pascal_call(grandchild);
                        }
                    }
                } else {
                    self.visit_pascal_block(child);
                }
            }
        });
    }
}

impl ExtractorContext for TreeSitterExtractor<'_> {
    fn create_node(
        &mut self,
        kind: NodeKind,
        name: &str,
        node: SyntaxNode<'_>,
        extra: NodeExtra,
    ) -> Option<Node> {
        TreeSitterExtractor::create_node(self, kind, name, node, extra)
    }

    fn visit_node(&mut self, node: SyntaxNode<'_>) {
        TreeSitterExtractor::visit_node(self, node);
    }

    fn visit_function_body(&mut self, body: SyntaxNode<'_>, function_id: &str) {
        TreeSitterExtractor::visit_function_body(self, body, function_id);
    }

    fn add_unresolved_reference(&mut self, reference: UnresolvedReference) {
        self.unresolved_references.push(reference);
    }

    fn push_scope(&mut self, node_id: String) {
        self.node_stack.push(node_id);
    }

    fn pop_scope(&mut self) {
        self.node_stack.pop();
    }

    fn file_path(&self) -> &str {
        &self.file_path
    }

    fn source(&self) -> &str {
        self.source
    }

    fn node_stack(&self) -> &[String] {
        &self.node_stack
    }

    fn nodes(&self) -> &[Node] {
        &self.nodes
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_types::ImportInfo;

    /// Minimal TypeScript LanguageExtractor (a subset of
    /// `src/extraction/languages/typescript.ts`) used to exercise the core
    /// engine without depending on the `languages/` module (ported by a
    /// separate task). Also serves as the worked implementation example for
    /// the languages port — see notes/extraction-core.md.
    struct TsTestExtractor;

    impl LanguageExtractor for TsTestExtractor {
        fn function_types(&self) -> &[&str] {
            &[
                "function_declaration",
                "arrow_function",
                "function_expression",
            ]
        }
        fn class_types(&self) -> &[&str] {
            &["class_declaration", "abstract_class_declaration"]
        }
        fn method_types(&self) -> &[&str] {
            &["method_definition", "public_field_definition"]
        }
        fn interface_types(&self) -> &[&str] {
            &["interface_declaration"]
        }
        fn struct_types(&self) -> &[&str] {
            &[]
        }
        fn enum_types(&self) -> &[&str] {
            &["enum_declaration"]
        }
        fn enum_member_types(&self) -> &[&str] {
            &["property_identifier", "enum_assignment"]
        }
        fn type_alias_types(&self) -> &[&str] {
            &["type_alias_declaration"]
        }
        fn import_types(&self) -> &[&str] {
            &["import_statement"]
        }
        fn call_types(&self) -> &[&str] {
            &["call_expression"]
        }
        fn variable_types(&self) -> &[&str] {
            &["lexical_declaration", "variable_declaration"]
        }
        fn name_field(&self) -> &str {
            "name"
        }
        fn body_field(&self) -> &str {
            "body"
        }
        fn params_field(&self) -> &str {
            "parameters"
        }
        fn return_field(&self) -> Option<&str> {
            Some("return_type")
        }
        fn is_exported(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
            let mut current = node.parent();
            while let Some(p) = current {
                if p.kind() == "export_statement" {
                    return Some(true);
                }
                current = p.parent();
            }
            Some(false)
        }
        fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
            if node.kind() == "lexical_declaration" {
                for i in 0..node.child_count() as u32 {
                    if let Some(c) = node.child(i) {
                        if c.kind() == "const" {
                            return Some(true);
                        }
                    }
                }
            }
            Some(false)
        }
        fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
            if let Some(source_field) = node.child_by_field_name("source") {
                let module_name = get_node_text(source_field, source).replace(['\'', '"'], "");
                if !module_name.is_empty() {
                    return ImportOutcome::Info(ImportInfo::new(
                        module_name,
                        get_node_text(node, source).trim(),
                    ));
                }
            }
            ImportOutcome::Declined
        }
    }

    fn extract_ts(file_path: &str, source: &str) -> ExtractionResult {
        let ext = TsTestExtractor;
        TreeSitterExtractor::new(file_path, source, Some(Language::Typescript), Some(&ext))
            .extract()
    }

    #[test]
    fn extracts_file_node_and_functions_with_ts_compatible_ids() {
        let source = "export function add(a: number, b: number): number {\n  return helper(a) + b;\n}\nfunction helper(x: number) {\n  return x;\n}\n";
        let result = extract_ts("src/math.ts", source);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let file = &result.nodes[0];
        assert_eq!(file.id, "file:src/math.ts");
        assert_eq!(file.kind, NodeKind::File);
        assert_eq!(file.name, "math.ts");
        assert_eq!(file.qualified_name, "src/math.ts");
        assert_eq!(file.start_line, 1);

        let add = result
            .nodes
            .iter()
            .find(|n| n.name == "add")
            .expect("add node");
        assert_eq!(add.kind, NodeKind::Function);
        assert_eq!(add.start_line, 1);
        assert_eq!(add.end_line, 3);
        assert_eq!(add.is_exported, Some(true));
        assert_eq!(add.qualified_name, "add");
        // ID derivation identical to the TS generateNodeId
        assert_eq!(
            add.id,
            crate::extraction::tree_sitter_helpers::generate_node_id(
                "src/math.ts",
                NodeKind::Function,
                "add",
                1
            )
        );
        assert!(add.id.starts_with("function:"));
        assert_eq!(add.id.len(), "function:".len() + 32);

        let helper = result
            .nodes
            .iter()
            .find(|n| n.name == "helper")
            .expect("helper node");
        assert_eq!(helper.is_exported, Some(false));
        assert_eq!(helper.start_line, 4);

        // Call reference from add → helper
        let call = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "helper")
            .expect("helper call ref");
        assert_eq!(call.reference_kind, EdgeKind::Calls);
        assert_eq!(call.from_node_id, add.id);
        assert_eq!(call.line, 2);

        // contains: file → add, file → helper
        let file_contains: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.source == file.id && e.kind == EdgeKind::Contains)
            .collect();
        assert_eq!(file_contains.len(), 2);
    }

    #[test]
    fn extracts_class_with_method_and_qualified_name() {
        let source =
            "class Greeter {\n  greet(who: string): string {\n    return hello(who);\n  }\n}\n";
        let result = extract_ts("src/greeter.ts", source);

        let class = result
            .nodes
            .iter()
            .find(|n| n.name == "Greeter")
            .expect("class node");
        assert_eq!(class.kind, NodeKind::Class);

        let method = result
            .nodes
            .iter()
            .find(|n| n.name == "greet")
            .expect("method node");
        assert_eq!(method.kind, NodeKind::Method);
        assert_eq!(method.qualified_name, "Greeter::greet");

        // contains: class → method
        assert!(result.edges.iter().any(|e| e.source == class.id
            && e.target == method.id
            && e.kind == EdgeKind::Contains));

        // call attributed to the method
        let call = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "hello")
            .expect("hello call");
        assert_eq!(call.from_node_id, method.id);
    }

    #[test]
    fn extracts_class_inheritance_references() {
        let source =
            "interface Base {}\nclass Impl extends Parent implements Base {\n  run() {}\n}\n";
        let result = extract_ts("src/impl.ts", source);

        let impl_node = result
            .nodes
            .iter()
            .find(|n| n.name == "Impl")
            .expect("Impl node");
        let extends_ref = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_kind == EdgeKind::Extends)
            .expect("extends ref");
        assert_eq!(extends_ref.reference_name, "Parent");
        assert_eq!(extends_ref.from_node_id, impl_node.id);
        let implements_ref = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_kind == EdgeKind::Implements)
            .expect("implements ref");
        assert_eq!(implements_ref.reference_name, "Base");
    }

    #[test]
    fn extracts_import_node_and_reference() {
        let source = "import { x } from './mod';\n";
        let result = extract_ts("src/a.ts", source);

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "./mod");
        assert_eq!(
            import.signature.as_deref(),
            Some("import { x } from './mod';")
        );

        let import_ref = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_kind == EdgeKind::Imports)
            .expect("imports ref");
        assert_eq!(import_ref.reference_name, "./mod");
        assert_eq!(import_ref.from_node_id, "file:src/a.ts");
    }

    #[test]
    fn extracts_const_variable_with_initializer_signature() {
        let source = "export const NAME = 'value';\nlet counter = 0;\n";
        let result = extract_ts("src/vars.ts", source);

        let constant = result
            .nodes
            .iter()
            .find(|n| n.name == "NAME")
            .expect("NAME node");
        assert_eq!(constant.kind, NodeKind::Constant);
        assert_eq!(constant.signature.as_deref(), Some("= 'value'"));
        assert_eq!(constant.is_exported, Some(true));

        let variable = result
            .nodes
            .iter()
            .find(|n| n.name == "counter")
            .expect("counter node");
        assert_eq!(variable.kind, NodeKind::Variable);
        assert_eq!(variable.is_exported, Some(false));
    }

    #[test]
    fn arrow_function_const_extracted_as_named_function() {
        let source = "export const useAuth = () => {\n  return login();\n};\n";
        let result = extract_ts("src/auth.ts", source);

        let func = result
            .nodes
            .iter()
            .find(|n| n.name == "useAuth")
            .expect("useAuth node");
        assert_eq!(func.kind, NodeKind::Function);
        assert_eq!(func.is_exported, Some(true));

        let call = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "login")
            .expect("login call");
        assert_eq!(call.from_node_id, func.id);
    }

    #[test]
    fn exported_store_object_functions_become_named_nodes() {
        // `export const useStore = create((set) => ({ fetchUser: async () => { load(); } }))`
        // — the store action must surface as a named function node (#issue in TS docs).
        let source = "export const useStore = create((set) => ({\n  fetchUser: async () => {\n    load();\n  },\n}));\n";
        let result = extract_ts("src/store.ts", source);

        let action = result
            .nodes
            .iter()
            .find(|n| n.name == "fetchUser")
            .expect("fetchUser node");
        assert_eq!(action.kind, NodeKind::Function);

        let call = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "load")
            .expect("load call");
        assert_eq!(call.from_node_id, action.id);
    }

    #[test]
    fn instantiation_inside_function_body_emits_instantiates_ref() {
        let source = "function build() {\n  const m = new ns.Mapper<string>();\n}\n";
        let result = extract_ts("src/build.ts", source);

        let build = result
            .nodes
            .iter()
            .find(|n| n.name == "build")
            .expect("build node");
        let inst = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_kind == EdgeKind::Instantiates)
            .expect("instantiates ref");
        // generic suffix and namespace qualifier stripped
        assert_eq!(inst.reference_name, "Mapper");
        assert_eq!(inst.from_node_id, build.id);
    }

    #[test]
    fn enum_members_are_extracted() {
        let source = "enum Color {\n  Red,\n  Green,\n}\n";
        let result = extract_ts("src/color.ts", source);

        let color = result
            .nodes
            .iter()
            .find(|n| n.name == "Color")
            .expect("enum node");
        assert_eq!(color.kind, NodeKind::Enum);
        let members: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::EnumMember)
            .collect();
        assert_eq!(members.len(), 2);
        assert!(members.iter().any(|m| m.name == "Red"));
        assert!(members.iter().any(|m| m.name == "Green"));
        assert!(
            members
                .iter()
                .all(|m| m.qualified_name.starts_with("Color::"))
        );
    }

    #[test]
    fn type_alias_members_surface_as_property_and_method() {
        let source = "type RecorderHandle = {\n  id: string;\n  stop: () => void;\n};\n";
        let result = extract_ts("src/recorder.ts", source);

        let alias = result
            .nodes
            .iter()
            .find(|n| n.name == "RecorderHandle")
            .expect("type alias");
        assert_eq!(alias.kind, NodeKind::TypeAlias);

        let id_member = result
            .nodes
            .iter()
            .find(|n| n.name == "id")
            .expect("id member");
        assert_eq!(id_member.kind, NodeKind::Property);
        assert_eq!(id_member.qualified_name, "RecorderHandle::id");

        // function-typed property is method-shaped
        let stop_member = result
            .nodes
            .iter()
            .find(|n| n.name == "stop")
            .expect("stop member");
        assert_eq!(stop_member.kind, NodeKind::Method);
    }

    #[test]
    fn unsupported_language_returns_error_result() {
        let result =
            TreeSitterExtractor::new("file.unknown", "x", Some(Language::Unknown), None).extract();
        assert_eq!(result.nodes.len(), 0);
        assert_eq!(result.errors.len(), 1);
        assert_eq!(result.errors[0].message, "Unsupported language: unknown");
        assert_eq!(
            result.errors[0].code.as_deref(),
            Some("unsupported_language")
        );
    }

    #[test]
    fn decorated_class_emits_decorates_reference() {
        let source = "@Injectable()\nclass Service {\n  run() {}\n}\n";
        let result = extract_ts("src/service.ts", source);

        let service = result
            .nodes
            .iter()
            .find(|n| n.name == "Service")
            .expect("Service node");
        let dec = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_kind == EdgeKind::Decorates)
            .expect("decorates ref");
        assert_eq!(dec.reference_name, "Injectable");
        assert_eq!(dec.from_node_id, service.id);
    }
}
