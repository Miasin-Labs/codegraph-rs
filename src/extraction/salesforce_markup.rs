//! SalesforceMarkupExtractor — Visualforce pages/components and Aura bundles.
//!
//! Both are tag markup whose graph value is in two places:
//!
//! **Visualforce** (`.page`, `.component`): the `controller=` /
//! `extensions=` attributes name Apex classes, and `{!expr}` bindings
//! evaluate against that controller. We emit Apex-language references for
//! the classes, `Controller.member` references for simple `{!member}`
//! bindings (the method-call tier — or the Salesforce resolver's
//! getter-convention fallback, `{!account}` → `getAccount`) and
//! `Controller.action` calls for `action="{!action}"` attributes. Formula
//! expressions (`{!IF(...)}`, operators) fail the whole-brace regex and are
//! skipped — only plain property paths become references.
//!
//! **Aura** (`.cmp`, `.app`, `.evt`): `controller=` names the server-side
//! Apex controller (Apex reference), and `{!c.handler}` action bindings
//! name functions in the bundle's client controller
//! (`<bundle>Controller.js`) — emitted as Aura-language calls the
//! Salesforce resolver binds to the synthesized controller function nodes.
//! `{!v.attr}` value-provider reads stay local to the bundle and are
//! skipped.
//!
//! Everything hangs off a file node (MyBatis-extractor pattern).

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::types::{EdgeKind, ExtractionResult, Language, Node, NodeKind, UnresolvedReference};

/// `controller="X"` on the root VF/Aura tag.
static CONTROLLER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<(?:apex:page|apex:component|aura:component|aura:application)\b[^>]*?\bcontroller\s*=\s*"([^"]+)""#)
        .expect("valid regex")
});

/// `extensions="A, B"` (Visualforce only).
static EXTENSIONS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"<apex:(?:page|component)\b[^>]*?\bextensions\s*=\s*"([^"]+)""#)
        .expect("valid regex")
});

/// `action="{!save}"` — explicit method invocations (Visualforce).
static ACTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\baction\s*=\s*"\{!\s*([A-Za-z_]\w*)\s*\}""#).expect("valid regex")
});

/// `{!ident}` / `{!ident.path}` — simple VF binding expressions. Formulas
/// (parens, operators, `$Label.x` globals) fail the whole-brace match.
static VF_BINDING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{!\s*([A-Za-z_]\w*)(?:\.\w+)*\s*\}").expect("valid regex"));

/// `{!c.handler}` — Aura client-controller action bindings.
static AURA_ACTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{!\s*c\.(\w+)\s*\}").expect("valid regex"));

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch")
        .as_millis() as i64
}

pub struct SalesforceMarkupExtractor<'a> {
    file_path: String,
    source: &'a str,
    language: Language,
}

impl<'a> SalesforceMarkupExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str, language: Language) -> Self {
        SalesforceMarkupExtractor {
            file_path: file_path.into(),
            source,
            language,
        }
    }

    pub fn extract(self) -> ExtractionResult {
        let start_time = std::time::Instant::now();
        let mut result = ExtractionResult::default();

        let file_node_id = self.create_file_node(&mut result);
        match self.language {
            Language::Visualforce => self.extract_visualforce(&file_node_id, &mut result),
            Language::Aura => self.extract_aura(&file_node_id, &mut result),
            _ => {}
        }

        result.duration_ms = start_time.elapsed().as_millis() as f64;
        result
    }

    fn create_file_node(&self, result: &mut ExtractionResult) -> String {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let id = generate_node_id(&self.file_path, NodeKind::File, &self.file_path, 1);
        let name = self
            .file_path
            .split('/')
            .next_back()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.file_path)
            .to_string();

        let mut node = Node::new(
            id.clone(),
            NodeKind::File,
            name,
            self.file_path.clone(),
            self.file_path.clone(),
            self.language,
            1,
            lines.len().max(1) as u32,
        );
        node.end_column = lines.last().map(|l| l.len()).unwrap_or(0) as u32;
        node.start_byte = Some(0);
        node.end_byte = Some(self.source.len() as u32);
        node.updated_at = now_ms();
        result.nodes.push(node);
        id
    }

    fn line_of(&self, offset: usize) -> u32 {
        self.source[..offset]
            .bytes()
            .filter(|&b| b == b'\n')
            .count() as u32
            + 1
    }

    fn push_ref(
        &self,
        result: &mut ExtractionResult,
        from: &str,
        name: String,
        kind: EdgeKind,
        offset: usize,
        language: Language,
    ) {
        result.unresolved_references.push(UnresolvedReference {
            from_node_id: from.to_string(),
            reference_name: name,
            reference_kind: kind,
            line: self.line_of(offset),
            column: 0,
            file_path: None,
            language: Some(language),
            candidates: None,
            metadata: None,
        });
    }

    fn extract_visualforce(&self, file_node_id: &str, result: &mut ExtractionResult) {
        // Controller + extensions → Apex class references. Managed-package
        // namespaces (`ns.Controller`) keep the class simple name.
        let mut binding_targets = Vec::new();
        let controller = CONTROLLER_RE.captures(self.source).map(|c| {
            let m = c.get(1).expect("group 1");
            let simple = m.as_str().rsplit('.').next().unwrap_or(m.as_str()).trim();
            self.push_ref(
                result,
                file_node_id,
                simple.to_string(),
                EdgeKind::References,
                m.start(),
                Language::Apex,
            );
            simple.to_string()
        });
        if let Some(controller) = controller {
            binding_targets.push(controller);
        }
        if let Some(caps) = EXTENSIONS_RE.captures(self.source) {
            let m = caps.get(1).expect("group 1");
            for ext in m.as_str().split(',') {
                let simple = ext.trim().rsplit('.').next().unwrap_or("").trim();
                if !simple.is_empty() {
                    self.push_ref(
                        result,
                        file_node_id,
                        simple.to_string(),
                        EdgeKind::References,
                        m.start(),
                        Language::Apex,
                    );
                    binding_targets.push(simple.to_string());
                }
            }
        }

        // Bindings only mean something against custom Apex classes. A bare
        // `standardController="Account"` binds sObject fields, which have no
        // nodes here; extensions are Apex classes and should receive bindings.
        if binding_targets.is_empty() {
            return;
        }

        let mut seen: HashSet<(String, EdgeKind)> = HashSet::new();
        let mut action_names: HashSet<String> = HashSet::new();
        // `action="{!save}"` invocations first (calls)…
        for caps in ACTION_RE.captures_iter(self.source) {
            let m = caps.get(1).expect("group 1");
            action_names.insert(m.as_str().to_string());
            for target in &binding_targets {
                let name = format!("{}.{}", target, m.as_str());
                if seen.insert((name.clone(), EdgeKind::Calls)) {
                    self.push_ref(
                        result,
                        file_node_id,
                        name,
                        EdgeKind::Calls,
                        m.start(),
                        Language::Apex,
                    );
                }
            }
        }
        // …then plain `{!member}` property reads (references).
        for caps in VF_BINDING_RE.captures_iter(self.source) {
            let m = caps.get(1).expect("group 1");
            let ident = m.as_str();
            if action_names.contains(ident) {
                continue;
            }
            if matches!(
                ident.to_ascii_lowercase().as_str(),
                "true" | "false" | "null"
            ) {
                continue;
            }
            for target in &binding_targets {
                let name = format!("{}.{}", target, ident);
                if seen.insert((name.clone(), EdgeKind::References)) {
                    self.push_ref(
                        result,
                        file_node_id,
                        name,
                        EdgeKind::References,
                        m.start(),
                        Language::Apex,
                    );
                }
            }
        }
    }

    fn extract_aura(&self, file_node_id: &str, result: &mut ExtractionResult) {
        // Server-side Apex controller.
        if let Some(caps) = CONTROLLER_RE.captures(self.source) {
            let m = caps.get(1).expect("group 1");
            let simple = m.as_str().rsplit('.').next().unwrap_or(m.as_str()).trim();
            if !simple.is_empty() {
                self.push_ref(
                    result,
                    file_node_id,
                    simple.to_string(),
                    EdgeKind::References,
                    m.start(),
                    Language::Apex,
                );
            }
        }

        // `{!c.handler}` → client controller functions; the Salesforce
        // resolver binds these to `<bundle>Controller.js`.
        let mut seen: HashSet<&str> = HashSet::new();
        for caps in AURA_ACTION_RE.captures_iter(self.source) {
            let m = caps.get(1).expect("group 1");
            if seen.insert(m.as_str()) {
                self.push_ref(
                    result,
                    file_node_id,
                    m.as_str().to_string(),
                    EdgeKind::Calls,
                    m.start(),
                    Language::Aura,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visualforce_page_extracts_controller_and_bindings() {
        let source = "<apex:page standardStylesheets=\"false\" controller=\"OrionWrapperClassController\" extensions=\"PageExt, ns.OtherExt\">\n    <apex:outputText value=\"{!greeting}\"/>\n    <apex:commandButton action=\"{!save}\" value=\"Save\"/>\n    <apex:outputText value=\"{!account.Name}\"/>\n    <apex:outputText value=\"{!IF(isOpen, 'a', 'b')}\"/>\n    <apex:outputText value=\"{!$Label.welcome}\"/>\n</apex:page>\n";
        let result = SalesforceMarkupExtractor::new(
            "force-app/main/default/pages/Wrapper.page",
            source,
            Language::Visualforce,
        )
        .extract();

        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].kind, NodeKind::File);
        assert_eq!(result.nodes[0].language, Language::Visualforce);

        let refs: Vec<(&str, EdgeKind)> = result
            .unresolved_references
            .iter()
            .map(|r| (r.reference_name.as_str(), r.reference_kind))
            .collect();

        // Controller + both extensions (namespace stripped), all Apex refs.
        assert!(refs.contains(&("OrionWrapperClassController", EdgeKind::References)));
        assert!(refs.contains(&("PageExt", EdgeKind::References)));
        assert!(refs.contains(&("OtherExt", EdgeKind::References)));
        // action= is a call; value bindings are references against the
        // controller and both extensions.
        assert!(refs.contains(&("OrionWrapperClassController.save", EdgeKind::Calls)));
        assert!(refs.contains(&("PageExt.save", EdgeKind::Calls)));
        assert!(refs.contains(&("OtherExt.save", EdgeKind::Calls)));
        assert!(!refs.contains(&("OrionWrapperClassController.save", EdgeKind::References)));
        assert!(refs.contains(&("OrionWrapperClassController.greeting", EdgeKind::References)));
        assert!(refs.contains(&("PageExt.greeting", EdgeKind::References)));
        assert!(refs.contains(&("OtherExt.greeting", EdgeKind::References)));
        assert!(refs.contains(&("OrionWrapperClassController.account", EdgeKind::References)));
        // Formula + $global expressions are skipped.
        assert!(!refs.iter().any(|(n, _)| n.contains("IF")));
        assert!(!refs.iter().any(|(n, _)| n.contains("Label")));
        for r in &result.unresolved_references {
            assert_eq!(r.language, Some(Language::Apex));
        }
    }

    #[test]
    fn visualforce_without_custom_controller_emits_no_binding_refs() {
        let source = "<apex:page standardController=\"Account\">\n    <apex:outputText value=\"{!Account.Name}\"/>\n</apex:page>\n";
        let result =
            SalesforceMarkupExtractor::new("pages/A.page", source, Language::Visualforce).extract();
        assert!(
            result.unresolved_references.is_empty(),
            "standard-controller bindings must be skipped: {:?}",
            result.unresolved_references
        );
    }

    #[test]
    fn visualforce_standard_controller_extensions_receive_bindings() {
        let source = "<apex:page standardController=\"Account\" extensions=\"AccountExt\">\n    <apex:commandButton action=\"{!save}\" value=\"Save\"/>\n    <apex:outputText value=\"{!status}\"/>\n</apex:page>\n";
        let result =
            SalesforceMarkupExtractor::new("pages/A.page", source, Language::Visualforce).extract();

        let refs: Vec<(&str, EdgeKind)> = result
            .unresolved_references
            .iter()
            .map(|r| (r.reference_name.as_str(), r.reference_kind))
            .collect();

        assert!(refs.contains(&("AccountExt", EdgeKind::References)));
        assert!(refs.contains(&("AccountExt.save", EdgeKind::Calls)));
        assert!(refs.contains(&("AccountExt.status", EdgeKind::References)));
        assert!(!refs.iter().any(|(name, _)| name.starts_with("Account.")));
    }

    #[test]
    fn aura_component_extracts_controller_and_actions() {
        let source = "<aura:component controller=\"AccountController\" implements=\"force:appHostable\">\n    <aura:attribute name=\"items\" type=\"List\"/>\n    <aura:handler name=\"init\" value=\"{!this}\" action=\"{!c.doInit}\"/>\n    <lightning:button onclick=\"{!c.handleClick}\" label=\"{!v.label}\"/>\n</aura:component>\n";
        let result = SalesforceMarkupExtractor::new(
            "force-app/main/default/aura/accountList/accountList.cmp",
            source,
            Language::Aura,
        )
        .extract();

        let refs: Vec<(&str, EdgeKind, Option<Language>)> = result
            .unresolved_references
            .iter()
            .map(|r| (r.reference_name.as_str(), r.reference_kind, r.language))
            .collect();

        assert!(refs.contains(&(
            "AccountController",
            EdgeKind::References,
            Some(Language::Apex)
        )));
        assert!(refs.contains(&("doInit", EdgeKind::Calls, Some(Language::Aura))));
        assert!(refs.contains(&("handleClick", EdgeKind::Calls, Some(Language::Aura))));
        // `{!v.label}` / `{!this}` value providers are not c. actions.
        assert!(!refs.iter().any(|(n, _, _)| *n == "label" || *n == "this"));
    }
}
