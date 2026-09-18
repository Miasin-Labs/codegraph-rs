//! `yarn.lock`, classic (v1) and Berry (v2+) layouts.
//!
//! Both are one entry per resolved package: an unindented key listing the
//! ranges it satisfies (`"a@^1", a@^1.2:`), then indented fields
//! (`version "1.2.3"` in v1, `version: 1.2.3` in Berry). Yarn records no
//! install paths or direct flags: a package is direct when the adjacent
//! package.json names it and only one version of it is locked.

use std::collections::{HashMap, HashSet};

use super::js::{npm_source, split_name_version};
use crate::deps::model::{DepKey, DepSource, Ecosystem, ResolvedDep};

#[derive(Debug, Default)]
struct Entry {
    name: String,
    range: String,
    version: String,
    resolved: Option<String>,
}

pub(crate) fn parse(text: &str, lockfile: &str, declared: &HashSet<String>) -> Vec<ResolvedDep> {
    let mut entries: Vec<Entry> = Vec::new();
    for raw in text.lines() {
        if raw.trim().is_empty() || raw.starts_with('#') {
            continue;
        }
        let indent = raw.len() - raw.trim_start().len();
        let line = raw.trim();
        if indent == 0 {
            let key = line.trim_end_matches(':');
            if key == "__metadata" {
                entries.push(Entry::default()); // swallow its fields
                continue;
            }
            let first = key.split(", ").next().unwrap_or(key);
            let first = first.trim_matches('"');
            match split_name_version(first) {
                Some((name, range)) => entries.push(Entry {
                    name: name.to_string(),
                    range: range.to_string(),
                    ..Entry::default()
                }),
                None => entries.push(Entry::default()),
            }
            continue;
        }
        if indent != 2 {
            continue;
        }
        let Some(entry) = entries.last_mut() else {
            continue;
        };
        let (field, value) = match line.split_once(' ') {
            Some((field, value)) => (field.trim_end_matches(':'), value.trim().trim_matches('"')),
            None => continue,
        };
        match field {
            "version" => entry.version = value.to_string(),
            "resolved" | "resolution" => entry.resolved = Some(value.to_string()),
            _ => {}
        }
    }
    entries.retain(|e| !e.name.is_empty() && !e.version.is_empty());

    let mut versions_per_name: HashMap<&str, HashSet<&str>> = HashMap::new();
    for e in &entries {
        versions_per_name
            .entry(e.name.as_str())
            .or_default()
            .insert(e.version.as_str());
    }
    let mut out = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for e in &entries {
        if !seen.insert((e.name.clone(), e.version.clone())) {
            continue;
        }
        let source = entry_source(e);
        if matches!(source, DepSource::Path { .. }) && e.range.starts_with("workspace:") {
            continue; // a workspace of this project
        }
        let unique = versions_per_name
            .get(e.name.as_str())
            .map_or(0, HashSet::len)
            == 1;
        let version = e.version.clone();
        out.push(ResolvedDep {
            key: DepKey::for_source(Ecosystem::Npm, &e.name, &version, &source),
            lock_version: version,
            direct: match source {
                DepSource::Path { .. } => None,
                _ if !declared.contains(&e.name) => Some(false),
                _ if unique => Some(true),
                _ => None,
            },
            source,
            lockfile: lockfile.to_string(),
            install_path: None,
        });
    }
    out
}

fn entry_source(entry: &Entry) -> DepSource {
    let resolved = entry.resolved.as_deref().map(strip_berry_name);
    let range = entry.range.strip_prefix("npm:").unwrap_or(&entry.range);
    npm_source(range, resolved.filter(|r| !r.starts_with("npm:")))
}

/// Berry resolutions are `name@npm:1.2.3`, `name@workspace:.`,
/// `name@https://…#commit=sha`; v1 writes the URL itself. A URL's first `@`
/// (`git+ssh://git@host`) comes after its `://`, a name's never does.
fn strip_berry_name(resolution: &str) -> &str {
    match split_name_version(resolution) {
        Some((name, rest)) if !name.contains("://") => rest,
        _ => resolution,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classic_v1() {
        let text = "# yarn lockfile v1\n\n\"@babel/code-frame@^7.0.0\", \"@babel/code-frame@^7.10.4\":\n  version \"7.12.13\"\n  resolved \"https://registry.yarnpkg.com/@babel/code-frame/-/code-frame-7.12.13.tgz#abc\"\n  dependencies:\n    \"@babel/highlight\" \"^7.12.13\"\n\n\"@babel/highlight@^7.12.13\":\n  version \"7.13.0\"\n\nleft-pad@^1:\n  version \"1.3.0\"\n\nleft-pad@^0.9:\n  version \"0.9.0\"\n\nmine@git+https://github.com/u/mine.git#abc123:\n  version \"2.0.0\"\n  resolved \"git+https://github.com/u/mine.git#abc123\"\n";
        let declared = HashSet::from([
            "@babel/code-frame".to_string(),
            "left-pad".into(),
            "mine".into(),
        ]);
        let deps = parse(text, "yarn.lock", &declared);
        assert_eq!(deps.len(), 5);
        let find = |n: &str, v: &str| {
            deps.iter()
                .find(|d| d.key.name == n && d.lock_version == v)
                .unwrap()
        };
        assert_eq!(find("@babel/code-frame", "7.12.13").direct, Some(true));
        assert_eq!(
            find("@babel/code-frame", "7.12.13").source,
            DepSource::Registry
        );
        assert_eq!(find("@babel/highlight", "7.13.0").direct, Some(false));
        assert_eq!(
            find("left-pad", "1.3.0").direct,
            None,
            "two versions locked"
        );
        assert!(matches!(
            find("mine", "2.0.0").source,
            DepSource::Git { .. }
        ));
    }

    #[test]
    fn berry() {
        let text = "__metadata:\n  version: 6\n  cacheKey: 8\n\n\"react@npm:^18.0.0\":\n  version: 18.2.0\n  resolution: \"react@npm:18.2.0\"\n  languageName: node\n  linkType: hard\n\n\"app@workspace:.\":\n  version: 0.0.0-use.local\n  resolution: \"app@workspace:.\"\n  linkType: soft\n";
        let declared = HashSet::from(["react".to_string()]);
        let deps = parse(text, "yarn.lock", &declared);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].key.name, "react");
        assert_eq!(deps[0].lock_version, "18.2.0");
        assert_eq!(deps[0].source, DepSource::Registry);
        assert_eq!(deps[0].direct, Some(true));
    }
}
