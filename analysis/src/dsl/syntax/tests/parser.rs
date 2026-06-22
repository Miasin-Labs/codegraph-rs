use crate::closure::ClosureDirection;
use crate::dsl::syntax::{DslOp, Projection, parse_query};
use crate::edges::EdgeKind;
use crate::label_reachability::{PatternAtom, Rep};
use crate::nodes::NodeKind;

#[test]
fn test_dsl_parse_simple() {
    let ops = parse_query(r#"fn("foo") | callees"#).unwrap();
    assert_eq!(ops, vec![DslOp::SelectFn("foo".into()), DslOp::Callees]);
}

#[test]
fn test_dsl_parse_depth() {
    let ops = parse_query(r#"fn("bar") | callees | depth 3"#).unwrap();
    assert_eq!(
        ops,
        vec![
            DslOp::SelectFn("bar".into()),
            DslOp::Callees,
            DslOp::Depth(3),
        ]
    );
}

#[test]
fn test_dsl_parse_full() {
    let ops = parse_query(r#"fn("x") | callers | depth 2 | filter kind=Function | show signature"#)
        .unwrap();
    assert_eq!(
        ops,
        vec![
            DslOp::SelectFn("x".into()),
            DslOp::Callers,
            DslOp::Depth(2),
            DslOp::Filter(NodeKind::Function),
            DslOp::Show(Projection::Signature),
        ]
    );
}

#[test]
fn test_dsl_parse_taint() {
    let ops = parse_query(r#"fn("process") | taint "user_input" | depth 5"#).unwrap();
    assert_eq!(
        ops,
        vec![
            DslOp::SelectFn("process".into()),
            DslOp::Taint("user_input".into()),
            DslOp::Depth(5),
        ]
    );
}

#[test]
fn test_dsl_parse_preconditions_normal() {
    let ops = parse_query(r#"fn("danger") | preconditions"#).unwrap();
    assert_eq!(
        ops,
        vec![DslOp::SelectFn("danger".into()), DslOp::Preconditions]
    );
}

#[test]
fn test_dsl_parse_preconditions_chain_robust() {
    let ops =
        parse_query(r#"fn("danger") | preconditions | filter kind=Function | depth 3"#).unwrap();
    assert_eq!(
        ops,
        vec![
            DslOp::SelectFn("danger".into()),
            DslOp::Preconditions,
            DslOp::Filter(NodeKind::Function),
            DslOp::Depth(3),
        ]
    );
}

#[test]
fn test_dsl_parse_type() {
    let ops = parse_query(r#"type("Config") | callees"#).unwrap();
    assert_eq!(
        ops,
        vec![DslOp::SelectType("Config".into()), DslOp::Callees]
    );
}

#[test]
fn test_dsl_parse_error_empty() {
    let err = parse_query("").unwrap_err();
    assert_eq!(err.position, 0);
    assert!(err.message.contains("empty"));
}

#[test]
fn test_dsl_parse_error_invalid_op() {
    let err = parse_query(r#"fn("x") | invalid_op"#).unwrap_err();
    assert!(err.position > 0);
    assert!(err.message.contains("unknown operation"));
    assert!(err.message.contains("invalid_op"));
}

#[test]
fn test_dsl_parse_error_missing_string() {
    let err = parse_query(r#"fn() | callees"#).unwrap_err();
    assert!(err.position > 0);
    assert!(err.message.contains("string"));
}

#[test]
fn dsl_reachable_via_parses_pattern_normal() {
    let ops = parse_query(r#"fn("m") | reachable via "Contains Calls+""#).unwrap();
    assert_eq!(
        ops,
        vec![
            DslOp::SelectFn("m".into()),
            DslOp::ReachableVia {
                pattern: vec![
                    PatternAtom::one(EdgeKind::Contains),
                    PatternAtom::plus(EdgeKind::Calls),
                ],
                direction: ClosureDirection::Outgoing,
            },
        ]
    );

    let ops = parse_query(r#"fn("m") | reachable via "Calls+" incoming"#).unwrap();
    assert_eq!(
        ops[1],
        DslOp::ReachableVia {
            pattern: vec![PatternAtom::plus(EdgeKind::Calls)],
            direction: ClosureDirection::Incoming,
        }
    );
    let ops = parse_query(r#"fn("m") | reachable via "any*" outgoing"#).unwrap();
    assert_eq!(
        ops[1],
        DslOp::ReachableVia {
            pattern: vec![PatternAtom::any(Rep::Star)],
            direction: ClosureDirection::Outgoing,
        }
    );
}

#[test]
fn dsl_reachable_via_rejects_bad_input_robust() {
    let err = parse_query(r#"fn("m") | reachable via "Bogus+""#).unwrap_err();
    assert!(err.to_string().contains("unknown edge label"), "{err}");

    let err = parse_query(r#"fn("m") | reachable "Calls+""#).unwrap_err();
    assert!(err.to_string().contains("expected 'via'"), "{err}");

    let err = parse_query(r#"fn("m") | reachable via "Calls+" sideways"#).unwrap_err();
    assert!(err.to_string().contains("unknown direction"), "{err}");

    let err = parse_query(r#"fn("m") | reachable via"#).unwrap_err();
    assert!(err.to_string().contains("expected pattern string"), "{err}");
}
