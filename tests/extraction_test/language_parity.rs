use crate::extraction_test::fixture::*;

#[test]
fn parity_language_extensions_are_detected_and_scanned() {
    let cases = [
        ("entry/src/main/ets/pages/Index.ets", Language::Arkts),
        ("Pages/Index.razor", Language::Razor),
        ("Views/Login.cshtml", Language::Razor),
        ("src/pages/index.astro", Language::Astro),
        ("analysis.R", Language::R),
        ("scripts/clean.r", Language::R),
        ("contracts/Vault.sol", Language::Solidity),
        ("default.nix", Language::Nix),
        ("Service.cfc", Language::Cfml),
        ("index.cfm", Language::Cfml),
        ("Helper.cfs", Language::Cfscript),
        ("app/cbl/CBACT01C.cbl", Language::Cobol),
        ("app/cbl/CBSTM03A.CBL", Language::Cobol),
        ("legacy/prog.cob", Language::Cobol),
        ("legacy/prog.cobol", Language::Cobol),
        ("app/cpy/CVACT01Y.cpy", Language::Cobol),
        ("app/Forms/MainForm.vb", Language::Vbnet),
        ("src/my_server.erl", Language::Erlang),
        ("include/records.hrl", Language::Erlang),
        ("bin/release_tool.escript", Language::Erlang),
        ("src/myapp.app.src", Language::Erlang),
        ("ebin/myapp.app", Language::Erlang),
        ("main.tf", Language::Terraform),
        ("terraform.tfvars", Language::Terraform),
        ("versions.tofu", Language::Terraform),
    ];

    for (path, expected) in cases {
        assert_eq!(
            detect_language(path, None),
            expected,
            "wrong language for {path}"
        );
        assert!(is_source_file(path), "{path} should be scanned");
    }

    assert_eq!(
        detect_language("legacy/module.src", None),
        Language::Unknown,
        "only the full .app.src suffix is Erlang"
    );
    assert!(!is_source_file("legacy/module.src"));
}

#[test]
fn parity_languages_are_reported_as_supported() {
    let languages = get_supported_languages();
    for language in [
        Language::Arkts,
        Language::Razor,
        Language::Astro,
        Language::R,
        Language::Solidity,
        Language::Nix,
        Language::Cfml,
        Language::Cfscript,
        Language::Cfquery,
        Language::Cobol,
        Language::Vbnet,
        Language::Erlang,
        Language::Terraform,
    ] {
        assert!(
            is_language_supported(language),
            "{language} should be supported"
        );
        assert!(
            languages.contains(&language),
            "{language} should be listed as supported"
        );
    }
}

#[test]
fn arkts_razor_and_astro_extract_framework_symbols() {
    let arkts = extract(
        "pages/Index.ets",
        "@Entry\n@Component\nstruct Index {\n  build() { Column() {} }\n}\n",
    );
    let component = find_named(&arkts, NodeKind::Struct, "Index")
        .expect("ArkTS @Component struct should be extracted");
    assert_eq!(component.language, Language::Arkts);

    let razor = extract(
        "Pages/Dashboard.razor",
        "@model DashboardModel\n<WidgetCard />\n@code { private void Refresh() {} }\n",
    );
    let component = find_named(&razor, NodeKind::Component, "Dashboard")
        .expect("Razor file should produce its component");
    assert_eq!(component.language, Language::Razor);
    assert!(find_ref(&razor, EdgeKind::References, "DashboardModel").is_some());
    assert!(find_ref(&razor, EdgeKind::References, "WidgetCard").is_some());

    let astro = extract(
        "src/components/Card.astro",
        "---\nexport function title() { return 'Card'; }\n---\n<h1>{title()}</h1>\n",
    );
    let component = find_named(&astro, NodeKind::Component, "Card")
        .expect("Astro file should produce its component");
    assert_eq!(component.language, Language::Astro);
    assert!(
        find_named(&astro, NodeKind::Function, "title").is_some(),
        "Astro frontmatter should delegate to TypeScript extraction"
    );
}

#[test]
fn r_solidity_and_nix_extract_native_declarations() {
    let r = extract(
        "analysis.R",
        "clean_data <- function(df) {\n  scale(df)\n}\n",
    );
    let function = find_named(&r, NodeKind::Function, "clean_data")
        .expect("R function assignment should be extracted");
    assert_eq!(function.language, Language::R);

    let solidity = extract(
        "contracts/Vault.sol",
        "pragma solidity ^0.8.20;\ncontract Vault {\n  function deposit(uint amount) external {}\n}\n",
    );
    let contract = find_named(&solidity, NodeKind::Class, "Vault")
        .expect("Solidity contract should be extracted as a class");
    assert_eq!(contract.language, Language::Solidity);
    assert!(find_named(&solidity, NodeKind::Method, "deposit").is_some());

    let nix = extract(
        "default.nix",
        "let\n  increment = value: value + 1;\nin\n  increment 2\n",
    );
    let function = find_named(&nix, NodeKind::Function, "increment")
        .expect("Nix lambda binding should be extracted as a function");
    assert_eq!(function.language, Language::Nix);
}

#[test]
fn cfml_dialects_extract_components_methods_and_query_calls() {
    let cfml = extract(
        "Query.cfc",
        "<cfcomponent>\n<cffunction name=\"getUsers\">\n<cfquery name=\"qUsers\">\nSELECT id FROM users WHERE owner = #getCurrentUser()#\n</cfquery>\n</cffunction>\n</cfcomponent>\n",
    );
    let component = find_named(&cfml, NodeKind::Class, "Query")
        .expect("tag-based CFML component should use the file stem as its name");
    assert_eq!(component.language, Language::Cfml);
    assert!(find_named(&cfml, NodeKind::Method, "getUsers").is_some());
    assert!(find_ref(&cfml, EdgeKind::Calls, "getCurrentUser").is_some());

    let cfscript = extract(
        "Sample.cfs",
        "component {\n  function ping() { return \"pong\"; }\n}\n",
    );
    let component = find_named(&cfscript, NodeKind::Class, "Sample")
        .expect("CFScript anonymous component should use the file stem as its name");
    assert_eq!(component.language, Language::Cfscript);
    assert!(find_named(&cfscript, NodeKind::Method, "ping").is_some());

    let cfquery = extract_from_source(
        "embedded.cfquery",
        "SELECT id FROM users WHERE owner = #getCurrentUser()#",
        Some(Language::Cfquery),
        None,
    );
    assert!(
        find_ref(&cfquery, EdgeKind::Calls, "getCurrentUser").is_some(),
        "the internal CFQuery grammar should expose calls in hash expressions"
    );
}

#[test]
fn cobol_vbnet_and_erlang_extract_primary_program_symbols() {
    let cobol = extract(
        "TESTPROG.cbl",
        "       IDENTIFICATION DIVISION.\n       PROGRAM-ID. TESTPROG.\n       PROCEDURE DIVISION.\n       MAIN.\n           GOBACK.\n",
    );
    let program = find_named(&cobol, NodeKind::Module, "TESTPROG")
        .expect("COBOL PROGRAM-ID should produce a module");
    assert_eq!(program.language, Language::Cobol);
    assert!(find_named(&cobol, NodeKind::Function, "MAIN").is_some());

    let vbnet = extract(
        "Invoice.vb",
        "Public Class Invoice\n    Public Function Total() As Integer\n        Return 1\n    End Function\nEnd Class\n",
    );
    let class =
        find_named(&vbnet, NodeKind::Class, "Invoice").expect("VB.NET class should be extracted");
    assert_eq!(class.language, Language::Vbnet);
    assert!(find_named(&vbnet, NodeKind::Method, "Total").is_some());

    let erlang = extract(
        "src/my_server.erl",
        "-module(my_server).\n-export([start/0]).\n\nstart() -> ok.\n",
    );
    let namespace = find_named(&erlang, NodeKind::Namespace, "my_server")
        .expect("Erlang -module should produce a namespace");
    assert_eq!(namespace.language, Language::Erlang);
    assert!(find_named(&erlang, NodeKind::Function, "start").is_some());
}

#[test]
fn cobol_error_fixture_with_short_trailing_line_finishes_parsing() {
    let source = concat!(
        "*> { dg-options \"-fdiagnostics-show-caret\" } \n",
        "*> { dg-do compile }\n",
        "\n",
        "       identification division.\n",
        "       porgram-id. hello. *> { dg-error \"8: syntax error, unexpected NAME, expecting FUNCTION or PROGRAM-ID\" }\n",
        "       procedure division.\n",
        "           display \"Hello World!\".\n",
        "           stop run.\n",
        "\n",
        "*<<\n",
        "{ dg-begin-multiline-output \"\" }\n",
        "        porgram-id. hello.\n",
        "        ^~~~~~~~~~~\n",
        "{ dg-end-multiline-output \"\" }\n",
        "*>>\n",
    );

    let result = extract("typo-1.cob", source);
    assert!(
        find_named(&result, NodeKind::File, "typo-1.cob").is_some(),
        "malformed COBOL should still produce a file node"
    );
}

#[test]
fn cobol_free_format_with_trailing_newline_finishes_parsing() {
    let source = concat!(
        "IDENTIFICATION DIVISION.\n",
        "PROGRAM-ID. HELLO.\n",
        "PROCEDURE DIVISION.\n",
        "DISPLAY \"Hello World!\".\n",
        "STOP RUN.\n",
    );

    let result = extract("free-format.cob", source);
    assert!(
        find_named(&result, NodeKind::Module, "HELLO").is_some(),
        "free-format COBOL with a trailing newline should finish parsing"
    );
}

#[test]
fn terraform_and_opentofu_extract_resource_blocks() {
    for path in ["main.tf", "main.tofu"] {
        let result = extract(
            path,
            "resource \"aws_s3_bucket\" \"assets\" {\n  bucket = \"example\"\n}\n",
        );
        let resource = find_named(&result, NodeKind::Class, "aws_s3_bucket.assets")
            .expect("Terraform resource should use its type and label as its name");
        assert_eq!(resource.language, Language::Terraform);
        assert_eq!(resource.qualified_name, "aws_s3_bucket.assets");
    }
}
