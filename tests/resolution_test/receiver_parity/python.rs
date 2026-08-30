use crate::fixture::*;

#[tokio::test(flavor = "current_thread")]
async fn python_imported_module_receiver_resolves_member_without_external_decoy() {
    // Given: an in-project submodule and a same-named function outside it.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("pkg/__init__.py", "");
    fx.write("pkg/module.py", "def func():\n    return 1\n");
    fx.write(
        "main.py",
        "from pkg import module\nimport os\n\ndef caller():\n    return module.func()\n\ndef external_caller():\n    return os.func()\n",
    );
    for path in ["pkg/__init__.py", "pkg/module.py", "main.py"] {
        fx.track(&q, path, Language::Python);
    }

    let module_file = node(
        "file:module",
        NodeKind::File,
        "module.py",
        "pkg/module.py",
        "pkg/module.py",
        Language::Python,
        1,
        2,
    );
    let target = node(
        "function:module:func",
        NodeKind::Function,
        "func",
        "func",
        "pkg/module.py",
        Language::Python,
        1,
        2,
    );
    let decoy = node(
        "function:decoy:func",
        NodeKind::Function,
        "func",
        "func",
        "other.py",
        Language::Python,
        1,
        2,
    );
    let caller = node(
        "function:caller",
        NodeKind::Function,
        "caller",
        "caller",
        "main.py",
        Language::Python,
        4,
        5,
    );
    let external = node(
        "function:external",
        NodeKind::Function,
        "external_caller",
        "external_caller",
        "main.py",
        Language::Python,
        7,
        8,
    );
    q.insert_nodes(&[
        module_file,
        target.clone(),
        decoy.clone(),
        caller.clone(),
        external.clone(),
    ])
    .unwrap();
    q.insert_unresolved_refs_batch(&[
        uref(
            &caller.id,
            "module.func",
            EdgeKind::Calls,
            5,
            "main.py",
            Language::Python,
        ),
        uref(
            &external.id,
            "os.func",
            EdgeKind::Calls,
            8,
            "main.py",
            Language::Python,
        ),
    ])
    .unwrap();

    // When: module-qualified calls are resolved.
    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    // Then: only the real module member receives an edge.
    let calls = outgoing(&q, &caller.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, target.id);
    assert_ne!(calls[0].target, decoy.id);
    assert!(outgoing(&q, &external.id, EdgeKind::Calls).is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn python_imported_singleton_receiver_resolves_owning_method_only() {
    // Given: an imported module-level instance and a same-named method decoy.
    let fx = Fx::new();
    let q = fx.q();
    fx.write(
        "store.py",
        "class ReproStore:\n    def notify(self):\n        pass\n\nrepro_store = ReproStore()\n",
    );
    fx.write(
        "caller.py",
        "from .store import repro_store\n\ndef call_imported():\n    repro_store.notify()\n",
    );
    for path in ["store.py", "caller.py"] {
        fx.track(&q, path, Language::Python);
    }

    let value = exported(node(
        "variable:repro_store",
        NodeKind::Variable,
        "repro_store",
        "repro_store",
        "store.py",
        Language::Python,
        5,
        5,
    ));
    let target = node(
        "method:repro:notify",
        NodeKind::Method,
        "notify",
        "ReproStore::notify",
        "store.py",
        Language::Python,
        2,
        3,
    );
    let decoy = node(
        "method:other:notify",
        NodeKind::Method,
        "notify",
        "ReproStoreShadow::notify",
        "other.py",
        Language::Python,
        2,
        3,
    );
    let caller = node(
        "function:call_imported",
        NodeKind::Function,
        "call_imported",
        "call_imported",
        "caller.py",
        Language::Python,
        3,
        4,
    );
    q.insert_nodes(&[value, decoy.clone(), target.clone(), caller.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &caller.id,
        "repro_store.notify",
        EdgeKind::Calls,
        4,
        "caller.py",
        Language::Python,
    )])
    .unwrap();

    // When: the imported singleton call is resolved.
    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    // Then: it targets the singleton's class method, never the decoy.
    let calls = outgoing(&q, &caller.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, target.id);
    assert_ne!(calls[0].target, decoy.id);
}

#[tokio::test(flavor = "current_thread")]
async fn python_aliased_from_import_module_member_resolves_to_real_submodule() {
    // `from pkg import module as mod_alias` binds the SUBMODULE `pkg.module`
    // at the local name `mod_alias`. The module path must be rebuilt from the
    // EXPORTED name (`module`), not the local one — building `pkg.mod_alias`
    // looks for a file that does not exist, so the aliased `mod_alias.func()`
    // call used to drop its `calls` edge while the plain form worked (#1626).
    let fx = Fx::new();
    let q = fx.q();
    fx.write("pkg/__init__.py", "");
    fx.write("pkg/module.py", "def func():\n    return 1\n");
    fx.write(
        "main.py",
        "from pkg import module as mod_alias\n\ndef caller():\n    return mod_alias.func()\n",
    );
    for path in ["pkg/__init__.py", "pkg/module.py", "main.py"] {
        fx.track(&q, path, Language::Python);
    }

    let module_file = node(
        "file:module",
        NodeKind::File,
        "module.py",
        "pkg/module.py",
        "pkg/module.py",
        Language::Python,
        1,
        2,
    );
    let target = node(
        "function:module:func",
        NodeKind::Function,
        "func",
        "func",
        "pkg/module.py",
        Language::Python,
        1,
        2,
    );
    let caller = node(
        "function:caller",
        NodeKind::Function,
        "caller",
        "caller",
        "main.py",
        Language::Python,
        3,
        4,
    );
    q.insert_nodes(&[module_file, target.clone(), caller.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &caller.id,
        "mod_alias.func",
        EdgeKind::Calls,
        4,
        "main.py",
        Language::Python,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let calls = outgoing(&q, &caller.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, target.id);
}

#[tokio::test(flavor = "current_thread")]
async fn python_aliased_function_direct_call_resolves_through_absolute_module() {
    // `from scripts.lire_fec import summarize as fec_summary` then a bare
    // `fec_summary()` call. Two compounding gaps (#1518): the generic import
    // loop had no findPythonModuleFile fallback for an ABSOLUTE dotted source
    // (`scripts.lire_fec`), and the Python extractor never set `is_exported`,
    // so the exported-name filter dropped the target. An aliased direct call
    // has no name to fuzzy-match on, so this generic path was its only route.
    let fx = Fx::new();
    let q = fx.q();
    fx.write("scripts/__init__.py", "");
    fx.write("scripts/lire_fec.py", "def summarize():\n    return 1\n");
    fx.write(
        "services/assistant.py",
        "from scripts.lire_fec import summarize as fec_summary\n\ndef build_context():\n    return fec_summary()\n",
    );
    for path in [
        "scripts/__init__.py",
        "scripts/lire_fec.py",
        "services/assistant.py",
    ] {
        fx.track(&q, path, Language::Python);
    }

    let module_file = node(
        "file:lire_fec",
        NodeKind::File,
        "lire_fec.py",
        "scripts/lire_fec.py",
        "scripts/lire_fec.py",
        Language::Python,
        1,
        2,
    );
    let target = exported(node(
        "function:lire_fec:summarize",
        NodeKind::Function,
        "summarize",
        "summarize",
        "scripts/lire_fec.py",
        Language::Python,
        1,
        2,
    ));
    let caller = node(
        "function:build_context",
        NodeKind::Function,
        "build_context",
        "build_context",
        "services/assistant.py",
        Language::Python,
        3,
        4,
    );
    q.insert_nodes(&[module_file, target.clone(), caller.clone()])
        .unwrap();
    q.insert_unresolved_refs_batch(&[uref(
        &caller.id,
        "fec_summary",
        EdgeKind::Calls,
        4,
        "services/assistant.py",
        Language::Python,
    )])
    .unwrap();

    fx.resolver()
        .resolve_and_persist_batched(None, None)
        .await
        .unwrap();

    let calls = outgoing(&q, &caller.id, EdgeKind::Calls);
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, target.id);
}
