use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::ResolverContext;
use crate::error::{log_debug, log_warn};

impl ResolverContext {
    pub(super) fn indexed_or_disk_file_exists(&self, file_path: &str) -> bool {
        if let Some(known) = self.known_files.borrow().as_ref() {
            let normalized = file_path.replace('\\', "/");
            if known.contains(file_path) || known.contains(&normalized) {
                return true;
            }
        }
        // Bound the filesystem fallback to the project root. Relative-import
        // resolution can hand us paths carrying `../` segments, and `Path::join`
        // does not clamp, so an unguarded probe would `stat` arbitrary absolute
        // paths outside the root. A path outside the root can never be an
        // indexed project file, so refusing it here is the correct answer, not
        // a new restriction. Lexical-only containment mirrors the indexing
        // tier's `allowSymlinkEscape` behavior: in-root symlinks still resolve.
        if !crate::utils::is_path_within_root(file_path, Path::new(&self.project_root)) {
            return false;
        }
        Path::new(&self.project_root).join(file_path).exists()
    }

    pub(super) fn cached_file_text(&self, file_path: &str) -> Option<String> {
        let key = file_path.to_string();
        if self.file_cache.borrow().has(&key) {
            return self.file_cache.borrow_mut().get(&key).cloned().flatten();
        }

        let full_path = Path::new(&self.project_root).join(file_path);
        match fs::read(&full_path) {
            Ok(bytes) => {
                let content = String::from_utf8_lossy(&bytes).into_owned();
                self.file_cache.borrow_mut().set(key, Some(content.clone()));
                Some(content)
            }
            Err(error) => {
                log_debug(
                    "Failed to read file for resolution",
                    Some(&serde_json::json!({
                        "filePath": file_path,
                        "error": error.to_string()
                    })),
                );
                self.file_cache.borrow_mut().set(key, None);
                None
            }
        }
    }

    pub(super) fn cached_all_files(&self) -> Vec<String> {
        if let Some(cached) = self.files_list.borrow().as_ref() {
            return cached.as_ref().clone();
        }
        let files = self.queries.get_all_file_paths().unwrap_or_else(|error| {
            log_warn(
                "Failed to load file paths during resolution",
                Some(&serde_json::json!({ "error": error.to_string() })),
            );
            Vec::new()
        });
        *self.files_list.borrow_mut() = Some(Arc::new(files.clone()));
        files
    }

    pub(super) fn directories_in(&self, relative_path: &str) -> Vec<String> {
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
                    "Failed to list directory for resolution",
                    Some(&serde_json::json!({
                        "relativePath": relative_path,
                        "error": error.to_string()
                    })),
                );
                Vec::new()
            }
        }
    }
}
