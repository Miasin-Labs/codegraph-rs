//! Directory Management
//!
//! Manages the `.codegraph/` directory structure for CodeGraph data.
//! Ported from `src/directory.ts`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::{CodeGraphError, Result};

/// Default CodeGraph directory name.
pub const CODEGRAPH_DIR: &str = ".codegraph";

static WARNED_INVALID_CODEGRAPH_DIR: AtomicBool = AtomicBool::new(false);

/// Active project data-directory name. `CODEGRAPH_DIR` may select a sibling
/// index (for example `.codegraph-ci`) but must remain one plain path segment.
pub fn codegraph_dir_name() -> String {
    codegraph_dir_name_for(std::env::var("CODEGRAPH_DIR").ok().as_deref())
}

fn codegraph_dir_name_for(raw: Option<&str>) -> String {
    let Some(raw) = raw.map(str::trim).filter(|raw| !raw.is_empty()) else {
        return CODEGRAPH_DIR.to_string();
    };
    let invalid = raw == "."
        || raw.contains("..")
        || raw.contains('/')
        || raw.contains('\\')
        || Path::new(raw).is_absolute();
    if invalid {
        if !WARNED_INVALID_CODEGRAPH_DIR.swap(true, Ordering::Relaxed) {
            eprintln!(
                "[codegraph] Ignoring invalid CODEGRAPH_DIR=\"{raw}\": it must be a plain directory name; using \"{CODEGRAPH_DIR}\""
            );
        }
        CODEGRAPH_DIR.to_string()
    } else {
        raw.to_string()
    }
}

/// Whether a path segment is any CodeGraph-owned data directory.
pub fn is_codegraph_data_dir(name: &str) -> bool {
    name == CODEGRAPH_DIR
        || name == codegraph_dir_name()
        || name.starts_with(&format!("{CODEGRAPH_DIR}-"))
}

const GITIGNORE_CONTENT: &str = "# CodeGraph data files — local to each machine, not for committing.\n\
# Ignore everything in .codegraph/ except this file itself, so transient\n\
# files (the database, daemon.pid, sockets, logs) never show up in git.\n\
*\n\
!.gitignore\n";

/// Get the `.codegraph` directory path for a project.
pub fn get_codegraph_dir(project_root: &Path) -> PathBuf {
    project_root.join(codegraph_dir_name())
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
///
/// The walk crosses git checkout boundaries, so from a worktree nested inside
/// the main checkout it finds the MAIN checkout's index. That is fine for
/// reading (with the worktree notice), never for writing: commands that write
/// an index resolve it through
/// [`crate::sync::worktree::find_writable_codegraph_root`], which refuses an
/// index that [`checkout_below_index_root`] says belongs to another checkout.
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

/// Whether `dir` is the top level of its own git checkout.
///
/// A checkout marks its top level with a `.git` entry: a directory for a
/// repository's working tree (the main checkout, or a clone nested inside
/// another project), a `gitdir:` file for a linked worktree (`git worktree
/// add`) or a clone with a separate git dir. Code above that entry belongs to
/// a different checkout, so an index found above it is someone else's.
///
/// A submodule is the exception. Its `.git` file points into the
/// superproject's `.git/modules/`; the superproject checks it out and indexes
/// its files (`git ls-files --recurse-submodules`), so a submodule stays part
/// of the superproject's checkout, like any plain subdirectory.
///
/// Filesystem checks only — one `stat`, plus reading a `.git` file when there
/// is one — never a `git` process: the prompt hook runs this under a timeout.
pub fn is_git_checkout_root(dir: &Path) -> bool {
    let dot_git = dir.join(".git");
    match fs::metadata(&dot_git) {
        Ok(meta) if meta.is_dir() => true,
        Ok(meta) if meta.is_file() => !links_a_submodule(dir, &dot_git),
        _ => false,
    }
}

/// Whether the `.git` file `dot_git` in `dir` links a submodule's working
/// tree rather than a linked worktree or a separate-git-dir clone.
fn links_a_submodule(dir: &Path, dot_git: &Path) -> bool {
    let Some(git_dir) = git_file_target(dir, dot_git) else {
        return false;
    };
    // Every linked worktree's private git dir holds a `commondir` file (and
    // lives in `<common>/worktrees/<name>`); a submodule's git dir is a whole
    // repository and has neither.
    if git_dir.join("commondir").is_file()
        || git_dir.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new("worktrees"))
    {
        return false;
    }
    git_dir
        .components()
        .any(|component| component.as_os_str() == "modules")
}

/// The git dir a `.git` file points at (`gitdir: <path>`, relative paths
/// taken from the directory holding the file).
fn git_file_target(dir: &Path, dot_git: &Path) -> Option<PathBuf> {
    use std::io::Read;

    // A gitfile is one short line; never read more than a path's worth.
    let mut text = String::new();
    fs::File::open(dot_git)
        .ok()?
        .take(4096)
        .read_to_string(&mut text)
        .ok()?;
    let target = text.lines().next()?.strip_prefix("gitdir:")?.trim();
    if target.is_empty() {
        return None;
    }
    Some(crate::utils::lexical_resolve(dir, target))
}

/// The git checkout nested below `index_root` that `start_path` is in, if any.
///
/// Walks up from `start_path` to `index_root` (exclusive) and returns the
/// innermost checkout top level ([`is_git_checkout_root`]) passed on the way.
/// `Some` means the index at `index_root` belongs to a different checkout than
/// the one `start_path` is in — a worktree nested inside the main checkout, a
/// clone nested inside another project. `None` means `start_path` is a plain
/// subdirectory of `index_root`'s own checkout (a monorepo package, a
/// not-yet-created path), or that `index_root` is not above it at all.
///
/// Only an index that itself lives in a git checkout belongs to one. An index
/// over a plain directory (a multi-repo workspace such as `~/work/.codegraph`
/// above `~/work/a` and `~/work/b`) covers the repos inside it, so they are
/// never "another checkout".
pub fn checkout_below_index_root(start_path: &Path, index_root: &Path) -> Option<PathBuf> {
    let start = real_path_lenient(start_path);
    let root = real_path_lenient(index_root);
    if !start.starts_with(&root) || !root.ancestors().any(is_git_checkout_root) {
        return None;
    }
    start
        .ancestors()
        .take_while(|dir| *dir != root)
        .find(|dir| is_git_checkout_root(dir))
        .map(Path::to_path_buf)
}

/// `path` made absolute with symlinks resolved as far as it exists, so a
/// not-yet-created sub-path still compares equal to its real ancestors.
pub(crate) fn real_path_lenient(path: &Path) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let absolute = crate::utils::lexical_resolve(&cwd, &path.to_string_lossy());
    let mut missing = Vec::new();
    let mut existing = absolute.as_path();
    loop {
        if let Ok(real) = fs::canonicalize(existing) {
            return missing
                .iter()
                .rev()
                .fold(real, |real, part| real.join(part));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_os_string());
                existing = parent;
            }
            _ => return absolute,
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
    fn codegraph_dir_override_accepts_only_a_plain_directory_name() {
        assert_eq!(codegraph_dir_name_for(None), ".codegraph");
        assert_eq!(
            codegraph_dir_name_for(Some(" .codegraph-ci ")),
            ".codegraph-ci"
        );
        for invalid in [".", "..", "a/dir", "a\\dir", "/absolute", "foo..bar"] {
            assert_eq!(codegraph_dir_name_for(Some(invalid)), ".codegraph");
        }
        assert!(is_codegraph_data_dir(".codegraph"));
        assert!(is_codegraph_data_dir(".codegraph-ci"));
    }

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

    /// `root/.git` as a repository, `root/.git/worktrees/<name>` as a linked
    /// worktree's admin dir (with the `commondir` git writes there).
    fn fake_repo(root: &Path, worktrees: &[&str]) {
        fs::create_dir_all(root.join(".git")).unwrap();
        for name in worktrees {
            let admin = root.join(".git/worktrees").join(name);
            fs::create_dir_all(&admin).unwrap();
            fs::write(admin.join("commondir"), "../..\n").unwrap();
        }
    }

    fn fake_index(root: &Path) {
        create_directory(root).unwrap();
        fs::write(get_codegraph_dir(root).join("codegraph.db"), b"").unwrap();
    }

    fn real(path: &Path) -> PathBuf {
        fs::canonicalize(path).unwrap()
    }

    #[test]
    fn checkout_roots_are_git_dirs_and_worktree_files_but_not_submodules() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        fake_repo(&main, &["wt"]);
        assert!(is_git_checkout_root(&main));

        // Linked worktree nested inside the main checkout (absolute gitdir).
        let worktree = main.join(".claude/worktrees/wt");
        fs::create_dir_all(&worktree).unwrap();
        fs::write(
            worktree.join(".git"),
            format!("gitdir: {}\n", main.join(".git/worktrees/wt").display()),
        )
        .unwrap();
        assert!(is_git_checkout_root(&worktree));

        // Submodule: relative gitdir into the superproject's .git/modules/.
        let submodule = main.join("vendor/lib");
        fs::create_dir_all(main.join(".git/modules/vendor/lib")).unwrap();
        fs::create_dir_all(&submodule).unwrap();
        fs::write(
            submodule.join(".git"),
            "gitdir: ../../.git/modules/vendor/lib\n",
        )
        .unwrap();
        assert!(!is_git_checkout_root(&submodule));

        // A worktree whose admin dir is gone still reads as a worktree, and a
        // gitfile that isn't one (or a separate-git-dir clone) is a boundary.
        let stale = main.join("stale");
        fs::create_dir_all(&stale).unwrap();
        fs::write(stale.join(".git"), "gitdir: /gone/.git/worktrees/stale\n").unwrap();
        assert!(is_git_checkout_root(&stale));
        let odd = main.join("odd");
        fs::create_dir_all(&odd).unwrap();
        fs::write(odd.join(".git"), "not a gitfile\n").unwrap();
        assert!(is_git_checkout_root(&odd));

        // Plain directories are not.
        assert!(!is_git_checkout_root(&main.join("vendor")));
        assert!(!is_git_checkout_root(tmp.path()));
    }

    #[test]
    fn a_nested_worktree_is_a_different_checkout_than_the_index_above_it() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        fake_repo(&main, &["wt"]);
        fake_index(&main);
        let worktree = main.join(".claude/worktrees/wt");
        fs::create_dir_all(worktree.join("src/deep")).unwrap();
        fs::write(
            worktree.join(".git"),
            "gitdir: ../../../.git/worktrees/wt\n",
        )
        .unwrap();

        // The read walk still finds the main checkout's index...
        let found = find_nearest_codegraph_root(&worktree.join("src/deep")).unwrap();
        assert_eq!(real(&found), real(&main));
        // ...but it is the worktree's checkout that sits between.
        for start in [
            worktree.clone(),
            worktree.join("src/deep"),
            worktree.join("not/yet/created"),
        ] {
            assert_eq!(
                checkout_below_index_root(&start, &main),
                Some(real(&worktree)),
                "{}",
                start.display()
            );
        }

        // With its own index the worktree resolves to itself.
        fake_index(&worktree);
        let found = find_nearest_codegraph_root(&worktree.join("src/deep")).unwrap();
        assert_eq!(real(&found), real(&worktree));
        assert_eq!(
            checkout_below_index_root(&worktree.join("src"), &found),
            None
        );
    }

    #[test]
    fn plain_subdirectories_and_submodules_belong_to_the_index_above_them() {
        let tmp = tempfile::tempdir().unwrap();
        let main = tmp.path().join("main");
        fake_repo(&main, &[]);
        fake_index(&main);
        let package = main.join("packages/app/src");
        fs::create_dir_all(&package).unwrap();
        assert_eq!(checkout_below_index_root(&package, &main), None);
        assert_eq!(checkout_below_index_root(&main, &main), None);
        // Monorepo sub-paths that don't exist yet (issue #238).
        assert_eq!(
            checkout_below_index_root(&main.join("new/dir"), &main),
            None
        );

        let submodule = main.join("vendor/lib");
        fs::create_dir_all(main.join(".git/modules/lib")).unwrap();
        fs::create_dir_all(submodule.join("src")).unwrap();
        fs::write(submodule.join(".git"), "gitdir: ../../.git/modules/lib\n").unwrap();
        assert_eq!(
            checkout_below_index_root(&submodule.join("src"), &main),
            None
        );

        // An index somewhere unrelated is never "above" the start.
        let elsewhere = tmp.path().join("elsewhere");
        fake_index(&elsewhere);
        assert_eq!(checkout_below_index_root(&package, &elsewhere), None);
    }

    #[test]
    fn a_repository_nested_below_an_indexed_checkout_is_a_different_checkout() {
        // A clone (`.git` directory) inside an indexed git project: the
        // index above it belongs to the outer checkout, not to it.
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        fake_repo(&project, &[]);
        fake_index(&project);
        let clone = project.join("vendor").join("repo");
        fake_repo(&clone, &[]);
        fs::create_dir_all(clone.join("src")).unwrap();
        assert_eq!(
            checkout_below_index_root(&clone.join("src"), &project),
            Some(real(&clone))
        );
    }

    #[test]
    fn repositories_in_an_indexed_plain_workspace_belong_to_its_index() {
        // `~/work/.codegraph` over `~/work/a` and `~/work/b`: the workspace
        // index covers the repos inside it, so they may read and write it.
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("workspace");
        fake_index(&workspace);
        let repo = workspace.join("repo");
        fake_repo(&repo, &[]);
        fs::create_dir_all(repo.join("src")).unwrap();
        assert_eq!(
            checkout_below_index_root(&repo.join("src"), &workspace),
            None
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
