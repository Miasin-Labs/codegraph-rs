use super::{Fixture, make_ref, match_reference, node};
use crate::resolution::types::ResolvedBy;
use crate::types::{EdgeKind, Language, NodeKind};

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
