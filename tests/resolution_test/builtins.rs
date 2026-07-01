use crate::fixture::*;

#[test]
fn go_leaves_stdlib_calls_external() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("go.mod", "module github.com/example/myproject\n\ngo 1.21\n");
    fx.write(
        "main.go",
        "package main\n\nimport \"fmt\"\n\nfunc main() {\n  fmt.Println(\"hi\")\n}\n",
    );
    fx.track(&q, "main.go", Language::Go);

    let main_fn = node(
        "func:main.go:main:5",
        NodeKind::Function,
        "main",
        "main::main",
        "main.go",
        Language::Go,
        5,
        7,
    );
    q.insert_nodes(std::slice::from_ref(&main_fn)).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &main_fn.id,
        "fmt.Println",
        EdgeKind::Calls,
        6,
        "main.go",
        Language::Go,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    // No spurious in-project edge — fmt.* must stay unresolved/external.
    let calls = outgoing(&q, &main_fn.id, EdgeKind::Calls);
    assert!(calls.is_empty());
}

// =============================================================================
// tsconfig path aliases (resolution.test.ts)
// =============================================================================

#[test]
fn resolve_one_skips_builtins_best_candidate_api() {
    // "Best-Candidate Resolution" — TS only asserted resolveOne exists on
    // the prototype; in Rust that's a compile-time fact, so exercise the
    // built-in short-circuit through the public API instead.
    let fx = Fx::new();
    let resolver = fx.resolver();
    let r = UnresolvedRef {
        from_node_id: "caller".to_string(),
        reference_name: "console".to_string(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        column: 0,
        file_path: "src/a.ts".to_string(),
        language: Language::Typescript,
        candidates: None,
        metadata: None,
    };
    assert!(resolver.resolve_one(&r).is_none());
}
