use crate::extraction_test::fixture::*;

// =============================================================================
// describe('File Node Extraction')
// =============================================================================

#[test]
fn file_node_creates_a_file_kind_node_for_each_parsed_file() {
    let code = r#"
export function greet(name: string): string {
  return "Hello " + name;
}
"#;
    let result = extract("greeter.ts", code);

    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    assert_eq!(file_node.name, "greeter.ts");
    assert_eq!(file_node.file_path, "greeter.ts");
    assert_eq!(file_node.language, Language::Typescript);
    assert_eq!(file_node.start_line, 1);
}

#[test]
fn file_node_creates_file_nodes_for_python_files() {
    let code = r#"
def main():
    pass
"#;
    let result = extract("main.py", code);

    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    assert_eq!(file_node.name, "main.py");
    assert_eq!(file_node.language, Language::Python);
}

#[test]
fn file_node_creates_containment_edges_from_file_node_to_top_level_declarations() {
    let code = r#"
export function foo() {}
export function bar() {}
"#;
    let result = extract("fns.ts", code);

    let file_node = find_kind(&result, NodeKind::File).expect("file node");

    // There should be contains edges from the file node to each function
    let contains_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.source == file_node.id && e.kind == EdgeKind::Contains)
        .collect();
    assert!(contains_edges.len() >= 2);
}
