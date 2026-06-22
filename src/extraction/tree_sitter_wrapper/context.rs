use std::time::{SystemTime, UNIX_EPOCH};

use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{LanguageExtractor, SyntaxNode};

/// Epoch milliseconds (`Date.now()` parity).
pub(super) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Collect a node's named children into a Vec (the TS `namedChildren` array).
pub(super) fn named_children<'t>(node: SyntaxNode<'t>) -> Vec<SyntaxNode<'t>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

/// First named child with the given kind (TS `namedChildren.find(c => c.type === kind)`).
pub(super) fn find_named_child<'t>(node: SyntaxNode<'t>, kind: &str) -> Option<SyntaxNode<'t>> {
    named_children(node).into_iter().find(|c| c.kind() == kind)
}

/// `path.basename(p)` parity for forward-slash paths.
pub(super) fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// Strip a namespace qualifier: keep the trailing identifier after the
/// rightmost `.` or `::`, then drop ONE leading `:`/`.` left by the `::`
/// split (TS `slice(lastDot + 1).replace(/^[:.]/, '')`).
pub(super) fn strip_qualifier(name: &str) -> String {
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
pub(super) fn init_signature(value_text: &str) -> String {
    let truncated: String = value_text.chars().take(100).collect();
    let long = value_text.chars().nth(99).is_some();
    format!("= {}{}", truncated, if long { "..." } else { "" })
}

/// Extract the name from a node based on language
pub(super) fn extract_name(
    node: SyntaxNode<'_>,
    source: &str,
    extractor: &dyn LanguageExtractor,
) -> String {
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
