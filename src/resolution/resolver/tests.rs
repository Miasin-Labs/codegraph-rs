use super::context::{is_js_family_path, is_low_value_js_ts_resolution_source};
use super::policy::{
    APEX_BUILT_IN_METHODS,
    APEX_SYSTEM_TYPES,
    BASH_BUILT_INS,
    C_BUILT_INS,
    CPP_BUILT_INS,
    GO_BUILT_INS,
    GO_STDLIB_PACKAGES,
    JS_BUILT_INS,
    PASCAL_BUILT_INS,
    PASCAL_UNIT_PREFIXES,
    PYTHON_BUILT_IN_METHODS,
    PYTHON_BUILT_IN_TYPES,
    PYTHON_BUILT_INS,
    REACT_HOOKS,
    capitalize_first,
};
use crate::resolution::types::UnresolvedRef;
use crate::types::{EdgeKind, Language};

#[test]
fn capitalize_first_matches_js() {
    assert_eq!(capitalize_first("recorder"), "Recorder");
    assert_eq!(capitalize_first("Recorder"), "Recorder");
    assert_eq!(capitalize_first(""), "");
    assert_eq!(capitalize_first("a"), "A");
}

#[test]
fn js_family_regex_matches_ts_pattern() {
    for path in [
        "a.ts",
        "a.tsx",
        "a.js",
        "a.jsx",
        "a.mts",
        "a.cts",
        "a.mjs",
        "a.cjs",
        "a.d.ts",
        "DIR/B.TSX",
    ] {
        assert!(is_js_family_path(path), "{path} should be JS-family");
    }
    for path in ["a.svelte", "a.vue", "a.py", "a.tsx.bak", "ats"] {
        assert!(!is_js_family_path(path), "{path} should NOT be JS-family");
    }
}

#[test]
fn low_value_js_ts_resolution_sources_are_skipped() {
    let reference = |file_path: &str, language: Language| UnresolvedRef {
        from_node_id: "node:src/app.js:caller:1".to_string(),
        reference_name: "_get".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        column: 1,
        file_path: file_path.to_string(),
        language,
        candidates: None,
    };

    assert!(is_low_value_js_ts_resolution_source(&reference(
        "assets/jquery-ui-1.11.4.min.js",
        Language::Javascript,
    )));
    assert!(is_low_value_js_ts_resolution_source(&reference(
        "assets/app.min.tsx",
        Language::Tsx,
    )));
    assert!(is_low_value_js_ts_resolution_source(&reference(
        "deobfuscated-bundles/bundle-1.deob.js",
        Language::Javascript,
    )));
    assert!(is_low_value_js_ts_resolution_source(&reference(
        "tmp/bundle-1.deob.js",
        Language::Javascript,
    )));
    assert!(!is_low_value_js_ts_resolution_source(&reference(
        "src/minifier.js",
        Language::Javascript,
    )));
    assert!(!is_low_value_js_ts_resolution_source(&reference(
        "src/runtime-bundle.js",
        Language::Javascript,
    )));
    assert!(!is_low_value_js_ts_resolution_source(&reference(
        "assets/site.min.css",
        Language::Javascript,
    )));
    assert!(!is_low_value_js_ts_resolution_source(&reference(
        "assets/tool.min.js",
        Language::Python,
    )));
}

#[test]
fn built_in_sets_have_ts_cardinalities() {
    assert_eq!(JS_BUILT_INS.len(), 28);
    assert_eq!(REACT_HOOKS.len(), 10);
    assert_eq!(PYTHON_BUILT_INS.len(), 23);
    assert_eq!(PYTHON_BUILT_IN_TYPES.len(), 13);
    assert_eq!(PYTHON_BUILT_IN_METHODS.len(), 45);
    assert_eq!(GO_STDLIB_PACKAGES.len(), 67);
    assert_eq!(GO_BUILT_INS.len(), 40);
    assert_eq!(PASCAL_UNIT_PREFIXES.len(), 15);
    assert_eq!(PASCAL_BUILT_INS.len(), 87);
    assert_eq!(C_BUILT_INS.len(), 137);
    assert_eq!(CPP_BUILT_INS.len(), 25);
    assert_eq!(APEX_SYSTEM_TYPES.len(), 46);
    assert_eq!(APEX_BUILT_IN_METHODS.len(), 42);
    assert_eq!(BASH_BUILT_INS.len(), 103);
}

#[test]
fn apex_built_in_sets_are_lowercase() {
    for value in APEX_SYSTEM_TYPES.iter().chain(APEX_BUILT_IN_METHODS.iter()) {
        assert_eq!(
            *value,
            value.to_lowercase(),
            "entry {value:?} must be lowercase"
        );
    }
}
