use std::io::Read;
use std::path::PathBuf;

use codegraph::analyze::bugs::{
    BugsOptions,
    BugsReport,
    Detector,
    ReviewPacket,
    ReviewSelection,
    bugs_report,
    bugs_review,
};
use codegraph::analyze::rules::{
    CheckReport,
    RuleSet,
    ScoreOptions,
    ScoreReport,
    check_rules,
    collect_rule_texts,
    rules_report,
    saved_rule_texts,
    score_rules,
};

use super::{
    CodeGraph,
    OpenOptions,
    analysis_reports,
    bold,
    cyan,
    dim,
    error_msg,
    format_number,
    green,
    info,
    is_initialized,
    parse_int_js,
    print_report_json,
    process,
    resolve_project_path,
    white,
    yellow,
};

/// codegraph analyze bugs [--base REV] [--detector deviance|lint] [--tests]
pub(crate) fn cmd_analyze_bugs(
    base: Option<&str>,
    detectors: &[String],
    include_tests: bool,
    top_arg: Option<&str>,
    path_arg: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            return Err(format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
        }
        let options = bugs_options(&project_path, base, detectors, include_tests, false)?;
        let top = findings_limit(top_arg, json);

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let report = bugs_report(&cg, &project_path, &options);
        cg.close();
        let mut report = report?;
        if let Some(top) = top {
            report.limit(top);
        }

        if json {
            return print_report_json("bugs", &report);
        }
        print_findings(&report, base, "Suspected bugs");
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze bugs failed: {msg}"));
        process::exit(1);
    }
}

/// How many findings `analyze bugs|rules` report: `--top` when given; else
/// every finding for `--json` (a consumer filters them itself) and the 50
/// most confident for a person reading them.
fn findings_limit(top_arg: Option<&str>, json: bool) -> Option<usize> {
    match top_arg {
        Some(arg) => Some(parse_int_js(arg).unwrap_or(50).max(1) as usize),
        None if json => None,
        None => Some(50),
    }
}

fn bugs_options(
    project_path: &std::path::Path,
    base: Option<&str>,
    detectors: &[String],
    include_tests: bool,
    allow_rule: bool,
) -> Result<BugsOptions, String> {
    let detectors = detectors
        .iter()
        .map(|name| match name.as_str() {
            "deviance" => Ok(Detector::Deviance),
            "lint" => Ok(Detector::Lint),
            "rule" if allow_rule => Ok(Detector::Rule),
            "rule" => Err(
                "rule findings come from `codegraph analyze rules` (or `analyze review --rules`)"
                    .to_string(),
            ),
            other => Err(format!(
                "unknown detector \"{other}\" — known: deviance, lint{}",
                if allow_rule { ", rule" } else { "" }
            )),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let only_files = base
        .map(|base| analysis_reports::changed_files(project_path, base))
        .transpose()?;
    Ok(BugsOptions {
        detectors,
        only_files,
        include_tests,
        taint_budget: None,
        dependency_summaries: cli_dependency_summaries(),
    })
}

/// `FILE` or `FILE:LINE`.
fn parse_at(at: &str) -> (String, Option<u32>) {
    match at.rsplit_once(':') {
        Some((file, line)) if line.parse::<u32>().is_ok() => (file.to_string(), line.parse().ok()),
        _ => (at.to_string(), None),
    }
}

/// codegraph analyze review [--at FILE[:LINE]] [--rule R] [--base REV] [--rules P]
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_analyze_review(
    at: Option<&str>,
    rule: Option<String>,
    base: Option<&str>,
    detectors: &[String],
    sources: &RuleSources,
    include_tests: bool,
    top_arg: &str,
    path_arg: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            return Err(format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
        }
        let options = bugs_options(&project_path, base, detectors, include_tests, true)?;
        let rules = if sources.is_empty() {
            None
        } else {
            Some(load_rules(sources, None, true)?)
        };
        let selection = ReviewSelection {
            rule,
            at: at.map(parse_at),
            top: parse_int_js(top_arg).unwrap_or(10).max(1) as usize,
        };

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let packets = bugs_review(&cg, &project_path, &options, &selection, rules.as_ref());
        cg.close();
        let packets = packets?;

        if json {
            return print_report_json("review", &packets);
        }
        if packets.is_empty() {
            info("No findings to review");
            return Ok(());
        }
        for (index, packet) in packets.iter().enumerate() {
            print_packet(index + 1, packets.len(), packet);
        }
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("analyze review failed: {msg}"));
        process::exit(1);
    }
}

fn print_packet(number: usize, total: usize, packet: &ReviewPacket) {
    let finding = &packet.finding;
    println!(
        "{}",
        bold(&format!(
            "\n── Review {number}/{total}: {} at {}:{} ({:.0}%)",
            finding.rule,
            finding.file,
            finding.line,
            finding.confidence * 100.0
        ))
    );
    if let Some(function) = &finding.function {
        println!("{}", dim(&format!("in {function}")));
    }
    println!("{}\n", finding.message);
    if let Some(snippet) = &packet.function {
        println!(
            "{}",
            white(&format!(
                "{}:{}-{}",
                snippet.file, snippet.start_line, snippet.end_line
            ))
        );
        println!("{}\n", snippet.source);
    }
    for snippet in &packet.evidence {
        println!(
            "{} {}",
            cyan(&format!("{}:{}", snippet.file, snippet.start_line)),
            dim(snippet.note.as_deref().unwrap_or(""))
        );
        println!("{}\n", snippet.source);
    }
    if !packet.callers.is_empty() {
        println!("{}", white("Called from:"));
        for caller in &packet.callers {
            println!(
                "  {} {}",
                caller.name,
                dim(&format!("{}:{}", caller.file, caller.line))
            );
        }
        println!();
    }
    println!("{}", yellow("Decide:"));
    for question in &packet.checklist {
        println!("  {} {question}", green("□"));
    }
}

/// Where `analyze rules`/`review` read rules from.
pub(crate) struct RuleSources {
    pub paths: Vec<String>,
    /// Inline YAML; `-` is stdin.
    pub texts: Vec<String>,
    pub builtin: bool,
    /// Also the project's saved rules (`.codegraph/rules/*.yaml`).
    pub saved: bool,
}

impl RuleSources {
    fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.texts.is_empty() && !self.builtin
    }
}

/// `--score` settings.
pub(crate) struct ScoreArgs {
    pub corpus: String,
    pub sample: Option<String>,
    pub seed: String,
    pub slack: Option<String>,
    pub work: Option<String>,
    pub jobs: String,
    pub deadline: Option<String>,
    pub cursor: Option<String>,
}

/// The `(label, yaml)` texts `sources` name (stdin read once), without the
/// saved rules.
fn source_texts(sources: &RuleSources) -> Result<Vec<(String, String)>, String> {
    let mut texts = Vec::new();
    for (index, text) in sources.texts.iter().enumerate() {
        if text == "-" {
            let mut stdin = String::new();
            std::io::stdin()
                .read_to_string(&mut stdin)
                .map_err(|e| format!("cannot read rules from stdin: {e}"))?;
            texts.push(("<stdin>".to_string(), stdin));
        } else {
            texts.push((format!("<rule-text {}>", index + 1), text.clone()));
        }
    }
    let paths: Vec<PathBuf> = sources.paths.iter().map(PathBuf::from).collect();
    let (texts, errors) = collect_rule_texts(&paths, &texts, sources.builtin);
    if let Some(error) = errors.first() {
        return Err(error.to_string());
    }
    Ok(texts)
}

/// The saved rules of the project at `root`, when `sources` asks for them.
fn saved_texts(sources: &RuleSources, root: Option<&std::path::Path>) -> Vec<(String, String)> {
    match root {
        Some(root) if sources.saved => saved_rule_texts(root),
        _ => Vec::new(),
    }
}

/// The rules `sources` name, plus the saved rules of the project at
/// `root` (each shadowed by a named rule of its id). With `strict`, any
/// rule that does not load is an error listing every problem.
fn load_rules(
    sources: &RuleSources,
    root: Option<&std::path::Path>,
    strict: bool,
) -> Result<RuleSet, String> {
    let saved = saved_texts(sources, root);
    if sources.is_empty() && saved.is_empty() {
        return Err(
            "no rules given: pass rule files or directories (--rules), --rule-text <yaml|->, \
             or --builtin (or save rules to .codegraph/rules/ with the MCP `rules` tool)"
                .to_string(),
        );
    }
    let mut rules = RuleSet::default();
    for (label, text) in source_texts(sources)? {
        rules.add_text(&label, &text);
    }
    rules.add_shadowed(&saved);
    if strict && !rules.errors.is_empty() {
        let list: Vec<String> = rules.errors.iter().map(|e| format!("  {e}")).collect();
        return Err(format!(
            "{} rule problem{} (fix them, or run `codegraph analyze rules --check`):\n{}",
            rules.errors.len(),
            if rules.errors.len() == 1 { "" } else { "s" },
            list.join("\n")
        ));
    }
    Ok(rules)
}

/// codegraph analyze rules [RULES]... [--rules P] [--rule-text Y|-] [--builtin] [--check]
/// [--score CORPUS]
#[allow(clippy::too_many_arguments)]
pub(crate) fn cmd_analyze_rules(
    sources: &RuleSources,
    check: bool,
    score: Option<ScoreArgs>,
    base: Option<&str>,
    include_tests: bool,
    top_arg: Option<&str>,
    path_arg: Option<&str>,
    json: bool,
) {
    let project_path = resolve_project_path(path_arg);
    let saved_root = is_initialized(&project_path).then_some(project_path.as_path());
    if check {
        let report = match load_rules(sources, saved_root, false) {
            Ok(rules) => check_rules(&rules),
            Err(msg) => {
                error_msg(&format!("analyze rules --check failed: {msg}"));
                process::exit(1);
            }
        };
        if json {
            if let Err(msg) = print_report_json("rules-check", &report) {
                error_msg(&msg);
                process::exit(1);
            }
        } else {
            print_check(&report);
        }
        if !report.ok() {
            process::exit(1);
        }
        return;
    }
    if let Some(score) = score {
        if let Err(msg) = cmd_score(sources, saved_root, &score, json) {
            error_msg(&format!("analyze rules --score failed: {msg}"));
            process::exit(1);
        }
        return;
    }

    let body = || -> Result<(), String> {
        let rules = load_rules(sources, saved_root, true)?;
        if !is_initialized(&project_path) {
            return Err(format!(
                "CodeGraph not initialized in {} (`--check` runs rule examples without an \
                 index)",
                project_path.display()
            ));
        }
        let only_files = base
            .map(|base| analysis_reports::changed_files(&project_path, base))
            .transpose()?;
        let options = BugsOptions {
            detectors: vec![Detector::Rule],
            only_files,
            include_tests,
            taint_budget: None,
            dependency_summaries: cli_dependency_summaries(),
        };
        let top = findings_limit(top_arg, json);
        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let report = rules_report(&cg, &project_path, &rules, &options);
        cg.close();
        let mut report = report?;
        if let Some(top) = top {
            report.limit(top);
        }
        if json {
            return print_report_json("rules", &report);
        }
        print_findings(&report, base, "Rule findings");
        Ok(())
    };
    if let Err(msg) = body() {
        error_msg(&format!("analyze rules failed: {msg}"));
        process::exit(1);
    }
}

fn positive(value: Option<&str>, flag: &str) -> Result<Option<u64>, String> {
    value
        .map(|v| {
            v.trim()
                .parse::<u64>()
                .map_err(|_| format!("{flag} takes a whole number, not `{v}`"))
        })
        .transpose()
}

/// `analyze rules --score`: stage, index and score; print per-rule verdicts.
fn cmd_score(
    sources: &RuleSources,
    saved_root: Option<&std::path::Path>,
    args: &ScoreArgs,
    json: bool,
) -> Result<(), String> {
    let texts = source_texts(sources)?;
    let saved = saved_texts(sources, saved_root);
    if texts.is_empty() && saved.is_empty() {
        return Err(
            "no rules to score: pass --rules, --rule-text or --builtin (or run in a project \
             with saved rules)"
                .to_string(),
        );
    }
    let mut options = ScoreOptions::new(PathBuf::from(&args.corpus));
    options.sample = positive(args.sample.as_deref(), "--sample")?.map(|n| n as usize);
    options.seed = positive(Some(&args.seed), "--seed")?.unwrap_or(1);
    options.slack = positive(args.slack.as_deref(), "--slack")?.map(|n| n as i64);
    options.jobs = positive(Some(&args.jobs), "--jobs")?.unwrap_or(4).max(1) as usize;
    options.deadline =
        positive(args.deadline.as_deref(), "--deadline")?.map(std::time::Duration::from_secs);
    options.work = args.work.as_ref().map(PathBuf::from);
    options.cursor = args.cursor.clone();
    let quiet = json;
    let report = score_rules(&texts, &saved, &options, &|line| {
        if !quiet {
            eprintln!("  {line}");
        }
    })?;
    if json {
        let mut value = serde_json::to_value(&report).map_err(|e| e.to_string())?;
        value["metrics"] = report.metrics.clone();
        return print_report_json("rules-score", &value);
    }
    print_score(&report);
    Ok(())
}

fn pct(value: Option<f64>) -> String {
    value.map_or("-".to_string(), |v| format!("{:.1}%", v * 100.0))
}

fn print_score(report: &ScoreReport) {
    println!(
        "{}",
        bold(&format!(
            "\nRule scores on {} ({} scoring, {} of {} units{})",
            report.corpus,
            report.scoring,
            report.units.done,
            report.units.total,
            if report.units.failed > 0 {
                format!(", {} failed", report.units.failed)
            } else {
                String::new()
            }
        ))
    );
    println!(
        "{}\n",
        dim(&format!(
            "base rate {} over {} positives; units staged in {}",
            pct(report.base_rate),
            report.positives,
            report.work
        ))
    );
    for rule in &report.rules {
        let verdict = match rule.verdict {
            codegraph::analyze::rules::score::Decision::Keep => green("KEEP   "),
            codegraph::analyze::rules::score::Decision::Discard => yellow("DISCARD"),
        };
        let counts = match (rule.fp, rule.off_fix) {
            (Some(fp), _) => format!(
                "{} findings: {} TP, {fp} FP, {} unlabeled",
                rule.findings,
                rule.tp,
                rule.unlabeled.unwrap_or(0)
            ),
            (None, off) => format!(
                "{} findings: {} TP, {} off the fix, {} not discriminating, {} background",
                rule.findings,
                rule.tp,
                off.unwrap_or(0),
                rule.not_discriminating.unwrap_or(0),
                rule.background.unwrap_or(0)
            ),
        };
        println!(
            "{verdict} {} {}",
            white(&rule.id),
            dim(&format!(
                "precision {}, recall {}",
                pct(rule.precision),
                pct(rule.recall)
            ))
        );
        println!("        {counts}");
        println!("        {}", rule.reason);
    }
    for (unit, status) in &report.failures {
        println!("{} {unit}: {status}", yellow("unit"));
    }
    if let Some(cursor) = &report.next_cursor {
        println!(
            "\n{}",
            yellow(&format!(
                "Incomplete ({} units pending): the verdicts are provisional. Resume with \
                 --cursor {cursor}",
                report.units.pending
            ))
        );
    }
    println!();
}

fn print_check(report: &CheckReport) {
    for error in &report.errors {
        println!("{} {error}", yellow("ERROR"));
    }
    for rule in &report.rules {
        let place = match rule.line {
            Some(line) => format!("{}:{line}", rule.source),
            None => rule.source.clone(),
        };
        if rule.passed {
            let bad = rule
                .examples
                .iter()
                .filter(|e| e.example.starts_with("bad"))
                .count();
            println!(
                "{} {} {}",
                green("PASS"),
                white(&rule.id),
                dim(&format!(
                    "({bad} bad, {} good; {place})",
                    rule.examples.len() - bad
                ))
            );
            continue;
        }
        println!("{} {} {}", yellow("FAIL"), white(&rule.id), dim(&place));
        if let Some(error) = &rule.error {
            println!("     {error}");
        }
        for example in rule.examples.iter().filter(|e| !e.passed) {
            let at = example
                .line
                .map(|line| format!("{}:{line}: ", rule.source))
                .unwrap_or_default();
            println!("     {at}{}", example.explain());
        }
    }
    println!(
        "\n{} passed, {} failed{}",
        report.passed,
        report.failed,
        if report.errors.is_empty() {
            String::new()
        } else {
            format!(", {} file error(s)", report.errors.len())
        }
    );
}

fn print_findings(report: &BugsReport, base: Option<&str>, title: &str) {
    if report.findings.is_empty() {
        info(&format!(
            "No findings across {} files and {} call sites{}",
            format_number(report.files_scanned as u64),
            format_number(report.call_sites as u64),
            base.map(|b| format!(" (changes since {b})"))
                .unwrap_or_default()
        ));
        return;
    }
    let shown = report.findings.len();
    println!(
        "{}",
        bold(&format!(
            "\n{title}: {} of {} ({} files, {} call sites)\n",
            shown,
            shown + report.findings_omitted,
            format_number(report.files_scanned as u64),
            format_number(report.call_sites as u64)
        ))
    );
    for (index, finding) in report.findings.iter().enumerate() {
        println!(
            "{:>3}. {} {} {}",
            index + 1,
            yellow(&finding.rule),
            white(&format!("{}:{}", finding.file, finding.line)),
            dim(&format!(
                "({:.0}%{})",
                finding.confidence * 100.0,
                finding
                    .function
                    .as_deref()
                    .map(|f| format!(", in {f}"))
                    .unwrap_or_default()
            ))
        );
        println!("     {}", finding.message);
        for evidence in finding.evidence.iter().take(4) {
            println!(
                "     {} {}",
                cyan(&format!("{}:{}", evidence.file, evidence.line)),
                dim(&evidence.note)
            );
        }
        if finding.evidence.len() > 4 {
            println!(
                "     {}",
                dim(&format!("… {} more", finding.evidence.len() - 4))
            );
        }
    }
    if report.findings_omitted > 0 {
        println!(
            "{}",
            dim(&format!(
                "\n  … {} more (raise --top)",
                report.findings_omitted
            ))
        );
    }
    println!();
    info(&report.note);
}

/// The CLI's taint follows calls into dependency shards through their
/// summaries, building a missing one on first need within
/// `CODEGRAPH_DEP_SUMMARIES_BUILD_MS` (default 60 s) in all.
fn cli_dependency_summaries() -> Option<codegraph::deps::summaries::compose::Access> {
    let budget = std::env::var("CODEGRAPH_DEP_SUMMARIES_BUILD_MS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(60_000);
    codegraph::deps::summaries::summaries_enabled().then_some(
        codegraph::deps::summaries::compose::Access::BuildMissing(
            std::time::Duration::from_millis(budget),
        ),
    )
}
