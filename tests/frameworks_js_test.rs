//! JS/TS-ecosystem framework resolver tests.
//!
//! Ported from the cases covering express/nestjs/react/svelte in
//! `__tests__/frameworks.test.ts`, plus the unit (non-end-to-end) cases of
//! `__tests__/expo-modules.test.ts`, `__tests__/fabric-view.test.ts` and
//! `__tests__/react-native-bridge.test.ts`.
//!
//! Deferred (need the full CodeGraph pipeline, owned by other port agents —
//! see rust/notes/frameworks-js.md):
//!   - `__tests__/rn-event-channel.test.ts` (entirely e2e through the
//!     callback synthesizer's `rn-event-channel`)
//!   - the "end-to-end" describe blocks of expo-modules / fabric-view tests
//!   - `getApplicableFrameworks` cases (frameworks/mod.rs is stitched later)

use std::collections::HashMap;

use codegraph::resolution::frameworks::expo_modules::ExpoModulesResolver;
use codegraph::resolution::frameworks::express::ExpressResolver;
use codegraph::resolution::frameworks::fabric::FabricViewResolver;
use codegraph::resolution::frameworks::nestjs::NestjsResolver;
use codegraph::resolution::frameworks::react::ReactResolver;
use codegraph::resolution::frameworks::react_native::ReactNativeBridgeResolver;
use codegraph::resolution::frameworks::svelte::SvelteResolver;
use codegraph::resolution::frameworks::vue::VueResolver;
use codegraph::resolution::types::{
    FrameworkResolver,
    ImportMapping,
    ResolutionContext,
    ResolvedBy,
    UnresolvedRef,
};
use codegraph::types::{EdgeKind, Language, Node, NodeKind};

// ─── Test fixtures ──────────────────────────────────────────────────────────

/// Mock ResolutionContext over an in-memory node list + file map (mirrors the
/// inline `baseContext`/`makeContext` objects in the TS tests).
#[derive(Default)]
struct TestContext {
    nodes: Vec<Node>,
    files: HashMap<String, String>,
}

impl TestContext {
    fn new(nodes: Vec<Node>, files: &[(&str, &str)]) -> Self {
        TestContext {
            nodes,
            files: files
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }
}

impl ResolutionContext for TestContext {
    fn get_nodes_in_file(&self, file_path: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.file_path == file_path)
            .cloned()
            .collect()
    }
    fn get_nodes_by_name(&self, name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.name == name)
            .cloned()
            .collect()
    }
    fn get_nodes_by_qualified_name(&self, _qualified_name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.kind == kind)
            .cloned()
            .collect()
    }
    fn file_exists(&self, file_path: &str) -> bool {
        self.files.contains_key(file_path) || self.nodes.iter().any(|n| n.file_path == file_path)
    }
    fn read_file(&self, file_path: &str) -> Option<String> {
        self.files.get(file_path).cloned()
    }
    fn get_project_root(&self) -> &str {
        "/test"
    }
    fn get_all_files(&self) -> Vec<String> {
        let mut all: Vec<String> = self.files.keys().cloned().collect();
        for n in &self.nodes {
            if !all.contains(&n.file_path) {
                all.push(n.file_path.clone());
            }
        }
        all
    }
    fn get_nodes_by_lower_name(&self, _lower_name: &str) -> Vec<Node> {
        Vec::new()
    }
    fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
}

fn mk_node(
    id: &str,
    kind: NodeKind,
    name: &str,
    qualified_name: &str,
    file_path: &str,
    language: Language,
    start_line: u32,
    end_line: u32,
) -> Node {
    Node::new(
        id,
        kind,
        name,
        qualified_name,
        file_path,
        language,
        start_line,
        end_line,
    )
}

fn mk_method(name: &str, language: Language, file_path: &str) -> Node {
    let start_line = 10;
    mk_node(
        &format!("{language:?}:{file_path}:{name}:{start_line}"),
        NodeKind::Method,
        name,
        &format!("{file_path}::{name}"),
        file_path,
        language,
        start_line,
        start_line + 5,
    )
}

fn mk_ref(name: &str, language: Language, file_path: &str) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: format!("caller:{file_path}"),
        reference_name: name.to_string(),
        reference_kind: EdgeKind::Calls,
        line: 1,
        column: 0,
        file_path: file_path.to_string(),
        language,
        candidates: None,
    }
}

// ─── expressResolver.extract ────────────────────────────────────────────────

#[test]
fn express_extracts_route_with_inline_handler_reference() {
    let src = "app.get('/users', listUsers);\n";
    let result = ExpressResolver.extract("routes.ts", src).unwrap();
    assert_eq!(result.nodes.len(), 1);
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "listUsers");
}

#[test]
fn express_extracts_route_with_router_post_and_middleware_chain() {
    let src = "router.post('/items', auth, createItem);\n";
    let result = ExpressResolver.extract("items.ts", src).unwrap();
    assert_eq!(result.nodes[0].name, "POST /items");
    // Multiple handlers: prefer the LAST one (convention: middleware first, handler last)
    assert_eq!(result.references[0].reference_name, "createItem");
}

#[test]
fn express_extracts_route_with_controller_method_reference() {
    let src = "app.get('/x', userController.list);\n";
    let result = ExpressResolver.extract("routes.ts", src).unwrap();
    assert_eq!(result.references[0].reference_name, "list");
}

#[test]
fn express_skips_commented_routes() {
    let src = "\n// app.get('/fake', fakeHandler);\n/* router.post('/also-fake', otherHandler); */\napp.get('/real', realHandler);\n";
    let result = ExpressResolver.extract("routes.ts", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["GET /real"]);
    let refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(refs, vec!["realHandler"]);
}

// ─── nestjsResolver.extract — HTTP ──────────────────────────────────────────

#[test]
fn nestjs_joins_controller_prefix_with_get_and_links_handler() {
    let src = "\n@Controller('users')\nexport class UsersController {\n  @Get()\n  findAll() { return []; }\n}\n";
    let result = NestjsResolver.extract("users.controller.ts", src).unwrap();
    assert_eq!(result.nodes.len(), 1);
    assert_eq!(result.nodes[0].kind, NodeKind::Route);
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "findAll");
    assert_eq!(result.references[0].reference_kind, EdgeKind::References);
    assert_eq!(result.references[0].from_node_id, result.nodes[0].id);
}

#[test]
fn nestjs_joins_controller_prefix_with_method_level_path_param() {
    let src = "\n@Controller('cats')\nexport class CatsController {\n  @Get(':id')\n  findOne(@Param('id') id: string) { return id; }\n}\n";
    let result = NestjsResolver.extract("cats.controller.ts", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /cats/:id");
    assert_eq!(result.references[0].reference_name, "findOne");
}

#[test]
fn nestjs_handles_empty_controller_and_empty_post() {
    let src = "\n@Controller()\nexport class AppController {\n  @Post()\n  create() {}\n}\n";
    let result = NestjsResolver.extract("app.controller.ts", src).unwrap();
    assert_eq!(result.nodes[0].name, "POST /");
    assert_eq!(result.references[0].reference_name, "create");
}

#[test]
fn nestjs_covers_http_verbs_and_skips_intervening_method_decorators() {
    let src = "\n@Controller('todos')\nexport class TodosController {\n  @Put(':id')\n  @UseGuards(AuthGuard)\n  update(@Param('id') id: string) {}\n\n  @Delete(':id')\n  async remove(@Param('id') id: string) {}\n}\n";
    let result = NestjsResolver.extract("todos.controller.ts", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["PUT /todos/:id", "DELETE /todos/:id"]);
    let refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(refs, vec!["update", "remove"]);
}

#[test]
fn nestjs_attributes_methods_to_the_right_controller_when_a_file_has_two() {
    let src = "\n@Controller('a')\nexport class AController {\n  @Get('x')\n  ax() {}\n}\n\n@Controller('b')\nexport class BController {\n  @Get('y')\n  by() {}\n}\n";
    let result = NestjsResolver.extract("multi.controller.ts", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["GET /a/x", "GET /b/y"]);
}

// ─── nestjsResolver.extract — GraphQL ───────────────────────────────────────

#[test]
fn nestjs_emits_query_mutation_nodes_defaulting_to_method_name() {
    let src = "\n@Resolver(() => User)\nexport class UsersResolver {\n  @Query(() => [User])\n  users() { return []; }\n\n  @Mutation(() => User)\n  createUser(@Args('input') input: CreateUserInput) {}\n}\n";
    let result = NestjsResolver.extract("users.resolver.ts", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["QUERY users", "MUTATION createUser"]);
    let refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(refs, vec!["users", "createUser"]);
}

#[test]
fn nestjs_uses_explicit_operation_name_when_given() {
    let src = "\n@Resolver()\nexport class CatsResolver {\n  @Query(() => Cat, { name: 'cat' })\n  getCat() {}\n}\n";
    let result = NestjsResolver.extract("cats.resolver.ts", src).unwrap();
    assert_eq!(result.nodes[0].name, "QUERY cat");
}

#[test]
fn nestjs_does_not_treat_rest_query_parameter_decorator_as_graphql_op() {
    let src = "\n@Controller('search')\nexport class SearchController {\n  @Get()\n  search(@Query() query: SearchDto) { return query; }\n}\n";
    let result = NestjsResolver.extract("search.controller.ts", src).unwrap();
    // Only the HTTP route — the @Query() param decorator must be ignored.
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["GET /search"]);
}

// ─── nestjsResolver.extract — microservices & websockets ────────────────────

#[test]
fn nestjs_extracts_message_pattern_and_event_pattern_handlers() {
    let src = "\n@Controller()\nexport class MathController {\n  @MessagePattern({ cmd: 'sum' })\n  accumulate(data: number[]) {}\n\n  @EventPattern('user.created')\n  handleUserCreated(data: any) {}\n}\n";
    let result = NestjsResolver.extract("math.controller.ts", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["MESSAGE sum", "EVENT user.created"]);
    let refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(refs, vec!["accumulate", "handleUserCreated"]);
}

#[test]
fn nestjs_extracts_subscribe_message_handlers_with_gateway_namespace() {
    let src = "\n@WebSocketGateway({ namespace: 'chat' })\nexport class ChatGateway {\n  @SubscribeMessage('message')\n  handleMessage(@MessageBody() data: string) {}\n}\n";
    let result = NestjsResolver.extract("chat.gateway.ts", src).unwrap();
    assert_eq!(result.nodes[0].name, "WS chat:message");
    assert_eq!(result.references[0].reference_name, "handleMessage");
}

#[test]
fn nestjs_extracts_subscribe_message_without_namespace() {
    let src = "\n@WebSocketGateway()\nexport class EventsGateway {\n  @SubscribeMessage('events')\n  onEvent() {}\n}\n";
    let result = NestjsResolver.extract("events.gateway.ts", src).unwrap();
    assert_eq!(result.nodes[0].name, "WS events");
}

#[test]
fn nestjs_returns_empty_for_non_js_ts_file() {
    let result = NestjsResolver
        .extract("thing.py", "@Controller(\"x\")")
        .unwrap();
    assert!(result.nodes.is_empty());
    assert!(result.references.is_empty());
}

#[test]
fn nestjs_skips_commented_decorators() {
    let src = "\n@Controller('users')\nexport class UsersController {\n  // @Get('fake')\n  // fake() {}\n  /* @Post('also-fake')\n     alsoFake() {} */\n  @Get('real')\n  real() {}\n}\n";
    let result = NestjsResolver.extract("users.controller.ts", src).unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["GET /users/real"]);
    let refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    assert_eq!(refs, vec!["real"]);
}

// ─── nestjsResolver.detect ──────────────────────────────────────────────────

#[test]
fn nestjs_detects_nestjs_dep_in_package_json() {
    let ctx = TestContext::new(
        vec![],
        &[(
            "package.json",
            r#"{"dependencies":{"@nestjs/common":"^10.0.0"}}"#,
        )],
    );
    assert!(NestjsResolver.detect(&ctx));
}

#[test]
fn nestjs_detects_controller_in_controller_ts_file_without_package_json() {
    let ctx = TestContext::new(
        vec![],
        &[(
            "src/users.controller.ts",
            "@Controller('users')\nexport class UsersController {}",
        )],
    );
    assert!(NestjsResolver.detect(&ctx));
}

#[test]
fn nestjs_returns_false_for_non_nest_project() {
    let ctx = TestContext::new(
        vec![],
        &[("package.json", r#"{"dependencies":{"express":"^4"}}"#)],
    );
    assert!(!NestjsResolver.detect(&ctx));
}

// ─── nestjsResolver.resolve ─────────────────────────────────────────────────

#[test]
fn nestjs_resolves_injected_service_reference_to_class_in_service_file() {
    let svc_node = mk_node(
        "class:src/users/users.service.ts:UsersService:3",
        NodeKind::Class,
        "UsersService",
        "src/users/users.service.ts::UsersService",
        "src/users/users.service.ts",
        Language::Typescript,
        3,
        3,
    );
    let ctx = TestContext::new(vec![svc_node.clone()], &[]);
    let reference = UnresolvedRef {
        from_node_id: "class:src/users/users.controller.ts:UsersController:5".to_string(),
        reference_name: "UsersService".to_string(),
        reference_kind: EdgeKind::References,
        line: 6,
        column: 4,
        file_path: "src/users/users.controller.ts".to_string(),
        language: Language::Typescript,
        candidates: None,
    };
    let result = NestjsResolver.resolve(&reference, &ctx).unwrap();
    assert_eq!(result.target_node_id, svc_node.id);
    assert_eq!(result.resolved_by, ResolvedBy::Framework);
    assert!(result.confidence >= 0.85);
}

#[test]
fn nestjs_returns_none_for_name_without_provider_suffix() {
    let ctx = TestContext::new(vec![], &[]);
    let reference = UnresolvedRef {
        from_node_id: "x".to_string(),
        reference_name: "doThing".to_string(),
        reference_kind: EdgeKind::References,
        line: 1,
        column: 1,
        file_path: "a.ts".to_string(),
        language: Language::Typescript,
        candidates: None,
    };
    assert!(NestjsResolver.resolve(&reference, &ctx).is_none());
}

// ─── nestjsResolver.postExtract — RouterModule ──────────────────────────────

fn mk_class(name: &str, file_path: &str, start_line: u32, end_line: u32) -> Node {
    mk_node(
        &format!("class:{file_path}:{start_line}:{name}"),
        NodeKind::Class,
        name,
        &format!("{file_path}::{name}"),
        file_path,
        Language::Typescript,
        start_line,
        end_line,
    )
}

fn mk_route(
    file_path: &str,
    line: u32,
    method: &str,
    path: &str,
    name_override: Option<&str>,
) -> Node {
    let mut node = mk_node(
        &format!("route:{file_path}:{line}:{method}:{path}"),
        NodeKind::Route,
        name_override.unwrap_or(&format!("{method} {path}")),
        &format!("{file_path}::{method}:{path}"),
        file_path,
        Language::Typescript,
        line,
        line,
    );
    node.updated_at = 0;
    node
}

#[test]
fn nestjs_post_extract_prepends_router_module_prefix_top_level_register() {
    let ctx = TestContext::new(
        vec![
            mk_class("AdminController", "src/admin/admin.controller.ts", 1, 10),
            mk_route("src/admin/admin.controller.ts", 3, "GET", "/", None),
        ],
        &[(
            "src/app.module.ts",
            r#"
          @Module({
            imports: [
              RouterModule.register([
                { path: 'admin', module: AdminModule },
              ]),
            ],
          })
          export class AppModule {}

          @Module({ controllers: [AdminController] })
          export class AdminModule {}
        "#,
        )],
    );

    let updates = NestjsResolver.post_extract(&ctx).unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].name, "GET /admin");
    // id and qualifiedName must be preserved so existing route→handler edges
    // stay intact and the pass remains idempotent on a second run.
    assert_eq!(updates[0].id, "route:src/admin/admin.controller.ts:3:GET:/");
    assert_eq!(
        updates[0].qualified_name,
        "src/admin/admin.controller.ts::GET:/"
    );
}

#[test]
fn nestjs_post_extract_resolves_nested_children_issue_459() {
    let ctx = TestContext::new(
        vec![
            mk_class("UsersController", "src/users/users.controller.ts", 1, 10),
            mk_route("src/users/users.controller.ts", 3, "GET", "/", None),
        ],
        &[
            (
                "src/app.module.ts",
                r#"
          @Module({
            imports: [
              AdminModule,
              UsersModule,
              RouterModule.register([
                {
                  path: 'admin',
                  module: AdminModule,
                  children: [
                    { path: 'users', module: UsersModule },
                  ],
                },
              ]),
            ],
          })
          export class AppModule {}
        "#,
            ),
            (
                "src/users/users.module.ts",
                r#"
          @Module({ controllers: [UsersController] })
          export class UsersModule {}
        "#,
            ),
        ],
    );

    let updates = NestjsResolver.post_extract(&ctx).unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].name, "GET /admin/users");
}

#[test]
fn nestjs_post_extract_joins_module_prefix_with_controller_path_and_params() {
    let ctx = TestContext::new(
        vec![
            mk_class("UsersController", "src/users.controller.ts", 1, 10),
            // Existing extract emitted GET /users/:id from @Controller('users') + @Get(':id')
            mk_route("src/users.controller.ts", 3, "GET", "/users/:id", None),
        ],
        &[(
            "src/app.module.ts",
            r#"
          RouterModule.register([{ path: 'admin', module: UsersModule }])

          @Module({ controllers: [UsersController] })
          export class UsersModule {}
        "#,
        )],
    );

    let updates = NestjsResolver.post_extract(&ctx).unwrap();
    assert_eq!(updates.len(), 1);
    assert_eq!(updates[0].name, "GET /admin/users/:id");
}

#[test]
fn nestjs_post_extract_is_idempotent() {
    // Simulate the state after one round of postExtract: name is already
    // 'GET /admin', but qualifiedName still encodes the original 'GET:/'.
    let ctx = TestContext::new(
        vec![
            mk_class("UsersController", "src/users.controller.ts", 1, 10),
            mk_route("src/users.controller.ts", 3, "GET", "/", Some("GET /admin")),
        ],
        &[(
            "src/app.module.ts",
            r#"
          RouterModule.register([{ path: 'admin', module: UsersModule }])
          @Module({ controllers: [UsersController] })
          export class UsersModule {}
        "#,
        )],
    );

    let updates = NestjsResolver.post_extract(&ctx).unwrap();
    assert_eq!(updates.len(), 0);
}

#[test]
fn nestjs_post_extract_is_noop_without_router_module() {
    let ctx = TestContext::new(
        vec![
            mk_class("UsersController", "src/users.controller.ts", 1, 10),
            mk_route("src/users.controller.ts", 3, "GET", "/", None),
        ],
        &[(
            "src/app.module.ts",
            r#"
          @Module({ controllers: [UsersController] })
          export class AppModule {}
        "#,
        )],
    );

    let updates = NestjsResolver.post_extract(&ctx).unwrap();
    assert_eq!(updates.len(), 0);
}

#[test]
fn nestjs_post_extract_attributes_routes_to_right_controller_in_one_file() {
    // Two controllers in one file, declared in two different modules with
    // two different module prefixes. The route's startLine has to match the
    // class scope, not just the file path.
    let ctx = TestContext::new(
        vec![
            mk_class("AController", "src/multi.controller.ts", 1, 5),
            mk_class("BController", "src/multi.controller.ts", 7, 12),
            mk_route("src/multi.controller.ts", 3, "GET", "/a/x", None),
            mk_route("src/multi.controller.ts", 9, "GET", "/b/y", None),
        ],
        &[(
            "src/app.module.ts",
            r#"
          RouterModule.register([
            { path: 'p1', module: AModule },
            { path: 'p2', module: BModule },
          ])
          @Module({ controllers: [AController] }) export class AModule {}
          @Module({ controllers: [BController] }) export class BModule {}
        "#,
        )],
    );

    let updates = NestjsResolver.post_extract(&ctx).unwrap();
    assert_eq!(updates.len(), 2);
    let by_id: HashMap<&str, &str> = updates
        .iter()
        .map(|u| (u.id.as_str(), u.name.as_str()))
        .collect();
    assert_eq!(
        by_id.get("route:src/multi.controller.ts:3:GET:/a/x"),
        Some(&"GET /p1/a/x")
    );
    assert_eq!(
        by_id.get("route:src/multi.controller.ts:9:GET:/b/y"),
        Some(&"GET /p2/b/y")
    );
}

#[test]
fn nestjs_post_extract_merges_registrations_across_module_files() {
    let ctx = TestContext::new(
        vec![
            mk_class("AController", "src/a.controller.ts", 1, 5),
            mk_class("BController", "src/b.controller.ts", 1, 5),
            mk_route("src/a.controller.ts", 3, "GET", "/", None),
            mk_route("src/b.controller.ts", 3, "GET", "/", None),
        ],
        &[
            (
                "src/app.module.ts",
                r#"
          RouterModule.register([{ path: 'a', module: AModule }])
          @Module({ controllers: [AController] }) export class AModule {}
        "#,
            ),
            (
                "src/feature.module.ts",
                r#"
          RouterModule.forChild([{ path: 'b', module: BModule }])
          @Module({ controllers: [BController] }) export class BModule {}
        "#,
            ),
        ],
    );

    let updates = NestjsResolver.post_extract(&ctx).unwrap();
    assert_eq!(updates.len(), 2);
    let by_id: HashMap<&str, &str> = updates
        .iter()
        .map(|u| (u.id.as_str(), u.name.as_str()))
        .collect();
    assert_eq!(
        by_id.get("route:src/a.controller.ts:3:GET:/"),
        Some(&"GET /a")
    );
    assert_eq!(
        by_id.get("route:src/b.controller.ts:3:GET:/"),
        Some(&"GET /b")
    );
}

#[test]
fn nestjs_post_extract_silently_skips_controllers_missing_from_graph() {
    // RouterModule declares a prefix for a module, but the @Module that
    // would link it to a controller is missing — common during partial
    // re-extraction. Must not panic.
    let ctx = TestContext::new(
        vec![], // no class or route nodes
        &[(
            "src/app.module.ts",
            r#"
          RouterModule.register([{ path: 'orphans', module: GhostModule }])
          @Module({ controllers: [GhostController] }) export class GhostModule {}
        "#,
        )],
    );

    let updates = NestjsResolver.post_extract(&ctx).unwrap();
    assert_eq!(updates.len(), 0);
}

// ─── reactResolver.extract — React Router ───────────────────────────────────

#[test]
fn react_extracts_v6_route_with_element() {
    let src = r#"<Route path="/users" element={<UsersPage/>}/>"#;
    let result = ReactResolver.extract("App.tsx", src).unwrap();
    let route = result.nodes.iter().find(|n| n.kind == NodeKind::Route);
    assert_eq!(route.map(|n| n.name.as_str()), Some("/users"));
    assert_eq!(result.references[0].reference_name, "UsersPage");
}

#[test]
fn react_extracts_v5_route_with_component_attributes_in_any_order() {
    let src = r#"<Route exact path="/login" component={Login} />"#;
    let result = ReactResolver.extract("App.jsx", src).unwrap();
    let route = result.nodes.iter().find(|n| n.kind == NodeKind::Route);
    assert_eq!(route.map(|n| n.name.as_str()), Some("/login"));
    assert_eq!(result.references[0].reference_name, "Login");
}

#[test]
fn react_does_not_treat_routes_container_as_route() {
    let src = r#"<Routes><Route path="/x" element={<X/>}/></Routes>"#;
    let result = ReactResolver.extract("App.tsx", src).unwrap();
    let routes: Vec<&Node> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .collect();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].name, "/x");
}

#[test]
fn react_extracts_create_browser_router_object_routes() {
    let src = r#"const router = createBrowserRouter([
      { path: "/dashboard", element: <Dashboard /> },
      { path: "/login", Component: Login },
    ]);"#;
    let result = ReactResolver.extract("router.tsx", src).unwrap();
    let mut routes: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| n.name.as_str())
        .collect();
    routes.sort();
    assert_eq!(routes, vec!["/dashboard", "/login"]);
    let mut refs: Vec<&str> = result
        .references
        .iter()
        .map(|r| r.reference_name.as_str())
        .collect();
    refs.sort();
    assert_eq!(refs, vec!["Dashboard", "Login"]);
}

#[test]
fn react_does_not_treat_config_files_or_nextjs_pages_dir_as_routes() {
    let cfg = ReactResolver
        .extract("apps/nextjs-pages/next.config.mjs", "export default {}")
        .unwrap();
    assert_eq!(
        cfg.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .count(),
        0
    );
    let vite = ReactResolver
        .extract("src/pages/vite.config.ts", "export default {}")
        .unwrap();
    assert_eq!(
        vite.nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Route)
            .count(),
        0
    );
    // a real page still works
    let page = ReactResolver
        .extract(
            "src/pages/about.tsx",
            "export default function About(){return null}",
        )
        .unwrap();
    let names: Vec<&str> = page
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Route)
        .map(|n| n.name.as_str())
        .collect();
    assert_eq!(names, vec!["/about"]);
}

// ─── svelteResolver.extract (smoke) ─────────────────────────────────────────

#[test]
fn svelte_extract_returns_nodes_and_references_shape() {
    let result = SvelteResolver.extract("+page.svelte", "").unwrap();
    // TS asserts the result has `nodes` and `references` properties.
    assert!(result.nodes.is_empty() || !result.nodes.is_empty());
    assert!(result.references.is_empty());
}

// ─── vueResolver (smoke parity with svelte) ─────────────────────────────────

#[test]
fn vue_extracts_nuxt_page_route_and_api_route() {
    let page = VueResolver
        .extract("src/pages/blog/[slug].vue", "")
        .unwrap();
    let names: Vec<&str> = page.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["/blog/:slug"]);
    assert_eq!(page.nodes[0].kind, NodeKind::Route);
    assert_eq!(page.nodes[0].language, Language::Vue);

    let api = VueResolver
        .extract("src/server/api/users/index.ts", "")
        .unwrap();
    let names: Vec<&str> = api.nodes.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(names, vec!["/api/users"]);
}

// ─── Expo Modules framework extractor ───────────────────────────────────────

#[test]
fn expo_extracts_async_function_function_property_literals_as_method_nodes() {
    let source = r#"
import ExpoModulesCore

public class HapticsModule: Module {
  public func definition() -> ModuleDefinition {
    Name("ExpoHaptics")

    AsyncFunction("notificationAsync") { (notificationType: NotificationType) in
      // body
    }

    AsyncFunction("impactAsync") { (style: ImpactStyle) in
      // body
    }

    Function("synchronousThing") {
      return 1
    }

    Property("isAvailable") {
      return true
    }
  }
}
"#;
    let result = ExpoModulesResolver
        .extract("ios/HapticsModule.swift", source)
        .unwrap();
    let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    for expected in [
        "notificationAsync",
        "impactAsync",
        "synchronousThing",
        "isAvailable",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
    assert!(result.nodes.iter().all(|n| n.kind == NodeKind::Method));
    assert!(
        result
            .nodes
            .iter()
            .all(|n| n.qualified_name.contains("ExpoHaptics."))
    );
}

#[test]
fn expo_falls_back_to_class_name_without_name_literal() {
    let source = r#"
public class BareModule: Module {
  public func definition() -> ModuleDefinition {
    Function("doX") { return 1 }
  }
}
"#;
    let result = ExpoModulesResolver
        .extract("ios/BareModule.swift", source)
        .unwrap();
    // BareModule is used as the qualifier since there's no Name() literal.
    assert!(result.nodes[0].qualified_name.contains("BareModule.doX"));
}

#[test]
fn expo_returns_no_nodes_for_non_expo_swift_file() {
    let source = "\nclass Helper {\n  func doX() { }\n}\n";
    let result = ExpoModulesResolver.extract("Helper.swift", source).unwrap();
    assert_eq!(result.nodes.len(), 0);
}

#[test]
fn expo_also_extracts_from_kotlin_module_files() {
    let source = r#"
class FooModule : Module() {
    override fun definition() = ModuleDefinition {
        Name("ExpoFoo")
        AsyncFunction("doAsync") { name: String -> name.uppercase() }
        Function("doSync") { 42 }
    }
}
"#;
    let result = ExpoModulesResolver.extract("FooModule.kt", source).unwrap();
    assert_eq!(result.nodes.len(), 2);
    let mut names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["doAsync", "doSync"]);
    assert!(result.nodes.iter().all(|n| n.language == Language::Kotlin));
}

// ─── Fabric view component extractor ────────────────────────────────────────

#[test]
fn fabric_extracts_component_and_prop_nodes_from_native_spec() {
    let source = r#"
'use client';
import { codegenNativeComponent } from 'react-native';
import type { ViewProps, CodegenTypes as CT, ColorValue } from 'react-native';

type TapEvent = Readonly<{ x: number; y: number }>;

export interface NativeProps extends ViewProps {
  color?: ColorValue;
  onTap?: CT.DirectEventHandler<TapEvent>;
  caption?: string;
}

export default codegenNativeComponent<NativeProps>('MyView', {});
"#;
    let result = FabricViewResolver
        .extract("src/MyViewNativeComponent.ts", source)
        .unwrap();
    let component_nodes: Vec<&Node> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Component)
        .collect();
    let mut prop_names: Vec<&str> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property)
        .map(|n| n.name.as_str())
        .collect();
    prop_names.sort();
    assert_eq!(component_nodes.len(), 1);
    assert_eq!(component_nodes[0].name, "MyView");
    assert_eq!(prop_names, vec!["caption", "color", "onTap"]);
}

#[test]
fn fabric_returns_nothing_without_codegen_native_component() {
    let source = "export const x = 1;";
    let result = FabricViewResolver.extract("plain.ts", source).unwrap();
    assert_eq!(result.nodes.len(), 0);
}

#[test]
fn fabric_handles_spec_with_no_native_props_interface() {
    let source = r#"
import { codegenNativeComponent } from 'react-native';
export default codegenNativeComponent('BareComponent');
"#;
    let result = FabricViewResolver.extract("Bare.ts", source).unwrap();
    // Component node exists; no prop nodes.
    let components: Vec<&Node> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Component)
        .collect();
    let props: Vec<&Node> = result
        .nodes
        .iter()
        .filter(|n| n.kind == NodeKind::Property)
        .collect();
    assert_eq!(components.len(), 1);
    assert_eq!(components[0].name, "BareComponent");
    assert_eq!(props.len(), 0);
}

// ─── React Native bridge resolver — detect() ────────────────────────────────

#[test]
fn rn_detect_true_when_package_json_declares_react_native() {
    let ctx = TestContext::new(
        vec![],
        &[(
            "package.json",
            r#"{"name":"x","dependencies":{"react-native":"^0.73.0"}}"#,
        )],
    );
    assert!(ReactNativeBridgeResolver::new().detect(&ctx));
}

#[test]
fn rn_detect_true_when_objc_file_uses_rct_export_module() {
    let ctx = TestContext::new(
        vec![],
        &[(
            "NativeFoo.mm",
            "@implementation Foo\nRCT_EXPORT_MODULE()\n@end",
        )],
    );
    assert!(ReactNativeBridgeResolver::new().detect(&ctx));
}

#[test]
fn rn_detect_true_when_ts_file_uses_turbo_module_registry() {
    let ctx = TestContext::new(
        vec![],
        &[(
            "NativeFoo.ts",
            "import { TurboModuleRegistry } from 'react-native';\nexport default TurboModuleRegistry.getEnforcing<Spec>('Foo');",
        )],
    );
    assert!(ReactNativeBridgeResolver::new().detect(&ctx));
}

#[test]
fn rn_detect_false_when_no_rn_signals_present() {
    let ctx = TestContext::new(vec![mk_method("hi", Language::Objc, "X.m")], &[]);
    assert!(!ReactNativeBridgeResolver::new().detect(&ctx));
}

// ─── React Native bridge resolver — legacy bridge, ObjC side ────────────────

#[test]
fn rn_resolves_js_callsite_via_rct_export_method_with_default_module_name() {
    // RCTGeolocation → module name 'Geolocation' (RCT prefix stripped).
    let native = mk_method("getCurrentPosition:", Language::Objc, "RCTGeolocation.m");
    let ctx = TestContext::new(
        vec![native.clone()],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "RCTGeolocation.m",
                "@implementation RCTGeolocation\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(getCurrentPosition:(RCTResponseSenderBlock)cb) {}\n@end",
            ),
        ],
    );
    let result = ReactNativeBridgeResolver::new()
        .resolve(
            &mk_ref("getCurrentPosition", Language::Javascript, "App.js"),
            &ctx,
        )
        .unwrap();
    assert_eq!(result.target_node_id, native.id);
    assert_eq!(result.resolved_by, ResolvedBy::Framework);
}

#[test]
fn rn_resolves_via_explicit_module_name_in_rct_export_module() {
    let native = mk_method("startScan:", Language::Objc, "Bluetooth.m");
    let ctx = TestContext::new(
        vec![native.clone()],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "Bluetooth.m",
                "@implementation BluetoothImpl\nRCT_EXPORT_MODULE(BluetoothManager)\nRCT_EXPORT_METHOD(startScan:(RCTResponseSenderBlock)cb) {}\n@end",
            ),
        ],
    );
    let result = ReactNativeBridgeResolver::new()
        .resolve(&mk_ref("startScan", Language::Javascript, "App.js"), &ctx)
        .unwrap();
    assert_eq!(result.target_node_id, native.id);
}

#[test]
fn rn_resolves_rct_remap_method_with_js_name_override() {
    let native = mk_method("doInternalCompute:", Language::Objc, "Computer.m");
    let ctx = TestContext::new(
        vec![native.clone()],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "Computer.m",
                "@implementation Computer\nRCT_EXPORT_MODULE()\nRCT_REMAP_METHOD(compute, doInternalCompute:(NSDictionary *)opts) {}\n@end",
            ),
        ],
    );
    let result = ReactNativeBridgeResolver::new()
        .resolve(&mk_ref("compute", Language::Javascript, "App.js"), &ctx)
        .unwrap();
    assert_eq!(result.target_node_id, native.id);
}

// ─── React Native bridge resolver — legacy bridge, Java side ────────────────

#[test]
fn rn_resolves_react_method_with_get_name_literal() {
    let native = mk_method(
        "getCurrentPosition",
        Language::Java,
        "GeolocationModule.java",
    );
    let ctx = TestContext::new(
        vec![native.clone()],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "GeolocationModule.java",
                "class GeolocationModule extends ReactContextBaseJavaModule {\n  @Override public String getName() { return \"Geolocation\"; }\n  @ReactMethod public void getCurrentPosition(Callback cb) {}\n}",
            ),
        ],
    );
    let result = ReactNativeBridgeResolver::new()
        .resolve(
            &mk_ref("getCurrentPosition", Language::Javascript, "App.js"),
            &ctx,
        )
        .unwrap();
    assert_eq!(result.target_node_id, native.id);
}

#[test]
fn rn_resolves_kotlin_react_method_fun() {
    let native = mk_method("startScan", Language::Kotlin, "BluetoothModule.kt");
    let ctx = TestContext::new(
        vec![native.clone()],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "BluetoothModule.kt",
                "class BluetoothModule(ctx: ReactApplicationContext) : ReactContextBaseJavaModule(ctx) {\n  override fun getName(): String = \"BluetoothManager\"\n  @ReactMethod fun startScan(cb: Callback) {}\n}",
            ),
        ],
    );
    let result = ReactNativeBridgeResolver::new()
        .resolve(&mk_ref("startScan", Language::Javascript, "App.js"), &ctx)
        .unwrap();
    assert_eq!(result.target_node_id, native.id);
}

// ─── React Native bridge resolver — TurboModule spec resolution ─────────────

#[test]
fn rn_matches_spec_method_to_native_objc_implementation_by_name() {
    // The Spec interface lists `getTotalLength`; ObjC has a method by the
    // same first keyword. Bridge matches by name.
    let native = mk_method(
        "getTotalLength:",
        Language::Objc,
        "RNSVGRenderableManager.mm",
    );
    let ctx = TestContext::new(
        vec![native.clone()],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "NativeSvgRenderableModule.ts",
                "import { TurboModuleRegistry } from 'react-native';\nexport interface Spec extends TurboModule {\n  getTotalLength(tag: number): number;\n  isPointInFill(tag: number, options?: object): boolean;\n}\nexport default TurboModuleRegistry.getEnforcing<Spec>('RNSVGRenderableModule');",
            ),
        ],
    );
    let result = ReactNativeBridgeResolver::new()
        .resolve(
            &mk_ref("getTotalLength", Language::Tsx, "SvgComponent.tsx"),
            &ctx,
        )
        .unwrap();
    assert_eq!(result.target_node_id, native.id);
}

#[test]
fn rn_returns_none_when_spec_method_has_no_matching_native_impl() {
    let ctx = TestContext::new(
        vec![],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "NativeFoo.ts",
                "import { TurboModuleRegistry } from 'react-native';\nexport interface Spec extends TurboModule {\n  thingThatDoesntExist(): void;\n}\nexport default TurboModuleRegistry.getEnforcing<Spec>('Foo');",
            ),
        ],
    );
    let result = ReactNativeBridgeResolver::new().resolve(
        &mk_ref("thingThatDoesntExist", Language::Tsx, "Caller.tsx"),
        &ctx,
    );
    assert!(result.is_none());
}

// ─── React Native bridge resolver — qualified vs bare callsite names ────────

#[test]
fn rn_handles_bare_method_name_post_receiver_strip() {
    let native = mk_method("compute:", Language::Objc, "Mod.m");
    let ctx = TestContext::new(
        vec![native],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "Mod.m",
                "@implementation Mod\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(compute:(NSDictionary *)x) {}\n@end",
            ),
        ],
    );
    assert!(
        ReactNativeBridgeResolver::new()
            .resolve(&mk_ref("compute", Language::Javascript, "App.js"), &ctx)
            .is_some()
    );
}

#[test]
fn rn_strips_dot_prefix_on_receiver_qualified_callsite() {
    let native = mk_method("compute:", Language::Objc, "Mod.m");
    let ctx = TestContext::new(
        vec![native],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "Mod.m",
                "@implementation Mod\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(compute:(NSDictionary *)x) {}\n@end",
            ),
        ],
    );
    assert!(
        ReactNativeBridgeResolver::new()
            .resolve(
                &mk_ref("NativeModules.Mod.compute", Language::Javascript, "App.js"),
                &ctx
            )
            .is_some()
    );
}

#[test]
fn rn_does_not_resolve_native_language_callers() {
    let native = mk_method("compute:", Language::Objc, "Mod.m");
    let ctx = TestContext::new(vec![native], &[]);
    assert!(
        ReactNativeBridgeResolver::new()
            .resolve(&mk_ref("compute", Language::Objc, "OtherMod.m"), &ctx)
            .is_none()
    );
}

// ─── React Native bridge resolver — RCTEventEmitter built-ins blocklist ─────

#[test]
fn rn_skips_add_listener_and_remove_emitter_builtins() {
    // A repo with RCTEventEmitter subclass: defines `addListener:` and
    // `remove:` because that's what `[RCTEventEmitter addListener:]`
    // requires. JS callers of `.addListener(...)` should NOT resolve
    // here — they're hitting the JS-side `NativeEventEmitter`
    // abstraction, not the native emitter directly.
    let native1 = mk_method("addListener:", Language::Objc, "EventEmitter.m");
    let native2 = mk_method("remove:", Language::Objc, "EventEmitter.m");
    let ctx = TestContext::new(
        vec![native1, native2],
        &[
            (
                "package.json",
                r#"{"dependencies":{"react-native":"^0.73"}}"#,
            ),
            (
                "EventEmitter.m",
                "@implementation EventEmitter\nRCT_EXPORT_MODULE()\nRCT_EXPORT_METHOD(addListener:(NSString *)eventName) {}\nRCT_EXPORT_METHOD(remove:(double)id) {}\n@end",
            ),
        ],
    );
    assert!(
        ReactNativeBridgeResolver::new()
            .resolve(&mk_ref("addListener", Language::Javascript, "App.js"), &ctx)
            .is_none()
    );
    assert!(
        ReactNativeBridgeResolver::new()
            .resolve(&mk_ref("remove", Language::Typescript, "App.ts"), &ctx)
            .is_none()
    );
}

// ─── Framework detection (resolution.test.ts subset for my resolvers) ───────

#[test]
fn express_detect_via_package_json() {
    let ctx = TestContext::new(
        vec![],
        &[
            ("package.json", r#"{"dependencies":{"express":"^4.18.0"}}"#),
            ("src/app.js", ""),
        ],
    );
    assert!(ExpressResolver.detect(&ctx));
}

#[test]
fn react_detect_via_package_json() {
    let ctx = TestContext::new(
        vec![],
        &[
            ("package.json", r#"{"dependencies":{"react":"^18.0.0"}}"#),
            ("src/App.tsx", ""),
        ],
    );
    assert!(ReactResolver.detect(&ctx));
}

#[test]
fn react_resolves_component_references() {
    let mock = mk_node(
        "component:src/Button.tsx:Button:5",
        NodeKind::Component,
        "Button",
        "src/Button.tsx::Button",
        "src/Button.tsx",
        Language::Tsx,
        5,
        20,
    );
    let ctx = TestContext::new(
        vec![mock],
        &[("package.json", r#"{"dependencies":{"react":"^18.0.0"}}"#)],
    );
    let mut reference = mk_ref("Button", Language::Typescript, "src/App.tsx");
    reference.reference_kind = EdgeKind::References;
    let result = ReactResolver.resolve(&reference, &ctx).unwrap();
    assert_eq!(result.target_node_id, "component:src/Button.tsx:Button:5");
}

#[test]
fn react_resolves_custom_hook_references() {
    let mock = mk_node(
        "hook:src/hooks/useAuth.ts:useAuth:1",
        NodeKind::Function,
        "useAuth",
        "src/hooks/useAuth.ts::useAuth",
        "src/hooks/useAuth.ts",
        Language::Typescript,
        1,
        20,
    );
    let ctx = TestContext::new(
        vec![mock],
        &[("package.json", r#"{"dependencies":{"react":"^18.0.0"}}"#)],
    );
    let reference = mk_ref("useAuth", Language::Typescript, "src/App.tsx");
    let result = ReactResolver.resolve(&reference, &ctx).unwrap();
    assert_eq!(result.target_node_id, "hook:src/hooks/useAuth.ts:useAuth:1");
}

// ─── Svelte resolve patterns (unit coverage for the ported logic) ───────────

#[test]
fn svelte_resolves_runes_to_from_node() {
    let ctx = TestContext::new(vec![], &[]);
    let reference = mk_ref("$state", Language::Svelte, "App.svelte");
    let result = SvelteResolver.resolve(&reference, &ctx).unwrap();
    assert_eq!(result.target_node_id, reference.from_node_id);
    assert_eq!(result.confidence, 1.0);
    assert_eq!(result.resolved_by, ResolvedBy::Framework);
}

#[test]
fn svelte_resolves_store_auto_subscription_to_store_variable() {
    let store = mk_node(
        "var:src/stores.ts:count:1",
        NodeKind::Variable,
        "count",
        "src/stores.ts::count",
        "src/stores.ts",
        Language::Typescript,
        1,
        1,
    );
    let ctx = TestContext::new(vec![store.clone()], &[]);
    let reference = mk_ref("$count", Language::Svelte, "App.svelte");
    let result = SvelteResolver.resolve(&reference, &ctx).unwrap();
    assert_eq!(result.target_node_id, store.id);
    assert_eq!(result.confidence, 0.85);
}

#[test]
fn svelte_sveltekit_route_extraction() {
    let result = SvelteResolver
        .extract("src/routes/blog/[slug]/+page.svelte", "")
        .unwrap();
    assert_eq!(result.nodes.len(), 1);
    assert_eq!(result.nodes[0].name, "/blog/:slug");
    assert_eq!(result.nodes[0].kind, NodeKind::Route);
    assert_eq!(result.nodes[0].language, Language::Svelte);
}

#[test]
fn vue_resolves_compiler_macros_and_nuxt_auto_imports() {
    let ctx = TestContext::new(vec![], &[]);
    let macro_ref = mk_ref("defineProps", Language::Vue, "App.vue");
    let result = VueResolver.resolve(&macro_ref, &ctx).unwrap();
    assert_eq!(result.target_node_id, macro_ref.from_node_id);
    assert_eq!(result.confidence, 1.0);

    let nuxt_ref = mk_ref("useFetch", Language::Vue, "App.vue");
    let result = VueResolver.resolve(&nuxt_ref, &ctx).unwrap();
    assert_eq!(result.target_node_id, nuxt_ref.from_node_id);
}
