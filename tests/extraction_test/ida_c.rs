use crate::extraction_test::fixture::*;

// =============================================================================
// describe('IDA C Extraction')
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn ida_c_extracts_leading_dot_thunk_functions_and_alias_target() {
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

#[tokio::test(flavor = "current_thread")]
async fn ida_c_extracts_hexrays_sub_functions_and_call_references_without_tree_sitter() {
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

#[tokio::test(flavor = "current_thread")]
async fn ida_c_detects_named_decompiler_outputs_and_filters_pseudo_calls() {
    let code = r#"
__int64 __fastcall clang_Type_getNullability(
        __int64 a1,
        __int64 a2,
        __int64 a3,
        __int64 a4,
        __int64 a5,
        __int64 a6,
        int a7,
        __int64 a8)
{
  unsigned __int16 v8; // ax

  if ( (a8 & 0xFFFFFFFFFFFFFFF8LL) > 0xF
    && (v8 = sub_B16670(*(_QWORD *)(a8 & 0xFFFFFFFFFFFFFFF0LL)), HIBYTE(v8))
    && (unsigned __int8)v8 <= 3u )
  {
    return *((unsigned int *)qword_3C8C240 + (unsigned __int8)v8);
  }
  else
  {
    return 3;
  }
}
"#;

    let path = "binary/libclang/all/clang_Type_getNullability.c";
    assert!(is_ida_generated_c(path, code));

    let result = extract(path, code);
    let call_names = ref_names(&refs_of_kind(&result, EdgeKind::Calls));
    assert!(call_names.contains(&"sub_B16670".to_string()));
    assert!(!call_names.contains(&"HIBYTE".to_string()));
    assert!(
        result
            .nodes
            .iter()
            .any(|n| n.kind == NodeKind::DataSymbol && n.name == "qword_3C8C240")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn ida_c_does_not_classify_plain_fastcall_c_without_decompiler_markers() {
    let code = r#"
__int64 __fastcall exported_function(__int64 value)
{
  return value;
}
"#;

    assert!(!is_ida_generated_c("src/exported_function.c", code));
}

#[tokio::test(flavor = "current_thread")]
async fn ida_c_filters_cpu_intrinsics_and_syntax_artifacts_without_dropping_real_calls() {
    let code = r#"
__int64 __fastcall sub_4400(__int64 a1)
{
  __int64 v2; // x0

  __debugbreak();
  __break(0x3E8u);
  __dmb(0xBu);
  v2 = __ldrex(a1);
  __strex(v2, a1);
  _ReadStatusReg(0xC000u);
  if ( _bittest64(&v2, a1) )
    __attribute__(0);
  return sub_5500(a1);
}
"#;

    let result = extract("all/sub_4400.c", code);
    let call_names = ref_names(&refs_of_kind(&result, EdgeKind::Calls));
    assert!(call_names.contains(&"sub_5500".to_string()));
    for pseudo in [
        "__debugbreak",
        "__break",
        "__dmb",
        "__ldrex",
        "__strex",
        "_ReadStatusReg",
        "_bittest64",
        "__attribute__",
    ] {
        assert!(
            !call_names.contains(&pseudo.to_string()),
            "{pseudo} leaked as a call: {call_names:?}"
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn ida_c_extracts_alignment_and_function_table_data_symbols() {
    let code = r#"
__int64 __fastcall sub_6600(__int64 a1)
{
  if ( *(_DWORD *)algn_811D3B0 )
    (*(void (**)(void))algn_811D3B0)();
  return funcs_80D7A54[a1] + *(_QWORD *)tbyte_401000;
}
"#;

    let result = extract("all/sub_6600.c", code);
    let data_names = names(&filter_kind(&result, NodeKind::DataSymbol));
    assert!(data_names.contains(&"algn_811D3B0".to_string()));
    assert!(data_names.contains(&"funcs_80D7A54".to_string()));
    assert!(data_names.contains(&"tbyte_401000".to_string()));
}

#[tokio::test(flavor = "current_thread")]
async fn ida_c_extracts_memory_call_role_and_cfg_facts() {
    let code = r#"
__int64 __fastcall sub_7000(__int64 a1, char *dst, char *src)
{
  __int64 v4; // rax

  *(_DWORD *)(a1 + 148) = 7;
  v4 = *(_QWORD *)(a1 + 152);
  memcpy(dst, src, 32);
  if ( v4 )
    goto LABEL_3;
  switch ( *(int *)(a1 + 156) )
  {
    case 0:
      return jpt_401000[0];
    default:
LABEL_3:
      return 0;
  }
}
"#;

    let result = extract("all/sub_7000.c", code);

    let data_names = names(&filter_kind(&result, NodeKind::DataSymbol));
    assert!(data_names.contains(&"mem:a1+148".to_string()));
    assert!(data_names.contains(&"mem:a1+152".to_string()));
    assert!(data_names.contains(&"mem:a1+156".to_string()));
    assert!(data_names.contains(&"callarg:memcpy:8:2".to_string()));
    assert!(data_names.contains(&"label:LABEL_3".to_string()));
    assert!(data_names.contains(&"switch:11".to_string()));
    assert!(data_names.contains(&"jpt_401000".to_string()));

    let has_memory_write = result.edges.iter().any(|edge| {
        edge.kind == EdgeKind::Writes
            && edge.target == "data_symbol:mem:a1+148"
            && edge
                .metadata
                .as_ref()
                .and_then(|m| m.get("kind"))
                .and_then(|v| v.as_str())
                == Some("memory_access")
    });
    assert!(has_memory_write);

    let memcpy_ref = find_ref(&result, EdgeKind::Calls, "memcpy").expect("memcpy call ref");
    let roles = memcpy_ref
        .metadata
        .as_ref()
        .and_then(|m| m.get("arguments"))
        .and_then(|v| v.as_array())
        .expect("memcpy role metadata");
    assert!(
        roles
            .iter()
            .any(|v| v.get("role").and_then(|r| r.as_str()) == Some("write_dst"))
    );
    assert!(
        roles
            .iter()
            .any(|v| v.get("role").and_then(|r| r.as_str()) == Some("read_src"))
    );
    assert!(
        roles
            .iter()
            .any(|v| v.get("role").and_then(|r| r.as_str()) == Some("size"))
    );

    let cfg_roles: Vec<&str> = result
        .edges
        .iter()
        .filter_map(|edge| edge.metadata.as_ref())
        .filter(|m| m.get("kind").and_then(|v| v.as_str()) == Some("ida_cfg"))
        .filter_map(|m| m.get("role").and_then(|v| v.as_str()))
        .collect();
    assert!(cfg_roles.contains(&"label"));
    assert!(cfg_roles.contains(&"goto"));
    assert!(cfg_roles.contains(&"switch"));
    assert!(cfg_roles.contains(&"jump_table"));
}

#[tokio::test(flavor = "current_thread")]
async fn ida_c_extracts_parameters_locals_and_type_edges() {
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

#[tokio::test(flavor = "current_thread")]
async fn ida_c_does_not_classify_ordinary_c_files_as_ida_dumps() {
    let code = r#"
int main(void) {
  return puts("hello");
}
"#;
    assert!(!is_ida_generated_c("src/main.c", code));
}

#[tokio::test(flavor = "current_thread")]
async fn ida_c_indexes_oversized_ida_dumps_with_the_lightweight_extractor() {
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
        .await
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
