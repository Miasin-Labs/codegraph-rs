//! `pnpm-lock.yaml` (lockfile formats 5.x, 6.x and 9.x).
//!
//! Read with an indentation scanner, not a YAML parser: pnpm writes a fixed
//! two-space layout. `packages:` keys name every resolved package
//! (`/name/1.0.0` in v5, `/name@1.0.0(peer)` in v6, `name@1.0.0` in v9);
//! `importers:` (or v5/v6 top-level `dependencies:`) name the direct ones.

use std::collections::HashSet;

use super::js::{npm_source, split_name_version};
use crate::deps::model::{DepKey, DepSource, Ecosystem, ResolvedDep};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Importers,
    Packages,
    /// v5/v6 single-project top-level `dependencies:`-style groups.
    RootDependencies,
    Other,
}

#[derive(Debug, Default)]
struct PackageEntry {
    name: String,
    /// Version from the key, or the entry's own `version:` for URL keys.
    version: String,
    /// The key's version part (a URL for tarball/git keys).
    key_version: String,
    resolution: Option<String>,
}

pub(crate) fn parse(text: &str, lockfile: &str) -> Vec<ResolvedDep> {
    let mut major = 9u32;
    let mut section = Section::Other;
    let mut packages: Vec<PackageEntry> = Vec::new();
    // (name, version as written in the importer, stripped of peer suffixes)
    let mut direct: HashSet<(String, String)> = HashSet::new();
    let mut links: Vec<(String, String)> = Vec::new();
    let mut in_dep_group = false;
    let mut current_dep: Option<String> = None;

    for raw in text.lines() {
        if raw.trim().is_empty() || raw.trim_start().starts_with('#') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if indent == 0 {
            let key = line.trim_end_matches(':');
            if let Some(v) = line.strip_prefix("lockfileVersion:") {
                major = parse_major(v);
            }
            section = match key {
                "importers" => Section::Importers,
                "packages" => Section::Packages,
                "dependencies" | "devDependencies" | "optionalDependencies" => {
                    in_dep_group = true;
                    Section::RootDependencies
                }
                _ => Section::Other,
            };
            current_dep = None;
            continue;
        }
        match section {
            Section::Importers => match indent {
                2 => in_dep_group = false,
                4 => {
                    in_dep_group = matches!(
                        line.trim_end_matches(':'),
                        "dependencies" | "devDependencies" | "optionalDependencies"
                    );
                    current_dep = None;
                }
                6 if in_dep_group => current_dep = Some(unquote(line.trim_end_matches(':'))),
                8 => {
                    if let (Some(name), Some(version)) = (&current_dep, field(line, "version")) {
                        note_direct(name, &version, major, &mut direct, &mut links);
                    }
                }
                _ => {}
            },
            Section::RootDependencies if in_dep_group => match indent {
                2 => {
                    let (name, inline) = match line.split_once(':') {
                        Some((name, rest)) => (unquote(name), unquote(rest.trim())),
                        None => (unquote(line), String::new()),
                    };
                    if inline.is_empty() {
                        current_dep = Some(name);
                    } else {
                        note_direct(&name, &inline, major, &mut direct, &mut links);
                        current_dep = None;
                    }
                }
                4 => {
                    if let (Some(name), Some(version)) = (&current_dep, field(line, "version")) {
                        note_direct(name, &version, major, &mut direct, &mut links);
                    }
                }
                _ => {}
            },
            Section::Packages => match indent {
                2 => {
                    if let Some(entry) = parse_package_key(line.trim_end_matches(':'), major) {
                        packages.push(entry);
                    }
                }
                4 => {
                    let Some(entry) = packages.last_mut() else {
                        continue;
                    };
                    if let Some(version) = field(line, "version") {
                        if is_url_like(&entry.key_version) {
                            entry.version = version;
                        }
                    } else if let Some(resolution) = line.strip_prefix("resolution:") {
                        entry.resolution = Some(resolution.trim().to_string());
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }

    let mut out: Vec<ResolvedDep> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for entry in packages {
        if !seen.insert((entry.name.clone(), entry.version.clone())) {
            continue;
        }
        let source = entry_source(&entry);
        let is_direct = direct.contains(&(entry.name.clone(), entry.key_version.clone()));
        out.push(ResolvedDep {
            key: DepKey::for_source(Ecosystem::Npm, &entry.name, &entry.version, &source),
            lock_version: entry.version,
            direct: match source {
                DepSource::Path { .. } => None,
                _ => Some(is_direct),
            },
            source,
            lockfile: lockfile.to_string(),
            install_path: None,
        });
    }
    for (name, path) in links {
        let source = DepSource::Path { path: Some(path) };
        out.push(ResolvedDep {
            key: DepKey::for_source(Ecosystem::Npm, &name, "0.0.0", &source),
            lock_version: "0.0.0".to_string(),
            source,
            direct: None,
            lockfile: lockfile.to_string(),
            install_path: None,
        });
    }
    out
}

fn parse_major(value: &str) -> u32 {
    unquote(value.trim())
        .split('.')
        .next()
        .and_then(|m| m.parse().ok())
        .unwrap_or(9)
}

fn note_direct(
    name: &str,
    version: &str,
    major: u32,
    direct: &mut HashSet<(String, String)>,
    links: &mut Vec<(String, String)>,
) {
    if let Some(path) = version
        .strip_prefix("link:")
        .or_else(|| version.strip_prefix("file:"))
    {
        links.push((name.to_string(), path.to_string()));
        return;
    }
    direct.insert((
        name.to_string(),
        strip_peer_suffix(version, major).to_string(),
    ));
}

/// `1.0.0(peer@2)(x@1)` → `1.0.0`; v5 `1.0.0_peer@2` → `1.0.0`.
fn strip_peer_suffix(version: &str, major: u32) -> &str {
    let cut = if major <= 5 && !is_url_like(version) {
        version.find('_')
    } else {
        version.find('(')
    };
    cut.map_or(version, |i| &version[..i])
}

/// A version that is really a source spec (`https://…`, `github:…`,
/// `link:…`) rather than a registry version (`npm:` aliases excepted).
fn is_url_like(version: &str) -> bool {
    version.contains("://") || (version.contains(':') && !version.starts_with("npm:"))
}

fn parse_package_key(key: &str, major: u32) -> Option<PackageEntry> {
    let key = unquote(key);
    let key = key.strip_prefix('/').unwrap_or(&key);
    let (name, version) = if major <= 5 {
        let (name, version) = key.rsplit_once('/')?;
        (
            name.to_string(),
            strip_peer_suffix(version, major).to_string(),
        )
    } else {
        let (name, version) = split_name_version(key)?;
        (
            name.to_string(),
            strip_peer_suffix(version, major).to_string(),
        )
    };
    Some(PackageEntry {
        name,
        version: if is_url_like(&version) {
            "0.0.0".to_string()
        } else {
            version.clone()
        },
        key_version: version,
        resolution: None,
    })
}

fn entry_source(entry: &PackageEntry) -> DepSource {
    if let Some(resolution) = &entry.resolution {
        let fields = flow_map(resolution);
        let get = |k: &str| {
            fields
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        if let (Some(repo), Some(commit)) = (get("repo"), get("commit")) {
            return DepSource::Git {
                url: repo.to_string(),
                rev: commit.to_string(),
            };
        }
        if let Some(directory) = get("directory") {
            return DepSource::Path {
                path: Some(directory.to_string()),
            };
        }
        if let Some(tarball) = get("tarball") {
            let source = npm_source("", Some(tarball));
            if source != DepSource::Registry {
                return source;
            }
        }
    }
    if is_url_like(&entry.key_version) {
        return npm_source(&entry.key_version, None);
    }
    DepSource::Registry
}

/// `{a: 1, b: x}` → `[(a, 1), (b, x)]`.
fn flow_map(text: &str) -> Vec<(String, String)> {
    let inner = text.trim().trim_start_matches('{').trim_end_matches('}');
    inner
        .split(", ")
        .filter_map(|pair| {
            let (k, v) = pair.split_once(": ")?;
            Some((unquote(k.trim()), unquote(v.trim())))
        })
        .collect()
}

/// `key: value` on one line → `value`.
fn field(line: &str, key: &str) -> Option<String> {
    let rest = line.strip_prefix(key)?.strip_prefix(':')?;
    let value = unquote(rest.trim());
    (!value.is_empty()).then_some(value)
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    s.strip_prefix('\'')
        .and_then(|r| r.strip_suffix('\''))
        .or_else(|| s.strip_prefix('"').and_then(|r| r.strip_suffix('"')))
        .unwrap_or(s)
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(deps: &'a [ResolvedDep], name: &str) -> &'a ResolvedDep {
        deps.iter()
            .find(|d| d.key.name == name)
            .unwrap_or_else(|| panic!("{name} missing from {deps:#?}"))
    }

    #[test]
    fn v9_reads_importers_packages_and_url_keys() {
        let text = "lockfileVersion: '9.0'\n\nimporters:\n\n  .:\n    dependencies:\n      arrpc:\n        specifier: github:Vencord/arrpc#1dc801e8\n        version: https://codeload.github.com/Vencord/arrpc/tar.gz/1dc801e8\n      electron-updater:\n        specifier: 6.6.2\n        version: 6.6.2(patch_hash=08)(supports-color@7.2.0)\n      local-lib:\n        specifier: workspace:*\n        version: link:packages/local-lib\n    devDependencies:\n      '@types/node':\n        specifier: ^20\n        version: 20.1.0\n\npackages:\n\n  '@types/node@20.1.0':\n    resolution: {integrity: sha512-abc==}\n\n  arrpc@https://codeload.github.com/Vencord/arrpc/tar.gz/1dc801e8:\n    resolution: {tarball: https://codeload.github.com/Vencord/arrpc/tar.gz/1dc801e8}\n    version: 3.5.0\n\n  electron-updater@6.6.2:\n    resolution: {integrity: sha512-x==}\n\n  lodash@4.17.21:\n    resolution: {integrity: sha512-y==}\n\nsnapshots:\n\n  electron-updater@6.6.2(supports-color@7.2.0):\n    dependencies:\n      lodash: 4.17.21\n";
        let deps = parse(text, "pnpm-lock.yaml");
        assert_eq!(deps.len(), 5);
        assert_eq!(find(&deps, "@types/node").direct, Some(true));
        assert_eq!(find(&deps, "electron-updater").direct, Some(true));
        assert_eq!(find(&deps, "lodash").direct, Some(false));
        let arrpc = find(&deps, "arrpc");
        assert_eq!(arrpc.lock_version, "3.5.0");
        assert_eq!(arrpc.direct, Some(true));
        assert!(matches!(arrpc.source, DepSource::Git { .. }));
        assert_eq!(arrpc.key.version, "3.5.0+git.1dc801e8");
        assert!(matches!(
            find(&deps, "local-lib").source,
            DepSource::Path { .. }
        ));
    }

    #[test]
    fn v6_and_v5_keys() {
        let v6 = "lockfileVersion: '6.0'\n\ndependencies:\n  react:\n    specifier: ^18\n    version: 18.2.0\n\npackages:\n\n  /react@18.2.0:\n    resolution: {integrity: sha512-a==}\n\n  /@scope/x@1.0.0(react@18.2.0):\n    resolution: {integrity: sha512-b==}\n";
        let deps = parse(v6, "pnpm-lock.yaml");
        assert_eq!(find(&deps, "react").direct, Some(true));
        assert_eq!(find(&deps, "@scope/x").lock_version, "1.0.0");

        let v5 = "lockfileVersion: 5.4\n\nspecifiers:\n  react: ^18\n\ndependencies:\n  react: 18.2.0\n\npackages:\n\n  /react/18.2.0:\n    resolution: {integrity: sha512-a==}\n\n  /@scope/x/1.0.0_react@18.2.0:\n    resolution: {integrity: sha512-b==}\n";
        let deps = parse(v5, "pnpm-lock.yaml");
        assert_eq!(find(&deps, "react").direct, Some(true));
        let scoped = find(&deps, "@scope/x");
        assert_eq!(scoped.lock_version, "1.0.0");
        assert_eq!(scoped.direct, Some(false));
    }

    #[test]
    fn git_resolution_maps() {
        let text = "lockfileVersion: '9.0'\n\npackages:\n\n  thing@git+https://github.com/u/thing.git#abc123:\n    resolution: {commit: abc123, repo: https://github.com/u/thing.git, type: git}\n    version: 1.2.3\n";
        let deps = parse(text, "pnpm-lock.yaml");
        assert_eq!(
            find(&deps, "thing").source,
            DepSource::Git {
                url: "https://github.com/u/thing.git".into(),
                rev: "abc123".into()
            }
        );
    }
}
