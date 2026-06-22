use super::{BridgeOptions, Node, NodeKind, OsString, engine_safe_field_name, field_type_from};

#[test]
fn bridge_options_env_gate_parsing() {
    let on = |v: &str| BridgeOptions::from_env_value(Some(OsString::from(v))).include_fields;
    assert!(!BridgeOptions::from_env_value(None).include_fields);
    assert!(on("1"));
    assert!(on("true"));
    assert!(on("TRUE"));
    assert!(on(" 1 "));
    assert!(!on("0"));
    assert!(!on(""));
    assert!(!on("yes"));
    assert!(!BridgeOptions::default().include_fields);
}

#[test]
fn field_type_heuristic_covers_both_signature_shapes() {
    let mk = |name: &str, sig: Option<&str>| {
        let mut n = Node::new(
            "field:x",
            NodeKind::Field,
            name,
            format!("S::{name}"),
            "src/s.ts",
            crate::types::Language::Typescript,
            1,
            1,
        );
        n.signature = sig.map(String::from);
        n
    };
    assert_eq!(field_type_from(&mk("host", Some("host: string"))), "string");
    assert_eq!(
        field_type_from(&mk("p", Some("p: std::path::PathBuf"))),
        "std::path::PathBuf"
    );
    assert_eq!(field_type_from(&mk("count", Some("int count"))), "int");
    assert_eq!(field_type_from(&mk("name", Some("string $name"))), "string");
    assert_eq!(field_type_from(&mk("x", None)), "");
    assert_eq!(field_type_from(&mk("x", Some("$x"))), "");
    assert_eq!(field_type_from(&mk("x", Some("unrelated"))), "");
    assert_eq!(field_type_from(&mk("x", Some("a;b x"))), "a b");

    assert!(engine_safe_field_name("ok_name"));
    assert!(!engine_safe_field_name(""));
    assert!(!engine_safe_field_name("a;b"));
    assert!(!engine_safe_field_name("a:b"));
    assert!(!engine_safe_field_name("a,b"));
}
