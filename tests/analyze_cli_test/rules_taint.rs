/// A servlet with one real SQL injection and three look-alikes the taint
/// analysis must see through: a constant map key, a dead branch, and a
/// parameterized query.
const TAINTED_SERVLET: &str = r#"package app;

public class Servlet {
    public void doPost(HttpServletRequest request, java.sql.Statement st) throws Exception {
        String param = request.getParameter("id");
        String sql = "SELECT * FROM users WHERE id='" + param + "'";
        st.executeQuery(sql);
    }

    public void mapKeys(HttpServletRequest request, java.sql.Statement st) throws Exception {
        String param = request.getParameter("id");
        java.util.HashMap<String, Object> map = new java.util.HashMap<String, Object>();
        map.put("keyA", "a_Value");
        map.put("keyB", param);
        String bar = (String) map.get("keyA");
        st.executeQuery("SELECT * FROM users WHERE id='" + bar + "'");
    }

    public void deadBranch(HttpServletRequest request, java.sql.Statement st) throws Exception {
        String param = request.getParameter("id");
        String bar;
        int num = 86;
        if ((7 * 42) - num > 200) bar = "This_should_always_happen";
        else bar = param;
        st.executeQuery("SELECT * FROM users WHERE id='" + bar + "'");
    }

    public void prepared(HttpServletRequest request, java.sql.Connection c) throws Exception {
        String param = request.getParameter("id");
        java.sql.PreparedStatement ps = c.prepareStatement("SELECT * FROM users WHERE id=?");
        ps.setString(1, param);
        ps.executeQuery();
    }
}
"#;

#[test]
fn analyze_rules_taint_follows_data_through_the_function() {
    let (_dir, root) = temp_project();
    support::write(&root.join("src/app/Servlet.java"), TAINTED_SERVLET);
    init_fixture_files_only(&root);

    let report = run_analyze_json(&root, &["rules", "--builtin", "--tests"]);
    let findings: Vec<&serde_json::Value> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == "java-sql-injection")
        .collect();
    assert_eq!(findings.len(), 1, "{report}");
    let finding = findings[0];
    assert_eq!(finding["file"], "src/app/Servlet.java");
    assert_eq!(finding["line"], 7);
    assert_eq!(
        finding["message"],
        "untrusted `request.getParameter(\"id\")` reaches the SQL of executeQuery() [jdbc]"
    );
    // Evidence: the source, each step, the sink.
    let evidence: Vec<(u64, String)> = finding["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["line"].as_u64().unwrap(),
                e["note"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(evidence[0].0, 5, "{evidence:?}");
    assert!(evidence[0].1.starts_with("source: `request.getParameter(\"id\")`"), "{evidence:?}");
    assert_eq!(evidence[1].0, 6, "{evidence:?}");
    assert!(evidence[1].1.starts_with("flows through `String sql ="), "{evidence:?}");
    assert_eq!(evidence.last().unwrap().0, 7, "{evidence:?}");
    assert!(evidence.last().unwrap().1.starts_with("sink: `st.executeQuery(sql);`"), "{evidence:?}");

    // The same rule checks its examples without the index.
    let out = run_cli(&root, &["analyze", "rules", "--check", "--builtin"]);
    assert!(out.status.success(), "{}", stderr_str(&out));
    assert!(stdout_str(&out).contains("java-sql-injection"), "{}", stdout_str(&out));
}

#[test]
fn analyze_rules_taint_check_explains_a_missing_flow() {
    let (_dir, root) = temp_project();
    let rule = r#"
id: env-to-exec
language: java
taint:
  sources:
    - query: '((method_invocation name: (identifier) @m) @src (#eq? @m "getenv"))'
      value: src
  sinks:
    - query: '((method_invocation name: (identifier) @m arguments: (argument_list (_) @arg)) (#eq? @m "exec"))'
      argument: arg
examples:
  bad:
    - "class A { void f(Runtime r) { String v = \"ls\"; r.exec(v); } }"
"#;
    support::write(&root.join("rules/env.yaml"), rule);
    let out = run_cli(&root, &["analyze", "rules", "--check", "rules/env.yaml"]);
    assert!(!out.status.success());
    let text = stdout_str(&out);
    assert!(text.contains("bad[0] did not match"), "{text}");
    assert!(text.contains("no source matched in `f`"), "{text}");

    // A misplaced role key is a located load error.
    support::write(
        &root.join("rules/bad.yaml"),
        "id: x\nlanguage: java\ntaint:\n  sources:\n    - query: \"(identifier) @v\"\n      argument: v\n  sinks:\n    - query: \"(identifier) @v\"\n      argument: v\nexamples:\n  bad: [\"class A {}\"]\n",
    );
    let out = run_cli(&root, &["analyze", "rules", "--check", "rules/bad.yaml"]);
    assert!(!out.status.success());
    let text = stdout_str(&out);
    assert!(
        text.contains("rules/bad.yaml:6: rule `x`: taint.sources[0]: `argument` does not apply to sources"),
        "{text}"
    );
}

/// The servlet: its input goes to another file's method, which runs the
/// SQL, and to one that ignores it; a name comes back from a helper.
const WEB: &str = r#"package app;

public class Web {
    public void doPost(HttpServletRequest request, java.sql.Connection conn) throws Exception {
        String id = request.getParameter("id");
        Dao dao = new Dao();
        dao.find(conn, id);
        dao.findSafe(conn, id);
        String name = Params.name(request);
        conn.createStatement().execute("DELETE FROM t WHERE name='" + name + "'");
    }
}
"#;

const DAO: &str = r#"package app;

public class Dao {
    public void find(java.sql.Connection conn, String id) throws Exception {
        String sql = "SELECT * FROM users WHERE id='" + id + "'";
        conn.createStatement().executeQuery(sql);
    }

    public void findSafe(java.sql.Connection conn, String id) throws Exception {
        String sql = "SELECT * FROM users WHERE id='42'";
        conn.createStatement().executeQuery(sql);
    }
}

class Params {
    static String name(HttpServletRequest request) {
        return request.getParameter("name");
    }
}
"#;

#[test]
fn analyze_rules_taint_follows_data_across_methods_and_files() {
    let (_dir, root) = temp_project();
    support::write(&root.join("src/app/Web.java"), WEB);
    support::write(&root.join("src/app/Dao.java"), DAO);
    init_fixture_files_only(&root);

    let report = run_analyze_json(&root, &["rules", "--builtin", "--tests"]);
    let mut findings: Vec<&serde_json::Value> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == "java-sql-injection")
        .collect();
    findings.sort_by_key(|f| (f["file"].as_str().unwrap().to_string(), f["line"].as_u64()));
    let places: Vec<(&str, u64)> = findings
        .iter()
        .map(|f| (f["file"].as_str().unwrap(), f["line"].as_u64().unwrap()))
        .collect();
    // The sink `find` runs, and the sink the helper's source reaches; not
    // `findSafe`, which ignores its input.
    assert_eq!(
        places,
        vec![("src/app/Dao.java", 6), ("src/app/Web.java", 10)],
        "{report}"
    );
    let evidence = |finding: &serde_json::Value| -> Vec<(String, u64, String)> {
        finding["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["file"].as_str().unwrap().to_string(),
                    e["line"].as_u64().unwrap(),
                    e["note"].as_str().unwrap().to_string(),
                )
            })
            .collect()
    };
    // Source in Web.java, the call, the callee's steps, the sink in Dao.java.
    let into_dao = evidence(findings[0]);
    assert_eq!(
        (into_dao[0].0.as_str(), into_dao[0].1),
        ("src/app/Web.java", 5),
        "{into_dao:?}"
    );
    assert!(into_dao[0].2.starts_with("source: `request.getParameter(\"id\")`"), "{into_dao:?}");
    assert!(
        into_dao.iter().any(|(file, line, note)| file == "src/app/Web.java"
            && *line == 7
            && note.starts_with("flows through `dao.find(conn, id);`")),
        "{into_dao:?}"
    );
    assert!(
        into_dao.iter().any(|(file, line, _)| file == "src/app/Dao.java" && *line == 5),
        "{into_dao:?}"
    );
    let last = into_dao.last().unwrap();
    assert_eq!((last.0.as_str(), last.1), ("src/app/Dao.java", 6), "{into_dao:?}");
    assert!(last.2.starts_with("sink: `conn.createStatement().executeQuery(sql);`"), "{into_dao:?}");
    // The helper's source: from Dao.java back into Web.java.
    let from_helper = evidence(findings[1]);
    assert_eq!(
        (from_helper[0].0.as_str(), from_helper[0].1),
        ("src/app/Dao.java", 17),
        "{from_helper:?}"
    );
    assert!(
        from_helper[0].2.starts_with("source: `request.getParameter(\"name\")`"),
        "{from_helper:?}"
    );
    assert!(
        from_helper
            .iter()
            .any(|(file, line, _)| file == "src/app/Web.java" && *line == 9),
        "{from_helper:?}"
    );
}
