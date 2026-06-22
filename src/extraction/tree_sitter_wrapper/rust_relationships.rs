use super::context::*;
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::*;
use crate::extraction::tree_sitter_types::*;
use crate::types::*;

impl<'a> TreeSitterExtractor<'a> {
    /// Rust `impl Trait for Type` — creates an implements edge from Type to Trait.
    /// For plain `impl Type { ... }` (no trait), no inheritance edge is needed.
    /// Emit `Implements` edges for a Rust `#[derive(Trait, …)]` attribute on a
    /// struct/enum. Derive macros generate real trait impls (`Clone`,
    /// `PartialEq`, `serde::Serialize`, thiserror's `Error`→`Display`), which
    /// the literal-`impl` path never sees — so "what implements Serialize?"
    /// otherwise misses every derive site (~1k in a typical Rust crate).
    pub(super) fn extract_rust_derives(&mut self, decl_node: SyntaxNode<'_>, type_id: &str) {
        if self.language != Language::Rust {
            return;
        }
        // `#[derive(...)]` parses as `attribute_item` PRECEDING siblings of the
        // declaration. Walk backwards over the contiguous attribute run.
        let Some(parent) = decl_node.parent() else {
            return;
        };
        let decl_start = decl_node.start_byte();
        let siblings = named_children(parent);
        let Some(idx) = siblings.iter().position(|s| s.start_byte() == decl_start) else {
            return;
        };
        for j in (0..idx).rev() {
            let sib = siblings[j];
            if sib.kind() != "attribute_item" {
                break; // a non-attribute separator ends this declaration's run
            }
            self.collect_rust_derives(sib, type_id);
        }
    }

    /// Pull trait names out of one `#[derive(...)]` `attribute_item` and emit an
    /// `Implements` ref per trait. For path traits (`serde::Serialize`) the
    /// last path segment is used (`Serialize`).
    pub(super) fn collect_rust_derives(&mut self, attr_item: SyntaxNode<'_>, type_id: &str) {
        let Some(attribute) = find_named_child(attr_item, "attribute") else {
            return;
        };
        // The attribute name (first child identifier) must be `derive`.
        let is_derive = attribute
            .named_child(0)
            .map(|c| get_node_text(c, self.source) == "derive")
            .unwrap_or(false);
        if !is_derive {
            return;
        }
        let Some(token_tree) = find_named_child(attribute, "token_tree") else {
            return;
        };
        // token_tree children are raw tokens: `(`, identifiers, `::`, `,`, `)`.
        // Group by `,`; each group's last identifier is the trait name.
        let mut last_ident: Option<SyntaxNode<'_>> = None;
        for i in 0..token_tree.child_count() as u32 {
            let Some(tok) = token_tree.child(i) else {
                continue;
            };
            match tok.kind() {
                "identifier" | "type_identifier" => last_ident = Some(tok),
                "," => {
                    if let Some(id) = last_ident.take() {
                        let name = get_node_text(id, self.source).to_string();
                        self.push_ref(type_id, name, EdgeKind::Implements, id);
                    }
                }
                _ => {}
            }
        }
        if let Some(id) = last_ident {
            let name = get_node_text(id, self.source).to_string();
            self.push_ref(type_id, name, EdgeKind::Implements, id);
        }
    }

    pub(super) fn extract_rust_impl_item(&mut self, node: SyntaxNode<'_>) {
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

    /// The node id of an `impl` block's implementing (Self) type, if it was
    /// already extracted. For `impl Foo`, `impl Trait for Foo`, and the generic
    /// forms (`impl<T> Foo<T>`, `impl<T> Trait for Foo<T>`) the Self type is the
    /// LAST type child (the first, when present before `for`, is the trait).
    /// Used to scope an impl body so associated items get a `Foo::member` name.
    pub(super) fn rust_impl_self_type_id(&self, node: SyntaxNode<'_>) -> Option<String> {
        let type_children: Vec<SyntaxNode<'_>> = named_children(node)
            .into_iter()
            .filter(|c| {
                matches!(
                    c.kind(),
                    "type_identifier" | "generic_type" | "scoped_type_identifier"
                )
            })
            .collect();
        let self_type = type_children.last()?;
        let type_name = if self_type.kind() == "generic_type" {
            match find_named_child(*self_type, "type_identifier") {
                Some(inner) => get_node_text(inner, self.source).to_string(),
                None => get_node_text(*self_type, self.source).to_string(),
            }
        } else {
            // scoped_type_identifier (`module::Foo`) → take the last segment.
            get_node_text(*self_type, self.source)
                .rsplit("::")
                .next()
                .unwrap_or_default()
                .to_string()
        };
        self.find_node_by_name(&type_name)
    }

    /// Find a previously-extracted node by name (used for back-references like impl blocks)
    pub(super) fn find_node_by_name(&self, name: &str) -> Option<String> {
        self.nodes
            .iter()
            .find(|n| {
                n.name == name
                    && matches!(n.kind, NodeKind::Struct | NodeKind::Enum | NodeKind::Class)
            })
            .map(|n| n.id.clone())
    }
}
