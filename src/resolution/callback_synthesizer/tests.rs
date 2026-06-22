use super::channels::{CC_APPEND_DIRECT_RE, CC_APPEND_WRITE_RE, CC_DISPATCH_RE};
use super::gin::{go_balanced_args, go_handler_ident, go_split_args};
use super::ordered::{OrderedMap, OrderedSet};
use super::source::{
    dispatcher_field,
    enclosing_fn,
    kebab_to_pascal,
    registrar_field,
    slice_lines,
};
use crate::types::{Language, Node, NodeKind};

#[test]
fn kebab_to_pascal_matches_ts() {
    assert_eq!(kebab_to_pascal("el-button"), "ElButton");
    assert_eq!(kebab_to_pascal("my-fancy-tag"), "MyFancyTag");
    assert_eq!(kebab_to_pascal("single"), "Single");
}

#[test]
fn slice_lines_matches_ts_semantics() {
    let content = "a\nb\nc\nd";
    assert_eq!(slice_lines(content, 2, 3).unwrap(), "b\nc");
    assert_eq!(slice_lines(content, 1, 99).unwrap(), "a\nb\nc\nd");
    // falsy bounds → None (TS `if (!startLine || !endLine) return null`)
    assert!(slice_lines(content, 0, 3).is_none());
    assert!(slice_lines(content, 2, 0).is_none());
    // out-of-range start → empty (TS slice clamps to [])
    assert_eq!(slice_lines(content, 10, 12).unwrap(), "");
}

#[test]
fn registrar_and_dispatcher_field_extraction() {
    assert_eq!(
        registrar_field("onUpdate(cb) { this.callbacks.add(cb); }").as_deref(),
        Some("callbacks")
    );
    assert_eq!(
        registrar_field("onX(cb) { this.listeners.push(cb); }").as_deref(),
        Some("listeners")
    );
    assert!(registrar_field("noop() { return 1; }").is_none());

    // for-of + an invocation in the body
    assert_eq!(
        dispatcher_field("triggerUpdate() { for (const cb of this.callbacks) cb(); }").as_deref(),
        Some("callbacks")
    );
    // Array.from variant
    assert_eq!(
        dispatcher_field("t() { for (const cb of Array.from( this.subs)) cb(); }").as_deref(),
        Some("subs")
    );
    // forEach variant
    assert_eq!(
        dispatcher_field("notify() { this.watchers.forEach((w) => w()); }").as_deref(),
        Some("watchers")
    );
    assert!(dispatcher_field("idle() { return; }").is_none());
}

#[test]
fn closure_collection_regexes_match_swift_shapes() {
    let caps = CC_DISPATCH_RE
        .captures("validators.forEach { $0() }")
        .unwrap();
    assert_eq!(&caps[1], "validators");
    // Kotlin `it()` form
    assert!(CC_DISPATCH_RE.is_match("handlers.forEach { it() }"));
    // non-invoking forEach is NOT a dispatcher
    assert!(!CC_DISPATCH_RE.is_match("names.forEach { print($0) }"));

    let w = CC_APPEND_WRITE_RE
        .captures("validators.write { $0.append(validator) }")
        .unwrap();
    assert_eq!(&w[1], "validators");
    assert!(w.get(2).is_none());
    // nested property form captures group 2
    let w2 = CC_APPEND_WRITE_RE
        .captures("state.write { $0.streams.append(stream) }")
        .unwrap();
    assert_eq!(&w2[1], "state");
    assert_eq!(&w2[2], "streams");

    // direct append mis-captures `$0` as `0` — rejected by the digits guard
    let a = CC_APPEND_DIRECT_RE
        .captures("$0.append(validator)")
        .unwrap();
    assert_eq!(&a[1], "0");
}

#[test]
fn enclosing_fn_prefers_tightest_encloser() {
    let outer = Node::new(
        "outer",
        NodeKind::Function,
        "outer",
        "outer",
        "a.ts",
        Language::Typescript,
        1,
        20,
    );
    let inner = Node::new(
        "inner",
        NodeKind::Method,
        "inner",
        "inner",
        "a.ts",
        Language::Typescript,
        5,
        10,
    );
    let not_fn = Node::new(
        "cls",
        NodeKind::Class,
        "cls",
        "cls",
        "a.ts",
        Language::Typescript,
        1,
        20,
    );
    let nodes = vec![outer, inner, not_fn];
    assert_eq!(enclosing_fn(&nodes, 7).unwrap().id, "inner");
    assert_eq!(enclosing_fn(&nodes, 15).unwrap().id, "outer");
    assert!(enclosing_fn(&nodes, 25).is_none());
}

#[test]
fn go_helpers_match_ts() {
    // balanced args
    let s = r#"r.GET("/ping", gin.Logger(), handlePing)"#;
    let open = s.find('(').unwrap();
    assert_eq!(
        go_balanced_args(s, open).unwrap(),
        r#""/ping", gin.Logger(), handlePing"#
    );
    assert!(go_balanced_args("(unbalanced", 0).is_none());

    // split args respecting nesting
    assert_eq!(
        go_split_args(r#""/p", f(a, b), g"#),
        vec![
            r#""/p""#.to_string(),
            " f(a, b)".to_string(),
            " g".to_string()
        ]
    );

    // handler ident
    assert_eq!(go_handler_ident("gin.Logger()").as_deref(), Some("Logger"));
    assert_eq!(
        go_handler_ident(" authMiddleware ").as_deref(),
        Some("authMiddleware")
    );
    assert!(go_handler_ident(r#""/path""#).is_none());
    assert!(go_handler_ident("func(c *gin.Context) {}").is_none());
    assert!(go_handler_ident("`raw`").is_none());
}

#[test]
fn ordered_map_and_set_preserve_insertion_order() {
    let mut m: OrderedMap<u32> = OrderedMap::new();
    m.set("b", 1);
    m.set("a", 2);
    m.set("b", 3); // update keeps position (JS Map.set parity)
    let entries: Vec<(&str, u32)> = m.iter().map(|(k, v)| (k, *v)).collect();
    assert_eq!(entries, vec![("b", 3), ("a", 2)]);

    let mut s = OrderedSet::default();
    s.add("x");
    s.add("y");
    s.add("x");
    let items: Vec<&String> = s.iter().collect();
    assert_eq!(items, vec!["x", "y"]);
    assert_eq!(s.len(), 2);
}
