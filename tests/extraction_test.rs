//! Extraction Tests
//!
//! Port of `__tests__/extraction.test.ts` (the extraction crown-jewel suite)
//! plus the extraction half of `__tests__/object-literal-methods.test.ts`.
//!
//! Test names are `<describe>_<it>` snake-cased. Fixture source strings are
//! byte-identical to the TS suite. Tests that needed `CodeGraph.init` +
//! reference resolution (IDA callers/callees, store-action caller resolution)
//! are deferred to the public-API wiring wave and noted in
//! `notes/extraction-orchestrator.md`; the `Full Indexing` describe block is
//! ported here against `ExtractionOrchestrator` + `QueryBuilder` directly
//! (the layers under `CodeGraph`).

use std::fs;
use std::path::Path;
use std::process::Command;

use codegraph::db::{DatabaseConnection, QueryBuilder};
use codegraph::extraction::{
    ExtractionOrchestrator,
    detect_language,
    extract_from_source,
    get_supported_languages,
    init_grammars,
    is_ida_generated_c,
    is_language_supported,
    is_source_file,
    load_all_grammars,
    scan_directory,
};
use codegraph::types::{
    EdgeKind,
    ExtractionResult,
    Language,
    Node,
    NodeKind,
    UnresolvedReference,
    Visibility,
};
use codegraph::utils::normalize_path;

// =============================================================================
// Helpers (the TS suite's beforeAll + inline lambdas)
// =============================================================================

/// `extractFromSource(path, code)` — the two-arg TS call shape.
fn extract(path: &str, code: &str) -> ExtractionResult {
    init_grammars();
    load_all_grammars();
    extract_from_source(path, code, None, None)
}

fn find_kind(result: &ExtractionResult, kind: NodeKind) -> Option<&Node> {
    result.nodes.iter().find(|n| n.kind == kind)
}

fn filter_kind(result: &ExtractionResult, kind: NodeKind) -> Vec<&Node> {
    result.nodes.iter().filter(|n| n.kind == kind).collect()
}

fn find_named<'a>(result: &'a ExtractionResult, kind: NodeKind, name: &str) -> Option<&'a Node> {
    result
        .nodes
        .iter()
        .find(|n| n.kind == kind && n.name == name)
}

fn names(nodes: &[&Node]) -> Vec<String> {
    nodes.iter().map(|n| n.name.clone()).collect()
}

fn refs_of_kind(result: &ExtractionResult, kind: EdgeKind) -> Vec<&UnresolvedReference> {
    result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == kind)
        .collect()
}

fn find_ref<'a>(
    result: &'a ExtractionResult,
    kind: EdgeKind,
    name: &str,
) -> Option<&'a UnresolvedReference> {
    result
        .unresolved_references
        .iter()
        .find(|r| r.reference_kind == kind && r.reference_name == name)
}

fn ref_names(refs: &[&UnresolvedReference]) -> Vec<String> {
    refs.iter().map(|r| r.reference_name.clone()).collect()
}

/// The `CodeGraph.initSync(tempDir)` layers used directly: a `.codegraph` DB +
/// QueryBuilder + orchestrator over the project dir.
fn open_graph(dir: &Path) -> (DatabaseConnection, QueryBuilder) {
    let cg_dir = dir.join(".codegraph");
    fs::create_dir_all(&cg_dir).unwrap();
    let conn = DatabaseConnection::initialize(cg_dir.join("codegraph.db")).expect("initialize db");
    let db = conn.get_db().expect("get db");
    (conn, QueryBuilder::new(db))
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("git spawn");
    assert!(status.success(), "git {:?} failed", args);
}

// =============================================================================
// describe('Language Detection')
// =============================================================================

#[test]
fn language_detection_typescript_files() {
    assert_eq!(detect_language("src/index.ts", None), Language::Typescript);
    assert_eq!(
        detect_language("components/Button.tsx", None),
        Language::Tsx
    );
}

#[test]
fn language_detection_javascript_files() {
    assert_eq!(detect_language("index.js", None), Language::Javascript);
    assert_eq!(detect_language("App.jsx", None), Language::Jsx);
    assert_eq!(detect_language("config.mjs", None), Language::Javascript);
}

#[test]
fn language_detection_python_files() {
    assert_eq!(detect_language("main.py", None), Language::Python);
}

#[test]
fn language_detection_go_files() {
    assert_eq!(detect_language("main.go", None), Language::Go);
}

#[test]
fn language_detection_rust_files() {
    assert_eq!(detect_language("lib.rs", None), Language::Rust);
}

#[test]
fn language_detection_java_files() {
    assert_eq!(detect_language("Main.java", None), Language::Java);
}

#[test]
fn language_detection_c_files() {
    assert_eq!(detect_language("main.c", None), Language::C);
    assert_eq!(detect_language("utils.h", None), Language::C);
}

#[test]
fn language_detection_cpp_files() {
    assert_eq!(detect_language("main.cpp", None), Language::Cpp);
    assert_eq!(detect_language("class.hpp", None), Language::Cpp);
}

#[test]
fn language_detection_csharp_files() {
    assert_eq!(detect_language("Program.cs", None), Language::Csharp);
}

#[test]
fn language_detection_php_files() {
    assert_eq!(detect_language("index.php", None), Language::Php);
}

#[test]
fn language_detection_ruby_files() {
    assert_eq!(detect_language("app.rb", None), Language::Ruby);
}

#[test]
fn language_detection_swift_files() {
    assert_eq!(
        detect_language("ViewController.swift", None),
        Language::Swift
    );
}

#[test]
fn language_detection_kotlin_files() {
    assert_eq!(detect_language("MainActivity.kt", None), Language::Kotlin);
    assert_eq!(detect_language("build.gradle.kts", None), Language::Kotlin);
}

#[test]
fn language_detection_dart_files() {
    assert_eq!(detect_language("main.dart", None), Language::Dart);
}

#[test]
fn language_detection_objective_c_files() {
    assert_eq!(detect_language("AppDelegate.m", None), Language::Objc);
    assert_eq!(detect_language("ViewController.mm", None), Language::Objc);
    let objc_header = "@interface Foo : NSObject\n@end\n";
    assert_eq!(detect_language("Foo.h", Some(objc_header)), Language::Objc);
    assert_eq!(
        detect_language("stdio.h", Some("#ifndef STDIO_H\nvoid printf();\n#endif\n")),
        Language::C
    );
}

#[test]
fn language_detection_unknown_for_unsupported_extensions() {
    assert_eq!(detect_language("styles.css", None), Language::Unknown);
    assert_eq!(detect_language("data.json", None), Language::Unknown);
}

// =============================================================================
// describe('Language Support')
// =============================================================================

#[test]
fn language_support_reports_supported_languages() {
    assert!(is_language_supported(Language::Typescript));
    assert!(is_language_supported(Language::Python));
    assert!(is_language_supported(Language::Go));
    assert!(!is_language_supported(Language::Unknown));
}

#[test]
fn language_support_lists_all_supported_languages() {
    let languages = get_supported_languages();
    for lang in [
        Language::Typescript,
        Language::Javascript,
        Language::Python,
        Language::Go,
        Language::Rust,
        Language::Java,
        Language::Csharp,
        Language::Php,
        Language::Ruby,
        Language::Swift,
        Language::Kotlin,
        Language::Dart,
    ] {
        assert!(languages.contains(&lang), "missing {lang}");
    }
}

// =============================================================================
// describe('IDA C Extraction')
// =============================================================================

#[test]
fn ida_c_extracts_leading_dot_thunk_functions_and_resolved_target_calls() {
    let code = r#"
// ============================================================
// THUNK / TRAMPOLINE
// ============================================================
// Address:         0x19E10
// Name:            .mysql_init
// Disassembly:
//   0x19E10 jmp     cs:off_23E6F0
//
// Resolved target: mysql_init
// Target address:  0x2442C0
// ============================================================

void .mysql_init(/* see target signature */)
{
    return mysql_init(/* forwarded args */);
}
"#;

    assert!(is_ida_generated_c("lumina/all/.mysql_init.c", code));

    let result = extract("lumina/all/.mysql_init.c", code);
    let thunk = find_kind(&result, NodeKind::Function);
    assert_eq!(thunk.map(|n| n.name.as_str()), Some(".mysql_init"));

    let calls = refs_of_kind(&result, EdgeKind::Calls);
    assert!(calls.iter().any(|r| r.reference_name == "mysql_init"));
}

#[test]
fn ida_c_extracts_hexrays_sub_functions_and_call_references_without_tree_sitter() {
    let code = r#"
__int64 __fastcall sub_E2F10(__int64 a1, int a2, unsigned __int8 *a3)
{
  __int64 v4; // rax

  v4 = *(int *)(a1 + 144);
  LODWORD(v4) = 1;
  if ( (int)v4 + a2 > *(_DWORD *)(a1 + 148) )
  {
    if ( (unsigned int)sub_E15B0(a1) )
      return 0;
  }
  return tag_strlen((const char *)a3);
}
"#;

    assert!(is_ida_generated_c("lumina/all/sub_E2F10.c", code));

    let result = extract("lumina/all/sub_E2F10.c", code);
    let func = find_kind(&result, NodeKind::Function);
    assert_eq!(func.map(|n| n.name.as_str()), Some("sub_E2F10"));

    let call_names = ref_names(&refs_of_kind(&result, EdgeKind::Calls));
    assert!(call_names.contains(&"sub_E15B0".to_string()));
    assert!(call_names.contains(&"tag_strlen".to_string()));
    assert!(!call_names.contains(&"if".to_string()));
    assert!(!call_names.contains(&"_DWORD".to_string()));
    assert!(!call_names.contains(&"LODWORD".to_string()));
}

#[test]
fn ida_c_extracts_parameters_locals_and_type_edges() {
    let code = r#"
ida_mcp::mcp::Response *__fastcall ida_mcp::tools::debugger::make_response(
        ida_mcp::tools::debugger *this,
        ida_mcp::mcp::McpServer *server)
{
  ida_mcp::mcp::RequestContext *ctx; // [rsp+0h] [rbp-8h] BYREF
  int status; // eax

  status = ida_mcp::mcp::build_response(ctx, server);
  return ctx;
}
"#;

    let path = "ida_mcp/all/_ZN7ida_mcp5tools8debugger13make_response.c";
    assert!(is_ida_generated_c(path, code));

    let result = extract(path, code);
    let func = find_kind(&result, NodeKind::Function).expect("function node");
    assert_eq!(func.name, "make_response");
    assert_eq!(
        func.qualified_name,
        "ida_mcp::tools::debugger::make_response"
    );

    let parameter_names = names(&filter_kind(&result, NodeKind::Parameter));
    assert_eq!(parameter_names, vec!["this", "server"]);

    let variable_names = names(&filter_kind(&result, NodeKind::Variable));
    assert_eq!(variable_names, vec!["ctx", "status"]);

    let mut type_names: Vec<String> = filter_kind(&result, NodeKind::TypeAlias)
        .iter()
        .map(|n| n.qualified_name.clone())
        .collect();
    type_names.sort();
    assert_eq!(
        type_names,
        vec![
            "ida_mcp::mcp::McpServer",
            "ida_mcp::mcp::RequestContext",
            "ida_mcp::mcp::Response",
            "ida_mcp::tools::debugger",
        ]
    );

    let return_refs = refs_of_kind(&result, EdgeKind::Returns);
    assert!(
        return_refs
            .iter()
            .any(|r| r.reference_name == "ida_mcp::mcp::Response")
    );

    let type_refs = refs_of_kind(&result, EdgeKind::TypeOf);
    assert!(
        type_refs
            .iter()
            .any(|r| r.reference_name == "ida_mcp::mcp::McpServer")
    );
    assert!(
        type_refs
            .iter()
            .any(|r| r.reference_name == "ida_mcp::mcp::RequestContext")
    );

    let call_refs = refs_of_kind(&result, EdgeKind::Calls);
    assert!(
        call_refs
            .iter()
            .any(|r| r.reference_name == "ida_mcp::mcp::build_response")
    );
}

// 'should resolve IDA sub callers and callees after indexAll' requires the
// CodeGraph public API + reference resolution — deferred to the wiring wave
// (see notes/extraction-orchestrator.md).

#[test]
fn ida_c_does_not_classify_ordinary_c_files_as_ida_dumps() {
    let code = r#"
int main(void) {
  return puts("hello");
}
"#;
    assert!(!is_ida_generated_c("src/main.c", code));
}

#[test]
fn ida_c_indexes_oversized_ida_dumps_with_the_lightweight_extractor() {
    let temp_dir = tempfile::tempdir().unwrap();

    let file_path = temp_dir.path().join("sub_A743A0.c");
    let large_padding = "// IDA padding\n".repeat(90_000);
    fs::write(
        &file_path,
        format!(
            "__int64 __fastcall sub_A743A0(__int64 a1)\n{{\n  sub_A743B0(a1);\n{large_padding}  return 0;\n}}\n"
        ),
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch
        .index_files(&["sub_A743A0.c".to_string()])
        .expect("index_files");
    assert!(
        !result
            .errors
            .iter()
            .any(|e| e.code.as_deref() == Some("size_exceeded"))
    );
    let nodes = queries.get_nodes_by_file("sub_A743A0.c").unwrap();
    assert!(
        nodes
            .iter()
            .any(|n| n.kind == NodeKind::Function && n.name == "sub_A743A0")
    );
}

// =============================================================================
// describe('TypeScript Extraction')
// =============================================================================

#[test]
fn typescript_extracts_function_declarations() {
    let code = r#"
export function processPayment(amount: number): Promise<Receipt> {
  return stripe.charge(amount);
}
"#;
    let result = extract("payment.ts", code);

    // File node + function node
    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    assert_eq!(file_node.name, "payment.ts");

    let func_node = find_kind(&result, NodeKind::Function).expect("function node");
    assert_eq!(func_node.name, "processPayment");
    assert_eq!(func_node.language, Language::Typescript);
    assert_eq!(func_node.is_exported, Some(true));
    assert!(
        func_node
            .signature
            .as_deref()
            .unwrap_or_default()
            .contains("amount: number")
    );
}

#[test]
fn typescript_extracts_class_declarations() {
    let code = r#"
export class PaymentService {
  private stripe: StripeClient;

  constructor(apiKey: string) {
    this.stripe = new StripeClient(apiKey);
  }

  async charge(amount: number): Promise<Receipt> {
    return this.stripe.charge(amount);
  }
}
"#;
    let result = extract("service.ts", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class node");
    let method_nodes = filter_kind(&result, NodeKind::Method);

    assert_eq!(class_node.name, "PaymentService");
    assert_eq!(class_node.is_exported, Some(true));

    assert!(!method_nodes.is_empty());
    assert!(method_nodes.iter().any(|m| m.name == "charge"));
}

#[test]
fn typescript_extracts_interfaces() {
    let code = r#"
export interface User {
  id: string;
  name: string;
  email: string;
}
"#;
    let result = extract("types.ts", code);

    assert!(find_kind(&result, NodeKind::File).is_some());

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface node");
    assert_eq!(iface_node.name, "User");
    assert_eq!(iface_node.is_exported, Some(true));
}

#[test]
fn typescript_extracts_type_refs_from_interface_property_signatures() {
    let code = r#"
import type { IPage } from '../PromoterList';
import type { IOrderField } from '../types';

interface Hprops {
  value?: Partial<IPage> & Partial<IOrderField>;
}
"#;
    let result = extract("HeaderFilter.ts", code);

    let refs = refs_of_kind(&result, EdgeKind::References);
    assert!(refs.iter().any(|r| r.reference_name == "IPage"));
    assert!(refs.iter().any(|r| r.reference_name == "IOrderField"));
}

#[test]
fn typescript_extracts_type_refs_from_interface_method_signatures() {
    let code = r#"
import type { IPage } from '../PromoterList';
import type { IOrderField } from '../types';

interface MethodForm {
  fetchPage(arg: IPage): IOrderField;
}
"#;
    let result = extract("MethodForm.ts", code);

    let refs = refs_of_kind(&result, EdgeKind::References);
    assert!(refs.iter().any(|r| r.reference_name == "IPage"));
    assert!(refs.iter().any(|r| r.reference_name == "IOrderField"));
}

#[test]
fn typescript_tracks_function_calls() {
    let code = r#"
function main() {
  const result = processData();
  console.log(result);
}
"#;
    let result = extract("main.ts", code);

    assert!(!result.unresolved_references.is_empty());
    let calls = refs_of_kind(&result, EdgeKind::Calls);
    assert!(calls.iter().any(|c| c.reference_name == "processData"));
}

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

// =============================================================================
// describe('Type Alias Extraction')
// =============================================================================

#[test]
fn type_alias_extracts_exported_type_aliases_in_typescript() {
    let code = r#"
export type AuthContextValue = {
  user: User | null;
  login: () => void;
  logout: () => void;
};
"#;
    let result = extract("types.ts", code);

    let type_node = find_kind(&result, NodeKind::TypeAlias).expect("type_alias");
    assert_eq!(type_node.name, "AuthContextValue");
    assert_eq!(type_node.is_exported, Some(true));
}

#[test]
fn type_alias_extracts_non_exported_type_aliases() {
    let code = r#"
type InternalState = {
  loading: boolean;
  error: string | null;
};
"#;
    let result = extract("internal.ts", code);

    let type_node = find_kind(&result, NodeKind::TypeAlias).expect("type_alias");
    assert_eq!(type_node.name, "InternalState");
    assert_eq!(type_node.is_exported, Some(false));
}

#[test]
fn type_alias_extracts_multiple_type_aliases_from_the_same_file() {
    let code = r#"
export type UnitSystem = 'metric' | 'imperial';
export type DateFormat = 'ISO' | 'US' | 'EU';
type Internal = string;
"#;
    let result = extract("config.ts", code);

    let type_aliases = filter_kind(&result, NodeKind::TypeAlias);
    assert_eq!(type_aliases.len(), 3);

    let exported: Vec<&&Node> = type_aliases
        .iter()
        .filter(|n| n.is_exported == Some(true))
        .collect();
    assert_eq!(exported.len(), 2);
    let mut exported_names: Vec<String> = exported.iter().map(|n| n.name.clone()).collect();
    exported_names.sort();
    assert_eq!(exported_names, vec!["DateFormat", "UnitSystem"]);
}

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

// =============================================================================
// describe('Python Extraction')
// =============================================================================

#[test]
fn python_extracts_function_definitions() {
    let code = r#"
def calculate_total(items: list, tax_rate: float) -> float:
    """Calculate total with tax."""
    subtotal = sum(item.price for item in items)
    return subtotal * (1 + tax_rate)
"#;
    let result = extract("calc.py", code);

    assert!(find_kind(&result, NodeKind::File).is_some());

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "calculate_total");
    assert_eq!(func_node.language, Language::Python);
}

#[test]
fn python_extracts_class_definitions() {
    let code = r#"
class UserService:
    """Service for managing users."""

    def __init__(self, db):
        self.db = db

    def get_user(self, user_id: str) -> User:
        return self.db.find_user(user_id)
"#;
    let result = extract("service.py", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserService");
}

// =============================================================================
// describe('Go Extraction')
// =============================================================================

#[test]
fn go_extracts_function_declarations() {
    let code = r#"
package main

func ProcessOrder(order Order) (Receipt, error) {
    // Process the order
    return Receipt{}, nil
}
"#;
    let result = extract("main.go", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "ProcessOrder");
}

#[test]
fn go_extracts_method_declarations() {
    let code = r#"
package main

type Service struct {
    db *Database
}

func (s *Service) GetUser(id string) (*User, error) {
    return s.db.FindUser(id)
}
"#;
    let result = extract("service.go", code);

    let method_node = find_kind(&result, NodeKind::Method).expect("method");
    assert_eq!(method_node.name, "GetUser");
}

// =============================================================================
// describe('Rust Extraction')
// =============================================================================

#[test]
fn rust_extracts_function_declarations() {
    let code = r#"
pub fn process_data(input: &str) -> Result<Output, Error> {
    // Process data
    Ok(Output::new())
}
"#;
    let result = extract("lib.rs", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "process_data");
    assert_eq!(func_node.visibility, Some(Visibility::Public));
}

#[test]
fn rust_extracts_struct_declarations() {
    let code = r#"
pub struct User {
    pub id: String,
    pub name: String,
    email: String,
}
"#;
    let result = extract("models.rs", code);

    let struct_node = find_kind(&result, NodeKind::Struct).expect("struct");
    assert_eq!(struct_node.name, "User");
}

#[test]
fn rust_extracts_trait_declarations() {
    let code = r#"
pub trait Repository {
    fn find(&self, id: &str) -> Option<Entity>;
    fn save(&mut self, entity: Entity) -> Result<(), Error>;
}
"#;
    let result = extract("traits.rs", code);

    let trait_node = find_kind(&result, NodeKind::Trait).expect("trait");
    assert_eq!(trait_node.name, "Repository");
}

#[test]
fn rust_extracts_impl_trait_for_type_as_implements_edges() {
    let code = r#"
pub struct MyCache {}

pub trait Cache {
    fn get(&self, key: &str) -> Option<String>;
}

impl Cache for MyCache {
    fn get(&self, key: &str) -> Option<String> {
        None
    }
}
"#;
    let result = extract("cache.rs", code);

    // Should have an unresolved reference for implements
    let impl_ref = find_ref(&result, EdgeKind::Implements, "Cache").expect("implements ref");

    // The struct MyCache should be the source
    let my_cache_node = find_named(&result, NodeKind::Struct, "MyCache").expect("MyCache");
    assert_eq!(impl_ref.from_node_id, my_cache_node.id);
}

#[test]
fn rust_extracts_trait_supertraits_as_extends_references() {
    let code = r#"
pub trait Display {}

pub trait Error: Display {
    fn description(&self) -> &str;
}
"#;
    let result = extract("error.rs", code);

    let extends_ref = find_ref(&result, EdgeKind::Extends, "Display").expect("extends ref");

    let error_trait = find_named(&result, NodeKind::Trait, "Error").expect("Error trait");
    assert_eq!(extends_ref.from_node_id, error_trait.id);
}

#[test]
fn rust_does_not_create_implements_edges_for_plain_impl_blocks() {
    let code = r#"
pub struct Counter {
    count: u32,
}

impl Counter {
    pub fn new() -> Counter {
        Counter { count: 0 }
    }
    pub fn increment(&mut self) {
        self.count += 1;
    }
}
"#;
    let result = extract("counter.rs", code);

    // Should have no implements references (no trait involved)
    let impl_refs = refs_of_kind(&result, EdgeKind::Implements);
    assert_eq!(impl_refs.len(), 0);
}

// =============================================================================
// describe('Java Extraction')
// =============================================================================

#[test]
fn java_extracts_class_declarations() {
    let code = r#"
public class UserService {
    private final UserRepository repository;

    public UserService(UserRepository repository) {
        this.repository = repository;
    }

    public User getUser(String id) {
        return repository.findById(id);
    }
}
"#;
    let result = extract("UserService.java", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserService");
    assert_eq!(class_node.visibility, Some(Visibility::Public));
}

#[test]
fn java_extracts_method_declarations() {
    let code = r#"
public class Calculator {
    public static int add(int a, int b) {
        return a + b;
    }
}
"#;
    let result = extract("Calculator.java", code);

    let method_node = find_named(&result, NodeKind::Method, "add").expect("add method");
    assert_eq!(method_node.is_static, Some(true));
}

#[test]
fn java_wraps_top_level_declarations_in_a_namespace_from_package_declaration() {
    let code = r#"
package com.example.foo;

public class Bar {
    public String greet() { return "hi"; }
}
"#;
    let result = extract("Bar.java", code);

    let ns = find_kind(&result, NodeKind::Namespace).expect("namespace");
    assert_eq!(ns.name, "com.example.foo");

    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "com.example.foo::Bar");

    let greet = find_named(&result, NodeKind::Method, "greet").expect("greet");
    assert_eq!(greet.qualified_name, "com.example.foo::Bar::greet");
}

#[test]
fn java_does_not_wrap_when_no_package_is_declared() {
    let code = r#"
public class Bar {
    public String greet() { return "hi"; }
}
"#;
    let result = extract("Bar.java", code);
    assert!(find_kind(&result, NodeKind::Namespace).is_none());
    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "Bar");
}

#[test]
fn java_extracts_anonymous_class_overrides_from_new_t() {
    // The pattern that breaks the trace through `strategy.foo()` in
    // libraries like guava's Splitter: the lambda-returned anonymous
    // class overrides abstract methods on the base, but without
    // extracting those overrides the interface→impl synthesizer has
    // nothing to bridge.
    let code = r#"
package com.example;

abstract class Base {
  abstract int compute(int x);
}

public class Factory {
  public Base make() {
    return new Base() {
      @Override
      int compute(int x) { return x + 1; }
    };
  }
}
"#;
    let result = extract("Factory.java", code);

    let anon = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name.contains("Base$anon@"))
        .expect("anonymous Base subclass should be extracted as a class");

    let compute = result
        .nodes
        .iter()
        .find(|n| {
            n.kind == NodeKind::Method && n.name == "compute" && n.qualified_name.contains("$anon@")
        })
        .expect("override method should be a method on the anon class");
    assert!(
        compute
            .qualified_name
            .contains("Factory::make::<Base$anon@")
    );
    assert!(compute.qualified_name.ends_with("::compute"));

    // Anon class must extend Base so Phase 5.5 (interface-impl) can bridge.
    let extends_ref = result.unresolved_references.iter().find(|r| {
        r.reference_kind == EdgeKind::Extends
            && r.reference_name == "Base"
            && r.from_node_id == anon.id
    });
    assert!(
        extends_ref.is_some(),
        "anon class should carry an `extends Base` reference"
    );

    // The enclosing `make` method still emits an instantiates edge to Base —
    // anon extraction must not swallow that signal.
    let instantiates_ref = find_ref(&result, EdgeKind::Instantiates, "Base");
    assert!(
        instantiates_ref.is_some(),
        "enclosing method should still instantiate Base"
    );
}

#[test]
fn java_extracts_anonymous_class_overrides_inside_a_lambda_body() {
    // The exact guava pattern: a lambda is passed to a constructor, and the
    // lambda body returns `new T() { @Override ... }`. The anon class must
    // still surface even though it sits inside a lambda_expression node.
    let code = r#"
package com.example;

interface Strategy {
  java.util.Iterator<String> iterator(String s);
}

abstract class BaseIter implements java.util.Iterator<String> {
  abstract int separatorStart(int start);
}

public class Splitter {
  private final Strategy strategy;
  public Splitter(Strategy s) { this.strategy = s; }

  public static Splitter on(char c) {
    return new Splitter((seq) ->
        new BaseIter() {
          @Override
          int separatorStart(int start) { return start + 1; }
          @Override public boolean hasNext() { return false; }
          @Override public String next() { return null; }
        });
  }
}
"#;
    let result = extract("Splitter.java", code);

    let anon = result
        .nodes
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.name.contains("BaseIter$anon@"));
    assert!(
        anon.is_some(),
        "anon BaseIter inside the lambda body should be extracted"
    );

    let sep_start = result.nodes.iter().find(|n| {
        n.kind == NodeKind::Method
            && n.name == "separatorStart"
            && n.qualified_name.contains("$anon@")
    });
    assert!(
        sep_start.is_some(),
        "override inside the lambda-returned anon class should be a method node"
    );
}

// =============================================================================
// describe('C# Extraction')
// =============================================================================

#[test]
fn csharp_extracts_class_declarations() {
    let code = r#"
public class OrderService
{
    private readonly IOrderRepository _repository;

    public OrderService(IOrderRepository repository)
    {
        _repository = repository;
    }

    public async Task<Order> GetOrderAsync(string id)
    {
        return await _repository.FindByIdAsync(id);
    }
}
"#;
    let result = extract("OrderService.cs", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "OrderService");
    assert_eq!(class_node.visibility, Some(Visibility::Public));
}

// =============================================================================
// describe('PHP Extraction')
// =============================================================================

#[test]
fn php_extracts_class_declarations() {
    let code = r#"<?php

class UserController
{
    private UserService $userService;

    public function __construct(UserService $userService)
    {
        $this->userService = $userService;
    }

    public function show(string $id): User
    {
        return $this->userService->find($id);
    }
}
"#;
    let result = extract("UserController.php", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserController");
}

#[test]
fn php_extracts_class_inheritance_extends_and_interface_implementation() {
    let code = r#"<?php

class ChildController extends BaseController implements Serializable, JsonSerializable
{
    public function serialize(): string
    {
        return json_encode($this);
    }
}
"#;
    let result = extract("ChildController.php", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "ChildController");

    let extends_ref = refs_of_kind(&result, EdgeKind::Extends);
    assert_eq!(
        extends_ref.first().map(|r| r.reference_name.as_str()),
        Some("BaseController")
    );

    let implements_refs = refs_of_kind(&result, EdgeKind::Implements);
    assert_eq!(implements_refs.len(), 2);
    let impl_names = ref_names(&implements_refs);
    assert!(impl_names.contains(&"Serializable".to_string()));
    assert!(impl_names.contains(&"JsonSerializable".to_string()));
}

// =============================================================================
// describe('Swift Extraction')
// =============================================================================

#[test]
fn swift_extracts_class_declarations() {
    let code = r#"
public class NetworkManager {
    private let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    public func fetchData(from url: URL) async throws -> Data {
        let (data, _) = try await session.data(from: url)
        return data
    }
}
"#;
    let result = extract("NetworkManager.swift", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "NetworkManager");
}

#[test]
fn swift_extracts_function_declarations() {
    let code = r#"
func calculateSum(_ numbers: [Int]) -> Int {
    return numbers.reduce(0, +)
}

public func formatCurrency(amount: Double) -> String {
    return String(format: "$%.2f", amount)
}
"#;
    let result = extract("utils.swift", code);

    let functions = filter_kind(&result, NodeKind::Function);
    assert!(!functions.is_empty());
}

#[test]
fn swift_extracts_struct_declarations() {
    let code = r#"
public struct User {
    let id: UUID
    var name: String
    var email: String

    func displayName() -> String {
        return name
    }
}
"#;
    let result = extract("User.swift", code);

    let struct_node = find_kind(&result, NodeKind::Struct).expect("struct");
    assert_eq!(struct_node.name, "User");
}

#[test]
fn swift_extracts_protocol_declarations() {
    let code = r#"
public protocol Repository {
    associatedtype Entity

    func find(id: String) async throws -> Entity?
    func save(_ entity: Entity) async throws
}
"#;
    let result = extract("Repository.swift", code);

    let protocol_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(protocol_node.name, "Repository");
}

#[test]
fn swift_extracts_class_inheritance_and_protocol_conformance() {
    let code = r#"
class DataRequest: Request {
    func validate() {}
}

class UploadRequest: DataRequest, Sendable {
    func upload() {}
}

enum AFError: Error {
    case invalidURL
}

struct HTTPMethod: RawRepresentable {
    let rawValue: String
}

protocol UploadConvertible: URLRequestConvertible {
    func asURLRequest() throws -> URLRequest
}
"#;
    let result = extract("Inheritance.swift", code);

    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    let extends_names = ref_names(&extends_refs);

    // DataRequest extends Request
    assert!(extends_names.contains(&"Request".to_string()));
    // UploadRequest extends DataRequest and Sendable
    assert!(extends_names.contains(&"DataRequest".to_string()));
    assert!(extends_names.contains(&"Sendable".to_string()));
    // AFError extends Error
    assert!(extends_names.contains(&"Error".to_string()));
    // HTTPMethod extends RawRepresentable
    assert!(extends_names.contains(&"RawRepresentable".to_string()));
    // UploadConvertible extends URLRequestConvertible
    assert!(extends_names.contains(&"URLRequestConvertible".to_string()));
}

// =============================================================================
// describe('Kotlin Extraction')
// =============================================================================

#[test]
fn kotlin_extracts_class_declarations() {
    let code = r#"
class UserRepository(private val database: Database) {
    fun findById(id: String): User? {
        return database.query("SELECT * FROM users WHERE id = ?", id)
    }

    suspend fun save(user: User) {
        database.insert(user)
    }
}
"#;
    let result = extract("UserRepository.kt", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserRepository");
}

#[test]
fn kotlin_extracts_function_declarations() {
    let code = r#"
fun calculateTotal(items: List<Item>): Double {
    return items.sumOf { it.price }
}

suspend fun fetchUserData(userId: String): User {
    return api.getUser(userId)
}
"#;
    let result = extract("utils.kt", code);

    let functions = filter_kind(&result, NodeKind::Function);
    assert!(!functions.is_empty());
}

#[test]
fn kotlin_detects_suspend_functions_as_async() {
    let code = r#"
suspend fun loadData(): List<String> {
    delay(1000)
    return listOf("a", "b", "c")
}
"#;
    let result = extract("loader.kt", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.is_async, Some(true));
}

#[test]
fn kotlin_extracts_fun_interface_declarations() {
    let code = r#"
fun interface OnObjectRetainedListener {
  fun onObjectRetained()
}
"#;
    let result = extract("listener.kt", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "OnObjectRetainedListener");

    let method_node = find_kind(&result, NodeKind::Method).expect("method");
    assert_eq!(method_node.name, "onObjectRetained");
    assert_eq!(
        method_node.qualified_name,
        "OnObjectRetainedListener::onObjectRetained"
    );
}

#[test]
fn kotlin_extracts_complex_fun_interface_with_nested_classes() {
    let code = r#"
fun interface EventListener {
  fun onEvent(event: Event)

  sealed class Event {
    class DumpingHeap : Event()
  }
}
"#;
    let result = extract("events.kt", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "EventListener");

    // Nested sealed class should still be extracted (as sibling due to grammar limitations)
    assert!(find_named(&result, NodeKind::Class, "Event").is_some());
    assert!(find_named(&result, NodeKind::Class, "DumpingHeap").is_some());
}

#[test]
fn kotlin_does_not_affect_regular_function_declarations() {
    let code = r#"
fun interface MyCallback {
  fun invoke(value: Int)
}

fun regularFunction(): String {
  return "hello"
}
"#;
    let result = extract("mixed.kt", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "MyCallback");

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "regularFunction");
}

#[test]
fn kotlin_extracts_fun_interface_with_annotation_on_method_pattern_2b() {
    // When the SAM method has annotations like @Throws, tree-sitter produces a different
    // misparse: function_declaration > ERROR("interface Name {") instead of
    // function_declaration > user_type("interface"). This is the OkHttp Interceptor pattern.
    let code = r#"
import java.io.IOException

fun interface Interceptor {
  @Throws(IOException::class)
  fun intercept(chain: Chain): Response
}
"#;
    let result = extract("interceptor.kt", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "Interceptor");
}

#[test]
fn kotlin_extracts_methods_from_interface_with_nested_fun_interface() {
    // When an interface contains a nested `fun interface`, tree-sitter misparsed
    // the parent body as ERROR. Methods inside should still be extracted.
    let code = r#"
interface WebSocket {
  fun request(): Request
  fun send(text: String): Boolean
  fun cancel()
  fun interface Factory {
    fun newWebSocket(request: Request): WebSocket
  }
}
"#;
    let result = extract("websocket.kt", code);

    assert!(find_named(&result, NodeKind::Interface, "WebSocket").is_some());

    let method_names: Vec<String> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Method && n.qualified_name.starts_with("WebSocket::"))
        .map(|n| n.name.clone())
        .collect();
    assert!(method_names.contains(&"request".to_string()));
    assert!(method_names.contains(&"send".to_string()));
    assert!(method_names.contains(&"cancel".to_string()));
}

#[test]
fn kotlin_wraps_top_level_declarations_in_a_namespace_from_package_header() {
    let code = r#"
package com.example.foo

class Bar {
  fun greet(): String = "hi"
}

fun util(): Int = 42
"#;
    let result = extract("Bar.kt", code);

    let ns = find_kind(&result, NodeKind::Namespace).expect("namespace");
    assert_eq!(ns.name, "com.example.foo");

    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "com.example.foo::Bar");

    let greet = find_named(&result, NodeKind::Method, "greet").expect("greet");
    assert_eq!(greet.qualified_name, "com.example.foo::Bar::greet");

    let util = find_named(&result, NodeKind::Function, "util").expect("util");
    assert_eq!(util.qualified_name, "com.example.foo::util");
}

#[test]
fn kotlin_handles_a_single_segment_package() {
    let code = r#"
package foo

class Bar
"#;
    let result = extract("Bar.kt", code);
    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "foo::Bar");
}

#[test]
fn kotlin_does_not_wrap_when_no_package_is_declared() {
    let code = r#"
class Bar {
  fun greet() = "hi"
}
"#;
    let result = extract("Bar.kt", code);
    assert!(find_kind(&result, NodeKind::Namespace).is_none());
    let cls = find_named(&result, NodeKind::Class, "Bar").expect("Bar");
    assert_eq!(cls.qualified_name, "Bar");
}

// =============================================================================
// describe('Dart Extraction')
// =============================================================================

#[test]
fn dart_extracts_class_declarations() {
    let code = r#"
class UserService {
  final Database _db;

  Future<User> findById(String id) async {
    return await _db.query(id);
  }

  void _privateMethod() {}
}
"#;
    let result = extract("service.dart", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "UserService");
    assert_eq!(class_node.visibility, Some(Visibility::Public));

    let method_nodes = filter_kind(&result, NodeKind::Method);
    assert!(method_nodes.len() >= 2);

    let find_by_id = method_nodes
        .iter()
        .find(|m| m.name == "findById")
        .expect("findById");
    assert_eq!(find_by_id.is_async, Some(true));

    let private_method = method_nodes
        .iter()
        .find(|m| m.name == "_privateMethod")
        .expect("_privateMethod");
    assert_eq!(private_method.visibility, Some(Visibility::Private));

    // Dart models a method body as a SIBLING of the signature, so the method
    // node must be extended to span its body (not just the signature line) —
    // required for body-level analysis (callees, the callback synthesizer).
    assert!(find_by_id.end_line > find_by_id.start_line);
}

#[test]
fn dart_extracts_top_level_function_declarations() {
    let code = r#"
void topLevelFunction(String name) {
  print(name);
}
"#;
    let result = extract("utils.dart", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "topLevelFunction");
    assert_eq!(func_node.language, Language::Dart);
}

#[test]
fn dart_extracts_enum_declarations() {
    let code = r#"
enum Status { active, inactive, pending }
"#;
    let result = extract("models.dart", code);

    let enum_node = find_kind(&result, NodeKind::Enum).expect("enum");
    assert_eq!(enum_node.name, "Status");
}

#[test]
fn dart_extracts_mixin_declarations() {
    let code = r#"
mixin LoggerMixin {
  void log(String message) {}
}
"#;
    let result = extract("mixins.dart", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "LoggerMixin");

    let method_node = find_kind(&result, NodeKind::Method).expect("method");
    assert_eq!(method_node.name, "log");
}

#[test]
fn dart_extracts_extension_declarations() {
    let code = r#"
extension StringExt on String {
  bool get isBlank => trim().isEmpty;
}
"#;
    let result = extract("extensions.dart", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "StringExt");
}

#[test]
fn dart_detects_static_methods() {
    let code = r#"
class Utils {
  static void doWork() {}
}
"#;
    let result = extract("utils.dart", code);

    let method_node = find_kind(&result, NodeKind::Method).expect("method");
    assert_eq!(method_node.name, "doWork");
    assert_eq!(method_node.is_static, Some(true));
}

#[test]
fn dart_detects_async_functions() {
    let code = r#"
Future<String> fetchData() async {
  return await http.get('/data');
}
"#;
    let result = extract("api.dart", code);

    let func_node = find_kind(&result, NodeKind::Function).expect("function");
    assert_eq!(func_node.name, "fetchData");
    assert_eq!(func_node.is_async, Some(true));
}

#[test]
fn dart_detects_private_visibility_via_underscore_convention() {
    let code = r#"
void _privateHelper() {}

void publicFunction() {}
"#;
    let result = extract("helpers.dart", code);

    let functions = filter_kind(&result, NodeKind::Function);
    let private_func = functions.iter().find(|f| f.name == "_privateHelper");
    let public_func = functions.iter().find(|f| f.name == "publicFunction");

    assert_eq!(
        private_func.and_then(|f| f.visibility),
        Some(Visibility::Private)
    );
    assert_eq!(
        public_func.and_then(|f| f.visibility),
        Some(Visibility::Public)
    );
}

// =============================================================================
// describe('Import Extraction')
// =============================================================================

fn import_nodes(result: &ExtractionResult) -> Vec<&Node> {
    filter_kind(result, NodeKind::Import)
}

fn first_import(result: &ExtractionResult) -> Option<&Node> {
    find_kind(result, NodeKind::Import)
}

fn sig(node: &Node) -> &str {
    node.signature.as_deref().unwrap_or_default()
}

// --- TypeScript/JavaScript imports ---

#[test]
fn ts_imports_default() {
    let result = extract("app.tsx", "import React from 'react';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "react");
    assert_eq!(sig(import_node), "import React from 'react';");
}

#[test]
fn ts_imports_named() {
    let result = extract(
        "icons.tsx",
        "import { Bug, Database } from '@phosphor-icons/react';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "@phosphor-icons/react");
    assert!(sig(import_node).contains("Bug"));
    assert!(sig(import_node).contains("Database"));
}

#[test]
fn ts_imports_namespace() {
    let result = extract(
        "icons.tsx",
        "import * as Icons from '@phosphor-icons/react';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "@phosphor-icons/react");
    assert!(sig(import_node).contains("* as Icons"));
}

#[test]
fn ts_imports_side_effect() {
    let result = extract("app.tsx", "import './styles.css';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "./styles.css");
}

#[test]
fn ts_imports_mixed_default_plus_named() {
    let result = extract(
        "app.tsx",
        "import React, { useState, useEffect } from 'react';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "react");
    assert!(sig(import_node).contains("React"));
    assert!(sig(import_node).contains("useState"));
    assert!(sig(import_node).contains("useEffect"));
}

#[test]
fn ts_imports_multiple_statements() {
    let code = r#"
import React from 'react';
import { Button } from './components';
import './styles.css';
"#;
    let result = extract("app.tsx", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"react".to_string()));
    assert!(import_names.contains(&"./components".to_string()));
    assert!(import_names.contains(&"./styles.css".to_string()));
}

#[test]
fn ts_imports_type_imports() {
    let result = extract("types.ts", "import type { FC, ReactNode } from 'react';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "react");
    assert!(sig(import_node).contains("type"));
    assert!(sig(import_node).contains("FC"));
}

#[test]
fn ts_imports_aliased_named() {
    let result = extract(
        "hooks.ts",
        "import { useState as useStateAlias } from 'react';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "react");
    assert!(sig(import_node).contains("useState"));
    assert!(sig(import_node).contains("useStateAlias"));
}

#[test]
fn ts_imports_relative_path() {
    let result = extract(
        "components/Button.tsx",
        "import { helper } from '../utils/helper';",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "../utils/helper");
    assert!(sig(import_node).contains("helper"));
}

// --- Python imports ---

#[test]
fn py_imports_simple() {
    let result = extract("utils.py", "import json");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "json");
}

#[test]
fn py_imports_from() {
    let result = extract("utils.py", "from os import path");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "os");
    assert!(sig(import_node).contains("path"));
}

#[test]
fn py_imports_multiple_from_same_module() {
    let result = extract("types.py", "from typing import List, Dict, Optional");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "typing");
    assert!(sig(import_node).contains("List"));
    assert!(sig(import_node).contains("Dict"));
}

#[test]
fn py_imports_multiple_statements() {
    let code = "
import os
import sys
";
    let result = extract("main.py", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 2);
    let import_names = names(&imports);
    assert!(import_names.contains(&"os".to_string()));
    assert!(import_names.contains(&"sys".to_string()));
}

#[test]
fn py_imports_aliased() {
    let result = extract("data.py", "import numpy as np");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "numpy");
    assert!(sig(import_node).contains("as np"));
}

#[test]
fn py_imports_relative() {
    let result = extract("module.py", "from .utils import helper");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, ".utils");
    assert!(sig(import_node).contains("helper"));
}

#[test]
fn py_imports_wildcard() {
    let result = extract("types.py", "from typing import *");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "typing");
    assert!(sig(import_node).contains("*"));
}

// --- Rust imports ---

#[test]
fn rust_imports_simple_use_declaration() {
    let result = extract("main.rs", "use std::io;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "std");
    assert_eq!(sig(import_node), "use std::io;");
}

#[test]
fn rust_imports_scoped_use_list() {
    let result = extract("main.rs", "use std::{ffi::OsStr, io, path::Path};");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "std");
    assert!(sig(import_node).contains("ffi::OsStr"));
    assert!(sig(import_node).contains("path::Path"));
}

#[test]
fn rust_imports_crate() {
    let result = extract("lib.rs", "use crate::error::Error;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "crate");
}

#[test]
fn rust_imports_super() {
    let result = extract("submod.rs", "use super::utils;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "super");
}

#[test]
fn rust_imports_external_crate() {
    let result = extract("types.rs", "use serde::{Serialize, Deserialize};");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "serde");
    assert!(sig(import_node).contains("Serialize"));
    assert!(sig(import_node).contains("Deserialize"));
}

// --- Go imports ---

#[test]
fn go_imports_single() {
    let code = "
package main

import \"fmt\"
";
    let result = extract("main.go", code);
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "fmt");
}

#[test]
fn go_imports_grouped() {
    let code = "
package main

import (
\t\"fmt\"
\t\"os\"
\t\"encoding/json\"
)
";
    let result = extract("main.go", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"fmt".to_string()));
    assert!(import_names.contains(&"os".to_string()));
    assert!(import_names.contains(&"encoding/json".to_string()));
}

#[test]
fn go_imports_aliased() {
    let code = "
package main

import f \"fmt\"
";
    let result = extract("main.go", code);
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "fmt");
    assert!(sig(import_node).contains("f"));
}

#[test]
fn go_imports_dot() {
    let code = "
package main

import . \"math\"
";
    let result = extract("main.go", code);
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "math");
    assert!(sig(import_node).contains("."));
}

#[test]
fn go_imports_blank() {
    let code = "
package main

import _ \"github.com/go-sql-driver/mysql\"
";
    let result = extract("main.go", code);
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "github.com/go-sql-driver/mysql");
    assert!(sig(import_node).contains("_"));
}

// --- Swift imports ---

#[test]
fn swift_imports_simple() {
    let result = extract("main.swift", "import Foundation");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Foundation");
    assert_eq!(sig(import_node), "import Foundation");
}

#[test]
fn swift_imports_testable() {
    let result = extract("Tests.swift", "@testable import Alamofire");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Alamofire");
    assert!(sig(import_node).contains("@testable"));
}

#[test]
fn swift_imports_preconcurrency() {
    let result = extract("Auth.swift", "@preconcurrency import Security");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Security");
}

#[test]
fn swift_imports_multiple() {
    let code = "
import Foundation
import UIKit
import Alamofire
";
    let result = extract("App.swift", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"Foundation".to_string()));
    assert!(import_names.contains(&"UIKit".to_string()));
    assert!(import_names.contains(&"Alamofire".to_string()));
}

// --- Kotlin imports ---

#[test]
fn kotlin_imports_simple() {
    let result = extract("Main.kt", "import java.io.IOException");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.io.IOException");
    assert_eq!(sig(import_node), "import java.io.IOException");
}

#[test]
fn kotlin_imports_aliased() {
    let result = extract(
        "Utils.kt",
        "import okhttp3.Request.Builder as RequestBuilder",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "okhttp3.Request.Builder");
    assert!(sig(import_node).contains("as RequestBuilder"));
}

#[test]
fn kotlin_imports_wildcard() {
    let result = extract("Time.kt", "import java.util.concurrent.TimeUnit.*");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util.concurrent.TimeUnit");
    assert!(sig(import_node).contains(".*"));
}

#[test]
fn kotlin_imports_multiple() {
    let code = "
import java.io.IOException
import kotlin.test.assertFailsWith
import okhttp3.OkHttpClient
";
    let result = extract("Test.kt", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"java.io.IOException".to_string()));
    assert!(import_names.contains(&"kotlin.test.assertFailsWith".to_string()));
    assert!(import_names.contains(&"okhttp3.OkHttpClient".to_string()));
}

// --- Java imports ---

#[test]
fn java_imports_simple() {
    let result = extract("Main.java", "import java.util.List;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util.List");
    assert_eq!(sig(import_node), "import java.util.List;");
}

#[test]
fn java_imports_static() {
    let result = extract(
        "Utils.java",
        "import static java.util.Collections.emptyList;",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util.Collections.emptyList");
    assert!(sig(import_node).contains("static"));
}

#[test]
fn java_imports_wildcard() {
    let result = extract("App.java", "import java.util.*;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util");
    assert!(sig(import_node).contains(".*"));
}

#[test]
fn java_imports_nested_class() {
    let result = extract("MapUtil.java", "import java.util.Map.Entry;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "java.util.Map.Entry");
}

#[test]
fn java_imports_multiple() {
    let code = "
import java.util.List;
import java.util.Map;
import java.io.IOException;
";
    let result = extract("Service.java", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"java.util.List".to_string()));
    assert!(import_names.contains(&"java.util.Map".to_string()));
    assert!(import_names.contains(&"java.io.IOException".to_string()));
}

// --- C# imports ---

#[test]
fn csharp_imports_simple_using() {
    let result = extract("Program.cs", "using System;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "System");
    assert_eq!(sig(import_node), "using System;");
}

#[test]
fn csharp_imports_qualified_using() {
    let result = extract("Utils.cs", "using System.Collections.Generic;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "System.Collections.Generic");
}

#[test]
fn csharp_imports_static_using() {
    let result = extract("App.cs", "using static System.Console;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "System.Console");
    assert!(sig(import_node).contains("static"));
}

#[test]
fn csharp_imports_alias_using() {
    let result = extract(
        "Types.cs",
        "using MyList = System.Collections.Generic.List<int>;",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "System.Collections.Generic.List<int>");
    assert!(sig(import_node).contains("MyList ="));
}

#[test]
fn csharp_imports_multiple_usings() {
    let code = "
using System;
using System.Threading.Tasks;
using Microsoft.Extensions.DependencyInjection;
";
    let result = extract("Service.cs", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"System".to_string()));
    assert!(import_names.contains(&"System.Threading.Tasks".to_string()));
    assert!(import_names.contains(&"Microsoft.Extensions.DependencyInjection".to_string()));
}

// --- PHP imports ---

#[test]
fn php_imports_simple_use() {
    let result = extract("Test.php", "<?php use PHPUnit\\Framework\\TestCase;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "PHPUnit\\Framework\\TestCase");
}

#[test]
fn php_imports_aliased_use() {
    let result = extract("Test.php", "<?php use Mockery as m;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Mockery");
    assert!(sig(import_node).contains("as m"));
}

#[test]
fn php_imports_function_use() {
    let result = extract(
        "helpers.php",
        "<?php use function Illuminate\\Support\\env;",
    );
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "Illuminate\\Support\\env");
    assert!(sig(import_node).contains("function"));
}

#[test]
fn php_imports_grouped_use() {
    let result = extract(
        "Models.php",
        "<?php use Illuminate\\Database\\{Model, Builder};",
    );
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 2);
    let import_names = names(&imports);
    assert!(import_names.contains(&"Illuminate\\Database\\Model".to_string()));
    assert!(import_names.contains(&"Illuminate\\Database\\Builder".to_string()));
}

#[test]
fn php_imports_multiple_uses() {
    let code = "<?php
use Illuminate\\Support\\Collection;
use Illuminate\\Support\\Str;
use Closure;
";
    let result = extract("Service.php", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"Illuminate\\Support\\Collection".to_string()));
    assert!(import_names.contains(&"Illuminate\\Support\\Str".to_string()));
    assert!(import_names.contains(&"Closure".to_string()));
}

// --- Ruby imports ---

#[test]
fn ruby_imports_require() {
    let result = extract("app.rb", "require 'json'");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "json");
    assert_eq!(sig(import_node), "require 'json'");
}

#[test]
fn ruby_imports_require_with_path() {
    let result = extract("config.rb", "require 'active_support/core_ext/string'");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "active_support/core_ext/string");
}

#[test]
fn ruby_imports_require_relative() {
    let result = extract("test/my_test.rb", "require_relative '../test_helper'");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "../test_helper");
    assert!(sig(import_node).contains("require_relative"));
}

#[test]
fn ruby_imports_does_not_extract_non_require_calls() {
    let result = extract("app.rb", "puts 'hello'");
    assert!(first_import(&result).is_none());
}

#[test]
fn ruby_imports_multiple_requires() {
    let code = "
require 'json'
require 'yaml'
require_relative 'helper'
";
    let result = extract("lib.rb", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"json".to_string()));
    assert!(import_names.contains(&"yaml".to_string()));
    assert!(import_names.contains(&"helper".to_string()));
}

// --- Ruby modules ---

#[test]
fn ruby_modules_extracts_module_as_module_node_with_containment() {
    let code = "
module CachedCounting
  def self.disable
    @enabled = false
  end

  def perform_increment!(key, count)
    write_cache!(key, count)
  end
end
";
    let result = extract("concerns/cached_counting.rb", code);

    let module_node =
        find_named(&result, NodeKind::Module, "CachedCounting").expect("CachedCounting");
    assert_eq!(module_node.qualified_name, "CachedCounting");

    // Methods inside module should have module-qualified names
    let disable_method = result
        .nodes
        .iter()
        .find(|n| n.name == "disable" && n.kind == NodeKind::Method)
        .expect("disable");
    assert_eq!(disable_method.qualified_name, "CachedCounting::disable");

    let increment_method = result
        .nodes
        .iter()
        .find(|n| n.name == "perform_increment!" && n.kind == NodeKind::Method)
        .expect("perform_increment!");
    assert_eq!(
        increment_method.qualified_name,
        "CachedCounting::perform_increment!"
    );

    // Containment edge from module to methods
    let contains_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.source == module_node.id && e.kind == EdgeKind::Contains)
        .collect();
    assert!(contains_edges.len() >= 2);
}

#[test]
fn ruby_modules_handles_nested_modules_with_classes() {
    let code = "
module Discourse
  module Auth
    class AuthProvider
      def authenticate(params)
        validate(params)
      end
    end
  end
end
";
    let result = extract("lib/auth.rb", code);

    assert!(find_named(&result, NodeKind::Module, "Discourse").is_some());

    let auth_module = find_named(&result, NodeKind::Module, "Auth").expect("Auth");
    assert_eq!(auth_module.qualified_name, "Discourse::Auth");

    let auth_provider = find_named(&result, NodeKind::Class, "AuthProvider").expect("AuthProvider");
    assert_eq!(
        auth_provider.qualified_name,
        "Discourse::Auth::AuthProvider"
    );

    let auth_method = result
        .nodes
        .iter()
        .find(|n| n.name == "authenticate")
        .expect("authenticate");
    assert_eq!(
        auth_method.qualified_name,
        "Discourse::Auth::AuthProvider::authenticate"
    );
}

// --- C/C++ imports ---

#[test]
fn cpp_imports_system_include() {
    let result = extract("main.cpp", "#include <iostream>");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "iostream");
    assert_eq!(sig(import_node), "#include <iostream>");
}

#[test]
fn cpp_imports_system_include_with_path() {
    let result = extract("app.cpp", "#include <nlohmann/json.hpp>");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "nlohmann/json.hpp");
}

#[test]
fn cpp_imports_local_include() {
    let result = extract("main.cpp", "#include \"myheader.h\"");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "myheader.h");
}

#[test]
fn c_imports_header() {
    let result = extract("main.c", "#include <stdio.h>");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "stdio.h");
}

#[test]
fn cpp_imports_multiple_includes() {
    let code = "
#include <iostream>
#include <vector>
#include \"config.h\"
";
    let result = extract("app.cpp", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"iostream".to_string()));
    assert!(import_names.contains(&"vector".to_string()));
    assert!(import_names.contains(&"config.h".to_string()));
}

#[test]
fn cpp_imports_creates_unresolved_references_for_local_includes() {
    let result = extract("main.cpp", "#include \"myheader.h\"");
    let import_ref = find_ref(&result, EdgeKind::Imports, "myheader.h").expect("imports ref");
    assert_eq!(import_ref.line, 1);
}

#[test]
fn cpp_imports_creates_unresolved_references_for_system_includes() {
    let result = extract("main.cpp", "#include <iostream>");
    assert!(find_ref(&result, EdgeKind::Imports, "iostream").is_some());
}

// --- Dart imports ---

#[test]
fn dart_imports_dart_scheme() {
    let result = extract("main.dart", "import 'dart:async';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "dart:async");
    assert_eq!(sig(import_node), "import 'dart:async';");
}

#[test]
fn dart_imports_package() {
    let result = extract("app.dart", "import 'package:flutter/material.dart';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "package:flutter/material.dart");
}

#[test]
fn dart_imports_aliased() {
    let result = extract("api.dart", "import 'package:http/http.dart' as http;");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "package:http/http.dart");
    assert!(sig(import_node).contains("as http"));
}

#[test]
fn dart_imports_multiple() {
    let code = "
import 'dart:async';
import 'dart:convert';
import 'package:flutter/material.dart';
";
    let result = extract("main.dart", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"dart:async".to_string()));
    assert!(import_names.contains(&"dart:convert".to_string()));
    assert!(import_names.contains(&"package:flutter/material.dart".to_string()));
}

#[test]
fn dart_imports_relative() {
    let result = extract("lib/main.dart", "import '../utils/helpers.dart';");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "../utils/helpers.dart");
}

// --- Liquid imports ---

#[test]
fn liquid_imports_render_tag() {
    let result = extract("template.liquid", "{% render 'loading-spinner' %}");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "loading-spinner");
    assert!(sig(import_node).contains("render"));
}

#[test]
fn liquid_imports_section_tag() {
    let result = extract("layout/theme.liquid", "{% section 'header' %}");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "header");
    assert!(sig(import_node).contains("section"));
}

#[test]
fn liquid_imports_include_tag() {
    let result = extract("snippets/header.liquid", "{% include 'icon-cart' %}");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "icon-cart");
    assert!(sig(import_node).contains("include"));
}

#[test]
fn liquid_imports_render_with_whitespace_control() {
    let result = extract("snippets/product.liquid", "{%- render 'price' -%}");
    let import_node = first_import(&result).expect("import");
    assert_eq!(import_node.name, "price");
}

#[test]
fn liquid_imports_multiple() {
    let code = "
{% section 'header' %}
{% render 'loading-spinner' %}
{% render 'cart-drawer' %}
";
    let result = extract("layout/theme.liquid", code);
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 3);
    let import_names = names(&imports);
    assert!(import_names.contains(&"header".to_string()));
    assert!(import_names.contains(&"loading-spinner".to_string()));
    assert!(import_names.contains(&"cart-drawer".to_string()));
}

// =============================================================================
// describe('Pascal / Delphi Extraction')
// =============================================================================

#[test]
fn pascal_detects_pascal_files() {
    assert_eq!(detect_language("UAuth.pas", None), Language::Pascal);
    assert_eq!(detect_language("App.dpr", None), Language::Pascal);
    assert_eq!(detect_language("Package.dpk", None), Language::Pascal);
    assert_eq!(detect_language("App.lpr", None), Language::Pascal);
    assert_eq!(detect_language("MainForm.dfm", None), Language::Pascal);
    assert_eq!(detect_language("MainForm.fmx", None), Language::Pascal);
}

#[test]
fn pascal_is_reported_as_supported() {
    assert!(is_language_supported(Language::Pascal));
    assert!(get_supported_languages().contains(&Language::Pascal));
}

#[test]
fn pascal_extracts_unit_as_module() {
    let code = "unit MyUnit;\ninterface\nimplementation\nend.";
    let result = extract("MyUnit.pas", code);

    let module_node = find_kind(&result, NodeKind::Module).expect("module");
    assert_eq!(module_node.name, "MyUnit");
    assert_eq!(module_node.language, Language::Pascal);
}

#[test]
fn pascal_extracts_program_as_module() {
    let code = "program MyApp;\nbegin\nend.";
    let result = extract("MyApp.dpr", code);

    let module_node = find_kind(&result, NodeKind::Module).expect("module");
    assert_eq!(module_node.name, "MyApp");
}

#[test]
fn pascal_falls_back_to_filename_when_module_name_is_empty() {
    // Some .dpr templates use "program;" without a name
    let code = "program;\nuses SysUtils;\nbegin\nend.";
    let result = extract("Console.dpr", code);

    let module_node = find_kind(&result, NodeKind::Module).expect("module");
    assert_eq!(module_node.name, "Console");
}

#[test]
fn pascal_extracts_uses_as_individual_imports() {
    let code =
        "unit Test;\ninterface\nuses\n  System.SysUtils,\n  System.Classes;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 2);
    let import_names = names(&imports);
    assert!(import_names.contains(&"System.SysUtils".to_string()));
    assert!(import_names.contains(&"System.Classes".to_string()));
}

#[test]
fn pascal_creates_unresolved_references_for_imports() {
    let code = "unit Test;\ninterface\nuses\n  UAuth;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let import_ref = refs_of_kind(&result, EdgeKind::Imports);
    assert_eq!(
        import_ref.first().map(|r| r.reference_name.as_str()),
        Some("UAuth")
    );
}

#[test]
fn pascal_extracts_class_declarations() {
    let code = "unit Test;\ninterface\ntype\n  TMyClass = class\n  public\n    procedure DoSomething;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "TMyClass");
}

#[test]
fn pascal_extracts_class_with_inheritance() {
    let code =
        "unit Test;\ninterface\ntype\n  TChild = class(TParent)\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    assert_eq!(
        extends_refs.first().map(|r| r.reference_name.as_str()),
        Some("TParent")
    );
}

#[test]
fn pascal_extracts_class_with_interface_implementation() {
    let code = "unit Test;\ninterface\ntype\n  TService = class(TInterfacedObject, ILogger)\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    let implements_refs = refs_of_kind(&result, EdgeKind::Implements);
    assert_eq!(
        extends_refs.first().map(|r| r.reference_name.as_str()),
        Some("TInterfacedObject")
    );
    assert_eq!(
        implements_refs.first().map(|r| r.reference_name.as_str()),
        Some("ILogger")
    );
}

#[test]
fn pascal_extracts_records_as_class_nodes() {
    let code = "unit Test;\ninterface\ntype\n  TPoint = record\n    X: Double;\n    Y: Double;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "TPoint");

    let fields = filter_kind(&result, NodeKind::Field);
    assert_eq!(fields.len(), 2);
    let field_names = names(&fields);
    assert!(field_names.contains(&"X".to_string()));
    assert!(field_names.contains(&"Y".to_string()));
}

#[test]
fn pascal_extracts_interface_declarations() {
    let code = "unit Test;\ninterface\ntype\n  ILogger = interface\n    procedure Log(const AMsg: string);\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "ILogger");
}

#[test]
fn pascal_extracts_methods_with_visibility() {
    let code = "unit Test;\ninterface\ntype\n  TMyClass = class\n  private\n    FValue: Integer;\n  public\n    constructor Create;\n    function GetValue: Integer;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let methods = filter_kind(&result, NodeKind::Method);
    assert_eq!(methods.len(), 2);

    let create_method = methods.iter().find(|m| m.name == "Create").expect("Create");
    assert_eq!(create_method.visibility, Some(Visibility::Public));

    let get_value = methods
        .iter()
        .find(|m| m.name == "GetValue")
        .expect("GetValue");
    assert_eq!(get_value.visibility, Some(Visibility::Public));

    let fields = filter_kind(&result, NodeKind::Field);
    let f_value = fields.iter().find(|f| f.name == "FValue").expect("FValue");
    assert_eq!(f_value.visibility, Some(Visibility::Private));
}

#[test]
fn pascal_detects_static_methods_class_methods() {
    let code = "unit Test;\ninterface\ntype\n  THelper = class\n  public\n    class function Create: THelper; static;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let methods = filter_kind(&result, NodeKind::Method);
    let static_method = methods.iter().find(|m| m.name == "Create").expect("Create");
    assert_eq!(static_method.is_static, Some(true));
}

#[test]
fn pascal_extracts_enums_with_members() {
    let code =
        "unit Test;\ninterface\ntype\n  TColor = (clRed, clGreen, clBlue);\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let enum_node = find_kind(&result, NodeKind::Enum).expect("enum");
    assert_eq!(enum_node.name, "TColor");

    let members = filter_kind(&result, NodeKind::EnumMember);
    assert_eq!(members.len(), 3);
    assert_eq!(names(&members), vec!["clRed", "clGreen", "clBlue"]);
}

#[test]
fn pascal_extracts_properties() {
    let code = "unit Test;\ninterface\ntype\n  TObj = class\n  public\n    property Name: string read FName write FName;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let prop_node = find_kind(&result, NodeKind::Property).expect("property");
    assert_eq!(prop_node.name, "Name");
    assert_eq!(prop_node.visibility, Some(Visibility::Public));
}

#[test]
fn pascal_extracts_constants() {
    let code = "unit Test;\ninterface\nconst\n  MAX_RETRIES = 3;\n  APP_NAME = 'MyApp';\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let constants = filter_kind(&result, NodeKind::Constant);
    assert_eq!(constants.len(), 2);
    let constant_names = names(&constants);
    assert!(constant_names.contains(&"MAX_RETRIES".to_string()));
    assert!(constant_names.contains(&"APP_NAME".to_string()));
}

#[test]
fn pascal_extracts_type_aliases() {
    let code = "unit Test;\ninterface\ntype\n  TUserName = string;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let alias_node = find_kind(&result, NodeKind::TypeAlias).expect("type_alias");
    assert_eq!(alias_node.name, "TUserName");
}

#[test]
fn pascal_extracts_calls_from_implementation_bodies() {
    let code = "unit Test;\ninterface\ntype\n  TObj = class\n  public\n    procedure DoWork;\n  end;\nimplementation\nprocedure TObj.DoWork;\nbegin\n  WriteLn('hello');\nend;\nend.";
    let result = extract("Test.pas", code);

    let call_refs = refs_of_kind(&result, EdgeKind::Calls);
    assert_eq!(
        call_refs.first().map(|r| r.reference_name.as_str()),
        Some("WriteLn")
    );
}

#[test]
fn pascal_creates_contains_edges_for_class_members() {
    let code = "unit Test;\ninterface\ntype\n  TObj = class\n  public\n    procedure Foo;\n  end;\nimplementation\nend.";
    let result = extract("Test.pas", code);

    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    let method_node = find_kind(&result, NodeKind::Method).expect("method");

    let contains_edge = result.edges.iter().find(|e| {
        e.source == class_node.id && e.target == method_node.id && e.kind == EdgeKind::Contains
    });
    assert!(contains_edge.is_some());
}

// --- Full fixture: UAuth.pas ---

const UAUTH_PAS: &str = "unit UAuth;

interface

uses
  System.SysUtils,
  System.Classes;

type
  ITokenValidator = interface
    ['{11111111-1111-1111-1111-111111111111}']
    function Validate(const AToken: string): Boolean;
  end;

  TAuthService = class(TInterfacedObject, ITokenValidator)
  private
    FToken: string;
    FLoginCount: Integer;
    procedure IncLoginCount;
  protected
    function GetToken: string;
  public
    constructor Create;
    destructor Destroy; override;
    function Validate(const AToken: string): Boolean;
    function Login(const AUser, APass: string): string;
    property Token: string read GetToken;
    property LoginCount: Integer read FLoginCount;
  end;

implementation

constructor TAuthService.Create;
begin
  inherited Create;
  FToken := '';
  FLoginCount := 0;
end;

destructor TAuthService.Destroy;
begin
  FToken := '';
  inherited Destroy;
end;

procedure TAuthService.IncLoginCount;
begin
  Inc(FLoginCount);
end;

function TAuthService.GetToken: string;
begin
  Result := FToken;
end;

function TAuthService.Validate(const AToken: string): Boolean;
begin
  Result := AToken <> '';
end;

function TAuthService.Login(const AUser, APass: string): string;
begin
  IncLoginCount;
  if Validate(AUser + ':' + APass) then
  begin
    FToken := AUser;
    Result := 'ok';
  end
  else
    Result := '';
end;

end.";

#[test]
fn pascal_uauth_fixture_extracts_all_expected_nodes() {
    let result = extract("UAuth.pas", UAUTH_PAS);

    assert_eq!(result.errors.len(), 0);

    // Module
    let module_node = find_kind(&result, NodeKind::Module).expect("module");
    assert_eq!(module_node.name, "UAuth");

    // Imports
    let imports = import_nodes(&result);
    assert_eq!(imports.len(), 2);

    // Interface
    let iface_node = find_kind(&result, NodeKind::Interface).expect("interface");
    assert_eq!(iface_node.name, "ITokenValidator");

    // Class
    let class_node = find_kind(&result, NodeKind::Class).expect("class");
    assert_eq!(class_node.name, "TAuthService");

    // Methods
    let methods = filter_kind(&result, NodeKind::Method);
    assert!(methods.len() >= 6);
    let method_names = names(&methods);
    assert!(method_names.contains(&"Create".to_string()));
    assert!(method_names.contains(&"Destroy".to_string()));
    assert!(method_names.contains(&"Login".to_string()));

    // Fields
    let fields = filter_kind(&result, NodeKind::Field);
    assert_eq!(fields.len(), 2);
    assert!(
        fields
            .iter()
            .all(|f| f.visibility == Some(Visibility::Private))
    );

    // Properties
    let props = filter_kind(&result, NodeKind::Property);
    assert_eq!(props.len(), 2);
    let prop_names = names(&props);
    assert!(prop_names.contains(&"Token".to_string()));
    assert!(prop_names.contains(&"LoginCount".to_string()));
}

#[test]
fn pascal_uauth_fixture_extracts_inheritance_and_interface_implementation() {
    let result = extract("UAuth.pas", UAUTH_PAS);

    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    assert_eq!(
        extends_refs.first().map(|r| r.reference_name.as_str()),
        Some("TInterfacedObject")
    );

    let implements_refs = refs_of_kind(&result, EdgeKind::Implements);
    assert_eq!(
        implements_refs.first().map(|r| r.reference_name.as_str()),
        Some("ITokenValidator")
    );
}

#[test]
fn pascal_uauth_fixture_extracts_calls_from_implementation() {
    let result = extract("UAuth.pas", UAUTH_PAS);

    let call_names = ref_names(&refs_of_kind(&result, EdgeKind::Calls));
    assert!(call_names.contains(&"Inc".to_string()));
    assert!(call_names.contains(&"Validate".to_string()));
}

// --- Full fixture: UTypes.pas ---

const UTYPES_PAS: &str = "unit UTypes;

interface

uses
  System.SysUtils;

const
  C_MAX_RETRIES = 3;
  C_DEFAULT_NAME = 'Guest';

type
  TUserRole = (urAdmin, urEditor, urViewer);

  TPoint2D = record
    X: Double;
    Y: Double;
  end;

  TUserName = string;

  TUserInfo = class
  public
    type
      TAddress = record
        Street: string;
        City: string;
        Zip: string;
      end;
  private
    FName: TUserName;
    FRole: TUserRole;
    FAddress: TAddress;
  public
    constructor Create(const AName: TUserName; ARole: TUserRole);
    function GetDisplayName: string;
    class function CreateAdmin(const AName: TUserName): TUserInfo; static;
    property Name: TUserName read FName write FName;
    property Role: TUserRole read FRole;
    property Address: TAddress read FAddress write FAddress;
  end;

implementation

constructor TUserInfo.Create(const AName: TUserName; ARole: TUserRole);
begin
  FName := AName;
  FRole := ARole;
end;

function TUserInfo.GetDisplayName: string;
begin
  if FRole = urAdmin then
    Result := '[Admin] ' + FName
  else
    Result := FName;
end;

class function TUserInfo.CreateAdmin(const AName: TUserName): TUserInfo;
begin
  Result := TUserInfo.Create(AName, urAdmin);
end;

end.";

#[test]
fn pascal_utypes_fixture_extracts_enums_with_members() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let enum_node = find_kind(&result, NodeKind::Enum).expect("enum");
    assert_eq!(enum_node.name, "TUserRole");

    let members = filter_kind(&result, NodeKind::EnumMember);
    assert_eq!(members.len(), 3);
    assert_eq!(names(&members), vec!["urAdmin", "urEditor", "urViewer"]);
}

#[test]
fn pascal_utypes_fixture_extracts_constants() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let constants = filter_kind(&result, NodeKind::Constant);
    assert_eq!(constants.len(), 2);
    let constant_names = names(&constants);
    assert!(constant_names.contains(&"C_MAX_RETRIES".to_string()));
    assert!(constant_names.contains(&"C_DEFAULT_NAME".to_string()));
}

#[test]
fn pascal_utypes_fixture_extracts_type_aliases() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let aliases = filter_kind(&result, NodeKind::TypeAlias);
    assert!(names(&aliases).contains(&"TUserName".to_string()));
}

#[test]
fn pascal_utypes_fixture_extracts_records_as_classes_with_fields() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let classes = filter_kind(&result, NodeKind::Class);
    assert!(names(&classes).contains(&"TPoint2D".to_string()));

    // TPoint2D fields
    let fields = filter_kind(&result, NodeKind::Field);
    let field_names = names(&fields);
    assert!(field_names.contains(&"X".to_string()));
    assert!(field_names.contains(&"Y".to_string()));
}

#[test]
fn pascal_utypes_fixture_extracts_static_class_methods() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let methods = filter_kind(&result, NodeKind::Method);
    let static_method = methods
        .iter()
        .find(|m| m.name == "CreateAdmin")
        .expect("CreateAdmin");
    assert_eq!(static_method.is_static, Some(true));
}

#[test]
fn pascal_utypes_fixture_extracts_nested_types() {
    let result = extract("UTypes.pas", UTYPES_PAS);

    let classes = filter_kind(&result, NodeKind::Class);
    assert!(names(&classes).contains(&"TAddress".to_string()));
}

// =============================================================================
// describe('DFM/FMX Extraction')
// =============================================================================

#[test]
fn dfm_extracts_components() {
    let code = "object Form1: TForm1
  Left = 0
  Top = 0
  Caption = 'My Form'
  object Button1: TButton
    Left = 10
    Top = 10
    Caption = 'Click Me'
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 2);
    let component_names = names(&components);
    assert!(component_names.contains(&"Form1".to_string()));
    assert!(component_names.contains(&"Button1".to_string()));

    let button = components
        .iter()
        .find(|c| c.name == "Button1")
        .expect("Button1");
    assert_eq!(button.signature.as_deref(), Some("TButton"));
}

#[test]
fn dfm_extracts_nested_component_hierarchy() {
    let code = "object Form1: TForm1
  object Panel1: TPanel
    object Label1: TLabel
      Caption = 'Hello'
    end
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 3);

    // Check nesting: Panel1 contains Label1
    let panel = components
        .iter()
        .find(|c| c.name == "Panel1")
        .expect("Panel1");
    let label = components
        .iter()
        .find(|c| c.name == "Label1")
        .expect("Label1");
    let contains_edge = result
        .edges
        .iter()
        .find(|e| e.source == panel.id && e.target == label.id && e.kind == EdgeKind::Contains);
    assert!(contains_edge.is_some());
}

#[test]
fn dfm_extracts_event_handler_references() {
    let code = "object Form1: TForm1
  OnCreate = FormCreate
  OnDestroy = FormDestroy
  object Button1: TButton
    OnClick = Button1Click
  end
end";
    let result = extract("Form1.dfm", code);

    let refs = &result.unresolved_references;
    assert_eq!(refs.len(), 3);
    let all_names: Vec<&str> = refs.iter().map(|r| r.reference_name.as_str()).collect();
    assert!(all_names.contains(&"FormCreate"));
    assert!(all_names.contains(&"FormDestroy"));
    assert!(all_names.contains(&"Button1Click"));
    assert!(
        refs.iter()
            .all(|r| r.reference_kind == EdgeKind::References)
    );
}

#[test]
fn dfm_handles_multi_line_properties() {
    let code = "object Form1: TForm1
  SQL.Strings = (
    'SELECT * FROM users'
    'WHERE active = 1')
  object Button1: TButton
    OnClick = Button1Click
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 2);

    let refs = &result.unresolved_references;
    assert_eq!(refs.len(), 1);
    assert_eq!(refs[0].reference_name, "Button1Click");
}

#[test]
fn dfm_handles_inherited_keyword() {
    let code = "inherited Form1: TForm1
  Caption = 'Inherited Form'
  object Button1: TButton
    OnClick = Button1Click
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 2);
    assert!(names(&components).contains(&"Form1".to_string()));
}

#[test]
fn dfm_handles_item_collection_properties() {
    let code = "object Form1: TForm1
  object StatusBar1: TStatusBar
    Panels = <
      item
        Width = 200
      end
      item
        Width = 200
      end>
  end
end";
    let result = extract("Form1.dfm", code);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 2);
}

const MAINFORM_DFM: &str = "object frmMain: TfrmMain
  Left = 0
  Top = 0
  Caption = 'CodeGraph DFM Fixture'
  ClientHeight = 480
  ClientWidth = 640
  OnCreate = FormCreate
  OnDestroy = FormDestroy
  object pnlTop: TPanel
    Left = 0
    Top = 0
    Width = 640
    Height = 50
    object lblTitle: TLabel
      Left = 16
      Top = 16
      Caption = 'Authentication Service'
    end
    object btnLogin: TButton
      Left = 540
      Top = 12
      OnClick = btnLoginClick
    end
  end
  object pnlContent: TPanel
    Left = 0
    Top = 50
    object edtUsername: TEdit
      Left = 16
      Top = 16
      OnChange = edtUsernameChange
    end
    object edtPassword: TEdit
      Left = 16
      Top = 48
      OnKeyPress = edtPasswordKeyPress
    end
    object mmoLog: TMemo
      Left = 16
      Top = 88
    end
  end
  object pnlStatus: TStatusBar
    Left = 0
    Top = 440
    Panels = <
      item
        Width = 200
      end
      item
        Width = 200
      end>
  end
end";

#[test]
fn dfm_mainform_fixture_extracts_all_components() {
    let result = extract("MainForm.dfm", MAINFORM_DFM);

    let components = filter_kind(&result, NodeKind::Component);
    assert_eq!(components.len(), 9);
    let component_names = names(&components);
    for expected in [
        "frmMain",
        "pnlTop",
        "lblTitle",
        "btnLogin",
        "pnlContent",
        "edtUsername",
        "edtPassword",
        "mmoLog",
        "pnlStatus",
    ] {
        assert!(
            component_names.contains(&expected.to_string()),
            "missing {expected}"
        );
    }
}

#[test]
fn dfm_mainform_fixture_extracts_all_event_handlers() {
    let result = extract("MainForm.dfm", MAINFORM_DFM);

    let refs = &result.unresolved_references;
    assert_eq!(refs.len(), 5);
    let all_names: Vec<&str> = refs.iter().map(|r| r.reference_name.as_str()).collect();
    for expected in [
        "FormCreate",
        "FormDestroy",
        "btnLoginClick",
        "edtUsernameChange",
        "edtPasswordKeyPress",
    ] {
        assert!(all_names.contains(&expected), "missing {expected}");
    }
}

// =============================================================================
// describe('Full Indexing')
// =============================================================================

#[test]
fn full_indexing_indexes_a_typescript_file() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    fs::write(
        src_dir.join("utils.ts"),
        "
export function add(a: number, b: number): number {
  return a + b;
}

export function multiply(a: number, b: number): number {
  return a * b;
}
",
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");

    assert!(result.success);
    assert_eq!(result.files_indexed, 1);
    assert!(result.nodes_created >= 2);

    // Check nodes were stored
    let nodes = queries.get_nodes_by_file("src/utils.ts").unwrap();
    assert!(nodes.len() >= 2);

    let add_func = nodes.iter().find(|n| n.name == "add").expect("add");
    assert_eq!(add_func.kind, NodeKind::Function);
}

#[test]
fn full_indexing_indexes_multiple_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();

    fs::write(
        src_dir.join("math.ts"),
        "export function add(a: number, b: number) { return a + b; }",
    )
    .unwrap();
    fs::write(
        src_dir.join("string.ts"),
        "export function capitalize(s: string) { return s.toUpperCase(); }",
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");

    assert!(result.success);
    assert_eq!(result.files_indexed, 2);

    let files = queries.get_all_files().unwrap();
    assert_eq!(files.len(), 2);
}

#[test]
fn full_indexing_tracks_file_hashes_for_incremental_updates() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    fs::write(src_dir.join("main.ts"), "export const x = 1;").unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    orch.index_all(None, None, false).expect("index_all");

    // Check file is tracked
    let file = queries.get_file_by_path("src/main.ts").unwrap();
    let file = file.expect("tracked file");
    assert!(!file.content_hash.is_empty());

    // Modify file (content change; the size/mtime pre-filter passes because
    // index time vs write time differ)
    fs::write(src_dir.join("main.ts"), "export const x = 2;").unwrap();

    // Check for changes
    let changes = orch.get_changed_files().expect("get_changed_files");
    assert!(changes.modified.contains(&"src/main.ts".to_string()));
}

#[test]
fn full_indexing_syncs_and_detects_changes() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    fs::write(
        src_dir.join("main.ts"),
        "export function original() { return 1; }",
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    orch.index_all(None, None, false).expect("index_all");

    let initial_nodes = queries.get_nodes_by_file("src/main.ts").unwrap();
    assert!(initial_nodes.iter().any(|n| n.name == "original"));

    // Modify file
    fs::write(
        src_dir.join("main.ts"),
        "export function updated() { return 2; }",
    )
    .unwrap();

    // Sync
    let sync_result = orch.sync(None).expect("sync");
    assert_eq!(sync_result.files_modified, 1);

    // Check nodes were updated
    let updated_nodes = queries.get_nodes_by_file("src/main.ts").unwrap();
    assert!(updated_nodes.iter().any(|n| n.name == "updated"));
    assert!(!updated_nodes.iter().any(|n| n.name == "original"));
}

#[test]
fn full_indexing_counts_file_level_tracked_yaml_files_as_indexed() {
    let temp_dir = tempfile::tempdir().unwrap();
    fs::write(temp_dir.path().join("app.yaml"), "name: test\n").unwrap();
    fs::write(temp_dir.path().join("routes.yml"), "route: value\n").unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");

    assert!(result.success);
    assert_eq!(result.files_indexed, 2);
    assert_eq!(result.files_skipped, 0);
    let mut tracked: Vec<String> = queries
        .get_all_files()
        .unwrap()
        .into_iter()
        .map(|f| f.path)
        .collect();
    tracked.sort();
    assert_eq!(tracked, vec!["app.yaml", "routes.yml"]);
}

#[test]
fn full_indexing_counts_file_level_tracked_yaml_twig_files_as_indexed_in_index_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    fs::write(temp_dir.path().join("app.yaml"), "name: test\n").unwrap();
    fs::write(temp_dir.path().join("view.twig"), "{{ title }}\n").unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch
        .index_files(&["app.yaml".to_string(), "view.twig".to_string()])
        .expect("index_files");

    assert!(result.success);
    assert_eq!(result.files_indexed, 2);
    assert_eq!(result.files_skipped, 0);

    let mut tracked: Vec<String> = queries
        .get_all_files()
        .unwrap()
        .into_iter()
        .map(|f| format!("{}:{}", f.path, f.language))
        .collect();
    tracked.sort();
    assert_eq!(tracked, vec!["app.yaml:yaml", "view.twig:twig"]);
}

#[test]
fn full_indexing_counts_file_level_tracked_properties_files_as_indexed() {
    let temp_dir = tempfile::tempdir().unwrap();
    fs::write(
        temp_dir.path().join("application.properties"),
        "server.port=8080\n",
    )
    .unwrap();
    fs::write(temp_dir.path().join("log.properties"), "log.level=INFO\n").unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");

    assert!(result.success);
    assert_eq!(result.files_indexed, 2);
    assert_eq!(result.files_skipped, 0);
}

#[test]
fn full_indexing_counts_the_full_file_level_tracked_class_in_index_files() {
    let temp_dir = tempfile::tempdir().unwrap();
    fs::write(temp_dir.path().join("app.yaml"), "name: test\n").unwrap();
    fs::write(temp_dir.path().join("view.twig"), "{{ title }}\n").unwrap();
    fs::write(
        temp_dir.path().join("application.properties"),
        "server.port=8080\n",
    )
    .unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch
        .index_files(&[
            "app.yaml".to_string(),
            "view.twig".to_string(),
            "application.properties".to_string(),
        ])
        .expect("index_files");

    assert!(result.success);
    assert_eq!(result.files_indexed, 3);
    assert_eq!(result.files_skipped, 0);

    let mut tracked: Vec<String> = queries
        .get_all_files()
        .unwrap()
        .into_iter()
        .map(|f| format!("{}:{}", f.path, f.language))
        .collect();
    tracked.sort();
    assert_eq!(
        tracked,
        vec![
            "app.yaml:yaml",
            "application.properties:properties",
            "view.twig:twig"
        ]
    );
}

// =============================================================================
// describe('Path Normalization')
// =============================================================================

#[test]
fn path_normalization_converts_backslashes_to_forward_slashes() {
    assert_eq!(
        normalize_path("gui\\node_modules\\foo"),
        "gui/node_modules/foo"
    );
    assert_eq!(
        normalize_path("src\\components\\Button.tsx"),
        "src/components/Button.tsx"
    );
}

#[test]
fn path_normalization_leaves_forward_slash_paths_unchanged() {
    assert_eq!(
        normalize_path("src/components/Button.tsx"),
        "src/components/Button.tsx"
    );
}

#[test]
fn path_normalization_handles_empty_string() {
    assert_eq!(normalize_path(""), "");
}

// =============================================================================
// describe('Directory Exclusion')
// =============================================================================

#[test]
fn directory_exclusion_excludes_directories_listed_in_gitignore() {
    let temp_dir = tempfile::tempdir().unwrap();
    // Create structure: src/index.ts + node_modules/pkg/index.js, gitignore node_modules
    let src_dir = temp_dir.path().join("src");
    let nm_dir = temp_dir.path().join("node_modules").join("pkg");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&nm_dir).unwrap();
    fs::write(src_dir.join("index.ts"), "export const x = 1;").unwrap();
    fs::write(nm_dir.join("index.js"), "module.exports = {};").unwrap();
    fs::write(temp_dir.path().join(".gitignore"), "node_modules/\n").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(files.contains(&"src/index.ts".to_string()));
    assert!(files.iter().all(|f| !f.contains("node_modules")));
}

#[test]
fn directory_exclusion_excludes_nested_node_modules_via_a_root_gitignore() {
    let temp_dir = tempfile::tempdir().unwrap();
    // A trailing-slash pattern with no leading slash matches at any depth.
    let src_dir = temp_dir.path().join("packages").join("app").join("src");
    let nm_dir = temp_dir
        .path()
        .join("packages")
        .join("app")
        .join("node_modules")
        .join("pkg");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&nm_dir).unwrap();
    fs::write(src_dir.join("index.ts"), "export const x = 1;").unwrap();
    fs::write(nm_dir.join("index.js"), "module.exports = {};").unwrap();
    fs::write(temp_dir.path().join(".gitignore"), "node_modules/\n").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(files.contains(&"packages/app/src/index.ts".to_string()));
    assert!(files.iter().all(|f| !f.contains("node_modules")));
}

#[test]
fn directory_exclusion_excludes_tracked_files_listed_in_codegraphignore() {
    let temp_dir = tempfile::tempdir().unwrap();
    let root = temp_dir.path();

    git(root, &["init", "-q"]);
    git(root, &["config", "user.email", "test@test.com"]);
    git(root, &["config", "user.name", "Test"]);

    fs::create_dir_all(root.join("src")).unwrap();
    fs::create_dir_all(root.join("research/decompiled-references/all")).unwrap();
    fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    fs::write(
        root.join("research/decompiled-references/all/generated.c"),
        "int generated(void) { return 1; }",
    )
    .unwrap();
    fs::write(
        root.join(".codegraphignore"),
        "research/decompiled-references/\n",
    )
    .unwrap();
    git(root, &["add", "-A"]);
    git(root, &["commit", "-q", "-m", "tracked corpus"]);

    let files = scan_directory(root, None);

    assert!(files.contains(&"src/main.rs".to_string()));
    assert!(!files.contains(&"research/decompiled-references/all/generated.c".to_string()));
}

#[test]
fn directory_exclusion_applies_a_nested_gitignore_only_to_its_own_subtree() {
    let temp_dir = tempfile::tempdir().unwrap();
    let app_src = temp_dir.path().join("app").join("src");
    fs::create_dir_all(&app_src).unwrap();
    fs::write(app_src.join("keep.ts"), "export const a = 1;").unwrap();
    fs::write(app_src.join("skip.ts"), "export const b = 2;").unwrap();
    fs::write(
        temp_dir.path().join("app").join(".gitignore"),
        "src/skip.ts\n",
    )
    .unwrap();
    // A sibling with the same name outside app/ must NOT be ignored.
    let other_dir = temp_dir.path().join("other").join("src");
    fs::create_dir_all(&other_dir).unwrap();
    fs::write(other_dir.join("skip.ts"), "export const c = 3;").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(files.contains(&"app/src/keep.ts".to_string()));
    assert!(!files.contains(&"app/src/skip.ts".to_string()));
    assert!(files.contains(&"other/src/skip.ts".to_string()));
}

#[test]
fn directory_exclusion_always_skips_git_directories() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    let git_dir = temp_dir.path().join(".git").join("objects");
    fs::create_dir_all(&src_dir).unwrap();
    fs::create_dir_all(&git_dir).unwrap();
    fs::write(src_dir.join("index.ts"), "export const x = 1;").unwrap();
    fs::write(git_dir.join("pack.ts"), "export const y = 2;").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert!(files.contains(&"src/index.ts".to_string()));
    assert!(files.iter().all(|f| !f.contains(".git")));
}

#[test]
fn directory_exclusion_returns_forward_slash_paths_on_all_platforms() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src").join("components");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("Button.tsx"), "export function Button() {}").unwrap();

    let files = scan_directory(temp_dir.path(), None);

    assert_eq!(files.len(), 1);
    assert_eq!(files[0], "src/components/Button.tsx");
    assert!(!files[0].contains('\\'));
}

// =============================================================================
// describe('Git Submodules')
// =============================================================================

#[test]
fn git_submodules_indexes_files_inside_git_submodules_issue_147() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Build a separate "library" repo to use as a submodule source.
    let lib_dir = temp_dir.path().join("_lib");
    fs::create_dir_all(&lib_dir).unwrap();
    git(&lib_dir, &["init", "-q"]);
    git(&lib_dir, &["config", "user.email", "test@test.com"]);
    git(&lib_dir, &["config", "user.name", "Test"]);
    fs::write(lib_dir.join("lib.ts"), "export const fromSubmodule = 1;").unwrap();
    git(&lib_dir, &["add", "-A"]);
    git(&lib_dir, &["commit", "-q", "-m", "lib init"]);

    // Build the main repo and add the lib repo as a submodule.
    let main_dir = temp_dir.path().join("main");
    fs::create_dir_all(&main_dir).unwrap();
    git(&main_dir, &["init", "-q"]);
    git(&main_dir, &["config", "user.email", "test@test.com"]);
    git(&main_dir, &["config", "user.name", "Test"]);
    fs::write(main_dir.join("app.ts"), "export const app = 1;").unwrap();
    git(&main_dir, &["add", "-A"]);
    git(&main_dir, &["commit", "-q", "-m", "app init"]);
    // protocol.file.allow=always is required to add a local-path submodule on
    // recent git versions (CVE-2022-39253 mitigation).
    git(
        &main_dir,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            lib_dir.to_str().unwrap(),
            "libs/lib",
        ],
    );
    git(&main_dir, &["commit", "-q", "-m", "add submodule"]);

    let files = scan_directory(&main_dir, None);

    assert!(files.contains(&"app.ts".to_string()));
    assert!(files.contains(&"libs/lib/lib.ts".to_string()));
}

// =============================================================================
// describe('Nested non-submodule git repos')
// =============================================================================

#[test]
fn nested_repos_indexes_files_in_embedded_git_repos_run_from_a_git_super_repo_issue_193() {
    let temp_dir = tempfile::tempdir().unwrap();

    // Top-level workspace is itself a git repo, holding no source directly —
    // the CMake "super-repo" layout from the issue.
    let root = temp_dir.path().join("root");
    fs::create_dir_all(root.join("coding")).unwrap();
    git(&root, &["init", "-q"]);
    git(&root, &["config", "user.email", "test@test.com"]);
    git(&root, &["config", "user.name", "Test"]);
    fs::write(
        root.join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.10)\n",
    )
    .unwrap();

    // Two independent clones living inside the workspace (NOT submodules):
    // one with committed source, one with only untracked source.
    let sub1 = root.join("sub_repo1").join("src");
    fs::create_dir_all(&sub1).unwrap();
    git(&root.join("sub_repo1"), &["init", "-q"]);
    git(
        &root.join("sub_repo1"),
        &["config", "user.email", "test@test.com"],
    );
    git(&root.join("sub_repo1"), &["config", "user.name", "Test"]);
    fs::write(sub1.join("one.ts"), "export const one = 1;").unwrap();
    git(&root.join("sub_repo1"), &["add", "-A"]);
    git(
        &root.join("sub_repo1"),
        &["commit", "-q", "-m", "sub1 init"],
    );

    let sub2 = root.join("sub_repo2").join("src");
    fs::create_dir_all(&sub2).unwrap();
    git(&root.join("sub_repo2"), &["init", "-q"]);
    fs::write(sub2.join("two.ts"), "export const two = 2;").unwrap();

    let files = scan_directory(&root, None);

    // Both committed and untracked source from the nested repos must be found.
    assert!(files.contains(&"sub_repo1/src/one.ts".to_string()));
    assert!(files.contains(&"sub_repo2/src/two.ts".to_string()));
}

#[test]
fn nested_repos_respects_each_embedded_repos_own_gitignore() {
    let temp_dir = tempfile::tempdir().unwrap();

    let root = temp_dir.path().join("root");
    fs::create_dir_all(&root).unwrap();
    git(&root, &["init", "-q"]);

    let sub = root.join("sub_repo").join("src");
    fs::create_dir_all(&sub).unwrap();
    git(&root.join("sub_repo"), &["init", "-q"]);
    fs::write(
        root.join("sub_repo").join(".gitignore"),
        "src/generated.ts\n",
    )
    .unwrap();
    fs::write(sub.join("real.ts"), "export const real = 1;").unwrap();
    fs::write(sub.join("generated.ts"), "export const generated = 1;").unwrap();

    let files = scan_directory(&root, None);

    assert!(files.contains(&"sub_repo/src/real.ts".to_string()));
    assert!(!files.contains(&"sub_repo/src/generated.ts".to_string()));
}

// =============================================================================
// describe('Scala Extraction')
// =============================================================================

#[test]
fn scala_detects_scala_files() {
    assert_eq!(detect_language("Main.scala", None), Language::Scala);
    assert_eq!(detect_language("script.sc", None), Language::Scala);
    assert_eq!(
        detect_language("src/UserService.scala", None),
        Language::Scala
    );
}

#[test]
fn scala_is_reported_as_supported() {
    assert!(is_language_supported(Language::Scala));
    assert!(get_supported_languages().contains(&Language::Scala));
}

#[test]
fn scala_extracts_class_definitions() {
    let code = "
class UserService(private val repo: UserRepository) {
  def findUser(id: String): Option[String] = Some(id)
}
";
    let result = extract("UserService.scala", code);
    let cls = find_named(&result, NodeKind::Class, "UserService").expect("UserService");
    assert_eq!(cls.language, Language::Scala);
}

#[test]
fn scala_extracts_object_definitions_as_class_kind() {
    let code = "
object DatabaseConfig {
  val url = \"jdbc:postgresql://localhost/mydb\"
}
";
    let result = extract("Config.scala", code);
    assert!(find_named(&result, NodeKind::Class, "DatabaseConfig").is_some());
}

#[test]
fn scala_extracts_trait_definitions_as_trait_kind() {
    let code = "
trait Repository[A] {
  def findById(id: String): Option[A]
  def save(entity: A): Unit
}
";
    let result = extract("Repository.scala", code);
    assert!(find_named(&result, NodeKind::Trait, "Repository").is_some());
}

#[test]
fn scala_extracts_method_definitions_inside_a_class() {
    let code = "
class Calculator {
  def add(a: Int, b: Int): Int = a + b
  def divide(a: Double, b: Double): Double = a / b
}
";
    let result = extract("Calculator.scala", code);
    let methods = filter_kind(&result, NodeKind::Method);
    assert!(methods.iter().any(|m| m.name == "add"));
    assert!(methods.iter().any(|m| m.name == "divide"));
}

#[test]
fn scala_extracts_method_signatures() {
    let code = "
class Greeter {
  def greet(name: String): String = s\"Hello, ${name}!\"
}
";
    let result = extract("Greeter.scala", code);
    let method = result
        .nodes
        .iter()
        .find(|n| n.name == "greet")
        .expect("greet");
    let signature = method.signature.as_deref().unwrap_or_default();
    assert!(signature.contains("name: String"));
    assert!(signature.contains("String"));
}

#[test]
fn scala_extracts_top_level_function_definitions_as_functions() {
    let code = "
def factorial(n: Int): Int = if (n <= 1) 1 else n * factorial(n - 1)
def greet(name: String): String = s\"Hello, ${name}!\"
";
    let result = extract("utils.scala", code);
    let fns = filter_kind(&result, NodeKind::Function);
    assert!(fns.iter().any(|f| f.name == "factorial"));
    assert!(fns.iter().any(|f| f.name == "greet"));
}

#[test]
fn scala_extracts_val_inside_a_class_as_field() {
    let code = "
class Config {
  val timeout: Int = 30
  val host: String = \"localhost\"
}
";
    let result = extract("Config.scala", code);
    let fields = filter_kind(&result, NodeKind::Field);
    assert!(fields.iter().any(|f| f.name == "timeout"));
    assert!(fields.iter().any(|f| f.name == "host"));
}

#[test]
fn scala_extracts_var_inside_a_class_as_field() {
    let code = "
class Counter {
  var count: Int = 0
}
";
    let result = extract("Counter.scala", code);
    assert!(find_named(&result, NodeKind::Field, "count").is_some());
}

#[test]
fn scala_extracts_top_level_val_as_constant() {
    let code = "
val MaxConnections: Int = 100
val DefaultTimeout = 30
";
    let result = extract("constants.scala", code);
    let consts = filter_kind(&result, NodeKind::Constant);
    assert!(consts.iter().any(|c| c.name == "MaxConnections"));
}

#[test]
fn scala_extracts_top_level_var_as_variable() {
    let code = "
var retries: Int = 3
";
    let result = extract("state.scala", code);
    assert!(find_named(&result, NodeKind::Variable, "retries").is_some());
}

#[test]
fn scala_includes_type_in_val_var_signature() {
    let code = "
class Service {
  val timeout: Int = 30
}
";
    let result = extract("Service.scala", code);
    let field = result
        .nodes
        .iter()
        .find(|n| n.name == "timeout")
        .expect("timeout");
    let signature = field.signature.as_deref().unwrap_or_default();
    assert!(signature.contains("timeout"));
    assert!(signature.contains("Int"));
}

#[test]
fn scala_extracts_enum_definitions() {
    let code = "
enum Color:
  case Red
  case Green
  case Blue
";
    let result = extract("Color.scala", code);
    assert!(find_named(&result, NodeKind::Enum, "Color").is_some());
}

#[test]
fn scala_extracts_enum_cases_as_enum_member() {
    let code = "
enum Direction:
  case North
  case South
  case East
  case West
";
    let result = extract("Direction.scala", code);
    let members = filter_kind(&result, NodeKind::EnumMember);
    assert!(members.iter().any(|m| m.name == "North"));
    assert!(members.iter().any(|m| m.name == "South"));
    assert!(members.len() >= 4);
}

#[test]
fn scala_extracts_type_aliases() {
    let code = "
type UserId = String
type UserMap = Map[String, String]
";
    let result = extract("types.scala", code);
    let aliases = filter_kind(&result, NodeKind::TypeAlias);
    assert!(aliases.iter().any(|a| a.name == "UserId"));
    assert!(aliases.iter().any(|a| a.name == "UserMap"));
}

#[test]
fn scala_extracts_import_declarations() {
    let code = "
import scala.collection.mutable.ListBuffer
import scala.concurrent.Future
";
    let result = extract("imports.scala", code);
    let imports = import_nodes(&result);
    assert!(imports.len() >= 2);
}

#[test]
fn scala_extracts_private_visibility() {
    let code = "
class Service {
  private val secret: String = \"abc\"
  private def helper(): Unit = {}
}
";
    let result = extract("Service.scala", code);
    let secret_field = result
        .nodes
        .iter()
        .find(|n| n.name == "secret")
        .expect("secret");
    assert_eq!(secret_field.visibility, Some(Visibility::Private));
    let helper_method = result
        .nodes
        .iter()
        .find(|n| n.name == "helper")
        .expect("helper");
    assert_eq!(helper_method.visibility, Some(Visibility::Private));
}

#[test]
fn scala_extracts_protected_visibility() {
    let code = "
class Base {
  protected def helperMethod(): Unit = {}
}
";
    let result = extract("Base.scala", code);
    let method = result
        .nodes
        .iter()
        .find(|n| n.name == "helperMethod")
        .expect("helperMethod");
    assert_eq!(method.visibility, Some(Visibility::Protected));
}

#[test]
fn scala_defaults_to_public_visibility() {
    let code = "
class Greeter {
  def hello(): Unit = {}
}
";
    let result = extract("Greeter.scala", code);
    let method = result
        .nodes
        .iter()
        .find(|n| n.name == "hello")
        .expect("hello");
    assert_eq!(method.visibility, Some(Visibility::Public));
}

#[test]
fn scala_extracts_extends_relationships() {
    let code = "
class AdminUser extends User {
  def adminAction(): Unit = {}
}
";
    let result = extract("AdminUser.scala", code);
    let extends_refs = refs_of_kind(&result, EdgeKind::Extends);
    assert!(extends_refs.iter().any(|r| r.reference_name == "User"));
}

#[test]
fn scala_extracts_function_call_expressions() {
    let code = "
def processData(): Unit = {
  val result = computeResult()
  println(result)
}
";
    let result = extract("processor.scala", code);
    let calls = refs_of_kind(&result, EdgeKind::Calls);
    assert!(!calls.is_empty());
}

// =============================================================================
// describe('Vue Extraction')
// =============================================================================

#[test]
fn vue_detects_vue_files() {
    assert_eq!(detect_language("App.vue", None), Language::Vue);
    assert_eq!(
        detect_language("components/Button.vue", None),
        Language::Vue
    );
    assert!(is_language_supported(Language::Vue));
}

#[test]
fn vue_extracts_component_node_from_a_vue_sfc() {
    let code = "<template>
  <div>{{ message }}</div>
</template>

<script>
export default {
  data() {
    return { message: 'Hello' };
  }
}
</script>
";
    let result = extract("HelloWorld.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");
    assert_eq!(component_node.name, "HelloWorld");
    assert_eq!(component_node.language, Language::Vue);
    assert_eq!(component_node.is_exported, Some(true));
}

#[test]
fn vue_extracts_functions_from_script_block() {
    let code = "<template>
  <button @click=\"handleClick\">Click</button>
</template>

<script>
function handleClick() {
  console.log('clicked');
}

const count = 0;
</script>
";
    let result = extract("Button.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");
    assert_eq!(component_node.name, "Button");

    let func_node = find_named(&result, NodeKind::Function, "handleClick").expect("handleClick");
    assert_eq!(func_node.language, Language::Vue);
}

#[test]
fn vue_extracts_from_script_setup_lang_ts_block() {
    let code = "<template>
  <div>{{ count }}</div>
</template>

<script setup lang=\"ts\">
import { ref } from 'vue';

const count = ref(0);

function increment(): void {
  count.value++;
}
</script>
";
    let result = extract("Counter.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");
    assert_eq!(component_node.name, "Counter");

    let func_node = find_named(&result, NodeKind::Function, "increment").expect("increment");
    assert_eq!(func_node.language, Language::Vue);

    // All nodes should be marked as vue language
    for node in &result.nodes {
        assert_eq!(node.language, Language::Vue);
    }
}

#[test]
fn vue_extracts_calls_from_top_level_script_setup_initializers() {
    let code = "<template>
  <div>{{ token }}</div>
</template>

<script setup lang=\"ts\">
import { getTokenMp } from './api/upload';

const token = getTokenMp();
</script>
";
    let result = extract("Issue425Setup.vue", code);

    assert!(find_ref(&result, EdgeKind::Calls, "getTokenMp").is_some());
}

#[test]
fn vue_extracts_calls_from_vue_options_api_object_methods() {
    let code = "<template>
  <button @click=\"save\">Save</button>
</template>

<script>
import { getTokenMp } from './api/upload';

export default {
  methods: {
    save() {
      return getTokenMp();
    }
  },
  setup() {
    return getTokenMp();
  }
}
</script>
";
    let result = extract("Issue425Options.vue", code);

    let calls: Vec<_> = result
        .unresolved_references
        .iter()
        .filter(|r| r.reference_kind == EdgeKind::Calls && r.reference_name == "getTokenMp")
        .collect();
    assert_eq!(calls.len(), 2);
}

#[test]
fn vue_extracts_component_usages_from_the_vue_template_issue_629() {
    let code = "<template>
  <div class=\"wrap\">
    <UserCard :user=\"u\" />
    <my-button>Click</my-button>
    <Transition><span>x</span></Transition>
  </div>
</template>

<script setup lang=\"ts\">
import UserCard from './UserCard.vue';
import MyButton from './MyButton.vue';
</script>
";
    let result = extract("Host.vue", code);
    let refs = ref_names(&refs_of_kind(&result, EdgeKind::References));

    assert!(refs.contains(&"UserCard".to_string())); // PascalCase tag
    assert!(refs.contains(&"MyButton".to_string())); // kebab <my-button> → MyButton
    assert!(!refs.contains(&"Transition".to_string())); // Vue built-in skipped
    assert!(!refs.contains(&"Div".to_string())); // native HTML element skipped
    assert!(!refs.contains(&"Span".to_string()));
}

#[test]
fn vue_extracts_from_both_script_and_script_setup_blocks() {
    let code = "<template>
  <div>{{ msg }}</div>
</template>

<script>
export default {
  name: 'DualScript'
}
</script>

<script setup>
const msg = 'hello';

function greet() {
  return msg;
}
</script>
";
    let result = extract("DualScript.vue", code);

    assert!(find_kind(&result, NodeKind::Component).is_some());
    assert!(find_named(&result, NodeKind::Function, "greet").is_some());
}

#[test]
fn vue_creates_component_node_for_template_only_vue_file() {
    let code = "<template>
  <div>Static content</div>
</template>
";
    let result = extract("Static.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");
    assert_eq!(component_node.name, "Static");
    assert_eq!(component_node.language, Language::Vue);

    // Only the component node should exist (no script nodes)
    assert_eq!(result.nodes.len(), 1);
}

#[test]
fn vue_creates_containment_edges_from_component_to_script_nodes() {
    let code = "<template>
  <div>{{ value }}</div>
</template>

<script setup lang=\"ts\">
const value = 42;
</script>
";
    let result = extract("Contained.vue", code);

    let component_node = find_kind(&result, NodeKind::Component).expect("component");

    // Should have containment edges from component to child nodes
    let contain_edges: Vec<_> = result
        .edges
        .iter()
        .filter(|e| e.source == component_node.id && e.kind == EdgeKind::Contains)
        .collect();
    assert!(!contain_edges.is_empty());
}

// =============================================================================
// describe('Instantiates + Decorates edge extraction')
// =============================================================================

#[test]
fn instantiates_emits_an_instantiates_ref_for_new_foo() {
    let code = "
class Foo {}
function bootstrap() { return new Foo(); }
";
    let result = extract("app.ts", code);
    assert!(find_ref(&result, EdgeKind::Instantiates, "Foo").is_some());
}

#[test]
fn instantiates_strips_type_argument_suffix_from_generic_constructors() {
    let code = "
class Container<T> { constructor(_: T) {} }
function go() { return new Container<string>('x'); }
";
    let result = extract("app.ts", code);
    let instantiates = refs_of_kind(&result, EdgeKind::Instantiates);
    let r = instantiates.first().expect("instantiates ref");
    // Container<string> must be normalised to "Container" — otherwise
    // resolution can never match the class node.
    assert_eq!(r.reference_name, "Container");
}

#[test]
fn instantiates_keeps_trailing_identifier_from_qualified_new_ns_foo() {
    let code = "
const ns = { Foo: class {} };
function go() { return new ns.Foo(); }
";
    let result = extract("app.ts", code);
    let instantiates = refs_of_kind(&result, EdgeKind::Instantiates);
    // We can't always resolve which Foo, but the name should be the
    // simple identifier so name-matching has a chance.
    assert_eq!(
        instantiates.first().map(|r| r.reference_name.as_str()),
        Some("Foo")
    );
}

#[test]
fn decorates_emits_a_decorates_ref_for_foo_class_x() {
    let code = "
function Foo(_arg: string) { return (cls: any) => cls; }
@Foo('x')
class X {}
";
    let result = extract("app.ts", code);
    assert!(find_ref(&result, EdgeKind::Decorates, "Foo").is_some());
}

#[test]
fn decorates_does_not_attribute_a_prior_classs_decorator_to_the_next_class() {
    // Regression: the sibling-walk must stop at the first non-
    // decorator separator. `@A class Foo {} @B class Bar {}` must
    // produce `decorates(Foo, A)` and `decorates(Bar, B)` — never
    // `decorates(Bar, A)`.
    let code = "
function A(cls: any) { return cls; }
function B(cls: any) { return cls; }
@A
class Foo {}
@B
class Bar {}
";
    let result = extract("app.ts", code);
    let decorates_edges = refs_of_kind(&result, EdgeKind::Decorates);
    // Exactly one decorates ref per decorated class, no cross-attribution.
    let from_bar: Vec<_> = decorates_edges
        .iter()
        .filter(|r| {
            result
                .nodes
                .iter()
                .any(|n| n.id == r.from_node_id && n.name == "Bar")
        })
        .collect();
    assert_eq!(from_bar.len(), 1);
    assert_eq!(from_bar[0].reference_name, "B");
}

#[test]
fn decorates_emits_a_decorates_ref_for_foo_method() {
    let code = "
function Get(p: string) { return (t: any, k: string) => t; }
class Svc {
  @Get('/x') method() { return 1; }
}
";
    let result = extract("app.ts", code);
    let decor_method = find_ref(&result, EdgeKind::Decorates, "Get").expect("decorates ref");
    // The decorated symbol must be `method`, not the constructor or class.
    let decorated_node = result
        .nodes
        .iter()
        .find(|n| n.id == decor_method.from_node_id)
        .expect("decorated node");
    assert_eq!(decorated_node.name, "method");
}

// =============================================================================
// describe('Lua Extraction')
// =============================================================================

#[test]
fn lua_detects_lua_files() {
    assert_eq!(detect_language("init.lua", None), Language::Lua);
    assert_eq!(detect_language("src/util.lua", None), Language::Lua);
}

#[test]
fn lua_is_reported_as_supported() {
    assert!(is_language_supported(Language::Lua));
    assert!(get_supported_languages().contains(&Language::Lua));
}

#[test]
fn lua_extracts_global_and_local_functions() {
    let code = "
function configure(opts) return opts end
local function helper(x) return x * 2 end
";
    let result = extract("init.lua", code);
    let funcs = names(&filter_kind(&result, NodeKind::Function));
    assert!(funcs.contains(&"configure".to_string()));
    assert!(funcs.contains(&"helper".to_string()));
    let configure = result
        .nodes
        .iter()
        .find(|n| n.name == "configure")
        .expect("configure");
    assert_eq!(configure.language, Language::Lua);
    assert_eq!(configure.signature.as_deref(), Some("(opts)"));
}

#[test]
fn lua_splits_table_method_functions_into_a_receiver_and_method_name() {
    let code = "
function M.connect(host, port) return host end
function M:send(data) return self end
";
    let result = extract("init.lua", code);
    let methods = filter_kind(&result, NodeKind::Method);
    let connect = methods
        .iter()
        .find(|m| m.name == "connect")
        .expect("connect");
    assert_eq!(connect.qualified_name, "M::connect");
    let send = methods.iter().find(|m| m.name == "send").expect("send");
    assert_eq!(send.qualified_name, "M::send");
}

#[test]
fn lua_extracts_local_variable_declarations() {
    let code = "
local M = {}
local count = 0
";
    let result = extract("mod.lua", code);
    let vars = names(&filter_kind(&result, NodeKind::Variable));
    assert!(vars.contains(&"M".to_string()));
    assert!(vars.contains(&"count".to_string()));
}

#[test]
fn lua_extracts_require_in_local_declarations_and_bare_calls() {
    let code = "
local socket = require(\"socket\")
local http = require \"resty.http\"
require(\"side.effect\")
";
    let result = extract("net.lua", code);
    let imports = names(&import_nodes(&result));
    assert!(imports.contains(&"socket".to_string()));
    assert!(imports.contains(&"resty.http".to_string()));
    assert!(imports.contains(&"side.effect".to_string()));

    assert!(find_ref(&result, EdgeKind::Imports, "socket").is_some());
}

#[test]
fn lua_keeps_extracting_require_across_many_sequential_parses() {
    // Regression guard ported from TS (the WASM-heap-corruption scenario can't
    // happen natively, but the loop is kept as a parity check).
    let mut last = None;
    for i in 0..8 {
        last = Some(extract(
            &format!("f{i}.lua"),
            &format!("local m = require(\"module.{i}\")\nreturn m\n"),
        ));
    }
    let last = last.unwrap();
    let imports = names(&import_nodes(&last));
    assert!(imports.contains(&"module.7".to_string()));
}

#[test]
fn lua_records_intra_file_calls_as_resolvable_references() {
    let code = "
local function helper(x) return x end
local function run(y) return helper(y) end
";
    let result = extract("calls.lua", code);
    assert!(find_ref(&result, EdgeKind::Calls, "helper").is_some());
}

// =============================================================================
// describe('Luau Extraction')
// =============================================================================

#[test]
fn luau_detects_luau_files() {
    assert_eq!(detect_language("init.luau", None), Language::Luau);
    assert_eq!(detect_language("src/Client.luau", None), Language::Luau);
}

#[test]
fn luau_is_reported_as_supported() {
    assert!(is_language_supported(Language::Luau));
    assert!(get_supported_languages().contains(&Language::Luau));
}

#[test]
fn luau_extracts_type_and_export_type_definitions() {
    let code = "
export type Vector = { x: number, y: number }
type Handler = (msg: string) -> boolean
";
    let result = extract("types.luau", code);
    let aliases = filter_kind(&result, NodeKind::TypeAlias);
    let vector = aliases.iter().find(|a| a.name == "Vector").expect("Vector");
    assert_eq!(vector.is_exported, Some(true));
    let handler = aliases
        .iter()
        .find(|a| a.name == "Handler")
        .expect("Handler");
    assert_eq!(handler.is_exported, Some(false));
}

#[test]
fn luau_captures_typed_signatures_and_splits_methods_by_receiver() {
    let code = "
function configure(opts: { debug: boolean }): boolean
\treturn opts.debug
end
function Client:fetch(path: string): Response
\treturn path
end
";
    let result = extract("client.luau", code);
    let configure = find_named(&result, NodeKind::Function, "configure").expect("configure");
    assert_eq!(configure.language, Language::Luau);
    assert_eq!(
        configure.signature.as_deref(),
        Some("(opts: { debug: boolean }): boolean")
    );
    let fetch = find_named(&result, NodeKind::Method, "fetch").expect("fetch");
    assert_eq!(fetch.qualified_name, "Client::fetch");
}

#[test]
fn luau_extracts_string_and_roblox_instance_path_require_imports() {
    let code = "
local http = require(\"http\")
local Signal = require(script.Parent.Signal)
local count = 0
";
    let result = extract("mod.luau", code);
    let imports = names(&import_nodes(&result));
    assert!(imports.contains(&"http".to_string())); // string require
    assert!(imports.contains(&"Signal".to_string())); // Roblox instance-path require
    let vars = names(&filter_kind(&result, NodeKind::Variable));
    assert!(vars.contains(&"count".to_string()));
}

// =============================================================================
// describe('Objective-C Extraction')
// =============================================================================

const OBJC_SAMPLE: &str = "
#import <Foundation/Foundation.h>
#import \"MyClass.h\"

@interface MyClass : NSObject <NSCopying>
@property (nonatomic, copy) NSString *name;
- (void)greet;
- (void)doThing:(id)x with:(id)y;
+ (instancetype)shared;
@end

@implementation MyClass

- (void)greet {
    NSLog(@\"Hello\");
    [self doWork];
}

- (void)doThing:(id)x with:(id)y {
    [self notify:x];
}

+ (instancetype)shared {
    return [[MyClass alloc] init];
}

@end

void helperFunction(int count) {
    MyClass *obj = [MyClass shared];
    [obj greet];
}
";

#[test]
fn objc_extracts_classes_methods_functions_and_imports() {
    let result = extract("App.m", OBJC_SAMPLE);

    let classes = filter_kind(&result, NodeKind::Class);
    assert_eq!(classes.iter().filter(|c| c.name == "MyClass").count(), 1);

    let methods = filter_kind(&result, NodeKind::Method);
    let mut method_names = names(&methods);
    method_names.sort();
    assert_eq!(method_names, vec!["doThing:with:", "greet", "shared"]);

    let shared = methods.iter().find(|m| m.name == "shared").expect("shared");
    assert_eq!(shared.is_static, Some(true));

    let properties = filter_kind(&result, NodeKind::Property);
    assert!(properties.iter().any(|p| p.name == "name"));

    let functions = filter_kind(&result, NodeKind::Function);
    assert!(functions.iter().any(|f| f.name == "helperFunction"));

    let imports = names(&import_nodes(&result));
    assert!(imports.contains(&"Foundation/Foundation.h".to_string()));
    assert!(imports.contains(&"MyClass.h".to_string()));
}

#[test]
fn objc_records_inheritance_and_protocol_conformance() {
    let result = extract("App.m", OBJC_SAMPLE);
    let extends_refs = ref_names(&refs_of_kind(&result, EdgeKind::Extends));
    let implements_refs = ref_names(&refs_of_kind(&result, EdgeKind::Implements));
    assert!(extends_refs.contains(&"NSObject".to_string()));
    assert!(implements_refs.contains(&"NSCopying".to_string()));
}

#[test]
fn objc_records_message_sends_and_c_calls() {
    let result = extract("App.m", OBJC_SAMPLE);
    let calls = ref_names(&refs_of_kind(&result, EdgeKind::Calls));
    for expected in ["NSLog", "doWork", "MyClass.shared", "obj.greet"] {
        assert!(calls.contains(&expected.to_string()), "missing {expected}");
    }
}

#[test]
fn objc_reconstructs_multi_keyword_selectors_at_the_call_site() {
    // Regression for the gap discovered post-#165: message_expression's
    // multi-keyword form `[obj a:1 b:2]` was only emitting the first keyword,
    // so calls never resolved to multi-part method definitions like
    // `GET:parameters:headers:progress:success:failure:`. The call-site name
    // must match the method-definition name with full keywords + trailing colons.
    let code = "
@implementation Caller
- (void)demo {
    NSMutableDictionary *d = [NSMutableDictionary new];
    [d setObject:@\"v\" forKey:@\"k\"];
    [d setObject:@\"v2\" forKey:@\"k2\" withRetry:@YES];
    [self touchesBegan:nil withEvent:nil];
}
@end
";
    let result = extract("Caller.m", code);
    let calls = ref_names(&refs_of_kind(&result, EdgeKind::Calls));
    for expected in [
        "d.setObject:forKey:",
        "d.setObject:forKey:withRetry:",
        "touchesBegan:withEvent:",
    ] {
        assert!(calls.contains(&expected.to_string()), "missing {expected}");
    }
}

#[test]
fn objc_does_not_classify_pure_c_headers_with_at_end_in_comments_as_objc() {
    let c_header = "/* @end of file */\n#ifndef STDIO_H\nvoid printf(const char *);\n#endif\n";
    assert_eq!(detect_language("stdio.h", Some(c_header)), Language::C);
}

#[test]
fn objc_extracts_protocol_declarations() {
    let code = "
@protocol DataSource <NSObject>
- (NSInteger)numberOfItems;
@end
";
    let result = extract("DataSource.h", code);
    assert!(find_named(&result, NodeKind::Protocol, "DataSource").is_some());
}

#[test]
fn objc_is_reported_as_supported() {
    assert!(is_language_supported(Language::Objc));
    assert!(get_supported_languages().contains(&Language::Objc));
}

// =============================================================================
// describe('Regression: issue-specific extraction fixes')
// =============================================================================

#[test]
fn regression_indexes_inner_functions_of_an_anonymous_amd_commonjs_module_wrapper_issue_528() {
    let code = "
define(['dep'], function (dep) {
  function innerHelper(x) { return x + 1; }
  function compute(y) { return innerHelper(y); }
  return { compute: compute };
});
";
    let result = extract("amd-module.js", code);
    let fns = names(&filter_kind(&result, NodeKind::Function));
    assert!(fns.contains(&"innerHelper".to_string()));
    assert!(fns.contains(&"compute".to_string()));
}

#[test]
fn regression_attaches_go_methods_on_generic_receivers_to_their_type_issue_583() {
    let code = "
package main

type Stack[T any] struct { items []T }

func (s *Stack[T]) Push(v T) { s.items = append(s.items, v) }
func (s Stack[T]) Len() int { return len(s.items) }
";
    let result = extract("stack.go", code);
    let methods = filter_kind(&result, NodeKind::Method);
    assert_eq!(
        methods
            .iter()
            .find(|m| m.name == "Push")
            .map(|m| m.qualified_name.as_str()),
        Some("Stack::Push")
    );
    assert_eq!(
        methods
            .iter()
            .find(|m| m.name == "Len")
            .map(|m| m.qualified_name.as_str()),
        Some("Stack::Len")
    );
}

#[test]
fn regression_indexes_new_module_extensions_mts_cts_xsjs_xsjslib_issues_366_556() {
    assert!(is_source_file("mod.mts"));
    assert!(is_source_file("mod.cts"));
    assert!(is_source_file("service.xsjs"));
    assert!(is_source_file("lib.xsjslib"));
    assert_eq!(detect_language("mod.mts", None), Language::Typescript);
    assert_eq!(detect_language("service.xsjs", None), Language::Javascript);

    // End-to-end: a .mts file is parsed as TS, a .xsjs file as JS.
    let ts = extract("mod.mts", "export function hello(): number { return 1; }");
    assert!(
        ts.nodes
            .iter()
            .any(|n| n.name == "hello" && n.kind == NodeKind::Function)
    );
    let js = extract("service.xsjs", "function handleRequest() { return 1; }");
    assert!(
        js.nodes
            .iter()
            .any(|n| n.name == "handleRequest" && n.kind == NodeKind::Function)
    );
}

// =============================================================================
// describe('object-literal method extraction')
// (from __tests__/object-literal-methods.test.ts — the extraction half; the
// end-to-end resolution describe needs CodeGraph + resolution, deferred)
// =============================================================================

#[test]
fn object_literal_extracts_zustand_store_actions_as_function_nodes() {
    let code = "
      import { create } from 'zustand'
      interface Store {
        count: number
        fetchUser(): Promise<void>
        switchOrganization(id: string): Promise<void>
        reset(): void
      }
      export const useStore = create<Store>((set, get) => ({
        count: 0,
        fetchUser: async () => { await get().reset() },
        switchOrganization: async (id: string) => { set({ count: 1 }) },
        reset: () => set({ count: 0 }),
      }))
    ";
    let result = extract("store.ts", code);
    let fn_names = names(&filter_kind(&result, NodeKind::Function));
    assert!(fn_names.contains(&"fetchUser".to_string()));
    assert!(fn_names.contains(&"switchOrganization".to_string()));
    assert!(fn_names.contains(&"reset".to_string()));

    // Each action's body was walked: fetchUser references its sibling `reset`,
    // so an in-store calls edge will resolve once the pipeline runs.
    let fetch_user = result
        .nodes
        .iter()
        .find(|n| n.name == "fetchUser")
        .expect("fetchUser");
    let fetch_user_refs: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.from_node_id == fetch_user.id)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(fetch_user_refs.contains(&"reset"));

    // The action's body wasn't mis-attributed to the file scope (the reason we
    // skip the generic body-visit for the store-factory call).
    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    let file_refs: Vec<&str> = result
        .unresolved_references
        .iter()
        .filter(|r| r.from_node_id == file_node.id)
        .map(|r| r.reference_name.as_str())
        .collect();
    assert!(!file_refs.contains(&"reset"));
}

#[test]
fn object_literal_extracts_actions_through_a_middleware_wrapper() {
    let code = "
      import { create } from 'zustand'
      import { persist } from 'zustand/middleware'
      export const useCounter = create(
        persist(
          (set, get) => ({
            value: 0,
            increment: () => set({ value: get().value + 1 }),
          }),
          { name: 'counter' }
        )
      )
    ";
    let result = extract("counter.ts", code);
    let fn_names = names(&filter_kind(&result, NodeKind::Function));
    assert!(fn_names.contains(&"increment".to_string()));
}

#[test]
fn object_literal_extracts_actions_when_the_initializer_returns_via_a_block() {
    let code = "
      import { create } from 'zustand'
      export const useThing = create((set) => {
        const initial = 0
        return {
          value: initial,
          bump: () => set({ value: 1 }),
        }
      })
    ";
    let result = extract("thing.ts", code);
    let fn_names = names(&filter_kind(&result, NodeKind::Function));
    assert!(fn_names.contains(&"bump".to_string()));
}

#[test]
fn object_literal_does_not_extract_methods_from_a_non_exported_call_wrapped_object() {
    let code = "
      function wrap(f: any) { return f }
      const local = wrap(() => ({ shouldNotExtract: () => {} }))
    ";
    let result = extract("inline.ts", code);
    let all_names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(!all_names.contains(&"shouldNotExtract"));
}

#[test]
fn object_literal_still_extracts_the_existing_direct_object_shape() {
    let code = "
      export const actions = {
        load: async () => { helper() },
      }
      function helper() {}
    ";
    let result = extract("actions.ts", code);
    let fn_names = names(&filter_kind(&result, NodeKind::Function));
    assert!(fn_names.contains(&"load".to_string()));
}

// =============================================================================
// Byte ranges (schema v5) — tree-sitter start_byte()/end_byte() stored on nodes
// =============================================================================

#[test]
fn byte_ranges_match_source_slices_for_typescript() {
    let code = "export function add(a: number, b: number): number {\n  return a + b;\n}\n\nexport class Greeter {\n  greet(name: string): string {\n    return `hi ${name}`;\n  }\n}\n";
    let result = extract("src/bytes.ts", code);

    // Every tree-sitter-extracted node carries a well-formed byte range.
    for node in &result.nodes {
        let range = node
            .byte_range()
            .unwrap_or_else(|| panic!("node {} ({:?}) missing byte range", node.name, node.kind));
        assert!(
            range.end <= code.len(),
            "range {range:?} exceeds source for {}",
            node.name
        );
    }

    // The file node spans the whole source.
    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    assert_eq!(file_node.byte_range(), Some(0..code.len()));

    // Function/class/method ranges anchor exactly at their tree-sitter nodes
    // and slice back to the original declaration text.
    let add = find_named(&result, NodeKind::Function, "add").expect("add");
    let add_range = add.byte_range().unwrap();
    assert_eq!(add_range.start, code.find("function add").unwrap());
    let add_src = &code[add_range];
    assert!(add_src.starts_with("function add"));
    assert!(add_src.ends_with("return a + b;\n}"));

    let greeter = find_named(&result, NodeKind::Class, "Greeter").expect("Greeter");
    let greeter_src = &code[greeter.byte_range().unwrap()];
    assert!(greeter_src.starts_with("class Greeter"));
    assert!(greeter_src.ends_with('}'));

    let greet = find_named(&result, NodeKind::Method, "greet").expect("greet");
    let greet_src = &code[greet.byte_range().unwrap()];
    assert!(greet_src.starts_with("greet(name: string)"));
    assert!(greet_src.contains("return `hi ${name}`;"));
}

#[test]
fn byte_ranges_round_trip_through_full_indexing() {
    let temp_dir = tempfile::tempdir().unwrap();
    let src_dir = temp_dir.path().join("src");
    fs::create_dir(&src_dir).unwrap();
    let code = "export function add(a: number, b: number): number {\n  return a + b;\n}\n";
    fs::write(src_dir.join("utils.ts"), code).unwrap();

    let (_conn, queries) = open_graph(temp_dir.path());
    let orch = ExtractionOrchestrator::new(temp_dir.path(), &queries);
    let result = orch.index_all(None, None, false).expect("index_all");
    assert!(result.success);

    // Byte offsets survive SQLite storage and slice the on-disk source back
    // to the declaration.
    let nodes = queries.get_nodes_by_file("src/utils.ts").unwrap();
    let add = nodes.iter().find(|n| n.name == "add").expect("add");
    let range = add.byte_range().expect("byte range stored");
    let on_disk = fs::read_to_string(src_dir.join("utils.ts")).unwrap();
    assert!(on_disk[range].starts_with("function add"));

    let file_node = nodes
        .iter()
        .find(|n| n.kind == NodeKind::File)
        .expect("file node");
    assert_eq!(file_node.byte_range(), Some(0..code.len()));
}

#[test]
fn byte_ranges_svelte_script_nodes_are_whole_file_offsets() {
    let code = "<script>\n  function bump(n) {\n    return n + 1;\n  }\n</script>\n\n<button on:click={() => bump(1)}>+</button>\n";
    let result = extract("src/Counter.svelte", code);

    // The component node spans the whole .svelte file.
    let component = find_kind(&result, NodeKind::Component).expect("component node");
    assert_eq!(component.byte_range(), Some(0..code.len()));

    // Script-block nodes are remapped from slice-relative to whole-file
    // offsets, so slicing the full source recovers the declaration.
    let bump = find_named(&result, NodeKind::Function, "bump").expect("bump");
    let range = bump.byte_range().expect("script node byte range");
    assert_eq!(range.start, code.find("function bump").unwrap());
    assert!(code[range].starts_with("function bump"));
}

#[test]
fn byte_ranges_vue_script_nodes_are_whole_file_offsets() {
    let code = "<template>\n  <button @click=\"bump\">+</button>\n</template>\n\n<script setup>\nfunction bump(n) {\n  return n + 1;\n}\n</script>\n";
    let result = extract("src/Counter.vue", code);

    let component = find_kind(&result, NodeKind::Component).expect("component node");
    assert_eq!(component.byte_range(), Some(0..code.len()));

    let bump = find_named(&result, NodeKind::Function, "bump").expect("bump");
    let range = bump.byte_range().expect("script node byte range");
    assert_eq!(range.start, code.find("function bump").unwrap());
    assert!(code[range].starts_with("function bump"));
}

#[test]
fn byte_ranges_absent_for_extractors_without_offsets() {
    // The IDA-C extractor tracks line/column only — its function nodes keep
    // NULL byte offsets (honest absence), while its file node spans the file.
    let code = "//----- (00000001800012F0) ----------------------------------------------------\n// Function: sub_1800012F0\n__int64 __fastcall sub_1800012F0(__int64 a1)\n{\n  return a1 + 1;\n}\n";
    assert!(is_ida_generated_c("sub_1800012F0.c", code));
    let result = extract("sub_1800012F0.c", code);

    let file_node = find_kind(&result, NodeKind::File).expect("file node");
    assert_eq!(file_node.byte_range(), Some(0..code.len()));

    let func = find_kind(&result, NodeKind::Function).expect("function node");
    assert_eq!(func.start_byte, None);
    assert_eq!(func.end_byte, None);
    assert_eq!(func.byte_range(), None);
}
