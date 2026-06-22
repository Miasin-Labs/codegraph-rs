//! Shared regexes and ranking helpers.

use std::sync::OnceLock;

use regex::Regex;

use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{EdgeKind, Node, NodeKind};

pub(super) fn dot_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([0-9A-Za-z_]+)\.([0-9A-Za-z_]+)$").expect("valid regex"))
}

pub(super) fn colon_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([0-9A-Za-z_]+)::([0-9A-Za-z_]+)$").expect("valid regex"))
}

pub(super) fn cpp_keyword_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(const|volatile|mutable|typename|class|struct)\b").expect("valid regex")
    })
}

pub(super) fn ptr_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[&*]+").expect("valid regex"))
}

pub(super) fn angle_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]*>").expect("valid regex"))
}

pub(super) fn whitespace_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").expect("valid regex"))
}

pub(super) fn cpp_source_ext_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\.(?:c|cc|cpp|cxx)$").expect("valid regex"))
}

pub(super) fn array_brackets_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\s*\]").expect("valid regex"))
}

pub(super) fn varargs_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.\.\.$").expect("valid regex"))
}

pub(super) fn dot_space_split_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[.\s]+").expect("valid regex"))
}

pub(super) fn camel_lower_upper_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([a-z])([A-Z])").expect("valid regex"))
}

pub(super) fn camel_acronym_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([A-Z]+)([A-Z][a-z])").expect("valid regex"))
}

pub(super) fn word_split_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\s._:/\\]+").expect("valid regex"))
}

/// Uppercase the first character (JS `charAt(0).toUpperCase() + slice(1)`).
pub(super) fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Split a camelCase or PascalCase string into words.
pub(super) fn split_camel_case(s: &str) -> Vec<String> {
    let s = camel_lower_upper_re().replace_all(s, "${1} ${2}");
    let s = camel_acronym_re().replace_all(&s, "${1} ${2}");
    word_split_re()
        .split(&s)
        .filter(|w| w.chars().count() > 1)
        .map(|w| w.to_string())
        .collect()
}

/// Compute directory proximity between two file paths.
/// Returns a score based on the number of shared directory segments.
/// Higher score = closer in directory tree.
pub(super) fn compute_path_proximity(file_path1: &str, file_path2: &str) -> i32 {
    let mut dir1: Vec<&str> = file_path1.split('/').collect();
    dir1.pop();
    let mut dir2: Vec<&str> = file_path2.split('/').collect();
    dir2.pop();

    let mut shared: i32 = 0;
    for i in 0..dir1.len().min(dir2.len()) {
        if dir1[i] == dir2[i] {
            shared += 1;
        } else {
            break;
        }
    }

    // Each shared directory segment contributes 15 points, capped at 80
    (shared * 15).min(80)
}

/// Find the best matching node when there are multiple candidates
pub(super) fn find_best_match<'a>(
    reference: &UnresolvedRef,
    candidates: &'a [Node],
    _context: &dyn ResolutionContext,
) -> Option<&'a Node> {
    // Prioritization rules:
    // 1. Same file > different file
    // 2. Directory proximity (same module/package > different module)
    // 3. Same language > different language
    // 4. Functions/methods > classes/types (for call references)
    // 5. Exported > non-exported

    let mut best_score: f64 = -1.0;
    let mut best_node: Option<&Node> = None;

    for candidate in candidates {
        let mut score: f64 = 0.0;

        // Same file bonus
        if candidate.file_path == reference.file_path {
            score += 100.0;
        }

        // Directory proximity bonus — strongly prefer same module/package
        score += compute_path_proximity(&reference.file_path, &candidate.file_path) as f64;

        // Language matching: strongly prefer same language, penalize cross-language
        if candidate.language == reference.language {
            score += 50.0;
        } else {
            score -= 80.0;
        }

        // For call references, prefer functions/methods
        if reference.reference_kind == EdgeKind::Calls
            && (candidate.kind == NodeKind::Function || candidate.kind == NodeKind::Method)
        {
            score += 25.0;
        }

        // For instantiation references (`new Foo()`), prefer class-like
        // targets — without this, a function named `Foo` in another module
        // could outscore the actual class.
        if reference.reference_kind == EdgeKind::Instantiates
            && (candidate.kind == NodeKind::Class
                || candidate.kind == NodeKind::Struct
                || candidate.kind == NodeKind::Interface)
        {
            score += 25.0;
        }

        // For decorator references (`@Foo`), prefer functions. Class
        // decorators (Python `@SomeClass`, Java annotation interfaces)
        // also resolve here, hence the smaller class bonus.
        if reference.reference_kind == EdgeKind::Decorates {
            if candidate.kind == NodeKind::Function || candidate.kind == NodeKind::Method {
                score += 25.0;
            } else if candidate.kind == NodeKind::Class || candidate.kind == NodeKind::Interface {
                score += 15.0;
            }
        }

        // Exported bonus
        if candidate.is_exported == Some(true) {
            score += 10.0;
        }

        // Closer line number (within same file)
        if candidate.file_path == reference.file_path && candidate.start_line != 0 {
            let distance = (candidate.start_line as i64 - reference.line as i64).abs() as f64;
            score += (20.0 - distance / 10.0).max(0.0);
        }

        if score > best_score {
            best_score = score;
            best_node = Some(candidate);
        }
    }

    best_node
}
