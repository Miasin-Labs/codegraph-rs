use std::time::Duration;

use codegraph::analyze::bugs::{BugsOptions, Detector};
use codegraph::analyze::codeql::{CodeqlOptions, CodeqlState, codeql_report};

use super::bugs::{
    RuleSources,
    findings_limit,
    load_rules,
    print_codeql_status,
    print_findings,
    write_sarif,
};
use super::{
    CodeGraph,
    OpenOptions,
    error_msg,
    is_initialized,
    parse_int_js,
    print_report_json,
    process,
    resolve_project_path,
};

/// `codegraph analyze codeql` arguments, as clap parsed them.
pub(crate) struct CodeqlArgs {
    pub languages: Vec<String>,
    pub suites: Vec<String>,
    pub wait: String,
    pub rules: RuleSources,
    pub tests: bool,
    pub top: Option<String>,
    pub sarif: Option<String>,
    pub path: Option<String>,
    pub json: bool,
}

/// codegraph analyze codeql [--language L]… [--suite S]… [--wait S] [--builtin] [--sarif F]
pub(crate) fn cmd_analyze_codeql(args: CodeqlArgs) {
    let project_path = resolve_project_path(args.path.as_deref());
    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            return Err(format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
        }
        let wait = parse_int_js(&args.wait)
            .filter(|&secs| secs >= 0)
            .ok_or_else(|| format!("--wait takes a whole number, not `{}`", args.wait))?;
        let options = BugsOptions {
            detectors: vec![Detector::CodeQL],
            include_tests: args.tests,
            codeql: Some(CodeqlOptions {
                languages: args.languages.clone(),
                suites: args.suites.clone(),
                wait: Duration::from_secs(wait as u64),
            }),
            ..BugsOptions::default()
        };
        let rules = if args.rules.paths.is_empty() && !args.rules.builtin {
            None
        } else {
            Some(load_rules(&args.rules, None, true)?)
        };
        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let report = codeql_report(&cg, &project_path, &options, rules.as_ref());
        cg.close();
        let mut report = report?;
        let unavailable = report
            .codeql
            .as_ref()
            .is_some_and(|status| status.state == CodeqlState::Unavailable);
        if write_sarif(&report, &project_path, args.sarif.as_deref())? {
            return Ok(());
        }
        if let Some(top) = findings_limit(args.top.as_deref(), args.json) {
            report.limit(top);
        }
        if args.json {
            return print_report_json("codeql", &report);
        }
        print_codeql_status(report.codeql.as_ref());
        if unavailable && report.findings.is_empty() {
            return Ok(());
        }
        print_findings(&report, None, "CodeQL findings");
        Ok(())
    };
    if let Err(msg) = body() {
        error_msg(&format!("analyze codeql failed: {msg}"));
        process::exit(1);
    }
}
