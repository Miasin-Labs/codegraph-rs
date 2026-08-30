//! JavaScript language extraction config.
//!
//! Ported from `src/extraction/languages/javascript.ts`.

use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    SyntaxNode,
};

pub struct JavascriptExtractor;

#[derive(Clone, Copy)]
enum LexState {
    Code,
    SingleQuoted,
    DoubleQuoted,
    Template,
    LineComment,
    BlockComment,
}

impl JavascriptExtractor {
    /// Return a bounded lexical prefix with comments and string literals
    /// replaced by spaces. Newlines are retained so a line-comment `export`
    /// cannot leak into the declaration on the following line.
    fn sanitized_prefix(node: SyntaxNode<'_>, source: &str) -> String {
        let start = node.start_byte().min(source.len());
        let prefix_source = &source[..start];
        let prefix_start = prefix_source
            .char_indices()
            .rev()
            .nth(4_096)
            .map(|(index, _)| index)
            .unwrap_or(0);
        let mut chars = prefix_source[prefix_start..].chars().peekable();
        let mut output = String::new();
        let mut state = LexState::Code;
        while let Some(ch) = chars.next() {
            match state {
                LexState::Code => match (ch, chars.peek().copied()) {
                    ('/', Some('/')) => {
                        output.push(' ');
                        output.push(' ');
                        chars.next();
                        state = LexState::LineComment;
                    }
                    ('/', Some('*')) => {
                        output.push(' ');
                        output.push(' ');
                        chars.next();
                        state = LexState::BlockComment;
                    }
                    ('\'', _) => {
                        output.push(' ');
                        state = LexState::SingleQuoted;
                    }
                    ('"', _) => {
                        output.push(' ');
                        state = LexState::DoubleQuoted;
                    }
                    ('`', _) => {
                        output.push(' ');
                        state = LexState::Template;
                    }
                    _ => output.push(ch),
                },
                LexState::LineComment => {
                    if ch == '\n' {
                        output.push('\n');
                        state = LexState::Code;
                    } else {
                        output.push(' ');
                    }
                }
                LexState::BlockComment => {
                    if ch == '*' && chars.peek() == Some(&'/') {
                        output.push(' ');
                        output.push(' ');
                        chars.next();
                        state = LexState::Code;
                    } else if ch == '\n' {
                        output.push('\n');
                    } else {
                        output.push(' ');
                    }
                }
                LexState::SingleQuoted | LexState::DoubleQuoted | LexState::Template => {
                    let closing = match state {
                        LexState::SingleQuoted => '\'',
                        LexState::DoubleQuoted => '"',
                        LexState::Template => '`',
                        _ => unreachable!(),
                    };
                    if ch == '\\' {
                        output.push(' ');
                        if chars.next().is_some() {
                            output.push(' ');
                        }
                    } else if ch == closing {
                        output.push(' ');
                        state = LexState::Code;
                    } else if ch == '\n' {
                        output.push('\n');
                        if !matches!(state, LexState::Template) {
                            state = LexState::Code;
                        }
                    } else {
                        output.push(' ');
                    }
                }
            }
        }
        output
    }

    fn has_export_modifier(prefix: &str) -> bool {
        let prefix = prefix.trim_end();
        ["export", "export default", "export async"]
            .into_iter()
            .any(|modifier| {
                let Some(before_modifier) = prefix.strip_suffix(modifier) else {
                    return false;
                };
                before_modifier.chars().next_back().is_none_or(|before| {
                    !before.is_ascii_alphanumeric()
                        && before != '_'
                        && before != '$'
                        && before != '.'
                })
            })
    }

    /// Arrow/function-expression nodes begin after the declaration keyword, so
    /// recover the nearest bounded `const`/`let`/`var` binding without parent
    /// walks and inspect the sanitized tokens immediately before it.
    fn exported_variable_initializer(node: SyntaxNode<'_>, prefix: &str) -> bool {
        if !matches!(node.kind(), "arrow_function" | "function_expression") {
            return false;
        }
        let binding_start = ["const ", "let ", "var "]
            .into_iter()
            .filter_map(|keyword| prefix.rfind(keyword))
            .max();
        let Some(binding_start) = binding_start else {
            return false;
        };
        Self::has_export_modifier(&prefix[..binding_start])
    }
}

impl LanguageExtractor for JavascriptExtractor {
    fn function_types(&self) -> &[&str] {
        &[
            "function_declaration",
            "arrow_function",
            "function_expression",
        ]
    }
    fn class_types(&self) -> &[&str] {
        &["class_declaration"]
    }
    fn method_types(&self) -> &[&str] {
        &["method_definition", "field_definition"]
    }
    fn interface_types(&self) -> &[&str] {
        &[]
    }
    fn struct_types(&self) -> &[&str] {
        &[]
    }
    fn enum_types(&self) -> &[&str] {
        &[]
    }
    fn type_alias_types(&self) -> &[&str] {
        &[]
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

    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, body_field: &str) -> Option<SyntaxNode<'t>> {
        // field_definition (arrow function class fields) nest the body inside
        // an arrow_function or function_expression child:
        //   field_definition → arrow_function → body (statement_block)
        // Also handles wrapper patterns like: field = throttle((e) => { ... })
        //   field_definition → call_expression → arguments → arrow_function → body
        if node.kind() == "field_definition" {
            for i in 0..node.named_child_count() as u32 {
                let Some(child) = node.named_child(i) else {
                    continue;
                };
                if child.kind() == "arrow_function" || child.kind() == "function_expression" {
                    return get_child_by_field(child, body_field);
                }
                if child.kind() == "call_expression" {
                    if let Some(args) = get_child_by_field(child, "arguments") {
                        let mut cursor = args.walk();
                        for arg in args.named_children(&mut cursor) {
                            if arg.kind() == "arrow_function" || arg.kind() == "function_expression"
                            {
                                return get_child_by_field(arg, body_field);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        Some(get_node_text(params, source).to_string())
    }

    fn is_exported(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        // Avoid `node.parent()` here. In tree-sitter, finding a parent walks down
        // from the tree root to the descendant. Bun's `lots-of-for-loop.js`
        // fixture has hundreds of thousands of nested `for` statements, so doing
        // that parent lookup for every nested `let` declaration becomes
        // quadratic and can make indexing look hung. JavaScript export modifiers
        // appear immediately before the declaration they export, so a bounded
        // lexical prefix check is enough for declaration extraction.
        let prefix = Self::sanitized_prefix(node, source);
        Some(
            Self::has_export_modifier(&prefix)
                || Self::exported_variable_initializer(node, &prefix),
        )
    }

    fn is_async(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "async" {
                return Some(true);
            }
        }
        Some(false)
    }

    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        if node.kind() == "lexical_declaration" {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "const" {
                    return Some(true);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn javascript_smoke_extraction() {
        let source = "import { helper } from './util.js';\n\nexport async function main() {\n  helper();\n}\n\nclass Widget {\n  render() {\n    main();\n  }\n}\n\nconst MAX = 5;\nlet count = 0;\n";
        let result = TreeSitterExtractor::new(
            "src/app.js",
            source,
            Some(Language::Javascript),
            Some(&JavascriptExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let func = result.nodes.iter().find(|n| n.name == "main").unwrap();
        assert_eq!(func.kind, NodeKind::Function);
        assert_eq!(func.is_exported, Some(true));
        assert_eq!(func.is_async, Some(true));
        assert_eq!(func.signature.as_deref(), Some("()"));

        let class = result.nodes.iter().find(|n| n.name == "Widget").unwrap();
        assert_eq!(class.kind, NodeKind::Class);
        let method = result.nodes.iter().find(|n| n.name == "render").unwrap();
        assert_eq!(method.kind, NodeKind::Method);

        let constant = result.nodes.iter().find(|n| n.name == "MAX").unwrap();
        assert_eq!(constant.kind, NodeKind::Constant);
        let variable = result.nodes.iter().find(|n| n.name == "count").unwrap();
        assert_eq!(variable.kind, NodeKind::Variable);

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "./util.js");
    }

    #[test]
    fn exported_arrow_binding_is_marked_exported_without_parent_walks() {
        assert!(JavascriptExtractor::has_export_modifier(";export"));
        assert!(!JavascriptExtractor::has_export_modifier("obj.export"));
        assert!(!JavascriptExtractor::has_export_modifier("notexport"));
        assert!(!JavascriptExtractor::has_export_modifier("éxport"));

        let source = "export /* public */ const fetchData = async () => 1;\n// export\nconst local = () => 2;\n";
        let result = TreeSitterExtractor::new(
            "src/app.js",
            source,
            Some(Language::Javascript),
            Some(&JavascriptExtractor),
        )
        .extract();

        let exported = result
            .nodes
            .iter()
            .find(|node| node.name == "fetchData")
            .unwrap();
        let local = result
            .nodes
            .iter()
            .find(|node| node.name == "local")
            .unwrap();
        assert_eq!(exported.is_exported, Some(true));
        assert_eq!(local.is_exported, Some(false));
    }

    #[test]
    fn deeply_nested_loop_declarations_do_not_walk_every_ancestor_for_export_status() {
        let mut source = String::from("let top = 0;\n");
        for _ in 0..2_000 {
            source.push_str("for (let i = 0; i < 1; i++) ");
        }
        source.push_str("let inner = 1;\n");

        let result = TreeSitterExtractor::new(
            "src/deep.js",
            &source,
            Some(Language::Javascript),
            Some(&JavascriptExtractor),
        )
        .extract();

        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        let inner = result.nodes.iter().find(|n| n.name == "inner").unwrap();
        assert_eq!(inner.kind, NodeKind::Variable);
        assert_eq!(inner.is_exported, Some(false));
    }
}
