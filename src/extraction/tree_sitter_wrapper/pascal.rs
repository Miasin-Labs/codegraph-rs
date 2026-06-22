use super::context::*;
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::*;
use crate::extraction::tree_sitter_types::*;
use crate::types::*;

impl<'a> TreeSitterExtractor<'a> {
    /// Handle Pascal-specific AST structures.
    /// Returns true if the node was fully handled and children should be skipped.
    pub(super) fn visit_pascal_node(&mut self, node: SyntaxNode<'_>) -> bool {
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
}
