//! Drupal Framework Resolver
//!
//! Supports Drupal 8/9/10/11 (Composer-based projects). Drupal 7 is not supported.
//! Ported from `src/resolution/frameworks/drupal.ts`.
//!
//! ## What this resolver does
//!
//! 1. **Detection** — reads composer.json and checks for any `drupal/*` dependency in
//!    `require` or `require-dev`.
//!
//! 2. **Route extraction** — parses `*.routing.yml` files and emits `route` nodes for each
//!    Drupal route, with `references` edges to the `_controller`, `_form`, or entity handler
//!    class/method.
//!
//! 3. **Hook detection** — scans `.module`, `.install`, `.theme`, and `.inc` files for Drupal
//!    hook implementations. Two strategies are used:
//!    a. Docblock: `@Implements hook_X()` → precise, no false positives.
//!    b. Name pattern: function `{moduleName}_{hookSuffix}()` → catches hooks without
//!    docblocks but may produce false positives on helper functions.
//!    Detected hooks emit an `UnresolvedRef` from the implementing function node to the
//!    canonical `hook_X` name, linking implementations to the hook when `codegraph_callers`
//!    is invoked.
//!
//! ## Design decisions (review in future iterations)
//!
//! - Hook graph resolution (v1): hook references are stored as UnresolvedRef pointing to the
//!   canonical `hook_X` name. If Drupal core is indexed, these will resolve to core hook
//!   definitions. Without core, they remain unresolved but are still searchable via
//!   `codegraph_search("form_alter")`. Full hook-node creation (virtual nodes for every hook)
//!   is deferred to a future iteration.
//!
//! - Services / plugins (out of scope for v1): `*.services.yml` service definitions and plugin
//!   annotations (`@Block`, `@FormElement`, etc.) are not extracted.
//!
//! - Twig templates (out of scope for v1): `.twig` files are tracked as file nodes but no
//!   symbol extraction is performed.

use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::extraction::tree_sitter_helpers::generate_node_id;
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse the last PHP namespace segment from a FQCN like `\Drupal\mymodule\Controller\Foo`.
/// Returns `None` for strings that don't look like a FQCN.
fn last_segment(fqcn: &str) -> Option<&str> {
    let clean = fqcn.trim_start_matches('\\').trim();
    if !clean.contains('\\') {
        return None;
    }
    clean.split('\\').next_back()
}

static MODULE_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"/([^/]+)\.[^./]+$").unwrap());

/// Derive the Drupal module name from a file path.
/// e.g. `web/modules/custom/my_module/my_module.module` → `my_module`
fn module_name_from_path(file_path: &str) -> Option<String> {
    MODULE_NAME_RE.captures(file_path).map(|m| m[1].to_string())
}

// ---------------------------------------------------------------------------
// Route extraction helpers
// ---------------------------------------------------------------------------

struct PendingRoute {
    name: String,
    line_num: u32,
}

static TOP_LEVEL_KEY_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\S.*:\s*$").unwrap());
static LEADING_WS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s").unwrap());
static PATH_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r##"^path:\s*['"]?([^'"#\n]+?)['"]?\s*(?:#.*)?$"##).unwrap());
static CONTROLLER_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r##"^_controller:\s*['"]?([^'"#\n]+?)['"]?\s*(?:#.*)?$"##).unwrap()
});
static FORM_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r##"^_form:\s*['"]?([^'"#\n]+?)['"]?\s*(?:#.*)?$"##).unwrap());
static ENTITY_LINE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r##"^_(entity_form|entity_list|entity_view):\s*['"]?([^'"#\n]+?)['"]?\s*(?:#.*)?$"##)
        .unwrap()
});
static METHODS_LINE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^methods:\s*\[([^\]]+)\]").unwrap());

#[allow(clippy::too_many_arguments)]
fn flush_route(
    pending: &Option<PendingRoute>,
    current_path: &Option<String>,
    handler_refs: &[String],
    methods: &[String],
    file_path: &str,
    now: i64,
    nodes: &mut Vec<Node>,
    references: &mut Vec<UnresolvedRef>,
) {
    let (Some(pending), Some(current_path)) = (pending.as_ref(), current_path.as_ref()) else {
        return;
    };

    let method_tag = if !methods.is_empty() {
        format!(" [{}]", methods.join(","))
    } else {
        String::new()
    };
    let mut route_node = Node::new(
        format!("route:{file_path}:{}:{current_path}", pending.line_num),
        NodeKind::Route,
        format!("{current_path}{method_tag}"),
        format!("{file_path}::{}", pending.name),
        file_path,
        Language::Yaml,
        pending.line_num,
        pending.line_num,
    );
    route_node.updated_at = now;
    let route_id = route_node.id.clone();
    nodes.push(route_node);

    for handler in handler_refs {
        references.push(UnresolvedRef {
            from_node_id: route_id.clone(),
            reference_name: handler.clone(),
            reference_kind: EdgeKind::References,
            line: pending.line_num,
            column: 0,
            file_path: file_path.to_string(),
            language: Language::Yaml,
            candidates: None,
        });
    }
}

/// Extract route nodes and handler references from a Drupal `*.routing.yml` file.
///
/// Drupal routing YAML format:
///
/// ```yaml
/// route.name:
///   path: '/some/path'
///   defaults:
///     _controller: '\Drupal\module\Controller\MyController::method'
///     _form: '\Drupal\module\Form\MyForm'
///     _title: 'Page title'
///   requirements:
///     _permission: 'access content'
///   methods: [GET, POST]   # optional
/// ```
fn extract_drupal_routes(file_path: &str, content: &str) -> FrameworkExtractionResult {
    let mut nodes: Vec<Node> = Vec::new();
    let mut references: Vec<UnresolvedRef> = Vec::new();
    let now = now_millis();

    let lines: Vec<&str> = content.split('\n').collect();

    let mut pending: Option<PendingRoute> = None;
    let mut current_path: Option<String> = None;
    let mut handler_refs: Vec<String> = Vec::new();
    let mut methods: Vec<String> = Vec::new();

    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();

        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // Top-level route name: no leading whitespace, ends with a colon (no value after)
        if TOP_LEVEL_KEY_RE.is_match(line) && !LEADING_WS_RE.is_match(line) {
            flush_route(
                &pending,
                &current_path,
                &handler_refs,
                &methods,
                file_path,
                now,
                &mut nodes,
                &mut references,
            );
            pending = Some(PendingRoute {
                name: trimmed[..trimmed.len() - 1].trim().to_string(),
                line_num: i as u32 + 1,
            });
            current_path = None;
            handler_refs = Vec::new();
            methods = Vec::new();
            continue;
        }

        // path: '/some/path'
        if let Some(path_match) = PATH_LINE_RE.captures(trimmed) {
            current_path = Some(path_match[1].trim().to_string());
            continue;
        }

        // _controller: '\Drupal\...\Class::method'
        if let Some(controller_match) = CONTROLLER_LINE_RE.captures(trimmed) {
            handler_refs.push(controller_match[1].trim().to_string());
            continue;
        }

        // _form: '\Drupal\...\Form\MyForm'
        if let Some(form_match) = FORM_LINE_RE.captures(trimmed) {
            handler_refs.push(form_match[1].trim().to_string());
            continue;
        }

        // _entity_form / _entity_list / _entity_view: entity.type
        if let Some(entity_match) = ENTITY_LINE_RE.captures(trimmed) {
            handler_refs.push(entity_match[2].trim().to_string());
            continue;
        }

        // methods: [GET, POST]  or  methods: [GET]
        if let Some(methods_match) = METHODS_LINE_RE.captures(trimmed) {
            methods = methods_match[1]
                .split(',')
                .map(|m| m.trim().to_uppercase())
                .filter(|m| !m.is_empty())
                .collect();
            continue;
        }
    }

    flush_route(
        &pending,
        &current_path,
        &handler_refs,
        &methods,
        file_path,
        now,
        &mut nodes,
        &mut references,
    );
    FrameworkExtractionResult { nodes, references }
}

// ---------------------------------------------------------------------------
// Hook detection helpers
// ---------------------------------------------------------------------------

const HOOK_FILE_EXTENSIONS: &[&str] = &[".module", ".install", ".theme", ".inc"];

fn is_drupal_hook_file(file_path: &str) -> bool {
    HOOK_FILE_EXTENSIONS
        .iter()
        .any(|ext| file_path.ends_with(ext))
}

static FUNC_DEF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^function\s+(\w+)\s*\(").unwrap());
// Strategy A: docblock `Implements hook_X().` followed by function definition.
// The docblock and function may be separated by blank lines.
static DOCBLOCK_HOOK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"/\*\*[\s\S]*?(?:@|\*\s+)Implements\s+(hook_\w+)\s*\(\)[\s\S]*?\*/\s*\n(?:\s*\n)*function\s+(\w+)\s*\(",
    )
    .unwrap()
});

/// Extract hook implementation references from a Drupal PHP file.
///
/// Strategy A (primary): look for docblocks containing `Implements hook_X().`
/// followed immediately by the function definition. This is the Drupal coding
/// standard and is precise.
///
/// Strategy B (fallback): for functions whose name starts with `{moduleName}_`,
/// treat the suffix as the hook name. Catches hooks without docblocks but may
/// produce false positives on non-hook helper functions.
///
/// Each detected hook emits an UnresolvedRef from the implementing function node
/// (identified by computing the same ID tree-sitter would generate) to the
/// canonical hook name, e.g. `hook_form_alter`.
fn extract_drupal_hooks(file_path: &str, content: &str) -> FrameworkExtractionResult {
    let mut references: Vec<UnresolvedRef> = Vec::new();

    // Build a map of function name → 1-indexed line number for all top-level functions.
    // This mirrors tree-sitter's line numbering so we can reconstruct node IDs.
    // (TS used an insertion-ordered Map; here a HashMap for lookup + Vec for order.)
    let mut func_line_map: HashMap<String, u32> = HashMap::new();
    let mut func_order: Vec<String> = Vec::new();
    for fm in FUNC_DEF_RE.captures_iter(content) {
        let name = fm[1].to_string();
        if !func_line_map.contains_key(&name) {
            // line = number of newlines before match start + 1
            func_line_map.insert(name.clone(), line_of(content, fm.get(0).unwrap().start()));
            func_order.push(name);
        }
    }

    let emit_hook_ref = |hook_name: &str, func_name: &str, refs: &mut Vec<UnresolvedRef>| {
        let Some(&line_num) = func_line_map.get(func_name) else {
            return;
        };
        let node_id = generate_node_id(file_path, NodeKind::Function, func_name, line_num);
        refs.push(UnresolvedRef {
            from_node_id: node_id,
            reference_name: hook_name.to_string(),
            reference_kind: EdgeKind::References,
            line: line_num,
            column: 0,
            file_path: file_path.to_string(),
            language: Language::Php,
            candidates: None,
        });
    };

    let mut docblock_matched: HashSet<String> = HashSet::new();
    for m in DOCBLOCK_HOOK_RE.captures_iter(content) {
        let hook_name = m[1].to_string();
        let func_name = m[2].to_string();
        emit_hook_ref(&hook_name, &func_name, &mut references);
        docblock_matched.insert(func_name);
    }

    // Strategy B: fallback name-pattern matching for functions without docblocks.
    // Only applies to functions whose name starts with {moduleName}_ and that were
    // not already matched by Strategy A.
    if let Some(module_name) = module_name_from_path(file_path) {
        let prefix = format!("{module_name}_");
        for func_name in &func_order {
            if docblock_matched.contains(func_name) {
                continue;
            }
            if !func_name.starts_with(&prefix) {
                continue;
            }
            let hook_suffix = &func_name[prefix.len()..];
            if hook_suffix.is_empty() {
                continue;
            }
            // Emit a reference to hook_{suffix} — the resolver will link it if the
            // hook is defined somewhere in the indexed graph (e.g. Drupal core).
            emit_hook_ref(&format!("hook_{hook_suffix}"), func_name, &mut references);
        }
    }

    FrameworkExtractionResult {
        nodes: Vec::new(),
        references,
    }
}

// ---------------------------------------------------------------------------
// Resolver
// ---------------------------------------------------------------------------

static CLAIM_CLASS_METHOD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*::?\w+$").unwrap());
static CONTROLLER_FQCN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\\?(?:Drupal\\[^:]+\\)?([^\\:]+)::?(\w+)$").unwrap());

/// Drupal framework resolver (TS `drupalResolver`).
pub struct DrupalResolver;

impl FrameworkResolver for DrupalResolver {
    fn name(&self) -> &str {
        "drupal"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Php, Language::Yaml])
    }

    /// Drupal route handlers are FQCNs (`\Drupal\…\Class::method`, the single-colon
    /// controller-service form `\Drupal\…\Class:method`, or a bare `\…\FormClass`)
    /// and hook refs are canonical `hook_*` names — none match a declared symbol, so
    /// resolveOne's pre-filter would drop them before resolve() runs. Claim the
    /// shapes resolve() handles (mirrors the Rails `controller#action` claim).
    fn claims_reference(&self, name: &str) -> bool {
        name.starts_with("hook_") || name.contains('\\') || CLAIM_CLASS_METHOD_RE.is_match(name)
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Primary: composer.json identifies a Drupal project/module/theme/profile.
        // A contrib module often has an EMPTY `require` (no `drupal/*` dep) but still
        // declares `"name": "drupal/<module>"` and `"type": "drupal-module"`, so check
        // those too — checking deps alone misses every standalone contrib module.
        if let Some(composer) = context.read_file("composer.json") {
            if let Ok(json) = serde_json::from_str::<serde_json::Value>(&composer) {
                if json
                    .get("name")
                    .and_then(|v| v.as_str())
                    .is_some_and(|n| n.starts_with("drupal/"))
                {
                    return true;
                }
                if json
                    .get("type")
                    .and_then(|v| v.as_str())
                    .is_some_and(|t| t.starts_with("drupal-"))
                {
                    return true;
                }
                let require_keys = json
                    .get("require")
                    .and_then(|v| v.as_object())
                    .into_iter()
                    .flat_map(|o| o.keys());
                let require_dev_keys = json
                    .get("require-dev")
                    .and_then(|v| v.as_object())
                    .into_iter()
                    .flat_map(|o| o.keys());
                if require_keys
                    .chain(require_dev_keys)
                    .any(|k| k.starts_with("drupal/"))
                {
                    return true;
                }
            }
            // malformed composer.json — fall through to file-based detection
        }

        // Fallback (composer-less module, or a non-Drupal composer.json): the
        // unmistakable Drupal signature is a `*.info.yml` manifest alongside a
        // Drupal PHP/route file. Require both so a stray `.info.yml` elsewhere
        // doesn't trigger a false positive.
        let files = context.get_all_files();
        let has_info_yml = files.iter().any(|f| f.ends_with(".info.yml"));
        if !has_info_yml {
            return false;
        }
        files.iter().any(|f| {
            f.ends_with(".routing.yml")
                || f.ends_with(".module")
                || f.ends_with(".install")
                || f.ends_with(".theme")
        })
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let name = reference.reference_name.as_str();

        // _controller: '\Drupal\module\...\ClassName::methodName' (double colon) or the
        // single-colon controller-service form '\Drupal\...\ClassName:methodName'.
        if let Some(controller_match) = CONTROLLER_FQCN_RE.captures(name) {
            let class_name = &controller_match[1];
            let method_name = &controller_match[2];
            let class_nodes = context.get_nodes_by_name(class_name);
            for cls in &class_nodes {
                if cls.kind != NodeKind::Class {
                    continue;
                }
                let file_nodes = context.get_nodes_in_file(&cls.file_path);
                if let Some(method) = file_nodes
                    .iter()
                    .find(|n| n.kind == NodeKind::Method && n.name == method_name)
                {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: method.id.clone(),
                        confidence: 0.9,
                        resolved_by: ResolvedBy::Framework,
                    });
                }
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: cls.id.clone(),
                    confidence: 0.7,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        // _form / _entity_form: '\Drupal\module\...\ClassName'  (bare FQCN, no method)
        if name.contains('\\') && !name.contains(':') {
            if let Some(class_name) = last_segment(name) {
                let class_nodes = context.get_nodes_by_name(class_name);
                if let Some(cls) = class_nodes.iter().find(|n| n.kind == NodeKind::Class) {
                    return Some(ResolvedRef {
                        original: reference.clone(),
                        target_node_id: cls.id.clone(),
                        confidence: 0.85,
                        resolved_by: ResolvedBy::Framework,
                    });
                }
            }
        }

        // hook_X — find any function whose name ends in _{hookSuffix} in a hook file
        if let Some(hook_suffix) = name.strip_prefix("hook_") {
            let suffix_pattern = format!("_{hook_suffix}");
            let candidates: Vec<Node> = context
                .get_nodes_by_kind(NodeKind::Function)
                .into_iter()
                .filter(|n| n.name.ends_with(&suffix_pattern) && is_drupal_hook_file(&n.file_path))
                .collect();
            if let Some(first) = candidates.first() {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: first.id.clone(),
                    confidence: 0.75,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if file_path.ends_with(".routing.yml") {
            return Some(extract_drupal_routes(file_path, content));
        }

        if is_drupal_hook_file(file_path) || file_path.ends_with(".php") {
            return Some(extract_drupal_hooks(file_path, content));
        }

        Some(FrameworkExtractionResult::default())
    }
}
