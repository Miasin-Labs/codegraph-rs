use super::*;

// =============================================================================
// Commands
// =============================================================================

/// codegraph init [path]
pub(crate) fn cmd_init(path_arg: Option<&str>, verbose: bool) {
    let project_path = resolve_absolute(path_arg);

    clack_intro("Initializing CodeGraph");

    let body = || -> Result<(), String> {
        if is_initialized(&project_path) {
            clack_log_warn(&format!(
                "Already initialized in {}",
                project_path.display()
            ));
            clack_log_info("Use \"codegraph index\" to re-index or \"codegraph sync\" to update");
            // try { offerWatchFallback } catch { /* non-fatal */ }
            offer_watch_fallback(&project_path, false);
            clack_outro("");
            return Ok(());
        }

        let cg = CodeGraph::init_sync(&project_path).map_err(|e| e.to_string())?;
        clack_log_success(&format!("Initialized in {}", project_path.display()));

        let result = run_index_all(&cg, verbose).map_err(|e| e.to_string())?;
        let totals = cg.get_stats().ok().map(|s| (s.node_count, s.edge_count));
        print_index_result(&result, Some(&project_path), totals);

        // try { offerWatchFallback } catch { /* non-fatal */ }
        offer_watch_fallback(&project_path, false);

        clack_outro("Done");
        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        clack_log_error(&format!("Failed: {msg}"));
        process::exit(1);
    }
}

/// codegraph uninit [path]
pub(crate) fn cmd_uninit(path_arg: Option<&str>, force: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            warn(&format!(
                "CodeGraph is not initialized in {}",
                project_path.display()
            ));
            return Ok(());
        }

        if !force {
            // Confirm with user
            print!(
                "{}",
                yellow(&format!(
                    "{} This will permanently delete all CodeGraph data. Continue? (y/N) ",
                    get_glyphs().warn
                ))
            );
            let _ = io::stdout().flush();
            let mut answer = String::new();
            let _ = io::stdin().lock().read_line(&mut answer);
            if answer.trim().to_lowercase() != "y" {
                info("Cancelled");
                return Ok(());
            }
        }

        let cg = CodeGraph::open_sync(&project_path).map_err(|e| e.to_string())?;
        cg.uninitialize().map_err(|e| e.to_string())?;

        // Clean up any git sync hooks we installed (no-op if none / not a repo).
        let removed = remove_git_sync_hook(&project_path, &DEFAULT_SYNC_HOOKS);
        if !removed.installed.is_empty() {
            let names: Vec<&str> = removed.installed.iter().map(|h| h.as_str()).collect();
            info(&format!(
                "Removed git {} sync hook{}",
                names.join(", "),
                if names.len() > 1 { "s" } else { "" }
            ));
        }

        success(&format!(
            "Removed CodeGraph from {}",
            project_path.display()
        ));
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to uninitialize: {msg}"));
        process::exit(1);
    }
}

/// codegraph index [path]
pub(crate) fn cmd_index(path_arg: Option<&str>, force: bool, quiet: bool, verbose: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            info("Run \"codegraph init\" first");
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;

        if quiet {
            // Quiet mode: no UI, just run
            if force {
                cg.clear().map_err(|e| e.to_string())?;
            }
            let result = cg
                .index_all(&IndexOptions::default())
                .map_err(|e| e.to_string())?;
            if !result.success {
                process::exit(1);
            }
            cg.close();
            return Ok(());
        }

        clack_intro("Indexing project");

        if force {
            cg.clear().map_err(|e| e.to_string())?;
            clack_log_info("Cleared existing index");
        }

        let result = run_index_all(&cg, verbose).map_err(|e| e.to_string())?;

        let totals = cg.get_stats().ok().map(|s| (s.node_count, s.edge_count));
        print_index_result(&result, Some(&project_path), totals);

        if !result.success {
            process::exit(1);
        }

        clack_outro("Done");
        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to index: {msg}"));
        process::exit(1);
    }
}

/// codegraph sync [path]
pub(crate) fn cmd_sync(path_arg: Option<&str>, quiet: bool) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            if !quiet {
                error_msg(&format!(
                    "CodeGraph not initialized in {}",
                    project_path.display()
                ));
            }
            process::exit(1);
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;

        if quiet {
            cg.sync(&IndexOptions::default())
                .map_err(|e| e.to_string())?;
            cg.close();
            return Ok(());
        }

        clack_intro("Syncing CodeGraph");

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
            cg.sync(&IndexOptions {
                on_progress: Some(cb_ref),
                signal: None,
                verbose: false,
            })
        };

        progress.into_inner().stop();
        let result = result.map_err(|e| e.to_string())?;

        let total_changes = result.files_added + result.files_modified + result.files_removed;

        if total_changes == 0 {
            clack_log_info("Already up to date");
        } else {
            clack_log_success(&format!(
                "Synced {} changed files",
                format_number(total_changes as u64)
            ));
            let mut details: Vec<String> = Vec::new();
            if result.files_added > 0 {
                details.push(format!("Added: {}", result.files_added));
            }
            if result.files_modified > 0 {
                details.push(format!("Modified: {}", result.files_modified));
            }
            if result.files_removed > 0 {
                details.push(format!("Removed: {}", result.files_removed));
            }
            clack_log_info(&format!(
                "{} {} {} nodes in {}",
                details.join(", "),
                get_glyphs().dash,
                format_number(result.nodes_updated as u64),
                format_duration(result.duration_ms)
            ));
        }

        clack_outro("Done");
        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        if !quiet {
            error_msg(&format!("Failed to sync: {msg}"));
        }
        process::exit(1);
    }
}
