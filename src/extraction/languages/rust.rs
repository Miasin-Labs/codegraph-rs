//! Rust language extraction config.
//!
//! Ported from `src/extraction/languages/rust.ts`.
//!
//! # Known grammar limitations (tree-sitter-rust 0.24)
//!
//! Extraction is only as good as the pinned grammar's parse. The grammar does
//! not yet model some **nightly / unstable** syntax, and misparses degrade
//! extraction for the affected item only (the rest of the file is unaffected):
//!
//! - **`const trait`** (const-trait feature, e.g. the nightly
//!   `pub const trait Iterator { … }` in `library/core`): the `const trait`
//!   header fails to parse as a `trait_item`, so **no trait node is produced**
//!   and the trait's required methods are hoisted to file scope as bare
//!   `function`s instead of `method|Trait::m`. A plain `pub trait Iterator`
//!   extracts correctly — this is purely the `const` qualifier on the trait.
//!   (`const impl` and `~const` bounds, by contrast, *do* parse on 0.24 and
//!   extract normally.)
//! - **Trait aliases** (`pub trait Combo = Send + Sync;`, the `trait_alias`
//!   feature): the alias is **not extracted at all** — there is no alias node,
//!   and a multi-bound right-hand side can bleed into the following item (a
//!   trailing `trait`/`impl` may be misparsed). Upstream tree-sitter-rust #229.
//! - **Declarative macros 2.0** (`pub macro name($x:expr) { … }`, the
//!   `decl_macro` feature): the `macro` (not `macro_rules!`) form is dropped —
//!   no `Macro` node is produced. Classic `macro_rules!` defs extract fine.
//!   Upstream tree-sitter-rust #45.
//! - Other bleeding-edge syntax the pinned grammar predates can misparse the
//!   same way. The fix is a grammar bump, not an extractor change. Stable Rust
//!   (incl. let-else, GATs, const generics, raw identifiers, `gen` blocks,
//!   safe/unsafe `extern`, auto traits, doc comments) is fully supported.
//!
//! Macro *expansion* is also out of scope by design: tree-sitter parses tokens
//! and does not expand macros, so symbols **generated** by a `macro_rules!`
//! invocation (e.g. a `struct` produced inside `impl_two_step_sync_job! { … }`)
//! are not extracted. The macro *definition* and *invocation site* are
//! recorded; the generated items are not.

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference, Visibility};

/// Helper to get the root crate/module from a scoped path.
fn get_root_module(scoped_node: SyntaxNode<'_>, source: &str) -> String {
    // Recursion guard — depth driven by nested scoped_identifier path segments.
    crate::ensure_sufficient_stack(|| get_root_module_inner(scoped_node, source))
}

fn get_root_module_inner(scoped_node: SyntaxNode<'_>, source: &str) -> String {
    let Some(first_child) = scoped_node.named_child(0) else {
        return get_node_text(scoped_node, source).to_string();
    };
    match first_child.kind() {
        "identifier" | "crate" | "super" | "self" => get_node_text(first_child, source).to_string(),
        "scoped_identifier" => get_root_module(first_child, source),
        _ => get_node_text(first_child, source).to_string(),
    }
}

pub struct RustExtractor;

impl LanguageExtractor for RustExtractor {
    fn function_types(&self) -> &[&str] {
        // `function_signature_item` is a trait *required* method declaration
        // (`fn next(&mut self) -> ...;` with no body) — without it the most
        // important methods in trait-defining files (Iterator::next, Hash::hash,
        // Future::poll) are invisible.
        &["function_item", "function_signature_item"]
    }
    fn class_types(&self) -> &[&str] {
        // Rust has impl blocks
        &[]
    }
    fn method_types(&self) -> &[&str] {
        // Methods are functions in impl blocks
        &["function_item", "function_signature_item"]
    }
    fn interface_types(&self) -> &[&str] {
        &["trait_item"]
    }
    fn struct_types(&self) -> &[&str] {
        // `union_item` is struct-shaped (a `field_declaration_list` body); the
        // generic struct walker handles its fields just like a struct.
        &["struct_item", "union_item"]
    }
    fn enum_types(&self) -> &[&str] {
        &["enum_item"]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enum_variant"]
    }
    fn type_alias_types(&self) -> &[&str] {
        // `type_item` is a top-level / impl `type X = …`; `associated_type` is a
        // trait's `type Item;` (with optional bounds). Both become TypeAlias
        // nodes — without `associated_type` a trait's associated types (e.g.
        // `Iterator::Item`, `Deref::Target`) are invisible.
        &["type_item", "associated_type"]
    }
    fn import_types(&self) -> &[&str] {
        // `extern crate alloc;` is an import edge just like `use`.
        &["use_declaration", "extern_crate_declaration"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["let_declaration", "const_item", "static_item"]
    }
    fn field_types(&self) -> &[&str] {
        // Named struct/union fields (`pub host: String`). The generic field
        // walker extracts each `field_declaration` as a Field node inside the
        // owning struct/union (gated on `is_inside_class_like_node`).
        &["field_declaration"]
    }
    fn interface_kind(&self) -> NodeKind {
        NodeKind::Trait
    }
    fn module_is_class_like(&self) -> bool {
        // Rust modules are namespaces: a free `fn` in a `mod` is a function
        // (not a method) and a module-level `const`/`static` is a variable.
        false
    }
    fn struct_is_definition_without_body(&self) -> bool {
        // `struct Unit;` and `struct Tuple(u32)` have no field-list body but are
        // complete definitions (unlike a C forward declaration).
        true
    }
    fn extract_member_variables(&self) -> bool {
        // Trait/impl associated `const`/`static` are members worth indexing
        // (`Trait::ASSOC`, `Type::MAX`).
        true
    }
    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        // `const_item` → Constant; `static_item`/`let_declaration` → Variable.
        Some(node.kind() == "const_item")
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

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        match node.kind() {
            // `mod foo { ... }` — a Module node that scopes its children so
            // they get module-qualified names (`foo::bar`) and Contains edges.
            // `mod foo;` (no body) still yields the node but has nothing to
            // descend into.
            "mod_item" => {
                let Some(name_node) = node.child_by_field_name("name") else {
                    return false;
                };
                let name = get_node_text(name_node, ctx.source()).to_string();
                let visibility = self.get_visibility(node, ctx.source());
                let Some(module_node) = ctx.create_node(
                    NodeKind::Module,
                    &name,
                    node,
                    NodeExtra {
                        visibility,
                        ..Default::default()
                    },
                ) else {
                    return false;
                };
                if let Some(body) = node.child_by_field_name("body") {
                    ctx.push_scope(module_node.id);
                    for child in named_children(body) {
                        ctx.visit_node(child);
                    }
                    ctx.pop_scope();
                }
                true
            }
            // `macro_rules! name { ... }` — a Macro definition symbol. Macro
            // *expansion* is out of scope; this records the definition so the
            // macro is searchable and callers can be wired by name.
            "macro_definition" => {
                let Some(name_node) = node.child_by_field_name("name") else {
                    return false;
                };
                let name = get_node_text(name_node, ctx.source()).to_string();
                ctx.create_node(NodeKind::Macro, &name, node, NodeExtra::default());
                true
            }
            // `some_macro! { … }` — a macro invocation. We don't expand it (its
            // generated items are out of scope), but we record a `References`
            // edge to the macro name so the call site is wired to the macro
            // definition. This is the link that makes the original
            // `impl_two_step_sync_job!` invocation discoverable. We return
            // `false` so the walker still descends into the token tree for any
            // nested calls/identifiers the grammar did tokenize.
            "macro_invocation" => {
                if let Some(macro_node) = node.child_by_field_name("macro") {
                    // `macro` may be an identifier or a scoped_identifier
                    // (`crate::m!`) — take the last path segment as the name.
                    let raw = get_node_text(macro_node, ctx.source());
                    let name = raw.rsplit("::").next().unwrap_or(raw).to_string();
                    if !name.is_empty() {
                        if let Some(parent_id) = ctx.node_stack().last().cloned() {
                            ctx.add_unresolved_reference(UnresolvedReference {
                                from_node_id: parent_id,
                                reference_name: name,
                                reference_kind: EdgeKind::References,
                                line: node.start_position().row as u32 + 1,
                                column: node.start_position().column as u32,
                                file_path: None,
                                language: None,
                                candidates: None,
                            });
                        }
                    }
                }
                false
            }
            _ => false,
        }
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let return_type = get_child_by_field(node, "return_type");
        let mut sig = get_node_text(params, source).to_string();
        if let Some(rt) = return_type {
            sig.push_str(" -> ");
            sig.push_str(get_node_text(rt, source));
        }
        Some(sig)
    }

    fn is_async(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "async" {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "visibility_modifier" {
                    return Some(if get_node_text(child, source).contains("pub") {
                        Visibility::Public
                    } else {
                        Visibility::Private
                    });
                }
            }
        }
        // Rust defaults to private
        Some(Visibility::Private)
    }

    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // Walk up the tree-sitter AST to find a parent impl_item
        let mut parent = node.parent();
        while let Some(p) = parent {
            if p.kind() == "impl_item" {
                // For `impl Type { ... }` — the type is a direct type_identifier child
                // For `impl Trait for Type { ... }` — the type is the LAST type_identifier
                // (the first is part of the trait path)
                let children = named_children(p);
                // Find all direct type_identifier children (not nested in scoped paths)
                let type_idents: Vec<_> = children
                    .iter()
                    .filter(|c| c.kind() == "type_identifier")
                    .collect();
                if let Some(type_node) = type_idents.last() {
                    // Last type_identifier is always the implementing type
                    return Some(get_node_text(**type_node, source).to_string());
                }
                // Handle generic types: impl<T> MyStruct<T> { ... }
                if let Some(generic_type) = children.iter().find(|c| c.kind() == "generic_type") {
                    if let Some(inner_type) = named_children(*generic_type)
                        .into_iter()
                        .find(|c| c.kind() == "type_identifier")
                    {
                        return Some(get_node_text(inner_type, source).to_string());
                    }
                }
                return None;
            }
            parent = p.parent();
        }
        None
    }

    /// `let fold_pass = ConstantFoldPass;` — a bare PascalCase path initializer
    /// is a unit-struct construction (or enum-variant / associated-const
    /// reference). The generic variable path records the binding but drops the
    /// link to the referenced type, so value-assembled pipelines (unit-struct
    /// passes wired by value, not by call) are invisible as edges. Returns the
    /// referenced symbol so the core emits a `References` edge.
    fn extract_value_reference(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        if node.kind() != "let_declaration" {
            return None;
        }
        let value = get_child_by_field(node, "value")?;
        if !matches!(value.kind(), "identifier" | "scoped_identifier") {
            return None;
        }
        let path = get_node_text(value, source);
        let last = path.rsplit("::").next().unwrap_or(path);
        // PascalCase / SCREAMING_CASE → a type, variant, or const (lowercase
        // initializers are locals — not worth an edge).
        last.chars()
            .next()
            .is_some_and(|c| c.is_uppercase())
            .then(|| last.to_string())
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();

        // Find the use argument. `use_as_clause` (`use a::B as C`) and
        // `use_wildcard` (`use a::*`) wrap the real path, so they must be
        // matched too — otherwise the whole `use` is dropped.
        let use_arg = named_children(node).into_iter().find(|c| {
            matches!(
                c.kind(),
                "scoped_use_list"
                    | "scoped_identifier"
                    | "use_list"
                    | "identifier"
                    | "use_as_clause"
                    | "use_wildcard"
            )
        });

        if let Some(use_arg) = use_arg {
            // For `a::B as C` / `a::*`, resolve the root from the inner `path`
            // field; for the plain forms the node itself is the path.
            let path_node = match use_arg.kind() {
                "use_as_clause" | "use_wildcard" => {
                    get_child_by_field(use_arg, "path").unwrap_or(use_arg)
                }
                _ => use_arg,
            };
            return ImportOutcome::Info(ImportInfo::new(
                get_root_module(path_node, source),
                import_text,
            ));
        }
        ImportOutcome::Declined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn rust_smoke_extraction() {
        let source = "use std::collections::HashMap;\n\npub struct Config {\n    pub name: String,\n}\n\npub trait Runner {\n    fn run(&self);\n}\n\npub enum Mode {\n    Fast,\n    Slow,\n}\n\nimpl Config {\n    pub fn load(path: &str) -> Config {\n        helper();\n        Config { name: path.to_string() }\n    }\n}\n\nasync fn helper() {}\n";
        let result = TreeSitterExtractor::new(
            "src/lib.rs",
            source,
            Some(Language::Rust),
            Some(&RustExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let config = result.nodes.iter().find(|n| n.name == "Config").unwrap();
        assert_eq!(config.kind, NodeKind::Struct);
        assert_eq!(config.visibility, Some(Visibility::Public));

        let runner = result.nodes.iter().find(|n| n.name == "Runner").unwrap();
        // interface_kind override → trait
        assert_eq!(runner.kind, NodeKind::Trait);

        let mode = result.nodes.iter().find(|n| n.name == "Mode").unwrap();
        assert_eq!(mode.kind, NodeKind::Enum);
        let fast = result.nodes.iter().find(|n| n.name == "Fast").unwrap();
        assert_eq!(fast.kind, NodeKind::EnumMember);

        let load = result.nodes.iter().find(|n| n.name == "load").unwrap();
        assert!(
            load.qualified_name.contains("Config"),
            "impl method should carry receiver type, got {:?}",
            load.qualified_name
        );
        assert_eq!(load.signature.as_deref(), Some("(path: &str) -> Config"));

        let helper = result.nodes.iter().find(|n| n.name == "helper").unwrap();
        // TS-parity: the hook scans direct children for an `async` token, but
        // tree-sitter-rust nests it under `function_modifiers`, so detection
        // misses — identical to the TS behavior on the same grammar shape
        // (the TS suite does not assert Rust isAsync).
        assert_eq!(helper.is_async, Some(false));
        assert_eq!(helper.visibility, Some(Visibility::Private));

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        // Root module of `use std::collections::HashMap;` is `std`
        assert_eq!(import.name, "std");
    }

    /// Pins the six previously-missing Rust constructs: `function_signature_item`
    /// (trait required methods), `mod_item` (modules + module-qualified child
    /// names), struct fields, `macro_definition`, `union_item`, and
    /// `foreign_mod_item` (extern blocks) + `extern crate`.
    #[test]
    fn rust_extracts_previously_missed_constructs() {
        let source = r#"
pub mod config {
    pub const MAX: u32 = 10;
    pub fn helper() -> u32 { MAX }
    pub struct Inner { pub v: u32 }
}

pub union MyUnion { a: u32, b: f32 }

macro_rules! my_macro { () => {}; }

pub trait MyTrait {
    fn required_method(&self) -> u32;
    fn provided_method(&self) -> u32 { 0 }
}

extern crate alloc;

extern "C" {
    fn c_fn(x: i32) -> i32;
}
"#;
        let result = TreeSitterExtractor::new(
            "src/lib.rs",
            source,
            Some(Language::Rust),
            Some(&RustExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let find = |name: &str| result.nodes.iter().find(|n| n.name == name);

        // mod_item → Module node; free fn inside it is a FUNCTION (not method)
        // and carries a module-qualified name; module-level const stays a variable.
        let module = find("config").expect("module node");
        assert_eq!(module.kind, NodeKind::Module);
        let helper = find("helper").expect("free fn in module");
        assert_eq!(helper.kind, NodeKind::Function);
        assert_eq!(helper.qualified_name, "config::helper");
        let max = find("MAX").expect("module-level const");
        // `const` → Constant (a `static` would be Variable); module-qualified.
        assert_eq!(max.kind, NodeKind::Constant);
        assert_eq!(max.qualified_name, "config::MAX");

        // struct field (inside a module) → Field node, fully qualified
        let inner = find("Inner").expect("nested struct");
        assert_eq!(inner.kind, NodeKind::Struct);
        let field_v = find("v").expect("struct field");
        assert_eq!(field_v.kind, NodeKind::Field);
        assert_eq!(field_v.qualified_name, "config::Inner::v");

        // union_item → Struct-kind with extracted fields
        let union = find("MyUnion").expect("union node");
        assert_eq!(union.kind, NodeKind::Struct);
        assert!(find("a").is_some_and(|n| n.kind == NodeKind::Field));

        // macro_definition → Macro node
        let mac = find("my_macro").expect("macro def node");
        assert_eq!(mac.kind, NodeKind::Macro);

        // trait required method (function_signature_item) → Method, qualified
        let required = find("required_method").expect("trait required method");
        assert_eq!(required.kind, NodeKind::Method);
        assert_eq!(required.qualified_name, "MyTrait::required_method");

        // extern crate → Import node
        assert!(
            result
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::Import && n.name == "alloc"),
            "extern crate alloc should be an import"
        );

        // foreign function in `extern "C"` block (function_signature_item)
        let c_fn = find("c_fn").expect("FFI fn in extern block");
        assert!(matches!(c_fn.kind, NodeKind::Function | NodeKind::Method));
    }

    /// Pins a second batch of constructs: unit structs, tuple structs (+
    /// positional fields), trait associated types/consts, and impl-block
    /// associated `const`/`type` with `Type::member` qualified names.
    #[test]
    fn rust_extracts_unit_tuple_and_associated_items() {
        let source = r#"
pub struct Unit;
pub struct Pair(pub u32, String);

pub trait Container {
    type Item;
    const CAPACITY: usize;
    fn get(&self) -> Self::Item;
}

pub struct Reg;
impl Reg {
    pub const MAX: u32 = 100;
    pub type Alias = u32;
    pub fn make() -> Self { Reg }
}
"#;
        let result = TreeSitterExtractor::new(
            "src/lib.rs",
            source,
            Some(Language::Rust),
            Some(&RustExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let find = |name: &str| result.nodes.iter().find(|n| n.name == name);

        // Unit struct (no body) is a definition, not skipped.
        assert_eq!(find("Unit").map(|n| n.kind), Some(NodeKind::Struct));

        // Tuple struct + positional fields named by index.
        assert_eq!(find("Pair").map(|n| n.kind), Some(NodeKind::Struct));
        let f0 = result
            .nodes
            .iter()
            .find(|n| n.name == "0" && n.qualified_name == "Pair::0")
            .expect("tuple field 0");
        assert_eq!(f0.kind, NodeKind::Field);
        assert!(result.nodes.iter().any(|n| n.qualified_name == "Pair::1"));

        // Trait associated type + associated const, qualified to the trait.
        let item = result
            .nodes
            .iter()
            .find(|n| n.name == "Item" && n.qualified_name == "Container::Item")
            .expect("assoc type");
        assert_eq!(item.kind, NodeKind::TypeAlias);
        let cap = result
            .nodes
            .iter()
            .find(|n| n.name == "CAPACITY" && n.qualified_name == "Container::CAPACITY")
            .expect("trait const");
        assert_eq!(cap.kind, NodeKind::Constant);

        // impl-block associated const + type alias, qualified to the Self type.
        let max = find("MAX").expect("impl const");
        assert_eq!(max.kind, NodeKind::Constant);
        assert_eq!(max.qualified_name, "Reg::MAX");
        let alias = find("Alias").expect("impl type alias");
        assert_eq!(alias.kind, NodeKind::TypeAlias);
        assert_eq!(alias.qualified_name, "Reg::Alias");
    }

    /// Pins aliased/wildcard `use` imports, enum-variant fields (struct + tuple
    /// variants), and the `References` edge from a macro invocation to its
    /// definition.
    #[test]
    fn rust_extracts_aliased_use_enum_fields_and_macro_refs() {
        let source = r#"
use std::collections::HashMap as Map;
use std::fmt::*;

macro_rules! define_thing { ($n:ident) => {}; }
define_thing! { Generated }

pub enum Shape {
    Circle { radius: f64 },
    Rect(f64, f64),
    Unit,
}
"#;
        let result = TreeSitterExtractor::new(
            "src/lib.rs",
            source,
            Some(Language::Rust),
            Some(&RustExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        // Aliased + wildcard `use` both yield an import rooted at `std`.
        let std_imports = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Import && n.name == "std")
            .count();
        assert_eq!(std_imports, 2, "aliased + wildcard use should both import");

        // Struct-variant fields are qualified under the variant.
        assert!(
            result
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::Field && n.qualified_name == "Shape::Circle::radius"),
            "struct-variant field `radius` should be extracted"
        );
        // Tuple-variant fields are index-named under the variant.
        assert!(
            result
                .nodes
                .iter()
                .any(|n| n.kind == NodeKind::Field && n.qualified_name == "Shape::Rect::0"),
            "tuple-variant field `0` should be extracted"
        );

        // The macro invocation `define_thing! { … }` produces a `References`
        // edge to the macro definition (unresolved at extraction time).
        let macro_def = result
            .nodes
            .iter()
            .find(|n| n.name == "define_thing" && n.kind == NodeKind::Macro)
            .expect("macro def node");
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "define_thing"
                    && r.reference_kind == EdgeKind::References),
            "macro invocation should reference its definition `{}`",
            macro_def.name
        );
    }

    /// Documents the `const trait` grammar limitation (tree-sitter-rust 0.24):
    /// a normal `trait` extracts its methods as `Trait::method`, but a nightly
    /// `const trait` fails to parse as a trait, hoisting its methods to bare
    /// file-scope functions. If this test starts failing because `ConstIter`
    /// IS found, the grammar gained `const trait` support — delete the negative
    /// assertion and the limitation note in the module docs.
    #[test]
    fn rust_const_trait_is_a_known_grammar_limitation() {
        let source = "pub const trait ConstIter {\n    fn cnext(&mut self) -> u32;\n}\npub trait NormalIter {\n    fn nnext(&mut self) -> u32;\n}\n";
        let result = TreeSitterExtractor::new(
            "src/lib.rs",
            source,
            Some(Language::Rust),
            Some(&RustExtractor),
        )
        .extract();

        // Normal trait: required method is a qualified Method.
        let nnext = result.nodes.iter().find(|n| n.name == "nnext").unwrap();
        assert_eq!(nnext.kind, NodeKind::Method);
        assert_eq!(nnext.qualified_name, "NormalIter::nnext");

        // const trait: grammar misparse → no trait node, method hoisted to fn.
        assert!(
            !result
                .nodes
                .iter()
                .any(|n| n.name == "ConstIter" && n.kind == NodeKind::Trait),
            "tree-sitter-rust 0.24 cannot parse `const trait`; if this fails the \
             grammar was upgraded — update the limitation note in the module docs"
        );
    }

    /// Documents two more grammar limitations (tree-sitter-rust 0.24): trait
    /// aliases (upstream #229) and declarative macros 2.0 (upstream #45) are not
    /// extracted. If these start failing because the symbol IS found, the
    /// grammar gained support — remove the assertion + the module-doc note.
    #[test]
    fn rust_trait_alias_and_macro2_are_known_grammar_limitations() {
        let extract = |source: &str| {
            TreeSitterExtractor::new(
                "src/lib.rs",
                source,
                Some(Language::Rust),
                Some(&RustExtractor),
            )
            .extract()
        };

        // Trait alias (upstream #229): the alias name is not extracted; the
        // trailing anchor still parses. (`;`-terminated alias — the reliable,
        // context-independent shape.)
        let alias = extract("pub trait Combo = Send + Sync;\npub fn anchor() {}\n");
        assert!(alias.nodes.iter().any(|n| n.name == "anchor"));
        assert!(
            !alias.nodes.iter().any(|n| n.name == "Combo"),
            "tree-sitter-rust 0.24 does not parse trait aliases (upstream #229); \
             if this fails the grammar was upgraded — update the module docs"
        );

        // Declarative macro 2.0 (upstream #45): the `macro` form is dropped,
        // while the anchor and classic `macro_rules!` still extract.
        let mac = extract(
            "pub macro mac2($x:expr) { $x }\nmacro_rules! classic { () => {}; }\npub fn anchor() {}\n",
        );
        assert!(mac.nodes.iter().any(|n| n.name == "anchor"));
        assert!(
            mac.nodes
                .iter()
                .any(|n| n.name == "classic" && n.kind == NodeKind::Macro),
            "classic macro_rules! should still extract"
        );
        assert!(
            !mac.nodes.iter().any(|n| n.name == "mac2"),
            "tree-sitter-rust 0.24 does not parse `macro` 2.0 defs (upstream #45); \
             if this fails the grammar was upgraded — update the module docs"
        );
    }
}
