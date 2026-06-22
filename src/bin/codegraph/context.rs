use super::{
    AnalysisContextOptions,
    BridgeOptions,
    BuildContextOptions,
    CodeGraph,
    ContextFormat,
    OpenOptions,
    Path,
    TaskInput,
    bridge_project_with_options,
    context_analysis,
    error_msg,
    info,
    is_initialized,
    parse_int_js,
    print_json,
    process,
    resolve_project_path,
};

// =============================================================================
// context command
// =============================================================================

/// codegraph context <task> [--budget tokens] [--strategy classic|analysis] [--fields]
///
/// `classic` (default) is the existing `ContextBuilder` pipeline (FTS entry
/// points + graph expansion) — its output is unchanged. `analysis` routes
/// through the analysis engine's context modules over the bridged index:
/// dataflow-seeded entry points, retrieval-gated expansion, clustered
/// per-file source, rendered to markdown and trimmed to the token budget.
/// `--fields` (analysis only) bridges with field/property carrying so the
/// engine's partial-struct views render — same effect as
/// `CODEGRAPH_ANALYSIS_FIELDS=1`, scoped to this invocation.
pub(crate) fn cmd_context(
    task: &str,
    path_arg: Option<&str>,
    budget_arg: Option<&str>,
    strategy: &str,
    fields: bool,
    json: bool,
    verbose: bool,
) {
    let project_path = resolve_project_path(path_arg);

    let budget_tokens = match budget_arg {
        Some(raw) => match parse_int_js(raw) {
            Some(n) if n > 0 => Some(n as usize),
            _ => {
                error_msg(&format!(
                    "Invalid --budget \"{raw}\" — expected a positive token count"
                ));
                process::exit(1);
            }
        },
        None => None,
    };

    let body = || -> Result<(), String> {
        match strategy {
            "classic" => {
                if fields {
                    eprintln!(
                        "warning: --fields requires --strategy analysis (ignored for classic)"
                    );
                }
                cmd_context_classic(task, &project_path, budget_tokens, json, verbose)
            }
            "analysis" => {
                cmd_context_analysis(task, &project_path, budget_tokens, fields, json, verbose)
            }
            other => {
                error_msg(&format!(
                    "Invalid --strategy \"{other}\" — expected \"classic\" or \"analysis\""
                ));
                process::exit(1);
            }
        }
    };

    if let Err(msg) = body() {
        error_msg(&format!("Context build failed: {msg}"));
        process::exit(1);
    }
}

/// The pre-existing `ContextBuilder` path. Without `--budget` the printed
/// output is exactly what `CodeGraph::build_context` returns (regression-
/// pinned); `--budget` applies a plain output trim on top (markdown only —
/// trimming would corrupt the JSON shape).
pub(crate) fn cmd_context_classic(
    task: &str,
    project_path: &Path,
    budget_tokens: Option<usize>,
    json: bool,
    verbose: bool,
) -> Result<(), String> {
    if !is_initialized(project_path) {
        error_msg(&format!(
            "CodeGraph not initialized in {}",
            project_path.display()
        ));
        info("Run \"codegraph init\" first");
        process::exit(1);
    }

    let cg = CodeGraph::open(project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
    let options = BuildContextOptions {
        format: Some(if json {
            ContextFormat::Json
        } else {
            ContextFormat::Markdown
        }),
        ..Default::default()
    };
    let output = cg
        .build_context(&TaskInput::Text(task.to_string()), Some(&options))
        .map_err(|e| e.to_string())?;
    cg.close();

    let output = match (budget_tokens, json) {
        (Some(tokens), false) => {
            let (trimmed, truncated) = context_analysis::trim_to_token_budget(&output, tokens);
            if verbose && truncated {
                eprintln!(
                    "note: classic output trimmed to ~{tokens} tokens (use --strategy analysis \
                     for budget-aware selection)"
                );
            }
            trimmed
        }
        (Some(_), true) => {
            eprintln!("warning: --budget is ignored for classic JSON output");
            output
        }
        (None, _) => output,
    };
    println!("{output}");
    Ok(())
}

/// The analysis-engine path: bridge the index, run the engine's context
/// pipeline (`codegraph::context_analysis`), print markdown (or the full
/// JSON report). Capability notes go to stderr under `--verbose` and are
/// always present in the JSON report.
///
/// `fields` ORs into the environment's bridge options
/// (`CODEGRAPH_ANALYSIS_FIELDS=1`) — the flag can only ADD field carrying,
/// never strip it from an env-enabled run.
pub(crate) fn cmd_context_analysis(
    task: &str,
    project_path: &Path,
    budget_tokens: Option<usize>,
    fields: bool,
    json: bool,
    verbose: bool,
) -> Result<(), String> {
    let mut options = BridgeOptions::from_env();
    options.include_fields = options.include_fields || fields;
    let bridged = bridge_project_with_options(project_path, false, json, &options)?;
    let report = context_analysis::build_analysis_context(
        &bridged.graph,
        project_path,
        task,
        &AnalysisContextOptions {
            budget_tokens,
            ..Default::default()
        },
    );

    if verbose {
        for note in &report.notes {
            eprintln!("note: {note}");
        }
        eprintln!(
            "note: strategy=analysis seeding={} measured-tokens={}{}",
            match report.seeding {
                context_analysis::SeedingMode::Dataflow => "dataflow",
                context_analysis::SeedingMode::CallGraph => "call-graph",
            },
            report.measured_tokens,
            report
                .budget_tokens
                .map(|t| format!(" budget-tokens={t}"))
                .unwrap_or_default(),
        );
    }

    if json {
        print_json(&report)
    } else {
        println!("{}", report.markdown);
        Ok(())
    }
}
