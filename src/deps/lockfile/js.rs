//! Helpers shared by the JavaScript lockfile readers.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde_json::{Map, Value};

use crate::deps::model::DepSource;

const DEPENDENCY_FIELDS: [&str; 4] = [
    "dependencies",
    "devDependencies",
    "optionalDependencies",
    "peerDependencies",
];

/// Every name a package.json-shaped object depends on.
pub(crate) fn declared_dependency_names(obj: &Map<String, Value>) -> HashSet<String> {
    DEPENDENCY_FIELDS
        .iter()
        .filter_map(|field| obj.get(*field).and_then(Value::as_object))
        .flat_map(|deps| deps.keys().cloned())
        .collect()
}

/// [`declared_dependency_names`] of the `package.json` in `dir` (empty when
/// there is none).
pub(crate) fn package_json_dependency_names(dir: &Path) -> HashSet<String> {
    fs::read_to_string(dir.join("package.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| value.as_object().map(declared_dependency_names))
        .unwrap_or_default()
}

/// The source a JS package manager recorded for a package, from its version
/// spec and/or resolved URL: git URLs (`git+…#sha`, `github:u/r#sha`, GitHub
/// codeload tarballs), local directories (`file:`, `link:`, `workspace:`),
/// otherwise the registry.
pub(crate) fn npm_source(version: &str, resolved: Option<&str>) -> DepSource {
    for spec in [resolved.unwrap_or(""), version] {
        if let Some(source) = non_registry_source(spec) {
            return source;
        }
    }
    DepSource::Registry
}

fn non_registry_source(spec: &str) -> Option<DepSource> {
    let spec = spec.trim();
    if spec.is_empty() {
        return None;
    }
    for prefix in ["file:", "link:", "workspace:", "portal:"] {
        if let Some(path) = spec.strip_prefix(prefix) {
            return Some(DepSource::Path {
                path: Some(path.to_string()),
            });
        }
    }
    if let Some(rest) = spec
        .strip_prefix("https://codeload.github.com/")
        .or_else(|| spec.strip_prefix("http://codeload.github.com/"))
    {
        // codeload.github.com/<owner>/<repo>/tar.gz/<sha>
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() >= 4 {
            return Some(DepSource::Git {
                url: format!("https://github.com/{}/{}", parts[0], parts[1]),
                rev: parts[parts.len() - 1].to_string(),
            });
        }
    }
    let git_like = spec.starts_with("git+")
        || spec.starts_with("git://")
        || spec.starts_with("git@")
        || spec.starts_with("github:")
        || spec.starts_with("gitlab:")
        || spec.starts_with("bitbucket:")
        || (spec.contains(".git#") && spec.contains("://"));
    if git_like {
        let url = spec.strip_prefix("git+").unwrap_or(spec);
        let (url, rev) = match url.split_once('#') {
            Some((url, rev)) => (url, rev.strip_prefix("commit=").unwrap_or(rev)),
            None => (url, ""),
        };
        return Some(DepSource::Git {
            url: url.to_string(),
            rev: rev.to_string(),
        });
    }
    None
}

/// Split `name@version` at the `@` that separates them (a scoped name's
/// leading `@` is part of the name).
pub(crate) fn split_name_version(spec: &str) -> Option<(&str, &str)> {
    let search_from = usize::from(spec.starts_with('@'));
    let at = spec[search_from..].find('@')? + search_from;
    let (name, version) = (&spec[..at], &spec[at + 1..]);
    (!name.is_empty() && !version.is_empty()).then_some((name, version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_sources() {
        assert_eq!(
            npm_source("1.0.0", Some("https://registry.npmjs.org/a/-/a-1.0.0.tgz")),
            DepSource::Registry
        );
        assert_eq!(
            npm_source("1.0.0", Some("git+https://github.com/u/r.git#abc")),
            DepSource::Git {
                url: "https://github.com/u/r.git".into(),
                rev: "abc".into()
            }
        );
        assert_eq!(
            npm_source(
                "https://codeload.github.com/Vencord/arrpc/tar.gz/1dc801e8",
                None
            ),
            DepSource::Git {
                url: "https://github.com/Vencord/arrpc".into(),
                rev: "1dc801e8".into()
            }
        );
        assert_eq!(
            npm_source("link:../x", None),
            DepSource::Path {
                path: Some("../x".into())
            }
        );
        assert!(matches!(
            npm_source("github:u/r#sha", None),
            DepSource::Git { .. }
        ));
    }

    #[test]
    fn splits_scoped_names() {
        assert_eq!(
            split_name_version("@types/node@25.0.3"),
            Some(("@types/node", "25.0.3"))
        );
        assert_eq!(split_name_version("zod@4.1.8"), Some(("zod", "4.1.8")));
        assert_eq!(
            split_name_version("@scope/x@npm:^1"),
            Some(("@scope/x", "npm:^1"))
        );
        assert_eq!(split_name_version("noversion"), None);
    }
}
