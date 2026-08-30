use crate::extraction_test::fixture::*;

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
// Unsupported-language projects report what they skipped (issue #1502)
// =============================================================================

#[tokio::test(flavor = "current_thread")]
async fn regression_index_all_reports_unsupported_extensions_issue_1502() {
    // A project written entirely in a language CodeGraph has no grammar for used
    // to be indistinguishable from an empty one: 0 files, no signal. index_all
    // now tallies the skipped files by extension so the CLI can say so (#1502).
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("a.pl"), "print 1;\n").unwrap();
    fs::write(root.join("b.pl"), "print 2;\n").unwrap();
    fs::write(root.join("c.foobar"), "x\n").unwrap();

    let (_connection, queries) = open_graph(root);
    let orchestrator = ExtractionOrchestrator::new(root, &queries);
    let result = orchestrator.index_all(None, None, false).await.unwrap();

    assert_eq!(result.files_indexed, 0);
    assert_eq!(result.files_skipped_unsupported, Some(3));
    let top = result
        .top_unsupported_extensions
        .expect("top extensions should be reported");
    // Biggest first: .pl (2) before .foobar (1).
    assert_eq!(top[0].ext, ".pl");
    assert_eq!(top[0].count, 2);
    assert!(top.iter().any(|e| e.ext == ".foobar" && e.count == 1));
}

#[tokio::test(flavor = "current_thread")]
async fn regression_index_all_omits_unsupported_report_on_a_healthy_project_issue_1502() {
    // The counter must never fire when every file was indexable (#1502).
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    fs::write(root.join("a.rs"), "fn a() {}\n").unwrap();

    let (_connection, queries) = open_graph(root);
    let orchestrator = ExtractionOrchestrator::new(root, &queries);
    let result = orchestrator.index_all(None, None, false).await.unwrap();

    assert!(result.files_indexed >= 1);
    assert_eq!(result.files_skipped_unsupported, None);
    assert!(result.top_unsupported_extensions.is_none());
}
