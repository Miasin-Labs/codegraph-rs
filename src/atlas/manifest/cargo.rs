//! `Cargo.toml`: `path = …` dependencies (every dependency table, including
//! `[target.*]`, `[workspace.dependencies]`, `[patch.*]` and `[replace]`)
//! and `[workspace] members` (globs expanded, `exclude` honoured). Parsed
//! with spans, so each link's evidence is the exact line.

use std::path::Path;

use toml::Spanned;
use toml::de::{DeTable, DeValue};

use super::{Parsed, RawLink, glob, line_of};
use crate::atlas::kinds::LinkKind;

const DEP_TABLES: &[&str] = &[
    "dependencies",
    "dev-dependencies",
    "build-dependencies",
    "dev_dependencies",
    "build_dependencies",
];

pub(super) fn parse(dir: &Path, text: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let Ok(doc) = DeTable::parse(text) else {
        return parsed;
    };
    let doc = doc.get_ref();
    parsed.package = table(doc, "package")
        .and_then(|package| string(package, "name"))
        .map(|(name, _)| name.to_owned());

    let mut dep_tables: Vec<&DeTable<'_>> =
        DEP_TABLES.iter().filter_map(|t| table(doc, t)).collect();
    if let Some(targets) = table(doc, "target") {
        for (_, target) in targets.iter() {
            if let DeValue::Table(target) = target.get_ref() {
                dep_tables.extend(DEP_TABLES.iter().filter_map(|t| table(target, t)));
            }
        }
    }
    if let Some(patches) = table(doc, "patch") {
        for (_, source) in patches.iter() {
            if let DeValue::Table(source) = source.get_ref() {
                dep_tables.push(source);
            }
        }
    }
    dep_tables.extend(table(doc, "replace"));
    if let Some(workspace) = table(doc, "workspace") {
        dep_tables.extend(table(workspace, "dependencies"));
        members(dir, text, workspace, &mut parsed);
    }
    for deps in dep_tables {
        for (name, dep) in deps.iter() {
            let DeValue::Table(dep) = dep.get_ref() else {
                continue;
            };
            if let Some((path, span)) = string(dep, "path") {
                parsed.links.push(RawLink {
                    kind: LinkKind::CargoPathDep,
                    path: path.to_owned(),
                    line: Some(line_of(text, span.start)),
                    detail: Some(name.get_ref().to_string()),
                });
            }
        }
    }
    parsed
}

/// `[workspace] members` → one link per member directory, each queued so
/// its own manifest is read too.
fn members(dir: &Path, text: &str, workspace: &DeTable<'_>, parsed: &mut Parsed) {
    let patterns = strings(workspace, "members");
    let excludes: Vec<&str> = strings(workspace, "exclude")
        .into_iter()
        .map(|(p, _)| p)
        .collect();
    let lines: Vec<(&str, u32)> = patterns
        .iter()
        .map(|(p, span)| (*p, line_of(text, span.start)))
        .collect();
    let includes: Vec<&str> = patterns.iter().map(|(p, _)| *p).collect();
    for (member, pattern) in glob::expand(dir, &includes, &excludes, "Cargo.toml") {
        let line = lines.iter().find(|(p, _)| *p == pattern).map(|(_, l)| *l);
        let rel = member.strip_prefix(dir).map_or_else(
            |_| member.display().to_string(),
            |r| r.display().to_string(),
        );
        parsed.links.push(RawLink {
            kind: LinkKind::CargoWorkspaceMember,
            path: member.to_string_lossy().into_owned(),
            line,
            detail: Some(rel),
        });
        parsed.members.push(member);
    }
}

fn table<'a, 'i>(parent: &'a DeTable<'i>, key: &str) -> Option<&'a DeTable<'i>> {
    parent.iter().find_map(|(k, v)| match v.get_ref() {
        DeValue::Table(t) if k.get_ref() == key => Some(t),
        _ => None,
    })
}

fn string<'a>(parent: &'a DeTable<'_>, key: &str) -> Option<(&'a str, std::ops::Range<usize>)> {
    parent.iter().find_map(|(k, v)| match v.get_ref() {
        DeValue::String(s) if k.get_ref() == key => Some((s.as_ref(), v.span())),
        _ => None,
    })
}

fn strings<'a>(parent: &'a DeTable<'_>, key: &str) -> Vec<(&'a str, std::ops::Range<usize>)> {
    parent
        .iter()
        .find_map(|(k, v)| match v.get_ref() {
            DeValue::Array(items) if k.get_ref() == key => Some(items),
            _ => None,
        })
        .map(|items| {
            items
                .iter()
                .filter_map(|item: &Spanned<DeValue<'_>>| match item.get_ref() {
                    DeValue::String(s) => Some((s.as_ref(), item.span())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}
