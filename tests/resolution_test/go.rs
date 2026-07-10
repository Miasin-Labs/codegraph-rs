use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn resolves_go_cross_package_qualified_calls_via_go_mod_388() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("go.mod", "module github.com/example/myproject\n\ngo 1.21\n");
    fx.write(
        "pkga/conv.go",
        "package pkga\nfunc Convert(x int) int { return x * 2 }\n",
    );
    fx.write(
        "pkgb/conv.go",
        "package pkgb\nfunc Convert(x int) int { return x + 1 }\n",
    );
    fx.write(
        "pkgc/use.go",
        "package pkgc\n\nimport \"github.com/example/myproject/pkga\"\n\nfunc UsePkga() {\n  pkga.Convert(5)\n}\n",
    );
    for f in ["pkga/conv.go", "pkgb/conv.go", "pkgc/use.go"] {
        fx.track(&q, f, Language::Go);
    }

    // Same-name exported function in two packages — only the imported one
    // should resolve.
    let convert_a = exported(node(
        "func:pkga/conv.go:Convert:2",
        NodeKind::Function,
        "Convert",
        "pkga::Convert",
        "pkga/conv.go",
        Language::Go,
        2,
        2,
    ));
    let convert_b = exported(node(
        "func:pkgb/conv.go:Convert:2",
        NodeKind::Function,
        "Convert",
        "pkgb::Convert",
        "pkgb/conv.go",
        Language::Go,
        2,
        2,
    ));
    let use_pkga = exported(node(
        "func:pkgc/use.go:UsePkga:5",
        NodeKind::Function,
        "UsePkga",
        "pkgc::UsePkga",
        "pkgc/use.go",
        Language::Go,
        5,
        7,
    ));
    q.insert_nodes(&[convert_a.clone(), convert_b, use_pkga.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &use_pkga.id,
        "pkga.Convert",
        EdgeKind::Calls,
        6,
        "pkgc/use.go",
        Language::Go,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let call_edges = outgoing(&q, &use_pkga.id, EdgeKind::Calls);
    assert_eq!(call_edges.len(), 1);
    let target = q.get_node_by_id(&call_edges[0].target).unwrap().unwrap();
    assert_eq!(target.name, "Convert");
    // Critical: the resolver must pick the imported pkga's Convert, not pkgb's.
    assert_eq!(target.file_path.replace('\\', "/"), "pkga/conv.go");
}

#[tokio::test(flavor = "current_thread")]
async fn resolves_go_aliased_imports_across_packages_388() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("go.mod", "module github.com/example/myproject\n\ngo 1.21\n");
    fx.write(
        "pkgb/lib.go",
        "package pkgb\nfunc Compute(x int) int { return x }\n",
    );
    fx.write(
        "pkgd/use.go",
        "package pkgd\n\nimport (\n  \"fmt\"\n  alias \"github.com/example/myproject/pkgb\"\n)\n\nfunc UseAliased() {\n  fmt.Println(\"hi\")\n  alias.Compute(3)\n}\n",
    );
    fx.track(&q, "pkgb/lib.go", Language::Go);
    fx.track(&q, "pkgd/use.go", Language::Go);

    let compute = exported(node(
        "func:pkgb/lib.go:Compute:2",
        NodeKind::Function,
        "Compute",
        "pkgb::Compute",
        "pkgb/lib.go",
        Language::Go,
        2,
        2,
    ));
    let use_aliased = exported(node(
        "func:pkgd/use.go:UseAliased:8",
        NodeKind::Function,
        "UseAliased",
        "pkgd::UseAliased",
        "pkgd/use.go",
        Language::Go,
        8,
        11,
    ));
    q.insert_nodes(&[compute.clone(), use_aliased.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[
        uref(
            &use_aliased.id,
            "fmt.Println",
            EdgeKind::Calls,
            9,
            "pkgd/use.go",
            Language::Go,
        ),
        uref(
            &use_aliased.id,
            "alias.Compute",
            EdgeKind::Calls,
            10,
            "pkgd/use.go",
            Language::Go,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    // fmt.Println is stdlib — must stay external. alias.Compute must resolve.
    let calls = outgoing(&q, &use_aliased.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1);
    let target = q.get_node_by_id(&calls[0].target).unwrap().unwrap();
    assert_eq!(target.name, "Compute");
    assert_eq!(target.file_path.replace('\\', "/"), "pkgb/lib.go");
}

#[tokio::test(flavor = "current_thread")]
async fn resolves_go_cross_package_calls_from_nested_module_root() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "go/go.mod",
        "module github.com/dolthub/dolt/go\n\ngo 1.24\n",
    );
    fx.write(
        "go/libraries/doltcore/diff/calc.go",
        "package diff\nfunc Calculate(x int) int { return x * 2 }\n",
    );
    fx.write(
        "go/cmd/dolt/diff/calc.go",
        "package diff\nfunc Calculate(x int) int { return x + 1 }\n",
    );
    fx.write(
        "go/cmd/dolt/main.go",
        "package main\n\nimport \"github.com/dolthub/dolt/go/libraries/doltcore/diff\"\n\nfunc main() {\n  diff.Calculate(5)\n}\n",
    );
    for f in [
        "go/libraries/doltcore/diff/calc.go",
        "go/cmd/dolt/diff/calc.go",
        "go/cmd/dolt/main.go",
    ] {
        fx.track(&q, f, Language::Go);
    }

    let imported_calculate = exported(node(
        "func:go/libraries/doltcore/diff/calc.go:Calculate:2",
        NodeKind::Function,
        "Calculate",
        "diff::Calculate",
        "go/libraries/doltcore/diff/calc.go",
        Language::Go,
        2,
        2,
    ));
    let nearby_distractor = exported(node(
        "func:go/cmd/dolt/diff/calc.go:Calculate:2",
        NodeKind::Function,
        "Calculate",
        "diff::Calculate",
        "go/cmd/dolt/diff/calc.go",
        Language::Go,
        2,
        2,
    ));
    let main_fn = exported(node(
        "func:go/cmd/dolt/main.go:main:5",
        NodeKind::Function,
        "main",
        "main::main",
        "go/cmd/dolt/main.go",
        Language::Go,
        5,
        7,
    ));
    q.insert_nodes(&[
        imported_calculate.clone(),
        nearby_distractor,
        main_fn.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &main_fn.id,
        "diff.Calculate",
        EdgeKind::Calls,
        6,
        "go/cmd/dolt/main.go",
        Language::Go,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let call_edges = outgoing(&q, &main_fn.id, EdgeKind::Calls);
    assert_eq!(call_edges.len(), 1);
    let target = q.get_node_by_id(&call_edges[0].target).unwrap().unwrap();
    assert_eq!(
        target.file_path.replace('\\', "/"),
        "go/libraries/doltcore/diff/calc.go"
    );
}
