use super::*;

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
