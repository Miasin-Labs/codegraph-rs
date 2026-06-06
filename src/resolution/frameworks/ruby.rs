//! Ruby Framework Resolver
//!
//! Handles Ruby on Rails patterns.
//! Ported from `src/resolution/frameworks/ruby.ts`.

use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::{
    FrameworkExtractionResult,
    FrameworkResolver,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{EdgeKind, Language, Node, NodeKind};

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn line_of(content: &str, idx: usize) -> u32 {
    content[..idx].matches('\n').count() as u32 + 1
}

static CONTROLLER_ACTION_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\w/]+#\w+$").unwrap());
static CONTROLLER_ACTION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([\w/]+)#(\w+)$").unwrap());
static MODEL_NAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][a-zA-Z]+$").unwrap());

// get/post/put/patch/delete/match '/path', to: 'controller#action'
// Also: get '/path' => 'controller#action'
static ROUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"\b(get|post|put|patch|delete|match)\s+['"]([^'"]+)['"]\s*(?:,\s*to:\s*|=>\s*)['"]([^#'"]+)#([^'"]+)['"]"#,
    )
    .unwrap()
});

// RESTful resources: `resources :articles` / `resource :user`
static RESOURCES_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(resources?)\s+:(\w+)([^\n]*)").unwrap());
static ONLY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"only:\s*\[([^\]]*)\]").unwrap());
static EXCEPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"except:\s*\[([^\]]*)\]").unwrap());

/// Rails framework resolver (TS `railsResolver`).
pub struct RailsResolver;

impl FrameworkResolver for RailsResolver {
    fn name(&self) -> &str {
        "rails"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Ruby])
    }

    /// `controller#action` route refs name no declared symbol, so resolveOne's
    /// pre-filter would drop them before resolve() runs. Claim them (like the django
    /// `_iterable_class` hook) so they reach Pattern 0.
    fn claims_reference(&self, name: &str) -> bool {
        CONTROLLER_ACTION_CLAIM_RE.is_match(name)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for Gemfile with rails
        if let Some(gemfile) = context.read_file("Gemfile") {
            if gemfile.contains("'rails'") {
                return true;
            }
        }

        // Check for config/application.rb (Rails signature)
        if context.file_exists("config/application.rb") {
            return true;
        }

        // Check for typical Rails directory structure
        context.file_exists("app/controllers/application_controller.rb")
            || context.file_exists("config/routes.rb")
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();

        // Pattern 0: route action `controller#action` (from RESTful `resources` or an
        // explicit route) → the action method in that controller. Precise — avoids the
        // bare-`action` ambiguity (every controller has an `index`/`show`).
        if let Some(ca) = CONTROLLER_ACTION_RE.captures(name) {
            if let Some(result) = resolve_controller_action(&ca[1], &ca[2], context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
            return None;
        }

        // Pattern 1: Model references (ActiveRecord)
        if MODEL_NAME_RE.is_match(name) {
            if let Some(result) = resolve_model(name, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 2: Controller references
        if name.ends_with("Controller") {
            if let Some(result) = resolve_controller(name, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.85,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 3: Helper references
        if name.ends_with("Helper") {
            if let Some(result) = resolve_helper(name, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // Pattern 4: Service/Job references
        if name.ends_with("Service") || name.ends_with("Job") {
            if let Some(result) = resolve_service(name, context) {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: result,
                    confidence: 0.8,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".rb") {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_millis();
        let safe = strip_comments_for_regex(content, CommentLang::Ruby);

        for m in ROUTE_RE.captures_iter(&safe) {
            let method = m.get(1).unwrap().as_str();
            let route_path = m.get(2).unwrap().as_str();
            let ctrl = m.get(3).unwrap().as_str();
            let action = m.get(4).unwrap().as_str();
            let whole = m.get(0).unwrap();
            let line = line_of(&safe, whole.start());
            let upper = method.to_uppercase();
            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{upper}:{route_path}"),
                NodeKind::Route,
                format!("{upper} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                Language::Ruby,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);

            references.push(UnresolvedRef {
                from_node_id: route_id,
                // precise controller#action, not bare action
                reference_name: format!("{ctrl}#{action}"),
                reference_kind: EdgeKind::References,
                line,
                column: 0,
                file_path: file_path.to_string(),
                language: Language::Ruby,
                candidates: None,
            });
        }

        // RESTful resources: `resources :articles` / `resource :user` (the dominant
        // Rails routing) generate a controller action per REST verb. The old resolver
        // only saw explicit `get '/x' => 'c#a'` routes, so resource-routed apps had
        // ZERO route nodes. Expand each into its actions → `controller#action` refs.
        for m in RESOURCES_RE.captures_iter(&safe) {
            let plural = m.get(1).unwrap().as_str() == "resources";
            let res_name = m.get(2).unwrap().as_str();
            let tail = m.get(3).map(|g| g.as_str()).unwrap_or("");
            let mut actions: Vec<&str> = if plural {
                PLURAL_ACTIONS.to_vec()
            } else {
                SINGULAR_ACTIONS.to_vec()
            };
            let sym_list = |s: &str| -> HashSet<String> {
                s.split(',')
                    .map(|x| x.trim().trim_start_matches(':').to_string())
                    .collect()
            };
            if let Some(only) = ONLY_RE.captures(tail) {
                let s = sym_list(&only[1]);
                actions.retain(|a| s.contains(*a));
            } else if let Some(except) = EXCEPT_RE.captures(tail) {
                let s = sym_list(&except[1]);
                actions.retain(|a| !s.contains(*a));
            }
            // `resources :articles` → ArticlesController; `resource :user` → UsersController.
            let ctrl = if plural {
                res_name.to_string()
            } else {
                pluralize(res_name)
            };
            let whole = m.get(0).unwrap();
            let line = line_of(&safe, whole.start());
            for action in actions {
                let (method, path) = restful_route(action, res_name);
                let mut route_node = Node::new(
                    format!("route:{file_path}:{line}:{method}:{ctrl}#{action}"),
                    NodeKind::Route,
                    format!("{method} {path}"),
                    format!("{file_path}::route:{ctrl}#{action}"),
                    file_path,
                    Language::Ruby,
                    line,
                    line,
                );
                route_node.end_column = whole.as_str().len() as u32;
                route_node.updated_at = now;
                let route_id = route_node.id.clone();
                nodes.push(route_node);
                references.push(UnresolvedRef {
                    from_node_id: route_id,
                    reference_name: format!("{ctrl}#{action}"),
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Ruby,
                    candidates: None,
                });
            }
        }

        Some(FrameworkExtractionResult { nodes, references })
    }
}

// Helper functions

/// RESTful action → HTTP verb + path. `resources` gets all seven; a singular
/// `resource` omits `index`.
fn restful_route(action: &str, r: &str) -> (&'static str, String) {
    match action {
        "index" => ("GET", format!("/{r}")),
        "create" => ("POST", format!("/{r}")),
        "new" => ("GET", format!("/{r}/new")),
        "show" => ("GET", format!("/{r}/:id")),
        "edit" => ("GET", format!("/{r}/:id/edit")),
        "update" => ("PATCH", format!("/{r}/:id")),
        "destroy" => ("DELETE", format!("/{r}/:id")),
        _ => unreachable!("unknown RESTful action: {action}"),
    }
}

const PLURAL_ACTIONS: &[&str] = &[
    "index", "create", "new", "show", "edit", "update", "destroy",
];
const SINGULAR_ACTIONS: &[&str] = &["create", "new", "show", "edit", "update", "destroy"];

static PLURALIZE_Y_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^aeiou]y$").unwrap());
static PLURALIZE_ES_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(s|x|z|ch|sh)$").unwrap());

/// Naive ActiveSupport-style pluralize — covers the common resource names.
fn pluralize(w: &str) -> String {
    if PLURALIZE_Y_RE.is_match(w) {
        format!("{}ies", &w[..w.len() - 1])
    } else if PLURALIZE_ES_RE.is_match(w) {
        format!("{w}es")
    } else {
        format!("{w}s")
    }
}

/// snake_case → CamelCase (`user_profiles` → `UserProfiles`).
fn camelize(s: &str) -> String {
    s.split('_')
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join("")
}

/// CamelCase -> snake_case.rb (TS `name.replace(/([A-Z])/g, '_$1').toLowerCase().slice(1)`).
fn rails_snake_case(name: &str) -> String {
    let mut s = String::new();
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            s.push('_');
        }
        s.push(ch);
    }
    let lower = s.to_lowercase();
    let mut chars = lower.chars();
    chars.next(); // TS .slice(1)
    chars.as_str().to_string()
}

/// Resolve a `controller#action` route ref to the action method in that controller.
fn resolve_controller_action(
    ctrl_path: &str,
    action: &str,
    context: &dyn ResolutionContext,
) -> Option<String> {
    // Rails convention: `articles` → app/controllers/articles_controller.rb.
    let direct = format!("app/controllers/{ctrl_path}_controller.rb");
    if context.file_exists(&direct) {
        if let Some(m) = context.get_nodes_in_file(&direct).into_iter().find(|n| {
            (n.kind == NodeKind::Method || n.kind == NodeKind::Function) && n.name == action
        }) {
            return Some(m.id);
        }
    }
    // Fall back: controller class by name, then the action method in its file.
    let cls = camelize(ctrl_path.split('/').next_back().unwrap_or(ctrl_path)) + "Controller";
    for ctrl in context
        .get_nodes_by_name(&cls)
        .into_iter()
        .filter(|n| n.kind == NodeKind::Class)
    {
        if let Some(m) = context
            .get_nodes_in_file(&ctrl.file_path)
            .into_iter()
            .find(|n| {
                (n.kind == NodeKind::Method || n.kind == NodeKind::Function) && n.name == action
            })
        {
            return Some(m.id);
        }
    }
    None
}

fn resolve_model(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    // Try direct file path lookup first (Rails convention: CamelCase -> snake_case.rb)
    let snake_name = rails_snake_case(name);
    let possible_paths = [
        format!("app/models/{snake_name}.rb"),
        format!("app/models/concerns/{snake_name}.rb"),
    ];

    for model_path in &possible_paths {
        if context.file_exists(model_path) {
            let nodes = context.get_nodes_in_file(model_path);
            if let Some(model_node) = nodes
                .iter()
                .find(|n| n.kind == NodeKind::Class && n.name == name)
            {
                return Some(model_node.id.clone());
            }
        }
    }

    // Fall back to name-based lookup
    let candidates = context.get_nodes_by_name(name);
    if let Some(model_node) = candidates
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.file_path.contains("app/models/"))
    {
        return Some(model_node.id.clone());
    }

    None
}

fn resolve_controller(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    // Try direct file path lookup first
    let snake_name = rails_snake_case(name);
    let possible_paths = [
        format!("app/controllers/{snake_name}.rb"),
        format!("app/controllers/api/{snake_name}.rb"),
        format!("app/controllers/api/v1/{snake_name}.rb"),
    ];

    for controller_path in &possible_paths {
        if context.file_exists(controller_path) {
            let nodes = context.get_nodes_in_file(controller_path);
            if let Some(controller_node) = nodes
                .iter()
                .find(|n| n.kind == NodeKind::Class && n.name == name)
            {
                return Some(controller_node.id.clone());
            }
        }
    }

    // Fall back to name-based lookup
    let candidates = context.get_nodes_by_name(name);
    if let Some(controller_node) = candidates
        .iter()
        .find(|n| n.kind == NodeKind::Class && n.file_path.contains("controllers/"))
    {
        return Some(controller_node.id.clone());
    }

    None
}

fn resolve_helper(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    let snake_name = rails_snake_case(name);
    let helper_path = format!("app/helpers/{snake_name}.rb");

    if context.file_exists(&helper_path) {
        let nodes = context.get_nodes_in_file(&helper_path);
        if let Some(helper_node) = nodes
            .iter()
            .find(|n| n.kind == NodeKind::Module && n.name == name)
        {
            return Some(helper_node.id.clone());
        }
    }

    None
}

fn resolve_service(name: &str, context: &dyn ResolutionContext) -> Option<String> {
    let snake_name = rails_snake_case(name);
    let possible_paths = [
        format!("app/services/{snake_name}.rb"),
        format!("app/jobs/{snake_name}.rb"),
        format!("app/workers/{snake_name}.rb"),
    ];

    for service_path in &possible_paths {
        if context.file_exists(service_path) {
            let nodes = context.get_nodes_in_file(service_path);
            if let Some(service_node) = nodes
                .iter()
                .find(|n| n.kind == NodeKind::Class && n.name == name)
            {
                return Some(service_node.id.clone());
            }
        }
    }

    None
}
