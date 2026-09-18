//! Go modules: `$GOMODCACHE/<escaped path>@<escaped version>`, or the
//! project's `vendor/` when `vendor/modules.txt` pins the same version.

use std::fs;
use std::path::{Path, PathBuf};

use crate::deps::model::ResolvedDep;

/// The module cache's case encoding: every upper-case letter becomes `!`
/// plus its lower-case form (`github.com/BurntSushi/toml` →
/// `github.com/!burnt!sushi/toml`), so paths differing only in case stay
/// distinct on case-insensitive filesystems. Applied to versions too.
pub fn escape_module_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 4);
    for c in path.chars() {
        if c.is_ascii_uppercase() {
            out.push('!');
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

pub(super) fn locate(
    dep: &ResolvedDep,
    project_root: &Path,
    mod_cache: Option<&Path>,
) -> Option<PathBuf> {
    let module = &dep.key.name;
    let version = &dep.lock_version;
    if let Some(cache) = mod_cache {
        let dir = cache.join(format!(
            "{}@{}",
            escape_module_path(module),
            escape_module_path(version)
        ));
        if dir.is_dir() {
            return Some(dir);
        }
    }
    let lock_dir = dep
        .lockfile
        .rsplit_once('/')
        .map_or(project_root.to_path_buf(), |(dir, _)| {
            project_root.join(dir)
        });
    vendored(&lock_dir, module, version)
}

fn vendored(lock_dir: &Path, module: &str, version: &str) -> Option<PathBuf> {
    let vendor = lock_dir.join("vendor");
    let manifest = fs::read_to_string(vendor.join("modules.txt")).ok()?;
    let pinned = manifest.lines().any(|line| {
        let mut words = line.split_whitespace();
        words.next() == Some("#") && words.next() == Some(module) && words.next() == Some(version)
    });
    let dir = vendor.join(module);
    (pinned && dir.is_dir()).then_some(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_upper_case() {
        assert_eq!(
            escape_module_path("github.com/BurntSushi/toml"),
            "github.com/!burnt!sushi/toml"
        );
        assert_eq!(escape_module_path("golang.org/x/sys"), "golang.org/x/sys");
        assert_eq!(escape_module_path("v1.0.0-RC1"), "v1.0.0-!r!c1");
    }
}
