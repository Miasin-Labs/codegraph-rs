//! Name Matcher
//!
//! Handles symbol name matching for reference resolution.
//! Ported from `src/resolution/name-matcher.ts`.
//!
//! Retrieval-quality-critical: the scoring weights, thresholds, and
//! tiebreaks below are ported EXACTLY from the TS source — do not tweak
//! without re-validating retrieval (see CLAUDE.md "Retrieval performance").

use std::sync::OnceLock;

use regex::Regex;

use super::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{EdgeKind, Language, Node, NodeKind};

// ---------------------------------------------------------------------------
// Static regexes (compiled once; patterns mirror the TS literals, with JS's
// ASCII-only `\w` spelled out as `[0-9A-Za-z_]` since the regex crate's `\w`
// is Unicode-aware)
// ---------------------------------------------------------------------------

fn dot_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([0-9A-Za-z_]+)\.([0-9A-Za-z_]+)$").expect("valid regex"))
}

fn colon_call_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^([0-9A-Za-z_]+)::([0-9A-Za-z_]+)$").expect("valid regex"))
}

fn cpp_keyword_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"\b(const|volatile|mutable|typename|class|struct)\b").expect("valid regex")
    })
}

fn ptr_ref_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[&*]+").expect("valid regex"))
}

fn angle_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"<[^>]*>").expect("valid regex"))
}

fn whitespace_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\s+").expect("valid regex"))
}

fn cpp_source_ext_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?i)\.(?:c|cc|cpp|cxx)$").expect("valid regex"))
}

fn array_brackets_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\[\s*\]").expect("valid regex"))
}

fn varargs_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"\.\.\.$").expect("valid regex"))
}

fn dot_space_split_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[.\s]+").expect("valid regex"))
}

fn camel_lower_upper_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([a-z])([A-Z])").expect("valid regex"))
}

fn camel_acronym_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([A-Z]+)([A-Z][a-z])").expect("valid regex"))
}

fn word_split_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[\s._:/\\]+").expect("valid regex"))
}

// ---------------------------------------------------------------------------
// Public matching strategies
// ---------------------------------------------------------------------------

/// Try to resolve a path-like reference (e.g., "snippets/drawer-menu.liquid")
/// by matching the filename against file nodes.
pub fn match_by_file_path(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if !reference.reference_name.contains('/') {
        return None;
    }

    // Extract the filename from the path
    let file_name = reference.reference_name.split('/').next_back()?;
    if file_name.is_empty() {
        return None;
    }

    // Search for file nodes with this name
    let candidates = context.get_nodes_by_name(file_name);
    let file_nodes: Vec<&Node> = candidates
        .iter()
        .filter(|n| n.kind == NodeKind::File)
        .collect();

    if file_nodes.is_empty() {
        return None;
    }

    // Prefer exact path match on qualified_name
    let exact_match = file_nodes.iter().find(|n| {
        n.qualified_name == reference.reference_name || n.file_path == reference.reference_name
    });
    if let Some(exact) = exact_match {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: exact.id.clone(),
            confidence: 0.95,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    // Fall back to suffix match (e.g., ref="snippets/foo.liquid" matches "src/snippets/foo.liquid")
    let suffix_match = file_nodes.iter().find(|n| {
        n.qualified_name.ends_with(&reference.reference_name)
            || n.file_path.ends_with(&reference.reference_name)
    });
    if let Some(suffix) = suffix_match {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: suffix.id.clone(),
            confidence: 0.85,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    // If only one file node with this name, use it with lower confidence
    if file_nodes.len() == 1 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: file_nodes[0].id.clone(),
            confidence: 0.7,
            resolved_by: ResolvedBy::FilePath,
        });
    }

    None
}

/// Try to resolve a reference by exact name match
pub fn match_by_exact_name(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let candidates = context.get_nodes_by_name(&reference.reference_name);

    if candidates.is_empty() {
        return None;
    }

    // If only one match, use it — but penalize cross-language matches
    if candidates.len() == 1 {
        let is_cross_language = candidates[0].language != reference.language;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: if is_cross_language { 0.5 } else { 0.9 },
            resolved_by: ResolvedBy::ExactMatch,
        });
    }

    // Multiple matches - try to narrow down
    if let Some(best_match) = find_best_match(reference, &candidates, context) {
        // Lower confidence when the match is from a distant/unrelated module
        let proximity = compute_path_proximity(&reference.file_path, &best_match.file_path);
        let confidence = if proximity >= 30 { 0.7 } else { 0.4 };
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: best_match.id.clone(),
            confidence,
            resolved_by: ResolvedBy::ExactMatch,
        });
    }

    None
}

/// Try to resolve by qualified name
pub fn match_by_qualified_name(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // Check if the reference name looks qualified (contains :: or .)
    if !reference.reference_name.contains("::") && !reference.reference_name.contains('.') {
        return None;
    }

    let candidates = context.get_nodes_by_qualified_name(&reference.reference_name);

    if candidates.len() == 1 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: candidates[0].id.clone(),
            confidence: 0.95,
            resolved_by: ResolvedBy::QualifiedName,
        });
    }

    // Try partial qualified name match
    let parts: Vec<&str> = reference.reference_name.split([':', '.']).collect();
    if let Some(last_name) = parts.last().filter(|s| !s.is_empty()) {
        let partial_candidates = context.get_nodes_by_name(last_name);
        for candidate in &partial_candidates {
            if candidate
                .qualified_name
                .ends_with(&reference.reference_name)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: candidate.id.clone(),
                    confidence: 0.85,
                    resolved_by: ResolvedBy::QualifiedName,
                });
            }
        }
    }

    None
}

/// Resolve `<typeName>::<methodName>` against indexed method nodes.
///
/// `preferred_fqn`: optional FQN that identifies WHICH class declaration
/// `type_name` refers to in the caller's file. When multiple candidates
/// share the same qualifiedName (`FooConverter::convert` in both
/// `dao/converter/` and `service/converter/`), the FQN's file-path-suffix
/// picks the right one — the disambiguation signal Java imports carry but
/// the call site doesn't (#314).
fn resolve_method_on_type(
    type_name: &str,
    method_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    confidence: f64,
    resolved_by: ResolvedBy,
    preferred_fqn: Option<&str>,
) -> Option<ResolvedRef> {
    // Look up methods by name and match by qualifiedName ending in
    // `<typeName>::<methodName>`. This works whether the method is defined
    // in-class (`class Foo { int bar() { ... } }`) or out-of-line in a separate
    // file (`int Foo::bar() { ... }` in foo.cpp while class Foo is in foo.hpp).
    // The previous same-file approach missed the latter — the typical C++ layout.
    let method_candidates = context.get_nodes_by_name(method_name);
    let want = format!("{type_name}::{method_name}");
    let want_suffix = format!("::{want}");
    let matches: Vec<&Node> = method_candidates
        .iter()
        .filter(|m| {
            m.kind == NodeKind::Method
                && m.language == reference.language
                && (m.qualified_name == want || m.qualified_name.ends_with(&want_suffix))
        })
        .collect();
    if matches.is_empty() {
        return None;
    }

    if matches.len() > 1 {
        if let Some(fqn) = preferred_fqn {
            let ext = if reference.language == Language::Kotlin {
                ".kt"
            } else {
                ".java"
            };
            let fqn_path = format!("{}{}", fqn.replace('.', "/"), ext);
            let chosen = matches.iter().find(|m| {
                let fp = m.file_path.replace('\\', "/");
                fp.ends_with(&fqn_path) || fp.ends_with(&format!("/{fqn_path}"))
            });
            if let Some(chosen) = chosen {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: chosen.id.clone(),
                    confidence,
                    resolved_by,
                });
            }
        }
    }

    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: matches[0].id.clone(),
        confidence,
        resolved_by,
    })
}

/// C++ keywords/control-flow tokens that can appear right before a receiver
/// (e.g. `return ptr->m()`) and must NOT be treated as a type.
const CPP_NON_TYPE_TOKENS: [&str; 28] = [
    "return",
    "if",
    "else",
    "for",
    "while",
    "do",
    "switch",
    "case",
    "default",
    "break",
    "continue",
    "goto",
    "throw",
    "new",
    "delete",
    "co_await",
    "co_yield",
    "co_return",
    "static_cast",
    "const_cast",
    "dynamic_cast",
    "reinterpret_cast",
    "sizeof",
    "alignof",
    "typeid",
    "and",
    "or",
    "not",
];
// NOTE: the TS Set also contains "xor" — kept separately because Rust const
// arrays need an exact length; see `is_cpp_non_type_token`.

fn is_cpp_non_type_token(token: &str) -> bool {
    token == "xor" || CPP_NON_TYPE_TOKENS.contains(&token)
}

fn normalize_cpp_type_name(type_name: &str) -> Option<String> {
    let s = cpp_keyword_re().replace_all(type_name, " ");
    let s = ptr_ref_re().replace_all(&s, " ");
    let s = angle_re().replace_all(&s, " ");
    let s = whitespace_re().replace_all(&s, " ");
    let normalized = s.trim();

    if normalized.is_empty() {
        return None;
    }
    let parts: Vec<&str> = normalized.split("::").filter(|p| !p.is_empty()).collect();
    let last = *parts.last()?;
    if last.is_empty() {
        return None;
    }
    if is_cpp_non_type_token(last) {
        return None;
    }
    Some(last.to_string())
}

/// Declarator regex: matches `Type receiver`, `Type* receiver`, `Type *receiver`,
/// `Type*receiver`, `Type<X> receiver`, etc., REQUIRING a declarator terminator
/// (`;`, `=`, `,`, `)`, `[`, `{`, `(`, or end-of-line) after the receiver. The
/// terminator rules out uses like `return receiver->m()` where the preceding
/// token is a keyword, not a type.
///
/// Deviation: the TS regex used a lookahead `(?=[;=,)\[{(]|$)` which the
/// `regex` crate doesn't support; since only the FIRST match's capture group 1
/// is consumed, a consuming non-capturing group is observably equivalent
/// (leftmost-first start position and group-1 contents are identical).
fn build_declarator_regex(escaped_receiver: &str) -> Regex {
    Regex::new(&format!(
        r"([A-Za-z_][0-9A-Za-z_:]*(?:\s*<[^;=(){{}}]+>)?(?:\s*[*&]+)?)\s*\b{escaped_receiver}\b\s*(?:[;=,)\[{{(]|$)"
    ))
    .expect("valid declarator regex")
}

/// Split source into lines like JS `split(/\r?\n/)` (a lone `\r` is NOT a
/// separator; a trailing `\r` before `\n` is stripped).
fn split_lines(source: &str) -> Vec<&str> {
    source
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect()
}

fn infer_cpp_receiver_type(
    receiver_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let source = context.read_file(&reference.file_path)?;
    if source.is_empty() {
        return None;
    }

    let lines = split_lines(&source);
    let call_line_index = ((reference.line as i64) - 1).clamp(0, lines.len() as i64 - 1) as usize;

    // Receiver names repeat constantly across a codebase's references
    // (`this`, `ctx`, `builder`, ...), and `Regex::new` costs tens of µs —
    // compiling two regexes PER REFERENCE was ~10% of the entire llvm
    // resolution pass. Memoize per receiver in a thread-local map (bounded:
    // distinct receiver identifiers are vocabulary-sized, not ref-sized).
    thread_local! {
        static RECEIVER_REGEX_CACHE: std::cell::RefCell<std::collections::HashMap<String, (Regex, Regex)>> =
            std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let (receiver_pattern, declarator_regex) = RECEIVER_REGEX_CACHE.with(|cache| {
        if let Some(pair) = cache.borrow().get(receiver_name) {
            return pair.clone();
        }
        let escaped_receiver = regex::escape(receiver_name);
        let pair = (
            Regex::new(&format!(r"\b{escaped_receiver}\b")).expect("valid receiver regex"),
            build_declarator_regex(&escaped_receiver),
        );
        let mut map = cache.borrow_mut();
        // Safety valve: a pathological generated file with millions of
        // distinct receivers must not grow the cache unboundedly.
        if map.len() >= 65536 {
            map.clear();
        }
        map.insert(receiver_name.to_string(), pair.clone());
        pair
    });

    for i in (0..=call_line_index).rev() {
        let line = lines[i];
        if line.is_empty() || !receiver_pattern.is_match(line) {
            continue;
        }

        if let Some(caps) = declarator_regex.captures(line) {
            let type_text = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Some(normalized) = normalize_cpp_type_name(type_text) {
                return Some(normalized);
            }
        }
    }

    let raw_candidates = [
        cpp_source_ext_re()
            .replace(&reference.file_path, ".h")
            .into_owned(),
        cpp_source_ext_re()
            .replace(&reference.file_path, ".hpp")
            .into_owned(),
        cpp_source_ext_re()
            .replace(&reference.file_path, ".hxx")
            .into_owned(),
    ];
    let mut header_candidates: Vec<&String> = Vec::new();
    for candidate in &raw_candidates {
        if !header_candidates.contains(&candidate) && candidate != &reference.file_path {
            header_candidates.push(candidate);
        }
    }

    for header_path in header_candidates {
        if !context.file_exists(header_path) {
            continue;
        }
        let Some(header_source) = context.read_file(header_path) else {
            continue;
        };
        if header_source.is_empty() {
            continue;
        }

        for line in split_lines(&header_source) {
            if !receiver_pattern.is_match(line) {
                continue;
            }
            let Some(caps) = declarator_regex.captures(line) else {
                continue;
            };
            let type_text = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            if let Some(normalized) = normalize_cpp_type_name(type_text) {
                return Some(normalized);
            }
        }
    }

    None
}

/// Java/Kotlin: infer a receiver's declared type by walking field declarations
/// in the class enclosing the call site. The field's `signature` is already in
/// the form "<TypeName> <fieldName>" (set by tree-sitter.ts extractField), so we
/// pull the type from there. Handles Spring `@Resource UserBO userbo;` /
/// `@Autowired private UserService userService;` where the receiver field name
/// doesn't match the class name by Java naming convention.
///
/// Returns the bare type name (generics stripped, dotted package stripped) or
/// None when no matching field is in the enclosing class.
fn infer_java_field_receiver_type(
    receiver_name: &str,
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<String> {
    let in_file = context.get_nodes_in_file(&reference.file_path);
    if in_file.is_empty() {
        return None;
    }

    // Find the class enclosing the call line (tightest match by latest start).
    let mut enclosing: Option<&Node> = None;
    for n in &in_file {
        if n.kind != NodeKind::Class && n.kind != NodeKind::Interface {
            continue;
        }
        if n.language != reference.language {
            continue;
        }
        let end = n.end_line;
        if n.start_line <= reference.line && end >= reference.line {
            match enclosing {
                Some(e) if n.start_line < e.start_line => {}
                _ => enclosing = Some(n),
            }
        }
    }
    let enclosing = enclosing?;

    let enclosing_end = enclosing.end_line;
    let field = in_file.iter().find(|n| {
        n.kind == NodeKind::Field
            && n.name == receiver_name
            && n.language == reference.language
            && n.start_line >= enclosing.start_line
            && n.end_line <= enclosing_end
    })?;
    let signature = field.signature.as_deref().filter(|s| !s.is_empty())?;

    // Signature shape: "<TypeName> <fieldName>" (extractField). Pull the type,
    // strip generics + dotted package, drop array/varargs markers.
    // (JS `lastIndexOf` returning -1 made `slice(0, -1)` drop the last char;
    // mirror that defensive edge.)
    let before_name = match signature.rfind(&field.name) {
        Some(i) => &signature[..i],
        None => {
            let mut chars = signature.chars();
            chars.next_back();
            chars.as_str()
        }
    };
    let type_raw = before_name.trim();
    if type_raw.is_empty() {
        return None;
    }

    let type_no_generics = angle_re().replace_all(type_raw, "");
    let type_no_generics = type_no_generics.trim();
    let type_no_array = array_brackets_re().replace_all(type_no_generics, "");
    let type_no_array = varargs_re().replace_all(&type_no_array, "");
    let type_no_array = type_no_array.trim();
    let parts: Vec<&str> = dot_space_split_re()
        .split(type_no_array)
        .filter(|p| !p.is_empty())
        .collect();
    let last_part = *parts.last()?;
    if last_part.is_empty() {
        return None;
    }
    if !last_part
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
    {
        return None; // primitives / lowercase → skip
    }
    Some(last_part.to_string())
}

/// Try to resolve by method name on a class/object
pub fn match_method_call(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // Parse method call patterns like "obj.method" or "Class::method"
    let dot_match = dot_call_re().captures(&reference.reference_name);
    let is_dot_match = dot_match.is_some();
    let colon_match = colon_call_re().captures(&reference.reference_name);

    let caps = dot_match.or(colon_match)?;

    let object_or_class = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let method_name = caps.get(2).map(|m| m.as_str()).unwrap_or("");

    if reference.language == Language::Cpp && is_dot_match {
        if let Some(inferred_type) = infer_cpp_receiver_type(object_or_class, reference, context) {
            let typed_match = resolve_method_on_type(
                &inferred_type,
                method_name,
                reference,
                context,
                0.9,
                ResolvedBy::InstanceMethod,
                None,
            );
            if typed_match.is_some() {
                return typed_match;
            }
        }
    }

    // Java/Kotlin: receiver may be a field whose name doesn't match the type by
    // Java naming convention (`userbo` → class `UserBO`, abbreviated). Look up
    // the field in the enclosing class to get its declared type, then resolve
    // the method on that type. Covers Spring `@Resource`/`@Autowired` field
    // injection where the field type is the concrete bean class.
    if (reference.language == Language::Java || reference.language == Language::Kotlin)
        && is_dot_match
    {
        if let Some(inferred_type) =
            infer_java_field_receiver_type(object_or_class, reference, context)
        {
            // When two classes share the same simple name, the caller file's
            // import is the only signal that names WHICH one — pass the
            // imported FQN so resolve_method_on_type can disambiguate (#314).
            let imports = context.get_import_mappings(&reference.file_path, reference.language);
            let imported_fqn = imports
                .iter()
                .find(|i| i.local_name == inferred_type)
                .map(|i| i.source.clone());
            let typed_match = resolve_method_on_type(
                &inferred_type,
                method_name,
                reference,
                context,
                0.9,
                ResolvedBy::InstanceMethod,
                imported_fqn.as_deref(),
            );
            if typed_match.is_some() {
                return typed_match;
            }
        }
    }

    // Strategy 1: Direct class name match (existing logic)
    let class_candidates = context.get_nodes_by_name(object_or_class);

    for class_node in &class_candidates {
        if class_node.kind == NodeKind::Class
            || class_node.kind == NodeKind::Struct
            || class_node.kind == NodeKind::Interface
        {
            // Skip cross-language class matches
            if class_node.language != reference.language {
                continue;
            }

            let nodes_in_file = context.get_nodes_in_file(&class_node.file_path);
            let method_node = nodes_in_file.iter().find(|n| {
                n.kind == NodeKind::Method
                    && n.name == method_name
                    && n.qualified_name.contains(&class_node.name)
            });

            if let Some(method_node) = method_node {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: method_node.id.clone(),
                    confidence: 0.85,
                    resolved_by: ResolvedBy::QualifiedName,
                });
            }
        }
    }

    // Strategy 2: Instance variable receiver - try capitalized form to find class
    // e.g., "permissionEngine" → look for classes containing "PermissionEngine"
    let capitalized_receiver = capitalize_first(object_or_class);
    if capitalized_receiver != object_or_class {
        let fuzzy_class_candidates = context.get_nodes_by_name(&capitalized_receiver);
        for class_node in &fuzzy_class_candidates {
            if class_node.kind == NodeKind::Class
                || class_node.kind == NodeKind::Struct
                || class_node.kind == NodeKind::Interface
            {
                // Skip cross-language class matches
                if class_node.language != reference.language {
                    continue;
                }

                let nodes_in_file = context.get_nodes_in_file(&class_node.file_path);
                let method_node = nodes_in_file.iter().find(|n| {
                    n.kind == NodeKind::Method
                        && n.name == method_name
                        && n.qualified_name.contains(&class_node.name)
                });

                if let Some(method_node) = method_node {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: method_node.id.clone(),
                        confidence: 0.8,
                        resolved_by: ResolvedBy::InstanceMethod,
                    });
                }
            }
        }
    }

    // Strategy 3: Find methods by name across the codebase, match by receiver
    // name similarity with the containing class. Handles abbreviated variable
    // names like permissionEngine → PermissionRuleEngine.
    if !method_name.is_empty() {
        let method_candidates = context.get_nodes_by_name(method_name);
        let methods: Vec<&Node> = method_candidates
            .iter()
            .filter(|n| n.kind == NodeKind::Method && n.name == method_name)
            .collect();

        // Filter to same-language candidates first
        let same_language_methods: Vec<&Node> = methods
            .iter()
            .filter(|m| m.language == reference.language)
            .copied()
            .collect();
        let target_methods: &[&Node] = if !same_language_methods.is_empty() {
            &same_language_methods
        } else {
            &methods
        };

        // If only one same-language method with this name exists, use it
        if target_methods.len() == 1 && target_methods[0].language == reference.language {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: target_methods[0].id.clone(),
                confidence: 0.7,
                resolved_by: ResolvedBy::InstanceMethod,
            });
        }

        // Multiple methods: score by receiver name word overlap with class name
        if target_methods.len() > 1 {
            let receiver_words = split_camel_case(object_or_class);
            let mut best_match: Option<&Node> = None;
            let mut best_score: i64 = 0;

            for &method in target_methods {
                let class_words = split_camel_case(&method.qualified_name);
                let mut score = receiver_words
                    .iter()
                    .filter(|w| {
                        class_words
                            .iter()
                            .any(|cw| cw.to_lowercase() == w.to_lowercase())
                    })
                    .count() as i64;
                // Bonus for same language
                if method.language == reference.language {
                    score += 1;
                }
                if score > best_score {
                    best_score = score;
                    best_match = Some(method);
                }
            }

            if let Some(best) = best_match {
                if best_score >= 2 {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: best.id.clone(),
                        confidence: 0.65,
                        resolved_by: ResolvedBy::InstanceMethod,
                    });
                }
            }
        }
    }

    None
}

/// Uppercase the first character (JS `charAt(0).toUpperCase() + slice(1)`).
fn capitalize_first(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Split a camelCase or PascalCase string into words.
fn split_camel_case(s: &str) -> Vec<String> {
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
fn compute_path_proximity(file_path1: &str, file_path2: &str) -> i32 {
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
fn find_best_match<'a>(
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

/// Fuzzy match - last resort with lower confidence
pub fn match_fuzzy(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let lower_name = reference.reference_name.to_lowercase();

    // Use pre-built lowercase index for O(1) lookup instead of scanning all nodes
    let candidates = context.get_nodes_by_lower_name(&lower_name);

    // Filter to callable kinds only (function, method, class)
    let callable_candidates: Vec<&Node> = candidates
        .iter()
        .filter(|n| {
            n.kind == NodeKind::Function || n.kind == NodeKind::Method || n.kind == NodeKind::Class
        })
        .collect();

    // Prefer same-language matches
    let same_language_candidates: Vec<&Node> = callable_candidates
        .iter()
        .filter(|n| n.language == reference.language)
        .copied()
        .collect();
    let final_candidates: &[&Node] = if !same_language_candidates.is_empty() {
        &same_language_candidates
    } else {
        &callable_candidates
    };

    if final_candidates.len() == 1 {
        let is_cross_language = final_candidates[0].language != reference.language;
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: final_candidates[0].id.clone(),
            confidence: if is_cross_language { 0.3 } else { 0.5 },
            resolved_by: ResolvedBy::Fuzzy,
        });
    }

    None
}

/// Match all strategies in order of confidence
pub fn match_reference(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    // Try strategies in order of confidence

    // 0. File path match (e.g., "snippets/drawer-menu.liquid" → file node)
    if let Some(result) = match_by_file_path(reference, context) {
        return Some(result);
    }

    // 1. Qualified name match (highest confidence)
    if let Some(result) = match_by_qualified_name(reference, context) {
        return Some(result);
    }

    // 2. Method call pattern
    if let Some(result) = match_method_call(reference, context) {
        return Some(result);
    }

    // 3. Exact name match
    if let Some(result) = match_by_exact_name(reference, context) {
        return Some(result);
    }

    // 4. Fuzzy match (lowest confidence)
    if let Some(result) = match_fuzzy(reference, context) {
        return Some(result);
    }

    None
}

// ---------------------------------------------------------------------------
// Tests — ported from the Name Matcher cases of __tests__/resolution.test.ts
// (the "Name Matcher" and "Name Matcher: kind bias for new ref kinds"
// describes), plus Rust-side coverage of the strategies the TS suite
// exercises only indirectly (file-path, fuzzy, C++/Java receiver inference).
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::resolution::types::ImportMapping;
    use crate::types::{EdgeKind, Language, Node, NodeKind};

    struct Fixture {
        nodes: Vec<Node>,
        files: HashMap<String, String>,
        imports: Vec<ImportMapping>,
    }

    impl Fixture {
        fn new(nodes: Vec<Node>) -> Self {
            Fixture {
                nodes,
                files: HashMap::new(),
                imports: Vec::new(),
            }
        }
    }

    impl ResolutionContext for Fixture {
        fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.file_path == file_path)
                .cloned()
                .collect()
        }
        fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name == name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == qualified_name)
                .cloned()
                .collect()
        }
        fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.kind == kind)
                .cloned()
                .collect()
        }
        fn file_exists(&self, file_path: &str) -> bool {
            self.files.contains_key(file_path)
        }
        fn read_file(&self, file_path: &str) -> Option<String> {
            self.files.get(file_path).cloned()
        }
        fn get_project_root(&self) -> &str {
            "/test"
        }
        fn get_all_files(&self) -> Vec<String> {
            self.files.keys().cloned().collect()
        }
        fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower_name)
                .cloned()
                .collect()
        }
        fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
            self.imports.clone()
        }
    }

    fn node(
        id: &str,
        kind: NodeKind,
        name: &str,
        qualified_name: &str,
        file_path: &str,
        language: Language,
        start_line: u32,
        end_line: u32,
    ) -> Node {
        Node::new(
            id,
            kind,
            name,
            qualified_name,
            file_path,
            language,
            start_line,
            end_line,
        )
    }

    fn make_ref(
        name: &str,
        kind: EdgeKind,
        line: u32,
        file_path: &str,
        language: Language,
    ) -> UnresolvedRef {
        UnresolvedRef {
            from_node_id: "caller:main:caller:5".into(),
            reference_name: name.into(),
            reference_kind: kind,
            line,
            column: 10,
            file_path: file_path.into(),
            language,
            candidates: None,
        }
    }

    // -- "should match exact name references" --------------------------------
    #[test]
    fn matches_exact_name_references() {
        let ctx = Fixture::new(vec![node(
            "func:test.ts:myFunction:10",
            NodeKind::Function,
            "myFunction",
            "test.ts::myFunction",
            "test.ts",
            Language::Typescript,
            10,
            20,
        )]);

        let r = make_ref(
            "myFunction",
            EdgeKind::Calls,
            5,
            "main.ts",
            Language::Typescript,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");

        assert_eq!(result.target_node_id, "func:test.ts:myFunction:10");
        assert_eq!(result.resolved_by, ResolvedBy::ExactMatch);
    }

    // -- "should prefer same-module candidates over cross-module matches" ----
    #[test]
    fn prefers_same_module_candidates_over_cross_module_matches() {
        // Simulates a Python monorepo where multiple apps define navigate()
        let candidate_a = node(
            "func:apps/app_a/src/server.py:navigate:10",
            NodeKind::Function,
            "navigate",
            "apps/app_a/src/server.py::navigate",
            "apps/app_a/src/server.py",
            Language::Python,
            10,
            20,
        );
        let candidate_b = node(
            "func:apps/app_b/src/server.py:navigate:15",
            NodeKind::Function,
            "navigate",
            "apps/app_b/src/server.py::navigate",
            "apps/app_b/src/server.py",
            Language::Python,
            15,
            25,
        );
        let ctx = Fixture::new(vec![candidate_a, candidate_b]);

        // Reference from app_a should resolve to app_a's navigate, not app_b's
        let r = make_ref(
            "navigate",
            EdgeKind::Calls,
            5,
            "apps/app_a/src/handler.py",
            Language::Python,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");

        assert_eq!(
            result.target_node_id,
            "func:apps/app_a/src/server.py:navigate:10"
        );
        assert_eq!(result.resolved_by, ResolvedBy::ExactMatch);
    }

    // -- "should lower confidence for cross-module exact matches" ------------
    #[test]
    fn lowers_confidence_for_cross_module_exact_matches() {
        let ctx = Fixture::new(vec![
            node(
                "func:apps/app_b/src/server.py:navigate:10",
                NodeKind::Function,
                "navigate",
                "apps/app_b/src/server.py::navigate",
                "apps/app_b/src/server.py",
                Language::Python,
                10,
                20,
            ),
            node(
                "func:apps/app_c/src/server.py:navigate:10",
                NodeKind::Function,
                "navigate",
                "apps/app_c/src/server.py::navigate",
                "apps/app_c/src/server.py",
                Language::Python,
                10,
                20,
            ),
        ]);

        // Reference from app_a — neither candidate is in the same module
        let r = make_ref(
            "navigate",
            EdgeKind::Calls,
            5,
            "apps/app_a/src/handler.py",
            Language::Python,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");

        // Should still resolve but with low confidence
        assert!(result.confidence <= 0.4);
    }

    // -- "should match qualified name references" ----------------------------
    #[test]
    fn matches_qualified_name_references() {
        let class_node = node(
            "class:user.ts:User:5",
            NodeKind::Class,
            "User",
            "user.ts::User",
            "user.ts",
            Language::Typescript,
            5,
            30,
        );
        let method_node = node(
            "method:user.ts:User.save:15",
            NodeKind::Method,
            "save",
            "user.ts::User::save",
            "user.ts",
            Language::Typescript,
            15,
            25,
        );
        let ctx = Fixture::new(vec![class_node, method_node]);

        let r = make_ref(
            "User.save",
            EdgeKind::Calls,
            5,
            "main.ts",
            Language::Typescript,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");

        assert_eq!(result.target_node_id, "method:user.ts:User.save:15");
    }

    // -- "prefers a class candidate over a function for `instantiates` refs" --
    #[test]
    fn prefers_class_candidate_over_function_for_instantiates_refs() {
        // A class and a function share a name across the codebase.
        // Without the kind bias, the function (which gets the +25 `calls`
        // bonus historically applied to all candidates of that kind) would
        // win. Now the instantiates branch reverses it.
        let func = node(
            "func:utils.ts:Logger:5",
            NodeKind::Function,
            "Logger",
            "utils.ts::Logger",
            "utils.ts",
            Language::Typescript,
            5,
            7,
        );
        let cls = node(
            "class:logger.ts:Logger:10",
            NodeKind::Class,
            "Logger",
            "logger.ts::Logger",
            "logger.ts",
            Language::Typescript,
            10,
            30,
        );
        let ctx = Fixture::new(vec![func, cls]);

        let r = make_ref(
            "Logger",
            EdgeKind::Instantiates,
            5,
            "main.ts",
            Language::Typescript,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(result.target_node_id, "class:logger.ts:Logger:10");
    }

    // -- "prefers a function candidate over a non-function for `decorates`" --
    #[test]
    fn prefers_function_candidate_over_non_function_for_decorates_refs() {
        let variable = node(
            "var:config.ts:Inject:5",
            NodeKind::Variable,
            "Inject",
            "config.ts::Inject",
            "config.ts",
            Language::Typescript,
            5,
            5,
        );
        let decorator = node(
            "func:di.ts:Inject:10",
            NodeKind::Function,
            "Inject",
            "di.ts::Inject",
            "di.ts",
            Language::Typescript,
            10,
            20,
        );
        let ctx = Fixture::new(vec![variable, decorator]);

        let r = make_ref(
            "Inject",
            EdgeKind::Decorates,
            5,
            "svc.ts",
            Language::Typescript,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(result.target_node_id, "func:di.ts:Inject:10");
    }

    // -- Rust-side coverage: file-path strategy ------------------------------
    #[test]
    fn file_path_match_prefers_exact_then_suffix_then_singleton() {
        let exact = node(
            "file:snippets/drawer-menu.liquid",
            NodeKind::File,
            "drawer-menu.liquid",
            "snippets/drawer-menu.liquid",
            "snippets/drawer-menu.liquid",
            Language::Liquid,
            1,
            1,
        );
        let ctx = Fixture::new(vec![exact]);
        let r = make_ref(
            "snippets/drawer-menu.liquid",
            EdgeKind::References,
            5,
            "templates/index.liquid",
            Language::Liquid,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(result.target_node_id, "file:snippets/drawer-menu.liquid");
        assert_eq!(result.resolved_by, ResolvedBy::FilePath);
        assert_eq!(result.confidence, 0.95);

        // Suffix match: indexed under src/ prefix
        let suffix = node(
            "file:src/snippets/foo.liquid",
            NodeKind::File,
            "foo.liquid",
            "src/snippets/foo.liquid",
            "src/snippets/foo.liquid",
            Language::Liquid,
            1,
            1,
        );
        let ctx = Fixture::new(vec![suffix]);
        let r = make_ref(
            "snippets/foo.liquid",
            EdgeKind::References,
            5,
            "templates/index.liquid",
            Language::Liquid,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(result.target_node_id, "file:src/snippets/foo.liquid");
        assert_eq!(result.confidence, 0.85);

        // Singleton fallback: name matches but path doesn't
        let only = node(
            "file:theme/bits/bar.liquid",
            NodeKind::File,
            "bar.liquid",
            "theme/bits/bar.liquid",
            "theme/bits/bar.liquid",
            Language::Liquid,
            1,
            1,
        );
        let ctx = Fixture::new(vec![only]);
        let r = make_ref(
            "other/bar.liquid",
            EdgeKind::References,
            5,
            "templates/index.liquid",
            Language::Liquid,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(result.target_node_id, "file:theme/bits/bar.liquid");
        assert_eq!(result.confidence, 0.7);
    }

    // -- Rust-side coverage: partial qualified-name suffix match --------------
    #[test]
    fn qualified_name_partial_suffix_match() {
        let method_node = node(
            "method:src/user.ts:User::save:15",
            NodeKind::Method,
            "save",
            "src/user.ts::User::save",
            "src/user.ts",
            Language::Typescript,
            15,
            25,
        );
        let ctx = Fixture::new(vec![method_node]);
        let r = make_ref(
            "User::save",
            EdgeKind::Calls,
            5,
            "main.ts",
            Language::Typescript,
        );
        let result = match_by_qualified_name(&r, &ctx).expect("should resolve");
        assert_eq!(result.target_node_id, "method:src/user.ts:User::save:15");
        assert_eq!(result.confidence, 0.85);
        assert_eq!(result.resolved_by, ResolvedBy::QualifiedName);
    }

    // -- Rust-side coverage: fuzzy match confidences --------------------------
    #[test]
    fn fuzzy_match_same_and_cross_language_confidence() {
        // Same language: single callable candidate → 0.5
        let func = node(
            "func:a.ts:myFunc:1",
            NodeKind::Function,
            "myFunc",
            "a.ts::myFunc",
            "a.ts",
            Language::Typescript,
            1,
            2,
        );
        let ctx = Fixture::new(vec![func]);
        let r = make_ref(
            "MYFUNC",
            EdgeKind::Calls,
            5,
            "main.ts",
            Language::Typescript,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(result.resolved_by, ResolvedBy::Fuzzy);
        assert_eq!(result.confidence, 0.5);

        // Cross language: single callable candidate → 0.3
        let func = node(
            "func:a.py:myFunc:1",
            NodeKind::Function,
            "myFunc",
            "a.py::myFunc",
            "a.py",
            Language::Python,
            1,
            2,
        );
        let ctx = Fixture::new(vec![func]);
        let r = make_ref(
            "MYFUNC",
            EdgeKind::Calls,
            5,
            "main.ts",
            Language::Typescript,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(result.resolved_by, ResolvedBy::Fuzzy);
        assert_eq!(result.confidence, 0.3);

        // Non-callable kinds are filtered out
        let var = node(
            "var:a.ts:myFunc:1",
            NodeKind::Variable,
            "myFunc",
            "a.ts::myFunc",
            "a.ts",
            Language::Typescript,
            1,
            1,
        );
        let ctx = Fixture::new(vec![var]);
        let r = make_ref(
            "MYFUNC",
            EdgeKind::Calls,
            5,
            "main.ts",
            Language::Typescript,
        );
        assert!(match_reference(&r, &ctx).is_none());
    }

    // -- Rust-side coverage: C++ receiver-type inference -----------------------
    #[test]
    fn cpp_receiver_type_inference_resolves_out_of_line_method() {
        let method = node(
            "method:src/logger.cpp:Logger::flush:12",
            NodeKind::Method,
            "flush",
            "src/logger.cpp::Logger::flush",
            "src/logger.cpp",
            Language::Cpp,
            12,
            20,
        );
        let mut ctx = Fixture::new(vec![method]);
        ctx.files.insert(
            "src/a.cpp".into(),
            "Logger logger;\nlogger.flush();\n".into(),
        );

        let r = make_ref(
            "logger.flush",
            EdgeKind::Calls,
            2,
            "src/a.cpp",
            Language::Cpp,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(
            result.target_node_id,
            "method:src/logger.cpp:Logger::flush:12"
        );
        assert_eq!(result.resolved_by, ResolvedBy::InstanceMethod);
        assert_eq!(result.confidence, 0.9);
    }

    #[test]
    fn cpp_receiver_type_inference_falls_back_to_header() {
        let method = node(
            "method:src/logger.cpp:Logger::flush:12",
            NodeKind::Method,
            "flush",
            "src/logger.cpp::Logger::flush",
            "src/logger.cpp",
            Language::Cpp,
            12,
            20,
        );
        let mut ctx = Fixture::new(vec![method]);
        // Declarator lives in the sibling header, not the .cpp file
        ctx.files
            .insert("src/a.cpp".into(), "logger.flush();\n".into());
        ctx.files
            .insert("src/a.h".into(), "class A {\n  Logger logger;\n};\n".into());

        let r = make_ref(
            "logger.flush",
            EdgeKind::Calls,
            1,
            "src/a.cpp",
            Language::Cpp,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(
            result.target_node_id,
            "method:src/logger.cpp:Logger::flush:12"
        );
        assert_eq!(result.resolved_by, ResolvedBy::InstanceMethod);
    }

    #[test]
    fn cpp_keyword_before_receiver_is_not_a_type() {
        // `return ptr.m()` must not treat `return` as ptr's type
        assert_eq!(normalize_cpp_type_name("return"), None);
        assert_eq!(
            normalize_cpp_type_name("const Logger&"),
            Some("Logger".into())
        );
        assert_eq!(
            normalize_cpp_type_name("std::vector<int>*"),
            Some("vector".into())
        );
        assert_eq!(normalize_cpp_type_name("xor"), None);
    }

    // -- Rust-side coverage: Java field receiver-type inference ----------------
    #[test]
    fn java_field_receiver_type_resolves_via_field_signature() {
        let class_node = node(
            "class:src/A.java:A:1",
            NodeKind::Class,
            "A",
            "src/A.java::A",
            "src/A.java",
            Language::Java,
            1,
            50,
        );
        let mut field_node = node(
            "field:src/A.java:A::userbo:3",
            NodeKind::Field,
            "userbo",
            "src/A.java::A::userbo",
            "src/A.java",
            Language::Java,
            3,
            3,
        );
        field_node.signature = Some("UserBO userbo".into());
        let method = node(
            "method:src/UserBO.java:UserBO::getUser:8",
            NodeKind::Method,
            "getUser",
            "src/UserBO.java::UserBO::getUser",
            "src/UserBO.java",
            Language::Java,
            8,
            12,
        );
        let ctx = Fixture::new(vec![class_node, field_node, method]);

        let r = make_ref(
            "userbo.getUser",
            EdgeKind::Calls,
            10,
            "src/A.java",
            Language::Java,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(
            result.target_node_id,
            "method:src/UserBO.java:UserBO::getUser:8"
        );
        assert_eq!(result.resolved_by, ResolvedBy::InstanceMethod);
        assert_eq!(result.confidence, 0.9);
    }

    #[test]
    fn java_preferred_fqn_disambiguates_same_named_classes() {
        // Two FooConverter::convert methods in different packages; the import
        // mapping names which one (#314).
        let class_node = node(
            "class:src/com/x/Caller.java:Caller:1",
            NodeKind::Class,
            "Caller",
            "src/com/x/Caller.java::Caller",
            "src/com/x/Caller.java",
            Language::Java,
            1,
            50,
        );
        let mut field_node = node(
            "field:src/com/x/Caller.java:Caller::conv:3",
            NodeKind::Field,
            "conv",
            "src/com/x/Caller.java::Caller::conv",
            "src/com/x/Caller.java",
            Language::Java,
            3,
            3,
        );
        field_node.signature = Some("FooConverter conv".into());
        let dao_method = node(
            "method:src/com/x/dao/converter/FooConverter.java:convert",
            NodeKind::Method,
            "convert",
            "src/com/x/dao/converter/FooConverter.java::FooConverter::convert",
            "src/com/x/dao/converter/FooConverter.java",
            Language::Java,
            5,
            9,
        );
        let service_method = node(
            "method:src/com/x/service/converter/FooConverter.java:convert",
            NodeKind::Method,
            "convert",
            "src/com/x/service/converter/FooConverter.java::FooConverter::convert",
            "src/com/x/service/converter/FooConverter.java",
            Language::Java,
            5,
            9,
        );
        let mut ctx = Fixture::new(vec![class_node, field_node, dao_method, service_method]);
        ctx.imports.push(ImportMapping {
            local_name: "FooConverter".into(),
            exported_name: "FooConverter".into(),
            source: "com.x.service.converter.FooConverter".into(),
            is_default: false,
            is_namespace: false,
            resolved_path: None,
        });

        let r = make_ref(
            "conv.convert",
            EdgeKind::Calls,
            10,
            "src/com/x/Caller.java",
            Language::Java,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        // The import points at service/converter — NOT the dao one that sorts first.
        assert_eq!(
            result.target_node_id,
            "method:src/com/x/service/converter/FooConverter.java:convert"
        );
    }

    // -- Rust-side coverage: capitalized receiver + word-overlap strategies ----
    #[test]
    fn capitalized_receiver_finds_class_method() {
        let class_node = node(
            "class:src/engine.ts:PermissionEngine:1",
            NodeKind::Class,
            "PermissionEngine",
            "src/engine.ts::PermissionEngine",
            "src/engine.ts",
            Language::Typescript,
            1,
            40,
        );
        let method_node = node(
            "method:src/engine.ts:PermissionEngine::check:5",
            NodeKind::Method,
            "check",
            "src/engine.ts::PermissionEngine::check",
            "src/engine.ts",
            Language::Typescript,
            5,
            10,
        );
        let ctx = Fixture::new(vec![class_node, method_node]);

        let r = make_ref(
            "permissionEngine.check",
            EdgeKind::Calls,
            7,
            "src/main.ts",
            Language::Typescript,
        );
        let result = match_reference(&r, &ctx).expect("should resolve");
        assert_eq!(
            result.target_node_id,
            "method:src/engine.ts:PermissionEngine::check:5"
        );
        assert_eq!(result.resolved_by, ResolvedBy::InstanceMethod);
        assert_eq!(result.confidence, 0.8);
    }

    #[test]
    fn split_camel_case_matches_ts_behavior() {
        assert_eq!(
            split_camel_case("permissionEngine"),
            vec!["permission".to_string(), "Engine".to_string()]
        );
        assert_eq!(
            split_camel_case("src/engine.ts::PermissionRuleEngine::check"),
            vec![
                "src".to_string(),
                "engine".to_string(),
                "ts".to_string(),
                "Permission".to_string(),
                "Rule".to_string(),
                "Engine".to_string(),
                "check".to_string()
            ]
        );
        // Acronym handling: HTTPServer → HTTP Server
        assert_eq!(
            split_camel_case("HTTPServer"),
            vec!["HTTP".to_string(), "Server".to_string()]
        );
    }
}
