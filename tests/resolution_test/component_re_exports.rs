use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn follows_default_re_export_of_a_svelte_component_629() {
    // `export { default as Foo } from './RealButton.svelte'` — alias differs
    // from the component's real name so only the import-chase (with the
    // component-node preference for default exports) can connect the edge.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/lib/RealButton.svelte",
        "<script lang=\"ts\">\n  export let label: string = '';\n</script>\n\n<button>{label}</button>\n",
    );
    fx.write(
        "src/lib/index.ts",
        "export { default as Foo } from './RealButton.svelte';\n",
    );
    fx.write(
        "src/Bar.svelte",
        "<script lang=\"ts\">\n  import { Foo } from './lib';\n</script>\n\n<Foo />\n",
    );
    fx.track(&q, "src/lib/RealButton.svelte", Language::Svelte);
    fx.track(&q, "src/lib/index.ts", Language::Typescript);
    fx.track(&q, "src/Bar.svelte", Language::Svelte);

    let real_button = exported(node(
        "component:src/lib/RealButton.svelte:RealButton:1",
        NodeKind::Component,
        "RealButton",
        "src/lib/RealButton.svelte::RealButton",
        "src/lib/RealButton.svelte",
        Language::Svelte,
        1,
        5,
    ));
    let bar = exported(node(
        "component:src/Bar.svelte:Bar:1",
        NodeKind::Component,
        "Bar",
        "src/Bar.svelte::Bar",
        "src/Bar.svelte",
        Language::Svelte,
        1,
        5,
    ));
    q.insert_nodes(&[real_button.clone(), bar.clone()]).unwrap();
    // The svelte extractor emits `<Foo />` markup usage as a `references` ref
    // from the consumer component node.
    q.insert_unresolved_refs_batch(&[uref(
        &bar.id,
        "Foo",
        EdgeKind::References,
        5,
        "src/Bar.svelte",
        Language::Svelte,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &real_button.id, EdgeKind::References);
    assert!(source_files(&q, &callers).contains(&"src/Bar.svelte".to_string()));
}

#[tokio::test(flavor = "current_thread")]
async fn resolves_barrel_import_from_vue_sfc_script_block_629() {
    // The barrel renames `realRun` → `run` so only the import-chase (not the
    // name-matcher) can connect the call from the .vue consumer.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/util.ts", "export function realRun(): void {}\n");
    fx.write("src/index.ts", "export { realRun as run } from './util';\n");
    fx.write(
        "src/App.vue",
        "<script lang=\"ts\">\nimport { run } from './';\nexport default { mounted() { run(); } };\n</script>\n<template><div/></template>\n",
    );
    fx.track(&q, "src/util.ts", Language::Typescript);
    fx.track(&q, "src/index.ts", Language::Typescript);
    fx.track(&q, "src/App.vue", Language::Vue);

    let real_run = exported(node(
        "func:src/util.ts:realRun:1",
        NodeKind::Function,
        "realRun",
        "src/util.ts::realRun",
        "src/util.ts",
        Language::Typescript,
        1,
        1,
    ));
    let app = exported(node(
        "component:src/App.vue:App:1",
        NodeKind::Component,
        "App",
        "src/App.vue::App",
        "src/App.vue",
        Language::Vue,
        1,
        5,
    ));
    q.insert_nodes(&[real_run.clone(), app.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &app.id,
        "run",
        EdgeKind::Calls,
        3,
        "src/App.vue",
        Language::Vue,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &real_run.id, EdgeKind::Calls);
    assert!(source_files(&q, &callers).contains(&"src/App.vue".to_string()));
}

#[tokio::test(flavor = "current_thread")]
async fn follows_vue_component_in_template_through_default_re_export_barrel_629() {
    // Vue analogue of the Svelte case: leaf is a `.vue` component
    // re-exported under an alias; the consumer uses it ONLY in markup.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/lib/Widget.vue",
        "<script setup lang=\"ts\">\ndefineProps<{ label?: string }>();\n</script>\n<template><button>x</button></template>\n",
    );
    fx.write(
        "src/lib/index.ts",
        "export { default as Thing } from './Widget.vue';\n",
    );
    fx.write(
        "src/App.vue",
        "<script setup lang=\"ts\">\nimport { Thing } from './lib';\n</script>\n<template>\n  <Thing />\n</template>\n",
    );
    fx.track(&q, "src/lib/Widget.vue", Language::Vue);
    fx.track(&q, "src/lib/index.ts", Language::Typescript);
    fx.track(&q, "src/App.vue", Language::Vue);

    let widget = exported(node(
        "component:src/lib/Widget.vue:Widget:1",
        NodeKind::Component,
        "Widget",
        "src/lib/Widget.vue::Widget",
        "src/lib/Widget.vue",
        Language::Vue,
        1,
        4,
    ));
    let app = exported(node(
        "component:src/App.vue:App:1",
        NodeKind::Component,
        "App",
        "src/App.vue::App",
        "src/App.vue",
        Language::Vue,
        1,
        6,
    ));
    q.insert_nodes(&[widget.clone(), app.clone()]).unwrap();
    // Template-tag extraction emits `<Thing />` as a `references` ref.
    q.insert_unresolved_refs_batch(&[uref(
        &app.id,
        "Thing",
        EdgeKind::References,
        5,
        "src/App.vue",
        Language::Vue,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &widget.id, EdgeKind::References);
    assert!(source_files(&q, &callers).contains(&"src/App.vue".to_string()));
}

// =============================================================================
// C/C++ include resolution, end-to-end (resolution.test.ts)
// =============================================================================
