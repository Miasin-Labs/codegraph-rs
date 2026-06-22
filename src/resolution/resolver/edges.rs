use super::ReferenceResolver;
use crate::resolution::types::ResolvedRef;
use crate::types::{Edge, EdgeKind, Metadata, NodeKind};

impl ReferenceResolver {
    /// Create edges from resolved references
    pub fn create_edges(&self, resolved: &[ResolvedRef]) -> Vec<Edge> {
        resolved
            .iter()
            .map(|resolved_ref| {
                let mut kind = resolved_ref.original.reference_kind;
                if kind == EdgeKind::Extends {
                    if let Some(target_node) = self.get_node_by_id(&resolved_ref.target_node_id) {
                        if target_node.kind == NodeKind::Interface
                            || target_node.kind == NodeKind::Protocol
                        {
                            if let Some(source_node) =
                                self.get_node_by_id(&resolved_ref.original.from_node_id)
                            {
                                if source_node.kind != NodeKind::Interface
                                    && source_node.kind != NodeKind::Protocol
                                {
                                    kind = EdgeKind::Implements;
                                }
                            }
                        }
                    }
                }
                if kind == EdgeKind::Calls {
                    if let Some(target_node) = self.get_node_by_id(&resolved_ref.target_node_id) {
                        if target_node.kind == NodeKind::Class
                            || target_node.kind == NodeKind::Struct
                        {
                            kind = EdgeKind::Instantiates;
                        }
                    }
                }

                let mut metadata = Metadata::new();
                metadata.insert(
                    "confidence".to_string(),
                    serde_json::json!(resolved_ref.confidence),
                );
                metadata.insert(
                    "resolvedBy".to_string(),
                    serde_json::Value::String(resolved_ref.resolved_by.as_str().to_string()),
                );

                Edge {
                    source: resolved_ref.original.from_node_id.clone(),
                    target: resolved_ref.target_node_id.clone(),
                    kind,
                    line: Some(resolved_ref.original.line),
                    column: Some(resolved_ref.original.column),
                    metadata: Some(metadata),
                    provenance: None,
                }
            })
            .collect()
    }
}
