use super::*;

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
    let logs_dir = logs.map(PathBuf::from).unwrap_or_else(default_jfc_logs_dir);
    let db_path = db.map(PathBuf::from).unwrap_or_else(default_history_path);

    let body = || -> Result<(), String> {
        let mut events = parse_logs_dir(&logs_dir);
        if let Some(p) = project {
            for e in &mut events {
                e.project = Some(p.to_string());
            }
        }
        let mut hdb = HistoryDb::open(&db_path).map_err(|e| e.to_string())?;
        let n = hdb.ingest(&events).map_err(|e| e.to_string())?;
        println!(
            "Ingested {} tool event(s) from {} into {}",
            format_number(n as u64),
            logs_dir.display(),
            db_path.display(),
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
        let hdb = HistoryDb::open(&db_path).map_err(|e| e.to_string())?;
        let total = hdb.count().map_err(|e| e.to_string())?;
        let tools = hdb.hot_tools(top).map_err(|e| e.to_string())?;
        let files = hdb.hot_files(project, top).map_err(|e| e.to_string())?;
        let chains = hdb.hot_chains(top).map_err(|e| e.to_string())?;
        let co = hdb.co_access(top).map_err(|e| e.to_string())?;

        if json {
            let val = serde_json::json!({
                "total": total,
                "hot_tools": tools,
                "hot_files": files,
                "hot_chains": chains,
                "co_access": co,
            });
            return print_json(&val);
        }

        println!(
            "{}",
            bold(&format!(
                "\nTool-call history — {} event(s)\n",
                format_number(total as u64)
            ))
        );
        println!("{}", bold("Hot tools:"));
        for (k, c) in &tools {
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
