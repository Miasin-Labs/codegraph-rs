use super::ReferenceResolver;
use crate::error::log_debug;

impl ReferenceResolver {
    /// Run each framework resolver's cross-file finalization pass and persist
    /// the returned node updates. Idempotent — safe to call after every indexAll
    /// and every incremental sync. Returns the number of nodes updated.
    ///
    /// Caches are cleared before/after so the post-extract pass sees fresh DB
    /// state and downstream queries see the updated names.
    pub fn run_post_extract(&self) -> usize {
        let mut updated = 0usize;
        self.clear_caches();
        for fw in &self.frameworks {
            let Some(nodes) = fw.post_extract(&self.context) else {
                continue;
            };
            for node in &nodes {
                match self.context.queries.update_node(node) {
                    Ok(()) => updated += 1,
                    Err(err) => {
                        log_debug(
                            &format!("Framework '{}' postExtract failed", fw.name()),
                            Some(&serde_json::json!({ "error": err.to_string() })),
                        );
                        break;
                    }
                }
            }
        }
        if updated > 0 {
            self.clear_caches();
        }
        updated
    }
}
