use super::Subcommand;

/// `codegraph history` — the global, redacted tool-call history flywheel.
#[derive(Subcommand)]
pub(crate) enum HistoryCommands {
    /// Read agent session stores (Claude Code, opencode, JFC) into the
    /// redacted history database and its cross-session memory (idempotent:
    /// one row per tool call, keyed on its native id; resumes from
    /// per-source checkpoints)
    Ingest {
        /// Budgeted background run: at most --budget-ms / --max-events per run
        #[arg(long)]
        incremental: bool,
        /// Sources to read: all, claude-code, opencode, jfc (comma-separated)
        #[arg(long, value_name = "list", default_value = "all")]
        source: String,
        /// JFC log directory (default: ~/.config/jfc/logs)
        #[arg(long, value_name = "dir")]
        logs: Option<String>,
        /// Claude Code projects directory (default: ~/.claude/projects)
        #[arg(long, value_name = "dir")]
        claude_dir: Option<String>,
        /// opencode database, opened read-only (default: ~/.local/share/opencode/opencode.db)
        #[arg(long, value_name = "path")]
        opencode_db: Option<String>,
        /// History DB path (default: $CODEGRAPH_HISTORY_DB or ~/.codegraph/history.db)
        #[arg(long, value_name = "path")]
        db: Option<String>,
        /// Attribute ingested calls to this project path (default: derived per call)
        #[arg(short = 'p', long, value_name = "path")]
        project: Option<String>,
        /// Only sessions that worked under this directory
        #[arg(long, value_name = "dir")]
        repo: Option<String>,
        /// Time budget in ms (default: 5000 with --incremental, else unlimited; 0 = unlimited)
        #[arg(long, value_name = "ms")]
        budget_ms: Option<u64>,
        /// Tool calls per run (default: 50000 with --incremental, else unlimited; 0 = unlimited)
        #[arg(long, value_name = "n")]
        max_events: Option<usize>,
        /// Skip mining git co-change for the repositories touched
        #[arg(long)]
        no_git: bool,
        /// Print nothing (background runs)
        #[arg(short = 'q', long)]
        quiet: bool,
    },
    /// Show usage rankings from the history database (read-only)
    Show {
        /// History DB path (default: $CODEGRAPH_HISTORY_DB or ~/.codegraph/history.db)
        #[arg(long, value_name = "path")]
        db: Option<String>,
        /// Scope rankings to a project path substring
        #[arg(short = 'p', long, value_name = "path")]
        project: Option<String>,
        /// Show top N per section
        #[arg(short = 't', long, value_name = "number", default_value = "20")]
        top: String,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// What earlier agent sessions in this repository did (read-only):
    /// a path prefix, `symbol:NAME`, `failures`, `cochange[:PATH]`, or `last`
    Recall {
        /// What to recall (default: last)
        #[arg(value_name = "about", default_value = "last")]
        about: String,
        /// Only activity newer than this (`12h`, `7d`, `2w`)
        #[arg(long, value_name = "age")]
        since: Option<String>,
        /// Episodes (or rows) to show
        #[arg(short = 'l', long, value_name = "n", default_value = "3")]
        limit: usize,
        /// Project: a path, or a name registered in the atlas (`codegraph
        /// projects`) (default: the current directory's repository)
        #[arg(short = 'p', long, value_name = "name|path")]
        project: Option<String>,
        /// Also recall from projects linked to it in the atlas (path
        /// dependencies either way, clones of its remote), labelled by project
        #[arg(long)]
        related: bool,
        /// History DB path (default: $CODEGRAPH_HISTORY_DB or ~/.codegraph/history.db)
        #[arg(long, value_name = "path")]
        db: Option<String>,
        /// Output as JSON (what the MCP tool returns)
        #[arg(short = 'j', long)]
        json: bool,
    },
}
