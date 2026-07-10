use super::ReferenceResolver;
#[cfg(feature = "gpu")]
use crate::types::Language;
use crate::types::Node;

impl ReferenceResolver {
    /// Get detected frameworks
    pub fn get_detected_frameworks(&self) -> Vec<String> {
        self.frameworks
            .borrow()
            .iter()
            .map(|framework| framework.name().to_string())
            .collect()
    }

    pub(super) fn get_node_by_id(&self, node_id: &str) -> Option<Node> {
        self.context.queries.get_node_by_id(node_id).ok().flatten()
    }

    #[cfg(feature = "gpu")]
    pub(super) fn get_file_path_from_node_id(&self, node_id: &str) -> String {
        self.get_node_by_id(node_id)
            .map(|node| node.file_path)
            .unwrap_or_default()
    }

    #[cfg(feature = "gpu")]
    pub(super) fn get_language_from_node_id(&self, node_id: &str) -> Language {
        self.get_node_by_id(node_id)
            .map(|node| node.language)
            .unwrap_or(Language::Unknown)
    }
}
