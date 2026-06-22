use std::sync::OnceLock;

use regex::Regex;

use super::types::{ImportMapping, ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum CandidateScope {
    Imported,
    SamePackage,
    SameSourceRoot,
}

pub(super) struct ScopedCandidates<'a> {
    pub(super) nodes: Vec<&'a Node>,
    pub(super) scope: CandidateScope,
}

pub(super) fn match_exact_name(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    candidates: &[Node],
) -> Option<ResolvedRef> {
    let scoped = scoped_candidates(reference, context, candidates)?;
    let target = *scoped.nodes.first()?;
    let (confidence, resolved_by) = match scoped.scope {
        CandidateScope::Imported => (0.9, ResolvedBy::Import),
        CandidateScope::SamePackage => (0.82, ResolvedBy::ExactMatch),
        CandidateScope::SameSourceRoot => (0.72, ResolvedBy::ExactMatch),
    };

    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id.clone(),
        confidence,
        resolved_by,
    })
}

pub(super) fn scoped_candidates<'a>(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    candidates: &'a [Node],
) -> Option<ScopedCandidates<'a>> {
    if !is_jvm_language(reference.language) {
        return None;
    }
    let symbol = reference_scope_symbol(&reference.reference_name)?;
    let jvm_candidates: Vec<&Node> = candidates
        .iter()
        .filter(|node| is_jvm_language(node.language) && node.name == symbol)
        .collect();
    if jvm_candidates.is_empty() {
        return None;
    }

    let imports = context.get_import_mappings(&reference.file_path, reference.language);
    let mut imported = Vec::new();
    for import in imports.iter().filter(|import| import.local_name == symbol) {
        for candidate in &jvm_candidates {
            if candidate_matches_import(candidate, import) {
                imported.push(*candidate);
            }
        }
    }
    if !imported.is_empty() {
        sort_scoped_candidates(reference, &mut imported);
        return Some(ScopedCandidates {
            nodes: imported,
            scope: CandidateScope::Imported,
        });
    }

    if let Some(package_name) = package_declaration(reference, context) {
        let mut same_package: Vec<&Node> = jvm_candidates
            .iter()
            .copied()
            .filter(|candidate| candidate_in_package(candidate, &package_name))
            .collect();
        if !same_package.is_empty() {
            sort_scoped_candidates(reference, &mut same_package);
            return Some(ScopedCandidates {
                nodes: same_package,
                scope: CandidateScope::SamePackage,
            });
        }

        if let Some(root) = source_root_for_package(&reference.file_path, &package_name) {
            let mut same_root: Vec<&Node> = jvm_candidates
                .iter()
                .copied()
                .filter(|candidate| normalize_path(&candidate.file_path).starts_with(&root))
                .collect();
            if !same_root.is_empty() {
                sort_scoped_candidates(reference, &mut same_root);
                return Some(ScopedCandidates {
                    nodes: same_root,
                    scope: CandidateScope::SameSourceRoot,
                });
            }
        }
    } else if let Some(dir) = parent_dir(&reference.file_path) {
        let mut same_dir: Vec<&Node> = jvm_candidates
            .iter()
            .copied()
            .filter(|candidate| parent_dir(&candidate.file_path).as_deref() == Some(dir.as_str()))
            .collect();
        if !same_dir.is_empty() {
            sort_scoped_candidates(reference, &mut same_dir);
            return Some(ScopedCandidates {
                nodes: same_dir,
                scope: CandidateScope::SamePackage,
            });
        }
    }

    None
}

fn package_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?m)^\s*package\s+([A-Za-z_][0-9A-Za-z_]*(?:\.[A-Za-z_][0-9A-Za-z_]*)*)\s*;?")
            .expect("valid regex")
    })
}

fn is_jvm_language(language: Language) -> bool {
    language == Language::Java || language == Language::Kotlin
}

fn reference_scope_symbol(reference_name: &str) -> Option<&str> {
    let symbol = reference_name
        .split('.')
        .next()
        .filter(|symbol| !symbol.is_empty())?;
    if symbol
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    {
        Some(symbol)
    } else {
        None
    }
}

fn package_declaration(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let content = context.read_file(&reference.file_path)?;
    let captures = package_re().captures(&content)?;
    captures.get(1).map(|m| m.as_str().to_string())
}

fn candidate_matches_import(candidate: &Node, import: &ImportMapping) -> bool {
    fqn_path_suffixes(&import.source)
        .iter()
        .any(|suffix| path_has_suffix(&candidate.file_path, suffix))
}

fn fqn_path_suffixes(fqn: &str) -> Vec<String> {
    let mut suffixes = Vec::with_capacity(4);
    for source in [Some(fqn), fqn.rfind('.').map(|dot| &fqn[..dot])]
        .into_iter()
        .flatten()
    {
        let path = source.replace('.', "/");
        suffixes.push(format!("{path}.java"));
        suffixes.push(format!("{path}.kt"));
    }
    suffixes
}

fn candidate_in_package(candidate: &Node, package_name: &str) -> bool {
    if qualified_name_package(candidate).as_deref() == Some(package_name) {
        return true;
    }
    let package_path = package_name.replace('.', "/");
    let path = normalize_path(&candidate.file_path);
    path.contains(&format!("/{package_path}/")) || path.starts_with(&format!("{package_path}/"))
}

fn qualified_name_package(candidate: &Node) -> Option<String> {
    let (package_name, _) = candidate.qualified_name.split_once("::")?;
    if package_name.contains('/')
        || package_name.ends_with(".java")
        || package_name.ends_with(".kt")
    {
        return None;
    }
    Some(package_name.to_string())
}

fn source_root_for_package(file_path: &str, package_name: &str) -> Option<String> {
    let path = normalize_path(file_path);
    let package_path = package_name.replace('.', "/");
    let marker = format!("{package_path}/");
    let idx = path.find(&marker)?;
    if idx == 0 {
        return None;
    }
    Some(path[..idx].to_string())
}

fn parent_dir(file_path: &str) -> Option<String> {
    let path = normalize_path(file_path);
    path.rsplit_once('/').map(|(dir, _)| dir.to_string())
}

fn path_has_suffix(file_path: &str, suffix: &str) -> bool {
    let path = normalize_path(file_path);
    path == suffix || path.ends_with(&format!("/{suffix}"))
}

fn normalize_path(path: &str) -> String {
    path.replace('\\', "/")
}

fn sort_scoped_candidates(reference: &UnresolvedRef, candidates: &mut Vec<&Node>) {
    candidates.sort_by(|left, right| {
        score_candidate(reference, right)
            .cmp(&score_candidate(reference, left))
            .then_with(|| left.file_path.cmp(&right.file_path))
            .then_with(|| left.id.cmp(&right.id))
    });
}

fn score_candidate(reference: &UnresolvedRef, candidate: &Node) -> i32 {
    let mut score = 0;
    if candidate.language == reference.language {
        score += 50;
    }
    if candidate.file_path == reference.file_path {
        score += 100;
    }
    if candidate.is_exported == Some(true) {
        score += 5;
    }
    if preferred_kind(reference.reference_kind, candidate.kind) {
        score += 25;
    }
    score
}

fn preferred_kind(reference_kind: EdgeKind, candidate_kind: NodeKind) -> bool {
    match reference_kind {
        EdgeKind::Calls => matches!(candidate_kind, NodeKind::Function | NodeKind::Method),
        EdgeKind::Instantiates
        | EdgeKind::References
        | EdgeKind::TypeOf
        | EdgeKind::Returns
        | EdgeKind::Extends
        | EdgeKind::Implements => matches!(
            candidate_kind,
            NodeKind::Class
                | NodeKind::Struct
                | NodeKind::Interface
                | NodeKind::Enum
                | NodeKind::TypeAlias
        ),
        EdgeKind::Decorates => matches!(
            candidate_kind,
            NodeKind::Function | NodeKind::Method | NodeKind::Class | NodeKind::Interface
        ),
        _ => false,
    }
}
