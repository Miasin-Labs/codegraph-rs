//! Rust crates: `$CARGO_HOME/registry/src/<index>/<name>-<version>/`, git
//! checkouts under `$CARGO_HOME/git/checkouts/<repo>-<hash>/<short rev>/`,
//! or the project's `vendor/` (as `cargo vendor` lays it out).

use std::fs;
use std::path::{Path, PathBuf};

use crate::deps::model::{DepSource, ResolvedDep};

/// Short-commit length cargo names checkout directories with.
const CHECKOUT_REV_CHARS: usize = 7;
/// How deep inside a git checkout a workspace member crate is looked for.
const CHECKOUT_SEARCH_DEPTH: usize = 3;

pub(super) fn locate(
    dep: &ResolvedDep,
    project_root: &Path,
    cargo_home: Option<&Path>,
    registry_dirs: &[PathBuf],
) -> Option<PathBuf> {
    let name = &dep.key.name;
    let version = &dep.lock_version;
    match &dep.source {
        DepSource::Registry => registry_dirs
            .iter()
            .map(|index| index.join(format!("{name}-{version}")))
            .find(|dir| dir.join("Cargo.toml").is_file())
            .or_else(|| vendored(project_root, name, version)),
        DepSource::Git { url, rev } => cargo_home
            .and_then(|home| git_checkout(&home.join("git").join("checkouts"), url, rev, name))
            .or_else(|| vendored(project_root, name, version)),
        DepSource::Path { .. } => None,
    }
}

/// `$CARGO_HOME/registry/src/*` — one directory per registry index.
pub(super) fn registry_dirs(cargo_home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(cargo_home.join("registry").join("src"))
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// `vendor/<name>-<version>` (duplicate versions) or `vendor/<name>`.
fn vendored(project_root: &Path, name: &str, version: &str) -> Option<PathBuf> {
    let vendor = project_root.join("vendor");
    let versioned = vendor.join(format!("{name}-{version}"));
    if versioned.join("Cargo.toml").is_file() {
        return Some(versioned);
    }
    let plain = vendor.join(name);
    let manifest = fs::read_to_string(plain.join("Cargo.toml")).ok()?;
    (manifest_field(&manifest, "name").as_deref() == Some(name)
        && manifest_field(&manifest, "version").as_deref() == Some(version))
    .then_some(plain)
}

/// A crate inside a git checkout: `checkouts/<repo>-<hash>/<rev7>/…`, where
/// the crate may be any workspace member of the repository.
fn git_checkout(checkouts: &Path, url: &str, rev: &str, name: &str) -> Option<PathBuf> {
    let repo = url
        .trim_end_matches('/')
        .rsplit('/')
        .next()?
        .trim_end_matches(".git");
    let short: String = rev.chars().take(CHECKOUT_REV_CHARS).collect();
    if repo.is_empty() || short.len() < CHECKOUT_REV_CHARS {
        return None;
    }
    let prefix = format!("{repo}-");
    let mut repos: Vec<PathBuf> = fs::read_dir(checkouts)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .and_then(|n| n.strip_prefix(&prefix))
                .is_some_and(|hash| hash.len() == 16 && hash.chars().all(|c| c.is_ascii_hexdigit()))
        })
        .map(|e| e.path())
        .collect();
    repos.sort();
    repos
        .into_iter()
        .map(|dir| dir.join(&short))
        .filter(|dir| dir.is_dir())
        .find_map(|checkout| crate_in_checkout(&checkout, name))
}

fn crate_in_checkout(checkout: &Path, name: &str) -> Option<PathBuf> {
    walkdir::WalkDir::new(checkout)
        .max_depth(CHECKOUT_SEARCH_DEPTH)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let n = e.file_name().to_string_lossy();
            e.depth() == 0 || !(n.starts_with('.') || n == "target")
        })
        .flatten()
        .filter(|e| e.file_type().is_file() && e.file_name() == "Cargo.toml")
        .find(|e| {
            fs::read_to_string(e.path())
                .ok()
                .and_then(|m| package_name(&m))
                .as_deref()
                == Some(name)
        })
        .and_then(|e| e.path().parent().map(Path::to_path_buf))
}

fn package_name(manifest: &str) -> Option<String> {
    manifest_field(manifest, "name")
}

/// `key = "…"` inside `[package]`.
fn manifest_field(manifest: &str, key: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if !in_package {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == key {
                let v = v.trim();
                return v
                    .strip_prefix('"')
                    .and_then(|r| r.split('"').next())
                    .map(str::to_string);
            }
        }
    }
    None
}
