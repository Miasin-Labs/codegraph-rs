use super::Subcommand;

/// `codegraph history` — the global, redacted tool-call history flywheel.
#[derive(Subcommand)]
pub(crate) enum HistoryCommands {
    /// Parse JFC logs into the redacted history database (idempotent: one
    /// row per tool call, keyed on its native id)
    Ingest {
        /// Log directory (default: ~/.config/jfc/logs)
        #[arg(long, value_name = "dir")]
        logs: Option<String>,
        /// History DB path (default: ~/.codegraph/history.db)
        #[arg(long, value_name = "path")]
        db: Option<String>,
        /// Attribute ingested calls to this project path (default: derived per call)
        #[arg(short = 'p', long, value_name = "path")]
        project: Option<String>,
    },
    /// Show usage rankings from the history database (read-only)
    Show {
        /// History DB path (default: ~/.codegraph/history.db)
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
}
