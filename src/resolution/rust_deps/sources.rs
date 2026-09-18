//! Where a locked crate's source is on this machine, if anywhere: vendored
//! into the project (`vendor/<name>-<version>/` or `vendor/<name>/`), in the
//! cargo registry's unpacked sources (`$CARGO_HOME/registry/src/*/`), or in
//! a git checkout (`$CARGO_HOME/git/checkouts/*/<short commit>/`). Nothing
//! is fetched: a crate that is not already here is left out.

use std::fs;
use std::path::{Path, PathBuf};

use super::lockfile::LockedCrate;

/// How many directories below a git checkout's root a crate may sit.
const MAX_GIT_DEPTH: usize = 3;
/// How many directories of one git checkout are looked into.
const MAX_GIT_DIRS: usize = 400;
/// How much of a `Cargo.toml` is read to find its `[package]` name/version.
const MAX_MANIFEST_BYTES: usize = 16 * 1024;

/// Where a crate's source came from: this decides where its artifact is
/// kept and what it is keyed by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Origin {
    /// Copied into the project's `vendor/`: the project owns it.
    Vendored,
    /// A registry release: immutable per name and version.
    Registry,
    /// A git dependency: immutable per commit.
    Git { commit: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrateSource {
    pub(crate) dir: PathBuf,
    pub(crate) origin: Origin,
}

/// The vendored copy of `krate` under `project_root`, if any.
pub(crate) fn vendored(krate: &LockedCrate, project_root: &Path) -> Option<PathBuf> {
    let vendor = project_root.join("vendor");
    let versioned = vendor.join(format!("{}-{}", krate.name, krate.version));
    if versioned.join("Cargo.toml").is_file() {
        return Some(versioned);
    }
    let plain = vendor.join(&krate.name);
    manifest_names(&plain, krate).then_some(plain)
}

/// Where `krate`'s source is: vendored first, then the registry or git
/// checkouts under `cargo_home`.
pub(crate) fn locate(
    krate: &LockedCrate,
    project_root: &Path,
    cargo_home: &Path,
) -> Option<CrateSource> {
    if let Some(dir) = vendored(krate, project_root) {
        return Some(CrateSource {
            dir,
            origin: Origin::Vendored,
        });
    }
    let source = krate.source.as_deref()?;
    if source.starts_with("registry+") || source.starts_with("sparse+") {
        let wanted = format!("{}-{}", krate.name, krate.version);
        let dir = subdirectories(&cargo_home.join("registry").join("src"))
            .into_iter()
            .map(|index| index.join(&wanted))
            .find(|dir| dir.join("Cargo.toml").is_file())?;
        return Some(CrateSource {
            dir,
            origin: Origin::Registry,
        });
    }
    let commit = krate.git_commit()?;
    let short = &commit[..7];
    let dir = subdirectories(&cargo_home.join("git").join("checkouts"))
        .into_iter()
        .map(|repository| repository.join(short))
        .filter(|checkout| checkout.is_dir())
        .find_map(|checkout| crate_in_checkout(&checkout, krate))?;
    Some(CrateSource {
        dir,
        origin: Origin::Git {
            commit: commit.to_string(),
        },
    })
}

/// The directory in a git checkout whose manifest is `krate`'s.
fn crate_in_checkout(checkout: &Path, krate: &LockedCrate) -> Option<PathBuf> {
    let mut level = vec![checkout.to_path_buf()];
    let mut seen = 0;
    for _ in 0..=MAX_GIT_DEPTH {
        let mut next = Vec::new();
        for dir in level {
            if manifest_names(&dir, krate) {
                return Some(dir);
            }
            seen += 1;
            if seen > MAX_GIT_DIRS {
                return None;
            }
            next.extend(subdirectories(&dir).into_iter().filter(|child| {
                child
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| !name.starts_with('.') && name != "target")
            }));
        }
        level = next;
    }
    None
}

/// `dir/Cargo.toml` declares the package `krate` (name and version).
fn manifest_names(dir: &Path, krate: &LockedCrate) -> bool {
    let Ok(text) = fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    let text = &text[..floor_char_boundary(&text, MAX_MANIFEST_BYTES)];
    let mut in_package = false;
    let (mut name, mut version) = (false, false);
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim().trim_matches('"');
        match key.trim() {
            "name" => name = value == krate.name,
            "version" => version = value == krate.version,
            _ => {}
        }
    }
    name && version
}

fn floor_char_boundary(text: &str, index: usize) -> usize {
    let mut end = index.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// The subdirectories of `dir`, sorted; none when it cannot be read.
fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{Origin, locate};
    use crate::resolution::rust_deps::lockfile::LockedCrate;

    fn krate(name: &str, version: &str, source: &str) -> LockedCrate {
        LockedCrate {
            name: name.into(),
            version: version.into(),
            source: Some(source.into()),
        }
    }

    fn manifest(dir: &Path, name: &str, version: &str) {
        fs::create_dir_all(dir).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"{version}\"\n"),
        )
        .unwrap();
    }

    #[test]
    fn finds_vendored_registry_and_git_sources() {
        let project = tempfile::tempdir().unwrap();
        let cargo = tempfile::tempdir().unwrap();
        let registry = "registry+https://github.com/rust-lang/crates.io-index";
        manifest(&project.path().join("vendor/vend"), "vend", "1.0.0");
        manifest(
            &cargo
                .path()
                .join("registry/src/index.crates.io-abc/reg-2.1.0"),
            "reg",
            "2.1.0",
        );
        manifest(
            &cargo
                .path()
                .join("git/checkouts/repo-9f8e/abcdef1/crates/gitty"),
            "gitty",
            "0.3.0",
        );
        let at = |krate: &LockedCrate| locate(krate, project.path(), cargo.path());

        let vend = at(&krate("vend", "1.0.0", registry)).unwrap();
        assert_eq!(vend.origin, Origin::Vendored);
        // A vendored copy of another version is not this crate.
        assert_eq!(
            at(&krate("vend", "1.0.1", registry)).map(|found| found.origin),
            None
        );
        let reg = at(&krate("reg", "2.1.0", registry)).unwrap();
        assert_eq!(reg.origin, Origin::Registry);
        assert!(reg.dir.ends_with("reg-2.1.0"));
        let git = at(&krate(
            "gitty",
            "0.3.0",
            "git+https://github.com/o/repo#abcdef1234567890",
        ))
        .unwrap();
        assert!(git.dir.ends_with("crates/gitty"));
        assert_eq!(
            git.origin,
            Origin::Git {
                commit: "abcdef1234567890".into()
            }
        );
        assert_eq!(at(&krate("absent", "1.0.0", registry)), None);
    }
}
