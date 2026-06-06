//! Swift Framework Resolver
//!
//! Handles SwiftUI, UIKit, and Vapor (server-side Swift) patterns.
//!
//! Ported from `src/resolution/frameworks/swift.ts`.

use std::collections::HashMap;
use std::sync::LazyLock;

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

// Directory patterns
const VIEW_DIRS: &[&str] = &["/Views/", "/View/", "/Screens/", "/Components/", "/UI/"];
const VIEWMODEL_DIRS: &[&str] = &[
    "/ViewModels/",
    "/ViewModel/",
    "/Stores/",
    "/Managers/",
    "/Services/",
];
const MODEL_DIRS: &[&str] = &["/Models/", "/Model/", "/Entities/", "/Domain/"];
const VC_DIRS: &[&str] = &[
    "/ViewControllers/",
    "/ViewController/",
    "/Controllers/",
    "/Screens/",
];
const UIVIEW_DIRS: &[&str] = &["/Views/", "/View/", "/UI/", "/Components/"];
const CELL_DIRS: &[&str] = &[
    "/Cells/",
    "/Cell/",
    "/Views/",
    "/TableViewCells/",
    "/CollectionViewCells/",
];
const VAPOR_CONTROLLER_DIRS: &[&str] = &["/Controllers/", "/Controller/", "/Routes/"];
const FLUENT_MODEL_DIRS: &[&str] = &["/Models/", "/Model/", "/Entities/", "/Database/"];
const VAPOR_MIDDLEWARE_DIRS: &[&str] = &["/Middleware/", "/Middlewares/"];

const VIEW_KINDS: &[NodeKind] = &[NodeKind::Struct, NodeKind::Component];
const CLASS_KINDS: &[NodeKind] = &[NodeKind::Class];
const MODEL_KINDS: &[NodeKind] = &[NodeKind::Struct, NodeKind::Class];
const PROTOCOL_KINDS: &[NodeKind] = &[NodeKind::Protocol];
const VAPOR_CONTROLLER_KINDS: &[NodeKind] = &[NodeKind::Class, NodeKind::Struct];

static PASCAL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z][a-zA-Z]+$").unwrap());

fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Number of the line containing byte offset `idx` (1-based) — TS
/// `safe.slice(0, idx).split('\n').length`.
fn line_at(safe: &str, idx: usize) -> u32 {
    (safe[..idx].matches('\n').count() + 1) as u32
}

fn framework_hit(reference: &UnresolvedRef, target: String, confidence: f64) -> ResolvedRef {
    ResolvedRef {
        original: reference.clone(),
        target_node_id: target,
        confidence,
        resolved_by: ResolvedBy::Framework,
    }
}

/// Resolve a symbol by name using indexed queries instead of scanning all files.
fn resolve_by_name_and_kind(
    name: &str,
    kinds: &[NodeKind],
    preferred_dir_patterns: &[&str],
    context: &dyn ResolutionContext,
) -> Option<String> {
    let candidates = context.get_nodes_by_name(name);
    if candidates.is_empty() {
        return None;
    }

    let kind_filtered: Vec<&Node> = candidates
        .iter()
        .filter(|n| kinds.contains(&n.kind))
        .collect();
    if kind_filtered.is_empty() {
        return None;
    }

    // Prefer candidates in framework-conventional directories
    if !preferred_dir_patterns.is_empty() {
        let preferred: Vec<&&Node> = kind_filtered
            .iter()
            .filter(|n| {
                preferred_dir_patterns
                    .iter()
                    .any(|d| n.file_path.contains(d))
            })
            .collect();
        if let Some(first) = preferred.first() {
            return Some(first.id.clone());
        }
    }

    // Fall back to any match
    Some(kind_filtered[0].id.clone())
}

// =============================================================================
// SwiftUI
// =============================================================================

/// TS `swiftUIResolver` (name: `"swiftui"`).
#[derive(Debug, Default)]
pub struct SwiftUIResolver;

impl FrameworkResolver for SwiftUIResolver {
    fn name(&self) -> &str {
        "swiftui"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Swift])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for SwiftUI imports in Swift files
        let all_files = context.get_all_files();
        for file in &all_files {
            if file.ends_with(".swift") {
                if let Some(content) = context.read_file(file) {
                    if content.contains("import SwiftUI") {
                        return true;
                    }
                }
            }
        }

        // Check for Xcode project with SwiftUI
        for file in &all_files {
            if file.ends_with(".xcodeproj") || file.ends_with(".xcworkspace") {
                return true;
            }
        }

        false
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // Pattern 1: View references (SwiftUI views are PascalCase ending in View)
        if reference.reference_name.ends_with("View")
            && reference
                .reference_name
                .starts_with(|c: char| c.is_ascii_uppercase())
        {
            if let Some(result) =
                resolve_by_name_and_kind(&reference.reference_name, VIEW_KINDS, VIEW_DIRS, context)
            {
                return Some(framework_hit(reference, result, 0.85));
            }
        }

        // Pattern 2: ViewModel/ObservableObject references
        if reference.reference_name.ends_with("ViewModel")
            || reference.reference_name.ends_with("Store")
            || reference.reference_name.ends_with("Manager")
        {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                CLASS_KINDS,
                VIEWMODEL_DIRS,
                context,
            ) {
                return Some(framework_hit(reference, result, 0.85));
            }
        }

        // Pattern 3: Model references
        if PASCAL_RE.is_match(&reference.reference_name) {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                MODEL_KINDS,
                MODEL_DIRS,
                context,
            ) {
                return Some(framework_hit(reference, result, 0.7));
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".swift") {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let now = now_millis();
        let safe = strip_comments_for_regex(content, CommentLang::Swift);

        // Extract SwiftUI View structs
        // struct ContentView: View { ... }
        static VIEW_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"struct\s+(\w+)\s*:\s*(?:\w+\s*,\s*)*View").unwrap());

        for caps in VIEW_RE.captures_iter(&safe) {
            let whole = caps.get(0).unwrap();
            let view_name = caps.get(1).unwrap().as_str();
            let line = line_at(&safe, whole.start());

            let mut node = Node::new(
                format!("view:{file_path}:{view_name}:{line}"),
                NodeKind::Component,
                view_name,
                format!("{file_path}::{view_name}"),
                file_path,
                Language::Swift,
                line,
                line,
            );
            node.end_column = whole.as_str().len() as u32;
            node.updated_at = now;
            nodes.push(node);
        }

        // Extract @main App entry point
        static APP_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"@main\s+struct\s+(\w+)\s*:\s*App").unwrap());

        for caps in APP_RE.captures_iter(&safe) {
            let whole = caps.get(0).unwrap();
            let app_name = caps.get(1).unwrap().as_str();
            let line = line_at(&safe, whole.start());

            let mut node = Node::new(
                format!("app:{file_path}:{app_name}:{line}"),
                NodeKind::Class,
                app_name,
                format!("{file_path}::{app_name}"),
                file_path,
                Language::Swift,
                line,
                line,
            );
            node.end_column = whole.as_str().len() as u32;
            node.updated_at = now;
            nodes.push(node);
        }

        Some(FrameworkExtractionResult {
            nodes,
            references: Vec::new(),
        })
    }
}

// =============================================================================
// UIKit
// =============================================================================

/// TS `uikitResolver` (name: `"uikit"`).
#[derive(Debug, Default)]
pub struct UIKitResolver;

impl FrameworkResolver for UIKitResolver {
    fn name(&self) -> &str {
        "uikit"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Swift])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        let all_files = context.get_all_files();
        for file in &all_files {
            if file.ends_with(".swift") {
                if let Some(content) = context.read_file(file) {
                    if content.contains("import UIKit")
                        || content.contains("UIViewController")
                        || content.contains("UIView")
                    {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // Pattern 1: ViewController references
        if reference.reference_name.ends_with("ViewController") {
            if let Some(result) =
                resolve_by_name_and_kind(&reference.reference_name, CLASS_KINDS, VC_DIRS, context)
            {
                return Some(framework_hit(reference, result, 0.85));
            }
        }

        // Pattern 2: UIView subclass references
        if reference.reference_name.ends_with("View")
            && !reference.reference_name.ends_with("ViewController")
        {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                CLASS_KINDS,
                UIVIEW_DIRS,
                context,
            ) {
                return Some(framework_hit(reference, result, 0.8));
            }
        }

        // Pattern 3: Cell references
        if reference.reference_name.ends_with("Cell") {
            if let Some(result) =
                resolve_by_name_and_kind(&reference.reference_name, CLASS_KINDS, CELL_DIRS, context)
            {
                return Some(framework_hit(reference, result, 0.85));
            }
        }

        // Pattern 4: Delegate/DataSource references
        if reference.reference_name.ends_with("Delegate")
            || reference.reference_name.ends_with("DataSource")
        {
            if let Some(result) =
                resolve_by_name_and_kind(&reference.reference_name, PROTOCOL_KINDS, &[], context)
            {
                return Some(framework_hit(reference, result, 0.8));
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".swift") {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let now = now_millis();
        let safe = strip_comments_for_regex(content, CommentLang::Swift);

        // Extract UIViewController subclasses
        static VC_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"class\s+(\w+)\s*:\s*(?:\w+\s*,\s*)*UIViewController").unwrap()
        });

        for caps in VC_RE.captures_iter(&safe) {
            let whole = caps.get(0).unwrap();
            let vc_name = caps.get(1).unwrap().as_str();
            let line = line_at(&safe, whole.start());

            let mut node = Node::new(
                format!("viewcontroller:{file_path}:{vc_name}:{line}"),
                NodeKind::Class,
                vc_name,
                format!("{file_path}::{vc_name}"),
                file_path,
                Language::Swift,
                line,
                line,
            );
            node.end_column = whole.as_str().len() as u32;
            node.updated_at = now;
            nodes.push(node);
        }

        // Extract UIView subclasses
        static UIVIEW_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"class\s+(\w+)\s*:\s*(?:\w+\s*,\s*)*UIView[^C]").unwrap());

        for caps in UIVIEW_RE.captures_iter(&safe) {
            let whole = caps.get(0).unwrap();
            let view_name = caps.get(1).unwrap().as_str();
            let line = line_at(&safe, whole.start());

            let mut node = Node::new(
                format!("uiview:{file_path}:{view_name}:{line}"),
                NodeKind::Class,
                view_name,
                format!("{file_path}::{view_name}"),
                file_path,
                Language::Swift,
                line,
                line,
            );
            node.end_column = whole.as_str().len() as u32;
            node.updated_at = now;
            nodes.push(node);
        }

        Some(FrameworkExtractionResult {
            nodes,
            references: Vec::new(),
        })
    }
}

// =============================================================================
// Vapor
// =============================================================================

/// TS `vaporResolver` (name: `"vapor"`).
#[derive(Debug, Default)]
pub struct VaporResolver;

impl FrameworkResolver for VaporResolver {
    fn name(&self) -> &str {
        "vapor"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Swift])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        // Check for Package.swift with Vapor dependency
        if let Some(package_swift) = context.read_file("Package.swift") {
            if !package_swift.is_empty() && package_swift.contains("vapor") {
                return true;
            }
        }

        // Check for Vapor imports
        let all_files = context.get_all_files();
        for file in &all_files {
            if file.ends_with(".swift") {
                if let Some(content) = context.read_file(file) {
                    if content.contains("import Vapor") {
                        return true;
                    }
                }
            }
        }

        false
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        // Pattern 1: Controller references
        if reference.reference_name.ends_with("Controller") {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                VAPOR_CONTROLLER_KINDS,
                VAPOR_CONTROLLER_DIRS,
                context,
            ) {
                return Some(framework_hit(reference, result, 0.85));
            }
        }

        // Pattern 2: Model references (Fluent)
        if PASCAL_RE.is_match(&reference.reference_name) {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                CLASS_KINDS,
                FLUENT_MODEL_DIRS,
                context,
            ) {
                return Some(framework_hit(reference, result, 0.75));
            }
        }

        // Pattern 3: Middleware references
        if reference.reference_name.ends_with("Middleware") {
            if let Some(result) = resolve_by_name_and_kind(
                &reference.reference_name,
                VAPOR_CONTROLLER_KINDS,
                VAPOR_MIDDLEWARE_DIRS,
                context,
            ) {
                return Some(framework_hit(reference, result, 0.8));
            }
        }

        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".swift") {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_millis();
        let safe = strip_comments_for_regex(content, CommentLang::Swift);

        // Build a group-var → path-prefix map first. Modern Vapor routes live on a
        // grouped builder (`let todos = routes.grouped("todos"); todos.get(use: index)`
        // or `routes.group("todos") { todos in todos.get(use: index) }`), so the path
        // comes from the group, not the call. Roots (app/routes/router) have no prefix.
        static SEG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#""([^"]*)""#).unwrap());
        let seg_join = |existing: &str, segs_str: &str| -> String {
            let mut out = existing.to_string();
            for caps in SEG_RE.captures_iter(segs_str) {
                out.push('/');
                out.push_str(caps.get(1).unwrap().as_str());
            }
            out
        };

        let mut group_prefix: HashMap<String, String> = HashMap::new();
        // let X = Y.grouped("a", "b")
        static GROUPED_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"\blet\s+(\w+)\s*=\s*(\w+)\.grouped\s*\(([^)]*)\)").unwrap()
        });
        for gm in GROUPED_RE.captures_iter(&safe) {
            let existing = group_prefix
                .get(gm.get(2).unwrap().as_str())
                .cloned()
                .unwrap_or_default();
            group_prefix.insert(
                gm.get(1).unwrap().as_str().to_string(),
                seg_join(&existing, gm.get(3).unwrap().as_str()),
            );
        }
        // Y.group("a") { X in ... }
        static GROUP_CLOSURE_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"\b(\w+)\.group\s*\(([^)]*)\)\s*\{\s*(\w+)\s+in").unwrap()
        });
        for gm in GROUP_CLOSURE_RE.captures_iter(&safe) {
            let existing = group_prefix
                .get(gm.get(1).unwrap().as_str())
                .cloned()
                .unwrap_or_default();
            group_prefix.insert(
                gm.get(3).unwrap().as_str().to_string(),
                seg_join(&existing, gm.get(2).unwrap().as_str()),
            );
        }

        // Vapor: <builder>.METHOD([path segs,] use: handler). Any receiver (app,
        // routes, or a grouped var); path segments optional and may be non-string
        // (`BlogUser.parameter`, `:id`, a path constant) so accept any comma-separated
        // args before `use:` — the label keeps only the string parts. `use:`
        // discriminates a real route from Environment.get("X")/req.parameters.get("X").
        static ROUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(
                r"\b(\w+)\.(get|post|put|patch|delete|head|options)\s*\(\s*((?:[^,()]+,\s*)*)use:\s*([A-Za-z_][\w.]*)",
            )
            .unwrap()
        });

        for caps in ROUTE_RE.captures_iter(&safe) {
            let whole = caps.get(0).unwrap();
            let receiver = caps.get(1).unwrap().as_str();
            let method = caps.get(2).unwrap().as_str();
            let segs_str = caps.get(3).unwrap().as_str();
            let handler_expr = caps.get(4).unwrap().as_str();
            let line = line_at(&safe, whole.start());
            let upper = method.to_uppercase();
            // TS: (groupPrefix.get(receiver) ?? '') + segJoin('', segsStr) || '/'
            let mut route_path = format!(
                "{}{}",
                group_prefix.get(receiver).cloned().unwrap_or_default(),
                seg_join("", segs_str)
            );
            if route_path.is_empty() {
                route_path = "/".to_string();
            }

            let mut route_node = Node::new(
                format!("route:{file_path}:{line}:{upper}:{route_path}"),
                NodeKind::Route,
                format!("{upper} {route_path}"),
                format!("{file_path}::route:{route_path}"),
                file_path,
                Language::Swift,
                line,
                line,
            );
            route_node.end_column = whole.as_str().len() as u32;
            route_node.updated_at = now;
            let route_node_id = route_node.id.clone();
            nodes.push(route_node);

            // Last segment of a dotted handler (self.list / UserController.list -> list)
            // (TS `if (handlerName)` — empty string is falsy, so skip it.)
            let handler_name = handler_expr
                .split('.')
                .next_back()
                .filter(|h| !h.is_empty());
            if let Some(handler_name) = handler_name {
                references.push(UnresolvedRef {
                    from_node_id: route_node_id,
                    reference_name: handler_name.to_string(),
                    reference_kind: EdgeKind::References,
                    line,
                    column: 0,
                    file_path: file_path.to_string(),
                    language: Language::Swift,
                    candidates: None,
                });
            }
        }

        Some(FrameworkExtractionResult { nodes, references })
    }
}
