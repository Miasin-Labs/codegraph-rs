use super::{
    CodeGraph,
    OpenOptions,
    bold,
    cyan,
    detect_worktree_index_mismatch,
    error_msg,
    format_number,
    get_codegraph_dir,
    get_glyphs,
    green,
    info,
    is_initialized,
    iso_from_epoch_ms,
    js_to_fixed,
    process,
    resolve_absolute,
    resolve_project_path,
    success,
    warn,
    worktree_mismatch_warning,
    yellow,
};

/// codegraph status [path]
pub(crate) fn cmd_status(path_arg: Option<&str>, json: bool) {
    let project_path = resolve_project_path(path_arg);
    // The directory the user actually ran from, before walking up to the index
    // root. Used to detect when the resolved index lives in a different git
    // working tree (e.g. a nested worktree borrowing the main checkout's index).
    let start_path = resolve_absolute(path_arg);
    let worktree_mismatch = detect_worktree_index_mismatch(&start_path, &project_path);

    let body = || -> Result<(), String> {
        if !is_initialized(&project_path) {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "initialized": false,
                        "version": env!("CARGO_PKG_VERSION"),
                        "projectPath": project_path.to_string_lossy(),
                        "indexPath": get_codegraph_dir(&project_path).to_string_lossy(),
                        "lastIndexed": null,
                    })
                );
                return Ok(());
            }
            println!("{}", bold("\nCodeGraph Status\n"));
            info(&format!("Project: {}", project_path.display()));
            warn("Not initialized");
            info("Run \"codegraph init\" to initialize");
            return Ok(());
        }

        let cg =
            CodeGraph::open(&project_path, &OpenOptions::default()).map_err(|e| e.to_string())?;
        let stats = cg.get_stats().map_err(|e| e.to_string())?;
        let changes = cg.get_changed_files().map_err(|e| e.to_string())?;
        let backend = cg.get_backend();
        let journal_mode = cg.get_journal_mode().map_err(|e| e.to_string())?;

        // JSON output mode
        if json {
            let last_indexed_ms = cg.get_last_indexed_at().map_err(|e| e.to_string())?;

            // nodesByKind / languages: HashMap iteration order is
            // nondeterministic, so keys are emitted alphabetically (the TS
            // object insertion order is itself data-dependent).
            let mut kind_entries: Vec<(&String, &u64)> = stats.nodes_by_kind.iter().collect();
            kind_entries.sort_by(|a, b| a.0.cmp(b.0));
            let mut nodes_by_kind = serde_json::Map::new();
            for (k, v) in kind_entries {
                nodes_by_kind.insert(k.clone(), serde_json::json!(v));
            }

            let mut languages: Vec<&String> = stats
                .files_by_language
                .iter()
                .filter(|(_, count)| **count > 0)
                .map(|(lang, _)| lang)
                .collect();
            languages.sort();

            println!(
                "{}",
                serde_json::json!({
                    "initialized": true,
                    "version": env!("CARGO_PKG_VERSION"),
                    "projectPath": project_path.to_string_lossy(),
                    "indexPath": get_codegraph_dir(&project_path).to_string_lossy(),
                    "lastIndexed": last_indexed_ms.map(iso_from_epoch_ms),
                    "fileCount": stats.file_count,
                    "nodeCount": stats.node_count,
                    "edgeCount": stats.edge_count,
                    "dbSizeBytes": stats.db_size_bytes,
                    "backend": backend.as_str(),
                    "journalMode": journal_mode,
                    "nodesByKind": nodes_by_kind,
                    "languages": languages,
                    "pendingChanges": {
                        "added": changes.added.len(),
                        "modified": changes.modified.len(),
                        "removed": changes.removed.len(),
                    },
                    "worktreeMismatch": worktree_mismatch.as_ref().map(|m| serde_json::json!({
                        "worktreeRoot": m.worktree_root.to_string_lossy(),
                        "indexRoot": m.index_root.to_string_lossy(),
                    })),
                })
            );
            cg.close();
            return Ok(());
        }

        println!("{}", bold("\nCodeGraph Status\n"));

        // Project info
        println!("{} {}", cyan("Project:"), project_path.display());
        if let Some(m) = &worktree_mismatch {
            warn(&worktree_mismatch_warning(m));
        }
        println!();

        // Index stats
        println!("{}", bold("Index Statistics:"));
        println!("  Files:     {}", format_number(stats.file_count));
        println!("  Nodes:     {}", format_number(stats.node_count));
        println!("  Edges:     {}", format_number(stats.edge_count));
        println!(
            "  DB Size:   {} MB",
            js_to_fixed(stats.db_size_bytes as f64 / 1024.0 / 1024.0, 2)
        );
        // Surface the active SQLite backend. (TS labels its node:sqlite
        // backend; the Rust port reports "native" per the porting contract.)
        let backend_label = green(&format!(
            "{} {} built-in (full WAL)",
            backend.as_str(),
            get_glyphs().dash
        ));
        println!("  Backend:   {backend_label}");
        // Effective journal mode: 'wal' means concurrent reads never block on a
        // writer; anything else means they can ("database is locked"). A non-wal
        // mode means the filesystem can't support it (network mounts, WSL2
        // /mnt). See issue #238.
        let journal_label = if journal_mode == "wal" {
            green("wal")
        } else {
            yellow(&format!(
                "{} {} WAL inactive; reads can block on writes",
                if journal_mode.is_empty() {
                    "unknown"
                } else {
                    journal_mode.as_str()
                },
                get_glyphs().dash
            ))
        };
        println!("  Journal:   {journal_label}");
        println!();

        // Node breakdown (count desc; key asc tie-break for determinism)
        println!("{}", bold("Nodes by Kind:"));
        let mut nodes_by_kind: Vec<(&String, &u64)> = stats
            .nodes_by_kind
            .iter()
            .filter(|(_, count)| **count > 0)
            .collect();
        nodes_by_kind.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (kind, count) in &nodes_by_kind {
            println!("  {:<15} {}", kind, format_number(**count));
        }
        println!();

        // Language breakdown
        println!("{}", bold("Files by Language:"));
        let mut files_by_lang: Vec<(&String, &u64)> = stats
            .files_by_language
            .iter()
            .filter(|(_, count)| **count > 0)
            .collect();
        files_by_lang.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        for (lang, count) in &files_by_lang {
            println!("  {:<15} {}", lang, format_number(**count));
        }
        println!();

        // Pending changes
        let total_changes = changes.added.len() + changes.modified.len() + changes.removed.len();
        if total_changes > 0 {
            println!("{}", bold("Pending Changes:"));
            if !changes.added.is_empty() {
                println!("  Added:     {} files", changes.added.len());
            }
            if !changes.modified.is_empty() {
                println!("  Modified:  {} files", changes.modified.len());
            }
            if !changes.removed.is_empty() {
                println!("  Removed:   {} files", changes.removed.len());
            }
            info("Run \"codegraph sync\" to update the index");
        } else {
            success("Index is up to date");
        }
        println!();

        cg.close();
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("Failed to get status: {msg}"));
        process::exit(1);
    }
}
