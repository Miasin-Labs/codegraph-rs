use super::super::context::{is_js_ts_language, is_low_value_js_ts_resolution_source};
use super::super::{ReferenceResolver, engine};
use super::{
    APEX_BUILT_IN_METHODS,
    APEX_SYSTEM_TYPES,
    BASH_BUILT_INS,
    C_BUILT_INS,
    C_CPP_STDLIB_CALLS,
    CPP_BUILT_INS,
    GO_BUILT_INS,
    GO_STDLIB_PACKAGES,
    JS_BUILT_INS,
    JVM_NAMESPACE_SEGMENTS,
    JVM_STDLIB_EXTERNAL_CALLS,
    JVM_STDLIB_IMPORT_PREFIXES,
    JVM_STDLIB_TYPES,
    PASCAL_BUILT_INS,
    PASCAL_UNIT_PREFIXES,
    PYTHON_BUILT_IN_METHODS,
    PYTHON_BUILT_IN_TYPES,
    PYTHON_BUILT_INS,
    REACT_HOOKS,
    capitalize_first,
    has_any_possible_match_in,
};
use crate::resolution::types::{ResolutionContext, UnresolvedRef};
use crate::types::{EdgeKind, Language};

impl ReferenceResolver {
    fn has_any_possible_match(&self, name: &str) -> bool {
        let guard = self.context.known_names.borrow();
        let Some(known) = guard.as_ref() else {
            return true;
        };

        has_any_possible_match_in(known, name)
    }

    fn has_any_possible_match_ci(&self, name: &str) -> bool {
        let lower = name.to_lowercase();
        let probe = |s: &str| !self.context.get_nodes_by_lower_name(s).is_empty();
        if probe(&lower) {
            return true;
        }
        if let Some(dot_idx) = lower.find('.') {
            if dot_idx > 0 {
                let receiver = &lower[..dot_idx];
                let member = &lower[dot_idx + 1..];
                if probe(receiver) || probe(member) {
                    return true;
                }
                let last_dot = lower.rfind('.').unwrap_or(0);
                if last_dot > dot_idx && probe(&lower[last_dot + 1..]) {
                    return true;
                }
            }
        }
        false
    }

    fn matches_any_import(&self, r: &UnresolvedRef) -> bool {
        let imports = self.context.get_import_mappings(&r.file_path, r.language);
        if imports.is_empty() {
            return false;
        }
        imports.iter().any(|imp| {
            imp.local_name == r.reference_name
                || r.reference_name
                    .starts_with(&format!("{}.", imp.local_name))
        })
    }

    fn is_built_in_or_external(&self, r: &UnresolvedRef) -> bool {
        let name = r.reference_name.as_str();
        if is_low_value_js_ts_resolution_source(r) {
            return true;
        }
        let is_js_ts = is_js_ts_language(r.language);

        if is_js_ts && JS_BUILT_INS.contains(name) {
            return true;
        }
        if is_js_ts
            && (name.starts_with("console.")
                || name.starts_with("Math.")
                || name.starts_with("JSON."))
        {
            return true;
        }
        if is_js_ts && REACT_HOOKS.contains(name) {
            return true;
        }
        if r.language == Language::Arkts && matches!(name, "$r" | "$rawfile") {
            return true;
        }
        if r.language == Language::Python && PYTHON_BUILT_INS.contains(name) {
            return true;
        }
        if r.language == Language::Python {
            if let Some(dot_idx) = name.find('.') {
                if dot_idx > 0 {
                    let receiver = &name[..dot_idx];
                    let method = &name[dot_idx + 1..];
                    if PYTHON_BUILT_IN_TYPES.contains(receiver) {
                        return true;
                    }
                    if PYTHON_BUILT_IN_METHODS.contains(method) {
                        let capitalized = capitalize_first(receiver);
                        if !self.context.known_has(&capitalized) {
                            return true;
                        }
                    }
                }
            }
            if PYTHON_BUILT_IN_METHODS.contains(name) && !self.context.known_has(name) {
                return true;
            }
        }

        if r.language == Language::Go {
            if let Some(dot_idx) = name.find('.') {
                if dot_idx > 0 {
                    let pkg = &name[..dot_idx];
                    if GO_STDLIB_PACKAGES.contains(pkg) {
                        return true;
                    }
                }
            }
            if GO_BUILT_INS.contains(name) {
                return true;
            }
        }

        if (r.language == Language::C || r.language == Language::Cpp)
            && r.reference_kind == EdgeKind::Calls
            && C_CPP_STDLIB_CALLS.contains(name)
            && !self.context.known_has(name)
        {
            return true;
        }
        if (r.language == Language::Java || r.language == Language::Kotlin)
            && r.reference_kind == EdgeKind::Calls
            && JVM_STDLIB_EXTERNAL_CALLS.contains(name)
            && !self.context.known_has(name)
        {
            return true;
        }
        if (r.language == Language::Java || r.language == Language::Kotlin)
            && r.reference_kind == EdgeKind::Imports
            && JVM_STDLIB_IMPORT_PREFIXES
                .iter()
                .any(|prefix| name.starts_with(prefix))
        {
            return true;
        }
        if (r.language == Language::Java || r.language == Language::Kotlin)
            && (r.reference_kind == EdgeKind::References
                || r.reference_kind == EdgeKind::Instantiates)
            && JVM_STDLIB_TYPES.contains(name)
            && !self.context.known_has(name)
        {
            return true;
        }
        if (r.language == Language::Java || r.language == Language::Kotlin)
            && r.reference_kind == EdgeKind::References
            && JVM_NAMESPACE_SEGMENTS.contains(name)
        {
            return true;
        }
        if r.language == Language::Pascal {
            if PASCAL_UNIT_PREFIXES.iter().any(|p| name.starts_with(p)) {
                return true;
            }
            if PASCAL_BUILT_INS.contains(name) {
                return true;
            }
        }
        if r.language == Language::Bash && BASH_BUILT_INS.contains(name) {
            return true;
        }
        if r.language == Language::Apex {
            if let Some(dot_idx) = name.find('.') {
                if dot_idx > 0 {
                    let receiver = &name[..dot_idx];
                    let method = &name[dot_idx + 1..];
                    if APEX_SYSTEM_TYPES.contains(receiver.to_lowercase().as_str())
                        && !self.context.known_has(receiver)
                        && !self.context.known_has(&capitalize_first(receiver))
                    {
                        return true;
                    }
                    if APEX_BUILT_IN_METHODS.contains(method.to_lowercase().as_str()) {
                        let capitalized = capitalize_first(receiver);
                        if !self.context.known_has(receiver)
                            && !self.context.known_has(&capitalized)
                        {
                            return true;
                        }
                    }
                }
            }
        }
        if r.language == Language::C || r.language == Language::Cpp {
            if name.starts_with("std::") {
                return true;
            }
            if C_BUILT_INS.contains(name) || CPP_BUILT_INS.contains(name) {
                return !self.has_any_possible_match(name);
            }
        }

        false
    }
}

impl engine::ResolutionPolicy for ReferenceResolver {
    fn is_built_in_or_external(&self, reference: &UnresolvedRef) -> bool {
        ReferenceResolver::is_built_in_or_external(self, reference)
    }

    fn has_any_possible_match(&self, name: &str) -> bool {
        ReferenceResolver::has_any_possible_match(self, name)
    }

    fn has_any_possible_match_ci(&self, name: &str) -> bool {
        ReferenceResolver::has_any_possible_match_ci(self, name)
    }

    fn matches_any_import(&self, reference: &UnresolvedRef) -> bool {
        ReferenceResolver::matches_any_import(self, reference)
    }
}
