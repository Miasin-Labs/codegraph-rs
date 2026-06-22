use std::fs;
use std::path::{Path, PathBuf};

use super::{ImportCacheKey, SnapshotContext};
use crate::error::log_debug;
use crate::resolution::import_resolver::{extract_import_mappings, extract_re_exports};
use crate::resolution::resolver::context::is_js_family_path;
use crate::resolution::types::{
    AliasMap,
    GoModule,
    ImportMapping,
    ReExport,
    ResolutionContext,
    WorkspacePackages,
};
use crate::types::{Language, Node, NodeKind};

impl ResolutionContext for SnapshotContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.nodes_by_file
            .get(file_path)
            .cloned()
            .unwrap_or_default()
    }

    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.nodes_by_name.get(name).cloned().unwrap_or_default()
    }

    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.nodes_by_qualified_name
            .get(qualified_name)
            .cloned()
            .unwrap_or_default()
    }

    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.nodes_by_kind.get(&kind).cloned().unwrap_or_default()
    }

    fn file_exists(&self, file_path: &str) -> bool {
        let normalized = file_path.replace('\\', "/");
        self.known_files.contains(file_path)
            || self.known_files.contains(&normalized)
            || Path::new(&self.project_root).join(file_path).exists()
    }

    fn read_file(&self, file_path: &str) -> Option<String> {
        {
            let cache = self
                .file_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.get(file_path) {
                return cached.clone();
            }
        }
        let full_path = Path::new(&self.project_root).join(file_path);
        let content = match fs::read(&full_path) {
            Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
            Err(error) => {
                log_debug(
                    "Failed to read file for snapshot resolution",
                    Some(&serde_json::json!({
                        "filePath": file_path,
                        "error": error.to_string()
                    })),
                );
                None
            }
        };
        self.file_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(file_path.to_string(), content.clone());
        content
    }

    fn get_project_root(&self) -> &str {
        &self.project_root
    }

    fn get_all_files(&self) -> Vec<String> {
        self.all_files.clone()
    }

    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.nodes_by_lower_name
            .get(lower_name)
            .cloned()
            .unwrap_or_default()
    }

    fn get_import_mappings(&self, file_path: &str, language: Language) -> Vec<ImportMapping> {
        let key: ImportCacheKey = (file_path.to_string(), language);
        {
            let cache = self
                .import_mapping_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }
        let mappings = match self.read_file(file_path) {
            Some(content) if !content.is_empty() => {
                extract_import_mappings(file_path, &content, language)
            }
            _ => Vec::new(),
        };
        self.import_mapping_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, mappings.clone());
        mappings
    }

    fn get_project_aliases(&self) -> Option<&AliasMap> {
        self.project_aliases.as_ref()
    }

    fn get_go_module(&self) -> Option<&GoModule> {
        self.go_module.as_ref()
    }

    fn get_workspace_packages(&self) -> Option<&WorkspacePackages> {
        self.workspace_packages.as_ref()
    }

    fn get_re_exports(&self, file_path: &str, language: Language) -> Vec<ReExport> {
        let parse_language = if is_js_family_path(file_path) {
            Language::Typescript
        } else {
            language
        };
        let key: ImportCacheKey = (file_path.to_string(), parse_language);
        {
            let cache = self
                .re_export_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(cached) = cache.get(&key) {
                return cached.clone();
            }
        }
        let re_exports = match self.read_file(file_path) {
            Some(content) if !content.is_empty() => extract_re_exports(&content, parse_language),
            _ => Vec::new(),
        };
        self.re_export_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, re_exports.clone());
        re_exports
    }

    fn list_directories(&self, relative_path: &str) -> Vec<String> {
        let target: PathBuf = if relative_path == "." || relative_path.is_empty() {
            PathBuf::from(&self.project_root)
        } else {
            Path::new(&self.project_root).join(relative_path)
        };
        match fs::read_dir(&target) {
            Ok(entries) => entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .file_type()
                        .map(|file_type| file_type.is_dir())
                        .unwrap_or(false)
                })
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(error) => {
                log_debug(
                    "Failed to list directory for snapshot resolution",
                    Some(&serde_json::json!({
                        "relativePath": relative_path,
                        "error": error.to_string()
                    })),
                );
                Vec::new()
            }
        }
    }

    fn get_cpp_include_dirs(&self) -> Vec<String> {
        self.cpp_include_dirs.clone()
    }
}
