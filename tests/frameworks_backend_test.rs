//! Backend-ecosystem framework resolver tests.
//!
//! Ports the backend frameworks' cases from:
//! - `__tests__/frameworks.test.ts` — djangoResolver / flaskResolver /
//!   fastapiResolver / laravelResolver / railsResolver / springResolver /
//!   playResolver / aspnetResolver `.extract` suites, the Play routes-file
//!   detection suite, and the "framework extractors ignore commented-out
//!   routes" regressions for these frameworks.
//! - `__tests__/drupal.test.ts` — detect / claimsReference / extract
//!   (routing.yml + hooks) / resolve.
//!
//! Pipeline-dependent end-to-end cases (CodeGraph.initSync + indexAll —
//! Django/Flask/Drupal end-to-end, the Java Spring integration suites in
//! `__tests__/frameworks-integration.test.ts`) are deferred to the stitch
//! agent; see `rust/notes/frameworks-backend.md`.

use std::collections::HashMap;

use codegraph::extraction::grammars::{is_play_routes_file, is_source_file};
use codegraph::resolution::frameworks::csharp::AspnetResolver;
use codegraph::resolution::frameworks::drupal::DrupalResolver;
use codegraph::resolution::frameworks::java::SpringResolver;
use codegraph::resolution::frameworks::laravel::LaravelResolver;
use codegraph::resolution::frameworks::play::PlayResolver;
use codegraph::resolution::frameworks::python::{DjangoResolver, FastapiResolver, FlaskResolver};
use codegraph::resolution::frameworks::ruby::RailsResolver;
use codegraph::resolution::types::{
    FrameworkResolver,
    ImportMapping,
    ResolutionContext,
    UnresolvedRef,
};
use codegraph::types::{EdgeKind, Language, Node, NodeKind};

// ---------------------------------------------------------------------------
// Fixture context (TS `makeContext` in drupal.test.ts)
// ---------------------------------------------------------------------------

#[derive(Default)]
struct FixtureContext {
    /// path → content; backs both `read_file` and `file_exists`.
    files: HashMap<String, String>,
    all_files: Vec<String>,
    nodes: Vec<Node>,
}

impl FixtureContext {
    fn with_file(mut self, path: &str, content: &str) -> Self {
        self.files.insert(path.to_string(), content.to_string());
        self
    }

    fn with_all_files(mut self, files: &[&str]) -> Self {
        self.all_files = files.iter().map(|f| f.to_string()).collect();
        self
    }

    fn with_nodes(mut self, nodes: Vec<Node>) -> Self {
        self.nodes = nodes;
        self
    }
}

impl ResolutionContext for FixtureContext {
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
    fn get_nodes_by_qualified_name(&self, qualified_name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.qualified_name == qualified_name)
            .cloned()
            .collect()
    }
    fn get_nodes_by_kind(&self, kind: NodeKind) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.kind == kind)
            .cloned()
            .collect()
    }
    fn file_exists(&self, file_path: &str) -> bool {
        self.files.contains_key(file_path)
    }
    fn read_file(&self, file_path: &str) -> Option<String> {
        self.files.get(file_path).cloned()
    }
    fn get_project_root(&self) -> &str {
        "/project"
    }
    fn get_all_files(&self) -> Vec<String> {
        self.all_files.clone()
    }
    fn get_nodes_by_lower_name(&self, lower_name: &str) -> Vec<Node> {
        self.nodes
            .iter()
            .filter(|n| n.name.to_lowercase() == lower_name)
            .cloned()
            .collect()
    }
    fn get_import_mappings(&self, _file_path: &str, _language: Language) -> Vec<ImportMapping> {
        Vec::new()
    }
}

fn make_node(
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

fn make_ref(name: &str, file_path: &str, language: Language) -> UnresolvedRef {
    UnresolvedRef {
        from_node_id: "route:x".to_string(),
        reference_name: name.to_string(),
        reference_kind: EdgeKind::References,
        line: 1,
        column: 0,
        file_path: file_path.to_string(),
        language,
        candidates: None,
        metadata: None,
    }
}

fn names(nodes: &[Node]) -> Vec<&str> {
    nodes.iter().map(|n| n.name.as_str()).collect()
}

fn ref_names(refs: &[UnresolvedRef]) -> Vec<&str> {
    refs.iter().map(|r| r.reference_name.as_str()).collect()
}

// ---------------------------------------------------------------------------
// djangoResolver.extract
// ---------------------------------------------------------------------------

#[test]
fn django_extracts_route_node_and_reference_for_path_with_cbv_as_view() {
    let src = "
from django.urls import path
from users.views import UserListView

urlpatterns = [
    path('users/', UserListView.as_view(), name='user-list'),
]
";
    let result = DjangoResolver.extract("users/urls.py", src).unwrap();
    assert_eq!(result.nodes.len(), 1);
    assert_eq!(result.nodes[0].kind, NodeKind::Route);
    assert_eq!(result.nodes[0].name, "users/");
    assert_eq!(result.references.len(), 1);
    assert_eq!(result.references[0].reference_name, "UserListView");
    assert_eq!(result.references[0].reference_kind, EdgeKind::References);
    assert_eq!(result.references[0].from_node_id, result.nodes[0].id);
}

#[test]
fn django_extracts_route_for_path_with_dotted_module_class_as_view() {
    let src = "from django.urls import path\nfrom api.v1 import views as api_v1_views\nurlpatterns = [path('api/', api_v1_views.UserListView.as_view())]\n";
    let result = DjangoResolver.extract("api/urls.py", src).unwrap();
    assert_eq!(result.nodes.len(), 1);
    assert_eq!(result.references[0].reference_name, "UserListView");
}

#[test]
fn django_extracts_route_for_path_with_bare_function_view() {
    let src =
        "from django.urls import path\nurlpatterns = [path('home/', home_view, name='home')]\n";
    let result = DjangoResolver.extract("home/urls.py", src).unwrap();
    assert_eq!(result.references[0].reference_name, "home_view");
}

#[test]
fn django_extracts_route_for_path_with_include() {
    let src = "from django.urls import path, include\nurlpatterns = [path('api/', include('api.urls'))]\n";
    let result = DjangoResolver.extract("root/urls.py", src).unwrap();
    assert_eq!(result.nodes.len(), 1);
    assert_eq!(result.nodes[0].kind, NodeKind::Route);
    assert_eq!(result.references[0].reference_name, "api.urls");
    assert_eq!(result.references[0].reference_kind, EdgeKind::Imports);
}

#[test]
fn django_extracts_routes_for_re_path_and_url() {
    let src = "from django.urls import re_path, url\nurlpatterns = [re_path(r'^users/$', UserView), url(r'^old/$', OldView)]\n";
    let result = DjangoResolver.extract("legacy/urls.py", src).unwrap();
    assert_eq!(result.nodes.len(), 2);
    assert_eq!(names(&result.nodes), vec!["^users/$", "^old/$"]);
}

#[test]
fn django_returns_empty_result_for_a_non_urls_py_python_file() {
    let src = "def foo(): return 1\n";
    let result = DjangoResolver.extract("views.py", src).unwrap();
    assert!(result.nodes.is_empty());
    assert!(result.references.is_empty());
}

// ---------------------------------------------------------------------------
// flaskResolver.extract
// ---------------------------------------------------------------------------

#[test]
fn flask_extracts_route_and_reference_from_app_route() {
    let src = "
@app.route('/users')
def list_users():
    return []
";
    let result = FlaskResolver.extract("app.py", src).unwrap();
    assert_eq!(result.nodes.len(), 1);
    assert_eq!(result.nodes[0].kind, NodeKind::Route);
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "list_users");
}

#[test]
fn flask_extracts_blueprint_routes() {
    let src = "
@users_bp.route('/<id>', methods=['POST'])
def create_user(id):
    pass
";
    let result = FlaskResolver.extract("routes.py", src).unwrap();
    assert_eq!(result.nodes[0].name, "POST /<id>");
    assert_eq!(result.references[0].reference_name, "create_user");
}

#[test]
fn flask_resolves_the_handler_across_an_intervening_decorator() {
    let src = "
@bp.route('/profile')
@login_required
def profile():
    return render_template('profile.html')
";
    let result = FlaskResolver.extract("routes.py", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /profile");
    assert_eq!(result.references[0].reference_name, "profile");
}

#[test]
fn flask_extracts_stacked_route_decorators_bound_to_one_view() {
    let src = "
@bp.route('/', methods=['GET', 'POST'])
@bp.route('/index', methods=['GET', 'POST'])
@login_required
def index():
    return render_template('index.html')
";
    let result = FlaskResolver.extract("routes.py", src).unwrap();
    assert_eq!(names(&result.nodes), vec!["GET /", "GET /index"]);
    assert_eq!(ref_names(&result.references), vec!["index", "index"]);
}

#[test]
fn flask_extracts_the_method_from_a_tuple_methods() {
    let src = "
@blueprint.route('/api/articles', methods=('POST',))
def make_article():
    pass
";
    let result = FlaskResolver.extract("views.py", src).unwrap();
    assert_eq!(result.nodes[0].name, "POST /api/articles");
    assert_eq!(result.references[0].reference_name, "make_article");
}

#[test]
fn flask_extracts_flask_restful_add_resource_to_the_resource_class() {
    let src = "
api.add_resource(TodoResource, '/todos/<id>')
api.add_org_resource(AlertResource, '/api/alerts/<id>', endpoint='alert')
";
    let result = FlaskResolver.extract("api.py", src).unwrap();
    assert_eq!(
        names(&result.nodes),
        vec!["ANY /todos/<id>", "ANY /api/alerts/<id>"]
    );
    assert_eq!(
        ref_names(&result.references),
        vec!["TodoResource", "AlertResource"]
    );
}

// ---------------------------------------------------------------------------
// fastapiResolver.extract
// ---------------------------------------------------------------------------

#[test]
fn fastapi_extracts_route_and_reference_from_app_get() {
    let src = "
@app.get('/users')
async def list_users():
    return []
";
    let result = FastapiResolver.extract("main.py", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "list_users");
}

#[test]
fn fastapi_extracts_route_from_router_post() {
    let src = "
@router.post('/items')
def create_item(item: Item):
    pass
";
    let result = FastapiResolver.extract("items.py", src).unwrap();
    assert_eq!(result.nodes[0].name, "POST /items");
    assert_eq!(result.references[0].reference_name, "create_item");
}

#[test]
fn fastapi_extracts_a_route_mounted_at_the_router_prefix_root() {
    let src = "
@router.get(\"\", response_model=ListOfArticles, name=\"articles:list\")
async def list_articles():
    return []
";
    let result = FastapiResolver.extract("articles.py", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /");
    assert_eq!(result.references[0].reference_name, "list_articles");
}

#[test]
fn fastapi_extracts_a_multi_line_decorator_with_an_empty_path() {
    let src = "
@router.post(
    \"\",
    status_code=201,
    response_model=ArticleInResponse,
)
async def create_article():
    pass
";
    let result = FastapiResolver.extract("articles.py", src).unwrap();
    assert_eq!(result.nodes[0].name, "POST /");
    assert_eq!(result.references[0].reference_name, "create_article");
}

// ---------------------------------------------------------------------------
// laravelResolver.extract
// ---------------------------------------------------------------------------

#[test]
fn laravel_extracts_route_with_controller_tuple_syntax() {
    let src = "Route::get('/users', [UserController::class, 'index']);\n";
    let result = LaravelResolver.extract("routes/web.php", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "UserController@index");
}

#[test]
fn laravel_extracts_route_with_controller_at_action_syntax() {
    let src = "Route::post('/users', 'UserController@store');\n";
    let result = LaravelResolver.extract("routes/web.php", src).unwrap();
    assert_eq!(result.references[0].reference_name, "UserController@store");
}

#[test]
fn laravel_extracts_resource_route() {
    let src = "Route::resource('users', UserController::class);\n";
    let result = LaravelResolver.extract("routes/web.php", src).unwrap();
    assert_eq!(result.nodes[0].kind, NodeKind::Route);
    assert_eq!(result.references[0].reference_name, "UserController");
}

// ---------------------------------------------------------------------------
// railsResolver.extract
// ---------------------------------------------------------------------------

#[test]
fn rails_extracts_route_with_controller_action_syntax() {
    let src = "get '/users', to: 'users#index'\n";
    let result = RailsResolver.extract("config/routes.rb", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "users#index");
}

#[test]
fn rails_extracts_route_without_to_keyword() {
    let src = "post '/items' => 'items#create'\n";
    let result = RailsResolver.extract("config/routes.rb", src).unwrap();
    assert_eq!(result.references[0].reference_name, "items#create");
}

// ---------------------------------------------------------------------------
// springResolver.extract
// ---------------------------------------------------------------------------

#[test]
fn spring_extracts_route_with_get_mapping_and_next_method() {
    let src = "
@GetMapping(\"/users\")
public List<User> listUsers() {
  return users;
}
";
    let result = SpringResolver.extract("UserController.java", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "listUsers");
}

#[test]
fn spring_extracts_a_kotlin_get_mapping_with_a_fun_handler() {
    let src = "
@GetMapping(\"/vets\")
fun showVetList(model: MutableMap<String, Any>): String {
  return \"vets\"
}
";
    let result = SpringResolver.extract("VetController.kt", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /vets");
    assert_eq!(result.references[0].reference_name, "showVetList");
    assert_eq!(result.nodes[0].language, Language::Kotlin);
}

#[test]
fn spring_joins_a_kotlin_class_request_mapping_prefix_and_skips_a_stacked_annotation() {
    let src = "
@RestController
@RequestMapping(\"/owners\")
class OwnerController {
  @GetMapping(\"/{ownerId}\")
  @ResponseBody
  fun showOwner(@PathVariable ownerId: Int): String {
    return \"owner\"
  }
}
";
    let result = SpringResolver.extract("OwnerController.kt", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /owners/{ownerId}");
    assert_eq!(result.references[0].reference_name, "showOwner");
}

// ---------------------------------------------------------------------------
// playResolver.extract (conf/routes)
// ---------------------------------------------------------------------------

#[test]
fn play_extracts_method_path_controller_action_routes_dropping_the_package_and_args() {
    let src = "# Routes
GET     /                    controllers.Application.index
GET     /computers           controllers.Application.list(p: Int ?= 0, s: Int ?= 2)
POST    /computers           controllers.Application.save
-> /v1/posts                 v1.post.PostRouter
";
    let result = PlayResolver.extract("conf/routes", src).unwrap();
    // the `->` include is skipped
    assert_eq!(
        names(&result.nodes),
        vec!["GET /", "GET /computers", "POST /computers"]
    );
    assert_eq!(
        ref_names(&result.references),
        vec!["Application.index", "Application.list", "Application.save"]
    );
}

#[test]
fn play_only_runs_on_play_routes_files() {
    let result = PlayResolver
        .extract("app/Foo.scala", "GET / controllers.X.y")
        .unwrap();
    assert_eq!(result.nodes.len(), 0);
}

#[test]
fn play_routes_file_detection_recognizes_conf_routes_and_dot_routes() {
    assert!(is_play_routes_file("conf/routes"));
    assert!(is_play_routes_file("myapp/conf/routes"));
    assert!(is_play_routes_file("conf/admin.routes"));
    assert!(is_source_file("conf/routes"));
    assert!(!is_play_routes_file("src/routes.ts"));
}

// ---------------------------------------------------------------------------
// aspnetResolver.extract
// ---------------------------------------------------------------------------

#[test]
fn aspnet_extracts_route_from_http_get_attribute() {
    let src = "
[HttpGet(\"/users\")]
public IActionResult ListUsers()
{
  return Ok();
}
";
    let result = AspnetResolver.extract("UserController.cs", src).unwrap();
    assert_eq!(result.nodes[0].name, "GET /users");
    assert_eq!(result.references[0].reference_name, "ListUsers");
}

// ---------------------------------------------------------------------------
// framework extractors ignore commented-out routes
// (regression: phantom routes from comments/docstrings before strip-comments)
// ---------------------------------------------------------------------------

#[test]
fn django_skips_line_comment_and_docstring_routes() {
    let src = "
# urls.py example:
# path('/admin/', AdminPanel.as_view())
\"\"\"
Other routing example:
    path('/users/', UserListView.as_view())
\"\"\"
urlpatterns = [path('/real/', RealView.as_view())]
";
    let result = DjangoResolver.extract("app/urls.py", src).unwrap();
    assert_eq!(names(&result.nodes), vec!["/real/"]);
}

#[test]
fn flask_skips_commented_out_app_route() {
    let src = "
# @app.route('/fake')
# def fake_view():
#     return ''

@app.route('/real')
def real_view():
    return ''
";
    let result = FlaskResolver.extract("app.py", src).unwrap();
    assert_eq!(names(&result.nodes), vec!["GET /real"]);
    assert_eq!(ref_names(&result.references), vec!["real_view"]);
}

#[test]
fn fastapi_skips_docstring_example_routes() {
    let src = "
\"\"\"
Example:
    @app.get('/in-docstring')
    async def doc():
        pass
\"\"\"
@app.get('/real')
async def real_handler():
    return {}
";
    let result = FastapiResolver.extract("main.py", src).unwrap();
    assert_eq!(names(&result.nodes), vec!["GET /real"]);
    assert_eq!(ref_names(&result.references), vec!["real_handler"]);
}

#[test]
fn laravel_skips_commented_route_calls() {
    let src = "<?php
// Route::get('/fake', [FakeController::class, 'index']);
# Route::get('/also-fake', 'FakeController@show');
/* Route::post('/another-fake', [X::class, 'y']); */
Route::get('/real', [RealController::class, 'index']);
";
    let result = LaravelResolver.extract("routes/web.php", src).unwrap();
    assert_eq!(names(&result.nodes), vec!["GET /real"]);
    assert_eq!(ref_names(&result.references), vec!["RealController@index"]);
}

#[test]
fn rails_skips_begin_end_and_hash_commented_routes() {
    let src = "
# get '/fake', to: 'fake#index'
=begin
get '/also-fake', to: 'fake#show'
=end
get '/real', to: 'real#index'
";
    let result = RailsResolver.extract("config/routes.rb", src).unwrap();
    assert_eq!(names(&result.nodes), vec!["GET /real"]);
    assert_eq!(ref_names(&result.references), vec!["real#index"]);
}

#[test]
fn spring_skips_commented_get_mapping() {
    let src = "
// @GetMapping(\"/fake\")
// public List<X> fake() { return null; }

/* @PostMapping(\"/also-fake\")
   public void alsoFake() {} */

@GetMapping(\"/real\")
public List<User> listUsers() { return users; }
";
    let result = SpringResolver.extract("UserController.java", src).unwrap();
    assert_eq!(names(&result.nodes), vec!["GET /real"]);
    assert_eq!(ref_names(&result.references), vec!["listUsers"]);
}

#[test]
fn aspnet_skips_commented_http_get_attributes() {
    let src = "
// [HttpGet(\"/fake\")]
// public IActionResult Fake() { return Ok(); }

/* [HttpPost(\"/also-fake\")]
   public IActionResult AlsoFake() { return Ok(); } */

[HttpGet(\"/real\")]
public IActionResult ListUsers() { return Ok(); }
";
    let result = AspnetResolver.extract("UserController.cs", src).unwrap();
    assert_eq!(names(&result.nodes), vec!["GET /real"]);
    assert_eq!(ref_names(&result.references), vec!["ListUsers"]);
}

// ---------------------------------------------------------------------------
// drupalResolver.detect
// ---------------------------------------------------------------------------

#[test]
fn drupal_detect_returns_true_when_composer_json_has_a_drupal_dependency() {
    let ctx = FixtureContext::default().with_file(
        "composer.json",
        r#"{"require":{"drupal/core-recommended":"~10.5","drush/drush":"^13"}}"#,
    );
    assert!(DrupalResolver.detect(&ctx));
}

#[test]
fn drupal_detect_returns_true_when_drupal_dependency_is_in_require_dev() {
    let ctx = FixtureContext::default()
        .with_file("composer.json", r#"{"require-dev":{"drupal/core":"^10"}}"#);
    assert!(DrupalResolver.detect(&ctx));
}

#[test]
fn drupal_detect_returns_false_when_composer_json_has_no_drupal_dependencies() {
    let ctx = FixtureContext::default().with_file(
        "composer.json",
        r#"{"require":{"laravel/framework":"^10","php":">=8.1"}}"#,
    );
    assert!(!DrupalResolver.detect(&ctx));
}

#[test]
fn drupal_detect_returns_false_when_composer_json_is_absent() {
    let ctx = FixtureContext::default();
    assert!(!DrupalResolver.detect(&ctx));
}

#[test]
fn drupal_detect_returns_false_when_composer_json_is_malformed_json() {
    let ctx = FixtureContext::default().with_file("composer.json", "{ bad json");
    assert!(!DrupalResolver.detect(&ctx));
}

#[test]
fn drupal_detect_returns_true_for_a_contrib_module_with_empty_require() {
    let ctx = FixtureContext::default().with_file(
        "composer.json",
        r#"{"name":"drupal/admin_toolbar","type":"drupal-module","require":{}}"#,
    );
    assert!(DrupalResolver.detect(&ctx));
}

#[test]
fn drupal_detect_returns_true_via_the_info_yml_fallback_when_composer_json_is_absent() {
    let ctx = FixtureContext::default().with_all_files(&[
        "mymodule/mymodule.info.yml",
        "mymodule/mymodule.routing.yml",
    ]);
    assert!(DrupalResolver.detect(&ctx));
}

#[test]
fn drupal_detect_returns_false_for_a_stray_info_yml_with_no_drupal_php_route_file() {
    let ctx = FixtureContext::default().with_all_files(&["some/unrelated.info.yml"]);
    assert!(!DrupalResolver.detect(&ctx));
}

// ---------------------------------------------------------------------------
// drupalResolver.claimsReference
// ---------------------------------------------------------------------------

#[test]
fn drupal_claims_fqcn_handler_refs_and_hook_names_the_pre_filter_would_drop() {
    assert!(DrupalResolver.claims_reference("\\Drupal\\m\\Form\\SettingsForm"));
    assert!(DrupalResolver.claims_reference("\\Drupal\\m\\Controller\\C:setNoJsCookie"));
    assert!(DrupalResolver.claims_reference("hook_form_alter"));
}

#[test]
fn drupal_does_not_claim_ordinary_identifiers_or_entity_handler_dotted_refs() {
    assert!(!DrupalResolver.claims_reference("someHelperFunction"));
    assert!(!DrupalResolver.claims_reference("comment.default"));
}

// ---------------------------------------------------------------------------
// drupalResolver.extract — routing.yml
// ---------------------------------------------------------------------------

const DRUPAL_ROUTING: &str = "
mymodule.example:
  path: '/mymodule/example'
  defaults:
    _controller: '\\Drupal\\mymodule\\Controller\\MyController::build'
    _title: 'Example page'
  requirements:
    _permission: 'access content'
";

#[test]
fn drupal_emits_a_route_node_for_each_yaml_route() {
    let result = DrupalResolver
        .extract("mymodule/mymodule.routing.yml", DRUPAL_ROUTING)
        .unwrap();
    assert_eq!(result.nodes.len(), 1);
    assert_eq!(result.nodes[0].kind, NodeKind::Route);
    assert_eq!(result.nodes[0].name, "/mymodule/example");
}

#[test]
fn drupal_sets_qualified_name_to_file_path_route_name() {
    let result = DrupalResolver
        .extract("mymodule/mymodule.routing.yml", DRUPAL_ROUTING)
        .unwrap();
    assert_eq!(
        result.nodes[0].qualified_name,
        "mymodule/mymodule.routing.yml::mymodule.example"
    );
}

#[test]
fn drupal_emits_a_references_edge_to_the_controller_fqcn() {
    let result = DrupalResolver
        .extract("mymodule/mymodule.routing.yml", DRUPAL_ROUTING)
        .unwrap();
    assert_eq!(result.references.len(), 1);
    assert_eq!(
        result.references[0].reference_name,
        "\\Drupal\\mymodule\\Controller\\MyController::build"
    );
    assert_eq!(result.references[0].reference_kind, EdgeKind::References);
}

#[test]
fn drupal_emits_a_references_edge_to_a_form_handler() {
    let src = "
mymodule.settings_form:
  path: '/admin/config/mymodule'
  defaults:
    _form: '\\Drupal\\mymodule\\Form\\SettingsForm'
    _title: 'MyModule settings'
  requirements:
    _permission: 'administer site configuration'
";
    let result = DrupalResolver
        .extract("mymodule/mymodule.routing.yml", src)
        .unwrap();
    assert_eq!(result.nodes.len(), 1);
    assert_eq!(
        result.references[0].reference_name,
        "\\Drupal\\mymodule\\Form\\SettingsForm"
    );
}

#[test]
fn drupal_handles_multiple_routes_in_one_file() {
    let src = "
mod.page_one:
  path: '/page-one'
  defaults:
    _controller: '\\Drupal\\mod\\Controller\\PageController::one'
  requirements:
    _permission: 'access content'

mod.page_two:
  path: '/page-two'
  defaults:
    _controller: '\\Drupal\\mod\\Controller\\PageController::two'
  requirements:
    _permission: 'access content'
";
    let result = DrupalResolver.extract("mod/mod.routing.yml", src).unwrap();
    assert_eq!(result.nodes.len(), 2);
    assert!(names(&result.nodes).contains(&"/page-one"));
    assert!(names(&result.nodes).contains(&"/page-two"));
    assert_eq!(result.references.len(), 2);
}

#[test]
fn drupal_skips_commented_out_lines() {
    let src = "
mod.page:
  path: '/page'
  defaults:
    #_controller: '\\Drupal\\mod\\Controller\\Old::build'
    _controller: '\\Drupal\\mod\\Controller\\New::build'
  requirements:
    _permission: 'access content'
";
    let result = DrupalResolver.extract("mod/mod.routing.yml", src).unwrap();
    assert_eq!(result.references.len(), 1);
    assert!(result.references[0].reference_name.contains("New"));
}

#[test]
fn drupal_includes_http_methods_in_the_route_node_name_when_present() {
    let src = "
mod.api:
  path: '/api/resource'
  defaults:
    _controller: '\\Drupal\\mod\\Controller\\ApiController::get'
  methods: [GET, POST]
  requirements:
    _permission: 'access content'
";
    let result = DrupalResolver.extract("mod/mod.routing.yml", src).unwrap();
    assert!(result.nodes[0].name.contains("GET"));
    assert!(result.nodes[0].name.contains("POST"));
}

#[test]
fn drupal_returns_empty_result_for_non_routing_yml_files() {
    // Module files go through hook detection, not route extraction
    let result = DrupalResolver
        .extract("mymodule.module", "<?php\n")
        .unwrap();
    assert_eq!(result.nodes.len(), 0);
}

#[test]
fn drupal_returns_empty_result_for_files_with_no_valid_routes() {
    let result = DrupalResolver
        .extract("some.routing.yml", "# empty\n")
        .unwrap();
    assert_eq!(result.nodes.len(), 0);
    assert_eq!(result.references.len(), 0);
}

// ---------------------------------------------------------------------------
// drupalResolver.extract — hook detection
// ---------------------------------------------------------------------------

#[test]
fn drupal_detects_hook_implementation_via_docblock_strategy_a() {
    let src = "<?php

/**
 * Implements hook_form_alter().
 */
function mymodule_form_alter(&$form, $form_state, $form_id) {
  // ...
}
";
    let result = DrupalResolver
        .extract("web/modules/custom/mymodule/mymodule.module", src)
        .unwrap();
    let hook_ref = result
        .references
        .iter()
        .find(|r| r.reference_name == "hook_form_alter");
    assert!(hook_ref.is_some());
    assert_eq!(hook_ref.unwrap().reference_kind, EdgeKind::References);
}

#[test]
fn drupal_detects_hook_implementation_via_name_pattern_strategy_b() {
    let src = "<?php

function mymodule_views_data() {
  return [];
}
";
    let result = DrupalResolver
        .extract("web/modules/custom/mymodule/mymodule.module", src)
        .unwrap();
    assert!(
        result
            .references
            .iter()
            .any(|r| r.reference_name == "hook_views_data")
    );
}

#[test]
fn drupal_does_not_emit_a_hook_ref_for_non_hook_helper_functions() {
    // 'other_module_helper' doesn't start with 'mymodule_', so no hook ref
    let src = "<?php
function other_module_helper() {}
";
    let result = DrupalResolver
        .extract("web/modules/custom/mymodule/mymodule.module", src)
        .unwrap();
    assert_eq!(result.references.len(), 0);
}

#[test]
fn drupal_detects_hooks_in_install_files() {
    let src = "<?php
/**
 * Implements hook_schema().
 */
function mymodule_schema() {
  return [];
}
";
    let result = DrupalResolver
        .extract("web/modules/custom/mymodule/mymodule.install", src)
        .unwrap();
    assert!(
        result
            .references
            .iter()
            .any(|r| r.reference_name == "hook_schema")
    );
}

#[test]
fn drupal_detects_hooks_in_theme_files() {
    let src = "<?php
/**
 * Implements hook_preprocess_node().
 */
function mytheme_preprocess_node(&$variables) {}
";
    let result = DrupalResolver
        .extract("web/themes/custom/mytheme/mytheme.theme", src)
        .unwrap();
    assert!(
        result
            .references
            .iter()
            .any(|r| r.reference_name == "hook_preprocess_node")
    );
}

#[test]
fn drupal_does_not_duplicate_refs_when_both_docblock_and_name_pattern_match() {
    // Strategy A matches first and adds to docblockMatched set;
    // Strategy B skips already-matched functions.
    let src = "<?php
/**
 * Implements hook_form_alter().
 */
function mymodule_form_alter(&$form, $form_state, $form_id) {}
";
    let result = DrupalResolver
        .extract("web/modules/custom/mymodule/mymodule.module", src)
        .unwrap();
    let hook_refs: Vec<_> = result
        .references
        .iter()
        .filter(|r| r.reference_name == "hook_form_alter")
        .collect();
    assert_eq!(hook_refs.len(), 1);
}

// ---------------------------------------------------------------------------
// drupalResolver.resolve
// ---------------------------------------------------------------------------

#[test]
fn drupal_resolves_a_controller_fqcn_with_double_colon_method_to_the_method_node() {
    let method_node = make_node(
        "method:abc123",
        NodeKind::Method,
        "build",
        "MyController::build",
        "web/modules/custom/mymodule/src/Controller/MyController.php",
        Language::Php,
        10,
        20,
    );
    let class_node = make_node(
        "class:def456",
        NodeKind::Class,
        "MyController",
        "MyController",
        "web/modules/custom/mymodule/src/Controller/MyController.php",
        Language::Php,
        5,
        30,
    );
    let ctx = FixtureContext::default().with_nodes(vec![class_node, method_node]);
    let r = make_ref(
        "\\Drupal\\mymodule\\Controller\\MyController::build",
        "mymodule.routing.yml",
        Language::Yaml,
    );
    let resolved = DrupalResolver.resolve(&r, &ctx);
    assert!(resolved.is_some());
    let resolved = resolved.unwrap();
    assert_eq!(resolved.target_node_id, "method:abc123");
    assert!(resolved.confidence >= 0.85);
}

#[test]
fn drupal_resolves_a_form_fqcn_no_method_to_the_class_node() {
    let class_node = make_node(
        "class:form123",
        NodeKind::Class,
        "SettingsForm",
        "SettingsForm",
        "web/modules/custom/mymodule/src/Form/SettingsForm.php",
        Language::Php,
        1,
        50,
    );
    let ctx = FixtureContext::default().with_nodes(vec![class_node]);
    let r = make_ref(
        "\\Drupal\\mymodule\\Form\\SettingsForm",
        "mymodule.routing.yml",
        Language::Yaml,
    );
    let resolved = DrupalResolver.resolve(&r, &ctx);
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().target_node_id, "class:form123");
}

#[test]
fn drupal_returns_none_when_the_target_class_cannot_be_found() {
    let ctx = FixtureContext::default();
    let r = make_ref(
        "\\Drupal\\mymodule\\Controller\\Missing::method",
        "mymodule.routing.yml",
        Language::Yaml,
    );
    assert!(DrupalResolver.resolve(&r, &ctx).is_none());
}

#[test]
fn drupal_resolves_a_single_colon_controller_service_ref() {
    let method_node = make_node(
        "method:nojs1",
        NodeKind::Method,
        "setNoJsCookie",
        "BigPipeController::setNoJsCookie",
        "core/modules/big_pipe/src/Controller/BigPipeController.php",
        Language::Php,
        10,
        20,
    );
    let class_node = make_node(
        "class:nojs2",
        NodeKind::Class,
        "BigPipeController",
        "BigPipeController",
        "core/modules/big_pipe/src/Controller/BigPipeController.php",
        Language::Php,
        5,
        30,
    );
    let ctx = FixtureContext::default().with_nodes(vec![class_node, method_node]);
    let r = make_ref(
        "\\Drupal\\big_pipe\\Controller\\BigPipeController:setNoJsCookie",
        "big_pipe.routing.yml",
        Language::Yaml,
    );
    let resolved = DrupalResolver.resolve(&r, &ctx);
    assert!(resolved.is_some());
    assert_eq!(resolved.unwrap().target_node_id, "method:nojs1");
}
