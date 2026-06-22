use super::*;

pub(crate) fn create_verbose_progress() -> impl Fn(&IndexProgress) {
    let last_phase = RefCell::new(String::new());
    let last_pct = Cell::new(-1i64);
    let start_time = now_ms();

    move |progress: &IndexProgress| {
        let elapsed = js_to_fixed((now_ms() - start_time) as f64 / 1000.0, 1);
        let phase = progress.phase.as_str();

        if phase != last_phase.borrow().as_str() {
            *last_phase.borrow_mut() = phase.to_string();
            last_pct.set(-1);
            println!("[{elapsed}s] Phase: {phase}");
        }

        if progress.total > 0 {
            let pct = ((progress.current as f64 / progress.total as f64) * 100.0).floor() as i64;
            // Log every 5% to keep output manageable
            if pct >= last_pct.get() + 5 || progress.current == progress.total {
                last_pct.set(pct);
                let file_suffix = match &progress.current_file {
                    Some(f) => format!(" {} {f}", get_glyphs().dash),
                    None => String::new(),
                };
                println!(
                    "[{elapsed}s]   {}/{} ({pct}%){file_suffix}",
                    progress.current, progress.total
                );
            }
        } else if progress.current > 0 {
            // Scanning phase (no total yet) — log periodically
            if progress.current.is_multiple_of(1000) || progress.current == 1 {
                println!(
                    "[{elapsed}s]   {} files found",
                    format_number(progress.current as u64)
                );
            }
        }
    }
}

/// Run `indexAll` with either the verbose line logger or the shimmer renderer
/// (TS init/index command bodies share this exact branch).
pub(crate) fn run_index_all(cg: &CodeGraph, verbose: bool) -> codegraph::Result<IndexResult> {
    if verbose {
        let cb = create_verbose_progress();
        let cb_ref: &dyn Fn(&IndexProgress) = &cb;
        cg.index_all(&IndexOptions {
            on_progress: Some(cb_ref),
            signal: None,
            verbose: true,
        })
    } else {
        println!("{DIM}{}{RESET}", get_glyphs().rail);
        let _ = io::stdout().flush();
        let progress = RefCell::new(create_shimmer_progress());
        let result = {
            let cb = |p: &IndexProgress| {
                progress.borrow_mut().on_progress(&UiIndexProgress {
                    phase: p.phase.as_str().to_string(),
                    current: p.current as u64,
                    total: p.total as u64,
                });
            };
            let cb_ref: &dyn Fn(&IndexProgress) = &cb;
            cg.index_all(&IndexOptions {
                on_progress: Some(cb_ref),
                signal: None,
                verbose: false,
            })
        };
        progress.into_inner().stop();
        result
    }
}

/// Print indexing results using clack log methods.
///
/// `totals` is the post-run (nodes, edges) snapshot of the whole index.
/// `nodes_created`/`edges_created` are the DB delta of this run — on a
/// re-index where most files are content-hash-unchanged that delta is tiny,
/// which read as "the index is nearly empty" without the totals alongside.
pub(crate) fn print_index_result(
    result: &IndexResult,
    project_path: Option<&Path>,
    totals: Option<(u64, u64)>,
) {
    let has_errors = result.files_errored > 0;

    // Surface non-file-level failures (e.g. lock-acquisition failure
    // when another indexer is running) before the file-count branches.
    // Without this the CLI falls through to "No files found to index",
    // which is actively misleading — the index DID run, it just couldn't
    // get the lock.
    if !result.success && !has_errors && result.files_indexed == 0 {
        let generic = result.errors.iter().find(|e| e.severity == Severity::Error);
        clack_log_error(&generic.map(|e| e.message.clone()).unwrap_or_else(|| {
            format!(
                "Indexing failed {} no further details available",
                get_glyphs().dash
            )
        }));
        return;
    }

    if result.files_indexed > 0 {
        if has_errors {
            clack_log_success(&format!(
                "Indexed {} files ({} could not be parsed)",
                format_number(result.files_indexed as u64),
                format_number(result.files_errored as u64)
            ));
        } else {
            clack_log_success(&format!(
                "Indexed {} files",
                format_number(result.files_indexed as u64)
            ));
        }
        let delta = format!(
            "+{} nodes, +{} edges in {}",
            format_number(result.nodes_created as u64),
            format_number(result.edges_created as u64),
            format_duration(result.duration_ms)
        );
        // Append totals when they differ from the delta (i.e. an incremental
        // re-index over an existing graph) so a small delta isn't mistaken
        // for a near-empty index.
        match totals {
            Some((nodes, edges))
                if nodes != result.nodes_created as u64 || edges != result.edges_created as u64 =>
            {
                clack_log_info(&format!(
                    "{delta} (index total: {} nodes, {} edges)",
                    format_number(nodes),
                    format_number(edges)
                ));
            }
            _ => clack_log_info(&delta),
        }
    } else if has_errors {
        clack_log_error(&format!(
            "Indexing failed {} all {} files had errors",
            get_glyphs().dash,
            format_number(result.files_errored as u64)
        ));
    } else {
        clack_log_warn("No files found to index");
    }

    if has_errors {
        // Insertion-ordered code → count map (TS `Map`).
        let mut errors_by_code: Vec<(String, u64)> = Vec::new();
        for err in &result.errors {
            if err.severity == Severity::Error {
                let code = err.code.clone().unwrap_or_else(|| "unknown".to_string());
                match errors_by_code.iter_mut().find(|(c, _)| *c == code) {
                    Some((_, count)) => *count += 1,
                    None => errors_by_code.push((code, 1)),
                }
            }
        }

        let code_label = |code: &str| -> Option<&'static str> {
            match code {
                "parse_error" => Some("files failed to parse"),
                "read_error" => Some("files could not be read"),
                "size_exceeded" => Some("files exceeded size limit"),
                "path_traversal" => Some("blocked paths"),
                "unsupported_language" => Some("unsupported language"),
                "parser_error" => Some("parser initialization failures"),
                _ => None,
            }
        };

        let breakdown = errors_by_code
            .iter()
            .map(|(code, count)| {
                format!(
                    "{} {}",
                    format_number(*count),
                    code_label(code).unwrap_or(code.as_str())
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        clack_note(&breakdown, "Error breakdown");

        if let Some(pp) = project_path {
            write_error_log(pp, &result.errors);
            clack_log_info("See .codegraph/errors.log for details");
        }

        if result.files_indexed > 0 {
            clack_log_info(&format!(
                "The index is fully usable {} only the failed files are missing.",
                get_glyphs().dash
            ));
        }
    } else if let Some(pp) = project_path {
        let log_path = pp.join(".codegraph").join("errors.log");
        if log_path.exists() {
            let _ = std::fs::remove_file(&log_path);
        }
    }
}
