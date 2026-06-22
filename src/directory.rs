//! Directory Management
//!
//! Manages the `.codegraph/` directory structure for CodeGraph data.
//! Ported from `src/directory.ts`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{CodeGraphError, Result};

/// CodeGraph directory name.
pub const CODEGRAPH_DIR: &str = ".codegraph";

const GITIGNORE_CONTENT: &str = "# CodeGraph data files — local to each machine, not for committing.\n\
# Ignore everything in .codegraph/ except this file itself, so transient\n\
# files (the database, daemon.pid, sockets, logs) never show up in git.\n\
*\n\
!.gitignore\n";

/// Get the `.codegraph` directory path for a project.
pub fn get_codegraph_dir(project_root: &Path) -> PathBuf {
    project_root.join(CODEGRAPH_DIR)
}

/// Check if a project has been initialized with CodeGraph.
/// Requires both `.codegraph/` directory AND `codegraph.db` to exist.
pub fn is_initialized(project_root: &Path) -> bool {
    let dir = get_codegraph_dir(project_root);
    match fs::symlink_metadata(&dir) {
        Ok(meta) if meta.file_type().is_dir() => dir.join("codegraph.db").exists(),
        _ => false,
    }
}

/// Find the nearest parent directory containing `.codegraph/`.
///
/// Walks up from the given path to find a CodeGraph-initialized project,
/// similar to how git finds `.git/` directories.
pub fn find_nearest_codegraph_root(start_path: &Path) -> Option<PathBuf> {
    let mut current = crate::utils::lexical_resolve(
        &std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/")),
        &start_path.to_string_lossy(),
    );
    loop {
        if is_initialized(&current) {
            return Some(current);
        }
        {
            let parent = current.parent()?;
            current = parent.to_path_buf()
        }
    }
}

/// Create the `.codegraph` directory structure.
/// Note: only errors if `codegraph.db` already exists, not just if `.codegraph/` exists.
pub fn create_directory(project_root: &Path) -> Result<()> {
    let dir = get_codegraph_dir(project_root);
    let db_path = dir.join("codegraph.db");

    // Only throw if CodeGraph is actually initialized (db exists)
    if db_path.exists() {
        return Err(CodeGraphError::other(format!(
            "CodeGraph already initialized in {}",
            project_root.display()
        )));
    }

    fs::create_dir_all(&dir)?;

    // Create .gitignore inside .codegraph (if it doesn't exist)
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() {
        fs::write(&gitignore, GITIGNORE_CONTENT)?;
    }
    Ok(())
}

/// Remove the `.codegraph` directory.
pub fn remove_directory(project_root: &Path) -> Result<()> {
    let dir = get_codegraph_dir(project_root);

    let meta = match fs::symlink_metadata(&dir) {
        Ok(m) => m,
        Err(_) => return Ok(()), // doesn't exist
    };

    // Verify .codegraph is a real directory, not a symlink pointing elsewhere
    if meta.file_type().is_symlink() {
        // Only remove the symlink itself, never follow it for recursive delete
        fs::remove_file(&dir)?;
        return Ok(());
    }
    if !meta.is_dir() {
        fs::remove_file(&dir)?;
        return Ok(());
    }
    fs::remove_dir_all(&dir)?;
    Ok(())
}

/// Get all files in the `.codegraph` directory (relative paths, forward slashes).
pub fn list_directory_contents(project_root: &Path) -> Vec<String> {
    let dir = get_codegraph_dir(project_root);
    let mut files = Vec::new();
    if !dir.exists() {
        return files;
    }

    fn walk(dir: &Path, prefix: &str, files: &mut Vec<String>) {
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let relative = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let ftype = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            // Skip symlinks to prevent following links outside .codegraph
            if ftype.is_symlink() {
                continue;
            }
            if ftype.is_dir() {
                walk(&entry.path(), &relative, files);
            } else {
                files.push(relative);
            }
        }
    }

    walk(&dir, "", &mut files);
    files
}

/// Get the total size of the `.codegraph` directory in bytes.
pub fn get_directory_size(project_root: &Path) -> u64 {
    let dir = get_codegraph_dir(project_root);
    if !dir.exists() {
        return 0;
    }

    fn walk(dir: &Path) -> u64 {
        let mut total = 0;
        let entries = match fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return 0,
        };
        for entry in entries.flatten() {
            let ftype = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if ftype.is_symlink() {
                continue;
            }
            if ftype.is_dir() {
                total += walk(&entry.path());
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
        total
    }

    walk(&dir)
}

/// Ensure a subdirectory exists within `.codegraph`.
pub fn ensure_subdirectory(project_root: &Path, subdir_name: &str) -> Result<PathBuf> {
    if subdir_name.contains("..")
        || subdir_name.contains('/')
        || subdir_name.contains(std::path::MAIN_SEPARATOR)
    {
        return Err(CodeGraphError::other(format!(
            "Invalid subdirectory name: {subdir_name}"
        )));
    }
    let subdir = get_codegraph_dir(project_root).join(subdir_name);
    if !subdir.exists() {
        fs::create_dir_all(&subdir)?;
    }
    Ok(subdir)
}

/// Result of validating the `.codegraph` directory structure.
#[derive(Debug, Clone)]
pub struct DirectoryValidation {
    pub valid: bool,
    pub errors: Vec<String>,
}

/// Check if the `.codegraph` directory has valid structure.
pub fn validate_directory(project_root: &Path) -> DirectoryValidation {
    let mut errors = Vec::new();
    let dir = get_codegraph_dir(project_root);

    match fs::metadata(&dir) {
        Err(_) => {
            errors.push("CodeGraph directory does not exist".to_string());
            return DirectoryValidation {
                valid: false,
                errors,
            };
        }
        Ok(meta) if !meta.is_dir() => {
            errors.push(".codegraph exists but is not a directory".to_string());
            return DirectoryValidation {
                valid: false,
                errors,
            };
        }
        Ok(_) => {}
    }

    // Auto-repair missing .gitignore (non-critical file)
    let gitignore = dir.join(".gitignore");
    if !gitignore.exists() && fs::write(&gitignore, GITIGNORE_CONTENT).is_err() {
        errors.push(
            ".gitignore missing in .codegraph directory and could not be created".to_string(),
        );
    }

    DirectoryValidation {
        valid: errors.is_empty(),
        errors,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_validate_directory() {
        let tmp = tempfile::tempdir().unwrap();
        create_directory(tmp.path()).unwrap();
        let dir = get_codegraph_dir(tmp.path());
        assert!(dir.is_dir());
        assert!(dir.join(".gitignore").exists());
        // Not initialized until codegraph.db exists
        assert!(!is_initialized(tmp.path()));
        fs::write(dir.join("codegraph.db"), b"").unwrap();
        assert!(is_initialized(tmp.path()));
        // Second create must fail now that db exists
        assert!(create_directory(tmp.path()).is_err());
    }

    #[test]
    fn find_nearest_root_walks_up() {
        let tmp = tempfile::tempdir().unwrap();
        create_directory(tmp.path()).unwrap();
        fs::write(get_codegraph_dir(tmp.path()).join("codegraph.db"), b"").unwrap();
        let nested = tmp.path().join("a/b/c");
        fs::create_dir_all(&nested).unwrap();
        let found = find_nearest_codegraph_root(&nested).unwrap();
        assert_eq!(
            fs::canonicalize(found).unwrap(),
            fs::canonicalize(tmp.path()).unwrap()
        );
    }

    #[test]
    fn remove_directory_handles_missing() {
        let tmp = tempfile::tempdir().unwrap();
        remove_directory(tmp.path()).unwrap(); // no-op
        create_directory(tmp.path()).unwrap();
        remove_directory(tmp.path()).unwrap();
        assert!(!get_codegraph_dir(tmp.path()).exists());
    }

    #[test]
    fn ensure_subdirectory_rejects_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        create_directory(tmp.path()).unwrap();
        assert!(ensure_subdirectory(tmp.path(), "..").is_err());
        assert!(ensure_subdirectory(tmp.path(), "a/b").is_err());
        let sub = ensure_subdirectory(tmp.path(), "cache").unwrap();
        assert!(sub.is_dir());
    }

    #[test]
    fn directory_size_and_listing() {
        let tmp = tempfile::tempdir().unwrap();
        create_directory(tmp.path()).unwrap();
        let dir = get_codegraph_dir(tmp.path());
        fs::write(dir.join("data.bin"), vec![0u8; 100]).unwrap();
        let files = list_directory_contents(tmp.path());
        assert!(files.contains(&"data.bin".to_string()));
        assert!(get_directory_size(tmp.path()) >= 100);
    }
}
