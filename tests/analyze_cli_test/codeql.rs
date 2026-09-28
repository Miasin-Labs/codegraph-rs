/// The CodeQL launcher the adapter would find (CODEGRAPH_CODEQL, PATH, or
/// the tools cache), when one runs.
fn codeql_launcher() -> Option<std::path::PathBuf> {
    let mut candidates = Vec::new();
    if let Some(explicit) = std::env::var_os("CODEGRAPH_CODEQL").filter(|v| !v.is_empty()) {
        let path = std::path::PathBuf::from(explicit);
        candidates.push(path.join("codeql"));
        candidates.push(path.join("codeql").join("codeql"));
        candidates.push(path);
    } else {
        if let Some(path) = std::env::var_os("PATH") {
            candidates.extend(std::env::split_paths(&path).map(|dir| dir.join("codeql")));
        }
        if let Some(cache) = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|home| home.join(".cache/codegraph-tools/codeql/codeql/codeql"))
        {
            candidates.push(cache);
        }
    }
    candidates.into_iter().find(|candidate| {
        candidate.is_file()
            && Command::new(candidate)
                .args(["version", "--format=terse"])
                .stdin(Stdio::null())
                .output()
                .is_ok_and(|out| out.status.success())
    })
}

/// Run `codegraph analyze <args> --json` with `CODEGRAPH_CODEQL` set.
fn run_codeql_json(root: &std::path::Path, codeql: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let out = Command::new(bin())
        .arg("analyze")
        .args(args)
        .arg("--json")
        .current_dir(root)
        .env("CODEGRAPH_HOME", concat!(env!("CARGO_TARGET_TMPDIR"), "/codegraph-home"))
        .env("CODEGRAPH_NO_DAEMON", "1")
        .env("CODEGRAPH_CODEQL", codeql)
        .stdin(Stdio::null())
        .output()
        .expect("spawn codegraph");
    assert!(out.status.success(), "{}", stderr_str(&out));
    let envelope: serde_json::Value = serde_json::from_str(stdout_str(&out).trim())
        .unwrap_or_else(|e| panic!("bad JSON ({e}): {}", stdout_str(&out)));
    envelope["data"].clone()
}

/// A servlet written for this test (so CodeQL runs on this repository's
/// own code): `doPost` builds SQL from a request parameter; `lookup` does
/// the same through a helper; `Tests.java` is test code.
fn write_codeql_fixture(root: &std::path::Path) {
    support::write(
        &root.join("src/main/java/demo/Search.java"),
        r#"package demo;

import java.io.IOException;
import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.ResultSet;
import java.sql.SQLException;
import java.sql.Statement;
import javax.servlet.http.HttpServlet;
import javax.servlet.http.HttpServletRequest;
import javax.servlet.http.HttpServletResponse;

public class Search extends HttpServlet {
    @Override
    protected void doPost(HttpServletRequest request, HttpServletResponse response)
            throws IOException {
        String name = request.getParameter("name");
        String sql = "SELECT * FROM users WHERE name = '" + name + "'";
        try {
            Connection connection = DriverManager.getConnection("jdbc:h2:mem:");
            Statement statement = connection.createStatement();
            ResultSet rows = statement.executeQuery(sql);
            response.getWriter().println(rows.next());
        } catch (SQLException e) {
            throw new IOException(e);
        }
    }
}
"#,
    );
}

#[test]
fn analyze_codeql_without_the_cli_says_how_to_get_it_and_the_license() {
    let (_dir, root) = temp_project();
    write_codeql_fixture(&root);
    let out = run_cli(&root, &["init"]);
    assert!(out.status.success(), "init failed: {}", stderr_str(&out));
    let missing = root.join("no-such-codeql");

    let report = run_codeql_json(&root, &missing, &["codeql"]);
    let status = &report["codeql"];
    assert_eq!(status["state"], "unavailable", "{report}");
    let failure = status["failure"].as_str().unwrap();
    assert!(failure.contains("CODEGRAPH_CODEQL"), "{failure}");
    assert!(
        status["license"].as_str().unwrap().contains("open-source code"),
        "{status}"
    );
    assert_eq!(report["findings"], serde_json::json!([]));

    // `analyze bugs --detector codeql` reports the same, and writes an
    // empty but valid SARIF log.
    let sarif = root.join("out.sarif");
    let bugs = run_codeql_json(
        &root,
        &missing,
        &["bugs", "--detector", "codeql", "--sarif", sarif.to_str().unwrap()],
    );
    assert_eq!(bugs["codeql"]["state"], "unavailable", "{bugs}");
    let log: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sarif).unwrap()).unwrap();
    assert_eq!(log["version"], "2.1.0");
    assert_eq!(log["runs"][0]["tool"]["driver"]["name"], "codegraph");
    assert_eq!(log["runs"][0]["results"], serde_json::json!([]));
    assert!(!root.join(".codegraph/codeql/run").exists(), "nothing started");
}

#[test]
fn analyze_codeql_maps_a_sql_injection_onto_the_graph() {
    let Some(codeql) = codeql_launcher() else {
        eprintln!("codeql not installed; skipped");
        return;
    };
    let (_dir, root) = temp_project();
    write_codeql_fixture(&root);
    let out = run_cli(&root, &["init"]);
    assert!(out.status.success(), "init failed: {}", stderr_str(&out));
    let suite = "codeql/java-queries:Security/CWE/CWE-089/SqlTainted.ql";
    let args = ["codeql", "--language", "java", "--suite", suite, "--wait", "900"];

    let report = run_codeql_json(&root, &codeql, &args);
    let status = &report["codeql"];
    assert_eq!(status["state"], "complete", "{report}");
    let java = &status["languages"][0];
    assert_eq!(java["language"], "java");
    assert_eq!(java["buildMode"], "none");
    assert_eq!(java["cached"], false);
    assert!(java["databaseBytes"].as_u64().unwrap() > 0, "{java}");

    let findings = report["findings"].as_array().unwrap();
    let sqli = findings
        .iter()
        .find(|f| f["rule"] == "codeql::java/sql-injection")
        .unwrap_or_else(|| panic!("no SQL injection in {report}"));
    assert_eq!(sqli["detector"], "codeql");
    assert_eq!(sqli["file"], "src/main/java/demo/Search.java");
    assert_eq!(sqli["line"], 22);
    assert!(
        sqli["function"].as_str().unwrap().ends_with("doPost"),
        "{sqli}"
    );
    let notes: Vec<&str> = sqli["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["note"].as_str().unwrap())
        .collect();
    assert!(notes[0].starts_with("source 1/"), "{notes:?}");
    assert!(
        sqli["message"]
            .as_str()
            .unwrap()
            .contains("reachable from request handler"),
        "{sqli}"
    );

    // Nothing changed: answered from the cache, same findings.
    let again = run_codeql_json(&root, &codeql, &args);
    assert_eq!(again["codeql"]["languages"][0]["cached"], true, "{again}");
    assert_eq!(again["findings"], report["findings"]);

    // With the built-in rules, codegraph's own SQL taint rule agrees at the
    // same place and CWE: one finding, CodeQL as its evidence.
    let mut merged_args = args.to_vec();
    merged_args.push("--builtin");
    let merged = run_codeql_json(&root, &codeql, &merged_args);
    let at_sink: Vec<&serde_json::Value> = merged["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["file"] == "src/main/java/demo/Search.java" && f["line"] == 22)
        .collect();
    let corroborated = at_sink.iter().find(|f| {
        f["evidence"][0]["note"]
            .as_str()
            .is_some_and(|note| note.starts_with("also reported by"))
    });
    assert!(
        corroborated.is_some(),
        "the rule and CodeQL findings merge: {at_sink:#?}"
    );

    // SARIF out carries CodeQL's rule metadata.
    let sarif = root.join("codeql.sarif");
    let _ = run_codeql_json(
        &root,
        &codeql,
        &[&args[..], &["--sarif", sarif.to_str().unwrap()]].concat(),
    );
    let log: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&sarif).unwrap()).unwrap();
    let rules = log["runs"][0]["tool"]["driver"]["rules"].as_array().unwrap();
    let rule = rules
        .iter()
        .find(|r| r["id"] == "codeql::java/sql-injection")
        .unwrap();
    assert_eq!(rule["properties"]["precision"], "high", "{rule}");
}
