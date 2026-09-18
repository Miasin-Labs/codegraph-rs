//! The crates a Rust workspace depends on directly, read from `Cargo.lock`.
//!
//! Packages without a `source` are the workspace's own members (and path
//! dependencies, which are indexed as project source). Every registry or
//! git package one of them lists under `dependencies` is a direct
//! dependency: its public methods can be called on values the project
//! holds. Transitive dependencies are left out — their types reach the
//! project only through a direct dependency's API — except those a
//! dependency re-exports (`clap` is `pub use clap_builder::*`), which
//! [`Lock::dependency_named`] finds.

use std::collections::BTreeSet;

/// One package `Cargo.lock` pins.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct LockedCrate {
    pub(crate) name: String,
    pub(crate) version: String,
    /// `registry+…`, `sparse+…`, or `git+…#<commit>`; `None` for workspace
    /// members and path dependencies.
    pub(crate) source: Option<String>,
}

impl LockedCrate {
    /// The commit a git dependency is pinned to.
    pub(crate) fn git_commit(&self) -> Option<&str> {
        let source = self.source.as_deref()?.strip_prefix("git+")?;
        let (_, commit) = source.rsplit_once('#')?;
        (commit.len() >= 7 && commit.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(commit)
    }
}

#[derive(Debug, Default)]
struct Package {
    krate: Option<LockedCrate>,
    dependencies: Vec<String>,
}

/// A parsed `Cargo.lock`.
#[derive(Debug, Default)]
pub(crate) struct Lock {
    packages: Vec<Package>,
}

impl Lock {
    pub(crate) fn parse(text: &str) -> Lock {
        Lock {
            packages: packages(text),
        }
    }

    /// The registry and git crates the workspace members depend on
    /// directly, sorted and deduplicated.
    pub(crate) fn direct_dependencies(&self) -> Vec<LockedCrate> {
        let mut direct = BTreeSet::new();
        for member in self.packages.iter().filter(|package| {
            package
                .krate
                .as_ref()
                .is_some_and(|krate| krate.source.is_none())
        }) {
            for entry in &member.dependencies {
                if let Some(krate) = self.resolve_entry(entry) {
                    if krate.source.is_some() {
                        direct.insert(krate.clone());
                    }
                }
            }
        }
        direct.into_iter().collect()
    }

    /// The dependency of `krate` that code names `ident` (`clap_builder`
    /// for the package `clap_builder` or `clap-builder`).
    pub(crate) fn dependency_named(&self, krate: &LockedCrate, ident: &str) -> Option<LockedCrate> {
        let package = self
            .packages
            .iter()
            .find(|package| package.krate.as_ref() == Some(krate))?;
        package
            .dependencies
            .iter()
            .filter_map(|entry| self.resolve_entry(entry))
            .find(|dependency| dependency.name.replace('-', "_") == ident)
            .filter(|dependency| dependency.source.is_some())
            .cloned()
    }

    /// A `dependencies` entry: `name`, `name version`, or
    /// `name version (source)`. A bare name is only written when the lock
    /// holds one version of it.
    fn resolve_entry(&self, entry: &str) -> Option<&LockedCrate> {
        let mut parts = entry.split_whitespace();
        let name = parts.next()?;
        let version = parts.next();
        self.packages
            .iter()
            .filter_map(|package| package.krate.as_ref())
            .find(|krate| krate.name == name && version.is_none_or(|v| krate.version == v))
    }
}

fn packages(lock: &str) -> Vec<Package> {
    let mut packages = Vec::new();
    let mut current: Option<Package> = None;
    let mut fields: (Option<String>, Option<String>, Option<String>) = (None, None, None);
    let mut in_dependencies = false;
    let finish = |current: &mut Option<Package>,
                  fields: &mut (Option<String>, Option<String>, Option<String>),
                  packages: &mut Vec<Package>| {
        if let Some(mut package) = current.take() {
            if let (Some(name), Some(version)) = (fields.0.take(), fields.1.take()) {
                package.krate = Some(LockedCrate {
                    name,
                    version,
                    source: fields.2.take(),
                });
            }
            packages.push(package);
        }
        *fields = (None, None, None);
    };
    for line in lock.lines() {
        let line = line.trim();
        if line.starts_with('[') && !in_dependencies {
            finish(&mut current, &mut fields, &mut packages);
            if line == "[[package]]" {
                current = Some(Package::default());
            }
            continue;
        }
        let Some(package) = current.as_mut() else {
            continue;
        };
        if in_dependencies {
            if line.starts_with(']') {
                in_dependencies = false;
            } else if let Some(entry) = quoted(line.trim_end_matches(',')) {
                package.dependencies.push(entry.to_string());
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key.trim() {
            "name" => fields.0 = quoted(value.trim()).map(str::to_string),
            "version" => fields.1 = quoted(value.trim()).map(str::to_string),
            "source" => fields.2 = quoted(value.trim()).map(str::to_string),
            "dependencies" => {
                let value = value.trim();
                in_dependencies = value == "[";
                if let Some(inline) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
                    package.dependencies.extend(
                        inline
                            .split(',')
                            .filter_map(|entry| quoted(entry.trim()))
                            .map(str::to_string),
                    );
                }
            }
            _ => {}
        }
    }
    finish(&mut current, &mut fields, &mut packages);
    packages
}

fn quoted(text: &str) -> Option<&str> {
    text.strip_prefix('"')?.strip_suffix('"')
}

#[cfg(test)]
mod tests {
    use super::Lock;

    fn direct_dependencies(text: &str) -> Vec<super::LockedCrate> {
        Lock::parse(text).direct_dependencies()
    }

    const LOCK: &str = r#"
# This file is automatically @generated by Cargo.
version = 4

[[package]]
name = "app"
version = "0.1.0"
dependencies = [
 "helper",
 "serde_json",
 "tree-sitter 0.26.9",
 "patched",
]

[[package]]
name = "helper"
version = "0.1.0"
dependencies = ["regex"]

[[package]]
name = "itoa"
version = "1.0.15"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "abc"

[[package]]
name = "patched"
version = "0.2.0"
source = "git+https://github.com/o/patched?rev=1234567#1234567890abcdef1234567890abcdef12345678"

[[package]]
name = "regex"
version = "1.11.1"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "serde_json"
version = "1.0.140"
source = "registry+https://github.com/rust-lang/crates.io-index"
dependencies = [
 "itoa",
]

[[package]]
name = "tree-sitter"
version = "0.20.99"

[[package]]
name = "tree-sitter"
version = "0.26.9"
source = "registry+https://github.com/rust-lang/crates.io-index"
"#;

    #[test]
    fn members_direct_registry_and_git_dependencies() {
        let direct: Vec<String> = direct_dependencies(LOCK)
            .into_iter()
            .map(|krate| format!("{} {}", krate.name, krate.version))
            .collect();
        // `helper` is a member (no source); `itoa` is transitive.
        assert_eq!(
            direct,
            [
                "patched 0.2.0",
                "regex 1.11.1",
                "serde_json 1.0.140",
                "tree-sitter 0.26.9"
            ]
        );
        let patched = &direct_dependencies(LOCK)[0];
        assert_eq!(
            patched.git_commit(),
            Some("1234567890abcdef1234567890abcdef12345678")
        );
    }

    /// A facade crate's re-exported dependency is found by the name code
    /// writes for it.
    #[test]
    fn finds_a_dependency_by_its_code_name() {
        let lock = Lock::parse(LOCK);
        let serde_json = direct_dependencies(LOCK)
            .into_iter()
            .find(|krate| krate.name == "serde_json")
            .unwrap();
        let itoa = lock.dependency_named(&serde_json, "itoa").unwrap();
        assert_eq!(
            (itoa.name.as_str(), itoa.version.as_str()),
            ("itoa", "1.0.15")
        );
        assert_eq!(lock.dependency_named(&serde_json, "regex"), None);
    }

    #[test]
    fn an_empty_or_foreign_file_has_no_dependencies() {
        assert!(direct_dependencies("").is_empty());
        assert!(direct_dependencies("[workspace]\nmembers = []\n").is_empty());
    }
}
