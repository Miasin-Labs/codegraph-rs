use crate::extraction_test::fixture::*;

// =============================================================================
// describe('IDA C Extraction')
// =============================================================================

#[test]
fn ida_c_extracts_leading_dot_thunk_functions_and_alias_target() {
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

    // The thunk's forwarding target is an `Aliases` edge, not a `Calls`.
    let aliases = refs_of_kind(&result, EdgeKind::Aliases);
    assert!(aliases.iter().any(|r| r.reference_name == "mysql_init"));
    let calls = refs_of_kind(&result, EdgeKind::Calls);
    assert!(!calls.iter().any(|r| r.reference_name == "mysql_init"));
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
