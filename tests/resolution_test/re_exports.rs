use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn chases_a_3_hop_barrel_chain_wildcard_named_declaration() {
    // main.ts → all.ts (wildcard) → index.ts (named) → auth.ts (declaration).
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/services/auth.ts",
        "export function signIn(): void {}\n",
    );
    fx.write(
        "src/services/index.ts",
        "export { signIn } from './auth';\n",
    );
    fx.write("src/all.ts", "export * from './services/index';\n");
    fx.write(
        "src/main.ts",
        "import { signIn } from './all';\nexport function go(): void { signIn(); }\n",
    );
    for f in [
        "src/services/auth.ts",
        "src/services/index.ts",
        "src/all.ts",
        "src/main.ts",
    ] {
        fx.track(&q, f, Language::Typescript);
    }

    let sign_in = exported(node(
        "func:src/services/auth.ts:signIn:1",
        NodeKind::Function,
        "signIn",
        "src/services/auth.ts::signIn",
        "src/services/auth.ts",
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
    q.insert_nodes(&[sign_in.clone(), go_fn.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &go_fn.id,
        "signIn",
        EdgeKind::Calls,
        2,
        "src/main.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &sign_in.id, EdgeKind::Calls);
    assert!(source_files(&q, &callers).contains(&"src/main.ts".to_string()));
}

#[tokio::test(flavor = "current_thread")]
async fn follows_a_renamed_named_re_export() {
    // `export { signIn as login } from './auth'` — the chase looks up
    // `signIn` upstream even though the importer asked for `login`.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/auth.ts", "export function signIn(): void {}\n");
    fx.write(
        "src/index.ts",
        "export { signIn as login } from './auth';\n",
    );
    fx.write(
        "src/main.ts",
        "import { login } from './index';\nexport function go(): void { login(); }\n",
    );
    for f in ["src/auth.ts", "src/index.ts", "src/main.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let sign_in = exported(node(
        "func:src/auth.ts:signIn:1",
        NodeKind::Function,
        "signIn",
        "src/auth.ts::signIn",
        "src/auth.ts",
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
    q.insert_nodes(&[sign_in.clone(), go_fn.clone()]).unwrap();
    // `login` has NO declaration anywhere — only the import-mapping escape in
    // the resolver's pre-filter lets this ref through.
    q.insert_unresolved_refs_batch(&[uref(
        &go_fn.id,
        "login",
        EdgeKind::Calls,
        2,
        "src/main.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let callers = incoming(&q, &sign_in.id, EdgeKind::Calls);
    assert!(source_files(&q, &callers).contains(&"src/main.ts".to_string()));
}
