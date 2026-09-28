use super::Subcommand;

/// `codegraph models` — library models for taint (sources, sinks,
/// summaries, sanitizers), imported from CodeQL's Models-as-Data.
#[derive(Subcommand)]
pub(crate) enum ModelsCommands {
    /// Import a github/codeql checkout's `*.model.yml` files into
    /// codegraph's model files (one `<language>.tsv` per language plus
    /// `NOTICE`), and report what was imported, approximated and dropped
    ImportCodeql {
        /// The checkout (or any dir holding `<language>/…/*.model.yml`)
        #[arg(value_name = "dir")]
        dir: String,
        /// Where to write (default: ~/.codegraph/models/codeql); use it
        /// with CODEGRAPH_MODELS_DIR, or point it at
        /// src/analyze/rules/models/data to refresh the vendored models
        #[arg(short = 'o', long, value_name = "dir")]
        out: Option<String>,
        /// Also import generated summaries and neutrals
        #[arg(long)]
        generated: bool,
        /// Only report; write nothing
        #[arg(long)]
        dry_run: bool,
    },
    /// List the loaded models, filtered
    List {
        /// java, cpp, python, javascript, rust
        #[arg(short = 'l', long, value_name = "language")]
        language: Option<String>,
        /// source, sink, summary, neutral, barrier, guard
        #[arg(short = 'r', long, value_name = "role")]
        role: Option<String>,
        /// A kind, group or alias (`sql`, `remote`, `path-injection`…)
        #[arg(short = 'k', long, value_name = "kind")]
        kind: Option<String>,
        /// Callable name (method or function)
        #[arg(short = 'n', long, value_name = "name")]
        name: Option<String>,
        /// At most this many rows
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// Counts of the loaded models per language, role and kind
    Stats,
    /// How many of a project's library calls models match, and how
    /// (resolution, declared type, import, name)
    Match {
        /// Project directory (default: the current directory)
        #[arg(short = 'p', long, value_name = "dir")]
        project: Option<String>,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
}
