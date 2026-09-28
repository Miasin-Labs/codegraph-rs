//! The population the beliefs are mined from: the crates already in the
//! cargo cache (`$CARGO_HOME/registry/src/*/<name>-<version>/`), each a user
//! of the library APIs it depends on. Read-only and offline — a dependency
//! is resolved to a version the cache holds, never fetched.
//!
//! One version per crate name (the newest): several releases of one crate
//! are mostly the same code, and counting them apart would let one author
//! vote many times.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use toml::de::{DeTable, DeValue};

/// A crate in the cache and what it depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheCrate {
    pub name: String,
    pub version: String,
    pub dir: PathBuf,
    /// Normal (and target-specific) dependencies: package name and the
    /// version requirement as written. Dev and build dependencies are left
    /// out: library code does not call them.
    pub deps: Vec<DepReq>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepReq {
    pub package: String,
    pub req: String,
}

/// Every crate under `registry_dirs`, with its manifest parsed (crates whose
/// manifest cannot be read are left out), path-ordered.
pub fn scan(registry_dirs: &[PathBuf]) -> Vec<CacheCrate> {
    let mut crates = Vec::new();
    for index in registry_dirs {
        let Ok(entries) = fs::read_dir(index) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_dir())
            .collect();
        dirs.sort();
        for dir in dirs {
            let Ok(text) = fs::read_to_string(dir.join("Cargo.toml")) else {
                continue;
            };
            if let Some(krate) = parse_manifest(&dir, &text) {
                crates.push(krate);
            }
        }
    }
    crates
}

/// A published (normalized) `Cargo.toml`.
pub fn parse_manifest(dir: &Path, text: &str) -> Option<CacheCrate> {
    let doc = DeTable::parse(text).ok()?;
    let doc = doc.get_ref();
    let package = table(doc, "package")?;
    let name = string(package, "name")?.to_string();
    let version = string(package, "version")?.to_string();
    let mut tables: Vec<&DeTable<'_>> = table(doc, "dependencies").into_iter().collect();
    if let Some(targets) = table(doc, "target") {
        for (_, target) in targets.iter() {
            if let DeValue::Table(target) = target.get_ref() {
                tables.extend(table(target, "dependencies"));
            }
        }
    }
    let mut deps: BTreeMap<String, DepReq> = BTreeMap::new();
    for deps_table in tables {
        for (key, value) in deps_table.iter() {
            let key = key.get_ref().to_string();
            let (package, req) = match value.get_ref() {
                DeValue::String(req) => (key.clone(), req.to_string()),
                DeValue::Table(dep) => {
                    if string(dep, "path").is_some() && string(dep, "version").is_none() {
                        continue;
                    }
                    let package = string(dep, "package").map_or(key.clone(), str::to_string);
                    (package, string(dep, "version").unwrap_or("*").to_string())
                }
                _ => continue,
            };
            deps.entry(package.clone())
                .or_insert(DepReq { package, req });
        }
    }
    Some(CacheCrate {
        name,
        version,
        dir: dir.to_path_buf(),
        deps: deps.into_values().collect(),
    })
}

/// The newest version of each crate name, by name.
pub fn newest_per_name(crates: &[CacheCrate]) -> Vec<CacheCrate> {
    let mut newest: BTreeMap<&str, &CacheCrate> = BTreeMap::new();
    for krate in crates {
        let keep = newest
            .get(krate.name.as_str())
            .is_none_or(|kept| parse_version(&krate.version) > parse_version(&kept.version));
        if keep {
            newest.insert(&krate.name, krate);
        }
    }
    newest.into_values().cloned().collect()
}

/// Every cached version of each crate name.
pub fn versions_by_name(crates: &[CacheCrate]) -> HashMap<String, Vec<(String, PathBuf)>> {
    let mut by_name: HashMap<String, Vec<(String, PathBuf)>> = HashMap::new();
    for krate in crates {
        by_name
            .entry(krate.name.clone())
            .or_default()
            .push((krate.version.clone(), krate.dir.clone()));
    }
    by_name
}

/// `major.minor.patch` and whether it is a pre-release (ordered below the
/// release).
type Version = (u64, u64, u64, bool);

fn parse_version(version: &str) -> Version {
    let (core, pre) = match version.split_once('-') {
        Some((core, _)) => (core, true),
        None => (version.split('+').next().unwrap_or(version), false),
    };
    let mut parts = core
        .split('.')
        .map(|p| p.trim().parse::<u64>().unwrap_or(0));
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        !pre,
    )
}

/// The newest of `available` a Cargo requirement `req` accepts: caret
/// (default), `~`, `=`, `>=`/`>`, `<`/`<=` bounds joined by `,`, and `*`.
/// Pre-releases only when the requirement names one.
pub fn choose_version<'a>(
    req: &str,
    available: &'a [(String, PathBuf)],
) -> Option<&'a (String, PathBuf)> {
    let wants_pre = req.contains('-');
    available
        .iter()
        .filter(|(version, _)| wants_pre || !version.contains('-'))
        .filter(|(version, _)| matches_req(req, version))
        .max_by_key(|(version, _)| parse_version(version))
}

fn matches_req(req: &str, version: &str) -> bool {
    let v = parse_version(version);
    req.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .all(|part| matches_one(part, v))
}

fn matches_one(part: &str, v: Version) -> bool {
    let (op, rest) = ["<=", ">=", "=", "<", ">", "^", "~"]
        .iter()
        .find_map(|op| part.strip_prefix(op).map(|rest| (*op, rest.trim())))
        .unwrap_or(("^", part));
    if rest == "*" || rest.is_empty() {
        return true;
    }
    let fields: Vec<&str> = rest.split(['.', '-', '+']).collect();
    let num = |i: usize| -> Option<u64> {
        fields
            .get(i)
            .filter(|f| **f != "*" && **f != "x")
            .and_then(|f| f.parse().ok())
    };
    let (major, minor, patch) = (num(0).unwrap_or(0), num(1), num(2));
    let lower = (major, minor.unwrap_or(0), patch.unwrap_or(0));
    let at = (v.0, v.1, v.2);
    match op {
        "=" => v.0 == major && minor.is_none_or(|m| v.1 == m) && patch.is_none_or(|p| v.2 == p),
        ">=" => at >= lower,
        ">" => at > lower,
        "<" => at < lower,
        "<=" => at <= lower,
        "~" => at >= lower && v.0 == major && minor.is_none_or(|m| v.1 == m),
        _ => {
            // Caret: the leftmost non-zero field stays.
            at >= lower
                && if major > 0 {
                    v.0 == major
                } else if minor.unwrap_or(0) > 0 || patch.is_none() {
                    v.0 == 0 && minor.is_none_or(|m| v.1 == m)
                } else {
                    at == lower
                }
        }
    }
}

fn table<'a, 'i>(parent: &'a DeTable<'i>, key: &str) -> Option<&'a DeTable<'i>> {
    parent.iter().find_map(|(k, v)| match v.get_ref() {
        DeValue::Table(t) if k.get_ref() == key => Some(t),
        _ => None,
    })
}

fn string<'a>(parent: &'a DeTable<'_>, key: &str) -> Option<&'a str> {
    parent.iter().find_map(|(k, v)| match v.get_ref() {
        DeValue::String(s) if k.get_ref() == key => Some(s.as_ref()),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(list: &[&str]) -> Vec<(String, PathBuf)> {
        list.iter()
            .map(|v| (v.to_string(), PathBuf::from(format!("/c/x-{v}"))))
            .collect()
    }

    #[test]
    fn requirements_pick_the_newest_compatible_cached_version() {
        let available = versions(&[
            "0.11.27",
            "0.12.4",
            "0.12.9",
            "1.0.0-rc.1",
            "1.2.0",
            "2.0.1",
        ]);
        let pick = |req| choose_version(req, &available).map(|(v, _)| v.as_str());
        assert_eq!(pick("0.12"), Some("0.12.9"));
        assert_eq!(pick("^0.11.3"), Some("0.11.27"));
        assert_eq!(pick("1"), Some("1.2.0"));
        assert_eq!(pick("~0.12.4"), Some("0.12.9"));
        assert_eq!(pick("=0.12.4"), Some("0.12.4"));
        assert_eq!(pick(">=1.0, <2"), Some("1.2.0"));
        assert_eq!(pick("*"), Some("2.0.1"));
        assert_eq!(pick("3"), None, "never a version the cache lacks");
    }

    #[test]
    fn a_published_manifest_lists_normal_and_target_dependencies() {
        let text = r#"
[package]
name = "demo"
version = "0.3.1"

[dependencies.reqwest]
version = "0.12"
optional = true

[dependencies.renamed]
version = "1"
package = "serde"

[dependencies]
log = "0.4"

[dev-dependencies.tokio]
version = "1"

[target."cfg(unix)".dependencies.libc]
version = "0.2.150"
"#;
        let krate = parse_manifest(Path::new("/c/demo-0.3.1"), text).unwrap();
        assert_eq!(
            (krate.name.as_str(), krate.version.as_str()),
            ("demo", "0.3.1")
        );
        let deps: Vec<(&str, &str)> = krate
            .deps
            .iter()
            .map(|d| (d.package.as_str(), d.req.as_str()))
            .collect();
        assert_eq!(
            deps,
            vec![
                ("libc", "0.2.150"),
                ("log", "0.4"),
                ("reqwest", "0.12"),
                ("serde", "1")
            ]
        );
    }

    #[test]
    fn one_version_per_crate_name() {
        let make = |name: &str, version: &str| CacheCrate {
            name: name.into(),
            version: version.into(),
            dir: PathBuf::from(format!("/c/{name}-{version}")),
            deps: Vec::new(),
        };
        let crates = vec![
            make("a", "1.0.9"),
            make("a", "1.0.10"),
            make("a", "2.0.0-beta.1"),
            make("b", "0.1.0"),
        ];
        let newest: Vec<String> = newest_per_name(&crates)
            .into_iter()
            .map(|c| format!("{}-{}", c.name, c.version))
            .collect();
        assert_eq!(newest, vec!["a-2.0.0-beta.1", "b-0.1.0"]);
    }
}
