//! Salesforce ↔ Apex bridge resolver — every cross-technology edge in a
//! Salesforce DX project:
//!
//! **LWC imports** — `import getAccounts from
//! '@salesforce/apex/AccountController.getAccounts'`. The module path isn't
//! a file — it names an `@AuraEnabled` method as `<Class>.<method>`
//! (optionally `<ns>.<Class>.<method>`). Import-based resolution can never
//! link it; this resolver reads the importing file's mappings and binds the
//! JS local name (and every call/`@wire` through it) to the Apex method.
//!
//! **LWC templates** — `{binding}` references extracted from `.html`
//! templates (`LwcTemplateExtractor`) bind to the component's JS class
//! members: sibling `<stem>.js` first, then the bundle's `<dir>.js`
//! (alternate templates returned by `render()`).
//!
//! **Visualforce** — `Controller.member` references extracted from
//! `.page`/`.component` bindings resolve against the controller class with
//! the VF getter convention: `{!account}` matches a method `account`, a
//! property/field `account`, or `getAccount`.
//!
//! **Aura** — `{!c.handler}` markup references bind to the bundle's client
//! controller (`<bundle>Controller.js`). Aura controller/helper JS is an
//! object literal the JS grammar yields no symbols for, so [`extract`]
//! synthesizes Function nodes per `name: function(…)` member (the Spring
//! `@Value` bind-node precedent) and emits `component.get("c.method")`
//! server-call references from the enclosing function to the Apex method.
//!
//! Apex-side references need nothing here: Apex's flat namespace resolves
//! through the normal name tiers (with the resolver's Apex case-folding).
//!
//! [`extract`]: FrameworkResolver::extract

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::resolution::types::{
    FrameworkExtractionResult,
    FrameworkResolver,
    ResolutionContext,
    ResolvedBy,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{EdgeKind, Language, Node, NodeKind};

const APEX_MODULE_PREFIX: &str = "@salesforce/apex/";

/// `doInit: function(` / `"doInit": function(` — Aura controller/helper
/// object-literal members.
static AURA_FN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?m)^\s*"?([A-Za-z_$][\w$]*)"?\s*:\s*function\s*\("#).expect("valid regex")
});

/// `component.get("c.serverMethod")` — Aura server-action lookups.
static AURA_SERVER_CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\.get\(\s*["']c\.(\w+)["']\s*\)"#).expect("valid regex"));

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch")
        .as_millis() as i64
}

fn line_of(source: &str, offset: usize) -> u32 {
    source[..offset].bytes().filter(|&b| b == b'\n').count() as u32 + 1
}

/// Member-shaped node kinds a template/markup binding may target.
fn is_member_kind(kind: NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::Method | NodeKind::Property | NodeKind::Field | NodeKind::Function
    )
}

pub struct SalesforceResolver;

impl FrameworkResolver for SalesforceResolver {
    fn name(&self) -> &str {
        "salesforce"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[
            Language::Javascript,
            Language::Typescript,
            Language::Apex,
            Language::Html,
            Language::Visualforce,
            Language::Aura,
        ])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // An sfdx project manifest is the unmistakable marker; fall back to
        // the presence of Salesforce sources for unpackaged metadata dumps.
        if context.file_exists("sfdx-project.json") {
            return true;
        }
        context
            .get_all_files()
            .iter()
            .any(|f| f.ends_with(".cls") || f.ends_with(".trigger") || f.ends_with(".cmp"))
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        match reference.language {
            // LWC JS: `@salesforce/apex/Class.method` imports.
            Language::Javascript | Language::Typescript => {
                resolve_lwc_apex_import(reference, context)
            }
            // LWC template `{binding}` → component JS class member.
            Language::Html => resolve_lwc_template_binding(reference, context),
            // Visualforce `Controller.member` bindings (getter convention).
            Language::Apex => resolve_visualforce_binding(reference, context),
            // Aura markup `{!c.handler}` → bundle client controller.
            Language::Aura => resolve_aura_handler(reference, context),
            _ => None,
        }
    }

    /// Synthesize symbol nodes for Aura controller/helper JS. These files
    /// are object literals (`({ doInit: function(…){…} })`) the JS grammar
    /// extracts nothing from; without nodes, `{!c.doInit}` markup refs have
    /// no target and server-side `c.method` calls are invisible.
    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !is_aura_client_js(file_path) {
            return None;
        }

        let now = now_ms();
        let mut result = FrameworkExtractionResult::default();

        // Function members, in file order; each spans to the next member's
        // line (the last one to EOF) so codegraph_explore shows the body.
        let fns: Vec<(String, u32, usize)> = AURA_FN_RE
            .captures_iter(content)
            .map(|c| {
                let m = c.get(1).expect("group 1");
                let whole = c.get(0).expect("match");
                (
                    m.as_str().to_string(),
                    line_of(content, whole.start()),
                    whole.start(),
                )
            })
            .collect();
        let total_lines = content.split('\n').count().max(1) as u32;

        for (i, (name, start_line, _)) in fns.iter().enumerate() {
            let end_line = fns
                .get(i + 1)
                .map(|(_, next_line, _)| next_line.saturating_sub(1).max(*start_line))
                .unwrap_or(total_lines);
            let mut node = Node::new(
                format!("aura-fn:{file_path}:{start_line}:{name}"),
                NodeKind::Function,
                name.clone(),
                format!("{file_path}::{name}"),
                file_path,
                Language::Javascript,
                *start_line,
                end_line,
            );
            node.signature = Some(format!("{name}(component, event, helper)"));
            node.updated_at = now;
            result.nodes.push(node);
        }

        // `component.get("c.serverMethod")` → call ref from the enclosing
        // function (the latest member starting before the call site) to the
        // Apex `@AuraEnabled` method. Language Apex so case-folding and
        // same-language preference apply.
        for caps in AURA_SERVER_CALL_RE.captures_iter(content) {
            let m = caps.get(1).expect("group 1");
            let offset = caps.get(0).expect("match").start();
            let Some(owner_idx) = fns.iter().rposition(|(_, _, fn_off)| *fn_off < offset) else {
                continue;
            };
            result.references.push(UnresolvedRef {
                from_node_id: result.nodes[owner_idx].id.clone(),
                reference_name: m.as_str().to_string(),
                reference_kind: EdgeKind::Calls,
                line: line_of(content, offset),
                column: 0,
                file_path: file_path.to_string(),
                language: Language::Apex,
                candidates: None,
            });
        }

        if result.nodes.is_empty() && result.references.is_empty() {
            None
        } else {
            Some(result)
        }
    }
}

/// `…/aura/<bundle>/<bundle>Controller.js` or `…Helper.js`.
fn is_aura_client_js(file_path: &str) -> bool {
    (file_path.contains("/aura/") || file_path.starts_with("aura/"))
        && (file_path.ends_with("Controller.js") || file_path.ends_with("Helper.js"))
}

/// LWC `@salesforce/apex/Class.method` import binding.
fn resolve_lwc_apex_import(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let imports = context.get_import_mappings(&reference.file_path, reference.language);
    let mapping = imports.iter().find(|m| {
        m.local_name == reference.reference_name && m.source.starts_with(APEX_MODULE_PREFIX)
    })?;

    // `AccountController.getAccounts` or `ns.AccountController.getAccounts`
    // — the method is the last segment, the class the one before it.
    let target = mapping.source.strip_prefix(APEX_MODULE_PREFIX)?;
    let (qualifier, method_name) = target.rsplit_once('.')?;
    let class_name = qualifier.rsplit('.').next().unwrap_or(qualifier);
    if class_name.is_empty() || method_name.is_empty() {
        return None;
    }

    let method = find_apex_member(context, class_name, method_name, false)?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: method.id,
        // The import names class + method explicitly; this is as close
        // to ground truth as cross-language linking gets.
        confidence: 0.95,
        resolved_by: ResolvedBy::Framework,
    })
}

/// LWC template `{binding}` → member of the component's JS class. Sibling
/// `<stem>.js` first; alternate templates (returned by `render()`) fall
/// back to the bundle's `<dir>.js`.
fn resolve_lwc_template_binding(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let html_path = reference.file_path.as_str();
    let stem_js = html_path
        .strip_suffix(".html")
        .or_else(|| html_path.strip_suffix(".htm"))?
        .to_string()
        + ".js";

    let mut candidates = vec![stem_js];
    if let Some(dir_end) = html_path.rfind('/') {
        let dir = &html_path[..dir_end];
        if let Some(bundle) = dir.rsplit('/').next() {
            let bundle_js = format!("{dir}/{bundle}.js");
            if !candidates.contains(&bundle_js) {
                candidates.push(bundle_js);
            }
        }
    }

    for js_path in candidates {
        if let Some(member) = context
            .get_nodes_in_file(&js_path)
            .into_iter()
            .find(|n| is_member_kind(n.kind) && n.name == reference.reference_name)
        {
            return Some(ResolvedRef {
                original: reference.clone(),
                target_node_id: member.id,
                confidence: 0.9,
                resolved_by: ResolvedBy::Framework,
            });
        }
    }
    None
}

/// Visualforce `Controller.member` binding, with the getter convention:
/// `{!account}` matches method `account`, property/field `account`, or
/// `getAccount`. Only fires for references extracted from VF markup —
/// regular Apex code resolves through the normal tiers.
fn resolve_visualforce_binding(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    if !reference.file_path.ends_with(".page") && !reference.file_path.ends_with(".component") {
        return None;
    }
    let (class_name, member) = reference.reference_name.split_once('.')?;
    let node = find_apex_member(context, class_name, member, true)?;
    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: node.id,
        confidence: 0.9,
        resolved_by: ResolvedBy::Framework,
    })
}

/// Aura markup `{!c.handler}` → function in the bundle's client controller
/// (`aura/<bundle>/<bundle>Controller.js`), falling back to the controller
/// file node so the bundle edge survives even if the member regex missed.
fn resolve_aura_handler(
    reference: &UnresolvedRef,
    context: &dyn ResolutionContext,
) -> Option<ResolvedRef> {
    let path = reference.file_path.as_str();
    let dir_end = path.rfind('/')?;
    let dir = &path[..dir_end];
    let bundle = dir.rsplit('/').next()?;
    let controller_js = format!("{dir}/{bundle}Controller.js");

    let nodes = context.get_nodes_in_file(&controller_js);
    let target = nodes
        .iter()
        .find(|n| is_member_kind(n.kind) && n.name == reference.reference_name)
        .or_else(|| nodes.iter().find(|n| n.kind == NodeKind::File))?;

    Some(ResolvedRef {
        original: reference.clone(),
        target_node_id: target.id.clone(),
        confidence: 0.9,
        resolved_by: ResolvedBy::Framework,
    })
}

/// Find a member of an Apex class: the class via the case-folded name
/// index, then a member in its file by case-insensitive name —
/// optionally accepting properties/fields and the `get<Member>` /
/// `set<Member>` accessor convention (Visualforce bindings).
fn find_apex_member(
    context: &dyn ResolutionContext,
    class_name: &str,
    member: &str,
    accessor_convention: bool,
) -> Option<Node> {
    let class_node = context
        .get_nodes_by_lower_name(&class_name.to_lowercase())
        .into_iter()
        .find(|n| n.language == Language::Apex && n.kind == NodeKind::Class)?;

    let in_class = |n: &Node| n.qualified_name.contains(&class_node.name);
    let nodes = context.get_nodes_in_file(&class_node.file_path);

    if let Some(found) = nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name.eq_ignore_ascii_case(member) && in_class(n))
    {
        return Some(found.clone());
    }
    if !accessor_convention {
        return None;
    }
    if let Some(found) = nodes.iter().find(|n| {
        matches!(n.kind, NodeKind::Property | NodeKind::Field)
            && n.name.eq_ignore_ascii_case(member)
            && in_class(n)
    }) {
        return Some(found.clone());
    }
    let getter = format!("get{member}");
    nodes
        .iter()
        .find(|n| n.kind == NodeKind::Method && n.name.eq_ignore_ascii_case(&getter) && in_class(n))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolution::types::ImportMapping;
    use crate::types::{EdgeKind, Node};

    struct Ctx {
        nodes: Vec<Node>,
    }

    impl Ctx {
        fn new() -> Self {
            let cls = "force-app/main/default/classes/AccountController.cls";
            let lwc_js = "force-app/main/default/lwc/accountList/accountList.js";
            let aura_ctrl = "force-app/main/default/aura/orderForm/orderFormController.js";
            let nodes = vec![
                Node::new(
                    "class:1",
                    NodeKind::Class,
                    "AccountController",
                    "AccountController",
                    cls,
                    Language::Apex,
                    1,
                    20,
                ),
                Node::new(
                    "method:1",
                    NodeKind::Method,
                    "getAccounts",
                    "AccountController::getAccounts",
                    cls,
                    Language::Apex,
                    3,
                    8,
                ),
                // VF getter-convention target: `{!greeting}` → getGreeting().
                Node::new(
                    "method:2",
                    NodeKind::Method,
                    "getGreeting",
                    "AccountController::getGreeting",
                    cls,
                    Language::Apex,
                    10,
                    12,
                ),
                // LWC component class members.
                Node::new(
                    "method:js",
                    NodeKind::Method,
                    "handleClick",
                    "AccountList::handleClick",
                    lwc_js,
                    Language::Javascript,
                    7,
                    9,
                ),
                // Aura client-controller function (synthesized by extract()).
                Node::new(
                    "aura-fn:1",
                    NodeKind::Function,
                    "doInit",
                    format!("{aura_ctrl}::doInit"),
                    aura_ctrl,
                    Language::Javascript,
                    2,
                    5,
                ),
            ];
            Ctx { nodes }
        }
    }

    impl ResolutionContext for Ctx {
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
        fn get_nodes_by_qualified_name(&self, qn: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.qualified_name == qn)
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
        fn get_nodes_by_lower_name(&self, lower: &str) -> Vec<Node> {
            self.nodes
                .iter()
                .filter(|n| n.name.to_lowercase() == lower)
                .cloned()
                .collect()
        }
        fn file_exists(&self, path: &str) -> bool {
            path == "sfdx-project.json"
        }
        fn read_file(&self, _: &str) -> Option<String> {
            None
        }
        fn get_project_root(&self) -> &str {
            ""
        }
        fn get_all_files(&self) -> Vec<String> {
            Vec::new()
        }
        fn get_import_mappings(&self, file_path: &str, _: Language) -> Vec<ImportMapping> {
            if file_path == "force-app/main/default/lwc/accountList/accountList.js" {
                vec![ImportMapping {
                    local_name: "fetchAccounts".to_string(),
                    exported_name: "default".to_string(),
                    source: "@salesforce/apex/AccountController.getAccounts".to_string(),
                    is_default: true,
                    is_namespace: false,
                    resolved_path: None,
                }]
            } else {
                Vec::new()
            }
        }
    }

    fn lwc_ref(name: &str) -> UnresolvedRef {
        UnresolvedRef {
            from_node_id: "function:lwc".to_string(),
            reference_name: name.to_string(),
            reference_kind: EdgeKind::Calls,
            line: 5,
            column: 8,
            file_path: "force-app/main/default/lwc/accountList/accountList.js".to_string(),
            language: Language::Javascript,
            candidates: None,
        }
    }

    #[test]
    fn detects_sfdx_project() {
        assert!(SalesforceResolver.detect(&Ctx::new()));
    }

    #[test]
    fn resolves_lwc_apex_import_to_method() {
        // The local alias (`fetchAccounts`) differs from the method name —
        // exactly the case name matching can never connect.
        let resolved = SalesforceResolver
            .resolve(&lwc_ref("fetchAccounts"), &Ctx::new())
            .expect("resolved");
        assert_eq!(resolved.target_node_id, "method:1");
        assert_eq!(resolved.resolved_by, ResolvedBy::Framework);
        assert!(resolved.confidence >= 0.9);
    }

    #[test]
    fn ignores_names_without_apex_import() {
        assert!(
            SalesforceResolver
                .resolve(&lwc_ref("somethingElse"), &Ctx::new())
                .is_none()
        );
    }

    #[test]
    fn ignores_apex_side_references() {
        let mut r = lwc_ref("fetchAccounts");
        r.language = Language::Apex;
        r.file_path = "force-app/main/default/classes/Foo.cls".to_string();
        assert!(SalesforceResolver.resolve(&r, &Ctx::new()).is_none());
    }

    #[test]
    fn lwc_template_binding_resolves_to_sibling_js_member() {
        let r = UnresolvedRef {
            from_node_id: "file:template".to_string(),
            reference_name: "handleClick".to_string(),
            reference_kind: EdgeKind::References,
            line: 3,
            column: 0,
            file_path: "force-app/main/default/lwc/accountList/accountList.html".to_string(),
            language: Language::Html,
            candidates: None,
        };
        let resolved = SalesforceResolver
            .resolve(&r, &Ctx::new())
            .expect("resolved");
        assert_eq!(resolved.target_node_id, "method:js");
        assert_eq!(resolved.resolved_by, ResolvedBy::Framework);

        // Alternate template (different stem) falls back to the bundle JS.
        let mut alt = r.clone();
        alt.file_path =
            "force-app/main/default/lwc/accountList/accountListLoading.html".to_string();
        let resolved = SalesforceResolver
            .resolve(&alt, &Ctx::new())
            .expect("resolved via bundle js");
        assert_eq!(resolved.target_node_id, "method:js");
    }

    #[test]
    fn visualforce_binding_uses_getter_convention() {
        let r = UnresolvedRef {
            from_node_id: "file:page".to_string(),
            // `{!greeting}` — no member named `greeting`; getGreeting() is.
            reference_name: "AccountController.greeting".to_string(),
            reference_kind: EdgeKind::References,
            line: 2,
            column: 0,
            file_path: "force-app/main/default/pages/Wrapper.page".to_string(),
            language: Language::Apex,
            candidates: None,
        };
        let resolved = SalesforceResolver
            .resolve(&r, &Ctx::new())
            .expect("resolved");
        assert_eq!(resolved.target_node_id, "method:2");

        // Direct (case-insensitive) method match still wins where it exists.
        let mut direct = r.clone();
        direct.reference_name = "accountcontroller.GETACCOUNTS".to_string();
        let resolved = SalesforceResolver
            .resolve(&direct, &Ctx::new())
            .expect("resolved");
        assert_eq!(resolved.target_node_id, "method:1");

        // Getter convention must NOT leak into non-VF Apex references.
        let mut apex_code = r.clone();
        apex_code.file_path = "force-app/main/default/classes/Foo.cls".to_string();
        assert!(
            SalesforceResolver
                .resolve(&apex_code, &Ctx::new())
                .is_none()
        );
    }

    #[test]
    fn aura_handler_resolves_to_controller_function() {
        let r = UnresolvedRef {
            from_node_id: "file:cmp".to_string(),
            reference_name: "doInit".to_string(),
            reference_kind: EdgeKind::Calls,
            line: 2,
            column: 0,
            file_path: "force-app/main/default/aura/orderForm/orderForm.cmp".to_string(),
            language: Language::Aura,
            candidates: None,
        };
        let resolved = SalesforceResolver
            .resolve(&r, &Ctx::new())
            .expect("resolved");
        assert_eq!(resolved.target_node_id, "aura-fn:1");
    }

    #[test]
    fn aura_extract_synthesizes_functions_and_server_calls() {
        let content = "({\n    doInit: function(component, event, helper) {\n        helper.load(component);\n    },\n    save: function(component, event, helper) {\n        var action = component.get(\"c.saveOrder\");\n        $A.enqueueAction(action);\n    }\n})\n";
        let result = SalesforceResolver
            .extract(
                "force-app/main/default/aura/orderForm/orderFormController.js",
                content,
            )
            .expect("extraction");

        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["doInit", "save"]);
        for n in &result.nodes {
            assert_eq!(n.kind, NodeKind::Function);
            assert_eq!(n.language, Language::Javascript);
        }
        assert_eq!(result.nodes[0].start_line, 2);
        // doInit spans to the line before save.
        assert_eq!(result.nodes[0].end_line, 4);

        // `component.get("c.saveOrder")` → Apex call ref from `save`.
        assert_eq!(result.references.len(), 1);
        let server_call = &result.references[0];
        assert_eq!(server_call.reference_name, "saveOrder");
        assert_eq!(server_call.from_node_id, result.nodes[1].id);
        assert_eq!(server_call.language, Language::Apex);
        assert_eq!(server_call.reference_kind, EdgeKind::Calls);
    }

    #[test]
    fn extract_ignores_non_aura_js() {
        assert!(
            SalesforceResolver
                .extract(
                    "force-app/main/default/lwc/accountList/accountList.js",
                    "({ x: function() {} })",
                )
                .is_none()
        );
    }
}
