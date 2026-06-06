//! Kotlin language extraction config.
//!
//! Ported from `src/extraction/languages/kotlin.ts`.

use super::find_named_child;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ClassLikeKind,
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

/// Check if a node matches the `fun interface` misparse pattern
fn is_fun_interface_node(node: SyntaxNode<'_>, source: &str) -> bool {
    let mut has_fun = false;
    let mut has_interface_type = false;
    for i in 0..node.child_count() as u32 {
        let Some(child) = node.child(i) else {
            continue;
        };
        if child.kind() == "fun" && !child.is_named() {
            has_fun = true;
        }
        if child.kind() == "user_type" {
            if let Some(type_id) = find_named_child(child, "type_identifier") {
                if get_node_text(type_id, source) == "interface" {
                    has_interface_type = true;
                }
            }
        }
        // Pattern 2b: user_type("interface") is inside an ERROR child
        if child.kind() == "ERROR" {
            for j in 0..child.child_count() as u32 {
                if let Some(gc) = child.child(j) {
                    if gc.kind() == "user_type" {
                        if let Some(type_id) = find_named_child(gc, "type_identifier") {
                            if get_node_text(type_id, source) == "interface" {
                                has_interface_type = true;
                            }
                        }
                    }
                }
            }
        }
    }
    has_fun && has_interface_type
}

pub struct KotlinExtractor;

impl LanguageExtractor for KotlinExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_declaration"]
    }
    fn class_types(&self) -> &[&str] {
        &["class_declaration"]
    }
    fn method_types(&self) -> &[&str] {
        // Methods are functions inside classes
        &["function_declaration"]
    }
    fn interface_types(&self) -> &[&str] {
        // Handled via classify_class_node
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        // Kotlin uses data classes
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        // Handled via classify_class_node
        &[]
    }
    fn enum_member_types(&self) -> &[&str] {
        &["enum_entry"]
    }
    fn type_alias_types(&self) -> &[&str] {
        &["type_alias"]
    }
    fn import_types(&self) -> &[&str] {
        // TS (fwcd grammar): `import_header`. The native tree-sitter-kotlin-ng
        // grammar names the node `import`; both are listed so the config stays
        // a superset of the TS one.
        &["import_header", "import"]
    }
    fn call_types(&self) -> &[&str] {
        &["call_expression"]
    }
    fn variable_types(&self) -> &[&str] {
        &["property_declaration"]
    }
    fn field_types(&self) -> &[&str] {
        &["property_declaration"]
    }
    fn extra_class_node_types(&self) -> &[&str] {
        &["object_declaration"]
    }
    fn name_field(&self) -> &str {
        "simple_identifier"
    }
    fn body_field(&self) -> &str {
        "function_body"
    }
    fn params_field(&self) -> &str {
        "function_value_parameters"
    }
    fn return_field(&self) -> Option<&str> {
        Some("type")
    }

    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        // Handle Kotlin `fun interface` declarations.
        // Tree-sitter-kotlin doesn't support `fun interface` syntax (Kotlin 1.4+).
        // It produces two different misparse patterns:
        //   Pattern 1 (simple): ERROR node + sibling lambda_literal for body
        //   Pattern 2 (complex): function_declaration misparse with ERROR child
        // Skip lambda_literal bodies that were already consumed by a fun interface ERROR node
        if node.kind() == "lambda_literal" {
            if let Some(prev) = node.prev_sibling() {
                if prev.kind() == "ERROR" && is_fun_interface_node(prev, ctx.source()) {
                    return true;
                }
            }
            return false;
        }

        if node.kind() != "ERROR" && node.kind() != "function_declaration" {
            return false;
        }

        // Skip ERROR nodes that are class bodies (start with `{`). These contain parent
        // methods + trailing `fun interface` tokens. The methods are extracted via
        // resolve_body; handling the ERROR here would consume the whole body.
        if node.kind() == "ERROR" {
            if let Some(first_child) = node.child(0) {
                if first_child.kind() == "{" {
                    return false;
                }
            }
        }

        if !is_fun_interface_node(node, ctx.source()) {
            return false;
        }

        // Extract the interface name.
        // For function_declaration misparses (patterns 2a/2b), the real name is inside
        // an ERROR child — direct simple_identifier children are the misparsed method name.
        let mut name_text: Option<String> = None;
        if node.kind() == "function_declaration" {
            'outer: for i in 0..node.child_count() as u32 {
                if let Some(child) = node.child(i) {
                    if child.kind() == "ERROR" {
                        for j in 0..child.child_count() as u32 {
                            if let Some(gc) = child.child(j) {
                                if gc.kind() == "simple_identifier" {
                                    name_text = Some(get_node_text(gc, ctx.source()).to_string());
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
            }
        }
        // Fallback: direct simple_identifier child (Pattern 1: ERROR node at top level)
        if name_text.is_none() {
            for i in 0..node.child_count() as u32 {
                if let Some(child) = node.child(i) {
                    if child.kind() == "simple_identifier" {
                        name_text = Some(get_node_text(child, ctx.source()).to_string());
                        break;
                    }
                }
            }
        }
        let Some(name_text) = name_text else {
            return false;
        };

        // Create the interface node
        let Some(iface_node) =
            ctx.create_node(NodeKind::Interface, &name_text, node, NodeExtra::default())
        else {
            return false;
        };

        ctx.push_scope(iface_node.id);

        if node.kind() == "ERROR" {
            // Pattern 1: body is in the next sibling lambda_literal
            if let Some(next_sibling) = node.next_sibling() {
                if next_sibling.kind() == "lambda_literal" {
                    for i in 0..next_sibling.named_child_count() as u32 {
                        if let Some(child) = next_sibling.named_child(i) {
                            if child.kind() == "statements" {
                                for j in 0..child.named_child_count() as u32 {
                                    if let Some(stmt) = child.named_child(j) {
                                        ctx.visit_node(stmt);
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        // Pattern 2 (function_declaration): nested classes are siblings at source_file level,
        // already visited by the normal traversal. The single abstract method is misparsed
        // and cannot be reliably recovered, but the interface node itself is the key value.

        ctx.pop_scope();
        true
    }

    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, _body_field: &str) -> Option<SyntaxNode<'t>> {
        // Kotlin's tree-sitter grammar doesn't use field names, so getChildByField fails.
        // Find body by type: function_body for functions/methods, class_body for classes,
        // enum_class_body for enums.
        //
        // Special case: when a class/interface contains a nested `fun interface`, tree-sitter
        // misparsed the parent's body as an ERROR node (starting with `{`) and creates
        // a class_body sibling for the nested interface's body. Prefer the ERROR body
        // so the parent's methods are extracted.
        for i in 0..node.named_child_count() as u32 {
            let Some(child) = node.named_child(i) else {
                continue;
            };
            if child.kind() == "ERROR" {
                if let Some(first_child) = child.child(0) {
                    if first_child.kind() == "{" {
                        return Some(child);
                    }
                }
            }
            if child.kind() == "function_body"
                || child.kind() == "class_body"
                || child.kind() == "enum_class_body"
            {
                return Some(child);
            }
        }
        None
    }

    fn classify_class_node(&self, node: SyntaxNode<'_>, source: &str) -> ClassLikeKind {
        // Kotlin reuses class_declaration for classes, interfaces, and enums.
        // Detect by checking for keyword children:
        //   interface Foo { }       → has 'interface' keyword child
        //   enum class Level { }    → has 'enum' keyword child
        //   class / data class / abstract class → default 'class'
        // The native tree-sitter-kotlin-ng grammar nests the `enum` keyword
        // inside `modifiers > class_modifier` (the fwcd grammar the TS code
        // targeted made it a direct child), so modifiers are scanned too.
        for i in 0..node.child_count() as u32 {
            let Some(child) = node.child(i) else {
                continue;
            };
            if child.kind() == "interface" {
                return ClassLikeKind::Interface;
            }
            if child.kind() == "enum" {
                return ClassLikeKind::Enum;
            }
            if child.kind() == "modifiers" {
                for j in 0..child.named_child_count() as u32 {
                    if let Some(modifier) = child.named_child(j) {
                        if modifier.kind() == "class_modifier" {
                            match get_node_text(modifier, source) {
                                "enum" => return ClassLikeKind::Enum,
                                "interface" => return ClassLikeKind::Interface,
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
        ClassLikeKind::Class
    }

    fn get_receiver_type(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // Kotlin extension functions: fun Type.method() { }
        // AST: function_declaration > user_type, ".", simple_identifier
        // The user_type before the dot is the receiver type.
        let mut found_user_type: Option<SyntaxNode<'_>> = None;
        for i in 0..node.child_count() as u32 {
            let Some(child) = node.child(i) else {
                continue;
            };
            if child.kind() == "user_type" {
                found_user_type = Some(child);
            } else if child.kind() == "." {
                if let Some(user_type) = found_user_type {
                    // The user_type before the dot is the receiver type
                    return Some(match find_named_child(user_type, "type_identifier") {
                        Some(type_id) => get_node_text(type_id, source).to_string(),
                        None => get_node_text(user_type, source).to_string(),
                    });
                }
            } else if child.kind() == "simple_identifier"
                || child.kind() == "function_value_parameters"
            {
                // Past the function name — no receiver
                break;
            }
        }
        None
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // Kotlin function signature: fun name(params): ReturnType
        let params = get_child_by_field(node, "function_value_parameters")?;
        let return_type = get_child_by_field(node, "type");
        let mut sig = get_node_text(params, source).to_string();
        if let Some(rt) = return_type {
            sig.push_str(": ");
            sig.push_str(get_node_text(rt, source));
        }
        Some(sig)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        // Check for visibility modifiers in Kotlin
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifiers" {
                    let text = get_node_text(child, source);
                    if text.contains("public") {
                        return Some(Visibility::Public);
                    }
                    if text.contains("private") {
                        return Some(Visibility::Private);
                    }
                    if text.contains("protected") {
                        return Some(Visibility::Protected);
                    }
                    if text.contains("internal") {
                        return Some(Visibility::Internal);
                    }
                }
            }
        }
        // Kotlin defaults to public
        Some(Visibility::Public)
    }

    fn is_static(&self, _node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        // Kotlin doesn't have static, uses companion objects
        Some(false)
    }

    fn is_async(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        // Kotlin uses suspend keyword for coroutines
        for i in 0..node.child_count() as u32 {
            if let Some(child) = node.child(i) {
                if child.kind() == "modifiers" && get_node_text(child, source).contains("suspend") {
                    return Some(true);
                }
            }
        }
        Some(false)
    }

    fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
        let import_text = get_node_text(node, source).trim();
        // tree-sitter-kotlin-ng wraps the dotted path in `qualified_identifier`
        // (the fwcd grammar used a single dotted `identifier`); an aliased
        // import's alias is a separate trailing `identifier`, so the qualified
        // path is preferred. Produces the same module names the TS suite
        // asserts (`java.io.IOException`, alias/wildcard stripped).
        if let Some(qualified) = find_named_child(node, "qualified_identifier") {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(qualified, source),
                import_text,
            ));
        }
        if let Some(identifier) = find_named_child(node, "identifier") {
            return ImportOutcome::Info(ImportInfo::new(
                get_node_text(identifier, source),
                import_text,
            ));
        }
        ImportOutcome::Declined
    }

    fn package_types(&self) -> &[&str] {
        &["package_header"]
    }

    fn extract_package(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        // package_header → identifier (dotted: `com.example.foo`)
        // (kotlin-ng: a `qualified_identifier` wrapping the segments)
        find_named_child(node, "qualified_identifier")
            .or_else(|| find_named_child(node, "identifier"))
            .map(|id| get_node_text(id, source).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn kotlin_smoke_extraction() {
        let source = "package com.example.app\n\nimport com.example.lib.Util\n\nclass Engine {\n    private fun ignite() {\n        warmUp()\n    }\n}\n\ninterface Drivable {\n    fun drive()\n}\n\nenum class Level { LOW, HIGH }\n\nsuspend fun warmUp() {}\n";
        let result = TreeSitterExtractor::new(
            "src/Engine.kt",
            source,
            Some(Language::Kotlin),
            Some(&KotlinExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result.nodes.iter().find(|n| n.name == "Engine").unwrap();
        assert_eq!(class.kind, NodeKind::Class);

        // class_declaration with `interface` keyword → interface via classify
        let iface = result.nodes.iter().find(|n| n.name == "Drivable").unwrap();
        assert_eq!(iface.kind, NodeKind::Interface);

        // enum class → enum via classify
        let level = result.nodes.iter().find(|n| n.name == "Level").unwrap();
        assert_eq!(level.kind, NodeKind::Enum);

        let ignite = result.nodes.iter().find(|n| n.name == "ignite").unwrap();
        assert_eq!(ignite.kind, NodeKind::Method);
        assert_eq!(ignite.visibility, Some(Visibility::Private));

        let warm_up = result.nodes.iter().find(|n| n.name == "warmUp").unwrap();
        assert_eq!(warm_up.is_async, Some(true));

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "com.example.lib.Util");

        // package_header wraps top-level declarations in a namespace node
        let ns = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Namespace)
            .expect("namespace node");
        assert_eq!(ns.name, "com.example.app");
    }

    #[test]
    fn kotlin_extension_function_receiver() {
        let source = "fun String.shout(): String = this.uppercase()\n";
        let result = TreeSitterExtractor::new(
            "src/Ext.kt",
            source,
            Some(Language::Kotlin),
            Some(&KotlinExtractor),
        )
        .extract();
        let shout = result
            .nodes
            .iter()
            .find(|n| n.name == "shout")
            .expect("extension fn");
        assert!(
            shout.qualified_name.contains("String"),
            "extension fn should carry receiver type, got {:?}",
            shout.qualified_name
        );
    }
}
