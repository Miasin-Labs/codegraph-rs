use super::ResolverContext;
use crate::error::log_warn;
use crate::types::{Node, NodeKind};

impl ResolverContext {
    pub(super) fn cached_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        let key = file_path.to_string();
        let has = self.node_cache.borrow().has(&key);
        if !has {
            let nodes = self
                .queries
                .get_nodes_by_file(file_path)
                .unwrap_or_else(|error| {
                    log_warn(
                        "Failed to load nodes for file during resolution",
                        Some(&serde_json::json!({
                            "filePath": file_path,
                            "error": error.to_string()
                        })),
                    );
                    Vec::new()
                });
            self.node_cache.borrow_mut().set(key.clone(), nodes);
        }
        self.node_cache
            .borrow_mut()
            .get(&key)
            .cloned()
            .unwrap_or_default()
    }

    pub(super) fn cached_nodes_by_name(&self, name: &str) -> Vec<Node> {
        let key = name.to_string();
        if let Some(cached) = self.name_cache.borrow_mut().get(&key) {
            return cached.clone();
        }
        let result = self
            .queries
            .get_nodes_by_name(name)
            .unwrap_or_else(|error| {
                log_warn(
                    "Failed to load nodes by name during resolution",
                    Some(&serde_json::json!({ "name": name, "error": error.to_string() })),
                );
                Vec::new()
            });
        self.name_cache.borrow_mut().set(key, result.clone());
        result
    }

    pub(super) fn cached_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        let key = qualified_name.to_string();
        if let Some(cached) = self.qualified_name_cache.borrow_mut().get(&key) {
            return cached.clone();
        }
        let result = self
            .queries
            .get_nodes_by_qualified_name_exact(qualified_name)
            .unwrap_or_else(|error| {
                log_warn(
                    "Failed to load nodes by qualified name during resolution",
                    Some(&serde_json::json!({
                        "qualifiedName": qualified_name,
                        "error": error.to_string()
                    })),
                );
                Vec::new()
            });
        self.qualified_name_cache
            .borrow_mut()
            .set(key, result.clone());
        result
    }

    pub(super) fn nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.queries
            .get_nodes_by_kind(kind)
            .unwrap_or_else(|error| {
                log_warn(
                    "Failed to load nodes by kind during resolution",
                    Some(&serde_json::json!({ "kind": kind.as_str(), "error": error.to_string() })),
                );
                Vec::new()
            })
    }

    pub(super) fn cached_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        let key = lower_name.to_string();
        if let Some(cached) = self.lower_name_cache.borrow_mut().get(&key) {
            return cached.clone();
        }
        let result = self
            .queries
            .get_nodes_by_lower_name(lower_name)
            .unwrap_or_else(|error| {
                log_warn(
                    "Failed to load nodes by lower name during resolution",
                    Some(&serde_json::json!({
                        "lowerName": lower_name,
                        "error": error.to_string()
                    })),
                );
                Vec::new()
            });
        self.lower_name_cache.borrow_mut().set(key, result.clone());
        result
    }
}
