use super::fixture::extract_ts;
use crate::types::EdgeKind;

#[test]
fn extracts_class_inheritance_references() {
    let source = "interface Base {}\nclass Impl extends Parent implements Base {\n  run() {}\n}\n";
    let result = extract_ts("src/impl.ts", source);

    let impl_node = result
        .nodes
        .iter()
        .find(|n| n.name == "Impl")
        .expect("Impl node");
    let extends_ref = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Extends)
        .expect("extends ref");
    assert_eq!(extends_ref.reference_name, "Parent");
    assert_eq!(extends_ref.from_node_id, impl_node.id);
    let implements_ref = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Implements)
        .expect("implements ref");
    assert_eq!(implements_ref.reference_name, "Base");
}

#[test]
fn preceding_decorators_attach_to_their_own_declaration() {
    // Decorators before `export` are siblings of the class, not children;
    // each run stops at the previous declaration, so `@A` stays on `Foo`.
    let source = "@A()\nexport class Foo {}\nfunction gap() {}\n@B\n@C()\nexport class Bar {}\n";
    let result = extract_ts("src/decorated.ts", source);

    let id_of = |name: &str| {
        result
            .nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("{name} node"))
            .id
            .clone()
    };
    let mut decorations: Vec<(String, String)> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Decorates)
        .map(|r| (r.from_node_id.clone(), r.reference_name.clone()))
        .collect();
    decorations.sort();
    let mut expected = vec![
        (id_of("Foo"), "A".to_string()),
        (id_of("Bar"), "B".to_string()),
        (id_of("Bar"), "C".to_string()),
    ];
    expected.sort();
    assert_eq!(decorations, expected);
}

#[test]
fn instantiation_inside_function_body_emits_instantiates_ref() {
    let source = "function build() {\n  const m = new ns.Mapper<string>();\n}\n";
    let result = extract_ts("src/build.ts", source);

    let build = result
        .nodes
        .iter()
        .find(|n| n.name == "build")
        .expect("build node");
    let inst = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Instantiates)
        .expect("instantiates ref");
    assert_eq!(inst.reference_name, "Mapper");
    assert_eq!(inst.from_node_id, build.id);
}

#[test]
fn decorated_class_emits_decorates_reference() {
    let source = "@Injectable()\nclass Service {\n  run() {}\n}\n";
    let result = extract_ts("src/service.ts", source);

    let service = result
        .nodes
        .iter()
        .find(|n| n.name == "Service")
        .expect("Service node");
    let dec = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Decorates)
        .expect("decorates ref");
    assert_eq!(dec.reference_name, "Injectable");
    assert_eq!(dec.from_node_id, service.id);
}

#[test]
fn function_docstrings_are_the_comments_right_before_them() {
    let source = "/** Adds. */\n// twice\nfunction add() {}\nconst gap = 1;\n// Subtracts.\nfunction sub() {}\nfunction bare() {}\n";
    let result = extract_ts("src/docs.ts", source);

    let doc = |name: &str| {
        result
            .nodes
            .iter()
            .find(|n| n.name == name)
            .unwrap_or_else(|| panic!("{name} node"))
            .docstring
            .clone()
    };
    assert_eq!(doc("add").as_deref(), Some("Adds.\ntwice"));
    assert_eq!(doc("sub").as_deref(), Some("Subtracts."));
    assert_eq!(doc("bare"), None);
}
