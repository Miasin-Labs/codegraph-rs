use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Exported Variable Extraction')
// =============================================================================

#[test]
fn exported_var_extracts_exported_const_with_call_expression_zustand_store() {
    let code = r#"
export const useUIStore = create<UIState>((set) => ({
  isOpen: false,
  toggle: () => set((s) => ({ isOpen: !s.isOpen })),
}));
"#;
    let result = extract("store.ts", code);

    let var_node = find_named(&result, NodeKind::Constant, "useUIStore").expect("useUIStore");
    assert_eq!(var_node.is_exported, Some(true));
}

#[test]
fn exported_var_extracts_exported_const_with_object_literal() {
    let code = r#"
export const config = {
  apiUrl: 'https://api.example.com',
  timeout: 5000,
};
"#;
    let result = extract("config.ts", code);

    let var_node = find_named(&result, NodeKind::Constant, "config").expect("config");
    assert_eq!(var_node.is_exported, Some(true));
}

#[test]
fn exported_var_extracts_exported_const_with_array_literal() {
    let code = r#"
export const SCREEN_NAMES = ['home', 'settings', 'profile'] as const;
"#;
    let result = extract("constants.ts", code);

    let var_node = find_named(&result, NodeKind::Constant, "SCREEN_NAMES").expect("SCREEN_NAMES");
    assert_eq!(var_node.is_exported, Some(true));
}

#[test]
fn exported_var_extracts_exported_const_with_primitive_value() {
    let code = r#"
export const MAX_RETRIES = 3;
export const API_VERSION = "v2";
"#;
    let result = extract("constants.ts", code);

    let variables = filter_kind(&result, NodeKind::Constant);
    assert_eq!(variables.len(), 2);
    let mut variable_names = names(&variables);
    variable_names.sort();
    assert_eq!(variable_names, vec!["API_VERSION", "MAX_RETRIES"]);
}

#[test]
fn exported_var_does_not_duplicate_arrow_functions_as_both_function_and_variable() {
    let code = r#"
export const useAuth = () => {
  return useContext(AuthContext);
};
"#;
    let result = extract("hooks.ts", code);

    // Should be extracted as function (from arrow function handler), NOT as variable
    let func_nodes: Vec<&Node> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Function && n.name == "useAuth")
        .collect();
    let var_nodes: Vec<&Node> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Variable && n.name == "useAuth")
        .collect();
    assert_eq!(func_nodes.len(), 1);
    assert_eq!(var_nodes.len(), 0);
}

#[test]
fn exported_var_extracts_non_exported_const_as_non_exported_variable() {
    let code = r#"
const internalConfig = {
  debug: true,
};
"#;
    let result = extract("internal.ts", code);

    // Non-exported const at file level should be extracted as a constant (not exported)
    let var_nodes: Vec<&Node> = result
        .nodes
        .iter()
        .filter(|n| {
            (n.kind == NodeKind::Variable || n.kind == NodeKind::Constant)
                && n.name == "internalConfig"
        })
        .collect();
    assert_eq!(var_nodes.len(), 1);
    assert_ne!(var_nodes[0].is_exported, Some(true));
}

#[test]
fn exported_var_extracts_zod_schema_exports() {
    let code = r#"
export const userSchema = z.object({
  id: z.string(),
  name: z.string(),
  email: z.string().email(),
});
"#;
    let result = extract("schemas.ts", code);

    let var_node = find_named(&result, NodeKind::Constant, "userSchema").expect("userSchema");
    assert_eq!(var_node.is_exported, Some(true));
}

#[test]
fn exported_var_extracts_xstate_machine_exports() {
    let code = r#"
export const authMachine = createMachine({
  id: "auth",
  initial: "idle",
  states: {
    idle: {},
    authenticated: {},
  },
});
"#;
    let result = extract("machine.ts", code);

    let var_node = find_named(&result, NodeKind::Constant, "authMachine").expect("authMachine");
    assert_eq!(var_node.is_exported, Some(true));
}

#[test]
fn exported_var_extracts_calls_from_a_top_level_variable_initializer_issue_425() {
    let code = r#"
import { getTokenMp } from './api/upload';

const token = getTokenMp();
"#;
    let result = extract("app.ts", code);

    let call = find_ref(&result, EdgeKind::Calls, "getTokenMp");
    assert!(call.is_some());
}
