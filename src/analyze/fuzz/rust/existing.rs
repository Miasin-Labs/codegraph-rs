//! Fuzz targets the project already has: `fuzz/fuzz_targets/*.rs` (the
//! cargo-fuzz layout) and what they call.
//!
//! Calls come from the index's resolved edges out of those files. A fuzz
//! crate calls the library by its crate path (`demo::decode(data)`), which
//! in-project resolution does not link, so each target file is also read as
//! text: a candidate counts as called when its name appears there as a path
//! or method call (`::name(` / `.name(`).

use std::collections::HashMap;
use std::path::Path;

use super::api::RustApi;
use crate::analyze::bugs::Project;

/// The existing targets and the functions they call directly.
#[derive(Debug, Default)]
pub struct ExistingTargets {
    /// Target names (file stems), sorted.
    pub targets: Vec<String>,
    /// Function id → the target calling it (from the index).
    pub called: HashMap<String, String>,
    /// Every target file: (target, source text).
    pub texts: Vec<(String, String)>,
}

impl ExistingTargets {
    pub fn find(project: &Project, api: &RustApi, root: &Path) -> Self {
        let mut found = Self::default();
        let mut files: Vec<String> = project
            .files()
            .iter()
            .filter(|file| is_target_file(file))
            .cloned()
            .collect();
        for site in project.call_sites() {
            if is_target_file(&site.file) {
                found
                    .called
                    .entry(site.callee_id.clone())
                    .or_insert_with(|| stem(&site.file));
            }
        }
        for krate in api.crates() {
            let dir = root.join(&krate.dir).join("fuzz").join("fuzz_targets");
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                if !path.extension().is_some_and(|ext| ext == "rs") {
                    continue;
                }
                if let Ok(relative) = path.strip_prefix(root) {
                    files.push(relative.to_string_lossy().to_string());
                }
            }
        }
        files.sort();
        files.dedup();
        for file in files {
            let name = stem(&file);
            if let Ok(text) = std::fs::read_to_string(root.join(&file)) {
                found.texts.push((name.clone(), text));
            }
            found.targets.push(name);
        }
        found.targets.sort();
        found.targets.dedup();
        found
    }

    /// The target that calls function `id` named `name`, if any.
    pub fn caller_of(&self, id: &str, name: &str) -> Option<&str> {
        if let Some(target) = self.called.get(id) {
            return Some(target);
        }
        self.texts
            .iter()
            .find(|(_, text)| calls_by_name(text, name))
            .map(|(target, _)| target.as_str())
    }
}

fn is_target_file(file: &str) -> bool {
    file.ends_with(".rs") && (file.starts_with("fuzz_targets/") || file.contains("/fuzz_targets/"))
}

fn stem(file: &str) -> String {
    Path::new(file)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// `text` calls `name` as `…::name(` or `.name(`.
fn calls_by_name(text: &str, name: &str) -> bool {
    [format!("::{name}("), format!(".{name}(")]
        .iter()
        .any(|needle| text.contains(needle.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_files_and_calls_by_name() {
        assert!(is_target_file("fuzz/fuzz_targets/parse.rs"));
        assert!(is_target_file("crates/x/fuzz/fuzz_targets/parse.rs"));
        assert!(!is_target_file("src/fuzz.rs"));
        let text = "fuzz_target!(|data: &[u8]| { let _ = demo::decode(data); });";
        assert!(calls_by_name(text, "decode"));
        assert!(!calls_by_name(text, "encode"));
    }
}
