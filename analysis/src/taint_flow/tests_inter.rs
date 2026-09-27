//! Summaries and their composition over a program: Java classes lowered
//! method by method, calls resolved by name within the source.

use std::collections::HashMap;
use std::sync::Arc;

use super::program::{CallTargets, Function, Solution, Solver};
use super::*;
use crate::ir::shared::{self, SharedNames};
use crate::ir::{IrOp, lower_with_rules};

/// A Java source's methods, lowered, with their byte ranges and names.
struct Methods {
    src: String,
    ranges: Vec<(usize, usize)>,
    names: Vec<String>,
    functions: Vec<Function>,
}

/// Fields declared in the source are shared, keyed `Class.field`.
struct Fields {
    fields: Vec<(String, String)>,
    class_of: String,
}

impl SharedNames for Fields {
    fn bare(&self, name: &str) -> Option<String> {
        self.fields
            .iter()
            .find(|(class, field)| *class == self.class_of && field == name)
            .map(|(class, field)| format!("{class}.{field}"))
    }
    fn member(&self, owner: &str, field: &str) -> Option<String> {
        let class = if owner == "this" {
            self.class_of.as_str()
        } else {
            owner
        };
        self.fields
            .iter()
            .find(|(c, f)| c == class && f == field)
            .map(|(c, f)| format!("{c}.{f}"))
    }
}

fn text<'a>(node: tree_sitter::Node<'_>, src: &'a str) -> &'a str {
    &src[node.byte_range()]
}

fn methods(src: &str) -> Methods {
    program("java", src)
}

/// The functions of a Java or C++ source, lowered, calls resolved by name.
fn program(lang: &'static str, src: &str) -> Methods {
    let mut parser = tree_sitter::Parser::new();
    let grammar: tree_sitter::Language = match lang {
        "cpp" => tree_sitter_cpp::LANGUAGE.into(),
        _ => tree_sitter_java::LANGUAGE.into(),
    };
    parser.set_language(&grammar).expect("grammar");
    let tree = parser.parse(src, None).expect("parse");
    let mut fields = Vec::new();
    let mut found = Vec::new();
    let mut stack = vec![(tree.root_node(), String::new())];
    while let Some((node, class)) = stack.pop() {
        let class = if matches!(node.kind(), "class_declaration" | "interface_declaration") {
            node.child_by_field_name("name")
                .map(|n| text(n, src).to_string())
                .unwrap_or(class)
        } else {
            class
        };
        if node.kind() == "field_declaration" {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if child.kind() == "variable_declarator" {
                    if let Some(name) = child.child_by_field_name("name") {
                        fields.push((class.clone(), text(name, src).to_string()));
                    }
                }
            }
        }
        if matches!(
            node.kind(),
            "method_declaration" | "constructor_declaration" | "function_definition"
        ) && node.child_by_field_name("body").is_some()
        {
            found.push((node, class.clone()));
        }
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            stack.push((child, class.clone()));
        }
    }
    found.sort_by_key(|(node, _)| node.start_byte());
    let mut out = Methods {
        src: src.to_string(),
        ranges: Vec::new(),
        names: Vec::new(),
        functions: Vec::new(),
    };
    for (node, class) in &found {
        let mut ir = lower_with_rules(lang, *node, src).expect("lowers");
        shared::canonicalize(
            &mut ir,
            &Fields {
                fields: fields.clone(),
                class_of: class.clone(),
            },
        );
        out.ranges.push((node.start_byte(), node.end_byte()));
        out.names.push(ir.name.clone());
        out.functions.push(Function {
            ir: Arc::new(ir),
            language: lang,
            calls: HashMap::new(),
        });
    }
    // Calls resolve by name to every method of that name.
    let names = out.names.clone();
    for function in &mut out.functions {
        for (op, ir_op) in function.ir.body.iter().enumerate() {
            let IrOp::Call { callee, .. } = ir_op else {
                continue;
            };
            let last = callee.rsplit(['.', ':']).next().unwrap_or(callee);
            let targets: Vec<FuncId> = names
                .iter()
                .enumerate()
                .filter(|(_, name)| *name == last)
                .map(|(id, _)| id as FuncId)
                .collect();
            if !targets.is_empty() {
                function.calls.insert(
                    op,
                    CallTargets {
                        targets,
                        in_project: true,
                        ..CallTargets::default()
                    },
                );
            }
        }
    }
    out
}

impl Methods {
    fn id(&self, name: &str) -> FuncId {
        self.names
            .iter()
            .position(|n| n == name)
            .unwrap_or_else(|| panic!("no method {name}")) as FuncId
    }

    /// The function holding byte `at`.
    fn owner(&self, at: usize) -> FuncId {
        self.ranges
            .iter()
            .enumerate()
            .filter(|(_, (start, end))| *start <= at && at < *end)
            .max_by_key(|(_, (start, _))| *start)
            .map(|(id, _)| id as FuncId)
            .expect("inside a method")
    }

    /// Mark `needle` inside the `nth` occurrence of `context` (a needle
    /// written `ctx|needle`; a bare needle is its own context).
    fn mark(&self, needle: &str, nth: usize) -> (FuncId, Mark) {
        let (context, needle) = needle.split_once('|').unwrap_or((needle, needle));
        let base = self
            .src
            .match_indices(context)
            .nth(nth)
            .unwrap_or_else(|| panic!("no {context} #{nth}"))
            .0;
        let start = base + context.find(needle).expect("needle in context");
        (
            self.owner(start),
            Mark {
                start_byte: start,
                end_byte: start + needle.len(),
                kind: None,
            },
        )
    }

    fn line_of(&self, byte: usize) -> u32 {
        self.src[..byte].matches('\n').count() as u32 + 1
    }
}

/// Specs from (role, needle, occurrence) triples.
fn specs(methods: &Methods, marks: &[(&str, &str, usize)]) -> HashMap<FuncId, TaintSpec> {
    let mut specs: HashMap<FuncId, TaintSpec> = HashMap::new();
    for &(role, needle, nth) in marks {
        let (func, mark) = methods.mark(needle, nth);
        let spec = specs.entry(func).or_default();
        match role {
            "source" => spec.sources.push(mark),
            "sink" => spec.sinks.push(mark),
            "sanitizer" => spec.sanitizers.push(mark),
            _ => panic!("role {role}"),
        }
    }
    specs
}

fn solve(methods: &Methods, specs: &HashMap<FuncId, TaintSpec>) -> Solution {
    let mut solver = Solver::new(&methods.functions);
    solver.run(specs, &mut Budget::unlimited())
}

const PARAM: &str = "class A {\n\
  void f(HttpServletRequest req, Statement st) {\n\
    String p = req.getParameter(\"a\");\n\
    String q = id(p);\n\
    st.executeQuery(q);\n\
  }\n\
  void g(HttpServletRequest req, Statement st) {\n\
    String p = req.getParameter(\"a\");\n\
    String q = constant(p);\n\
    st.executeQuery(q);\n\
  }\n\
  String id(String x) {\n\
    String y = \"a\" + x;\n\
    return y;\n\
  }\n\
  String constant(String x) {\n\
    return \"safe\";\n\
  }\n\
}";

#[test]
fn a_parameter_reaches_the_return_value() {
    let methods = methods(PARAM);
    let specs = specs(
        &methods,
        &[
            ("source", "req.getParameter(\"a\")", 0),
            ("sink", "executeQuery(q)|q", 0),
            ("source", "req.getParameter(\"a\")", 1),
            ("sink", "executeQuery(q)|q", 1),
        ],
    );
    let solution = solve(&methods, &specs);
    let f = methods.id("f");
    assert_eq!(solution.flows.len(), 1, "{:?}", solution.flows);
    let flow = &solution.flows[0];
    assert_eq!(flow.sink.func, f, "only f's sink is reached");
    // The path goes through `id`: its concatenation (line 13) and return.
    let id = methods.id("id");
    assert!(
        flow.path
            .iter()
            .any(|step| step.func == id && step.span.line == 13)
    );
    assert_eq!(flow.path.last().map(|step| step.span.line), Some(5));
}

#[test]
fn summaries_say_what_reaches_the_return_value() {
    let methods = methods(PARAM);
    let mut solver = Solver::new(&methods.functions);
    // A rule that marks nothing: pure summaries through `f`'s callees.
    let solution = solver.run(&HashMap::new(), &mut Budget::unlimited());
    assert!(solution.flows.is_empty());
    let resolve = |_| CallResolution::default();
    let id = methods.id("id");
    let (_, summary) = analyze_function(
        &methods.functions[id as usize].ir,
        id,
        "java",
        &TaintSpec::default(),
        &resolve,
        &mut Budget::unlimited(),
    );
    let summary = summary.expect("analyzed");
    assert_eq!(
        summary
            .returns
            .inputs
            .iter()
            .map(|(a, _)| a.clone())
            .collect::<Vec<_>>(),
        vec![Access::slot(Slot::Param(0))]
    );
    let constant = methods.id("constant");
    let (_, summary) = analyze_function(
        &methods.functions[constant as usize].ir,
        constant,
        "java",
        &TaintSpec::default(),
        &resolve,
        &mut Budget::unlimited(),
    );
    assert!(summary.expect("analyzed").returns.is_empty());
}

#[test]
fn a_parameter_reaches_a_sink_in_the_callee() {
    let src = "class A {\n\
      void f(HttpServletRequest req, Statement st) {\n\
        String p = req.getParameter(\"a\");\n\
        run(st, p);\n\
        run(st, \"constant\");\n\
      }\n\
      void run(Statement st, String sql) {\n\
        st.executeQuery(sql);\n\
      }\n\
    }";
    let methods = methods(src);
    let specs = specs(
        &methods,
        &[
            ("source", "req.getParameter(\"a\")", 0),
            ("sink", "executeQuery(sql)|sql", 0),
        ],
    );
    let solution = solve(&methods, &specs);
    assert_eq!(solution.flows.len(), 1, "{:?}", solution.flows);
    let flow = &solution.flows[0];
    let run = methods.id("run");
    assert_eq!(flow.sink.func, run, "reported at the sink, in the callee");
    assert_eq!(flow.source.func, methods.id("f"));
    let lines: Vec<u32> = flow.path.iter().map(|step| step.span.line).collect();
    assert_eq!(lines.first(), Some(&3), "from the source");
    assert!(lines.contains(&4), "through the call");
    assert_eq!(lines.last(), Some(&8), "to the sink: {lines:?}");
    // The summary of `run` says its second parameter reaches the sink.
    let (_, summary) = analyze_function(
        &methods.functions[run as usize].ir,
        run,
        "java",
        &specs[&run],
        &|_| CallResolution::default(),
        &mut Budget::unlimited(),
    );
    let summary = summary.expect("analyzed");
    assert_eq!(summary.sinks.len(), 1);
    assert_eq!(summary.sinks[0].0, Access::slot(Slot::Param(1)));
}

#[test]
fn a_source_the_callee_returns_reaches_the_caller_sink() {
    let src = "class A {\n\
      void f(HttpServletRequest req, Statement st) {\n\
        String v = fetch(req);\n\
        st.executeQuery(v);\n\
        String w = safe(req);\n\
        st.executeQuery(w);\n\
      }\n\
      String fetch(HttpServletRequest r) {\n\
        return r.getParameter(\"x\");\n\
      }\n\
      String safe(HttpServletRequest r) {\n\
        return \"bar\";\n\
      }\n\
    }";
    let methods = methods(src);
    let specs = specs(
        &methods,
        &[
            ("source", "r.getParameter(\"x\")", 0),
            ("sink", "executeQuery(v)|v", 0),
            ("sink", "executeQuery(w)|w", 0),
        ],
    );
    let solution = solve(&methods, &specs);
    assert_eq!(solution.flows.len(), 1, "{:?}", solution.flows);
    assert_eq!(solution.flows[0].source.func, methods.id("fetch"));
    assert_eq!(solution.flows[0].sink.index, 0, "the `v` sink only");
}

#[test]
fn recursion_reaches_a_fixpoint() {
    let src = "class A {\n\
      void f(HttpServletRequest req, Statement st) {\n\
        String p = req.getParameter(\"a\");\n\
        String q = rec(p, 3);\n\
        st.executeQuery(q);\n\
      }\n\
      String rec(String s, int n) {\n\
        if (n > 0) {\n\
          return ping(s, n - 1);\n\
        }\n\
        return s;\n\
      }\n\
      String ping(String s, int n) {\n\
        return rec(s + \"!\", n);\n\
      }\n\
    }";
    let methods = methods(src);
    let specs = specs(
        &methods,
        &[
            ("source", "req.getParameter(\"a\")", 0),
            ("sink", "executeQuery(q)|q", 0),
        ],
    );
    let solution = solve(&methods, &specs);
    assert_eq!(solution.flows.len(), 1, "{:?}", solution.flows);
    // A recursion that never hands its argument back carries nothing.
    let dropped = src.replace("return s;", "return \"x\";");
    let methods = self::methods(&dropped);
    let specs = self::specs(
        &methods,
        &[
            ("source", "req.getParameter(\"a\")", 0),
            ("sink", "executeQuery(q)|q", 0),
        ],
    );
    assert!(solve(&methods, &specs).flows.is_empty());
}

#[test]
fn a_dispatch_takes_the_union_of_its_targets_as_a_guess() {
    let src = "class A {\n\
      void f(HttpServletRequest req, Statement st, Thing thing) {\n\
        String p = req.getParameter(\"a\");\n\
        String q = thing.work(p);\n\
        st.executeQuery(q);\n\
      }\n\
      String one(String i) { return \"fixed\"; }\n\
      String two(String i) { String r = i; return r; }\n\
    }";
    let mut methods = methods(src);
    let f = methods.id("f");
    let one = methods.id("one");
    let two = methods.id("two");
    let op = methods.functions[f as usize]
        .ir
        .body
        .iter()
        .position(|op| matches!(op, IrOp::Call { callee, .. } if callee == "thing.work"))
        .expect("the call");
    methods.functions[f as usize].calls.insert(
        op,
        CallTargets {
            targets: vec![one, two],
            in_project: true,
            guessed: true,
            ..CallTargets::default()
        },
    );
    let specs = specs(
        &methods,
        &[
            ("source", "req.getParameter(\"a\")", 0),
            ("sink", "executeQuery(q)|q", 0),
        ],
    );
    let solution = solve(&methods, &specs);
    assert_eq!(solution.flows.len(), 1);
    assert!(solution.guessed.contains(&0), "flagged as a guess");
    // With only the constant implementation, nothing flows.
    methods.functions[f as usize].calls.insert(
        op,
        CallTargets {
            targets: vec![one],
            in_project: true,
            guessed: true,
            ..CallTargets::default()
        },
    );
    assert!(solve(&methods, &specs).flows.is_empty());
}

#[test]
fn fields_carry_taint_between_methods_per_call() {
    let src = "class A {\n\
      private String dataBad;\n\
      public static String data;\n\
      void bad(HttpServletRequest req) {\n\
        dataBad = req.getParameter(\"a\");\n\
        badSink();\n\
      }\n\
      void badSink() {\n\
        Runtime.getRuntime().exec(this.dataBad);\n\
      }\n\
      void good(HttpServletRequest req) {\n\
        data = \"foo\";\n\
        goodSink();\n\
      }\n\
      void bad2(HttpServletRequest req) {\n\
        A.data = req.getParameter(\"b\");\n\
        goodSink();\n\
      }\n\
      void goodSink() {\n\
        String d = A.data;\n\
        Runtime.getRuntime().exec(d);\n\
      }\n\
    }";
    let methods = methods(src);
    let specs = specs(
        &methods,
        &[
            ("source", "req.getParameter(\"a\")", 0),
            ("source", "req.getParameter(\"b\")", 0),
            ("sink", "this.dataBad", 0),
            ("sink", "exec(d)|d", 0),
        ],
    );
    let solution = solve(&methods, &specs);
    let mut found: Vec<(String, u32)> = solution
        .flows
        .iter()
        .map(|flow| {
            let source = flow.path[0];
            (
                methods.names[flow.sink.func as usize].clone(),
                source.span.line,
            )
        })
        .collect();
    found.sort();
    assert_eq!(
        found,
        vec![("badSink".to_string(), 5), ("goodSink".to_string(), 16)],
        "each sink once, from the caller that stored the input; good() stores a constant"
    );
    let _ = methods.line_of(0);
}

#[test]
fn a_callee_writes_into_the_caller_object() {
    let src = "class A {\n\
      void f(HttpServletRequest req, Statement st) {\n\
        StringBuilder sb = new StringBuilder();\n\
        fill(sb, req);\n\
        st.executeQuery(sb.toString());\n\
      }\n\
      void fill(StringBuilder out, HttpServletRequest r) {\n\
        out.append(r.getParameter(\"a\"));\n\
      }\n\
    }";
    let methods = methods(src);
    let specs = specs(
        &methods,
        &[
            ("source", "r.getParameter(\"a\")", 0),
            ("sink", "sb.toString()", 0),
        ],
    );
    let solution = solve(&methods, &specs);
    assert_eq!(solution.flows.len(), 1, "{:?}", solution.flows);
}

#[test]
fn a_guard_on_the_path_cleans_the_value() {
    let src = "class A {\n\
      void f(HttpServletRequest req) {\n\
        int d = Integer.parseInt(req.getParameter(\"a\"));\n\
        if (d < Integer.MAX_VALUE) {\n\
          IO.writeLine(d + 1);\n\
        }\n\
        IO.writeLine(d + 2);\n\
        if (!(d < Integer.MAX_VALUE)) {\n\
          IO.writeLine(d + 3);\n\
        }\n\
      }\n\
    }";
    let methods = methods(src);
    let mut specs = specs(
        &methods,
        &[
            ("source", "req.getParameter(\"a\")", 0),
            ("sink", "d + 1", 0),
            ("sink", "d + 2", 0),
            ("sink", "d + 3", 0),
        ],
    );
    let unguarded = solve(&methods, &specs);
    assert_eq!(unguarded.flows.len(), 3);
    let (func, check) = methods.mark("d < Integer.MAX_VALUE", 0);
    let (_, value) = methods.mark("d < Integer.MAX_VALUE|d", 0);
    let (_, negated_check) = methods.mark("d < Integer.MAX_VALUE", 1);
    let (_, negated_value) = methods.mark("d < Integer.MAX_VALUE|d", 1);
    let spec = specs.get_mut(&func).expect("f");
    spec.guards.push(GuardMark {
        check,
        value,
        safe_when_true: true,
    });
    spec.guards.push(GuardMark {
        check: negated_check,
        value: negated_value,
        safe_when_true: true,
    });
    let guarded = solve(&methods, &specs);
    let mut sinks: Vec<usize> = guarded.flows.iter().map(|flow| flow.sink.index).collect();
    sinks.sort();
    assert_eq!(
        sinks,
        vec![1, 2],
        "only the guarded `d + 1` is clean: `d + 2` is after the if, `d + 3` on the failing side"
    );
}

#[test]
fn a_list_read_at_a_known_position_is_that_element() {
    let src = "class A {\n\
      void f(HttpServletRequest req, Statement st) {\n\
        String param = req.getParameter(\"a\");\n\
        String bar = \"alsosafe\";\n\
        if (param != null) {\n\
          java.util.List<String> valuesList = new java.util.ArrayList<String>();\n\
          valuesList.add(\"safe\");\n\
          valuesList.add(param);\n\
          valuesList.add(\"moresafe\");\n\
          valuesList.remove(0);\n\
          bar = valuesList.get(1);\n\
        }\n\
        st.executeQuery(bar);\n\
      }\n\
    }";
    let methods = methods(src);
    let marks = [
        ("source", "req.getParameter(\"a\")", 0),
        ("sink", "executeQuery(bar)|bar", 0),
    ];
    assert!(solve(&methods, &specs(&methods, &marks)).flows.is_empty());
    let tainted = src.replace("valuesList.get(1)", "valuesList.get(0)");
    let methods = self::methods(&tainted);
    assert_eq!(solve(&methods, &specs(&methods, &marks)).flows.len(), 1);
}

#[test]
fn a_field_stored_by_one_method_reaches_a_method_nothing_calls() {
    // A constructor stores input in a field its destructor-like `close`
    // uses; nothing calls `close`, so it may run after the constructor.
    let src = "class A {\n\
      private String data;\n\
      A(HttpServletRequest req) {\n\
        data = req.getParameter(\"a\");\n\
      }\n\
      void close() {\n\
        Runtime.getRuntime().exec(data);\n\
      }\n\
      void store() {\n\
        data = \"x\";\n\
        use();\n\
      }\n\
      void use() {\n\
        Runtime.getRuntime().exec(data);\n\
      }\n\
    }";
    let methods = methods(src);
    let specs = specs(
        &methods,
        &[
            ("source", "req.getParameter(\"a\")", 0),
            ("sink", "exec(data)|data", 0),
            ("sink", "exec(data)|data", 1),
        ],
    );
    let solution = solve(&methods, &specs);
    let sinks: Vec<&str> = solution
        .flows
        .iter()
        .map(|flow| methods.names[flow.sink.func as usize].as_str())
        .collect();
    assert_eq!(
        sinks,
        vec!["close"],
        "`use` is only ever given what its caller stores"
    );
    assert!(solution.guessed.contains(&0), "the order is assumed");
}

#[test]
fn a_reference_parameter_assigned_assigns_the_callers_variable() {
    let src = "void badSource(int &data) {\n\
        data = rand();\n\
    }\n\
    void copySource(int data) {\n\
        data = rand();\n\
    }\n\
    void bad() {\n\
        int data = 0;\n\
        badSource(data);\n\
        sink(data + 1);\n\
    }\n\
    void good() {\n\
        int data = 0;\n\
        copySource(data);\n\
        sink(data + 2);\n\
    }\n";
    let methods = program("cpp", src);
    let specs = specs(
        &methods,
        &[
            ("source", "rand()", 0),
            ("source", "rand()", 1),
            ("sink", "data + 1", 0),
            ("sink", "data + 2", 0),
        ],
    );
    let solution = solve(&methods, &specs);
    let sinks: Vec<&str> = solution
        .flows
        .iter()
        .map(|flow| methods.names[flow.sink.func as usize].as_str())
        .collect();
    assert_eq!(sinks, vec!["bad"], "a by-value parameter is a copy");
}

#[test]
fn a_spent_budget_stops_and_says_so() {
    let methods = methods(PARAM);
    let specs = specs(
        &methods,
        &[
            ("source", "req.getParameter(\"a\")", 0),
            ("sink", "executeQuery(q)|q", 0),
        ],
    );
    let mut solver = Solver::new(&methods.functions);
    let solution = solver.run(&specs, &mut Budget::new(3, None));
    assert!(solution.partial);
    let solution = solver.run(&specs, &mut Budget::unlimited());
    assert!(!solution.partial);
    assert_eq!(solution.flows.len(), 1);
}
