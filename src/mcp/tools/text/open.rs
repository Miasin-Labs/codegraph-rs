//! Opening candidate files without leaving the project root, cheaply.
//!
//! `resolve_existing_path_within_root_real` canonicalizes every path it
//! checks — a handful of syscalls per path component, which dominates a
//! search over 70K small files. Here each *directory* is canonicalized and
//! checked once (per scanner thread), and the file itself is opened with
//! `O_NOFOLLOW`, so a symlinked file cannot redirect the read either. A file
//! that is a symlink takes the fully checked path instead.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use crate::utils::{resolve_existing_path_within_root_real, validate_path_within_root};

pub(in crate::mcp::tools) struct Opener<'a> {
    root: &'a Path,
    real_root: Option<PathBuf>,
    /// Canonical directory per project-relative directory; `None` when it
    /// does not exist or resolves outside the root.
    dirs: HashMap<String, Option<PathBuf>>,
}

impl<'a> Opener<'a> {
    pub fn new(root: &'a Path) -> Self {
        Self {
            root,
            real_root: std::fs::canonicalize(root).ok(),
            dirs: HashMap::new(),
        }
    }

    /// Open the project-relative `path` for reading, or `None` when it is
    /// missing or would resolve outside the project root.
    pub fn open(&mut self, path: &str) -> Option<File> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            if let Some(resolved) = self.resolve(path) {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&resolved)
                {
                    Ok(file) => return Some(file),
                    // The file is a symlink: check where it points.
                    Err(error) if error.raw_os_error() == Some(libc::ELOOP) => {}
                    Err(_) => return None,
                }
            }
        }
        let real = resolve_existing_path_within_root_real(self.root, path)?;
        File::open(real).ok()
    }

    /// The file's path under its canonical, in-root parent directory.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn resolve(&mut self, path: &str) -> Option<PathBuf> {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        if name.is_empty() || name == "." || name == ".." {
            return None;
        }
        let root = self.root;
        let real_root = self.real_root.as_deref()?;
        let real_dir = self
            .dirs
            .entry(dir.to_string())
            .or_insert_with(|| {
                let lexical = validate_path_within_root(root, dir)?;
                let real = std::fs::canonicalize(lexical).ok()?;
                real.starts_with(real_root).then_some(real)
            })
            .as_ref()?;
        Some(real_dir.join(name))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    fn read(opener: &mut Opener<'_>, path: &str) -> Option<String> {
        let mut text = String::new();
        opener.open(path)?.read_to_string(&mut text).ok()?;
        Some(text)
    }

    #[test]
    fn opens_files_inside_the_root_only() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "secret").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("src")).unwrap();
        std::fs::write(root.path().join("src/a.rs"), "fn a() {}").unwrap();
        std::fs::write(root.path().join("top.rs"), "top").unwrap();

        let mut opener = Opener::new(root.path());
        assert_eq!(read(&mut opener, "src/a.rs").as_deref(), Some("fn a() {}"));
        assert_eq!(read(&mut opener, "top.rs").as_deref(), Some("top"));
        assert!(read(&mut opener, "src/missing.rs").is_none());
        assert!(read(&mut opener, "../escape.rs").is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            // A symlinked directory or file that points outside is refused.
            symlink(outside.path(), root.path().join("linked")).unwrap();
            assert!(read(&mut opener, "linked/secret.txt").is_none());
            symlink(
                outside.path().join("secret.txt"),
                root.path().join("src/leak.rs"),
            )
            .unwrap();
            assert!(read(&mut opener, "src/leak.rs").is_none());
            // A symlink that stays inside the root still reads.
            symlink(root.path().join("top.rs"), root.path().join("src/alias.rs")).unwrap();
            assert_eq!(read(&mut opener, "src/alias.rs").as_deref(), Some("top"));
        }
    }
}
