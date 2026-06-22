// Stable marker — assert the `· skeleton` tag, not its exact trailing wording.
const SKELETON_MARK: &str = "· skeleton (signatures only";

// Names the spine (dispatch/proceed/handleLogging), the on-spine exemplar,
// the three off-spine siblings, and the distinct step.
const QUERY: &str = "dispatch proceed handleLogging LoggingInterceptor BridgeInterceptor CacheInterceptor RetryInterceptor ResponseFormatter";

// Names AuthInterceptor's `authenticate` and Codec's `encode` (both methods),
// plus the spine tokens so a spine still forms.
fn spare_query() -> String {
    format!("{QUERY} authenticate encode AuthInterceptor Codec JsonCodec")
}

/// The OkHttp-interceptor-chain-in-miniature fixture (see the TS test header
/// for the full design rationale).
fn adaptive_fixture(root: &Path) -> CodeGraph {
    let src = root.join("src");

    // The interchangeable contract — 4+ implementers below => sibling family.
    write(
        &src.join("interceptor.ts"),
        "export interface Interceptor {\n  intercept(request: string): string;\n}\n",
    );

    // The mechanism + the spine: dispatch -> proceed -> handleLogging.
    write(
        &src.join("dispatcher.ts"),
        "import { LoggingInterceptor } from './logging-interceptor';\n\nexport class RequestDispatcher {\n  dispatch(): string {\n    const chain = new InterceptorChain();\n    return chain.proceed();\n  }\n}\n\nexport class InterceptorChain {\n  proceed(): string {\n    const exemplar = new LoggingInterceptor();\n    return exemplar.handleLogging();\n  }\n}\n",
    );

    // On-spine exemplar: must stay FULL even though it is a sibling.
    write(
        &src.join("logging-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class LoggingInterceptor implements Interceptor {\n  handleLogging(): string {\n    const tag = 'LOGGING_BODY_MARKER';\n    return this.intercept(tag);\n  }\n  intercept(request: string): string {\n    return 'logged:' + request;\n  }\n}\n",
    );

    // Off-spine siblings — interchangeable impls of Interceptor => SKELETONIZE.
    write(
        &src.join("bridge-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class BridgeInterceptor implements Interceptor {\n  intercept(request: string): string {\n    const detail = 'BRIDGE_BODY_MARKER';\n    return 'bridged:' + request + detail;\n  }\n}\n",
    );
    write(
        &src.join("cache-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class CacheInterceptor implements Interceptor {\n  intercept(request: string): string {\n    const detail = 'CACHE_BODY_MARKER';\n    return 'cached:' + request + detail;\n  }\n}\n",
    );
    write(
        &src.join("retry-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class RetryInterceptor implements Interceptor {\n  intercept(request: string): string {\n    const detail = 'RETRY_BODY_MARKER';\n    return 'retried:' + request + detail;\n  }\n}\n",
    );

    // A 1:1 interface->impl pair: a DISTINCT step => FULL.
    write(
        &src.join("formatter.ts"),
        "export interface Formatter {\n  format(input: string): string;\n}\n",
    );
    write(
        &src.join("response-formatter.ts"),
        "import { Formatter } from './formatter';\nimport { JsonCodec } from './codec';\n\nexport class ResponseFormatter implements Formatter {\n  format(input: string): string {\n    const detail = 'FORMATTER_BODY_MARKER';\n    // Calls into the Codec family from OFF the dispatch spine.\n    return new JsonCodec().encode(input) + detail;\n  }\n}\n",
    );

    // An off-spine sibling that owns a uniquely-named method `authenticate`
    // (the RealCall fix): a named callable means "show me this" => stays full.
    write(
        &src.join("auth-interceptor.ts"),
        "import { Interceptor } from './interceptor';\n\nexport class AuthInterceptor implements Interceptor {\n  authenticate(token: string): string {\n    const detail = 'AUTH_BODY_MARKER';\n    return 'auth:' + token + detail;\n  }\n  intercept(request: string): string {\n    return this.authenticate(request);\n  }\n}\n",
    );

    // A base class that DEFINES a >=3-impl supertype AND co-locates its
    // subclasses (Django compiler.py shape).
    write(
        &src.join("codec.ts"),
        "export class Codec {\n  encode(input: string): string {\n    const detail = 'CODEC_BASE_MARKER';\n    return input + detail;\n  }\n}\nexport class JsonCodec extends Codec {\n  encode(input: string): string { return '{' + input + '}'; }\n}\nexport class XmlCodec extends Codec {\n  encode(input: string): string {\n    const detail = 'XML_BODY_MARKER';\n    return '<' + input + detail + '>';\n  }\n}\nexport class YamlCodec extends Codec {\n  encode(input: string): string { return '- ' + input; }\n}\n",
    );

    let cg = CodeGraph::init_sync(root).unwrap();
    cg.index_all(&IndexOptions::default()).unwrap();
    cg
}

fn explore_with_max_files(handler: &ToolHandler, query: &str, max_files: u32) -> String {
    let res = handler.execute(
        "codegraph_explore",
        &json!({ "query": query, "maxFiles": max_files }),
    );
    assert_ne!(res.is_error, Some(true), "explore errored: {}", res.text());
    res.text().to_string()
}

#[test]
fn fixture_sanity_interceptor_has_3_plus_implementers_formatter_has_fewer() {
    let _env = env_read();
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());

    let find = |name: &str, kind: NodeKind| {
        cg.search_nodes(name, None)
            .unwrap()
            .into_iter()
            .map(|r| r.node)
            .find(|n| n.name == name && n.kind == kind)
    };

    let interceptor = find("Interceptor", NodeKind::Interface).expect("Interceptor interface");
    let formatter = find("Formatter", NodeKind::Interface).expect("Formatter interface");

    let implementers = |id: &str| {
        cg.get_incoming_edges(id)
            .unwrap()
            .into_iter()
            .filter(|e| e.kind == EdgeKind::Implements || e.kind == EdgeKind::Extends)
            .count()
    };

    // The whole gate hinges on this signal — assert the fixture actually
    // produces the >=3 / <3 split.
    assert!(implementers(&interceptor.id) >= 3);
    assert!(implementers(&formatter.id) < 3);
}

#[test]
fn skeletonizes_off_spine_polymorphic_siblings() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, QUERY, 12);

    // Precondition: the spine must have formed, or nothing skeletonizes.
    assert!(
        text.contains("## Flow (call path among the symbols you queried)"),
        "no flow spine formed:\n{text}"
    );

    for (file, marker) in [
        ("bridge-interceptor.ts", "BRIDGE_BODY_MARKER"),
        ("cache-interceptor.ts", "CACHE_BODY_MARKER"),
        ("retry-interceptor.ts", "RETRY_BODY_MARKER"),
    ] {
        let section = section_for(&text, file);
        assert!(
            !section.is_empty(),
            "{file} should be present in the explore output"
        );
        assert!(
            section.contains(SKELETON_MARK),
            "{file} should be skeletonized:\n{section}"
        );
        // The signature line survives; the body (with its marker) is elided.
        assert!(section.contains("intercept(request"));
        assert!(
            !section.contains(marker),
            "{file} body marker must NOT survive skeletonization"
        );
    }
}

#[test]
fn keeps_the_on_spine_exemplar_full_even_though_it_is_a_sibling() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, QUERY, 12);

    let section = section_for(&text, "logging-interceptor.ts");
    assert!(
        !section.is_empty(),
        "logging-interceptor.ts should be present"
    );
    assert!(
        !section.contains(SKELETON_MARK),
        "on-spine exemplar must NOT be skeletonized:\n{section}"
    );
    // Full source => the body marker is present.
    assert!(section.contains("LOGGING_BODY_MARKER"));
}

#[test]
fn keeps_a_distinct_step_full() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, QUERY, 12);

    let section = section_for(&text, "response-formatter.ts");
    assert!(
        !section.is_empty(),
        "response-formatter.ts should be present"
    );
    assert!(
        !section.contains(SKELETON_MARK),
        "a 1:1 interface impl is not a sibling and must stay full"
    );
    assert!(section.contains("FORMATTER_BODY_MARKER"));
}

#[test]
fn adaptive_explore_env_zero_disables_skeletonization() {
    let _env = env_write();
    let _guard = EnvVarGuard::set("CODEGRAPH_ADAPTIVE_EXPLORE", "0");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, QUERY, 12);

    assert!(
        !text.contains(SKELETON_MARK),
        "no file should be skeletonized with the flag off"
    );
    // The previously-skeletonized siblings now render their full bodies.
    let section = section_for(&text, "bridge-interceptor.ts");
    assert!(!section.is_empty());
    assert!(section.contains("BRIDGE_BODY_MARKER"));
}

#[test]
fn spares_an_off_spine_sibling_when_the_agent_named_a_callable_in_it() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, &spare_query(), 15);
    assert!(text.contains("## Flow (call path among the symbols you queried)"));

    // auth-interceptor.ts is an off-spine Interceptor sibling — would
    // skeletonize — but the agent named its method `authenticate`, so it
    // stays FULL.
    let auth = section_for(&text, "auth-interceptor.ts");
    assert!(!auth.is_empty(), "auth-interceptor.ts should be present");
    assert!(
        !auth.contains(SKELETON_MARK),
        "a file holding an agent-named callable must NOT be skeletonized"
    );
    assert!(auth.contains("AUTH_BODY_MARKER"));

    // Contrast: bridge-interceptor.ts — same family, named only by TYPE —
    // still skeletonizes.
    let bridge = section_for(&text, "bridge-interceptor.ts");
    assert!(
        bridge.contains(SKELETON_MARK),
        "a sibling named only by type still skeletonizes:\n{bridge}"
    );
    assert!(!bridge.contains("BRIDGE_BODY_MARKER"));
}

#[test]
fn collapses_a_base_plus_subclasses_family_file_to_a_focused_view() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    let text = explore_with_max_files(&handler, &spare_query(), 15);

    // codec.ts defines the base Codec (>=3 subclasses extend it) and
    // co-locates the subclasses — a "family" file. It COLLAPSES, but
    // per-symbol: the named base method `Codec.encode` keeps its body while a
    // non-named subclass (XmlCodec) collapses to a signature.
    let codec = section_for(&text, "codec.ts");
    assert!(!codec.is_empty(), "codec.ts should be present");
    assert!(
        codec.contains("· focused"),
        "a named family file collapses to a focused (not full) view:\n{codec}"
    );
    assert!(
        codec.contains("CODEC_BASE_MARKER"),
        "the named base method body is kept (no Read-back)"
    );
    assert!(
        !codec.contains("XML_BODY_MARKER"),
        "a non-named subclass body is elided to a signature"
    );
}

#[test]
fn naming_a_shared_polymorphic_method_does_not_spare_the_siblings() {
    let _env = env_write();
    let _guard = EnvVarGuard::unset("CODEGRAPH_ADAPTIVE_EXPLORE");
    let dir = TempDir::new().unwrap();
    let cg = adaptive_fixture(dir.path());
    let handler = ToolHandler::new(Some(Rc::new(cg)));
    // `intercept` is implemented by every interceptor — a polymorphic name,
    // not a unique one. Naming it must NOT keep all five full.
    let text = explore_with_max_files(&handler, &format!("{QUERY} intercept"), 12);

    let bridge = section_for(&text, "bridge-interceptor.ts");
    assert!(
        bridge.contains(SKELETON_MARK),
        "a sibling named only via a shared method is not spared:\n{bridge}"
    );
    assert!(
        !bridge.contains("BRIDGE_BODY_MARKER"),
        "a shared method does not earn a body in a non-supertype leaf"
    );
}

// =============================================================================
// codegraph_explore — blast radius (__tests__/explore-blast-radius.test.ts)
