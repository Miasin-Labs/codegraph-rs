//! `package-lock.json` / `npm-shrinkwrap.json`.
//!
//! v2/v3 (npm ≥ 7) list every installed package under `packages`, keyed by
//! its install path (`node_modules/a/node_modules/b`) — which is also where
//! its source is. v1 (npm 6) nests `dependencies`; it is read too, walked
//! with an explicit stack.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::js::{declared_dependency_names, npm_source};
use crate::deps::model::{DepKey, DepSource, Ecosystem, ResolvedDep};

/// Parse a package-lock at `lockfile` (relative to the project root; `dir`
/// is its directory, also relative, `""` for the root). `declared` are the
/// names the adjacent package.json depends on (v1 locks don't record them).
pub(crate) fn parse(
    text: &str,
    lockfile: &str,
    dir: &str,
    declared: &HashSet<String>,
) -> Result<Vec<ResolvedDep>, String> {
    let doc: Value = serde_json::from_str(text).map_err(|e| format!("{lockfile}: {e}"))?;
    let Some(root) = doc.as_object() else {
        return Err(format!("{lockfile}: not a JSON object"));
    };
    if let Some(packages) = root.get("packages").and_then(Value::as_object) {
        return Ok(parse_packages(packages, lockfile, dir));
    }
    let deps = root
        .get("dependencies")
        .and_then(Value::as_object)
        .map(|deps| parse_v1(deps, declared, lockfile, dir))
        .unwrap_or_default();
    Ok(deps)
}

fn parse_packages(packages: &Map<String, Value>, lockfile: &str, dir: &str) -> Vec<ResolvedDep> {
    // Direct: installed at the top level and declared by the root or a
    // workspace member (entries whose key is not under node_modules).
    let mut declared: HashSet<String> = HashSet::new();
    for (key, entry) in packages {
        if !key.starts_with("node_modules/") && !key.contains("/node_modules/") {
            if let Some(obj) = entry.as_object() {
                declared.extend(declared_dependency_names(obj));
            }
        }
    }

    let mut out = Vec::new();
    for (key, entry) in packages {
        let Some(install_name) = install_name(key) else {
            continue; // the root or a workspace member: the project itself
        };
        let Some(obj) = entry.as_object() else {
            continue;
        };
        let name = obj
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(install_name)
            .to_string();
        let resolved = obj.get("resolved").and_then(Value::as_str);
        let is_link = obj.get("link").and_then(Value::as_bool).unwrap_or(false);
        let version = obj.get("version").and_then(Value::as_str);
        let source = if is_link {
            DepSource::Path {
                path: resolved.map(str::to_string),
            }
        } else {
            npm_source(version.unwrap_or(""), resolved)
        };
        let version = match (version, &source) {
            (Some(v), _) => v.to_string(),
            (None, DepSource::Path { .. }) => "0.0.0".to_string(),
            (None, _) => continue,
        };
        let top_level =
            key.matches("node_modules/").count() == 1 && key.starts_with("node_modules/");
        let direct = match source {
            DepSource::Path { .. } => None,
            _ => Some(top_level && declared.contains(install_name)),
        };
        out.push(ResolvedDep {
            key: DepKey::for_source(Ecosystem::Npm, &name, &version, &source),
            lock_version: version,
            source,
            direct,
            lockfile: lockfile.to_string(),
            install_path: Some(join_dir(dir, key)),
        });
    }
    out
}

/// The package directory name a `packages` key installs (`node_modules/@a/b`
/// → `@a/b`); `None` for keys that aren't installs.
fn install_name(key: &str) -> Option<&str> {
    let idx = key.rfind("node_modules/")?;
    let name = &key[idx + "node_modules/".len()..];
    (!name.is_empty()).then_some(name)
}

fn join_dir(dir: &str, rel: &str) -> String {
    if dir.is_empty() {
        rel.to_string()
    } else {
        format!("{dir}/{rel}")
    }
}

/// npm 6 nesting: `dependencies.{name}.dependencies.{name}…`, each level one
/// `node_modules/` deeper.
fn parse_v1(
    deps: &Map<String, Value>,
    direct: &HashSet<String>,
    lockfile: &str,
    dir: &str,
) -> Vec<ResolvedDep> {
    let mut out = Vec::new();
    let mut stack: Vec<(String, &Map<String, Value>)> = vec![(String::new(), deps)];
    while let Some((prefix, level)) = stack.pop() {
        for (name, entry) in level {
            let Some(obj) = entry.as_object() else {
                continue;
            };
            let install = format!("{prefix}node_modules/{name}");
            if let Some(nested) = obj.get("dependencies").and_then(Value::as_object) {
                stack.push((format!("{install}/"), nested));
            }
            let Some(version) = obj.get("version").and_then(Value::as_str) else {
                continue;
            };
            let source = npm_source(version, obj.get("resolved").and_then(Value::as_str));
            let version = match &source {
                // v1 writes the spec (`github:u/r#sha`, `file:../x`) as the version.
                DepSource::Registry => version.to_string(),
                _ => "0.0.0".to_string(),
            };
            let top_level = prefix.is_empty();
            out.push(ResolvedDep {
                key: DepKey::for_source(Ecosystem::Npm, name, &version, &source),
                lock_version: version,
                direct: match source {
                    DepSource::Path { .. } => None,
                    _ => Some(top_level && direct.contains(name)),
                },
                source,
                lockfile: lockfile.to_string(),
                install_path: Some(join_dir(dir, &install)),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const V3: &str = r#"{
  "name": "app", "version": "1.0.0", "lockfileVersion": 3, "requires": true,
  "packages": {
    "": { "name": "app", "dependencies": { "left-pad": "^1.3.0", "@types/node": "^20", "gitdep": "github:u/gitdep" },
          "devDependencies": { "typescript": "^5" }, "workspaces": ["packages/*"] },
    "node_modules/left-pad": { "version": "1.3.0", "resolved": "https://registry.npmjs.org/left-pad/-/left-pad-1.3.0.tgz" },
    "node_modules/@types/node": { "version": "20.1.0", "dev": true },
    "node_modules/typescript": { "version": "5.4.5", "dev": true },
    "node_modules/chalk": { "version": "5.0.0" },
    "node_modules/chalk/node_modules/left-pad": { "version": "1.0.0" },
    "node_modules/gitdep": { "version": "2.0.0", "resolved": "git+ssh://git@github.com/u/gitdep.git#0123456789abcdef0123" },
    "node_modules/ws-lib": { "resolved": "packages/ws-lib", "link": true },
    "node_modules/alias": { "name": "real-name", "version": "3.0.0" },
    "packages/ws-lib": { "name": "ws-lib", "version": "0.0.1", "dependencies": { "chalk": "^5" } }
  }
}"#;

    fn find<'a>(deps: &'a [ResolvedDep], name: &str, version: &str) -> &'a ResolvedDep {
        deps.iter()
            .find(|d| d.key.name == name && d.lock_version == version)
            .unwrap_or_else(|| panic!("{name}@{version} missing from {deps:#?}"))
    }

    #[test]
    fn v3_lists_installs_with_paths_sources_and_direct_flags() {
        let deps = parse(V3, "web/package-lock.json", "web", &HashSet::new()).unwrap();
        assert_eq!(deps.len(), 8);
        let pad = find(&deps, "left-pad", "1.3.0");
        assert_eq!(pad.direct, Some(true));
        assert_eq!(
            pad.install_path.as_deref(),
            Some("web/node_modules/left-pad")
        );
        let nested = find(&deps, "left-pad", "1.0.0");
        assert_eq!(nested.direct, Some(false));
        assert_eq!(
            nested.install_path.as_deref(),
            Some("web/node_modules/chalk/node_modules/left-pad")
        );
        assert_eq!(find(&deps, "typescript", "5.4.5").direct, Some(true));
        assert_eq!(find(&deps, "@types/node", "20.1.0").direct, Some(true));
        assert_eq!(
            find(&deps, "chalk", "5.0.0").direct,
            Some(true),
            "declared by a workspace"
        );
        let git = find(&deps, "gitdep", "2.0.0");
        assert_eq!(
            git.source,
            DepSource::Git {
                url: "ssh://git@github.com/u/gitdep.git".into(),
                rev: "0123456789abcdef0123".into()
            }
        );
        assert_eq!(
            find(&deps, "ws-lib", "0.0.0").source,
            DepSource::Path {
                path: Some("packages/ws-lib".into())
            }
        );
        let alias = find(&deps, "real-name", "3.0.0");
        assert_eq!(
            alias.install_path.as_deref(),
            Some("web/node_modules/alias")
        );
    }

    #[test]
    fn v1_nesting_is_walked() {
        let text = r#"{ "name": "old", "lockfileVersion": 1,
          "dependencies": {
            "a": { "version": "1.0.0", "dependencies": { "b": { "version": "2.0.0" } } },
            "c": { "version": "file:../c" }
          } }"#;
        let declared = HashSet::from(["a".to_string()]);
        let deps = parse(text, "package-lock.json", "", &declared).unwrap();
        assert_eq!(deps.len(), 3);
        let b = find(&deps, "b", "2.0.0");
        assert_eq!(
            b.install_path.as_deref(),
            Some("node_modules/a/node_modules/b")
        );
        assert_eq!(b.direct, Some(false));
        assert_eq!(find(&deps, "a", "1.0.0").direct, Some(true));
        assert!(matches!(
            find(&deps, "c", "0.0.0").source,
            DepSource::Path { .. }
        ));
    }

    #[test]
    fn rejects_non_json() {
        assert!(parse("nope", "package-lock.json", "", &HashSet::new()).is_err());
    }
}
