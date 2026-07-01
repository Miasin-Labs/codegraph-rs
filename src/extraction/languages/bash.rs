//! Bash / shell-script extraction config.
//!
//! tree-sitter-bash shapes: `function_definition` (fields `name`: word,
//! `body`: compound_statement), `command` (field `name`: command_name),
//! `variable_assignment` (field `name`). There is no import node type —
//! `source lib.sh` / `. lib.sh` is just a `command`, so [`visit_node`]
//! claims top-level source commands and emits Import nodes (the Lua
//! `require` precedent). Inside function bodies the body walker extracts
//! `command` nodes as calls; path-like callees (`./scripts/deploy.sh`)
//! resolve to file nodes through the file-path tier, bare names resolve to
//! shell functions, and common shell builtins/coreutils are filtered by
//! `BASH_BUILT_INS` in the resolver.
//!
//! [`visit_node`]: LanguageExtractor::visit_node

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{
    ExtractorContext,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference};

pub struct BashExtractor;

/// `source x.sh` / `. x.sh` → the sourced path, with quotes stripped.
/// Returns `None` for any other command (or a source with no argument).
fn sourced_path(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    if node.kind() != "command" {
        return None;
    }
    let name = get_child_by_field(node, "name")?;
    let name_text = get_node_text(name, source);
    if name_text != "source" && name_text != "." {
        return None;
    }
    // First named child after the command_name is the sourced path.
    let arg = named_children(node)
        .into_iter()
        .find(|c| c.kind() != "command_name")?;
    let path = get_node_text(arg, source)
        .trim_matches(|c| c == '"' || c == '\'')
        .to_string();
    if path.is_empty() { None } else { Some(path) }
}

impl LanguageExtractor for BashExtractor {
    fn function_types(&self) -> &[&str] {
        &["function_definition"]
    }
    fn class_types(&self) -> &[&str] {
        &[]
    }
    fn method_types(&self) -> &[&str] {
        &[]
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
        &[]
    }
    fn call_types(&self) -> &[&str] {
        &["command"]
    }
    // `variable_assignment` is claimed by visit_node below — the core
    // extract_variable only knows JS/Python/Go/… declaration shapes.
    fn variable_types(&self) -> &[&str] {
        &[]
    }
    fn name_field(&self) -> &str {
        "name"
    }
    fn body_field(&self) -> &str {
        "body"
    }
    // Shell functions have no parameter list; "parameters" never matches.
    fn params_field(&self) -> &str {
        "parameters"
    }

    /// Two claims (top-level only — the body walker doesn't run this hook):
    ///
    /// `VAR=value` assignments → Variable/Constant nodes. The core
    /// `extract_variable` only knows JS/Python/Go/… declaration shapes, so
    /// bash handles its own. UPPER_CASE names extract as constants — the
    /// dominant shell convention for configuration values.
    ///
    /// `source x.sh` / `. x.sh` commands → Import nodes, so they don't
    /// surface as calls to a builtin. (Inside function bodies those emit a
    /// `source` call the resolver filters.)
    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if node.kind() == "variable_assignment" {
            let Some(name_node) = get_child_by_field(node, "name") else {
                return true;
            };
            let name = get_node_text(name_node, ctx.source()).to_string();
            let kind = if name.chars().all(|c| !c.is_ascii_lowercase()) {
                NodeKind::Constant
            } else {
                NodeKind::Variable
            };
            let signature: Option<String> = get_child_by_field(node, "value")
                .map(|v| get_node_text(v, ctx.source()).chars().take(100).collect());
            ctx.create_node(
                kind,
                &name,
                node,
                NodeExtra {
                    signature,
                    ..Default::default()
                },
            );
            return true;
        }

        let Some(path) = sourced_path(node, ctx.source()) else {
            return false;
        };

        let signature: String = get_node_text(node, ctx.source())
            .trim()
            .chars()
            .take(100)
            .collect();
        let imp = ctx.create_node(
            NodeKind::Import,
            &path,
            node,
            NodeExtra {
                signature: Some(signature),
                ..Default::default()
            },
        );
        if imp.is_some() {
            if let Some(parent_id) = ctx.node_stack().last().cloned() {
                ctx.add_unresolved_reference(UnresolvedReference {
                    from_node_id: parent_id,
                    reference_name: path,
                    reference_kind: EdgeKind::Imports,
                    line: node.start_position().row as u32 + 1,
                    column: node.start_position().column as u32,
                    file_path: None,
                    language: None,
                    candidates: None,
                    metadata: None,
                });
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn bash_smoke_extraction() {
        let source = "#!/usr/bin/env bash\nset -euo pipefail\n\nsource ./lib/common.sh\n\nBUILD_DIR=target\n\nbuild() {\n    cargo build --release\n}\n\ndeploy() {\n    build\n    ./scripts/upload.sh \"$BUILD_DIR\"\n}\n\ndeploy\n";
        let result = TreeSitterExtractor::new(
            "scripts/release.sh",
            source,
            Some(Language::Bash),
            Some(&BashExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let build = result.nodes.iter().find(|n| n.name == "build").unwrap();
        assert_eq!(build.kind, NodeKind::Function);
        let deploy = result.nodes.iter().find(|n| n.name == "deploy").unwrap();
        assert_eq!(deploy.kind, NodeKind::Function);

        // `source ./lib/common.sh` becomes an Import node + imports ref.
        let imp = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(imp.name, "./lib/common.sh");
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "./lib/common.sh"
                    && r.reference_kind == EdgeKind::Imports),
            "imports ref missing: {:?}",
            result.unresolved_references
        );

        // Function-to-function call and script-path call from deploy's body.
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "build"
                    && r.reference_kind == EdgeKind::Calls
                    && r.from_node_id == deploy.id),
            "function call missing"
        );
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "./scripts/upload.sh"
                    && r.reference_kind == EdgeKind::Calls),
            "script-path call missing: {:?}",
            result.unresolved_references
        );

        // Top-level `deploy` invocation is attributed to the file node.
        let file = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::File)
            .unwrap();
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "deploy" && r.from_node_id == file.id),
            "top-level call missing"
        );

        // Top-level variable assignment.
        let var = result.nodes.iter().find(|n| n.name == "BUILD_DIR").unwrap();
        assert!(matches!(var.kind, NodeKind::Variable | NodeKind::Constant));
    }

    #[test]
    fn bash_dot_source_is_import() {
        let source = ". \"$HOME/.profile\"\n. ./env.sh\n";
        let result = TreeSitterExtractor::new(
            "scripts/env-setup.sh",
            source,
            Some(Language::Bash),
            Some(&BashExtractor),
        )
        .extract();
        let imports: Vec<_> = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Import)
            .collect();
        assert_eq!(imports.len(), 2, "imports: {imports:?}");
        assert!(imports.iter().any(|n| n.name == "./env.sh"));
    }
}
