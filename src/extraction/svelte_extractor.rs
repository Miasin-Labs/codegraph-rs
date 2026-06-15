//! SvelteExtractor — port of `src/extraction/svelte-extractor.ts`.

use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};

use regex::Regex;

use crate::extraction::grammars::is_language_supported;
use crate::extraction::tree_sitter_helpers::generate_node_id;
use crate::extraction::tree_sitter_types::LanguageExtractor;
use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
use crate::types::{
    Edge,
    EdgeKind,
    ExtractionError,
    ExtractionResult,
    Language,
    Node,
    NodeKind,
    Severity,
    UnresolvedReference,
};

/// Svelte 5 rune names — compiler builtins, not real functions
const SVELTE_RUNES: &[&str] = &[
    "$props",
    "$state",
    "$derived",
    "$effect",
    "$bindable",
    "$inspect",
    "$host",
    "$snippet",
];

/// Lookup used to obtain the per-language config for `<script>` block
/// delegation. The TS constructor did `EXTRACTORS[language]` internally;
/// natively the caller injects it (typically `languages::extractor_for`) so
/// this module stays a leaf — mirroring how `TreeSitterExtractor::new`
/// receives its extractor.
pub type ScriptExtractorLookup = fn(Language) -> Option<&'static dyn LanguageExtractor>;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn count_newlines(s: &str) -> usize {
    s.bytes().filter(|&b| b == b'\n').count()
}

/// TS: `/<script(\s[^>]*)?>(?<content>[\s\S]*?)<\/script>/g`
static SCRIPT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)<script(\s[^>]*)?>(?P<content>.*?)</script>").unwrap());

/// TS: `/lang\s*=\s*["'](ts|typescript)["']/`
static LANG_TS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"lang\s*=\s*["'](ts|typescript)["']"#).unwrap());

/// TS: `/context\s*=\s*["']module["']/`
static CONTEXT_MODULE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"context\s*=\s*["']module["']"#).unwrap());

/// TS: `/<(script|style)(\s[^>]*)?>[\s\S]*?<\/\1>/g` — the backreference is
/// expanded into an equivalent two-branch alternation (the `regex` crate has
/// no backreferences; with only two alternatives this is exactly equivalent).
static SCRIPT_STYLE_BLOCK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)<script(\s[^>]*)?>.*?</script>|<style(\s[^>]*)?>.*?</style>").unwrap()
});

/// TS: `/\{([^}#/:@][^}]*)\}/g`
static EXPR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{([^}#/:@][^}]*)\}").unwrap());

/// TS: `/\b([a-zA-Z_$][\w$.]*)\s*\(/g` (`\w` in JS is ASCII; `(?-u:\b)`
/// reproduces the ASCII word boundary).
static CALL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u:\b)([A-Za-z_$][0-9A-Za-z_$.]*)\s*\(").unwrap());

/// TS: `/<([A-Z][a-zA-Z0-9_$]*)\b/g`
static COMPONENT_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([A-Z][a-zA-Z0-9_$]*)(?-u:\b)").unwrap());

/// A `<script>` block extracted from the Svelte source.
struct ScriptBlock<'a> {
    content: &'a str,
    /// 0-indexed line where the script content starts (line after `<script>`).
    start_line: u32,
    /// Byte offset of `content` within the full `.svelte` source. Used to
    /// remap the inner extraction's byte ranges back to whole-file offsets.
    start_byte: u32,
    /// Computed for parity with TS, which stores but never reads it.
    #[allow(dead_code)]
    is_module: bool,
    is_typescript: bool,
}

/// SvelteExtractor - Extracts code relationships from Svelte component files
///
/// Svelte files are multi-language (script + template + style). Rather than
/// parsing the full Svelte grammar, we extract the `<script>` block content
/// and delegate it to the TypeScript/JavaScript TreeSitterExtractor.
///
/// Also extracts function calls from template expressions (`{fn(...)}`) so
/// cross-file call edges are captured even when calls live in markup.
///
/// Every .svelte file produces a component node (Svelte components are always importable).
pub struct SvelteExtractor<'a> {
    file_path: String,
    source: &'a str,
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    unresolved_references: Vec<UnresolvedReference>,
    errors: Vec<ExtractionError>,
    script_extractor_lookup: ScriptExtractorLookup,
}

impl<'a> SvelteExtractor<'a> {
    pub fn new(
        file_path: impl Into<String>,
        source: &'a str,
        script_extractor_lookup: ScriptExtractorLookup,
    ) -> Self {
        SvelteExtractor {
            file_path: file_path.into(),
            source,
            nodes: Vec::new(),
            edges: Vec::new(),
            unresolved_references: Vec::new(),
            errors: Vec::new(),
            script_extractor_lookup,
        }
    }

    /// Extract from Svelte source
    pub fn extract(mut self) -> ExtractionResult {
        let start_time = now_ms();

        // Create component node for the .svelte file itself
        let component_node = self.create_component_node();

        // Extract and process script blocks
        let script_blocks = self.extract_script_blocks();

        for block in &script_blocks {
            self.process_script_block(block, &component_node.id);
        }

        // Extract function calls from template expressions ({fn(...)})
        self.extract_template_calls(&component_node.id, &script_blocks);

        // Extract component usages from template (<ComponentName>)
        self.extract_template_components(&component_node.id);

        // Filter out Svelte rune calls ($state, $props, $derived, etc.)
        self.unresolved_references
            .retain(|r| !SVELTE_RUNES.contains(&r.reference_name.as_str()));

        ExtractionResult {
            nodes: self.nodes,
            edges: self.edges,
            unresolved_references: self.unresolved_references,
            errors: self.errors,
            duration_ms: (now_ms() - start_time) as f64,
        }
    }

    /// Create a component node for the .svelte file
    fn create_component_node(&mut self) -> Node {
        let lines: Vec<&str> = self.source.split('\n').collect();
        let last_segment = self
            .file_path
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default();
        // TS: `split(/[/\\]/).pop() || this.filePath` — "" is falsy.
        let file_name = if last_segment.is_empty() {
            self.file_path.as_str()
        } else {
            last_segment
        };
        let component_name = file_name.strip_suffix(".svelte").unwrap_or(file_name);
        let id = generate_node_id(&self.file_path, NodeKind::Component, component_name, 1);

        let node = Node {
            id,
            kind: NodeKind::Component,
            name: component_name.to_string(),
            qualified_name: format!("{}::{}", self.file_path, component_name),
            file_path: self.file_path.clone(),
            language: Language::Svelte,
            start_line: 1,
            end_line: lines.len() as u32,
            start_column: 0,
            end_column: lines.last().map(|l| l.len() as u32).unwrap_or(0),
            // The component node spans the whole .svelte file by definition.
            start_byte: Some(0),
            end_byte: Some(self.source.len() as u32),
            address: None,
            size: None,
            docstring: None,
            signature: None,
            visibility: None,
            is_exported: Some(true), // Svelte components are always importable
            is_async: None,
            is_static: None,
            is_abstract: None,
            decorators: None,
            type_parameters: None,
            updated_at: now_ms(),
        };

        self.nodes.push(node.clone());
        node
    }

    /// Extract `<script>` blocks from the Svelte source
    fn extract_script_blocks(&self) -> Vec<ScriptBlock<'a>> {
        let mut blocks: Vec<ScriptBlock<'a>> = Vec::new();

        for caps in SCRIPT_RE.captures_iter(self.source) {
            let attrs = caps.get(1).map(|m| m.as_str()).unwrap_or("");
            let content = caps.name("content").map(|m| m.as_str()).unwrap_or("");

            // Detect TypeScript from lang attribute
            let is_typescript = LANG_TS_RE.is_match(attrs);

            // Detect module script
            let is_module = CONTEXT_MODULE_RE.is_match(attrs);

            // Calculate start line of the script content (line after <script>)
            let full = caps.get(0).unwrap();
            let before_script = &self.source[..full.start()];
            let script_tag_line = count_newlines(before_script);
            // The content starts on the line after the opening <script> tag
            let gt_end = full.as_str().find('>').map(|i| i + 1).unwrap_or(0);
            let opening_tag = &full.as_str()[..gt_end];
            let opening_tag_lines = count_newlines(opening_tag);
            let content_start_line = script_tag_line + opening_tag_lines + 1; // 0-indexed line

            blocks.push(ScriptBlock {
                content,
                start_line: content_start_line as u32,
                start_byte: caps.name("content").map(|m| m.start() as u32).unwrap_or(0),
                is_module,
                is_typescript,
            });
        }

        blocks
    }

    /// Process a script block by delegating to TreeSitterExtractor
    fn process_script_block(&mut self, block: &ScriptBlock<'a>, component_node_id: &str) {
        let script_language = if block.is_typescript {
            Language::Typescript
        } else {
            Language::Javascript
        };

        // Check if the script language parser is available
        if !is_language_supported(script_language) {
            self.errors.push(ExtractionError {
                message: format!(
                    "Parser for {} not available, cannot parse Svelte script block",
                    script_language
                ),
                file_path: None,
                line: None,
                column: None,
                severity: Severity::Warning,
                code: None,
            });
            return;
        }

        // Delegate to TreeSitterExtractor
        let extractor = (self.script_extractor_lookup)(script_language);
        let result = TreeSitterExtractor::new(
            self.file_path.clone(),
            block.content,
            Some(script_language),
            extractor,
        )
        .extract();

        // Offset line numbers from script block back to .svelte file positions
        for mut node in result.nodes {
            node.start_line += block.start_line;
            node.end_line += block.start_line;
            // Byte offsets from the inner extraction are relative to the
            // script slice; shift them to whole-file offsets. `content` is a
            // byte slice of the original source, so the shift is exact.
            node.start_byte = node.start_byte.map(|b| b + block.start_byte);
            node.end_byte = node.end_byte.map(|b| b + block.start_byte);
            node.language = Language::Svelte; // Mark as svelte, not TS/JS

            let target = node.id.clone();
            self.nodes.push(node);

            // Add containment edge from component to this node
            self.edges
                .push(Edge::new(component_node_id, target, EdgeKind::Contains));
        }

        // Offset edges (they reference line numbers)
        for mut edge in result.edges {
            // TS: `if (edge.line)` — 0 is falsy and stays untouched.
            if let Some(line) = edge.line {
                if line != 0 {
                    edge.line = Some(line + block.start_line);
                }
            }
            self.edges.push(edge);
        }

        // Offset unresolved references
        for mut reference in result.unresolved_references {
            reference.line += block.start_line;
            reference.file_path = Some(self.file_path.clone());
            reference.language = Some(Language::Svelte);
            self.unresolved_references.push(reference);
        }

        // Carry over errors
        for mut error in result.errors {
            // TS: `if (error.line)` — 0 is falsy and stays untouched.
            if let Some(line) = error.line {
                if line != 0 {
                    error.line = Some(line + block.start_line);
                }
            }
            self.errors.push(error);
        }
    }

    /// Build a set of line ranges covered by `<script>` and `<style>` blocks
    /// (0-indexed, inclusive) so template scanning can skip them.
    fn covered_block_ranges(&self) -> Vec<(usize, usize)> {
        let mut covered_ranges: Vec<(usize, usize)> = Vec::new();
        for m in SCRIPT_STYLE_BLOCK_RE.find_iter(self.source) {
            let start_line = count_newlines(&self.source[..m.start()]);
            let end_line = start_line + count_newlines(m.as_str());
            covered_ranges.push((start_line, end_line));
        }
        covered_ranges
    }

    /// Extract function calls from Svelte template expressions.
    ///
    /// In Svelte, many function calls happen in markup (e.g., `class={cn(...)}`),
    /// not inside `<script>` blocks. We scan the template portion for `{expression}`
    /// blocks and extract call patterns from them.
    fn extract_template_calls(
        &mut self,
        component_node_id: &str,
        _script_blocks: &[ScriptBlock<'a>],
    ) {
        // Build a set of line ranges covered by <script> and <style> blocks so we skip them
        let covered_ranges = self.covered_block_ranges();

        // Find template expressions: {...} outside of script/style blocks
        // Matches curly-brace expressions, excluding Svelte block syntax
        // ({#if}, {:else}, {/if}, {@html}, {@render})
        let source = self.source;
        for (line_idx, line) in source.split('\n').enumerate() {
            // Skip lines inside script/style blocks
            if covered_ranges
                .iter()
                .any(|&(start, end)| line_idx >= start && line_idx <= end)
            {
                continue;
            }

            for expr_caps in EXPR_RE.captures_iter(line) {
                let expr_start = expr_caps.get(0).unwrap().start();
                let expr = expr_caps.get(1).unwrap().as_str();
                // Extract function calls: identifiers followed by (
                // Matches: cn(...), buttonVariants(...), obj.method(...)
                for call_caps in CALL_RE.captures_iter(expr) {
                    let callee_name = call_caps.get(1).unwrap().as_str();
                    // Skip Svelte runes, control flow keywords, and common non-function patterns
                    if SVELTE_RUNES.contains(&callee_name) {
                        continue;
                    }
                    if matches!(callee_name, "if" | "else" | "each" | "await") {
                        continue;
                    }

                    self.unresolved_references.push(UnresolvedReference {
                        from_node_id: component_node_id.to_string(),
                        reference_name: callee_name.to_string(),
                        reference_kind: EdgeKind::Calls,
                        line: (line_idx + 1) as u32, // 1-indexed
                        column: (expr_start + call_caps.get(0).unwrap().start()) as u32,
                        file_path: Some(self.file_path.clone()),
                        language: Some(Language::Svelte),
                        candidates: None,
                    });
                }
            }
        }
    }

    /// Extract component usages from the Svelte template.
    ///
    /// PascalCase tags like `<Modal>`, `<Button />`, `<DevServerPreview>` represent
    /// component instantiations — analogous to function calls in imperative code.
    /// Capturing these creates graph edges from parent to child components and
    /// gives codegraph_explore anchor points in the template markup.
    fn extract_template_components(&mut self, component_node_id: &str) {
        // Build ranges covered by <script> and <style> blocks to skip them
        let covered_ranges = self.covered_block_ranges();

        let source = self.source;
        // Match PascalCase opening/self-closing tags (closing tags </Foo> start with </ so won't match)
        for (line_idx, line) in source.split('\n').enumerate() {
            if covered_ranges
                .iter()
                .any(|&(start, end)| line_idx >= start && line_idx <= end)
            {
                continue;
            }

            for caps in COMPONENT_TAG_RE.captures_iter(line) {
                let component_name = caps.get(1).unwrap().as_str();

                self.unresolved_references.push(UnresolvedReference {
                    from_node_id: component_node_id.to_string(),
                    reference_name: component_name.to_string(),
                    reference_kind: EdgeKind::References,
                    line: (line_idx + 1) as u32, // 1-indexed
                    column: (caps.get(0).unwrap().start() + 1) as u32,
                    file_path: Some(self.file_path.clone()),
                    language: Some(Language::Svelte),
                    candidates: None,
                });
            }
        }
    }
}

/// Minimal TS/JS `LanguageExtractor` shared by the svelte/vue extractor tests
/// (a faithful subset of `src/extraction/languages/typescript.ts`, copied from
/// the reference implementation in `tree_sitter_wrapper.rs` tests). The real
/// `languages/` module is ported by a separate task; tests inject this via
/// the `ScriptExtractorLookup` parameter.
#[cfg(test)]
pub(crate) mod test_support {
    use crate::extraction::tree_sitter_helpers::get_node_text;
    use crate::extraction::tree_sitter_types::{
        ImportInfo,
        ImportOutcome,
        LanguageExtractor,
        SyntaxNode,
    };
    use crate::types::Language;

    pub(crate) struct TsLikeScriptExtractor;

    impl LanguageExtractor for TsLikeScriptExtractor {
        fn function_types(&self) -> &[&str] {
            &[
                "function_declaration",
                "arrow_function",
                "function_expression",
            ]
        }
        fn class_types(&self) -> &[&str] {
            &["class_declaration", "abstract_class_declaration"]
        }
        fn method_types(&self) -> &[&str] {
            &["method_definition", "public_field_definition"]
        }
        fn interface_types(&self) -> &[&str] {
            &["interface_declaration"]
        }
        fn struct_types(&self) -> &[&str] {
            &[]
        }
        fn enum_types(&self) -> &[&str] {
            &["enum_declaration"]
        }
        fn enum_member_types(&self) -> &[&str] {
            &["property_identifier", "enum_assignment"]
        }
        fn type_alias_types(&self) -> &[&str] {
            &["type_alias_declaration"]
        }
        fn import_types(&self) -> &[&str] {
            &["import_statement"]
        }
        fn call_types(&self) -> &[&str] {
            &["call_expression"]
        }
        fn variable_types(&self) -> &[&str] {
            &["lexical_declaration", "variable_declaration"]
        }
        fn name_field(&self) -> &str {
            "name"
        }
        fn body_field(&self) -> &str {
            "body"
        }
        fn params_field(&self) -> &str {
            "parameters"
        }
        fn return_field(&self) -> Option<&str> {
            Some("return_type")
        }
        fn is_exported(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
            let mut current = node.parent();
            while let Some(p) = current {
                if p.kind() == "export_statement" {
                    return Some(true);
                }
                current = p.parent();
            }
            Some(false)
        }
        fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
            if node.kind() == "lexical_declaration" {
                for i in 0..node.child_count() as u32 {
                    if let Some(c) = node.child(i) {
                        if c.kind() == "const" {
                            return Some(true);
                        }
                    }
                }
            }
            Some(false)
        }
        fn extract_import(&self, node: SyntaxNode<'_>, source: &str) -> ImportOutcome {
            if let Some(source_field) = node.child_by_field_name("source") {
                let module_name = get_node_text(source_field, source).replace(['\'', '"'], "");
                if !module_name.is_empty() {
                    return ImportOutcome::Info(ImportInfo::new(
                        module_name,
                        get_node_text(node, source).trim(),
                    ));
                }
            }
            ImportOutcome::Declined
        }
    }

    static TS_LIKE: TsLikeScriptExtractor = TsLikeScriptExtractor;

    /// `ScriptExtractorLookup` returning the TS-like config for any language.
    pub(crate) fn test_lookup(_language: Language) -> Option<&'static dyn LanguageExtractor> {
        Some(&TS_LIKE)
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::test_lookup;
    use super::*;

    fn extract(file_path: &str, source: &str) -> ExtractionResult {
        SvelteExtractor::new(file_path, source, test_lookup).extract()
    }

    /// Template-only .svelte file → exactly one component node, with the exact
    /// TS-computed node id (parity vector captured from the TS implementation).
    #[test]
    fn creates_component_node_for_template_only_file() {
        let result = extract("Static.svelte", "<h1>hello</h1>\n");
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);
        assert_eq!(result.nodes.len(), 1);

        let component = &result.nodes[0];
        assert_eq!(component.kind, NodeKind::Component);
        assert_eq!(component.name, "Static");
        assert_eq!(component.qualified_name, "Static.svelte::Static");
        assert_eq!(component.file_path, "Static.svelte");
        assert_eq!(component.language, Language::Svelte);
        assert_eq!(component.start_line, 1);
        assert_eq!(component.end_line, 2);
        assert_eq!(component.start_column, 0);
        assert_eq!(component.end_column, 0);
        assert_eq!(component.is_exported, Some(true));
        // Exact id parity with the TS implementation (probed on the same input).
        assert_eq!(component.id, "component:6fc2ade5f7dcfd79726083ba2bf1203a");
    }

    /// Script-block delegation: nodes/edges/refs are offset back to .svelte
    /// file positions, marked `language: svelte`, and the component contains
    /// every script node. All line/column expectations are parity vectors
    /// probed from the TS implementation on the identical fixture.
    #[test]
    fn extracts_script_block_with_offsets_and_template_calls() {
        let source = "<script lang=\"ts\">\nfunction greet(name: string): string {\n  return hello(name);\n}\nconst count = 0;\n</script>\n\n<h1>{cn('a', 'b')}</h1>\n{#if visible}\n  <Modal title={fmt(x)} />\n{/if}\n<style>\n  h1 { color: red; }\n</style>\n";
        let result = extract("src/App.svelte", source);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let summary: Vec<String> = result
            .nodes
            .iter()
            .map(|n| {
                format!(
                    "{}:{}@{}-{}",
                    n.kind.as_str(),
                    n.name,
                    n.start_line,
                    n.end_line
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                "component:App@1-15",
                "file:App.svelte@2-7",
                "function:greet@3-5",
                "constant:count@6-6",
            ]
        );
        for node in &result.nodes {
            assert_eq!(node.language, Language::Svelte);
        }

        // 2 inner contains (file→greet, file→count) + 3 component contains.
        let component = &result.nodes[0];
        assert_eq!(result.edges.len(), 5);
        let component_contains: Vec<_> = result
            .edges
            .iter()
            .filter(|e| e.source == component.id && e.kind == EdgeKind::Contains)
            .collect();
        assert_eq!(component_contains.len(), 3);

        let refs: Vec<String> = result
            .unresolved_references
            .iter()
            .map(|r| {
                format!(
                    "{}:{}@{}:{}",
                    r.reference_kind.as_str(),
                    r.reference_name,
                    r.line,
                    r.column
                )
            })
            .collect();
        assert_eq!(
            refs,
            vec![
                "calls:hello@4:9",
                "calls:cn@8:4",
                "calls:fmt@10:15",
                "references:Modal@10:3",
            ]
        );
        for r in &result.unresolved_references {
            assert_eq!(r.file_path.as_deref(), Some("src/App.svelte"));
            assert_eq!(r.language, Some(Language::Svelte));
        }
        // Template refs hang off the component node.
        let cn_ref = &result.unresolved_references[1];
        assert_eq!(cn_ref.from_node_id, component.id);
    }

    /// Svelte runes are filtered from unresolved references; control-flow
    /// blocks ({#if}, {:else}, {@html}, {/if}) produce no template calls.
    #[test]
    fn filters_runes_and_skips_control_flow_blocks() {
        let source = "<script>\n  let count = $state(0);\n  let doubled = $derived(count * 2);\n  function init() { setup(); }\n</script>\n{#if visible}\n  {@html raw}\n  {:else}\n  {format(count)}\n{/if}\n<MyWidget prop={compute(1)} />\n";
        let result = extract("Widget.svelte", source);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let refs: Vec<String> = result
            .unresolved_references
            .iter()
            .map(|r| {
                format!(
                    "{}:{}@{}:{}",
                    r.reference_kind.as_str(),
                    r.reference_name,
                    r.line,
                    r.column
                )
            })
            .collect();
        assert_eq!(
            refs,
            vec![
                "calls:setup@5:20",
                "calls:format@9:2",
                "calls:compute@11:15",
                "references:MyWidget@11:1",
            ]
        );

        let names: Vec<&str> = result.nodes.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["Widget", "Widget.svelte", "count", "doubled", "init"]
        );
    }

    /// Multiline opening tags and `context="module"` scripts: both script
    /// blocks are processed; line offsets match the TS implementation.
    #[test]
    fn handles_multiline_opening_tag_and_multiple_scripts() {
        let source = "<script\n  context=\"module\"\n  lang=\"ts\">\nexport function load() { return fetchData(); }\n</script>\n<script lang=\"ts\">\nfunction local() {}\n</script>\n<p>hi</p>\n";
        let result = extract("lib/Page.svelte", source);
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let summary: Vec<String> = result
            .nodes
            .iter()
            .map(|n| {
                format!(
                    "{}:{}@{}-{}:exp={:?}",
                    n.kind.as_str(),
                    n.name,
                    n.start_line,
                    n.end_line,
                    n.is_exported
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                "component:Page@1-10:exp=Some(true)",
                "file:Page.svelte@4-6:exp=Some(false)",
                "function:load@5-5:exp=Some(true)",
                "file:Page.svelte@7-9:exp=Some(false)",
                "function:local@8-8:exp=Some(false)",
            ]
        );

        let refs: Vec<String> = result
            .unresolved_references
            .iter()
            .map(|r| {
                format!(
                    "{}:{}@{}",
                    r.reference_kind.as_str(),
                    r.reference_name,
                    r.line
                )
            })
            .collect();
        assert_eq!(refs, vec!["calls:fetchData@5"]);
    }
}
