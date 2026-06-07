//! Apex (Salesforce) language extraction config.
//!
//! The sfapex grammar mirrors tree-sitter-java's node shapes
//! (`class_declaration`, `method_declaration`, `method_invocation` with
//! `object`+`name` fields, `superclass`/`interfaces` wrapping `type_list`,
//! `field_declaration` → `variable_declarator`, …), so this config matches
//! `java.rs` except where the languages genuinely differ:
//!
//! - No imports or packages — Apex has a flat, case-insensitive global
//!   namespace (the resolver case-folds Apex lookups to match).
//! - `trigger_declaration` (`trigger Foo on Account (before insert) {…}`)
//!   has no Java analogue; [`visit_node`] extracts it as a function-like
//!   node plus a `references` edge to the sObject it fires on.
//! - `global` is a visibility modifier above `public`; both mark the
//!   symbol exported.
//! - Modifier keywords are case-insensitive (`PUBLIC STATIC` is legal),
//!   so modifier checks fold case before matching.
//!
//! [`visit_node`]: LanguageExtractor::visit_node

use super::named_children;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::{ExtractorContext, LanguageExtractor, SyntaxNode};
use crate::types::{EdgeKind, NodeKind, UnresolvedReference, Visibility};

pub struct ApexExtractor;

/// Lowercased text of the declaration's `modifiers` child, if any.
fn modifiers_text(node: SyntaxNode<'_>, source: &str) -> Option<String> {
    for i in 0..node.child_count() as u32 {
        if let Some(child) = node.child(i) {
            if child.kind() == "modifiers" {
                return Some(get_node_text(child, source).to_lowercase());
            }
        }
    }
    None
}

impl LanguageExtractor for ApexExtractor {
    fn function_types(&self) -> &[&str] {
        &[]
    }
    fn class_types(&self) -> &[&str] {
        &["class_declaration"]
    }
    fn method_types(&self) -> &[&str] {
        &["method_declaration", "constructor_declaration"]
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
        &["enum_constant"]
    }
    fn type_alias_types(&self) -> &[&str] {
        &[]
    }
    fn import_types(&self) -> &[&str] {
        &[]
    }
    fn call_types(&self) -> &[&str] {
        &["method_invocation"]
    }
    fn variable_types(&self) -> &[&str] {
        &["local_variable_declaration"]
    }
    fn field_types(&self) -> &[&str] {
        &["field_declaration"]
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
        Some("type")
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let return_type = get_child_by_field(node, "type");
        let params_text = get_node_text(params, source);
        Some(match return_type {
            Some(rt) => format!("{} {}", get_node_text(rt, source), params_text),
            None => params_text.to_string(),
        })
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        let text = modifiers_text(node, source)?;
        // `global` and `webservice` expose the symbol beyond the package —
        // the closest Visibility is Public. Check before `public` so a
        // (illegal but parseable) combination still lands on Public.
        if text.contains("global") || text.contains("webservice") || text.contains("public") {
            return Some(Visibility::Public);
        }
        if text.contains("protected") {
            return Some(Visibility::Protected);
        }
        if text.contains("private") {
            return Some(Visibility::Private);
        }
        None
    }

    fn is_exported(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        Some(matches!(
            self.get_visibility(node, source),
            Some(Visibility::Public)
        ))
    }

    fn is_static(&self, node: SyntaxNode<'_>, source: &str) -> Option<bool> {
        Some(modifiers_text(node, source).is_some_and(|t| t.contains("static")))
    }

    /// Extract `trigger Foo on Account (before insert, after update) {…}` —
    /// a function-like node (triggers are anonymous executable bodies with a
    /// name), a `references` edge to the sObject the trigger fires on, and
    /// the body visited for calls so `trigger → handler class` edges resolve.
    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if node.kind() != "trigger_declaration" {
            return false;
        }

        let Some(name_node) = get_child_by_field(node, "name") else {
            return true;
        };
        let name = get_node_text(name_node, ctx.source()).to_string();

        let object_node = get_child_by_field(node, "object");
        let object_text = object_node.map(|o| get_node_text(o, ctx.source()).to_string());
        let events: Vec<&str> = named_children(node)
            .into_iter()
            .filter(|c| c.kind() == "trigger_event")
            .map(|c| get_node_text(c, ctx.source()))
            .collect();
        let signature = object_text
            .as_ref()
            .map(|obj| format!("on {} ({})", obj, events.join(", ")));

        let trigger_node = ctx.create_node(
            NodeKind::Function,
            &name,
            node,
            crate::extraction::tree_sitter_types::NodeExtra {
                signature,
                ..Default::default()
            },
        );
        let Some(trigger_node) = trigger_node else {
            return true;
        };

        // The sObject the trigger fires on. Standard objects (Account, …)
        // have no node and stay unresolved; custom handler patterns where a
        // class shares the object's name resolve normally.
        if let (Some(object_node), Some(object_text)) = (object_node, object_text) {
            ctx.add_unresolved_reference(UnresolvedReference {
                from_node_id: trigger_node.id.clone(),
                reference_name: object_text,
                reference_kind: EdgeKind::References,
                line: object_node.start_position().row as u32 + 1,
                column: object_node.start_position().column as u32,
                file_path: None,
                language: None,
                candidates: None,
            });
        }

        if let Some(body) = get_child_by_field(node, "body") {
            ctx.push_scope(trigger_node.id.clone());
            ctx.visit_function_body(body, &trigger_node.id);
            ctx.pop_scope();
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
    fn apex_smoke_extraction() {
        let source = "public with sharing class AccountService {\n    private static Integer count;\n    public Integer total { get; set; }\n\n    @AuraEnabled(cacheable=true)\n    public static List<Account> getAccounts(Integer max) {\n        validate(max);\n        return [SELECT Id FROM Account LIMIT :max];\n    }\n\n    global void deposit(Decimal amount) {\n        AccountHelper.applyRules(amount);\n    }\n}\n\npublic interface Validator {\n    Boolean validate(Integer x);\n}\n\npublic enum Status { OPEN, CLOSED }\n";
        let result = TreeSitterExtractor::new(
            "force-app/main/default/classes/AccountService.cls",
            source,
            Some(Language::Apex),
            Some(&ApexExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let class = result
            .nodes
            .iter()
            .find(|n| n.name == "AccountService")
            .unwrap();
        assert_eq!(class.kind, NodeKind::Class);
        assert_eq!(class.visibility, Some(Visibility::Public));
        assert_eq!(class.is_exported, Some(true));

        let method = result
            .nodes
            .iter()
            .find(|n| n.name == "getAccounts")
            .unwrap();
        assert_eq!(method.kind, NodeKind::Method);
        assert_eq!(method.is_static, Some(true));
        assert_eq!(
            method.signature.as_deref(),
            Some("List<Account> (Integer max)")
        );

        let deposit = result.nodes.iter().find(|n| n.name == "deposit").unwrap();
        assert_eq!(deposit.visibility, Some(Visibility::Public)); // global → Public

        let field = result.nodes.iter().find(|n| n.name == "count").unwrap();
        assert_eq!(field.kind, NodeKind::Field);
        assert_eq!(field.is_static, Some(true));

        // Apex property (`{ get; set; }`) extracts through the field path.
        let prop = result.nodes.iter().find(|n| n.name == "total").unwrap();
        assert_eq!(prop.kind, NodeKind::Field);

        let iface = result.nodes.iter().find(|n| n.name == "Validator").unwrap();
        assert_eq!(iface.kind, NodeKind::Interface);

        let status = result.nodes.iter().find(|n| n.name == "Status").unwrap();
        assert_eq!(status.kind, NodeKind::Enum);
        let open = result.nodes.iter().find(|n| n.name == "OPEN").unwrap();
        assert_eq!(open.kind, NodeKind::EnumMember);

        // Calls inside method bodies become unresolved call references.
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "validate" && r.reference_kind == EdgeKind::Calls),
            "bare call missing: {:?}",
            result.unresolved_references
        );
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "AccountHelper.applyRules"
                    && r.reference_kind == EdgeKind::Calls),
            "receiver call missing"
        );
    }

    #[test]
    fn apex_inheritance_extraction() {
        let source = "public class TriggerHandler extends BaseHandler implements Queueable, Schedulable {\n}\n";
        let result = TreeSitterExtractor::new(
            "force-app/main/default/classes/TriggerHandler.cls",
            source,
            Some(Language::Apex),
            Some(&ApexExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "BaseHandler"
                    && r.reference_kind == EdgeKind::Extends),
            "extends ref missing: {:?}",
            result.unresolved_references
        );
        for iface in ["Queueable", "Schedulable"] {
            assert!(
                result
                    .unresolved_references
                    .iter()
                    .any(|r| r.reference_name == iface && r.reference_kind == EdgeKind::Implements),
                "implements ref missing for {iface}"
            );
        }
    }

    #[test]
    fn apex_trigger_extraction() {
        let source = "trigger AccountTrigger on Account (before insert, after update) {\n    AccountTriggerHandler.run(Trigger.new);\n}\n";
        let result = TreeSitterExtractor::new(
            "force-app/main/default/triggers/AccountTrigger.trigger",
            source,
            Some(Language::Apex),
            Some(&ApexExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let trigger = result
            .nodes
            .iter()
            .find(|n| n.name == "AccountTrigger")
            .expect("trigger node");
        assert_eq!(trigger.kind, NodeKind::Function);
        assert_eq!(
            trigger.signature.as_deref(),
            Some("on Account (before insert, after update)")
        );

        // sObject reference + the handler call from the body.
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "Account"
                    && r.reference_kind == EdgeKind::References
                    && r.from_node_id == trigger.id),
            "sObject ref missing: {:?}",
            result.unresolved_references
        );
        assert!(
            result
                .unresolved_references
                .iter()
                .any(|r| r.reference_name == "AccountTriggerHandler.run"
                    && r.reference_kind == EdgeKind::Calls
                    && r.from_node_id == trigger.id),
            "handler call missing: {:?}",
            result.unresolved_references
        );
    }

    #[test]
    fn apex_case_insensitive_modifiers() {
        // Apex keywords are case-insensitive; `PUBLIC STATIC` must still parse
        // and extract with the right visibility/static flags.
        let source = "PUBLIC CLASS Util {\n    PUBLIC STATIC void log() {}\n}\n";
        let result = TreeSitterExtractor::new(
            "force-app/main/default/classes/Util.cls",
            source,
            Some(Language::Apex),
            Some(&ApexExtractor),
        )
        .extract();
        let method = result.nodes.iter().find(|n| n.name == "log").unwrap();
        assert_eq!(method.visibility, Some(Visibility::Public));
        assert_eq!(method.is_static, Some(true));
    }
}
