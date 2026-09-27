//! Per-language syntax the predicates need, as static tables looked up by
//! language: which nodes are calls (and which child holds their arguments),
//! which are functions, and how a Rust test is marked. The predicate code
//! never branches on a language.

use tree_sitter::Node;

use crate::extraction::grammar_language;
use crate::types::{LANGUAGES, Language};

pub(super) struct LangRules {
    /// Node kinds that are a call of something.
    pub calls: &'static [&'static str],
    /// Fields that hold a call's arguments (the callee is what precedes them).
    pub argument_fields: &'static [&'static str],
    /// Node kinds that are a function, method or closure body owner.
    pub functions: &'static [&'static str],
    /// Sibling node kinds before a function that can mark it a test
    /// (`#[test]`, `@Test`), matched on their text.
    pub test_markers: &'static [&'static str],
    /// Module node kinds whose name (`tests`) makes the functions inside
    /// test code.
    pub test_modules: &'static [&'static str],
    /// The language id the analysis crate lowers it to IR as (taint
    /// rules run where there is one).
    pub ir: Option<&'static str>,
}

const C: LangRules = LangRules {
    calls: &["call_expression"],
    argument_fields: &["arguments"],
    functions: &["function_definition"],
    test_markers: &[],
    test_modules: &[],
    ir: Some("c"),
};

const CPP: LangRules = LangRules {
    calls: &["call_expression", "new_expression"],
    argument_fields: &["arguments"],
    functions: &["function_definition", "lambda_expression"],
    test_markers: &[],
    test_modules: &[],
    ir: Some("cpp"),
};

const RUST: LangRules = LangRules {
    calls: &["call_expression", "macro_invocation"],
    argument_fields: &["arguments"],
    functions: &["function_item", "closure_expression"],
    test_markers: &["attribute_item"],
    test_modules: &["mod_item"],
    ir: None,
};

const GO: LangRules = LangRules {
    calls: &["call_expression"],
    argument_fields: &["arguments"],
    functions: &["function_declaration", "method_declaration", "func_literal"],
    test_markers: &[],
    test_modules: &[],
    ir: None,
};

const PYTHON: LangRules = LangRules {
    calls: &["call"],
    argument_fields: &["arguments"],
    functions: &["function_definition", "lambda"],
    test_markers: &["decorator"],
    test_modules: &[],
    ir: Some("python"),
};

const JS: LangRules = LangRules {
    calls: &["call_expression", "new_expression"],
    argument_fields: &["arguments"],
    functions: &[
        "function_declaration",
        "function_expression",
        "function",
        "arrow_function",
        "method_definition",
        "generator_function_declaration",
        "generator_function",
    ],
    test_markers: &[],
    test_modules: &[],
    ir: Some("javascript"),
};

const JAVA: LangRules = LangRules {
    calls: &["method_invocation", "object_creation_expression"],
    argument_fields: &["arguments"],
    functions: &[
        "method_declaration",
        "constructor_declaration",
        "lambda_expression",
    ],
    test_markers: &["modifiers"],
    test_modules: &[],
    ir: Some("java"),
};

const CSHARP: LangRules = LangRules {
    calls: &["invocation_expression", "object_creation_expression"],
    argument_fields: &["arguments"],
    functions: &[
        "method_declaration",
        "constructor_declaration",
        "local_function_statement",
        "lambda_expression",
    ],
    test_markers: &["attribute_list"],
    test_modules: &[],
    ir: None,
};

const PHP: LangRules = LangRules {
    calls: &[
        "function_call_expression",
        "member_call_expression",
        "scoped_call_expression",
        "object_creation_expression",
    ],
    argument_fields: &["arguments"],
    functions: &[
        "function_definition",
        "method_declaration",
        "anonymous_function",
        "arrow_function",
    ],
    test_markers: &[],
    test_modules: &[],
    ir: Some("php"),
};

const RUBY: LangRules = LangRules {
    calls: &["call"],
    argument_fields: &["arguments"],
    functions: &["method", "singleton_method", "lambda", "block", "do_block"],
    test_markers: &[],
    test_modules: &[],
    ir: None,
};

/// Any other language with a grammar: the common names.
const GENERIC: LangRules = LangRules {
    calls: &[
        "call_expression",
        "call",
        "method_invocation",
        "invocation_expression",
    ],
    argument_fields: &["arguments", "argument_list", "value_arguments"],
    functions: &[
        "function_definition",
        "function_declaration",
        "method_declaration",
        "function_item",
        "method_definition",
    ],
    test_markers: &[],
    test_modules: &[],
    ir: None,
};

pub(super) fn for_language(language: Language) -> &'static LangRules {
    match language {
        Language::C => &C,
        Language::Cpp | Language::Objc => &CPP,
        Language::Rust => &RUST,
        Language::Go => &GO,
        Language::Python => &PYTHON,
        Language::Javascript
        | Language::Typescript
        | Language::Tsx
        | Language::Jsx
        | Language::Arkts => &JS,
        Language::Java => &JAVA,
        Language::Csharp => &CSHARP,
        Language::Php => &PHP,
        Language::Ruby => &RUBY,
        _ => &GENERIC,
    }
}

/// A language name as rules write it (`c`, `cpp`/`c++`, `rust`, `ts`…),
/// if it has a grammar.
pub(super) fn parse_language(name: &str) -> Result<Language, String> {
    let lower = name.trim().to_ascii_lowercase();
    let canonical = match lower.as_str() {
        "c++" | "cxx" | "cc" => "cpp",
        "js" => "javascript",
        "ts" => "typescript",
        "py" => "python",
        "golang" => "go",
        "rs" => "rust",
        "c#" | "cs" => "csharp",
        "rb" => "ruby",
        other => other,
    };
    match canonical.parse::<Language>() {
        Ok(language) if grammar_language(language).is_some() => Ok(language),
        _ => Err(format!(
            "unknown language `{name}` — rules run on: {}",
            known_languages().join(", ")
        )),
    }
}

fn known_languages() -> Vec<&'static str> {
    LANGUAGES
        .iter()
        .filter(|language| grammar_language(**language).is_some())
        .map(|language| language.as_str())
        .collect()
}

/// The innermost call node at or above `node`.
pub(super) fn enclosing_call<'t>(rules: &LangRules, node: Node<'t>) -> Option<Node<'t>> {
    let mut current = Some(node);
    while let Some(n) = current {
        if rules.calls.contains(&n.kind()) {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// The innermost function node strictly above or at `node`.
pub(super) fn enclosing_function<'t>(rules: &LangRules, node: Node<'t>) -> Option<Node<'t>> {
    let mut current = Some(node);
    while let Some(n) = current {
        if rules.functions.contains(&n.kind()) {
            return Some(n);
        }
        current = n.parent();
    }
    None
}

/// What a call calls, as written: the code before its argument list,
/// whitespace and generic arguments removed (`client.send`,
/// `reqwest::get`, `println!`, `new Foo`), and its last name (`send`).
pub(super) fn callee(rules: &LangRules, call: Node, source: &str) -> (String, String) {
    let arguments = rules
        .argument_fields
        .iter()
        .find_map(|field| call.child_by_field_name(field))
        .or_else(|| {
            let count = call.named_child_count();
            (count > 1)
                .then(|| call.named_child((count - 1) as u32))
                .flatten()
                .filter(|last| last.start_byte() > call.start_byte())
        });
    let end = arguments.map_or(call.end_byte(), |a| a.start_byte());
    let raw = source.get(call.start_byte()..end).unwrap_or_default();
    let text = strip_generics(
        &raw.chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>(),
    );
    let text = text.trim_end_matches(['?', '.', '(']).to_string();
    let name = last_name(&text).to_string();
    (text, name)
}

/// The last name of a callee as written (`client.send` → `send`,
/// `println!` → `println`, `new Foo` → `Foo`).
pub(super) fn last_name(callee: &str) -> &str {
    callee
        .rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$' || c == '!'))
        .find(|part| !part.is_empty())
        .unwrap_or_default()
        .trim_end_matches('!')
}

/// `a::<T>::b<U>` → `a::::b`: drop balanced `<…>` groups.
fn strip_generics(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0usize;
    for ch in text.chars() {
        match ch {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.replace("::::", "::").trim_end_matches("::").to_string()
}

/// The name a function node declares (the innermost declarator for C/C++).
pub(super) fn function_name(node: Node, source: &str) -> String {
    let mut current = node;
    for _ in 0..8 {
        if let Some(name) = current.child_by_field_name("name") {
            return text(name, source);
        }
        match current.child_by_field_name("declarator") {
            Some(declarator) => current = declarator,
            None => break,
        }
        if matches!(
            current.kind(),
            "identifier" | "field_identifier" | "qualified_identifier" | "destructor_name"
        ) {
            return text(current, source);
        }
    }
    // A function assigned to a name (`const f = () => …`).
    node.parent()
        .and_then(|parent| parent.child_by_field_name("name"))
        .map(|name| text(name, source))
        .unwrap_or_default()
}

/// Whether the syntax marks `function` as test code: a test attribute or
/// decorator before it, a test-named function (`test_x`, `TestX`), or a
/// `mod tests` around it.
pub(super) fn is_test_function(rules: &LangRules, function: Node, source: &str) -> bool {
    let name = function_name(function, source);
    if name == "test"
        || name.starts_with("test_")
        || (name.starts_with("Test")
            && name[4..]
                .chars()
                .next()
                .is_none_or(|c| c.is_uppercase() || c == '_'))
    {
        return true;
    }
    let marked = |node: Node| {
        rules.test_markers.contains(&node.kind()) && {
            let text = text(node, source).to_ascii_lowercase();
            text.contains("test")
        }
    };
    let mut sibling = function.prev_named_sibling();
    for _ in 0..4 {
        match sibling {
            Some(node) if rules.test_markers.contains(&node.kind()) => {
                if marked(node) {
                    return true;
                }
                sibling = node.prev_named_sibling();
            }
            _ => break,
        }
    }
    // Decorators and modifiers held inside the node (Python's
    // decorated_definition parent, Java's modifiers child).
    if function.parent().is_some_and(|parent| {
        parent.kind() == "decorated_definition"
            && (0..parent.named_child_count() as u32)
                .filter_map(|i| parent.named_child(i))
                .any(marked)
    }) {
        return true;
    }
    if (0..function.named_child_count().min(2) as u32)
        .filter_map(|i| function.named_child(i))
        .any(marked)
    {
        return true;
    }
    let mut ancestor = function.parent();
    while let Some(node) = ancestor {
        if rules.test_modules.contains(&node.kind())
            && node
                .child_by_field_name("name")
                .is_some_and(|name| matches!(text(name, source).as_str(), "tests" | "test"))
        {
            return true;
        }
        ancestor = node.parent();
    }
    false
}

pub(super) fn text(node: Node, source: &str) -> String {
    source
        .get(node.byte_range())
        .unwrap_or_default()
        .to_string()
}
