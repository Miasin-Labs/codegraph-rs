use super::fixture::extract_ts;
use crate::types::{EdgeKind, NodeKind};

#[test]
fn extracts_file_node_and_functions_with_ts_compatible_ids() {
    let source = "export function add(a: number, b: number): number {\n  return helper(a) + b;\n}\nfunction helper(x: number) {\n  return x;\n}\n";
    let result = extract_ts("src/math.ts", source);
    assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

    let file = &result.nodes[0];
    assert_eq!(file.id, "file:src/math.ts");
    assert_eq!(file.kind, NodeKind::File);
    assert_eq!(file.name, "math.ts");
    assert_eq!(file.qualified_name, "src/math.ts");
    assert_eq!(file.start_line, 1);

    let add = result
        .nodes
        .iter()
        .find(|n| n.name == "add")
        .expect("add node");
    assert_eq!(add.kind, NodeKind::Function);
    assert_eq!(add.start_line, 1);
    assert_eq!(add.end_line, 3);
    assert_eq!(add.is_exported, Some(true));
    assert_eq!(add.qualified_name, "add");
    assert_eq!(
        add.id,
        crate::extraction::tree_sitter_helpers::generate_node_id(
            "src/math.ts",
            NodeKind::Function,
            "add",
            1
        )
    );
    assert!(add.id.starts_with("function:"));
    assert_eq!(add.id.len(), "function:".len() + 32);

    let helper = result
        .nodes
        .iter()
        .find(|n| n.name == "helper")
        .expect("helper node");
    assert_eq!(helper.is_exported, Some(false));
    assert_eq!(helper.start_line, 4);

    let call = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_name == "helper")
        .expect("helper call ref");
    assert_eq!(call.reference_kind, EdgeKind::Calls);
    assert_eq!(call.from_node_id, add.id);
    assert_eq!(call.line, 2);

    let file_contains: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.source == file.id && e.kind == EdgeKind::Contains)
        .collect();
    assert_eq!(file_contains.len(), 2);
}

#[test]
fn extracts_class_with_method_and_qualified_name() {
    let source =
        "class Greeter {\n  greet(who: string): string {\n    return hello(who);\n  }\n}\n";
    let result = extract_ts("src/greeter.ts", source);

    let class = result
        .nodes
        .iter()
        .find(|n| n.name == "Greeter")
        .expect("class node");
    assert_eq!(class.kind, NodeKind::Class);

    let method = result
        .nodes
        .iter()
        .find(|n| n.name == "greet")
        .expect("method node");
    assert_eq!(method.kind, NodeKind::Method);
    assert_eq!(method.qualified_name, "Greeter::greet");

    assert!(
        result
            .edges
            .iter()
            .any(|e| e.source == class.id && e.target == method.id && e.kind == EdgeKind::Contains)
    );

    let call = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_name == "hello")
        .expect("hello call");
    assert_eq!(call.from_node_id, method.id);
}

#[test]
fn extracts_import_node_and_reference() {
    let source = "import { x } from './mod';\n";
    let result = extract_ts("src/a.ts", source);

    let import = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Import)
        .expect("import node");
    assert_eq!(import.name, "./mod");
    assert_eq!(
        import.signature.as_deref(),
        Some("import { x } from './mod';")
    );

    let import_ref = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_kind == EdgeKind::Imports)
        .expect("imports ref");
    assert_eq!(import_ref.reference_name, "./mod");
    assert_eq!(import_ref.from_node_id, "file:src/a.ts");
}

#[test]
fn extracts_const_variable_with_initializer_signature() {
    let source = "export const NAME = 'value';\nlet counter = 0;\n";
    let result = extract_ts("src/vars.ts", source);

    let constant = result
        .nodes
        .iter()
        .find(|n| n.name == "NAME")
        .expect("NAME node");
    assert_eq!(constant.kind, NodeKind::Constant);
    assert_eq!(constant.signature.as_deref(), Some("= 'value'"));
    assert_eq!(constant.is_exported, Some(true));

    let variable = result
        .nodes
        .iter()
        .find(|n| n.name == "counter")
        .expect("counter node");
    assert_eq!(variable.kind, NodeKind::Variable);
    assert_eq!(variable.is_exported, Some(false));
}

#[test]
fn arrow_function_const_extracted_as_named_function() {
    let source = "export const useAuth = () => {\n  return login();\n};\n";
    let result = extract_ts("src/auth.ts", source);

    let func = result
        .nodes
        .iter()
        .find(|n| n.name == "useAuth")
        .expect("useAuth node");
    assert_eq!(func.kind, NodeKind::Function);
    assert_eq!(func.is_exported, Some(true));

    let call = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_name == "login")
        .expect("login call");
    assert_eq!(call.from_node_id, func.id);
}

#[test]
fn exported_store_object_functions_become_named_nodes() {
    let source = "export const useStore = create((set) => ({\n  fetchUser: async () => {\n    load();\n  },\n}));\n";
    let result = extract_ts("src/store.ts", source);

    let action = result
        .nodes
        .iter()
        .find(|n| n.name == "fetchUser")
        .expect("fetchUser node");
    assert_eq!(action.kind, NodeKind::Function);

    let call = result
        .unresolved_references
        .iter()
        .find(|r| r.reference_name == "load")
        .expect("load call");
    assert_eq!(call.from_node_id, action.id);
}
