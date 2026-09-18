//! `package.json` workspaces and local dependencies, and
//! `pnpm-workspace.yaml` packages.
//!
//! `file:`/`link:`/`portal:` deps name a path; `workspace:` deps name a
//! package of the same workspace (`workspace:*`, `workspace:^`) — resolved
//! by name once every package of the project is known — or, rarely, a path
//! (`workspace:../pkg`). JSON has no spans, so evidence lines come from the
//! first line holding the entry.

use std::path::Path;

use serde_json::Value;

use super::{Parsed, RawLink, glob, line_containing};
use crate::atlas::kinds::LinkKind;

const DEP_FIELDS: &[&str] = &[
    "dependencies",
    "devDependencies",
    "peerDependencies",
    "optionalDependencies",
];

pub(super) fn parse(dir: &Path, text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let Ok(Value::Object(doc)) = serde_json::from_str::<Value>(text) else {
        return parsed;
    };
    parsed.package = doc.get("name").and_then(Value::as_str).map(str::to_owned);

    let workspaces = match doc.get("workspaces") {
        Some(Value::Array(items)) => Some(items),
        Some(Value::Object(obj)) => obj.get("packages").and_then(Value::as_array),
        _ => None,
    };
    if let Some(items) = workspaces {
        let patterns: Vec<&str> = items.iter().filter_map(Value::as_str).collect();
        workspace_members(dir, text, &patterns, &mut parsed);
    }

    for field in DEP_FIELDS {
        let Some(Value::Object(deps)) = doc.get(*field) else {
            continue;
        };
        for (name, spec) in deps {
            let Some(spec) = spec.as_str() else { continue };
            let line = line_containing(text, &[&format!("\"{name}\""), spec]);
            if let Some(path) = ["file:", "link:", "portal:"]
                .iter()
                .find_map(|scheme| spec.strip_prefix(scheme))
            {
                parsed.links.push(RawLink {
                    kind: LinkKind::NpmFileDep,
                    path: path.to_owned(),
                    line,
                    detail: Some(name.clone()),
                });
            } else if let Some(rest) = spec.strip_prefix("workspace:") {
                if rest.starts_with('.') || rest.starts_with('/') {
                    parsed.links.push(RawLink {
                        kind: LinkKind::NpmFileDep,
                        path: rest.to_owned(),
                        line,
                        detail: Some(name.clone()),
                    });
                } else {
                    parsed.workspace_deps.push((name.clone(), line));
                }
            }
        }
    }
    parsed
}

/// `pnpm-workspace.yaml`: the `packages:` list (a minimal YAML read — the
/// file is one top-level key holding a list of quoted or bare globs).
pub(super) fn parse_pnpm_workspace(dir: &Path, text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut in_packages = false;
    let mut patterns: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if !line.starts_with([' ', '\t', '-']) {
            in_packages = trimmed.starts_with("packages:");
            continue;
        }
        if let (true, Some(item)) = (in_packages, trimmed.strip_prefix('-')) {
            let item = item.split(" #").next().unwrap_or_default().trim();
            patterns.push(item.trim_matches(['\'', '"']));
        }
    }
    workspace_members(dir, text, &patterns, &mut parsed);
    parsed
}

/// Expand workspace globs (`!pattern` excludes) into member links.
fn workspace_members(dir: &Path, text: &str, patterns: &[&str], parsed: &mut Parsed) {
    let includes: Vec<&str> = patterns
        .iter()
        .copied()
        .filter(|p| !p.starts_with('!'))
        .collect();
    let excludes: Vec<&str> = patterns
        .iter()
        .filter_map(|p| p.strip_prefix('!'))
        .collect();
    for (member, pattern) in glob::expand(dir, &includes, &excludes, "package.json") {
        let rel = member.strip_prefix(dir).map_or_else(
            |_| member.display().to_string(),
            |r| r.display().to_string(),
        );
        parsed.links.push(RawLink {
            kind: LinkKind::NpmWorkspace,
            path: member.to_string_lossy().into_owned(),
            line: line_containing(text, &[&pattern]),
            detail: Some(rel),
        });
        parsed.members.push(member);
    }
}
