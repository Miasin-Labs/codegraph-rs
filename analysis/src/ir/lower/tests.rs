use tree_sitter::{Language, Node, Parser, Tree};

use super::lower_with_rules;
use crate::ir::{IrFunction, IrOp, Operand, Place, Var};

pub(crate) fn grammar(lang: &str) -> Language {
    match lang {
        "java" => tree_sitter_java::LANGUAGE.into(),
        "c" => tree_sitter_c::LANGUAGE.into(),
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        "php" => tree_sitter_php::LANGUAGE_PHP.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "javascript" | "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        other => panic!("no grammar for {other}"),
    }
}

pub(crate) fn parse(lang: &str, source: &str) -> Tree {
    let mut parser = Parser::new();
    parser.set_language(&grammar(lang)).unwrap();
    parser.parse(source, None).unwrap()
}

/// The first function-like node (one with a body the lowering accepts).
fn first_function<'t>(lang: &str, node: Node<'t>) -> Option<Node<'t>> {
    let cfg = crate::cfg_rules::CfgRules::for_language(lang).unwrap();
    if cfg.function_nodes.contains(&node.kind()) {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children: Vec<Node<'t>> = node.named_children(&mut cursor).collect();
    children
        .into_iter()
        .find_map(|child| first_function(lang, child))
}

/// Lower the first function of `source`.
pub(crate) fn lower(lang: &str, source: &str) -> IrFunction {
    let tree = parse(lang, source);
    let function = first_function(lang, tree.root_node()).expect("a function");
    lower_with_rules(lang, function, source).expect("lowered")
}

fn calls(ir: &IrFunction) -> Vec<&str> {
    ir.body
        .iter()
        .filter_map(|op| match op {
            IrOp::Call { callee, .. } => Some(callee.as_str()),
            _ => None,
        })
        .collect()
}

fn assigned(ir: &IrFunction) -> Vec<&str> {
    ir.body
        .iter()
        .filter_map(|op| match op {
            IrOp::Assign { dst, .. } if !dst.as_str().starts_with("__t") => Some(dst.as_str()),
            _ => None,
        })
        .collect()
}

#[test]
fn java_statements_lower_with_spans_and_values() {
    let src = "class A {\n  void f(String p, int n) {\n    String x = req.getParameter(\"a\");\n    x = x + p;\n    stmt.executeQuery(x);\n  }\n}\n";
    let ir = lower("java", src);
    assert_eq!(ir.name, "f");
    assert_eq!(ir.params, vec![Var::new("p"), Var::new("n")]);
    assert_eq!(ir.param_spans[0].line, 2);
    assert_eq!(calls(&ir), vec!["req.getParameter", "stmt.executeQuery"]);
    assert_eq!(assigned(&ir), vec!["x", "x"]);
    assert_eq!(ir.spans.len(), ir.body.len());
    // Each call op sits at its call's start (line, column), where the index
    // records call edges.
    let call_spans: Vec<(u32, u32)> = ir
        .body
        .iter()
        .zip(&ir.spans)
        .filter(|(op, _)| matches!(op, IrOp::Call { .. }))
        .map(|(_, span)| (span.line, span.col))
        .collect();
    assert_eq!(call_spans, vec![(3, 15), (5, 4)]);
    // The receiver is a value, not an argument.
    let IrOp::Call { receiver, args, .. } = ir
        .body
        .iter()
        .find(|op| matches!(op, IrOp::Call { callee, .. } if callee.ends_with("executeQuery")))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(receiver, &Some(Operand::var("stmt")));
    assert_eq!(args, &vec![Operand::var("x")]);
    // Values are recorded by span: the argument `x` of executeQuery.
    let arg = ir
        .values
        .iter()
        .find(|v| v.span.line == 5 && v.kind == "identifier" && v.span.col == 22)
        .expect("value of the argument");
    assert_eq!(arg.operand, Operand::var("x"));
    assert_eq!(arg.place, Some(Place::var(Var::new("x"))));
}

#[test]
fn java_control_flow_lowers_to_labels() {
    let src = r#"class A { void f(boolean c) {
        String s = "";
        if (c) { s = a(); } else if (d) { s = b(); } else { s = e(); }
        while (c) { if (x) break; s = g(); }
        for (int i = 0; i < 3; i++) { continue; }
        for (String v : list) { s = v; }
        switch (s) { case "a": s = h(); break; default: s = k(); }
        try { s = m(); } catch (Exception ex) { s = ex.getMessage(); } finally { n(); }
        String t = c ? s : "z";
        return;
    } }"#;
    let ir = lower("java", src);
    for callee in [
        "a",
        "b",
        "e",
        "g",
        "h",
        "k",
        "m",
        "ex.getMessage",
        "n",
        "<next>",
    ] {
        assert!(
            calls(&ir).contains(&callee),
            "missing {callee}: {:?}",
            calls(&ir)
        );
    }
    // Every jump lands on a label of the function.
    for op in &ir.body {
        if let IrOp::Jump { target } | IrOp::Branch { target, .. } = op {
            assert!(ir.labels.contains_key(target), "dangling {target:?}");
        }
    }
    assert!(
        assigned(&ir).contains(&"v"),
        "the for-each binding is defined"
    );
    assert!(assigned(&ir).contains(&"t"));
}

#[test]
fn java_members_and_elements_are_places() {
    let src = "class A { void f() { String y = req.param.x; a[0] = y; b[i] = y; this.f = y; String z = (String) m.get(\"k\"); } }";
    let ir = lower("java", src);
    let places: Vec<String> = ir
        .values
        .iter()
        .filter_map(|v| v.place.as_ref().map(|p| p.to_string()))
        .collect();
    assert!(places.contains(&"req.param.x".to_string()), "{places:?}");
    let writes: Vec<String> = ir
        .body
        .iter()
        .filter_map(|op| match op {
            IrOp::FieldWrite { field, .. } => Some(field.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(writes, vec!["[0]", "[]", "f"]);
}

#[test]
fn c_declarations_out_arguments_and_pointers() {
    let src = r#"
void bad() {
    char *data;
    char buf[100] = "ls ";
    data = buf;
    size_t n = strlen(data);
    if (fgets(data + n, (int)(100 - n), stdin) != NULL) { data[n - 1] = '\0'; }
    system(data);
}"#;
    let ir = lower("c", src);
    assert_eq!(ir.name, "bad");
    assert_eq!(calls(&ir), vec!["strlen", "fgets", "system"]);
    assert_eq!(assigned(&ir), vec!["buf", "data", "n"]);
    // `fgets(data + n, …)` writes through `data`.
    let fgets = ir
        .body
        .iter()
        .position(|op| matches!(op, IrOp::Call { callee, .. } if callee == "fgets"))
        .unwrap();
    let places = ir.call_places(fgets).expect("recorded");
    assert_eq!(places.args.len(), 3);
    // `data + n` points into `data`.
    assert_eq!(places.args[0], Some(Place::var(Var::new("data"))));
    let arith = ir
        .values
        .iter()
        .find(|v| v.kind == "binary_expression" && v.span.line == 7)
        .unwrap();
    assert!(matches!(arith.operand, Operand::Var(_)));
}

#[test]
fn c_address_of_designates_its_operand() {
    let ir = lower("c", "void f() { int x; fscanf(stdin, \"%d\", &x); }");
    let call = ir
        .body
        .iter()
        .position(|op| matches!(op, IrOp::Call { .. }))
        .unwrap();
    assert_eq!(
        ir.call_places(call).unwrap().args[2],
        Some(Place::var(Var::new("x")))
    );
}

#[test]
fn object_like_macros_read_as_what_they_stand_for() {
    let src = "#define ARG3 data\n#define ARG1 \"-c\"\n#ifdef W\n#define PATH \"a\"\n#else\n#define PATH \"b\"\n#endif\nvoid f(char *data) { execl(PATH, ARG1, ARG3, NULL); }";
    let tree = parse("c", src);
    let macros = crate::ir::macro_aliases("c", tree.root_node(), src);
    assert_eq!(macros.get("ARG3").map(String::as_str), Some("data"));
    assert_eq!(macros.get("ARG1").map(String::as_str), Some("\"-c\""));
    assert!(!macros.contains_key("PATH"), "defined two ways");
    let function = first_function("c", tree.root_node()).unwrap();
    let ir = crate::ir::lower_with_macros("c", function, src, &macros).unwrap();
    let args = ir
        .body
        .iter()
        .find_map(|op| match op {
            IrOp::Call { args, .. } => Some(args.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(args[1], Operand::constant("\"-c\""));
    assert_eq!(args[2], Operand::var("data"));
}

#[test]
fn python_js_php_strings_interpolate() {
    let py = lower("python", "def f(a):\n    q = f\"select {a}\"\n    run(q)\n");
    assert!(calls(&py).contains(&"<concat>"), "{:?}", calls(&py));
    let js = lower(
        "javascript",
        "function f(a) { const q = `select ${a}`; run(q); }",
    );
    assert!(calls(&js).contains(&"<concat>"), "{:?}", calls(&js));
    let php = lower(
        "php",
        "<?php function f($a) { $q = \"select $a\"; run($q); }",
    );
    assert!(calls(&php).contains(&"<concat>"), "{:?}", calls(&php));
    assert_eq!(php.params, vec![Var::new("$a")]);
    // A plain string stays a constant.
    let plain = lower("python", "def f():\n    q = \"select 1\"\n");
    assert!(!calls(&plain).contains(&"<concat>"));
}

#[test]
fn top_level_code_lowers_without_its_functions() {
    let src = "<?php\n$id = $_GET['id'];\nfunction g($x) { return h($x); }\n$r = mysqli_query($c, \"q $id\");\n";
    let tree = parse("php", src);
    let ir = lower_with_rules("php", tree.root_node(), src).expect("top level");
    assert_eq!(assigned(&ir), vec!["$id", "$r"]);
    assert!(!calls(&ir).contains(&"h"), "function bodies are their own");
}

#[test]
fn compound_assignment_reads_its_target() {
    let ir = lower(
        "java",
        "class A { void f(String p) { String s = \"a\"; s += p; } }",
    );
    let binop = ir
        .body
        .iter()
        .find_map(|op| match op {
            IrOp::BinOp { lhs, rhs, .. } => Some((lhs.clone(), rhs.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(binop, (Operand::var("s"), Operand::var("p")));
    assert_eq!(assigned(&ir), vec!["s", "s"]);
}

#[test]
fn unknown_syntax_keeps_its_parts() {
    // A lambda is a constant, but the call keeps both argument positions.
    let ir = lower("java", "class A { void f(String p) { g(x -> x, p); } }");
    let args = ir
        .body
        .iter()
        .find_map(|op| match op {
            IrOp::Call { args, .. } => Some(args.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(args.len(), 2);
    assert_eq!(args[1], Operand::var("p"));
}

#[test]
fn deep_nesting_does_not_overflow() {
    let depth = 3000;
    let expr = format!("{}x{}", "(".repeat(depth), ")".repeat(depth));
    let src = format!("class A {{ void f(int x) {{ int y = {expr}; }} }}");
    let ir = lower("java", &src);
    assert!(assigned(&ir).contains(&"y"));
}
