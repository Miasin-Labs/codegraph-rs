use super::*;

/// `codegraph history` — the global, redacted tool-call history flywheel.
#[derive(Subcommand)]
pub(crate) enum HistoryCommands {
    /// Parse JFC logs into the redacted history database
    Ingest {
        /// Log directory (default: ~/.config/jfc/logs)
        #[arg(long, value_name = "dir")]
        logs: Option<String>,
        /// History DB path (default: ~/.codegraph/history.db)
        #[arg(long, value_name = "path")]
        db: Option<String>,
        /// Tag ingested events with this project path
        #[arg(short = 'p', long, value_name = "path")]
        project: Option<String>,
    },
    /// Show usage rankings from the history database
    Show {
        /// History DB path (default: ~/.codegraph/history.db)
        #[arg(long, value_name = "path")]
        db: Option<String>,
        /// Scope file rankings to a project path substring
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
