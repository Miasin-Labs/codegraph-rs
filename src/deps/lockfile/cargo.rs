//! `Cargo.lock` (formats v1–v4): every `[[package]]` with its source, plus
//! which ones the workspace names directly.
//!
//! The format is a flat list of `[[package]]` tables whose values are quoted
//! strings and one string array, so a line scanner reads it exactly without a
//! TOML dependency. Packages without a `source` are local: workspace members
//! (the project itself — skipped) or path dependencies (recorded as
//! [`DepSource::Path`] for the atlas, never built).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use crate::deps::model::{DepKey, DepSource, Ecosystem, ResolvedDep};

#[derive(Debug, Default, Clone)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    dependencies: Vec<String>,
}

/// Parse `text` (the lockfile at `lockfile`, relative to the project root).
/// `members` are the workspace's own package names, which are not
/// dependencies.
pub(crate) fn parse(text: &str, lockfile: &str, members: &HashSet<String>) -> Vec<ResolvedDep> {
    let packages = read_packages(text);

    // Direct = named by a local package. Path dependencies' own
    // dependencies count too (they are the project's code as far as the
    // lockfile can tell).
    let mut by_name: HashMap<&str, Vec<&Package>> = HashMap::new();
    for p in &packages {
        by_name.entry(p.name.as_str()).or_default().push(p);
    }
    let mut direct: HashSet<(String, String)> = HashSet::new();
    for local in packages.iter().filter(|p| p.source.is_none()) {
        for dep in &local.dependencies {
            let mut parts = dep.split_whitespace();
            let Some(name) = parts.next() else { continue };
            let version = parts.next();
            let Some(candidates) = by_name.get(name) else {
                continue;
            };
            for c in candidates {
                if version.is_none_or(|v| v == c.version) {
                    direct.insert((c.name.clone(), c.version.clone()));
                }
            }
        }
    }

    packages
        .iter()
        .filter(|p| !(p.source.is_none() && members.contains(&p.name)))
        .map(|p| {
            let source = parse_source(p.source.as_deref());
            let is_local = matches!(source, DepSource::Path { .. });
            ResolvedDep {
                key: DepKey::for_source(Ecosystem::Crates, &p.name, &p.version, &source),
                lock_version: p.version.clone(),
                direct: if is_local {
                    None
                } else {
                    Some(direct.contains(&(p.name.clone(), p.version.clone())))
                },
                source,
                lockfile: lockfile.to_string(),
                install_path: None,
            }
        })
        .collect()
}

fn read_packages(text: &str) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut current: Option<Package> = None;
    let mut in_deps_array = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') && !in_deps_array {
            if let Some(done) = current.take() {
                packages.push(done);
            }
            if line == "[[package]]" {
                current = Some(Package::default());
            }
            continue;
        }
        let Some(pkg) = current.as_mut() else {
            continue;
        };
        if in_deps_array {
            collect_quoted(line, &mut pkg.dependencies);
            if line.contains(']') {
                in_deps_array = false;
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "name" => pkg.name = unquote(value),
            "version" => pkg.version = unquote(value),
            "source" => pkg.source = Some(unquote(value)),
            "dependencies" => {
                collect_quoted(value, &mut pkg.dependencies);
                in_deps_array = value.contains('[') && !value.contains(']');
            }
            _ => {}
        }
    }
    if let Some(done) = current.take() {
        packages.push(done);
    }
    packages.retain(|p| !p.name.is_empty() && !p.version.is_empty());
    packages
}

/// `"x"` → `x` (trailing comments ignored).
fn unquote(value: &str) -> String {
    let value = value.trim();
    match value.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or("").to_string(),
        None => value.to_string(),
    }
}

/// Push every `"…"` string in `text`.
fn collect_quoted(text: &str, out: &mut Vec<String>) {
    let mut rest = text;
    while let Some(start) = rest.find('"') {
        let after = &rest[start + 1..];
        let Some(end) = after.find('"') else { break };
        out.push(after[..end].to_string());
        rest = &after[end + 1..];
    }
}

/// `registry+…` / `sparse+…` → registry; `git+URL?query#sha` → git.
fn parse_source(source: Option<&str>) -> DepSource {
    let Some(source) = source else {
        return DepSource::Path { path: None };
    };
    if let Some(git) = source.strip_prefix("git+") {
        let (without_fragment, fragment) = match git.split_once('#') {
            Some((url, rev)) => (url, Some(rev)),
            None => (git, None),
        };
        let (url, query) = match without_fragment.split_once('?') {
            Some((url, query)) => (url, Some(query)),
            None => (without_fragment, None),
        };
        // The fragment is the resolved commit; `?rev=` is only what the
        // manifest asked for (maybe a short hash or a branch).
        let rev = fragment
            .map(str::to_string)
            .or_else(|| {
                query.and_then(|q| {
                    q.split('&')
                        .find_map(|kv| kv.strip_prefix("rev=").map(str::to_string))
                })
            })
            .unwrap_or_default();
        return DepSource::Git {
            url: url.to_string(),
            rev,
        };
    }
    DepSource::Registry
}

/// The package names the workspace rooted at `root` builds: its own
/// `[package]` and every `[workspace] members` entry (one `*` level of glob).
/// Best effort: a manifest it cannot read contributes nothing.
pub(crate) fn workspace_members(root: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(manifest) = fs::read_to_string(root.join("Cargo.toml")) else {
        return names;
    };
    if let Some(name) = package_name(&manifest) {
        names.insert(name);
    }
    for member in member_patterns(&manifest) {
        let dirs = match member.strip_suffix("/*") {
            Some(parent) => fs::read_dir(root.join(parent))
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|e| e.path())
                        .filter(|p| p.is_dir())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default(),
            None if !member.contains('*') => vec![root.join(&member)],
            None => Vec::new(),
        };
        for dir in dirs {
            if let Some(name) = fs::read_to_string(dir.join("Cargo.toml"))
                .ok()
                .as_deref()
                .and_then(package_name)
            {
                names.insert(name);
            }
        }
    }
    names
}

/// `name = "…"` inside the `[package]` table.
fn package_name(manifest: &str) -> Option<String> {
    let mut in_package = false;
    for line in manifest.lines().map(str::trim) {
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "name" {
                    return Some(unquote(value));
                }
            }
        }
    }
    None
}

/// The strings of `members = [ … ]` inside `[workspace]`.
fn member_patterns(manifest: &str) -> Vec<String> {
    let mut in_workspace = false;
    let mut collecting = false;
    let mut out = Vec::new();
    for line in manifest.lines().map(str::trim) {
        if collecting {
            collect_quoted(line.split('#').next().unwrap_or(""), &mut out);
            if line.contains(']') {
                collecting = false;
            }
            continue;
        }
        if line.starts_with('[') {
            in_workspace = line == "[workspace]";
            continue;
        }
        if in_workspace {
            if let Some((key, value)) = line.split_once('=') {
                if key.trim() == "members" {
                    collect_quoted(value, &mut out);
                    collecting = value.contains('[') && !value.contains(']');
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOCK: &str = r#"# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "linkscope",
 "local-util",
 "serde",
 "syn 2.0.1",
]

[[package]]
name = "linkscope"
version = "0.1.0"
source = "git+https://github.com/Miasin-Labs/linkscope?rev=3c76713#3c76713b531a5bf75d41e89ce132a33f42f31f5f"

[[package]]
name = "local-util"
version = "0.2.0"
dependencies = ["itoa"]

[[package]]
name = "itoa"
version = "1.0.15"
source = "sparse+https://index.crates.io/"
checksum = "abc"

[[package]]
name = "serde"
version = "1.0.219"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "serde_derive",
]

[[package]]
name = "serde_derive"
version = "1.0.219"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "syn"
version = "1.0.109"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "syn"
version = "2.0.1"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[patch.unused]]
name = "ghost"
version = "9.9.9"
"#;

    fn find<'a>(deps: &'a [ResolvedDep], name: &str, version: &str) -> &'a ResolvedDep {
        deps.iter()
            .find(|d| d.key.name == name && d.lock_version == version)
            .unwrap_or_else(|| panic!("{name} {version} missing"))
    }

    #[test]
    fn parses_sources_versions_and_direct_flags() {
        let members = HashSet::from(["app".to_string()]);
        let deps = parse(LOCK, "Cargo.lock", &members);
        assert!(
            deps.iter().all(|d| d.key.name != "app"),
            "workspace member skipped"
        );
        assert!(
            deps.iter().all(|d| d.key.name != "ghost"),
            "patch.unused skipped"
        );
        assert_eq!(deps.len(), 7);

        let serde = find(&deps, "serde", "1.0.219");
        assert_eq!(serde.source, DepSource::Registry);
        assert_eq!(serde.direct, Some(true));
        assert_eq!(find(&deps, "serde_derive", "1.0.219").direct, Some(false));
        assert_eq!(
            find(&deps, "itoa", "1.0.15").direct,
            Some(true),
            "via path dep"
        );
        assert_eq!(find(&deps, "syn", "2.0.1").direct, Some(true));
        assert_eq!(find(&deps, "syn", "1.0.109").direct, Some(false));

        let git = find(&deps, "linkscope", "0.1.0");
        assert_eq!(
            git.source,
            DepSource::Git {
                url: "https://github.com/Miasin-Labs/linkscope".into(),
                rev: "3c76713b531a5bf75d41e89ce132a33f42f31f5f".into(),
            }
        );
        assert_eq!(git.key.version, "0.1.0+git.3c76713b531a");

        let local = find(&deps, "local-util", "0.2.0");
        assert_eq!(local.source, DepSource::Path { path: None });
        assert_eq!(local.direct, None);
    }

    #[test]
    fn reads_workspace_member_names() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\n  \"analysis\", # the lib\n  \"crates/*\",\n]\n\n[package]\nname = \"root-app\"\nversion = \"1.0.0\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("analysis")).unwrap();
        fs::write(
            root.join("analysis/Cargo.toml"),
            "[package]\nname = \"analysis-lib\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("crates/one")).unwrap();
        fs::write(
            root.join("crates/one/Cargo.toml"),
            "[package]\nname = \"one\"\n",
        )
        .unwrap();
        let names = workspace_members(root);
        assert_eq!(
            names,
            HashSet::from(["root-app".into(), "analysis-lib".into(), "one".into()])
        );
    }

    #[test]
    fn inline_dependency_arrays_and_v1_metadata_parse() {
        let text = "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nsource = \"registry+x\"\ndependencies = [\"b\", \"c 2.0.0 (registry+x)\"]\n\n[metadata]\n\"checksum a 1.0.0\" = \"zz\"\n";
        let deps = parse(text, "Cargo.lock", &HashSet::new());
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].key.name, "a");
        assert_eq!(deps[0].direct, Some(false));
    }
}
