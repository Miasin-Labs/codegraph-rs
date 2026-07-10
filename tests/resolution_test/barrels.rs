use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn resolves_bare_directory_import_to_index_ts_629() {
    // `import { helper } from '.'` (and './') must map to the directory's
    // index.ts before the rename chase can run.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/util.ts", "export function realHelper(): void {}\n");
    fx.write(
        "src/index.ts",
        "export { realHelper as helper } from './util';\n",
    );
    fx.write(
        "src/main.ts",
        "import { helper } from '.';\nexport function go(): void { helper(); }\n",
    );
    fx.write(
        "src/main2.ts",
        "import { helper } from './';\nexport function go2(): void { helper(); }\n",
    );
    for f in ["src/util.ts", "src/index.ts", "src/main.ts", "src/main2.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let real_helper = exported(node(
        "func:src/util.ts:realHelper:1",
        NodeKind::Function,
        "realHelper",
        "src/util.ts::realHelper",
        "src/util.ts",
        Language::Typescript,
        1,
        1,
    ));
    let go_fn = exported(node(
        "func:src/main.ts:go:2",
        NodeKind::Function,
        "go",
        "src/main.ts::go",
        "src/main.ts",
        Language::Typescript,
        2,
        2,
    ));
    let go2_fn = exported(node(
        "func:src/main2.ts:go2:2",
        NodeKind::Function,
        "go2",
        "src/main2.ts::go2",
        "src/main2.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[real_helper.clone(), go_fn.clone(), go2_fn.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[
        uref(
            &go_fn.id,
            "helper",
            EdgeKind::Calls,
            2,
            "src/main.ts",
            Language::Typescript,
        ),
        uref(
            &go2_fn.id,
            "helper",
            EdgeKind::Calls,
            2,
            "src/main2.ts",
            Language::Typescript,
        ),
    ])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &real_helper.id, EdgeKind::Calls);
    let files = source_files(&q, &callers);
    assert!(files.contains(&"src/main.ts".to_string()));
    assert!(files.contains(&"src/main2.ts".to_string()));
}

#[tokio::test(flavor = "current_thread")]
async fn resolves_workspace_package_subpath_barrel_629() {
    // bun/npm/pnpm workspace: `@scope/ui/widgets` → the `ui` member's
    // widgets/ subdir index, which re-exports a .svelte component under an
    // alias that defeats the name-matcher.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "package.json",
        "{\n  \"name\": \"root\",\n  \"private\": true,\n  \"workspaces\": [\"packages/*\"]\n}",
    );
    fx.write(
        "packages/ui/package.json",
        "{\n  \"name\": \"@scope/ui\",\n  \"version\": \"1.0.0\"\n}",
    );
    fx.write(
        "packages/ui/widgets/Widget.svelte",
        "<script lang=\"ts\">\n  export let label: string = '';\n</script>\n\n<button>{label}</button>\n",
    );
    fx.write(
        "packages/ui/widgets/index.ts",
        "export { default as Thing } from './Widget.svelte';\n",
    );
    fx.write(
        "app/App.svelte",
        "<script lang=\"ts\">\n  import { Thing } from '@scope/ui/widgets';\n</script>\n\n<Thing />\n",
    );
    fx.track(&q, "packages/ui/widgets/Widget.svelte", Language::Svelte);
    fx.track(&q, "packages/ui/widgets/index.ts", Language::Typescript);
    fx.track(&q, "app/App.svelte", Language::Svelte);

    let widget = exported(node(
        "component:packages/ui/widgets/Widget.svelte:Widget:1",
        NodeKind::Component,
        "Widget",
        "packages/ui/widgets/Widget.svelte::Widget",
        "packages/ui/widgets/Widget.svelte",
        Language::Svelte,
        1,
        5,
    ));
    let app = exported(node(
        "component:app/App.svelte:App:1",
        NodeKind::Component,
        "App",
        "app/App.svelte::App",
        "app/App.svelte",
        Language::Svelte,
        1,
        5,
    ));
    q.insert_nodes(&[widget.clone(), app.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &app.id,
        "Thing",
        EdgeKind::References,
        5,
        "app/App.svelte",
        Language::Svelte,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &widget.id, EdgeKind::References);
    assert!(source_files(&q, &callers).contains(&"app/App.svelte".to_string()));
}
