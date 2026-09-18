//! Local links read from a project's manifests: Cargo path deps and
//! workspace members, npm/pnpm workspaces and `file:`/`link:`/`workspace:`
//! deps, go.mod `replace … => ./dir`. Registry and git dependencies (and
//! every lockfile) belong to `deps/`, not here.
//!
//! Which manifests: the project root's, every workspace member's (followed
//! transitively), and every `Cargo.toml`/`package.json`/`go.mod` the
//! project's index tracks — bounded, never a walk of the tree.

mod cargo;
mod glob;
mod gomod;
mod npm;

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::kinds::LinkKind;
use crate::directory::real_path_lenient;

/// Most manifest directories read per project.
const MAX_MANIFEST_DIRS: usize = 2_000;
/// Largest manifest read.
const MAX_MANIFEST_BYTES: u64 = 2 * 1024 * 1024;

/// One link a manifest declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestLink {
    pub kind: LinkKind,
    /// The directory linked to (canonical as far as it exists).
    pub target: PathBuf,
    /// The manifest declaring it (absolute).
    pub manifest: PathBuf,
    /// 1-based line of the entry.
    pub line: Option<u32>,
    /// Dependency / member / module name.
    pub detail: Option<String>,
}

/// Everything read from one project's manifests.
#[derive(Debug, Clone, Default)]
pub struct ManifestScan {
    pub links: Vec<ManifestLink>,
    /// Package name the root manifest declares.
    pub root_package: Option<String>,
    pub manifests_read: usize,
    /// Stopped at [`MAX_MANIFEST_DIRS`].
    pub truncated: bool,
}

/// A link before its target is resolved against the manifest's directory.
struct RawLink {
    kind: LinkKind,
    /// Relative to the manifest's directory (or absolute).
    path: String,
    line: Option<u32>,
    detail: Option<String>,
}

/// An npm `workspace:` dependency, resolved by package name once every
/// package of the project is known.
struct NamedDep {
    manifest: PathBuf,
    name: String,
    line: Option<u32>,
}

/// Read the links of the project at `root`. `indexed` lists repo-relative
/// manifest paths the project's index tracks.
pub fn scan_manifests(root: &Path, indexed: &[String]) -> ManifestScan {
    let mut queue: VecDeque<PathBuf> = VecDeque::from([root.to_path_buf()]);
    for rel in indexed {
        if let Some(dir) = root.join(rel).parent() {
            queue.push_back(dir.to_path_buf());
        }
    }
    let mut seen = BTreeSet::new();
    let mut scan = ManifestScan::default();
    let mut packages: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut named: Vec<NamedDep> = Vec::new();
    while let Some(dir) = queue.pop_front() {
        if !seen.insert(dir.clone()) || is_vendored(root, &dir) {
            continue;
        }
        if seen.len() > MAX_MANIFEST_DIRS {
            scan.truncated = true;
            break;
        }
        let at_root = dir == root;
        for file in [
            "Cargo.toml",
            "package.json",
            "go.mod",
            "pnpm-workspace.yaml",
        ] {
            let manifest = dir.join(file);
            let Some(text) = read_manifest(&manifest) else {
                continue;
            };
            scan.manifests_read += 1;
            let parsed = match file {
                "Cargo.toml" => cargo::parse(&dir, &text),
                "package.json" => npm::parse(&dir, &text),
                "go.mod" => gomod::parse(&text),
                _ => npm::parse_pnpm_workspace(&dir, &text),
            };
            if at_root && scan.root_package.is_none() {
                scan.root_package.clone_from(&parsed.package);
            }
            if let (Some(name), "package.json") = (&parsed.package, file) {
                packages.entry(name.clone()).or_insert_with(|| dir.clone());
            }
            for member in &parsed.members {
                queue.push_back(member.clone());
            }
            for name in parsed.workspace_deps {
                named.push(NamedDep {
                    manifest: manifest.clone(),
                    name: name.0,
                    line: name.1,
                });
            }
            for link in parsed.links {
                scan.links.push(ManifestLink {
                    kind: link.kind,
                    target: real_path_lenient(&crate::utils::lexical_resolve(&dir, &link.path)),
                    manifest: manifest.clone(),
                    line: link.line,
                    detail: link.detail,
                });
            }
        }
    }
    for dep in named {
        if let Some(dir) = packages.get(&dep.name) {
            scan.links.push(ManifestLink {
                kind: LinkKind::NpmFileDep,
                target: real_path_lenient(dir),
                manifest: dep.manifest,
                line: dep.line,
                detail: Some(dep.name),
            });
        }
    }
    scan.links.sort_by(|a, b| {
        (&a.manifest, a.line, a.kind, &a.target).cmp(&(&b.manifest, b.line, b.kind, &b.target))
    });
    scan.links.dedup();
    scan
}

/// What one manifest file declares.
#[derive(Default)]
struct Parsed {
    package: Option<String>,
    links: Vec<RawLink>,
    /// Workspace member directories to read next.
    members: Vec<PathBuf>,
    /// `workspace:` deps by package name, with their line.
    workspace_deps: Vec<(String, Option<u32>)>,
}

/// Directories whose manifests are not this project's to read: outside its
/// root (a `members = ["../x"]` entry is recorded as a link, but `x`'s own
/// deps are `x`'s), or installed dependencies / build output below it.
fn is_vendored(root: &Path, dir: &Path) -> bool {
    let Ok(rel) = dir.strip_prefix(root) else {
        return true;
    };
    rel.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some("node_modules" | ".git" | "target" | ".cargo")
        )
    })
}

fn read_manifest(path: &Path) -> Option<String> {
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_MANIFEST_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// 1-based line of byte `offset` in `text`.
fn line_of(text: &str, offset: usize) -> u32 {
    let end = offset.min(text.len());
    let newlines = text.as_bytes()[..end]
        .iter()
        .filter(|&&b| b == b'\n')
        .count();
    u32::try_from(newlines + 1).unwrap_or(u32::MAX)
}

/// 1-based line of the first line containing every one of `needles`.
fn line_containing(text: &str, needles: &[&str]) -> Option<u32> {
    text.lines()
        .position(|line| needles.iter().all(|n| line.contains(n)))
        .and_then(|i| u32::try_from(i + 1).ok())
}

#[cfg(test)]
mod tests;
