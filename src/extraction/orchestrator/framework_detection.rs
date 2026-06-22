use std::fs;
use std::path::PathBuf;

use super::pipeline::ExtractionOrchestrator;
use super::scan::scan_directory;
use crate::resolution::frameworks::detect_frameworks;
use crate::resolution::types::{ImportMapping, ResolutionContext};
use crate::types::{Language, Node, NodeKind};
use crate::utils::validate_existing_path_within_root_real;

// =============================================================================
// Framework detection context
// =============================================================================

/// Filesystem-backed `ResolutionContext` sufficient for framework detection.
/// Graph-query methods return empty because the DB hasn't been populated yet,
/// but `detect()` only uses `read_file`, `file_exists`, `get_all_files` and
/// `list_directories`, so that's fine.
struct DetectionContext {
    root_dir: PathBuf,
    root_str: String,
    files: Vec<String>,
}

impl ResolutionContext for DetectionContext {
    fn get_nodes_in_file(&self, _file_path: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_name(&self, _name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_kind(&self, _kind: NodeKind) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
    fn get_all_files(&self) -> Vec<String> {
        self.files.clone()
    }
    fn get_project_root(&self) -> &str {
        &self.root_str
    }
    fn file_exists(&self, relative_path: &str) -> bool {
        match validate_existing_path_within_root_real(&self.root_dir, relative_path) {
            Some(full) => full.exists(),
            None => false,
        }
    }
    fn read_file(&self, relative_path: &str) -> Option<String> {
        let full = validate_existing_path_within_root_real(&self.root_dir, relative_path)?;
        fs::read(&full)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
    // Monorepo support — needed by framework detect()s that probe
    // subpackage manifests (e.g. fabric-view looking at
    // packages/<sub>/package.json when the root manifest is just a
    // workspace declaration). Matches the resolver-context shape.
    fn list_directories(&self, relative_path: &str) -> Vec<String> {
        let target = match validate_existing_path_within_root_real(&self.root_dir, relative_path) {
            Some(target) => target,
            None => return Vec::new(),
        };
        match fs::read_dir(&target) {
            Ok(entries) => entries
                .flatten()
                .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

impl<'a> ExtractionOrchestrator<'a> {
    /// Detect frameworks on demand using the current scanned files (or a fresh
    /// scan if none are provided). Cached on the orchestrator so repeat calls
    /// inside a single run don't re-scan.
    pub(super) fn ensure_detected_frameworks(&self, files: Option<&[String]>) -> Vec<String> {
        if let Some(names) = self.detected_framework_names.borrow().as_ref() {
            return names.clone();
        }
        let file_list: Vec<String> = match files {
            Some(f) => f.to_vec(),
            None => scan_directory(&self.root_dir, None),
        };
        let context = DetectionContext {
            root_str: self.root_dir.to_string_lossy().into_owned(),
            root_dir: self.root_dir.clone(),
            files: file_list,
        };
        let names: Vec<String> = detect_frameworks(&context)
            .iter()
            .map(|r| r.name().to_string())
            .collect();
        *self.detected_framework_names.borrow_mut() = Some(names.clone());
        names
    }

    pub fn reset_detected_frameworks(&self) {
        *self.detected_framework_names.borrow_mut() = None;
    }
}
