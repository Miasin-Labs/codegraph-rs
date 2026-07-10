//! CICS pseudo-conversational transaction resolver for COBOL.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use regex::Regex;

use crate::resolution::types::{
    FrameworkResolver,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{Language, Node, NodeKind};

const TRANSID_REF_PREFIX: &str = "cics-transid:";
static TRANID_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("(?i)TRAN").expect("valid CICS data-name regex"));
static VALUE_LITERAL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\bVALUE\s+['"]([A-Za-z0-9$#@]{1,4})['"]"#).expect("valid CICS VALUE regex")
});

#[derive(Debug, Default)]
pub struct CicsResolver {
    transid_index: Mutex<Option<(String, HashMap<String, String>)>>,
}

impl CicsResolver {
    pub fn new() -> Self {
        Self::default()
    }

    fn target_for(&self, transaction: &str, context: &dyn ResolutionContext) -> Option<String> {
        let project_root = context.get_project_root().to_string();
        let mut cached = self.transid_index.lock().ok()?;
        if cached
            .as_ref()
            .is_none_or(|(cached_root, _)| cached_root != &project_root)
        {
            *cached = Some((project_root, build_index(context)));
        }
        cached
            .as_ref()
            .and_then(|(_, index)| index.get(transaction).cloned())
    }
}

impl FrameworkResolver for CicsResolver {
    fn name(&self) -> &str {
        "cics"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Cobol])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        context
            .get_nodes_by_kind(NodeKind::Module)
            .iter()
            .any(|node| node.language == Language::Cobol)
    }

    fn claims_reference(&self, name: &str) -> bool {
        name.starts_with(TRANSID_REF_PREFIX)
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let transaction = reference
            .reference_name
            .strip_prefix(TRANSID_REF_PREFIX)?
            .to_uppercase();
        let target_node_id = self.target_for(&transaction, context)?;
        Some(ResolvedRef {
            original: reference.clone(),
            target_node_id,
            confidence: 0.85,
            resolved_by: ResolvedBy::Framework,
        })
    }
}

fn build_index(context: &dyn ResolutionContext) -> HashMap<String, String> {
    let mut index = HashMap::new();
    for kind in [NodeKind::Variable, NodeKind::Field, NodeKind::Constant] {
        for node in context.get_nodes_by_kind(kind) {
            let Some(transaction) = transid_from_data_node(&node) else {
                continue;
            };
            if index.contains_key(&transaction) {
                continue;
            }
            if let Some(module) =
                context
                    .get_nodes_in_file(&node.file_path)
                    .into_iter()
                    .find(|candidate| {
                        candidate.kind == NodeKind::Module && candidate.language == Language::Cobol
                    })
            {
                index.insert(transaction, module.id);
            }
        }
    }
    index
}

fn transid_from_data_node(node: &Node) -> Option<String> {
    if node.language != Language::Cobol || !TRANID_NAME_RE.is_match(&node.name) {
        return None;
    }
    VALUE_LITERAL_RE
        .captures(node.signature.as_deref()?)
        .and_then(|captures| captures.get(1))
        .map(|capture| capture.as_str().to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_transaction_id_from_conventional_data_item() {
        let mut node = Node::new(
            "data",
            NodeKind::Variable,
            "WS-TRANID",
            "WS-TRANID",
            "program.cbl",
            Language::Cobol,
            10,
            10,
        );
        node.signature = Some("05 WS-TRANID PIC X(04) VALUE 'cb00'.".to_string());
        assert_eq!(transid_from_data_node(&node).as_deref(), Some("CB00"));

        node.name = "WS-USER-ID".to_string();
        assert_eq!(transid_from_data_node(&node), None);
    }

    #[test]
    fn claims_only_extractor_emitted_cics_references() {
        let resolver = CicsResolver::new();
        assert!(resolver.claims_reference("cics-transid:CB00"));
        assert!(!resolver.claims_reference("CB00"));
    }
}
