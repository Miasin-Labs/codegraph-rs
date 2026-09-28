//! What a file's syntax says about the names its calls use: its imports
//! (Java `import`, Python `import`/`from … import`, JS `require`/`import`,
//! Rust `use`) and the declared types of its variables (per function) and
//! fields (per class). Read in one iterative walk per file, then looked up
//! per call through the call's enclosing function and class (O(depth)
//! per call, never O(file)).

use std::collections::HashMap;

use tree_sitter::Node;

use crate::types::Language;

/// A file's imports and declarations.
#[derive(Debug, Default)]
pub struct FileFacts {
    /// Java package / nothing elsewhere.
    pub package: Option<String>,
    /// A local name → what it stands for, fully qualified: Java class →
    /// `pkg.Class`, Python/JS binding → module path (`os.path`, `fs`,
    /// `fs.readFile`), Rust `use` leaf → path (`std::process::Command`).
    pub imports: HashMap<String, String>,
    /// Java `import pkg.*;` / Rust `use a::b::*;` prefixes.
    pub wildcards: Vec<String>,
    /// Java `import static pkg.T.m;`: `m` → `pkg.T`.
    pub static_imports: HashMap<String, String>,
    /// Scope node (function, class, or the file root) start byte → name →
    /// declared type as written.
    scopes: HashMap<usize, HashMap<String, String>>,
}

/// Node kinds that open a scope for declarations, per language family.
const SCOPE_KINDS: &[&str] = &[
    // functions
    "method_declaration",
    "constructor_declaration",
    "lambda_expression",
    "function_definition",
    "function_declaration",
    "function_expression",
    "arrow_function",
    "method_definition",
    "function_item",
    "closure_expression",
    // classes (fields)
    "class_body",
    "interface_body",
    "enum_body",
    "record_declaration",
    "field_declaration_list",
];

/// The innermost scope node strictly holding `node` (its own kind
/// counts), else the root.
fn scope_of(node: Node) -> Node {
    let mut current = node;
    loop {
        if SCOPE_KINDS.contains(&current.kind()) {
            return current;
        }
        match current.parent() {
            Some(parent) => current = parent,
            None => return current,
        }
    }
}

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    source.get(node.byte_range()).unwrap_or_default()
}

/// `java.util.List<String>[]` → `java.util.List`; `&mut Vec<u8>` → `Vec`
/// is left to the caller (Rust keeps its path).
pub fn bare_type(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth > 0 => {}
            '[' | ']' => {}
            c if c.is_whitespace() => {}
            c => out.push(c),
        }
    }
    let out = out
        .trim_start_matches('&')
        .trim_start_matches("mut")
        .trim_start_matches("dyn")
        .trim_start_matches("impl");
    out.trim_end_matches("...").to_string()
}

impl FileFacts {
    /// Read the imports and declarations of `root` (a file of `language`).
    pub fn read(language: Language, root: Node, source: &str) -> Self {
        let mut facts = FileFacts::default();
        let mut stack = vec![root];
        while let Some(node) = stack.pop() {
            match language {
                Language::Java => facts.java(node, source),
                Language::Python => facts.python(node, source),
                Language::Javascript | Language::Typescript | Language::Tsx | Language::Jsx => {
                    facts.javascript(node, source)
                }
                Language::Rust => facts.rust(node, source),
                _ => {}
            }
            let mut cursor = node.walk();
            stack.extend(node.named_children(&mut cursor));
        }
        facts
    }

    fn declare(&mut self, at: Node, name: &str, type_text: &str) {
        let name = name.trim();
        let type_text = type_text.trim();
        if name.is_empty() || type_text.is_empty() {
            return;
        }
        let scope = scope_of(at);
        self.scopes
            .entry(scope.start_byte())
            .or_default()
            .entry(name.to_string())
            .or_insert_with(|| type_text.to_string());
    }

    /// The declared type of `name` as seen from `node`: the innermost
    /// scope around it that declares it (function, then class, then file).
    pub fn declared_type(&self, node: Node, name: &str) -> Option<&str> {
        let mut current = Some(node);
        while let Some(n) = current {
            if SCOPE_KINDS.contains(&n.kind()) || n.parent().is_none() {
                if let Some(found) = self
                    .scopes
                    .get(&n.start_byte())
                    .and_then(|vars| vars.get(name))
                {
                    return Some(found);
                }
            }
            current = n.parent();
        }
        None
    }

    fn java(&mut self, node: Node, source: &str) {
        match node.kind() {
            "package_declaration" => {
                let t = text(node, source)
                    .trim_start_matches("package")
                    .trim_end_matches(';')
                    .trim();
                self.package = Some(t.split_whitespace().collect());
            }
            "import_declaration" => {
                let t: String = text(node, source)
                    .trim_start_matches("import")
                    .trim_end_matches(';')
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let (is_static, path) = match t.strip_prefix("static ") {
                    Some(rest) => (true, rest.replace(' ', "")),
                    None => (false, t.replace(' ', "")),
                };
                if let Some(prefix) = path.strip_suffix(".*") {
                    if !is_static {
                        self.wildcards.push(prefix.to_string());
                    }
                } else if let Some((owner, leaf)) = path.rsplit_once('.') {
                    if is_static {
                        self.static_imports
                            .insert(leaf.to_string(), owner.to_string());
                    } else {
                        self.imports.insert(leaf.to_string(), path.clone());
                    }
                }
            }
            "local_variable_declaration" | "field_declaration" => {
                let Some(ty) = node.child_by_field_name("type") else {
                    return;
                };
                let ty = text(ty, source).to_string();
                let mut cursor = node.walk();
                for declarator in node.children_by_field_name("declarator", &mut cursor) {
                    if let Some(name) = declarator.child_by_field_name("name") {
                        self.declare(node, text(name, source), &ty);
                    }
                }
            }
            "formal_parameter" | "enhanced_for_statement" | "resource" => {
                if let (Some(ty), Some(name)) = (
                    node.child_by_field_name("type"),
                    node.child_by_field_name("name"),
                ) {
                    let scope_node = if node.kind() == "formal_parameter" {
                        // A parameter belongs to its method's body scope:
                        // the method node itself.
                        node.parent().and_then(|p| p.parent()).unwrap_or(node)
                    } else {
                        node
                    };
                    self.declare(scope_node, text(name, source), text(ty, source));
                }
            }
            "catch_formal_parameter" => {
                let ty = (0..node.named_child_count() as u32)
                    .filter_map(|i| node.named_child(i))
                    .find(|c| c.kind() == "catch_type");
                if let (Some(ty), Some(name)) = (ty, node.child_by_field_name("name")) {
                    // `catch (IOException | X e)`: the first alternative.
                    let first = text(ty, source).split('|').next().unwrap_or_default();
                    self.declare(node, text(name, source), first);
                }
            }
            _ => {}
        }
    }

    fn python(&mut self, node: Node, source: &str) {
        match node.kind() {
            "import_statement" => {
                let mut cursor = node.walk();
                for name in node.children_by_field_name("name", &mut cursor) {
                    match name.kind() {
                        "aliased_import" => {
                            if let (Some(path), Some(alias)) = (
                                name.child_by_field_name("name"),
                                name.child_by_field_name("alias"),
                            ) {
                                self.imports.insert(
                                    text(alias, source).to_string(),
                                    text(path, source).to_string(),
                                );
                            }
                        }
                        _ => {
                            // `import a.b` binds `a`.
                            let path = text(name, source);
                            let head = path.split('.').next().unwrap_or(path);
                            self.imports.insert(head.to_string(), head.to_string());
                        }
                    }
                }
            }
            "import_from_statement" => {
                let Some(module) = node.child_by_field_name("module_name") else {
                    return;
                };
                let module = text(module, source).trim_start_matches('.').to_string();
                let mut cursor = node.walk();
                for name in node.children_by_field_name("name", &mut cursor) {
                    let (path, alias) = match name.kind() {
                        "aliased_import" => match (
                            name.child_by_field_name("name"),
                            name.child_by_field_name("alias"),
                        ) {
                            (Some(p), Some(a)) => (text(p, source), text(a, source)),
                            _ => continue,
                        },
                        _ => (text(name, source), text(name, source)),
                    };
                    self.imports
                        .insert(alias.to_string(), format!("{module}.{path}"));
                }
            }
            "assignment" => {
                // `x = mod.Class(…)`: `x` holds a `mod.Class` (resolved by
                // the matcher through the imports).
                if let (Some(left), Some(right)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                ) {
                    if left.kind() == "identifier" && right.kind() == "call" {
                        if let Some(function) = right.child_by_field_name("function") {
                            let callee = text(function, source);
                            let last = callee.rsplit('.').next().unwrap_or(callee);
                            if last.chars().next().is_some_and(char::is_uppercase) {
                                self.declare(node, text(left, source), callee);
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn javascript(&mut self, node: Node, source: &str) {
        let module_of = |string: Node| -> String {
            text(string, source)
                .trim_matches(['"', '\'', '`'])
                .to_string()
        };
        match node.kind() {
            "import_statement" => {
                let Some(from) = node.child_by_field_name("source") else {
                    return;
                };
                let module = module_of(from);
                let mut stack: Vec<Node> = (0..node.named_child_count() as u32)
                    .filter_map(|i| node.named_child(i))
                    .collect();
                while let Some(child) = stack.pop() {
                    match child.kind() {
                        "import_clause" | "named_imports" => {
                            let mut cursor = child.walk();
                            stack.extend(child.named_children(&mut cursor));
                        }
                        "identifier" => {
                            // `import x from 'm'`: the default export.
                            self.imports
                                .insert(text(child, source).to_string(), module.clone());
                        }
                        "namespace_import" => {
                            if let Some(id) = child.named_child(0) {
                                self.imports
                                    .insert(text(id, source).to_string(), module.clone());
                            }
                        }
                        "import_specifier" => {
                            let Some(name) = child.child_by_field_name("name") else {
                                continue;
                            };
                            let local = child.child_by_field_name("alias").unwrap_or(name);
                            self.imports.insert(
                                text(local, source).to_string(),
                                format!("{module}.{}", text(name, source)),
                            );
                        }
                        _ => {}
                    }
                }
            }
            "variable_declarator" => {
                let (Some(name), Some(value)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("value"),
                ) else {
                    return;
                };
                // `require('m')`, `require('m').x`, `new C(…)`.
                let (required, member) = match value.kind() {
                    "call_expression" => (value, None),
                    "member_expression" => match (
                        value.child_by_field_name("object"),
                        value.child_by_field_name("property"),
                    ) {
                        (Some(object), Some(property)) if object.kind() == "call_expression" => {
                            (object, Some(text(property, source)))
                        }
                        _ => return,
                    },
                    "new_expression" => {
                        if let (Some(constructor), "identifier") =
                            (value.child_by_field_name("constructor"), name.kind())
                        {
                            self.declare(node, text(name, source), text(constructor, source));
                        }
                        return;
                    }
                    _ => return,
                };
                let is_require = required
                    .child_by_field_name("function")
                    .is_some_and(|f| text(f, source) == "require");
                let Some(module) = required
                    .child_by_field_name("arguments")
                    .and_then(|args| args.named_child(0))
                    .filter(|arg| arg.kind() == "string")
                    .map(module_of)
                    .filter(|_| is_require)
                else {
                    return;
                };
                let base = match member {
                    Some(member) => format!("{module}.{member}"),
                    None => module,
                };
                match name.kind() {
                    "identifier" => {
                        self.imports.insert(text(name, source).to_string(), base);
                    }
                    "object_pattern" => {
                        let mut cursor = name.walk();
                        for property in name.named_children(&mut cursor) {
                            match property.kind() {
                                "shorthand_property_identifier_pattern" => {
                                    let local = text(property, source);
                                    self.imports
                                        .insert(local.to_string(), format!("{base}.{local}"));
                                }
                                "pair_pattern" => {
                                    if let (Some(key), Some(value)) = (
                                        property.child_by_field_name("key"),
                                        property.child_by_field_name("value"),
                                    ) {
                                        self.imports.insert(
                                            text(value, source).to_string(),
                                            format!("{base}.{}", text(key, source)),
                                        );
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn rust(&mut self, node: Node, source: &str) {
        match node.kind() {
            "use_declaration" => {
                let Some(argument) = node.child_by_field_name("argument") else {
                    return;
                };
                let mut leaves = Vec::new();
                expand_use("", text(argument, source), &mut leaves);
                for (leaf, path) in leaves {
                    if leaf == "*" {
                        self.wildcards.push(path);
                    } else {
                        self.imports.insert(leaf, path);
                    }
                }
            }
            "let_declaration" => {
                let Some(pattern) = node.child_by_field_name("pattern") else {
                    return;
                };
                let name = text(pattern, source).trim_start_matches("mut ").trim();
                if !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                    return;
                }
                if let Some(ty) = node.child_by_field_name("type") {
                    self.declare(node, name, text(ty, source));
                } else if let Some(value) = node.child_by_field_name("value") {
                    if let Some(ty) = rust_constructed_type(value, source) {
                        self.declare(node, name, &ty);
                    }
                }
            }
            "parameter" => {
                if let (Some(pattern), Some(ty)) = (
                    node.child_by_field_name("pattern"),
                    node.child_by_field_name("type"),
                ) {
                    let name = text(pattern, source).trim_start_matches("mut ").trim();
                    // A parameter's scope is its function.
                    let function = node.parent().and_then(|p| p.parent()).unwrap_or(node);
                    self.declare(function, name, text(ty, source));
                }
            }
            _ => {}
        }
    }
}

/// `Type::new(…)`, `Type::from(…)`, `Type { … }`, `Type::builder()…`
/// (the chain's head): the type a `let` without annotation holds.
fn rust_constructed_type(value: Node, source: &str) -> Option<String> {
    let mut value = value;
    // `Type::new(…)?` / `.unwrap()`: look through `?` only.
    while value.kind() == "try_expression" {
        value = value.named_child(0)?;
    }
    match value.kind() {
        "struct_expression" => value
            .child_by_field_name("name")
            .map(|n| text(n, source).to_string()),
        "call_expression" => {
            let function = value.child_by_field_name("function")?;
            if function.kind() != "scoped_identifier" {
                return None;
            }
            let path = function.child_by_field_name("path")?;
            let name = function.child_by_field_name("name")?;
            let name = text(name, source);
            let owner = text(path, source);
            let last = owner.rsplit("::").next().unwrap_or(owner);
            (last.chars().next().is_some_and(char::is_uppercase)
                && matches!(
                    name,
                    "new" | "from" | "default" | "with_capacity" | "open" | "create" | "builder"
                ))
            .then(|| owner.to_string())
        }
        _ => None,
    }
}

/// Expand a `use` tree (`a::b::{C, D as E, f::{g, self}}`, `x::*`) into
/// (local name, path) leaves; `*` leaves name a glob's prefix.
fn expand_use(prefix: &str, tree: &str, out: &mut Vec<(String, String)>) {
    // Recursion depth is the brace nesting of one `use`: bounded by input.
    codegraph_analysis::ensure_sufficient_stack(|| expand_use_inner(prefix, tree, out))
}

fn expand_use_inner(prefix: &str, tree: &str, out: &mut Vec<(String, String)>) {
    let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let join = |a: &str, b: &str| match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a}::{b}"),
    };
    let tree = tree.trim();
    if let Some(open) = tree.find('{') {
        let head = squash(&tree[..open]);
        let base = join(prefix, head.trim_end_matches("::"));
        let inner = tree[open + 1..].trim_end();
        let inner = inner.strip_suffix('}').unwrap_or(inner);
        let mut depth = 0usize;
        let mut start = 0usize;
        for (i, ch) in inner.char_indices() {
            match ch {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                ',' if depth == 0 => {
                    expand_use(&base, &inner[start..i], out);
                    start = i + 1;
                }
                _ => {}
            }
        }
        if !inner[start..].trim().is_empty() {
            expand_use(&base, &inner[start..], out);
        }
        return;
    }
    if tree.is_empty() {
        return;
    }
    let (path, alias) = match tree.split_once(" as ") {
        Some((path, alias)) => (squash(path), Some(squash(alias))),
        None => (squash(tree), None),
    };
    if path == "*" || path.ends_with("::*") {
        let glob = path.trim_end_matches('*').trim_end_matches("::");
        out.push(("*".into(), join(prefix, glob)));
        return;
    }
    if path == "self" {
        let leaf = prefix.rsplit("::").next().unwrap_or(prefix).to_string();
        out.push((alias.unwrap_or(leaf), prefix.to_string()));
        return;
    }
    let full = join(prefix, &path);
    let leaf = full.rsplit("::").next().unwrap_or(&full).to_string();
    if alias.as_deref() != Some("_") {
        out.push((alias.unwrap_or(leaf), full));
    }
}

#[cfg(test)]
mod tests {
    use super::expand_use;

    #[test]
    fn use_trees_expand_to_leaves() {
        let mut out = Vec::new();
        expand_use(
            "",
            "std::{io::{self, Read}, process::Command as Cmd, collections::*, hash::Hash}",
            &mut out,
        );
        assert!(out.contains(&("io".into(), "std::io".into())));
        assert!(out.contains(&("Read".into(), "std::io::Read".into())));
        assert!(out.contains(&("Cmd".into(), "std::process::Command".into())));
        assert!(out.contains(&("*".into(), "std::collections".into())));
        assert!(out.contains(&("Hash".into(), "std::hash::Hash".into())));
    }
}
