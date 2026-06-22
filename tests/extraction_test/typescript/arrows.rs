use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Arrow Function Export Extraction')
// =============================================================================

#[test]
fn arrow_fn_extracts_exported_arrow_functions_assigned_to_const() {
    let code = r#"
export const useAuth = (): AuthContextValue => {
  return useContext(AuthContext);
};
"#;
    let result = extract("hooks.ts", code);

    let func_node = find_named(&result, NodeKind::Function, "useAuth").expect("useAuth");
    assert_eq!(func_node.is_exported, Some(true));
}

#[test]
fn arrow_fn_extracts_exported_function_expressions_assigned_to_const() {
    let code = r#"
export const processData = function(input: string): string {
  return input.trim();
};
"#;
    let result = extract("utils.ts", code);

    let func_node = find_named(&result, NodeKind::Function, "processData").expect("processData");
    assert_eq!(func_node.is_exported, Some(true));
}

#[test]
fn arrow_fn_does_not_extract_non_exported_arrow_functions_as_exported() {
    let code = r#"
const internalHelper = () => {
  return 42;
};
"#;
    let result = extract("internal.ts", code);

    let helper_node = result
        .nodes
        .iter()
        .find(|n| n.name == "internalHelper")
        .expect("internalHelper");
    // toBeFalsy(): undefined or false both pass
    assert_ne!(helper_node.is_exported, Some(true));
}

#[test]
fn arrow_fn_still_skips_truly_anonymous_arrow_functions() {
    let code = r#"
const items = [1, 2, 3].map((x) => x * 2);
"#;
    let result = extract("anon.ts", code);

    // The inline arrow function passed to .map() has no variable_declarator parent
    // and should remain anonymous (skipped)
    let anon_functions: Vec<&Node> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name == "<anonymous>")
        .collect();
    assert_eq!(anon_functions.len(), 0);
}

#[test]
fn arrow_fn_extracts_multiple_exported_arrow_functions_from_the_same_file() {
    let code = r#"
export const add = (a: number, b: number): number => a + b;

export const subtract = (a: number, b: number): number => a - b;

const internal = () => 'not exported';
"#;
    let result = extract("math.ts", code);

    let exported: Vec<&Node> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.is_exported == Some(true))
        .collect();
    assert_eq!(exported.len(), 2);
    let mut exported_names = names(&exported);
    exported_names.sort();
    assert_eq!(exported_names, vec!["add", "subtract"]);

    let internal_node = result
        .nodes
        .iter()
        .find(|n| n.name == "internal")
        .expect("internal");
    assert_ne!(internal_node.is_exported, Some(true));
}

#[test]
fn arrow_fn_extracts_arrow_functions_in_javascript_files() {
    let code = r#"
export const fetchData = async () => {
  const response = await fetch('/api/data');
  return response.json();
};
"#;
    let result = extract("api.js", code);

    let func_node = find_named(&result, NodeKind::Function, "fetchData").expect("fetchData");
    assert_eq!(func_node.is_exported, Some(true));
}
