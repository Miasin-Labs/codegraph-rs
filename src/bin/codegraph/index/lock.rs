use super::{
    CodeGraph,
    OpenOptions,
    error_msg,
    get_codegraph_dir,
    get_glyphs,
    info,
    is_initialized,
    process,
    resolve_project_path,
    success,
};

/// codegraph unlock [path]
/// codegraph resolve-bench [path] --limit N (hidden)
///
/// Dry-runs the resolver over pending unresolved refs and prints throughput.
/// Measurement harness for resolver optimization work — no writes.
pub(crate) fn cmd_resolve_bench(path_arg: Option<&str>, limit: usize) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            process::exit(1);
        }
        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let report = cg.resolve_bench(limit).map_err(|e| e.to_string())?;
        println!("{report}");
        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("resolve-bench failed: {msg}"));
        process::exit(1);
    }
}

pub(crate) fn cmd_unlock(path_arg: Option<&str>) {
    let project_path = resolve_project_path(path_arg);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            error_msg(&format!(
                "CodeGraph not initialized in {}",
                project_path.display()
            ));
            return Ok(());
        }

        let lock_path = get_codegraph_dir(&project_path).join("codegraph.lock");

        if !lock_path.exists() {
            info(&format!(
                "No lock file found {} nothing to do",
                get_glyphs().dash
            ));
            return Ok(());
        }

        std::fs::remove_file(&lock_path).map_err(|e| e.to_string())?;
        success("Removed lock file. You can now run indexing again.");
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to remove lock: {msg}"));
        process::exit(1);
    }
}
