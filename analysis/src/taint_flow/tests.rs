use super::*;
use crate::ir::IrOp;
use crate::ir::lower_tests::lower;

/// A mark on `needle`, found inside the first occurrence of `context`.
fn mark(src: &str, context: &str, needle: &str) -> Mark {
    let base = src
        .find(context)
        .unwrap_or_else(|| panic!("no `{context}`"));
    let offset = context
        .find(needle)
        .unwrap_or_else(|| panic!("no `{needle}` in `{context}`"));
    Mark {
        start_byte: base + offset,
        end_byte: base + offset + needle.len(),
        kind: None,
    }
}

fn run(lang: &str, src: &str, spec: &TaintSpec) -> Vec<Flow> {
    let func = lower(lang, src);
    analyze(&func, lang, spec, &|_| CallResolution::default())
}

fn lines(src: &str, flow: &Flow) -> Vec<u32> {
    let _ = src;
    let mut lines: Vec<u32> = flow.hops.iter().map(|hop| hop.span.line).collect();
    lines.dedup();
    lines
}

const JAVA_SQL: &str = "class A { void f(HttpServletRequest req, Statement stmt) {\n\
    String p = req.getParameter(\"a\");\n\
    String sql = \"select \" + p;\n\
    stmt.executeQuery(sql);\n\
} }";

fn java_sql_spec(src: &str) -> TaintSpec {
    TaintSpec {
        sources: vec![mark(
            src,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(src, "executeQuery(sql)", "sql")],
        ..TaintSpec::default()
    }
}

#[test]
fn a_parameter_reaches_a_query_through_concatenation() {
    let flows = run("java", JAVA_SQL, &java_sql_spec(JAVA_SQL));
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].source, 0);
    assert_eq!(flows[0].sink, 0);
    // Source line, then the concatenation.
    assert_eq!(lines(JAVA_SQL, &flows[0]), vec![2, 3]);
}

#[test]
fn a_sanitizer_stops_the_flow() {
    let src = "class A { void f(HttpServletRequest req, Statement stmt) {\n\
        String p = req.getParameter(\"a\");\n\
        String s = esc(p);\n\
        stmt.executeQuery(s);\n\
    } }";
    let mut spec = TaintSpec {
        sources: vec![mark(
            src,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(src, "executeQuery(s)", "s")],
        ..TaintSpec::default()
    };
    assert_eq!(run("java", src, &spec).len(), 1, "unsanitized, it flows");
    spec.sanitizers = vec![mark(src, "esc(p)", "esc(p)")];
    assert!(run("java", src, &spec).is_empty());
}

#[test]
fn a_constant_key_keeps_map_entries_apart() {
    let src = "class A { void f(HttpServletRequest req, Runtime r) {\n\
        String param = req.getParameter(\"a\");\n\
        String bar = \"safe!\";\n\
        java.util.HashMap<String, Object> map = new java.util.HashMap<String, Object>();\n\
        map.put(\"keyA\", \"a_Value\");\n\
        map.put(\"keyB\", param);\n\
        bar = (String) map.get(\"keyB\");\n\
        bar = (String) map.get(\"keyA\");\n\
        r.exec(bar);\n\
    } }";
    let spec = TaintSpec {
        sources: vec![mark(
            src,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(src, "r.exec(bar)", "bar")],
        ..TaintSpec::default()
    };
    assert!(run("java", src, &spec).is_empty(), "keyA holds a constant");
    let tainted = src.replace(
        "bar = (String) map.get(\"keyA\");",
        "bar = (String) map.get(\"keyB\");",
    );
    let spec = TaintSpec {
        sources: vec![mark(
            &tainted,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(&tainted, "r.exec(bar)", "bar")],
        ..TaintSpec::default()
    };
    let flows = run("java", &tainted, &spec);
    assert_eq!(flows.len(), 1);
    assert_eq!(lines(&tainted, &flows[0]), vec![2, 6, 8]);
}

#[test]
fn a_dead_branch_carries_no_taint() {
    let src = "class A { void f(HttpServletRequest req, Statement stmt) {\n\
        String param = req.getParameter(\"a\");\n\
        String bar;\n\
        int num = 86;\n\
        if ((7 * 42) - num > 200) bar = \"This_should_always_happen\";\n\
        else bar = param;\n\
        stmt.execute(bar);\n\
    } }";
    let spec = TaintSpec {
        sources: vec![mark(
            src,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(src, "execute(bar)", "bar")],
        ..TaintSpec::default()
    };
    assert!(run("java", src, &spec).is_empty());
    let live = src.replace("> 200", "< 200");
    let spec = TaintSpec {
        sources: vec![mark(
            &live,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(&live, "execute(bar)", "bar")],
        ..TaintSpec::default()
    };
    assert_eq!(run("java", &live, &spec).len(), 1);
}

#[test]
fn string_builders_and_library_calls_propagate() {
    let src = "class A { void f(HttpServletRequest req, Statement stmt) {\n\
        String p = req.getParameter(\"a\");\n\
        StringBuilder sb = new StringBuilder();\n\
        sb.append(\"x\").append(p);\n\
        String q = String.valueOf(sb.toString()).trim();\n\
        stmt.executeQuery(q);\n\
    } }";
    let spec = TaintSpec {
        sources: vec![mark(
            src,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(src, "executeQuery(q)", "q")],
        ..TaintSpec::default()
    };
    let flows = run("java", src, &spec);
    assert_eq!(flows.len(), 1, "append(p) on a chained builder");
    // A constant argument makes nothing tainted, and a length is clean.
    let clean = "class A { void f(HttpServletRequest req, Statement stmt, Thing t) {\n\
        String p = req.getParameter(\"a\");\n\
        String bar = t.doSomething(\"const\");\n\
        int n = p.length();\n\
        stmt.executeQuery(bar + n);\n\
    } }";
    let spec = TaintSpec {
        sources: vec![mark(
            clean,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(clean, "executeQuery(bar + n)", "bar + n")],
        ..TaintSpec::default()
    };
    assert!(run("java", clean, &spec).is_empty());
}

#[test]
fn c_out_parameters_and_copies_carry_input() {
    let src = "void bad() {\n\
        char *data;\n\
        char buf[100] = \"ls \";\n\
        data = buf;\n\
        size_t n = strlen(data);\n\
        if (fgets(data + n, (int)(100 - n), stdin) != NULL) { n = strlen(data); }\n\
        char cmd[200];\n\
        strcpy(cmd, data);\n\
        system(cmd);\n\
    }";
    let spec = TaintSpec {
        // The buffer `fgets` fills.
        sources: vec![mark(src, "fgets(data + n", "data + n")],
        sinks: vec![mark(src, "system(cmd)", "cmd")],
        ..TaintSpec::default()
    };
    let flows = run("c", src, &spec);
    assert_eq!(flows.len(), 1);
    assert_eq!(lines(src, &flows[0]), vec![6, 8]);
}

#[test]
fn a_parameter_source_holds_from_entry() {
    let src = "void sink_it(char *data) {\n\
        char *copy = data;\n\
        printf(copy);\n\
    }";
    let spec = TaintSpec {
        sources: vec![mark(src, "(char *data)", "data")],
        sinks: vec![mark(src, "printf(copy)", "copy")],
        ..TaintSpec::default()
    };
    let flows = run("c", src, &spec);
    assert_eq!(flows.len(), 1);
    assert_eq!(flows[0].hops[0].op, None, "the flow starts at entry");
}

#[test]
fn rule_propagators_add_steps() {
    let src = "class A { void f(HttpServletRequest req, Statement stmt, Box b) {\n\
        String p = req.getParameter(\"a\");\n\
        b.store(p);\n\
        String q = b.load();\n\
        stmt.executeQuery(q);\n\
    } }";
    let mut spec = TaintSpec {
        sources: vec![mark(
            src,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(src, "executeQuery(q)", "q")],
        ..TaintSpec::default()
    };
    assert!(run("java", src, &spec).is_empty(), "store is not modelled");
    spec.propagators = vec![Propagator {
        from: mark(src, "b.store(p)", "p"),
        to: mark(src, "b.store(p)", "b"),
    }];
    assert_eq!(run("java", src, &spec).len(), 1);
}

#[test]
fn interpolated_strings_carry_taint() {
    let py = "def f(request, cur):\n    q = request.args.get('a')\n    s = f\"select {q}\"\n    cur.execute(s)\n";
    let spec = TaintSpec {
        sources: vec![mark(py, "request.args.get('a')", "request.args.get('a')")],
        sinks: vec![mark(py, "execute(s)", "s")],
        ..TaintSpec::default()
    };
    assert_eq!(run("python", py, &spec).len(), 1);

    let js = "function f(req, db) {\n  const q = req.query.a;\n  db.query(`select ${q}`);\n}";
    let spec = TaintSpec {
        sources: vec![mark(js, "req.query.a", "req.query.a")],
        sinks: vec![mark(js, "db.query(`select ${q}`)", "`select ${q}`")],
        ..TaintSpec::default()
    };
    assert_eq!(run("javascript", js, &spec).len(), 1);

    let php =
        "<?php function f() {\n  $q = $_GET['a'];\n  $s = \"select $q\";\n  mysql_query($s);\n}";
    let spec = TaintSpec {
        sources: vec![mark(php, "$_GET['a']", "$_GET['a']")],
        sinks: vec![mark(php, "mysql_query($s)", "$s")],
        ..TaintSpec::default()
    };
    assert_eq!(run("php", php, &spec).len(), 1);
}

#[test]
fn a_source_that_reads_storage_taints_what_is_below_it() {
    let py = "def f(cur):\n    q = request.args['a']\n    cur.execute(q)\n";
    let spec = TaintSpec {
        sources: vec![mark(py, "request.args['a']", "request.args")],
        sinks: vec![mark(py, "execute(q)", "q")],
        ..TaintSpec::default()
    };
    assert_eq!(run("python", py, &spec).len(), 1);
}

#[test]
fn a_source_below_a_parameter_or_in_a_receiver_holds() {
    let js =
        "function f(req, res) {\n  const id = req.query.id;\n  db.query(\"S '\" + id + \"'\");\n}";
    let spec = TaintSpec {
        sources: vec![mark(js, "req.query.id", "req.query")],
        sinks: vec![mark(
            js,
            "db.query(\"S '\" + id + \"'\")",
            "\"S '\" + id + \"'\"",
        )],
        ..TaintSpec::default()
    };
    assert_eq!(run("javascript", js, &spec).len(), 1);
    // The receiver of a call is not an argument written by it.
    let py = "def f(request, cur):\n    name = request.POST.get('name')\n    cur.execute(\"S '%s'\" % name)\n";
    let spec = TaintSpec {
        sources: vec![mark(py, "request.POST.get", "request.POST")],
        sinks: vec![mark(py, "execute(\"S '%s'\" % name)", "\"S '%s'\" % name")],
        ..TaintSpec::default()
    };
    assert_eq!(run("python", py, &spec).len(), 1);
}

#[test]
fn unreachable_sinks_are_not_reported() {
    let src = "class A { void f(HttpServletRequest req, Statement stmt) {\n\
        String p = req.getParameter(\"a\");\n\
        if (false) { stmt.executeQuery(p); }\n\
    } }";
    let spec = TaintSpec {
        sources: vec![mark(
            src,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(src, "executeQuery(p)", "p")],
        ..TaintSpec::default()
    };
    assert!(run("java", src, &spec).is_empty());
}

#[test]
fn resolved_names_select_library_models() {
    // `fetch` is unknown by name, but the index resolved it to a map read.
    let src = "class A { void f(HttpServletRequest req, Statement stmt, Store m) {\n\
        m.put(\"a\", req.getParameter(\"a\"));\n\
        String v = m.fetch(\"b\");\n\
        stmt.executeQuery(v);\n\
    } }";
    let spec = TaintSpec {
        sources: vec![mark(
            src,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(src, "executeQuery(v)", "v")],
        ..TaintSpec::default()
    };
    let func = lower("java", src);
    let default = analyze(&func, "java", &spec, &|_| CallResolution::default());
    assert_eq!(default.len(), 1, "an unknown call carries its receiver");
    let fetch = func
        .body
        .iter()
        .position(|op| matches!(op, IrOp::Call { callee, .. } if callee == "m.fetch"))
        .unwrap();
    let resolved = analyze(&func, "java", &spec, &|op| CallResolution {
        names: if op == fetch {
            vec!["java.util.Map::get".to_string()]
        } else {
            Vec::new()
        },
        in_project: false,
        summary: None,
    });
    assert!(resolved.is_empty(), "a keyed read of another key");
}

#[test]
fn project_calls_without_a_summary_carry_nothing() {
    let src = "class A { void f(HttpServletRequest req, Statement stmt) {\n\
        String p = req.getParameter(\"a\");\n\
        String bar = new Test().doSomething(req, p);\n\
        stmt.executeQuery(bar);\n\
    } }";
    let spec = TaintSpec {
        sources: vec![mark(
            src,
            "req.getParameter(\"a\")",
            "req.getParameter(\"a\")",
        )],
        sinks: vec![mark(src, "executeQuery(bar)", "bar")],
        ..TaintSpec::default()
    };
    let func = lower("java", src);
    assert_eq!(
        analyze(&func, "java", &spec, &|_| CallResolution::default()).len(),
        1,
        "a library call carries its arguments"
    );
    let project = analyze(&func, "java", &spec, &|op| CallResolution {
        names: Vec::new(),
        in_project: matches!(&func.body[op], IrOp::Call { callee, .. } if callee.ends_with("doSomething")),
        summary: None,
    });
    assert!(
        project.is_empty(),
        "project code without a summary carries nothing"
    );
}
