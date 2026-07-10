//! GoFrame route metadata resolver.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::resolution::strip_comments::{CommentLang, strip_comments_for_regex};
use crate::resolution::types::{
    FrameworkExtractionResult,
    FrameworkResolver,
    ResolutionContext,
    ResolvedRef,
    UnresolvedRef,
};
use crate::types::{Language, Node, NodeKind};

static GOFRAME_META_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\btype\s+([A-Z]\w*)\s+struct\s*\{\s*g\.Meta\s+`([^`]*)`"#)
        .expect("valid GoFrame metadata regex")
});
static META_PATH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bpath:"([^"]+)""#).expect("valid GoFrame path regex"));
static META_METHOD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\bmethod:"([^"]+)""#).expect("valid GoFrame method regex"));
static GO_PACKAGE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*package\s+(\w+)").expect("valid Go package regex"));

pub const GOFRAME_ROUTE_MARKER: &str = "::goframe-route:";

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn line_at(source: &str, offset: usize) -> u32 {
    source[..offset]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32
        + 1
}

#[derive(Debug, Default)]
pub struct GoFrameResolver;

impl FrameworkResolver for GoFrameResolver {
    fn name(&self) -> &str {
        "goframe"
    }

    fn languages(&self) -> Option<&[Language]> {
        Some(&[Language::Go])
    }

    fn detect(&self, context: &dyn ResolutionContext) -> bool {
        context
            .read_file("go.mod")
            .is_some_and(|go_mod| go_mod.contains("github.com/gogf/gf"))
    }

    fn resolve(
        &self,
        _reference: &UnresolvedRef,
        _context: &dyn ResolutionContext,
    ) -> Option<ResolvedRef> {
        None
    }

    fn extract(&self, file_path: &str, content: &str) -> Option<FrameworkExtractionResult> {
        if !file_path.ends_with(".go") || !content.contains("g.Meta") {
            return Some(FrameworkExtractionResult::default());
        }

        let safe = strip_comments_for_regex(content, CommentLang::Go);
        let package = GO_PACKAGE_RE
            .captures(&safe)
            .and_then(|captures| captures.get(1))
            .map(|capture| capture.as_str());
        let now = now_ms();
        let mut nodes = Vec::new();

        for captures in GOFRAME_META_RE.captures_iter(&safe) {
            let whole = captures.get(0).expect("whole GoFrame metadata match");
            let request_type = captures.get(1).expect("request type").as_str();
            let tag = captures.get(2).expect("g.Meta tag").as_str();
            let Some(route_path) = META_PATH_RE
                .captures(tag)
                .and_then(|path| path.get(1))
                .map(|path| path.as_str())
            else {
                continue;
            };
            let method = META_METHOD_RE
                .captures(tag)
                .and_then(|method| method.get(1))
                .map(|method| method.as_str().to_uppercase())
                .unwrap_or_else(|| "ANY".to_string());
            let line = line_at(&safe, whole.start());
            let join_key = package
                .map(|package| format!("{package}.{request_type}"))
                .unwrap_or_else(|| request_type.to_string());

            let mut route = Node::new(
                format!("route:{file_path}:{line}:{method}:{route_path}"),
                NodeKind::Route,
                format!("{method} {route_path}"),
                format!("{file_path}{GOFRAME_ROUTE_MARKER}{join_key}"),
                file_path,
                Language::Go,
                line,
                line,
            );
            route.start_column = 0;
            route.end_column = whole.as_str().len() as u32;
            route.updated_at = now;
            nodes.push(route);
        }

        Some(FrameworkExtractionResult {
            nodes,
            references: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_request_metadata_as_routes() {
        let source = r#"package cash

type ListReq struct {
    g.Meta `path:"/cash/list" method:"get" tags:"Cash"`
}

type ListRes struct {
    g.Meta `mime:"application/json"`
}
"#;
        let result = GoFrameResolver
            .extract("api/cash/list.go", source)
            .expect("extract hook is implemented");
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "GET /cash/list");
        assert_eq!(
            result.nodes[0].qualified_name,
            "api/cash/list.go::goframe-route:cash.ListReq"
        );
    }

    #[test]
    fn defaults_missing_method_to_any_and_ignores_comments() {
        let source = r#"package user
// type FakeReq struct { g.Meta `path:"/fake" method:"post"` }
type RealReq struct { g.Meta `path:"/real"` }
"#;
        let result = GoFrameResolver
            .extract("api/user.go", source)
            .expect("extract hook is implemented");
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].name, "ANY /real");
    }
}
