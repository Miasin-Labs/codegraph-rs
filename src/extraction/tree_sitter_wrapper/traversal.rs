use super::calls::INSTANTIATION_KINDS;
use super::context::{basename, named_children, now_ms};
use super::extractor::TreeSitterExtractor;
use super::type_annotations::is_type_annotation_language;
use crate::ensure_sufficient_stack;
use crate::extraction::grammars::{create_parser, is_language_supported};
use crate::extraction::tree_sitter_types::{ClassLikeKind, ClassMemberKind, SyntaxNode};
use crate::types::{ExtractionError, ExtractionResult, Language, Node, NodeKind, Severity};

impl<'a> TreeSitterExtractor<'a> {
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

        let parsed_source = self
            .extractor
            .map(|extractor| extractor.pre_parse(self.source, &self.file_path))
            .unwrap_or_else(|| std::borrow::Cow::Borrowed(self.source));

        match parser.parse(parsed_source.as_ref(), None) {
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
                    address: None,
                    size: None,
                    docstring: None,
                    signature: None,
                    return_type: None,
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
    pub(super) fn visit_node(&mut self, node: SyntaxNode<'_>) {
        ensure_sufficient_stack(|| self.visit_node_inner(node));
    }

    pub(super) fn visit_node_inner(&mut self, node: SyntaxNode<'_>) {
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
            if ext.classify_method_node(node, self.source) == ClassMemberKind::Property {
                let property = self.extract_property(node);
                if let (Some(property), Some(value)) = (property, node.child_by_field_name("value"))
                {
                    self.node_stack.push(property.id);
                    self.visit_function_body(value, "");
                    self.node_stack.pop();
                }
            } else {
                self.extract_method(node);
            }
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
        // Only extract top-level variables (not inside functions/methods),
        // EXCEPT associated `const`/`static` members of a Rust trait/impl, which
        // are real symbols (`Trait::ASSOC`, `Type::MAX`). `extract_member_
        // variables()` opts a language in; the generic gate still excludes
        // function-local `let`s (those are inside a Function scope, which is not
        // class-like).
        else if ext.variable_types().contains(&node_type)
            && (!self.is_inside_class_like_node() || ext.extract_member_variables())
        {
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
            // Scope the impl body to its implementing type so associated
            // `const`/`type`/`fn` get a `Type::member` qualified name and a
            // Contains edge to the type. Methods already recover the receiver
            // via `get_receiver_type`, but associated consts/type aliases do
            // not — without this they'd be bare `MAX`/`Item`. We push the type
            // node (if we've already extracted it) for the duration of the
            // body walk, then restore.
            if let Some(type_id) = self.rust_impl_self_type_id(node) {
                self.node_stack.push(type_id);
                for child in named_children(node) {
                    self.visit_node(child);
                }
                self.node_stack.pop();
                skip_children = true;
            }
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
}
