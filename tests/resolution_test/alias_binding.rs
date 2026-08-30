use crate::fixture::*;

// Regression tests for upstream #1482 (PR #1485): a call through a value alias
// (`export const alias = fn`, `export { fn as alias }`, `const local = fn`,
// `export const api = { run: fn }`) must reach the aliased function so
// callers/impact of the implementation include every consumer calling through
// the alias. These build extraction-shaped nodes (same signature strings the
// TS/JS extractor stores for a `const x = <init>` binding) over REAL files and
// REAL SQLite, then run the production resolver end-to-end.

/// A `const`/`variable` alias binding node, with its initializer stored in
/// `signature` exactly as the extractor records it (`init_signature`).
fn binding(
    id: &str,
    kind: NodeKind,
    name: &str,
    file_path: &str,
    line: u32,
    signature: &str,
    is_exported: bool,
) -> Node {
    let mut n = node(
        id,
        kind,
        name,
        &format!("{file_path}::{name}"),
        file_path,
        Language::Typescript,
        line,
        line,
    );
    n.signature = Some(signature.to_string());
    n.is_exported = Some(is_exported);
    n
}

fn func(id: &str, name: &str, file_path: &str, line: u32, is_exported: bool) -> Node {
    let mut n = node(
        id,
        NodeKind::Function,
        name,
        &format!("{file_path}::{name}"),
        file_path,
        Language::Typescript,
        line,
        line,
    );
    n.is_exported = Some(is_exported);
    n
}

#[tokio::test(flavor = "current_thread")]
async fn follows_export_const_alias_across_a_file() {
    // impl.ts: `export function realImpl` + `export const aliasName = realImpl`
    // consumer.ts: `import { aliasName } ...; consumerFn() { return aliasName(); }`
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "impl.ts",
        "export function realImpl(): number { return 1; }\nexport const aliasName = realImpl;\n",
    );
    fx.write(
        "consumer.ts",
        "import { aliasName } from './impl';\nexport function consumerFn(): number { return aliasName(); }\n",
    );
    for f in ["impl.ts", "consumer.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let real_impl = func("func:impl.ts:realImpl:1", "realImpl", "impl.ts", 1, true);
    let alias = binding(
        "const:impl.ts:aliasName:2",
        NodeKind::Constant,
        "aliasName",
        "impl.ts",
        2,
        "= realImpl",
        true,
    );
    let consumer = func(
        "func:consumer.ts:consumerFn:2",
        "consumerFn",
        "consumer.ts",
        2,
        true,
    );
    q.insert_nodes(&[real_impl.clone(), alias.clone(), consumer.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &consumer.id,
        "aliasName",
        EdgeKind::Calls,
        2,
        "consumer.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &real_impl.id, EdgeKind::Calls);
    assert!(
        source_files(&q, &callers).contains(&"consumer.ts".to_string()),
        "consumerFn should be a caller of realImpl through the alias"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn follows_local_export_rename_clause() {
    // `export { realImpl as aliasName }` - the declaration carries no `export`
    // keyword, so the export index must still bind the renamed export to it.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "impl.ts",
        "function realImpl(): number { return 1; }\nexport { realImpl as aliasName };\n",
    );
    fx.write(
        "consumer.ts",
        "import { aliasName } from './impl';\nexport function consumerFn(): number { return aliasName(); }\n",
    );
    for f in ["impl.ts", "consumer.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    // realImpl is NOT exported (bare declaration).
    let real_impl = func("func:impl.ts:realImpl:1", "realImpl", "impl.ts", 1, false);
    let consumer = func(
        "func:consumer.ts:consumerFn:2",
        "consumerFn",
        "consumer.ts",
        2,
        true,
    );
    q.insert_nodes(&[real_impl.clone(), consumer.clone()])
        .unwrap();
    // `aliasName` has NO declaration anywhere - only the import-mapping escape
    // in the resolver's pre-filter lets this ref through.
    q.insert_unresolved_refs_batch(&[uref(
        &consumer.id,
        "aliasName",
        EdgeKind::Calls,
        2,
        "consumer.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &real_impl.id, EdgeKind::Calls);
    assert!(
        source_files(&q, &callers).contains(&"consumer.ts".to_string()),
        "consumerFn should reach realImpl through the local export clause"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn follows_function_ref_in_object_literal_property() {
    // impl.ts: `export const api = { run: realImpl }`; consumer calls `api.run()`.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "impl.ts",
        "export function realImpl(): number { return 1; }\nexport const api = { run: realImpl };\n",
    );
    fx.write(
        "consumer.ts",
        "import { api } from './impl';\nexport function consumerFn(): number { return api.run(); }\n",
    );
    for f in ["impl.ts", "consumer.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let real_impl = func("func:impl.ts:realImpl:1", "realImpl", "impl.ts", 1, true);
    let api = binding(
        "const:impl.ts:api:2",
        NodeKind::Constant,
        "api",
        "impl.ts",
        2,
        "= { run: realImpl }",
        true,
    );
    let consumer = func(
        "func:consumer.ts:consumerFn:2",
        "consumerFn",
        "consumer.ts",
        2,
        true,
    );
    q.insert_nodes(&[real_impl.clone(), api.clone(), consumer.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &consumer.id,
        "api.run",
        EdgeKind::Calls,
        2,
        "consumer.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &real_impl.id, EdgeKind::Calls);
    assert!(
        source_files(&q, &callers).contains(&"consumer.ts".to_string()),
        "consumerFn should reach realImpl through the object-literal property"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn follows_same_file_alias_binding() {
    // impl.ts: `function realImpl`, `const localAlias = realImpl`, and
    // `consumerFn() { return localAlias(); }` - all in one file.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "impl.ts",
        "function realImpl(): number { return 1; }\nconst localAlias = realImpl;\nexport function consumerFn(): number { return localAlias(); }\n",
    );
    fx.track(&q, "impl.ts", Language::Typescript);

    let real_impl = func("func:impl.ts:realImpl:1", "realImpl", "impl.ts", 1, false);
    let local_alias = binding(
        "const:impl.ts:localAlias:2",
        NodeKind::Constant,
        "localAlias",
        "impl.ts",
        2,
        "= realImpl",
        false,
    );
    let consumer = func(
        "func:impl.ts:consumerFn:3",
        "consumerFn",
        "impl.ts",
        3,
        true,
    );
    q.insert_nodes(&[real_impl.clone(), local_alias.clone(), consumer.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &consumer.id,
        "localAlias",
        EdgeKind::Calls,
        3,
        "impl.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &real_impl.id, EdgeKind::Calls);
    assert!(
        source_files(&q, &callers).contains(&"impl.ts".to_string()),
        "consumerFn should reach realImpl through the same-file alias"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn genuine_wrapper_keeps_pointing_at_the_wrapper() {
    // `export const wrapper = (): number => realImpl();` is a real function (the
    // extractor stores it as a `function` node, not a `constant`): the call site
    // calls IT, so no hop to realImpl.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "impl.ts",
        "export function realImpl(): number { return 1; }\nexport const wrapper = (): number => realImpl();\n",
    );
    fx.write(
        "consumer.ts",
        "import { wrapper } from './impl';\nexport function consumerFn(): number { return wrapper(); }\n",
    );
    for f in ["impl.ts", "consumer.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let real_impl = func("func:impl.ts:realImpl:1", "realImpl", "impl.ts", 1, true);
    // wrapper is a Function node (arrow function), NOT a constant alias.
    let wrapper = func("func:impl.ts:wrapper:2", "wrapper", "impl.ts", 2, true);
    let consumer = func(
        "func:consumer.ts:consumerFn:2",
        "consumerFn",
        "consumer.ts",
        2,
        true,
    );
    q.insert_nodes(&[real_impl.clone(), wrapper.clone(), consumer.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &consumer.id,
        "wrapper",
        EdgeKind::Calls,
        2,
        "consumer.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let real_callers = incoming(&q, &real_impl.id, EdgeKind::Calls);
    assert!(
        !source_files(&q, &real_callers).contains(&"consumer.ts".to_string()),
        "consumerFn calls the wrapper, not realImpl"
    );
    let wrapper_callers = incoming(&q, &wrapper.id, EdgeKind::Calls);
    assert!(
        source_files(&q, &wrapper_callers).contains(&"consumer.ts".to_string()),
        "consumerFn should be a caller of the wrapper"
    );
}

#[tokio::test(flavor = "current_thread")]
async fn no_hop_when_the_aliased_name_is_ambiguous_across_files() {
    // Two same-named callables and no same-file declaration to prefer: a hop
    // would have to guess, and a wrong edge is worse than a missing one.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("one.ts", "export function shared(): number { return 1; }\n");
    fx.write("two.ts", "export function shared(): number { return 2; }\n");
    fx.write(
        "alias.ts",
        "import { shared } from './one';\nexport const aliasName = shared;\n",
    );
    fx.write(
        "consumer.ts",
        "import { aliasName } from './alias';\nexport function consumerFn(): number { return aliasName(); }\n",
    );
    for f in ["one.ts", "two.ts", "alias.ts", "consumer.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let shared_one = func("func:one.ts:shared:1", "shared", "one.ts", 1, true);
    let shared_two = func("func:two.ts:shared:1", "shared", "two.ts", 1, true);
    let alias = binding(
        "const:alias.ts:aliasName:2",
        NodeKind::Constant,
        "aliasName",
        "alias.ts",
        2,
        "= shared",
        true,
    );
    let consumer = func(
        "func:consumer.ts:consumerFn:2",
        "consumerFn",
        "consumer.ts",
        2,
        true,
    );
    q.insert_nodes(&[
        shared_one.clone(),
        shared_two.clone(),
        alias.clone(),
        consumer.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &consumer.id,
        "aliasName",
        EdgeKind::Calls,
        2,
        "consumer.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    for shared in [&shared_one, &shared_two] {
        let callers = incoming(&q, &shared.id, EdgeKind::Calls);
        assert!(
            !source_files(&q, &callers).contains(&"consumer.ts".to_string()),
            "ambiguous alias must not invent a hop to {}",
            shared.file_path
        );
    }
}
