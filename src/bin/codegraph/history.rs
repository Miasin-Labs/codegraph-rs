use std::path::Path;
use std::time::Duration;

use codegraph::history::memory::{About, RecallRequest, recall_at};
use codegraph::history::{
    ClaudeCodeProjects,
    EventSource,
    HistoryDb,
    IncrementalOptions,
    IncrementalReport,
    JfcLogs,
    OpencodeDb,
    default_history_path,
    default_jfc_logs_dir,
    now_ms,
    parse_duration_ms,
    sources,
};

use super::{
    HistoryCommands,
    PathBuf,
    bold,
    dim,
    error_msg,
    format_number,
    parse_int_js,
    print_json,
    process,
};

/// Hard deadline of a CLI recall (the MCP tool uses the same bound).
const RECALL_DEADLINE: Duration = Duration::from_millis(250);

pub(crate) fn cmd_history(command: HistoryCommands) {
    match command {
        HistoryCommands::Ingest {
            incremental,
            source,
            logs,
            claude_dir,
            opencode_db,
            db,
            project,
            repo,
            budget_ms,
            max_events,
            no_git,
            quiet,
        } => {
            let sources = IngestSources {
                list: source,
                logs,
                claude_dir,
                opencode_db,
            };
            let limit = |value: Option<u64>, default: u64| match value {
                Some(0) => None,
                Some(v) => Some(v),
                None => incremental.then_some(default),
            };
            let opts = IncrementalOptions {
                time_budget: limit(budget_ms, 5_000).map(Duration::from_millis),
                max_events: limit(max_events.map(|n| n as u64), 50_000).map(|n| n as usize),
                scope: repo.map(|r| std::fs::canonicalize(&r).unwrap_or_else(|_| PathBuf::from(r))),
                project,
                git: !no_git,
            };
            cmd_history_ingest(&sources, db.as_deref(), &opts, quiet);
        }
        HistoryCommands::Show {
            db,
            project,
            top,
            json,
        } => cmd_history_show(db.as_deref(), project.as_deref(), &top, json),
        HistoryCommands::Recall {
            about,
            since,
            limit,
            project,
            related,
            db,
            json,
        } => cmd_history_recall(
            &RecallArgs {
                about: &about,
                since: since.as_deref(),
                limit,
                project: project.as_deref(),
                related,
            },
            db.as_deref(),
            json,
        ),
    }
}

struct IngestSources {
    list: String,
    logs: Option<String>,
    claude_dir: Option<String>,
    opencode_db: Option<String>,
}

impl IngestSources {
    /// The selected adapters, most active agent first (a spent budget
    /// defers the later ones to the next run).
    fn build(&self) -> Result<Vec<Box<dyn EventSource>>, String> {
        let wanted: Vec<&str> = self
            .list
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect();
        let all = wanted.is_empty() || wanted.contains(&"all");
        for w in &wanted {
            if !matches!(*w, "all" | "claude-code" | "opencode" | "jfc") {
                return Err(format!(
                    "unknown source `{w}` (expected all, claude-code, opencode, jfc)"
                ));
            }
        }
        let on = |name: &str| all || wanted.contains(&name);
        let mut out: Vec<Box<dyn EventSource>> = Vec::new();
        if on("claude-code") {
            let dir = self
                .claude_dir
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(sources::claude_code::default_dir);
            out.push(Box::new(ClaudeCodeProjects::new(dir)));
        }
        if on("opencode") {
            let path = self
                .opencode_db
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(sources::opencode::default_path);
            out.push(Box::new(OpencodeDb::new(path)));
        }
        if on("jfc") {
            let dir = self
                .logs
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(default_jfc_logs_dir);
            out.push(Box::new(JfcLogs::new(dir)));
        }
        Ok(out)
    }
}

fn cmd_history_ingest(
    sources: &IngestSources,
    db: Option<&str>,
    opts: &IncrementalOptions,
    quiet: bool,
) {
    let db_path = db.map(PathBuf::from).unwrap_or_else(default_history_path);
    let body = || -> Result<Option<IncrementalReport>, String> {
        let adapters = sources.build()?;
        codegraph::history::ensure_store_dir(&db_path).map_err(|e| e.to_string())?;
        let mut lock = codegraph::history::ingest::writer_lock(&db_path);
        if lock.live_holder().is_some() {
            return Ok(None);
        }
        lock.acquire().map_err(|e| e.to_string())?;
        let mut hdb = HistoryDb::open(&db_path).map_err(|e| e.to_string())?;
        let refs: Vec<&dyn EventSource> = adapters.iter().map(|b| b.as_ref()).collect();
        let report = hdb.ingest_events(&refs, opts).map_err(|e| e.to_string())?;
        lock.release();
        Ok(Some(report))
    };
    match body() {
        Ok(_) if quiet => {}
        Ok(None) => println!(
            "Another history ingest is running for {}; skipped.",
            db_path.display()
        ),
        Ok(Some(report)) => print_ingest_report(&report, &db_path),
        Err(msg) => {
            if !quiet {
                error_msg(&format!("history ingest failed: {msg}"));
            }
            process::exit(1);
        }
    }
}

fn print_ingest_report(report: &IncrementalReport, db_path: &std::path::Path) {
    let total: usize = report.sources.iter().map(|s| s.writes.remembered).sum();
    println!(
        "Ingested {} new tool call(s) into {}",
        format_number(total as u64),
        db_path.display()
    );
    for s in &report.sources {
        println!(
            "{}",
            dim(&format!(
                "  {:<12} {} new, {} already present, {} prompt(s), {} input(s) read, {} unreadable, {} deferred, {} with masked credentials — {}",
                s.source,
                format_number(s.writes.remembered as u64),
                format_number(s.writes.already_present as u64),
                format_number(s.writes.prompts as u64),
                format_number(s.stats.inputs as u64),
                format_number(s.stats.skipped as u64),
                format_number(s.stats.deferred as u64),
                format_number(s.writes.redacted as u64),
                s.location,
            ))
        );
    }
    println!(
        "{}",
        dim(&format!(
            "  {} repositor(ies) rolled up{}",
            report.repos,
            if report.deferred > 0 {
                format!(
                    "; {} changed input(s) left for the next run",
                    report.deferred
                )
            } else {
                String::new()
            }
        ))
    );
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

/// What `history recall` was asked.
struct RecallArgs<'a> {
    about: &'a str,
    since: Option<&'a str>,
    limit: usize,
    project: Option<&'a str>,
    related: bool,
}

fn cmd_history_recall(args: &RecallArgs<'_>, db: Option<&str>, json: bool) {
    let db_path = db.map(PathBuf::from).unwrap_or_else(default_history_path);
    let body = || -> Result<(), String> {
        let since_ms = match args.since {
            Some(s) => Some(
                now_ms()
                    - parse_duration_ms(s)
                        .ok_or_else(|| format!("invalid --since `{s}` (try 12h, 7d, 2w)"))?,
            ),
            None => None,
        };
        let root = match args.project {
            Some(p) => recall_root(p)?,
            None => std::env::current_dir().map_err(|e| e.to_string())?,
        };
        let mut about = About::parse(args.about);
        // A project nested in its repository names paths relative to
        // itself; the memory keys them relative to the repository.
        if let Some(repo) = codegraph::history::repo_root_of(&root) {
            if let Ok(base) = root.strip_prefix(&repo) {
                about = about.under(&base.to_string_lossy());
            }
        }
        let request = RecallRequest {
            about,
            since_ms,
            limit: args.limit.max(1),
            related: args.related,
        };
        let report =
            recall_at(&db_path, &root, &request, RECALL_DEADLINE).map_err(|e| e.to_string())?;
        if json {
            return print_json(&serde_json::to_value(&report).map_err(|e| e.to_string())?);
        }
        print!("{}", report.render_text());
        Ok(())
    };
    if let Err(msg) = body() {
        error_msg(&format!("history recall failed: {msg}"));
        process::exit(1);
    }
}

/// `--project`: an existing path as given, else a project name the atlas
/// knows (read-only).
fn recall_root(arg: &str) -> Result<PathBuf, String> {
    if Path::new(arg).exists() {
        return Ok(std::fs::canonicalize(arg).unwrap_or_else(|_| PathBuf::from(arg)));
    }
    let atlas = codegraph::atlas::open_read_only()
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            format!("\"{arg}\" is not a path, and no atlas exists to look it up by name")
        })?;
    let mut named = atlas.projects_named(arg).map_err(|e| e.to_string())?;
    // Prefer whole checkouts that are not linked worktrees (the main one).
    if named.len() > 1 {
        let main: Vec<_> = named
            .iter()
            .filter(|p| p.is_checkout() && !p.is_worktree)
            .cloned()
            .collect();
        if main.len() == 1 {
            named = main;
        }
    }
    match named.len() {
        1 => Ok(named.remove(0).root),
        0 => Err(format!(
            "No path or registered project named \"{arg}\" (see \"codegraph projects\")"
        )),
        n => Err(format!(
            "\"{arg}\" names {n} projects; pass one of their paths:\n{}",
            named
                .iter()
                .map(|p| format!("  {}", p.root.display()))
                .collect::<Vec<_>>()
                .join("\n")
        )),
    }
}
