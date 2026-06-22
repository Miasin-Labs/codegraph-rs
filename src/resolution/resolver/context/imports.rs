use super::{ResolverContext, is_js_family_path};
use crate::resolution::import_resolver::{extract_import_mappings, extract_re_exports};
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
}
