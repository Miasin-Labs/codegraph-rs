use super::{
    HistoryCommands,
    HistoryDb,
    IngestOptions,
    JfcLogs,
    PathBuf,
    ToolCallSource,
    bold,
    default_history_path,
    default_jfc_logs_dir,
    dim,
    error_msg,
    format_number,
    parse_int_js,
    print_json,
    process,
};

pub(crate) fn cmd_history(command: HistoryCommands) {
    match command {
        HistoryCommands::Ingest { logs, db, project } => {
            cmd_history_ingest(logs.as_deref(), db.as_deref(), project.as_deref())
        }
        HistoryCommands::Show {
            db,
            project,
            top,
            json,
        } => cmd_history_show(db.as_deref(), project.as_deref(), &top, json),
    }
}

pub(crate) fn cmd_history_ingest(logs: Option<&str>, db: Option<&str>, project: Option<&str>) {
    let source = JfcLogs::new(logs.map(PathBuf::from).unwrap_or_else(default_jfc_logs_dir));
    let db_path = db.map(PathBuf::from).unwrap_or_else(default_history_path);
    let opts = IngestOptions {
        project: project.map(str::to_owned),
    };

    let body = || -> Result<(), String> {
        let mut hdb = HistoryDb::open(&db_path).map_err(|e| e.to_string())?;
        let report = hdb
            .ingest_source(&source, &opts)
            .map_err(|e| e.to_string())?;
        println!(
            "Ingested {} new tool call(s) from {} log file(s) in {} into {}",
            format_number(report.inserted as u64),
            format_number(report.source.inputs as u64),
            source.location(),
            db_path.display(),
        );
        println!(
            "{}",
            dim(&format!(
                "  {} already present, {} with masked credentials, {} unreadable file(s)",
                format_number(report.already_present as u64),
                format_number(report.redacted as u64),
                format_number(report.source.skipped as u64),
            ))
        );
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("history ingest failed: {msg}"));
        process::exit(1);
    }
}

pub(crate) fn cmd_history_show(db: Option<&str>, project: Option<&str>, top_arg: &str, json: bool) {
    let db_path = db.map(PathBuf::from).unwrap_or_else(default_history_path);
    let top = parse_int_js(top_arg).unwrap_or(20).max(1) as usize;

    let body = || -> Result<(), String> {
        // Read-only: `show` must never create or migrate the store.
        let Some(hdb) = HistoryDb::open_read_only(&db_path).map_err(|e| e.to_string())? else {
            if json {
                return print_json(&serde_json::json!({
                    "exists": false,
                    "total": 0,
                    "hot_tools": [],
                    "hot_commands": [],
                    "hot_files": [],
                    "hot_chains": [],
                    "co_access": [],
                }));
            }
            println!(
                "No tool-call history yet at {} — run `codegraph history ingest` to build it.",
                db_path.display()
            );
            return Ok(());
        };
        let err = |e: codegraph::history::HistoryError| e.to_string();
        let total = hdb.count(project).map_err(err)?;
        let tools = hdb.hot_tools(project, top).map_err(err)?;
        let commands = hdb.hot_commands(project, top).map_err(err)?;
        let files = hdb.hot_files(project, top).map_err(err)?;
        let chains = hdb.hot_chains(project, top).map_err(err)?;
        let co = hdb.co_access(project, top).map_err(err)?;

        if json {
            let val = serde_json::json!({
                "exists": true,
                "total": total,
                "hot_tools": tools,
                "hot_commands": commands,
                "hot_files": files,
                "hot_chains": chains,
                "co_access": co,
            });
            return print_json(&val);
        }

        let scope = project.map(|p| format!(" in {p}")).unwrap_or_default();
        println!(
            "{}",
            bold(&format!(
                "\nTool-call history — {} call(s){scope}\n",
                format_number(total as u64)
            ))
        );
        println!("{}", bold("Hot tools:"));
        for (k, c) in &tools {
            println!("  {c:>7}  {k}");
        }
        println!("{}", bold("\nHot commands:"));
        for (k, c) in &commands {
            println!("  {c:>7}  {k}");
        }
        println!("{}", bold("\nHot files:"));
        for (p, c) in &files {
            println!("  {c:>7}  {p}");
        }
        println!("{}", bold("\nHot command chains:"));
        for (chain, c) in &chains {
            println!("  {c:>7}  {chain}");
        }
        println!("{}", bold("\nCo-accessed file pairs (same session):"));
        for (a, b, c) in &co {
            println!("  {c:>7}  {a}  +  {b}");
        }
        Ok(())
    };

    if let Err(msg) = body() {
        error_msg(&format!("history show failed: {msg}"));
        process::exit(1);
    }
}
