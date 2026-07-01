use super::context::find_named_child;
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::types::{EdgeKind, UnresolvedReference};

/// Tree-sitter node kinds that represent constructor invocations
/// (`new Foo()` and friends). Used by extract_instantiation to emit
/// an `instantiates` reference targeting the class name.
pub(super) const INSTANTIATION_KINDS: &[&str] = &[
    "new_expression",               // typescript / javascript / tsx / jsx
    "object_creation_expression",   // java / c#
    "instance_creation_expression", // some grammars
];

impl<'a> TreeSitterExtractor<'a> {
    /// Extract a function call
    pub(super) fn extract_call(&mut self, node: SyntaxNode<'_>) {
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
                metadata: None,
            });
        }
    }
}
