use std::sync::Arc;

use super::{ResolverContext, is_js_family_path};
use crate::resolution::import_resolver::{extract_import_mappings, extract_re_exports};
use crate::resolution::name_matcher::{RustUse, rust_use_leaves};
use crate::resolution::types::{ImportMapping, ReExport};
use crate::types::Language;

impl ResolverContext {
    pub(super) fn cached_import_mappings(
        &self,
        file_path: &str,
        language: Language,
    ) -> Vec<ImportMapping> {
        let cache_key = file_path.to_string();
        if let Some(cached) = self.import_mapping_cache.borrow_mut().get(&cache_key) {
            return cached.clone();
        }
        let content = match self.cached_file_text(file_path) {
            Some(content) if !content.is_empty() => content,
            _ => {
                self.import_mapping_cache
                    .borrow_mut()
                    .set(cache_key, Vec::new());
                return Vec::new();
            }
        };
        let mappings = extract_import_mappings(file_path, &content, language);
        self.import_mapping_cache
            .borrow_mut()
            .set(cache_key, mappings.clone());
        mappings
    }

    pub(super) fn cached_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        let key = file_path.to_string();
        if let Some(cached) = self.re_export_cache.borrow_mut().get(&key) {
            return cached.clone();
        }
        let content = match self.cached_file_text(file_path) {
            Some(content) if !content.is_empty() => content,
            _ => {
                self.re_export_cache.borrow_mut().set(key, Vec::new());
                return Vec::new();
            }
        };
        let parse_language = if is_js_family_path(file_path) {
            Language::Typescript
        } else {
            language
        };
        let re_exports = extract_re_exports(&content, parse_language);
        self.re_export_cache
            .borrow_mut()
            .set(key, re_exports.clone());
        re_exports
    }

    /// Rust use leaves per file, parsed once from the file's Import nodes.
    pub(super) fn cached_rust_use_leaves(&self, file_path: &str) -> Arc<[RustUse]> {
        let key = file_path.to_string();
        if let Some(cached) = self.rust_use_cache.borrow_mut().get(&key) {
            return Arc::clone(cached);
        }
        let leaves: Arc<[RustUse]> = rust_use_leaves(&self.cached_nodes_in_file(file_path)).into();
        self.rust_use_cache
            .borrow_mut()
            .set(key, Arc::clone(&leaves));
        leaves
    }
}
