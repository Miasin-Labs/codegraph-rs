use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn creates_resolver_that_detects_react_from_project() {
    let fx = Fx::new();
    fx.write(
        "package.json",
        r#"{"name":"test","dependencies":{"react":"^18.0.0"}}"#,
    );
    fx.write(
        "src/utils.ts",
        "export function formatDate(date: Date): string {\n  return date.toISOString();\n}\n",
    );
    fx.write("src/main.ts", "import { formatDate } from './utils';\n");

    let resolver = fx.resolver();
    let frameworks = resolver.get_detected_frameworks();
    assert!(frameworks.contains(&"react".to_string()));
}

#[tokio::test(flavor = "current_thread")]
async fn resolves_references_after_indexing() {
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/helper.ts",
        "export function helperFunction(): void {\n  console.log('helper');\n}",
    );
    fx.write(
        "src/main.ts",
        "import { helperFunction } from './helper';\n\nfunction main(): void {\n  helperFunction();\n}",
    );
    fx.track(&q, "src/helper.ts", Language::Typescript);
    fx.track(&q, "src/main.ts", Language::Typescript);

    let helper = exported(node(
        "func:src/helper.ts:helperFunction:1",
        NodeKind::Function,
        "helperFunction",
        "src/helper.ts::helperFunction",
        "src/helper.ts",
        Language::Typescript,
        1,
        3,
    ));
    let main = node(
        "func:src/main.ts:main:3",
        NodeKind::Function,
        "main",
        "src/main.ts::main",
        "src/main.ts",
        Language::Typescript,
        3,
        5,
    );
    q.insert_nodes(&[helper.clone(), main.clone()]).unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &main.id,
        "helperFunction",
        EdgeKind::Calls,
        4,
        "src/main.ts",
        Language::Typescript,
    )])
    .unwrap();

    let resolver = fx.resolver();
    let result = resolver
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    // TS assertion: should have attempted resolution.
    assert!(result.stats.total >= 1);
    // Port validation: the import-based call edge landed.
    let callers = incoming(&q, &helper.id, EdgeKind::Calls);
    assert_eq!(callers.len(), 1);
    assert_eq!(callers[0].source, main.id);
}

#[tokio::test(flavor = "current_thread")]
async fn promotes_calls_to_instantiates_when_target_is_a_class_python() {
    // Python has no `new` keyword — `Foo()` is the standard instantiation
    // syntax. Extraction emits a `calls` ref; resolution promotes it to
    // `instantiates` once the target is known to be a class.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "src/app.py",
        "class UserService:\n    def __init__(self):\n        self.db = None\n\ndef bootstrap():\n    return UserService()\n",
    );
    fx.track(&q, "src/app.py", Language::Python);

    let class = node(
        "class:src/app.py:UserService:1",
        NodeKind::Class,
        "UserService",
        "src/app.py::UserService",
        "src/app.py",
        Language::Python,
        1,
        3,
    );
    let init = node(
        "method:src/app.py:UserService.__init__:2",
        NodeKind::Method,
        "__init__",
        "src/app.py::UserService::__init__",
        "src/app.py",
        Language::Python,
        2,
        3,
    );
    let bootstrap = node(
        "func:src/app.py:bootstrap:5",
        NodeKind::Function,
        "bootstrap",
        "src/app.py::bootstrap",
        "src/app.py",
        Language::Python,
        5,
        6,
    );
    q.insert_nodes(&[class.clone(), init, bootstrap.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &bootstrap.id,
        "UserService",
        EdgeKind::Calls,
        6,
        "src/app.py",
        Language::Python,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let out = q.get_outgoing_edges(&bootstrap.id, None, None).unwrap();
    let instantiates: Vec<&Edge> = out
        .iter()
        .filter(|e| e.kind == EdgeKind::Instantiates)
        .collect();
    assert_eq!(instantiates.len(), 1, "promoted instantiates edge expected");
    assert_eq!(instantiates[0].target, class.id);
    // Same edge must NOT also appear as a `calls` edge — promotion replaces
    // the kind, doesn't duplicate.
    let calls_to_user_service: Vec<&Edge> = out
        .iter()
        .filter(|e| e.kind == EdgeKind::Calls && e.target == instantiates[0].target)
        .collect();
    assert!(calls_to_user_service.is_empty());
}
