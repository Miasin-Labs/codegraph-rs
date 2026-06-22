use crate::fixture::*;

#[test]
fn resolves_aliased_import_to_alias_mapped_file() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/utils/format.ts",
        "export function pickMe(): number { return 1; }\n",
    );
    fx.write(
        "src/legacy/format.ts",
        "export function pickMe(): number { return 99; }\n",
    );
    fx.write(
        "src/main.ts",
        "import { pickMe } from '@utils/format';\nexport function go(): number { return pickMe(); }\n",
    );
    fx.write(
        "tsconfig.json",
        r#"{"compilerOptions":{"baseUrl":"./src","paths":{"@utils/*":["utils/*"]}}}"#,
    );
    for f in ["src/utils/format.ts", "src/legacy/format.ts", "src/main.ts"] {
        fx.track(&q, f, Language::Typescript);
    }

    let utils_pick = exported(node(
        "func:src/utils/format.ts:pickMe:1",
        NodeKind::Function,
        "pickMe",
        "src/utils/format.ts::pickMe",
        "src/utils/format.ts",
        Language::Typescript,
        1,
        1,
    ));
    let legacy_pick = exported(node(
        "func:src/legacy/format.ts:pickMe:1",
        NodeKind::Function,
        "pickMe",
        "src/legacy/format.ts::pickMe",
        "src/legacy/format.ts",
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
    q.insert_nodes(&[utils_pick.clone(), legacy_pick.clone(), go_fn.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &go_fn.id,
        "pickMe",
        EdgeKind::Calls,
        2,
        "src/main.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let utils_callers = incoming(&q, &utils_pick.id, EdgeKind::Calls);
    let legacy_callers = incoming(&q, &legacy_pick.id, EdgeKind::Calls);
    assert!(!utils_callers.is_empty());
    assert!(source_files(&q, &utils_callers).contains(&"src/main.ts".to_string()));
    // The legacy node should NOT have a caller from src/main.ts — the alias
    // correctly picked the utils version.
    assert!(!source_files(&q, &legacy_callers).contains(&"src/main.ts".to_string()));
}

#[test]
fn falls_back_gracefully_when_tsconfig_is_absent() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write("src/a.ts", "export function aFn(): void {}\n");
    fx.write(
        "src/b.ts",
        "import { aFn } from './a';\nexport function bFn(): void { aFn(); }\n",
    );
    fx.track(&q, "src/a.ts", Language::Typescript);
    fx.track(&q, "src/b.ts", Language::Typescript);

    let a_fn = exported(node(
        "func:src/a.ts:aFn:1",
        NodeKind::Function,
        "aFn",
        "src/a.ts::aFn",
        "src/a.ts",
        Language::Typescript,
        1,
        1,
    ));
    let b_fn = exported(node(
        "func:src/b.ts:bFn:2",
        NodeKind::Function,
        "bFn",
        "src/b.ts::bFn",
        "src/b.ts",
        Language::Typescript,
        2,
        2,
    ));
    q.insert_nodes(&[a_fn.clone(), b_fn.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &b_fn.id,
        "aFn",
        EdgeKind::Calls,
        2,
        "src/b.ts",
        Language::Typescript,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .unwrap();

    let callers = incoming(&q, &a_fn.id, EdgeKind::Calls);
    assert!(source_files(&q, &callers).contains(&"src/b.ts".to_string()));
}

// =============================================================================
// re-export chain following (resolution.test.ts)
// =============================================================================
