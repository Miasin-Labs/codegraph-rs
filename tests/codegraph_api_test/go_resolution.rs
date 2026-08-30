// Go resolution end-to-end tests.
//
// Ports of `__tests__/go-package-accessor-chain.test.ts` (PR #1640) and the
// `Go multi-module` block of `__tests__/resolution.test.ts` (PR #1521). Each
// writes real `.go` files into a temp dir and indexes them for real (no
// mocks), then inspects the resolved `calls` edges — exactly like the TS
// suites. Test names are `<describe>_<it>` snake-cased.

mod go_resolution {
    use codegraph::{CodeGraph, EdgeKind, IndexOptions, Node, NodeKind};

    use super::write;

    /// All resolved `calls` edges as `(source_name, target_name, target_qn)`.
    fn calls_of(cg: &CodeGraph) -> Vec<(String, String, String)> {
        let mut out = Vec::new();
        let nodes: Vec<Node> = [
            NodeKind::Function,
            NodeKind::Method,
            NodeKind::Variable,
            NodeKind::Constant,
        ]
        .iter()
        .flat_map(|k| cg.get_nodes_by_kind(*k).unwrap())
        .collect();
        for src in &nodes {
            for edge in cg.get_outgoing_edges(&src.id).unwrap() {
                if edge.kind != EdgeKind::Calls {
                    continue;
                }
                if let Some(tgt) = cg.get_node(&edge.target).unwrap() {
                    out.push((src.name.clone(), tgt.name.clone(), tgt.qualified_name.clone()));
                }
            }
        }
        out
    }

    fn has_call(calls: &[(String, String, String)], src: &str, tgt_qn: &str) -> bool {
        calls.iter().any(|(s, _, qn)| s == src && qn == tgt_qn)
    }

    /// Any resolved call `src` makes to something of the given bare name.
    fn calls_named(calls: &[(String, String, String)], src: &str, tgt: &str) -> bool {
        calls.iter().any(|(s, t, _)| s == src && t == tgt)
    }

    async fn index(dir: &std::path::Path) -> CodeGraph {
        let cg = CodeGraph::init_sync(dir).unwrap();
        cg.index_all(&IndexOptions::default()).await.unwrap();
        cg
    }

    // `svc` exposes an interface accessor; `impl` implements it; `ctrl` decoy.
    const SVC: &str = "package svc\n\ntype IAlpha interface{ Handle() string }\n\nvar localAlpha IAlpha\n\nfunc RegisterAlpha(i IAlpha) { localAlpha = i }\nfunc Alpha() IAlpha          { return localAlpha }\n";
    const IMPL: &str = "package impl\n\ntype SAlpha struct{}\n\nfunc (s *SAlpha) Handle() string { return \"real\" }\n";
    // Same method name on an unrelated type, signature does NOT satisfy IAlpha.
    const DECOY: &str = "package ctrl\n\ntype Req struct{ N int }\n\ntype Ctrl struct{}\n\nfunc (c *Ctrl) Handle(req *Req) string { return \"decoy\" }\n";

    // -----------------------------------------------------------------------
    // PR #1640 — package-qualified accessor chains.
    // -----------------------------------------------------------------------

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_interface_accessor_to_interface_method_not_decoy() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module repro\n\ngo 1.25\n");
        write(&dir.path().join("svc/svc.go"), SVC);
        write(&dir.path().join("impl/impl.go"), IMPL);
        write(&dir.path().join("ctrl/ctrl.go"), DECOY);
        write(
            &dir.path().join("caller/caller.go"),
            "package caller\n\nimport \"repro/svc\"\n\nfunc RunA() string { return svc.Alpha().Handle() }\n",
        );
        let cg = index(dir.path()).await;
        let calls = calls_of(&cg);
        assert!(has_call(&calls, "RunA", "IAlpha::Handle"), "calls={calls:?}");
        assert!(!has_call(&calls, "RunA", "Ctrl::Handle"), "calls={calls:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn reaches_implementation_through_interface() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module repro\n\ngo 1.25\n");
        write(&dir.path().join("svc/svc.go"), SVC);
        write(&dir.path().join("impl/impl.go"), IMPL);
        write(
            &dir.path().join("caller/caller.go"),
            "package caller\n\nimport \"repro/svc\"\n\nfunc RunA() string { return svc.Alpha().Handle() }\n",
        );
        let cg = index(dir.path()).await;
        let calls = calls_of(&cg);
        assert!(has_call(&calls, "RunA", "IAlpha::Handle"), "calls={calls:?}");
        // Dynamic-dispatch bridge: interface method -> implementation.
        assert!(has_call(&calls, "Handle", "SAlpha::Handle"), "calls={calls:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_concrete_accessor_to_returned_type_not_decoy() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module repro\n\ngo 1.25\n");
        write(
            &dir.path().join("impl/impl.go"),
            "package impl\n\ntype SGamma struct{}\n\nfunc (s *SGamma) Execute() string { return \"real\" }\n",
        );
        write(
            &dir.path().join("fac/fac.go"),
            "package fac\n\nimport \"repro/impl\"\n\nfunc Gamma() *impl.SGamma { return &impl.SGamma{} }\n",
        );
        write(
            &dir.path().join("ctrl/ctrl.go"),
            "package ctrl\n\ntype Req struct{ N int }\n\ntype Ctrl struct{}\n\nfunc (c *Ctrl) Execute(req *Req) string { return \"decoy\" }\n",
        );
        write(
            &dir.path().join("caller/caller.go"),
            "package caller\n\nimport \"repro/fac\"\n\nfunc RunC() string { return fac.Gamma().Execute() }\n",
        );
        let cg = index(dir.path()).await;
        let calls = calls_of(&cg);
        assert!(has_call(&calls, "RunC", "SGamma::Execute"), "calls={calls:?}");
        assert!(!has_call(&calls, "RunC", "Ctrl::Execute"), "calls={calls:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn honours_import_alias_as_package_qualifier() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module repro\n\ngo 1.25\n");
        write(&dir.path().join("svc/svc.go"), SVC);
        write(&dir.path().join("impl/impl.go"), IMPL);
        write(&dir.path().join("ctrl/ctrl.go"), DECOY);
        write(
            &dir.path().join("caller/caller.go"),
            "package caller\n\nimport svcx \"repro/svc\"\n\nfunc RunAlias() string { return svcx.Alpha().Handle() }\n",
        );
        let cg = index(dir.path()).await;
        let calls = calls_of(&cg);
        assert!(has_call(&calls, "RunAlias", "IAlpha::Handle"), "calls={calls:?}");
        assert!(!has_call(&calls, "RunAlias", "Ctrl::Handle"), "calls={calls:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn leaves_instance_chain_on_bare_name_path() {
        // `b.Inner().Value()` shares the `selector_expression` inner-callee
        // shape with a package-qualified accessor. Re-encoding it would strip
        // the edge (a variable's type is not recoverable here), so it must stay
        // bare and resolve by name to `Box::Value`.
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module repro\n\ngo 1.25\n");
        write(
            &dir.path().join("box/box.go"),
            "package box\n\ntype Box struct{}\n\nfunc (b *Box) Inner() *Box   { return b }\nfunc (b *Box) Value() string { return \"v\" }\n\nfunc UseInstance() string {\n\tvar b Box\n\treturn b.Inner().Value()\n}\n",
        );
        let cg = index(dir.path()).await;
        let calls = calls_of(&cg);
        assert!(has_call(&calls, "UseInstance", "Box::Value"), "calls={calls:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn picks_aliased_package_the_import_path_names() {
        // Two packages export `Build()` returning different interfaces. The call
        // site says `nope.Build()`, a name no directory carries — only the
        // import path maps it back to `one`.
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module repro\n\ngo 1.25\n");
        write(
            &dir.path().join("one/one.go"),
            "package one\n\ntype IOne interface{ Run() string }\n\nfunc Build() IOne { var x IOne; return x }\n",
        );
        write(
            &dir.path().join("two/two.go"),
            "package two\n\ntype ITwo interface{ Run() string }\n\nfunc Build() ITwo { var x ITwo; return x }\n",
        );
        write(
            &dir.path().join("caller/caller.go"),
            "package caller\n\nimport nope \"repro/one\"\n\nfunc RunAliased() string { return nope.Build().Run() }\n",
        );
        let cg = index(dir.path()).await;
        let calls = calls_of(&cg);
        assert!(has_call(&calls, "RunAliased", "IOne::Run"), "calls={calls:?}");
        assert!(!has_call(&calls, "RunAliased", "ITwo::Run"), "calls={calls:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn makes_no_edge_when_inferred_type_lacks_method() {
        // Absent-method safety: a same-named decoy must not be matched instead.
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module repro\n\ngo 1.25\n");
        write(&dir.path().join("svc/svc.go"), SVC);
        write(&dir.path().join("impl/impl.go"), IMPL);
        write(
            &dir.path().join("ctrl/ctrl.go"),
            "package ctrl\n\ntype Ctrl struct{}\n\nfunc (c *Ctrl) Missing() string { return \"decoy\" }\n",
        );
        write(
            &dir.path().join("caller/caller.go"),
            "package caller\n\nimport \"repro/svc\"\n\nfunc RunMissing() string { return svc.Alpha().Missing() }\n",
        );
        let cg = index(dir.path()).await;
        let calls = calls_of(&cg);
        assert!(!calls_named(&calls, "RunMissing", "Missing"), "calls={calls:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn makes_no_edge_when_factory_package_is_outside_index() {
        // `g.Redis().Do(...)` — the accessor and its type live in a dependency,
        // so nothing about the receiver is knowable. The bare `Do` must not
        // match a project function of that name.
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module repro\n\ngo 1.25\n");
        write(
            &dir.path().join("carrier/carrier.go"),
            "package carrier\n\nfunc Do() string { return \"unrelated project function\" }\n",
        );
        write(
            &dir.path().join("caller/caller.go"),
            "package caller\n\nimport \"github.com/gogf/gf/v2/frame/g\"\n\nfunc RunExternal() string {\n\tv, _ := g.Redis().Do(nil, \"GET\", \"k\")\n\treturn v.String()\n}\n",
        );
        let cg = index(dir.path()).await;
        let calls = calls_of(&cg);
        assert!(!calls_named(&calls, "RunExternal", "Do"), "calls={calls:?}");
        cg.close();
    }

    // -----------------------------------------------------------------------
    // PR #1521 — multi-module (side-by-side modules) resolution.
    // -----------------------------------------------------------------------

    /// Project-relative file path of every call target `src` reaches.
    fn call_target_files(cg: &CodeGraph, src_name: &str) -> Vec<String> {
        let mut files = Vec::new();
        for func in cg.get_nodes_by_kind(NodeKind::Function).unwrap() {
            if func.name != src_name {
                continue;
            }
            for edge in cg.get_outgoing_edges(&func.id).unwrap() {
                if edge.kind != EdgeKind::Calls {
                    continue;
                }
                if let Some(tgt) = cg.get_node(&edge.target).unwrap() {
                    files.push(tgt.file_path.replace('\\', "/"));
                }
            }
        }
        files
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_cross_module_call_between_side_by_side_modules() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/go.mod"), "module example.com/a\n\ngo 1.21\n");
        write(&dir.path().join("a/pkg/shared.go"), "package pkg\n\nfunc SharedFn() int { return 1 }\n");
        write(&dir.path().join("b/go.mod"), "module example.com/b\n\ngo 1.21\n");
        // Decoy with the SAME name in b's own sub-package.
        write(&dir.path().join("b/pkg/other.go"), "package pkg\n\nfunc SharedFn() int { return 2 }\n");
        write(
            &dir.path().join("b/use.go"),
            "package main\n\nimport apkg \"example.com/a/pkg\"\n\nfunc UseShared() {\n  apkg.SharedFn()\n}\n",
        );
        let cg = index(dir.path()).await;
        let targets = call_target_files(&cg, "UseShared");
        assert!(targets.contains(&"a/pkg/shared.go".to_string()), "targets={targets:?}");
        assert!(!targets.contains(&"b/pkg/other.go".to_string()), "targets={targets:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn does_not_cross_wire_same_named_symbol_to_non_imported_module() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/go.mod"), "module example.com/a\n\ngo 1.21\n");
        write(&dir.path().join("a/pkg/init.go"), "package pkg\n\nfunc Init() int { return 1 }\n");
        write(&dir.path().join("c/go.mod"), "module example.com/c\n\ngo 1.21\n");
        write(&dir.path().join("c/pkg/init.go"), "package pkg\n\nfunc Init() int { return 2 }\n");
        write(&dir.path().join("b/go.mod"), "module example.com/b\n\ngo 1.21\n");
        write(
            &dir.path().join("b/use.go"),
            "package main\n\nimport apkg \"example.com/a/pkg\"\n\nfunc UseInit() {\n  apkg.Init()\n}\n",
        );
        let cg = index(dir.path()).await;
        let targets = call_target_files(&cg, "UseInit");
        assert!(targets.contains(&"a/pkg/init.go".to_string()), "targets={targets:?}");
        assert!(!targets.contains(&"c/pkg/init.go".to_string()), "targets={targets:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn resolves_longest_matching_module_path_prefix_first() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("lib/go.mod"), "module example.com/x/commons\n\ngo 1.21\n");
        write(&dir.path().join("sdk/go.mod"), "module example.com/x/commons/sdk\n\ngo 1.21\n");
        write(&dir.path().join("sdk/pkg/thing.go"), "package pkg\n\nfunc SdkThing() int { return 1 }\n");
        write(&dir.path().join("app/go.mod"), "module example.com/x/app\n\ngo 1.21\n");
        write(
            &dir.path().join("app/use.go"),
            "package main\n\nimport sdkpkg \"example.com/x/commons/sdk/pkg\"\n\nfunc UseSdk() {\n  sdkpkg.SdkThing()\n}\n",
        );
        let cg = index(dir.path()).await;
        let targets = call_target_files(&cg, "UseSdk");
        assert!(targets.contains(&"sdk/pkg/thing.go".to_string()), "targets={targets:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn single_root_module_with_no_nested_modules_still_resolves() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module example.com/app\n\ngo 1.21\n");
        write(&dir.path().join("pkga/helper.go"), "package pkga\n\nfunc Helper() int { return 1 }\n");
        write(
            &dir.path().join("main.go"),
            "package main\n\nimport \"example.com/app/pkga\"\n\nfunc UseHelper() {\n  pkga.Helper()\n}\n",
        );
        let cg = index(dir.path()).await;
        let targets = call_target_files(&cg, "UseHelper");
        assert!(targets.contains(&"pkga/helper.go".to_string()), "targets={targets:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn does_not_match_symbol_in_subpackage_of_imported_package() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("go.mod"), "module example.com/app\n\ngo 1.21\n");
        write(&dir.path().join("pkga/top.go"), "package pkga\n\nfunc FuncX() int { return 1 }\n");
        write(&dir.path().join("pkga/subpkg/sub.go"), "package subpkg\n\nfunc FuncX() int { return 2 }\n");
        write(
            &dir.path().join("main.go"),
            "package main\n\nimport \"example.com/app/pkga\"\n\nfunc UseFuncX() {\n  pkga.FuncX()\n}\n",
        );
        let cg = index(dir.path()).await;
        let targets = call_target_files(&cg, "UseFuncX");
        assert!(targets.contains(&"pkga/top.go".to_string()), "targets={targets:?}");
        assert!(!targets.contains(&"pkga/subpkg/sub.go".to_string()), "targets={targets:?}");
        cg.close();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn builds_calls_edge_from_grouped_var_initializer_cross_module() {
        // Defect B consequence: a grouped-var initializer now has a source node,
        // so the call inside it (`apkg.NewLogger`) gets an edge to module a.
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a/go.mod"), "module example.com/a\n\ngo 1.21\n");
        write(
            &dir.path().join("a/pkg/logger.go"),
            "package pkg\n\ntype Logger struct{}\n\nfunc NewLogger(name string) *Logger { return nil }\n",
        );
        write(&dir.path().join("b/go.mod"), "module example.com/b\n\ngo 1.21\n");
        write(
            &dir.path().join("b/use.go"),
            "package main\n\nimport apkg \"example.com/a/pkg\"\n\nvar (\n  logger = apkg.NewLogger(\"x\")\n)\n",
        );
        let cg = index(dir.path()).await;
        let logger = cg
            .get_nodes_by_kind(NodeKind::Variable)
            .unwrap()
            .into_iter()
            .find(|n| n.name == "logger" && n.file_path.replace('\\', "/") == "b/use.go")
            .expect("logger var should be indexed (defect B)");
        let targets: Vec<String> = cg
            .get_outgoing_edges(&logger.id)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EdgeKind::Calls)
            .filter_map(|e| cg.get_node(&e.target).unwrap())
            .map(|n| n.file_path.replace('\\', "/"))
            .collect();
        assert!(targets.contains(&"a/pkg/logger.go".to_string()), "targets={targets:?}");
        cg.close();
    }
}
