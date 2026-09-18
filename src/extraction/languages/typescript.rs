//! TypeScript language extraction config.
//!
//! Ported from `src/extraction/languages/typescript.ts`.

use crate::extraction::tree_sitter_helpers::{
    get_child_by_field,
    get_node_text,
    get_preceding_docstring,
};
use crate::extraction::tree_sitter_types::{
    ClassMemberKind,
    ExtractorContext,
    ImportInfo,
    ImportOutcome,
    LanguageExtractor,
    NodeExtra,
    SyntaxNode,
};
use crate::types::{NodeKind, Visibility};

pub struct TypescriptExtractor;

/// A TypeScript declaration file (`.d.ts`, `.d.mts`, `.d.cts`).
fn is_declaration_file(path: &str) -> bool {
    [".d.ts", ".d.mts", ".d.cts"]
        .iter()
        .any(|ext| path.ends_with(ext))
}

/// TypeScript/ArkTS class fields are callable methods only when their value is
/// an arrow/function expression (or a HOF wrapping one). Plain fields are
/// properties, even though the grammar uses the same node kind for both.
pub(crate) fn classify_ts_class_member(node: SyntaxNode<'_>) -> ClassMemberKind {
    if !matches!(node.kind(), "public_field_definition" | "field_definition") {
        return ClassMemberKind::Method;
    }
    for i in 0..node.named_child_count() as u32 {
        let Some(child) = node.named_child(i) else {
            continue;
        };
        if matches!(child.kind(), "arrow_function" | "function_expression") {
            return ClassMemberKind::Method;
        }
        if child.kind() == "call_expression" {
            let Some(args) = get_child_by_field(child, "arguments") else {
                continue;
            };
            for j in 0..args.named_child_count() as u32 {
                if args.named_child(j).is_some_and(|arg| {
                    matches!(arg.kind(), "arrow_function" | "function_expression")
                }) {
                    return ClassMemberKind::Method;
                }
            }
        }
    }
    ClassMemberKind::Property
}

impl LanguageExtractor for TypescriptExtractor {
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
        &[
            "method_definition",
            "public_field_definition",
            "method_signature",
        ]
    }
    fn property_types(&self) -> &[&str] {
        &["property_signature"]
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

    fn classify_method_node(&self, node: SyntaxNode<'_>, _source: &str) -> ClassMemberKind {
        classify_ts_class_member(node)
    }

    /// Ambient function declarations (`export declare function f(): T;`,
    /// functions inside `declare module`/`namespace` blocks) are all a
    /// declaration file says about a function — its API. In a `.ts` source
    /// the same `function_signature` node is an overload whose
    /// implementation follows, so it becomes a node only in `.d.ts` files.
    fn visit_node(&self, node: SyntaxNode<'_>, ctx: &mut dyn ExtractorContext) -> bool {
        if node.kind() != "function_signature" || !is_declaration_file(ctx.file_path()) {
            return false;
        }
        let Some(name_node) = get_child_by_field(node, "name") else {
            return false;
        };
        let name = get_node_text(name_node, ctx.source()).to_string();
        let extra = NodeExtra {
            docstring: get_preceding_docstring(node, ctx.source()),
            signature: self.get_signature(node, ctx.source()),
            return_type: self.get_return_type(node, ctx.source()),
            is_exported: self.is_exported(node, ctx.source()),
            ..NodeExtra::default()
        };
        ctx.create_node(NodeKind::Function, &name, node, extra);
        true
    }

    fn resolve_body<'t>(&self, node: SyntaxNode<'t>, body_field: &str) -> Option<SyntaxNode<'t>> {
        // public_field_definition (arrow function class fields) nest the body inside
        // an arrow_function or function_expression child:
        //   public_field_definition → arrow_function → body (statement_block)
        // Also handles wrapper patterns like: field = withBatchedUpdates((e) => { ... })
        //   public_field_definition → call_expression → arguments → arrow_function → body
        if node.kind() == "public_field_definition" {
            for i in 0..node.named_child_count() as u32 {
                let Some(child) = node.named_child(i) else {
                    continue;
                };
                if child.kind() == "arrow_function" || child.kind() == "function_expression" {
                    return get_child_by_field(child, body_field);
                }
                // Check inside call_expression arguments (HOF wrappers like throttle, debounce)
                if child.kind() == "call_expression" {
                    if let Some(args) = get_child_by_field(child, "arguments") {
                        let mut cursor = args.walk();
                        for arg in args.named_children(&mut cursor) {
                            if arg.kind() == "arrow_function" || arg.kind() == "function_expression"
                            {
                                return get_child_by_field(arg, body_field);
                            }
                        }
                    }
                }
            }
        }
        None
    }

    fn get_signature(&self, node: SyntaxNode<'_>, source: &str) -> Option<String> {
        let params = get_child_by_field(node, "parameters")?;
        let return_type = get_child_by_field(node, "return_type");
        let mut sig = get_node_text(params, source).to_string();
        if let Some(rt) = return_type {
            // TS: getNodeText(returnType, source).replace(/^:\s*/, '')
            let rt_text = get_node_text(rt, source);
            let rt_text = rt_text
                .strip_prefix(':')
                .map(str::trim_start)
                .unwrap_or(rt_text);
            sig.push_str(": ");
            sig.push_str(rt_text);
        }
        Some(sig)
    }

    fn get_visibility(&self, node: SyntaxNode<'_>, source: &str) -> Option<Visibility> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "accessibility_modifier" {
                match get_node_text(child, source) {
                    "public" => return Some(Visibility::Public),
                    "private" => return Some(Visibility::Private),
                    "protected" => return Some(Visibility::Protected),
                    _ => {}
                }
            }
        }
        None
    }

    fn is_exported(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        // Walk the parent chain to find an export_statement ancestor.
        // This correctly handles deeply nested nodes like arrow functions
        // inside variable declarations: `export const X = () => { ... }`
        // where the arrow_function is 3 levels deep under export_statement.
        let mut current = node.parent();
        while let Some(p) = current {
            if p.kind() == "export_statement" {
                return Some(true);
            }
            current = p.parent();
        }
        Some(false)
    }

    fn is_async(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "async" {
                return Some(true);
            }
        }
        Some(false)
    }

    fn is_static(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.kind() == "static" {
                return Some(true);
            }
        }
        Some(false)
    }

    fn is_const(&self, node: SyntaxNode<'_>, _source: &str) -> Option<bool> {
        // For lexical_declaration, check if it's 'const' or 'let'
        // For variable_declaration, it's always 'var'
        if node.kind() == "lexical_declaration" {
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if child.kind() == "const" {
                    return Some(true);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{Language, NodeKind};

    #[test]
    fn typescript_smoke_extraction() {
        let source = "import { x } from './mod';\n\nexport async function fetchData(url: string): Promise<string> {\n  return x(url);\n}\n\nexport class Service {\n  private run(task: string): void {\n    fetchData(task);\n  }\n}\n\nexport interface Shape { area(): number; }\nexport type Alias = string;\nexport enum Color { Red, Green }\nconst LIMIT = 10;\n";
        let result = TreeSitterExtractor::new(
            "src/app.ts",
            source,
            Some(Language::Typescript),
            Some(&TypescriptExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let func = result
            .nodes
            .iter()
            .find(|n| n.name == "fetchData")
            .expect("function node");
        assert_eq!(func.kind, NodeKind::Function);
        assert_eq!(func.is_exported, Some(true));
        assert_eq!(func.is_async, Some(true));
        assert_eq!(
            func.signature.as_deref(),
            Some("(url: string): Promise<string>")
        );

        let class = result.nodes.iter().find(|n| n.name == "Service").unwrap();
        assert_eq!(class.kind, NodeKind::Class);

        let method = result.nodes.iter().find(|n| n.name == "run").unwrap();
        assert_eq!(method.kind, NodeKind::Method);
        assert_eq!(method.visibility, Some(Visibility::Private));
        assert_eq!(method.qualified_name, "Service::run");

        let iface = result.nodes.iter().find(|n| n.name == "Shape").unwrap();
        assert_eq!(iface.kind, NodeKind::Interface);
        let alias = result.nodes.iter().find(|n| n.name == "Alias").unwrap();
        assert_eq!(alias.kind, NodeKind::TypeAlias);
        let enum_node = result.nodes.iter().find(|n| n.name == "Color").unwrap();
        assert_eq!(enum_node.kind, NodeKind::Enum);
        let constant = result.nodes.iter().find(|n| n.name == "LIMIT").unwrap();
        assert_eq!(constant.kind, NodeKind::Constant);

        let import = result
            .nodes
            .iter()
            .find(|n| n.kind == NodeKind::Import)
            .expect("import node");
        assert_eq!(import.name, "./mod");
    }

    /// Regression for upstream #1638: interface member `method_signature` and
    /// `property_signature` nodes must be indexed. Before the fix an interface
    /// declared its methods/properties but they never became graph nodes, so
    /// every call site through a `.d.ts` platform API was invisible.
    #[test]
    fn typescript_interface_members_are_indexed() {
        let source = "export interface PlatformContext {
  deleteQueueCustomAck(queueName: string, thingId: number): void
  createQueueCustomAckKey(queueName: string, thingId: number): void
  readonly tenantId: string
}
";
        let result = TreeSitterExtractor::new(
            "src/api.d.ts",
            source,
            Some(Language::Typescript),
            Some(&TypescriptExtractor),
        )
        .extract();
        assert!(result.errors.is_empty(), "errors: {:?}", result.errors);

        let iface = result
            .nodes
            .iter()
            .find(|n| n.name == "PlatformContext")
            .expect("interface node");
        assert_eq!(iface.kind, NodeKind::Interface);

        let del = result
            .nodes
            .iter()
            .find(|n| n.name == "deleteQueueCustomAck")
            .expect("method_signature node");
        assert_eq!(del.kind, NodeKind::Method);
        assert_eq!(del.qualified_name, "PlatformContext::deleteQueueCustomAck");

        let create = result
            .nodes
            .iter()
            .find(|n| n.name == "createQueueCustomAckKey")
            .expect("second method_signature node");
        assert_eq!(create.kind, NodeKind::Method);

        let prop = result
            .nodes
            .iter()
            .find(|n| n.name == "tenantId")
            .expect("property_signature node");
        assert_eq!(prop.kind, NodeKind::Property);
        assert_eq!(prop.qualified_name, "PlatformContext::tenantId");

        let method_count = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Method)
            .count();
        assert_eq!(method_count, 2, "expected 2 interface methods");
        let property_count = result
            .nodes
            .iter()
            .filter(|n| n.kind == NodeKind::Property)
            .count();
        assert_eq!(property_count, 1, "expected 1 interface property");
    }

    #[test]
    fn declaration_files_index_ambient_function_signatures() {
        let source = "export declare function connect(url: string): Client;\n\
                      declare function globalHelper(): void;\n\
                      declare module \"plugin\" {\n  export function register(name: string): boolean;\n}\n";
        let functions = |path: &str| {
            TreeSitterExtractor::new(
                path,
                source,
                Some(Language::Typescript),
                Some(&TypescriptExtractor),
            )
            .extract()
            .nodes
            .into_iter()
            .filter(|n| n.kind == NodeKind::Function)
            .collect::<Vec<_>>()
        };
        let declared = functions("types/index.d.ts");
        let names: Vec<&str> = declared.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["connect", "globalHelper", "register"]);
        let connect = &declared[0];
        assert_eq!(connect.is_exported, Some(true));
        assert_eq!(connect.return_type.as_deref(), Some("Client"));
        assert_eq!(connect.signature.as_deref(), Some("(url: string): Client"));
        assert_eq!(declared[1].is_exported, Some(false));

        // In a source file the same syntax is an overload signature: the
        // implementation is the function.
        let overloads =
            "export function f(a: string): string;\nexport function f(a: any) { return a; }\n";
        let nodes = TreeSitterExtractor::new(
            "src/f.ts",
            overloads,
            Some(Language::Typescript),
            Some(&TypescriptExtractor),
        )
        .extract()
        .nodes;
        assert_eq!(
            nodes
                .iter()
                .filter(|n| n.kind == NodeKind::Function)
                .count(),
            1
        );
        assert!(functions("src/decl.ts").is_empty());
    }
}
