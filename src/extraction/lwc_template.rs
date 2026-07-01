//! LwcTemplateExtractor — HTML files, with LWC template bindings.
//!
//! Generic HTML gets a file node only (Twig/YAML-style file tracking — the
//! watcher needs the row, there are no symbols worth extracting). Lightning
//! Web Component templates (any `.html` inside an `lwc/` bundle directory)
//! additionally emit one `references` per unique `{binding}` expression —
//! LWC templates allow only plain property paths (`{prop}`, `{prop.sub}`,
//! `onclick={handler}`), no operators or calls, so a regex scan is exact.
//! The first segment names a member of the component's JS class; the
//! Salesforce framework resolver binds it to the sibling `<stem>.js` (or
//! the bundle's `<dir>.js`) class member, producing the template → JS edge.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::types::{EdgeKind, ExtractionResult, Language, Node, NodeKind, UnresolvedReference};

/// `{ident}` / `{ident.path}` — LWC binding expressions. Whole-brace match:
/// anything containing operators, quotes, or spaces inside the braces (CSS,
/// JSON examples in docs HTML, …) fails the match and is skipped.
static BINDING_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\s*([A-Za-z_$][\w$]*)(?:\.[\w$]+)*\s*\}").expect("valid regex")
});

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before epoch")
        .as_millis() as i64
}

pub struct LwcTemplateExtractor<'a> {
    file_path: String,
    source: &'a str,
}

impl<'a> LwcTemplateExtractor<'a> {
    pub fn new(file_path: impl Into<String>, source: &'a str) -> Self {
        LwcTemplateExtractor {
            file_path: file_path.into(),
            source,
        }
    }

    pub fn extract(self) -> ExtractionResult {
        let start_time = std::time::Instant::now();
        let mut result = ExtractionResult::default();

        let file_node_id = self.create_file_node(&mut result);

        if is_lwc_template(&self.file_path) {
            self.extract_bindings(&file_node_id, &mut result);
        }

        result.duration_ms = start_time.elapsed().as_millis() as f64;
        result
    }

    fn create_file_node(&self, result: &mut ExtractionResult) -> String {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let id = generate_node_id(&self.file_path, NodeKind::File, &self.file_path, 1);
        let name = self
            .file_path
            .split('/')
            .next_back()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.file_path)
            .to_string();

        let mut node = Node::new(
            id.clone(),
            NodeKind::File,
            name,
            self.file_path.clone(),
            self.file_path.clone(),
            Language::Html,
            1,
            lines.len().max(1) as u32,
        );
        node.end_column = lines.last().map(|l| l.len()).unwrap_or(0) as u32;
        node.start_byte = Some(0);
        node.end_byte = Some(self.source.len() as u32);
        node.updated_at = now_ms();
        result.nodes.push(node);
        id
    }

    fn extract_bindings(&self, file_node_id: &str, result: &mut ExtractionResult) {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for caps in BINDING_RE.captures_iter(self.source) {
            let ident = caps.get(1).expect("group 1").as_str();
            // One reference per unique member — templates repeat bindings.
            if !seen.insert(ident) {
                continue;
            }
            let offset = caps.get(0).expect("match").start();
            let line = self.source[..offset]
                .bytes()
                .filter(|&b| b == b'\n')
                .count() as u32
                + 1;
            result.unresolved_references.push(UnresolvedReference {
                from_node_id: file_node_id.to_string(),
                reference_name: ident.to_string(),
                reference_kind: EdgeKind::References,
                line,
                column: 0,
                file_path: None,
                language: Some(Language::Html),
                candidates: None,
                metadata: None,
            });
        }
    }
}

/// An `.html` file inside an LWC bundle (`…/lwc/<component>/…`).
pub fn is_lwc_template(file_path: &str) -> bool {
    file_path.contains("/lwc/") || file_path.starts_with("lwc/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lwc_template_emits_bindings() {
        let source = "<template>\n    <template lwc:if={isLoaded}>\n        <p onclick={handleClick}>{account.Name}</p>\n        <template for:each={accounts} for:item=\"acc\">\n            <span key={acc.Id}>{acc.Name}</span>\n        </template>\n    </template>\n    <p>{isLoaded}</p>\n</template>\n";
        let result = LwcTemplateExtractor::new(
            "force-app/main/default/lwc/accountList/accountList.html",
            source,
        )
        .extract();

        let file = &result.nodes[0];
        assert_eq!(file.kind, NodeKind::File);
        assert_eq!(file.language, Language::Html);

        let names: Vec<&str> = result
            .unresolved_references
            .iter()
            .map(|r| r.reference_name.as_str())
            .collect();
        // Unique first segments only — `isLoaded` appears twice but is
        // emitted once; `account.Name` binds via its first segment.
        assert_eq!(
            names,
            vec!["isLoaded", "handleClick", "account", "accounts", "acc"]
        );
        for r in &result.unresolved_references {
            assert_eq!(r.from_node_id, file.id);
            assert_eq!(r.reference_kind, EdgeKind::References);
            assert_eq!(r.language, Some(Language::Html));
        }
        // Line attribution: handleClick is on line 3.
        let click = result
            .unresolved_references
            .iter()
            .find(|r| r.reference_name == "handleClick")
            .unwrap();
        assert_eq!(click.line, 3);
    }

    #[test]
    fn generic_html_emits_only_file_node() {
        let source = "<!doctype html>\n<html><body><h1>Docs</h1>\n<style>.x { color: red }</style>\n</body></html>\n";
        let result = LwcTemplateExtractor::new("docs/index.html", source).extract();
        assert_eq!(result.nodes.len(), 1);
        assert_eq!(result.nodes[0].kind, NodeKind::File);
        assert!(
            result.unresolved_references.is_empty(),
            "generic html must not emit refs: {:?}",
            result.unresolved_references
        );
    }
}
