use super::context::find_named_child;
use super::extractor::TreeSitterExtractor;
use crate::extraction::tree_sitter_helpers::{get_child_by_field, get_node_text};
use crate::extraction::tree_sitter_types::SyntaxNode;
use crate::types::{EdgeKind, Language, Metadata, UnresolvedReference, dropped_receiver_metadata};

/// Tree-sitter node kinds that represent constructor invocations
/// (`new Foo()` and friends). Used by extract_instantiation to emit
/// an `instantiates` reference targeting the class name.
pub(super) const INSTANTIATION_KINDS: &[&str] = &[
    "new_expression",               // typescript / javascript / tsx / jsx
    "object_creation_expression",   // java / c#
    "instance_creation_expression", // some grammars
];

impl<'a> TreeSitterExtractor<'a> {
    fn push_call_reference(&mut self, caller_id: &str, name: String, node: SyntaxNode<'_>) {
        self.push_call_reference_with(caller_id, name, node, None);
    }

    fn push_call_reference_with(
        &mut self,
        caller_id: &str,
        name: String,
        node: SyntaxNode<'_>,
        metadata: Option<Metadata>,
    ) {
        if name.is_empty() {
            return;
        }
        self.unresolved_references.push(UnresolvedReference {
            from_node_id: caller_id.to_string(),
            reference_name: name,
            reference_kind: EdgeKind::Calls,
            line: node.start_position().row as u32 + 1,
            column: node.start_position().column as u32,
            file_path: None,
            language: None,
            candidates: None,
            metadata,
        });
    }

    /// Rust: a method call on anything but a plain identifier or `self`
    /// (`v.iter().next()`, `self.map.get(k)`, `x[0].len()`) is named by its
    /// bare method, and resolution must know the receiver was dropped (and
    /// gets it back as compact text, to follow the chain's types).
    fn drops_rust_receiver(&self, receiver: Option<SyntaxNode<'_>>) -> bool {
        self.language == Language::Rust
            && receiver.is_some_and(|receiver| receiver.kind() != "self")
    }

    /// C++ operator overloads invoked via infix (`a + b`) or subscript (`a[i]`)
    /// syntax get a `calls` edge into the operator method (#1258). tree-sitter
    /// parses these as `binary_expression` / `subscript_expression`, neither of
    /// which is a `call_expression`, so they never reached call extraction and
    /// no edge was recorded.
    ///
    /// Emit a `receiver.operator<sym>` reference — the same shape the explicit
    /// `a.operator+(b)` form resolves through — so the resolver's C++ receiver
    /// type inference finds the receiver's class and matches the operator method
    /// on it. Only a bare identifier receiver is emitted: a literal or nested
    /// expression can't aid type inference, and the `#1566` project-type guard
    /// plus exact operator-name matching keep a mismatched receiver from
    /// inventing an edge.
    pub(super) fn extract_cpp_operator_call(&mut self, node: SyntaxNode<'_>) {
        if self.language != Language::Cpp {
            return;
        }
        let Some(caller_id) = self.node_stack.last().cloned() else {
            return;
        };
        let (receiver_node, operator_symbol) = match node.kind() {
            "binary_expression" => {
                let Some(op) = get_child_by_field(node, "operator") else {
                    return;
                };
                (
                    get_child_by_field(node, "left"),
                    get_node_text(op, self.source).to_string(),
                )
            }
            "subscript_expression" => (get_child_by_field(node, "argument"), "[]".to_string()),
            _ => return,
        };
        let Some(receiver_node) = receiver_node else {
            return;
        };
        if receiver_node.kind() != "identifier" {
            return;
        }
        let receiver = get_node_text(receiver_node, self.source);
        // `self`/`this` receivers don't aid class inference here.
        if matches!(receiver, "this" | "self") {
            return;
        }
        let callee_name = format!("{receiver}.operator{operator_symbol}");
        self.push_call_reference(&caller_id, callee_name, node);
    }

    fn emit_arkui_attribute(&mut self, caller_id: &str, name_node: SyntaxNode<'_>) {
        let name = get_node_text(name_node, self.source).to_string();
        if !name.is_empty() {
            self.push_call_reference(caller_id, format!(".{name}"), name_node);
        }
    }

    fn emit_arkui_this_handlers(&mut self, caller_id: &str, args: Option<SyntaxNode<'_>>) {
        let Some(args) = args else { return };
        for i in 0..args.named_child_count() as u32 {
            let Some(arg) = args.named_child(i) else {
                continue;
            };
            if arg.kind() != "member_expression" {
                continue;
            }
            let Some(object) = get_child_by_field(arg, "object") else {
                continue;
            };
            let Some(property) = get_child_by_field(arg, "property") else {
                continue;
            };
            if object.kind() == "this" {
                let name = get_node_text(property, self.source).to_string();
                self.push_call_reference(caller_id, name, arg);
            }
        }
    }

    fn is_arkui_event_attribute(&self, node: SyntaxNode<'_>) -> bool {
        let name = get_node_text(node, self.source);
        name.strip_prefix("on")
            .and_then(|tail| tail.chars().next())
            .is_some_and(char::is_uppercase)
    }

    /// VB.NET permits the same parenthesized syntax for calls and index reads.
    /// Support both the upstream vendored grammar and the native crate's AST;
    /// false-positive index reads simply remain unresolved.
    fn extract_vbnet_call(&mut self, node: SyntaxNode<'_>, caller_id: &str) -> bool {
        if self.language != Language::Vbnet
            || !matches!(
                node.kind(),
                "array_access_expression"
                    | "invocation_expression"
                    | "generic_invocation_expression"
                    | "invocation"
            )
        {
            return false;
        }

        let Some(function) = get_child_by_field(node, "target")
            .or_else(|| get_child_by_field(node, "function"))
            .or_else(|| get_child_by_field(node, "array"))
        else {
            return true;
        };

        let callee_name = if matches!(
            function.kind(),
            "member_access" | "member_access_expression"
        ) {
            let member = get_child_by_field(function, "member")
                .or_else(|| get_child_by_field(function, "name"));
            let Some(member) = member else { return true };
            let method_name = get_node_text(member, self.source);
            let receiver = get_child_by_field(function, "object");
            match receiver {
                Some(receiver)
                    if matches!(receiver.kind(), "identifier" | "simple_name")
                        && !matches!(
                            get_node_text(receiver, self.source)
                                .to_ascii_lowercase()
                                .as_str(),
                            "me" | "mybase" | "myclass"
                        ) =>
                {
                    format!("{}.{}", get_node_text(receiver, self.source), method_name)
                }
                _ => method_name.to_string(),
            }
        } else if matches!(function.kind(), "identifier" | "simple_name") {
            get_node_text(function, self.source).to_string()
        } else {
            return true;
        };

        self.push_call_reference(caller_id, callee_name, node);
        true
    }

    /// Extract ArkUI declarative component/attribute chains. Attribute names
    /// carry a leading dot so resolution can restrict them to decorated
    /// `@Extend`/`@Styles`/`@Builder` helpers instead of matching ubiquitous
    /// framework attributes to unrelated symbols.
    fn extract_arkui_call(&mut self, node: SyntaxNode<'_>, caller_id: &str) -> bool {
        if self.language != crate::types::Language::Arkts {
            return false;
        }

        if node.kind() == "arkui_component_expression" {
            if let Some(component) = get_child_by_field(node, "function") {
                if component.kind() == "identifier" {
                    let name = get_node_text(component, self.source).to_string();
                    self.push_call_reference(caller_id, name, node);
                }
            }

            for i in 0..node.child_count() as u32 {
                let Some(child) = node.child(i) else {
                    continue;
                };
                if child.kind() != "property_identifier" {
                    continue;
                }
                self.emit_arkui_attribute(caller_id, child);
                if !self.is_arkui_event_attribute(child) {
                    continue;
                }
                let mut args = None;
                for j in i + 1..node.child_count() as u32 {
                    let Some(next) = node.child(j) else {
                        continue;
                    };
                    if next.kind() == "property_identifier" {
                        break;
                    }
                    if next.kind() == "arguments" {
                        args = Some(next);
                        break;
                    }
                }
                self.emit_arkui_this_handlers(caller_id, args);
            }
            return true;
        }

        if node.kind() == "call_expression" {
            let function = get_child_by_field(node, "function");
            if let Some(function) = function {
                if matches!(
                    function.kind(),
                    "member_expression" | "arkui_dsl_decorator_member_expression"
                ) {
                    let object = get_child_by_field(function, "object");
                    let property = get_child_by_field(function, "property");
                    if let Some(property) = property {
                        let is_attribute = function.kind()
                            == "arkui_dsl_decorator_member_expression"
                            || object.is_some_and(|object| {
                                matches!(
                                    object.kind(),
                                    "call_expression" | "arkui_component_expression"
                                )
                            });
                        if is_attribute {
                            self.emit_arkui_attribute(caller_id, property);
                            if self.is_arkui_event_attribute(property) {
                                self.emit_arkui_this_handlers(
                                    caller_id,
                                    get_child_by_field(node, "arguments"),
                                );
                            }
                            return true;
                        }
                    }
                }

                if function.kind() == "identifier" {
                    let mut parent = node.parent();
                    while parent.is_some_and(|p| {
                        matches!(p.kind(), "member_expression" | "call_expression")
                    }) {
                        parent = parent.and_then(|p| p.parent());
                    }
                    if parent.is_some_and(|p| p.kind() == "leading_dot_expression") {
                        self.emit_arkui_attribute(caller_id, function);
                        if self.is_arkui_event_attribute(function) {
                            self.emit_arkui_this_handlers(
                                caller_id,
                                get_child_by_field(node, "arguments"),
                            );
                        }
                        return true;
                    }
                }
            }
        }

        if node.kind() == "leading_dot_expression" {
            if node.named_child_count() == 1 {
                if let Some(name) = node.named_child(0).filter(|n| n.kind() == "identifier") {
                    self.emit_arkui_attribute(caller_id, name);
                    if self.is_arkui_event_attribute(name) {
                        let paren = node
                            .parent()
                            .and_then(|statement| statement.next_named_sibling())
                            .and_then(|statement| statement.named_child(0))
                            .filter(|child| child.kind() == "parenthesized_expression");
                        self.emit_arkui_this_handlers(caller_id, paren);
                    }
                }
            }
            return true;
        }

        false
    }

    /// Go: re-encode a factory-chain call receiver `<inner>().<method>` when the
    /// inner callee is a bare package-level factory (`New()`) or a
    /// package-qualified factory whose operand names an imported package
    /// (`service.Order()`). An instance chain (`obj.Method()`) keeps the bare
    /// method name — the resolver can't recover a variable's type, so
    /// re-encoding would only drop the edge (#1640).
    fn go_reencode_call_receiver(&mut self, receiver: SyntaxNode<'_>, method_name: &str) -> String {
        let inner_fn = get_child_by_field(receiver, "function");
        let reencode = match inner_fn {
            // Bare package-level factory (`New().Method()`).
            Some(inner_fn) if inner_fn.kind() == "identifier" => true,
            // Package-qualified factory (`service.Order().Method()`): the inner
            // callee is a `selector_expression`, the SAME node type as an
            // instance chain, so re-encode only when its operand names a package
            // this file imports (alias included).
            Some(inner_fn) if inner_fn.kind() == "selector_expression" => {
                get_child_by_field(inner_fn, "operand")
                    .filter(|operand| operand.kind() == "identifier")
                    .map(|operand| get_node_text(operand, self.source).to_string())
                    .is_some_and(|pkg| self.go_imported_packages(inner_fn).contains(&pkg))
            }
            _ => false,
        };
        if !reencode {
            return method_name.to_string();
        }
        let inner_callee = inner_fn
            .map(|f| {
                get_node_text(f, self.source)
                    .replace("->", ".")
                    .split_whitespace()
                    .collect::<String>()
            })
            .unwrap_or_default();
        if inner_callee.is_empty() {
            method_name.to_string()
        } else {
            format!("{}().{}", inner_callee, method_name)
        }
    }

    /// Go: the package identifiers this file imports — the alias when one is
    /// given, otherwise the import path's last segment. Memoized; the extractor
    /// instance is per-file. Used to tell a package-qualified factory chain from
    /// an instance chain, which share the `selector_expression` inner-callee
    /// shape. `from` is any node in the file's tree — the walk climbs to the
    /// root to find the import declarations.
    fn go_imported_packages(&mut self, from: SyntaxNode<'_>) -> &std::collections::HashSet<String> {
        if self.go_imported_pkgs.is_none() {
            let mut root = from;
            while let Some(parent) = root.parent() {
                root = parent;
            }
            let mut pkgs = std::collections::HashSet::new();
            collect_go_imported_packages(root, self.source, &mut pkgs);
            self.go_imported_pkgs = Some(pkgs);
        }
        self.go_imported_pkgs
            .as_ref()
            .expect("go_imported_pkgs just set")
    }

    /// Extract a function call
    pub(super) fn extract_call(&mut self, node: SyntaxNode<'_>) {
        let Some(caller_id) = self.node_stack.last().cloned() else {
            return;
        };

        if self.extract_arkui_call(node, &caller_id) {
            return;
        }
        if self.extract_vbnet_call(node, &caller_id) {
            return;
        }

        // Get the function/method being called
        let mut callee_name = String::new();
        let mut metadata = None;

        // Java/Kotlin method_invocation has 'object' + 'name' fields instead of 'function'
        // PHP member_call_expression has 'object' + 'name', scoped_call_expression has 'scope' + 'name'
        let name_field = get_child_by_field(node, "name");
        let object_field =
            get_child_by_field(node, "object").or_else(|| get_child_by_field(node, "scope"));
        let node_type = node.kind();

        let is_receiver_call = name_field.is_some()
            && object_field.is_some()
            && matches!(
                node_type,
                "method_invocation" | "member_call_expression" | "scoped_call_expression"
            );

        if is_receiver_call {
            let name_field = name_field.unwrap();
            let object_field = object_field.unwrap();
            // Method call with explicit receiver: receiver.method() / $receiver->method() / ClassName::method()
            let method_name = get_node_text(name_field, self.source).to_string();
            // Java `this.userbo.toLogin2()` parses as method_invocation(object=field_access(this, userbo)).
            // Without unwrapping, receiver_name is `this.userbo` and the name-matcher's
            // single-dot receiver regex fails. Pull out the immediate field after `this.`
            // so the receiver is the field name (`userbo`), which the resolver can then
            // look up in the enclosing class's field declarations.
            let mut receiver_name: String = if object_field.kind() == "field_access" {
                let inner = get_child_by_field(object_field, "object");
                let fld = get_child_by_field(object_field, "field");
                match (inner, fld) {
                    (Some(inner), Some(fld))
                        if inner.kind() == "this" || inner.kind() == "this_expression" =>
                    {
                        get_node_text(fld, self.source).to_string()
                    }
                    _ => get_node_text(object_field, self.source).to_string(),
                }
            } else {
                get_node_text(object_field, self.source).to_string()
            };
            // Strip PHP $ prefix from variable names
            if let Some(stripped) = receiver_name.strip_prefix('$') {
                receiver_name = stripped.to_string();
            }

            if !method_name.is_empty() {
                // Skip self/this/parent/static receivers — they don't aid resolution
                const SKIP_RECEIVERS: &[&str] =
                    &["self", "this", "cls", "super", "parent", "static"];
                if SKIP_RECEIVERS.contains(&receiver_name.as_str()) {
                    callee_name = method_name;
                } else {
                    callee_name = format!("{}.{}", receiver_name, method_name);
                }
            }
        } else if node_type == "message_expression" {
            // ObjC message expressions emit one `method` field child per selector
            // keyword: `[obj a:1 b:2 c:3]` has three `method=identifier` siblings.
            // Joining them with `:` reconstructs the full selector and matches the
            // multi-part selector names produced by the ObjC method_definition
            // extractor. Without this join, multi-keyword call sites only emitted
            // the first keyword and never resolved to their target methods.
            let mut method_keywords: Vec<String> = Vec::new();
            for i in 0..node.named_child_count() as u32 {
                if node.field_name_for_named_child(i) == Some("method") {
                    if let Some(kw) = node.named_child(i) {
                        method_keywords.push(get_node_text(kw, self.source).to_string());
                    }
                }
            }
            if !method_keywords.is_empty() {
                let method_name: String = if method_keywords.len() == 1 {
                    method_keywords[0].clone()
                } else {
                    method_keywords
                        .iter()
                        .map(|k| format!("{}:", k))
                        .collect::<Vec<_>>()
                        .join("")
                };
                let receiver_field = get_child_by_field(node, "receiver");
                const SKIP_RECEIVERS: &[&str] = &["self", "super"];
                match receiver_field {
                    Some(receiver) if receiver.kind() != "message_expression" => {
                        let receiver_name = get_node_text(receiver, self.source);
                        if !receiver_name.is_empty() && !SKIP_RECEIVERS.contains(&receiver_name) {
                            callee_name = format!("{}.{}", receiver_name, method_name);
                        } else {
                            callee_name = method_name;
                        }
                    }
                    _ => {
                        callee_name = method_name;
                    }
                }
            }
        } else {
            let func = get_child_by_field(node, "function").or_else(|| node.named_child(0));

            if let Some(func) = func {
                let func = if func.kind() == "generic_function" {
                    get_child_by_field(func, "function").unwrap_or(func)
                } else {
                    func
                };
                if matches!(
                    func.kind(),
                    "member_expression"
                        | "attribute"
                        | "selector_expression"
                        | "navigation_expression"
                        | "field_expression"
                ) {
                    // Method call: obj.method() or obj.field.method()
                    // Go uses selector_expression with 'field', JS/TS uses member_expression with 'property'
                    // Kotlin uses navigation_expression with navigation_suffix > simple_identifier
                    // C/C++ use field_expression for both `obj.method()` and `ptr->method()`
                    let mut property = get_child_by_field(func, "property")
                        .or_else(|| get_child_by_field(func, "field"));
                    if property.is_none() {
                        let child1 = func.named_child(1);
                        // Kotlin: navigation_suffix wraps the method name — extract simple_identifier from it
                        property = match child1 {
                            Some(c1) if c1.kind() == "navigation_suffix" => {
                                Some(find_named_child(c1, "simple_identifier").unwrap_or(c1))
                            }
                            other => other,
                        };
                    }
                    if let Some(property) = property {
                        let method_name = get_node_text(property, self.source).to_string();
                        // Include receiver name for qualified resolution (e.g., console.print → "console.print")
                        // This helps the resolver distinguish method calls from bare function calls.
                        // Skip self/this/cls as they don't aid resolution
                        let receiver = get_child_by_field(func, "object")
                            .or_else(|| get_child_by_field(func, "operand"))
                            .or_else(|| get_child_by_field(func, "argument"))
                            .or_else(|| func.named_child(0));
                        const SKIP_RECEIVERS: &[&str] = &["self", "this", "cls", "super"];
                        match receiver {
                            // Go: the receiver is itself a call — a factory chain
                            // `New().Method()` or `service.Order().Method()`. Keep
                            // the inner call so resolution can infer the method's
                            // type from what the inner call RETURNS (the #645/#608
                            // mechanism), encoded as `<innerCallee>().<method>`. An
                            // instance chain (`obj.Method().Other()`) shares the
                            // `selector_expression` inner-callee shape but a
                            // variable's type isn't recoverable here, so it must
                            // stay bare — re-encoding would drop the edge. The
                            // file's import set separates a package qualifier from
                            // a variable receiver.
                            Some(receiver)
                                if self.language == Language::Go
                                    && receiver.kind() == "call_expression" =>
                            {
                                callee_name =
                                    self.go_reencode_call_receiver(receiver, &method_name);
                            }
                            Some(receiver)
                                if matches!(
                                    receiver.kind(),
                                    "identifier" | "simple_identifier" | "field_identifier"
                                ) =>
                            {
                                let receiver_name = get_node_text(receiver, self.source);
                                if !SKIP_RECEIVERS.contains(&receiver_name) {
                                    callee_name = format!("{}.{}", receiver_name, method_name);
                                } else {
                                    callee_name = method_name;
                                }
                            }
                            _ => {
                                if let Some(receiver) =
                                    receiver.filter(|_| self.drops_rust_receiver(receiver))
                                {
                                    metadata = Some(dropped_receiver_metadata(
                                        crate::extraction::languages::rust::receiver_text(
                                            receiver,
                                            self.source,
                                        ),
                                    ));
                                }
                                callee_name = method_name;
                            }
                        }
                    }
                } else if func.kind() == "scoped_identifier"
                    || func.kind() == "scoped_call_expression"
                {
                    // Scoped call: Module::function()
                    callee_name = get_node_text(func, self.source).to_string();
                } else {
                    callee_name = get_node_text(func, self.source).to_string();
                }
            }
        }

        if !callee_name.is_empty() {
            self.push_call_reference_with(&caller_id, callee_name, node, metadata);
        }
    }
}

/// Walk a Go file's import declarations and collect the package identifiers it
/// binds — the alias when one is given, otherwise the import path's last
/// segment. Only descends `source_file` / `import_declaration` /
/// `import_spec_list`, matching the TS `goImportedPackages` walk.
fn collect_go_imported_packages(
    node: SyntaxNode<'_>,
    source: &str,
    pkgs: &mut std::collections::HashSet<String>,
) {
    use super::context::named_children;
    if node.kind() == "import_spec" {
        // An explicit alias (`svcx "repro/svc"` or `. "..."`) is a
        // package_identifier / identifier child. Prefer it over the path.
        if let Some(alias) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "package_identifier" || c.kind() == "identifier")
        {
            let text = get_node_text(alias, source);
            if !text.is_empty() {
                pkgs.insert(text.to_string());
            }
            return;
        }
        if let Some(path) = named_children(node)
            .into_iter()
            .find(|c| c.kind() == "interpreted_string_literal" || c.kind() == "raw_string_literal")
        {
            let raw = get_node_text(path, source).replace(['\'', '"', '`'], "");
            if let Some(last) = raw.split('/').next_back() {
                if !last.is_empty() {
                    pkgs.insert(last.to_string());
                }
            }
        }
        return;
    }
    if matches!(
        node.kind(),
        "source_file" | "import_declaration" | "import_spec_list"
    ) {
        for child in named_children(node) {
            collect_go_imported_packages(child, source, pkgs);
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::extraction::languages::extractor_for;
    use crate::extraction::tree_sitter_wrapper::TreeSitterExtractor;
    use crate::types::{EdgeKind, Language};

    #[test]
    fn cairo_and_sway_generic_calls_drop_type_arguments() {
        for (language, path, source) in [
            (
                Language::Cairo,
                "generic.cairo",
                "fn caller() { helper::<felt252>(1); }",
            ),
            (
                Language::Sway,
                "generic.sw",
                "script; fn caller() { helper::<u64>(1); }",
            ),
        ] {
            let result =
                TreeSitterExtractor::new(path, source, Some(language), extractor_for(language))
                    .extract();
            assert!(result.errors.is_empty(), "{language}: {:?}", result.errors);
            let calls: Vec<_> = result
                .unresolved_references
                .iter()
                .filter(|reference| reference.reference_kind == EdgeKind::Calls)
                .map(|reference| reference.reference_name.as_str())
                .collect();
            assert_eq!(calls, ["helper"], "{language}: {result:#?}");
        }
    }
}
