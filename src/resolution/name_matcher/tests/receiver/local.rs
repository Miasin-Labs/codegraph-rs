use super::super::{Fixture, make_ref, match_method_call, node};
use crate::resolution::types::ResolvedBy;
use crate::types::{EdgeKind, Language, NodeKind};

fn method(id: &str, owner: &str, language: Language) -> crate::types::Node {
    node(
        id,
        NodeKind::Method,
        "log",
        &format!("src/logger::{}::log", owner),
        "src/logger.ts",
        language,
        1,
        2,
    )
}

#[test]
fn typed_parameter_inference_disambiguates_typescript_receiver() {
    let mut ctx = Fixture::new(vec![
        method("other-log", "Other", Language::Typescript),
        method("logger-log", "Logger", Language::Typescript),
    ]);
    ctx.files.insert(
        "src/use.ts".into(),
        "function use(lg: Logger) {\n  lg.log();\n}\n".into(),
    );
    let reference = make_ref(
        "lg.log",
        EdgeKind::Calls,
        2,
        "src/use.ts",
        Language::Typescript,
    );

    let result = match_method_call(&reference, &ctx).expect("typed receiver should resolve");
    assert_eq!(result.target_node_id, "logger-log");
    assert_eq!(result.resolved_by, ResolvedBy::InstanceMethod);
    assert_eq!(result.confidence, 0.9);
}

#[test]
fn local_inference_is_bounded_to_the_enclosing_function() {
    let mut ctx = Fixture::new(vec![
        method("other-log", "Other", Language::Typescript),
        method("logger-log", "Logger", Language::Typescript),
        node(
            "first",
            NodeKind::Function,
            "first",
            "src/use::first",
            "src/use.ts",
            Language::Typescript,
            1,
            3,
        ),
        node(
            "second",
            NodeKind::Function,
            "second",
            "src/use::second",
            "src/use.ts",
            Language::Typescript,
            5,
            8,
        ),
    ]);
    ctx.files.insert(
        "src/use.ts".into(),
        "function first() {\n const lg = new Other();\n}\n\nfunction second() {\n const lg = new Logger();\n lg.log();\n}\n"
            .into(),
    );
    let reference = make_ref(
        "lg.log",
        EdgeKind::Calls,
        7,
        "src/use.ts",
        Language::Typescript,
    );

    let result = match_method_call(&reference, &ctx).expect("nearest local should resolve");
    assert_eq!(result.target_node_id, "logger-log");
}

#[test]
fn chained_receiver_reaches_method_name_fallback() {
    let target = node(
        "add-core",
        NodeKind::Method,
        "AddCoreServices",
        "src/services::Extensions::AddCoreServices",
        "src/services.cs",
        Language::Csharp,
        1,
        2,
    );
    let ctx = Fixture::new(vec![target]);
    let reference = make_ref(
        "builder.Services.AddCoreServices",
        EdgeKind::Calls,
        4,
        "src/program.cs",
        Language::Csharp,
    );

    let result = match_method_call(&reference, &ctx).expect("chained call should parse");
    assert_eq!(result.target_node_id, "add-core");
}

#[test]
fn lua_colon_call_uses_local_receiver_inference() {
    let mut ctx = Fixture::new(vec![
        method("other-log", "Other", Language::Lua),
        method("logger-log", "Logger", Language::Lua),
    ]);
    ctx.files.insert(
        "src/use.lua".into(),
        "local lg = Logger.new()\nlg:log()\n".into(),
    );
    let reference = make_ref("lg:log", EdgeKind::Calls, 2, "src/use.lua", Language::Lua);

    let result = match_method_call(&reference, &ctx).expect("Lua receiver should resolve");
    assert_eq!(result.target_node_id, "logger-log");
}

#[test]
fn builtin_map_local_receiver_does_not_resolve_to_project_method() {
    // #1566: a local `Map` receiver (`const values = new Map(); values.get()`)
    // must NOT fall back to a sole same-named project method. Receiver-type
    // inference identifies the external/built-in `Map`, and no project method
    // lives on it — the call stays unresolved instead of guessing.
    let mut ctx = Fixture::new(vec![
        node(
            "lru-get",
            NodeKind::Method,
            "get",
            "src/cache::LRUCache::get",
            "src/cache.ts",
            Language::Typescript,
            4,
            6,
        ),
        node(
            "lru-set",
            NodeKind::Method,
            "set",
            "src/cache::LRUCache::set",
            "src/cache.ts",
            Language::Typescript,
            8,
            10,
        ),
        node(
            "lru-has",
            NodeKind::Method,
            "has",
            "src/cache::LRUCache::has",
            "src/cache.ts",
            Language::Typescript,
            12,
            14,
        ),
    ]);
    ctx.files.insert(
        "src/use.ts".into(),
        "function useLocalMap(): boolean {\n  const values = new Map<string, string>();\n  values.set(\"answer\", \"42\");\n  values.get(\"answer\");\n  return values.has(\"answer\");\n}\n"
            .into(),
    );

    for (name, line) in [("values.set", 3), ("values.get", 4), ("values.has", 5)] {
        let reference = make_ref(
            name,
            EdgeKind::Calls,
            line,
            "src/use.ts",
            Language::Typescript,
        );
        assert!(
            match_method_call(&reference, &ctx).is_none(),
            "built-in Map receiver call `{name}` must stay unresolved"
        );
    }
}

#[test]
fn project_class_local_receiver_still_resolves_after_builtin_guard() {
    // Positive control: a local constructed as a PROJECT class still resolves
    // to that class's method — the #1566 guard only fires for non-project
    // (external/built-in) inferred types.
    let mut ctx = Fixture::new(vec![
        node(
            "logger-class",
            NodeKind::Class,
            "Logger",
            "src/logger::Logger",
            "src/logger.ts",
            Language::Typescript,
            1,
            20,
        ),
        node(
            "logger-log",
            NodeKind::Method,
            "log",
            "src/logger::Logger::log",
            "src/logger.ts",
            Language::Typescript,
            5,
            8,
        ),
    ]);
    ctx.files.insert(
        "src/use.ts".into(),
        "function use() {\n  const lg = new Logger();\n  lg.log();\n}\n".into(),
    );
    let reference = make_ref(
        "lg.log",
        EdgeKind::Calls,
        3,
        "src/use.ts",
        Language::Typescript,
    );
    let result = match_method_call(&reference, &ctx).expect("project-class receiver resolves");
    assert_eq!(result.target_node_id, "logger-log");
}
