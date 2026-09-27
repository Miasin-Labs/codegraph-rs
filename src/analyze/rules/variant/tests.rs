use super::*;

fn parse(language: Language, source: &str) -> Tree {
    create_parser(language)
        .unwrap()
        .parse(source, None)
        .unwrap()
}

/// The statement at a line becomes a `bad` example that parses, in every
/// language with a wrapper.
#[test]
fn statements_become_examples_that_parse() {
    let cases = [
        (
            Language::Rust,
            "fn f(x: u8) {\n    match x {\n        0 => {\n            g();\n            None\n        }\n        _ => h(),\n    }\n}\n",
            3,
            "match_expression",
        ),
        (
            Language::C,
            "void f(int x) {\n    if (x) {\n        free(p);\n    }\n}\n",
            2,
            "if_statement",
        ),
        (
            Language::Python,
            "def f(x):\n    for y in x:\n        eval(y)\n",
            2,
            "for_statement",
        ),
        (
            Language::Java,
            "class A {\n    void f() {\n        try {\n            g();\n        } catch (Exception e) {}\n    }\n}\n",
            3,
            "try_statement",
        ),
        (
            Language::Javascript,
            "function f(x) {\n  if (x) {\n    eval(x);\n  }\n}\n",
            2,
            "if_statement",
        ),
    ];
    for (language, source, line, statement) in cases {
        let tree = parse(language, source);
        let rules = lang::for_language(language);
        let node = statement_at(&tree, source, line).unwrap();
        let code = example_code(node, source, language, rules)
            .unwrap_or_else(|| panic!("{language:?}: no example"));
        assert!(parses(&code, language), "{language:?}:\n{code}");
        let root = parse(language, &code);
        let mut found = false;
        let mut stack = vec![root.root_node()];
        while let Some(n) = stack.pop() {
            found |= n.kind() == statement;
            let mut cursor = n.walk();
            stack.extend(n.named_children(&mut cursor));
        }
        assert!(found, "{language:?}: no {statement} in\n{code}");
    }
}

#[test]
fn the_node_at_a_line_is_the_outermost_starting_there() {
    let source = "fn f() {\n    let a = g(1);\n    a.h().await;\n}\n";
    let tree = parse(Language::Rust, source);
    assert_eq!(
        statement_at(&tree, source, 2).unwrap().kind(),
        "let_declaration"
    );
    assert_eq!(
        statement_at(&tree, source, 3).unwrap().kind(),
        "expression_statement"
    );
    assert_eq!(
        statement_at(&tree, source, 1).unwrap().kind(),
        "function_item"
    );
    let source = "fn f() {\n\n}\n";
    assert!(statement_at(&parse(Language::Rust, source), source, 2).is_none());
}

#[test]
fn trees_print_fields_and_leaf_text_compactly() {
    let source = "fn f() { if a != b { g(); } }\n";
    let tree = parse(Language::Rust, source);
    let node = tree.root_node().named_child(0).unwrap();
    let printed = sexp(node, source);
    assert!(
        printed.starts_with("(function_item\n  name: (identifier)  ; f"),
        "{printed}"
    );
    assert!(printed.contains("operator: \"!=\""), "{printed}");
    assert!(printed.contains("left: (identifier)  ; a"), "{printed}");
    // Parentheses balance outside the `;` comments.
    let code: String = printed
        .lines()
        .map(|l| l.split("  ; ").next().unwrap())
        .collect();
    assert_eq!(
        code.matches('(').count(),
        code.matches(')').count(),
        "{printed}"
    );
}

#[test]
fn dedent_keeps_relative_indentation() {
    let text = "match x {\n            0 => a,\n            _ => {\n                b\n            }\n        }";
    assert_eq!(
        dedent(text, "    "),
        "    match x {\n        0 => a,\n        _ => {\n            b\n        }\n    }"
    );
}
