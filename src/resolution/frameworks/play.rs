//! Play Framework (Scala/Java) resolver.
//!
//! Ported from `src/resolution/frameworks/play.ts`.
//!
//! Play declares HTTP routes in a dedicated `conf/routes` file (and included
//! `conf/*.routes`), Rails-style:
//!
//! ```text
//! GET   /computers        controllers.Application.list(p: Int ?= 0)
//! POST  /computers        controllers.Application.save
//! GET   /assets/*file     controllers.Assets.versioned(path = "/public", file: Asset)
//! ```
//!
//! The file is extensionless, so the file walk only indexes it because
//! `is_play_routes_file` (grammars.rs) opts it in; it's processed through the
//! no-grammar path and this resolver extracts the routes. Each route references
//! its handler as `Controller.method` (the package prefix is dropped), resolved
//! to the action method in the controller class.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::extraction::grammars::is_play_routes_file;
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

static ROUTE_LINE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\s+(\S+)\s+(.+)$").unwrap()
});
static BUILD_SBT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)playframework|"play"|sbt-plugin|PlayScala|PlayJava"#).unwrap()
});
static HANDLER_CLAIM_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*\.[A-Za-z_]\w*$").unwrap());
static HANDLER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([A-Za-z_]\w*)\.([A-Za-z_]\w*)$").unwrap());

const METHOD_KINDS: &[NodeKind] = &[NodeKind::Method, NodeKind::Function];
const CLASS_KINDS: &[NodeKind] = &[NodeKind::Class];

/// Play framework resolver (TS `playResolver`).
pub struct PlayResolver;

impl FrameworkResolver for PlayResolver {
    fn name(&self) -> &str {
        "play"
    }

    /// `yaml` so this resolver runs on conf/routes (detectLanguage maps it to yaml);
    /// `scala`/`java` so it's active in Play projects of either language.
    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Scala, Language::Java, Language::Yaml])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        if let Some(build_sbt) = context.read_file("build.sbt") {
            if BUILD_SBT_RE.is_match(&build_sbt) {
                return true;
            }
        }
        if context.file_exists("conf/routes") {
            return true;
        }
        if context.file_exists("conf/application.conf") {
            return true;
        }
        false
    }

    /// The handler is `Controller.method` (a class-qualified action), which names no
    /// bare declared symbol, so resolveOne's pre-filter could drop it — claim it.
    fn claims_reference(&self, name: &str) -> bool {
        HANDLER_CLAIM_RE.is_match(name)
    }

    fn resolve(
        &self,
        reference: &UnresolvedRef,
        context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        let m = HANDLER_RE.captures(&reference.reference_name)?;
        let class_name = &m[1];
        let method_name = &m[2];
        let class_nodes: Vec<Node> = context
            .get_nodes_by_name(class_name)
            .into_iter()
            .filter(|n| CLASS_KINDS.contains(&n.kind))
            .collect();
        for cls in &class_nodes {
            if let Some(method) = context
                .get_nodes_in_file(&cls.file_path)
                .into_iter()
                .find(|n| METHOD_KINDS.contains(&n.kind) && n.name == method_name)
            {
                return Some(ResolvedRef {
                    original: reference.clone(),
                    target_node_id: method.id,
                    confidence: 0.9,
                    resolved_by: ResolvedBy::Framework,
                });
            }
        }
        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !is_play_routes_file(file_path) {
            return Some(FrameworkExtractionResult::default());
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut references: Vec<UnresolvedRef> = Vec::new();
        let now = now_millis();

        for (i, raw_line) in content.split('\n').enumerate() {
            let line = raw_line.trim();
            // Skip comments and `->` route includes (a sub-router mount, not an action).
            if line.is_empty() || line.starts_with('#') || line.starts_with("->") {
                continue;
            }
            let Some(m) = ROUTE_LINE.captures(line) else {
                continue;
            };
            let method = m.get(1).unwrap().as_str();
            let route_path = m.get(2).unwrap().as_str();
            let action = m.get(3).unwrap().as_str();

            // action: `controllers.Application.list(p: Int ?= 0)` → drop args, keep the
            // last `Controller.method` segment (package prefix is irrelevant for lookup).
            let fqn = action.split('(').next().unwrap_or("").trim();
            let parts: Vec<&str> = fqn.split('.').filter(|p| !p.is_empty()).collect();
            if parts.len() < 2 {
                continue;
            }
            let handler_ref = parts[parts.len() - 2..].join("."); // Application.list

            let line_num = i as u32 + 1;
            let mut route_node = Node::new(
                format!("route:{file_path}:{line_num}:{method}:{route_path}"),
                NodeKind::Route,
                format!("{method} {route_path}"),
                format!("{file_path}::{method}:{route_path}"),
                file_path,
                Language::Scala,
                line_num,
                line_num,
            );
            route_node.updated_at = now;
            let route_id = route_node.id.clone();
            nodes.push(route_node);
            references.push(UnresolvedRef {
                from_node_id: route_id,
                reference_name: handler_ref,
                reference_kind: EdgeKind::References,
                line: line_num,
                column: 0,
                file_path: file_path.to_string(),
                language: Language::Scala,
                candidates: None,
            });
        }

        Some(FrameworkExtractionResult { nodes, references })
    }
}
