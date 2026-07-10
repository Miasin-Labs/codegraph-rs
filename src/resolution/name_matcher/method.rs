//! Method-call matching strategy.

use super::receiver::{
    infer_cpp_receiver_type,
    infer_java_field_receiver_type,
    infer_local_receiver_type,
    resolve_method_on_type,
};
use super::support::{
    capitalize_first,
    colon_call_re,
    dot_call_re,
    lua_colon_call_re,
    r_dollar_call_re,
    split_camel_case,
};
use crate::resolution::jvm_scope;
use crate::resolution::types::{ResolutionContext, ResolvedBy, ResolvedRef, UnresolvedRef};
use crate::types::{Language, Node, NodeKind};

/// Try to resolve by method name on a class/object
pub fn match_method_call(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    match_method_call_hinted(reference, context, None)
}

/// The exact `obj.method` / `Class::method` split `match_method_call` uses.
/// Shared with the GPU marshal so both sides gate on identical references.
#[cfg(feature = "gpu")]
pub(crate) fn split_method_call(name: &str) -> Option<(&str, &str)> {
    let caps = dot_call_re()
        .captures(name)
        .or_else(|| colon_call_re().captures(name))?;
    Some((caps.get(1)?.as_str(), caps.get(2)?.as_str()))
}

/// Shared with the GPU marshal (strategy-2 capitalized-receiver parity).
#[cfg(feature = "gpu")]
pub(crate) fn capitalize_first_shared(s: &str) -> String {
    capitalize_first(s)
}

/// `match_method_call` with an optional GPU-precomputed strategy-1/2 outcome
/// (feature `gpu`): the language-specific inference chain always runs first
/// (host-bound — file-content scans); only the class-candidate × file-method
/// scan block is replaced by the kernel's winner. `Some(None)` = the kernel
/// proved strategies 1+2 find nothing (fall through to strategy 3);
/// `Some(Some((method, via_strategy1)))` = the kernel's first-match winner.
pub fn match_method_call_hinted(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
    s12: Option<Option<(&Node, bool)>>,
) -> Option<ResolvedRef> {
    // Parse method call patterns like "obj.method" or "Class::method"
    let dot_match = dot_call_re().captures(&reference.reference_name);
    let is_dot_match = dot_match.is_some();
    let colon_match = colon_call_re().captures(&reference.reference_name);
    let lua_colon_match = matches!(reference.language, Language::Lua | Language::Luau)
        .then(|| lua_colon_call_re().captures(&reference.reference_name))
        .flatten();
    let r_dollar_match = (reference.language.as_str() == "r")
        .then(|| r_dollar_call_re().captures(&reference.reference_name))
        .flatten();
    let inferable_receiver = is_dot_match || lua_colon_match.is_some() || r_dollar_match.is_some();

    let caps = dot_match
        .or(colon_match)
        .or(lua_colon_match)
        .or(r_dollar_match)?;

    let object_or_class = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    let method_name = caps.get(2).map(|m| m.as_str()).unwrap_or("");

    // Apex identifiers are case-insensitive; every method-name comparison in
    // the strategies below folds case for Apex references and stays exact for
    // everything else.
    let apex = reference.language == Language::Apex;
    let names_eq = |a: &str, b: &str| -> bool {
        if apex {
            a.eq_ignore_ascii_case(b)
        } else {
            a == b
        }
    };

    if inferable_receiver {
        let inferred_type = if reference.language == Language::Cpp && is_dot_match {
            infer_cpp_receiver_type(object_or_class, reference, context)
        } else {
            infer_local_receiver_type(object_or_class, reference, context)
        };
        if let Some(inferred_type) = inferred_type {
            let imports = context.get_import_mappings(&reference.file_path, reference.language);
            let imported_fqn = matches!(reference.language, Language::Java | Language::Kotlin)
                .then(|| {
                    imports
                        .iter()
                        .find(|mapping| mapping.local_name == inferred_type)
                        .map(|mapping| mapping.source.as_str())
                })
                .flatten();
            let typed_match = resolve_method_on_type(
                &inferred_type,
                method_name,
                reference,
                context,
                0.9,
                ResolvedBy::InstanceMethod,
                imported_fqn,
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

    // GPU fast path: strategies 1+2 were precomputed by the containment-join
    // kernel in candidate order — identical selection to the loops below.
    // `Some(None)` = kernel proved no S1/S2 match — both loops are skipped
    // below via `run_s12_on_cpu`.
    if let Some(Some((method_node, via_strategy1))) = s12 {
        return Some(ResolvedRef {
            original: reference.clone(),
            target_node_id: method_node.id.clone(),
            confidence: if via_strategy1 { 0.85 } else { 0.8 },
            resolved_by: if via_strategy1 {
                ResolvedBy::QualifiedName
            } else {
                ResolvedBy::InstanceMethod
            },
        });
    }
    let run_s12_on_cpu = s12.is_none();

    // Strategy 1: Direct class name match (existing logic)
    let mut class_candidates = if run_s12_on_cpu {
        context.get_nodes_by_name(object_or_class)
    } else {
        Vec::new()
    };
    class_candidates.sort_by_key(|node| node.file_path != reference.file_path);

    // Apex: case-folded receiver lookup when the exact one finds nothing
    // (`accountservice.create` → class `AccountService`). Restricted to Apex
    // nodes; also subsumes the capitalized-receiver heuristic of strategy 2
    // for instance variables (`acctService` folds onto `AcctService`).
    if apex && run_s12_on_cpu && class_candidates.is_empty() {
        class_candidates = context
            .get_nodes_by_lower_name(&object_or_class.to_lowercase())
            .into_iter()
            .filter(|n| n.language == Language::Apex)
            .collect();
    }

    let mut class_candidate_order: Vec<&Node> = Vec::new();
    if let Some(scoped) = jvm_scope::scoped_candidates(reference, context, &class_candidates) {
        class_candidate_order.extend(scoped.nodes);
    }
    for class_node in &class_candidates {
        if !class_candidate_order
            .iter()
            .any(|ordered| ordered.id == class_node.id)
        {
            class_candidate_order.push(class_node);
        }
    }

    for class_node in class_candidate_order {
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
                    && names_eq(&n.name, method_name)
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
    if run_s12_on_cpu && capitalized_receiver != object_or_class {
        let mut fuzzy_class_candidates = context.get_nodes_by_name(&capitalized_receiver);
        fuzzy_class_candidates.sort_by_key(|node| node.file_path != reference.file_path);
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
                        && names_eq(&n.name, method_name)
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
        let mut method_candidates = context.get_nodes_by_name(method_name);
        // Apex case-folded fallback, mirroring the strategy-1 retry above.
        if apex && method_candidates.is_empty() {
            method_candidates = context
                .get_nodes_by_lower_name(&method_name.to_lowercase())
                .into_iter()
                .filter(|n| n.language == Language::Apex)
                .collect();
        }
        let methods: Vec<&Node> = method_candidates
            .iter()
            .filter(|n| n.kind == NodeKind::Method && names_eq(&n.name, method_name))
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
