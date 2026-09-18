//! `bun.lock` (Bun ≥ 1.2's text lockfile: JSON with trailing commas).
//!
//! `packages` maps an install key — the package name, or `parent/child` for
//! a copy nested under `parent` — to `["name@version", registry, meta,
//! integrity]`; `workspaces` declares what each workspace depends on.
//! (The older binary `bun.lockb` is not read.)

use std::collections::HashSet;

use jsonc_parser::{JsonValue, ParseOptions};

use super::js::{npm_source, split_name_version};
use crate::deps::model::{DepKey, DepSource, Ecosystem, ResolvedDep};

pub(crate) fn parse(text: &str, lockfile: &str, dir: &str) -> Result<Vec<ResolvedDep>, String> {
    let value = jsonc_parser::parse_to_value(text, &ParseOptions::default())
        .map_err(|e| format!("{lockfile}: {e}"))?
        .ok_or_else(|| format!("{lockfile}: empty"))?;
    let JsonValue::Object(root) = value else {
        return Err(format!("{lockfile}: not an object"));
    };

    let mut declared: HashSet<String> = HashSet::new();
    let mut packages = None;
    for (key, value) in root {
        match (key.as_str(), value) {
            ("workspaces", JsonValue::Object(workspaces)) => {
                for (_, workspace) in workspaces {
                    let JsonValue::Object(workspace) = workspace else {
                        continue;
                    };
                    for (field, deps) in workspace {
                        if field.ends_with("ependencies") {
                            if let JsonValue::Object(deps) = deps {
                                declared.extend(deps.into_iter().map(|(name, _)| name));
                            }
                        }
                    }
                }
            }
            ("packages", JsonValue::Object(map)) => packages = Some(map),
            _ => {}
        }
    }

    let mut out = Vec::new();
    for (install_key, entry) in packages.into_iter().flatten() {
        let JsonValue::Array(fields) = entry else {
            continue;
        };
        let Some(JsonValue::String(spec)) = fields.take_inner().into_iter().next() else {
            continue;
        };
        let Some((name, version)) = split_name_version(&spec) else {
            continue;
        };
        let source = npm_source(version, None);
        let version = match source {
            DepSource::Registry => version.to_string(),
            _ => "0.0.0".to_string(),
        };
        let segments = key_segments(&install_key);
        let top_level = segments.len() == 1;
        let install_path = segments
            .iter()
            .map(|s| format!("node_modules/{s}"))
            .collect::<Vec<_>>()
            .join("/");
        out.push(ResolvedDep {
            key: DepKey::for_source(Ecosystem::Npm, name, &version, &source),
            lock_version: version,
            direct: match source {
                DepSource::Path { .. } => None,
                _ => Some(top_level && declared.contains(name)),
            },
            source,
            lockfile: lockfile.to_string(),
            install_path: Some(if dir.is_empty() {
                install_path
            } else {
                format!("{dir}/{install_path}")
            }),
        });
    }
    Ok(out)
}

/// `@a/b/@c/d/e` → `["@a/b", "@c/d", "e"]`.
fn key_segments(key: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut parts = key.split('/');
    while let Some(part) = parts.next() {
        if part.starts_with('@') {
            match parts.next() {
                Some(rest) => out.push(format!("{part}/{rest}")),
                None => out.push(part.to_string()),
            }
        } else {
            out.push(part.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_packages_nesting_and_direct_flags() {
        let text = r#"{
  "lockfileVersion": 1,
  "workspaces": {
    "": { "name": "stats", "dependencies": { "@opencode-ai/plugin": "latest" },
          "devDependencies": { "typescript": "^5.0.0", }, },
  },
  "packages": {
    "@opencode-ai/plugin": ["@opencode-ai/plugin@1.0.201", "", { "dependencies": { "zod": "4.1.8" } }, "sha512-a=="],
    "typescript": ["typescript@5.9.3", "", {}, "sha512-b=="],
    "zod": ["zod@4.1.8", "", {}, "sha512-c=="],
    "@opencode-ai/plugin/zod": ["zod@3.0.0", "", {}, "sha512-d=="],
    "mine": ["mine@workspace:packages/mine", "", {}],
  }
}"#;
        let deps = parse(text, "bun.lock", "").unwrap();
        assert_eq!(deps.len(), 5);
        let find = |name: &str, v: &str| {
            deps.iter()
                .find(|d| d.key.name == name && d.lock_version == v)
                .unwrap()
        };
        assert_eq!(find("@opencode-ai/plugin", "1.0.201").direct, Some(true));
        assert_eq!(find("typescript", "5.9.3").direct, Some(true));
        assert_eq!(find("zod", "4.1.8").direct, Some(false));
        let nested = find("zod", "3.0.0");
        assert_eq!(nested.direct, Some(false));
        assert_eq!(
            nested.install_path.as_deref(),
            Some("node_modules/@opencode-ai/plugin/node_modules/zod")
        );
        assert!(matches!(
            find("mine", "0.0.0").source,
            DepSource::Path { .. }
        ));
    }
}
