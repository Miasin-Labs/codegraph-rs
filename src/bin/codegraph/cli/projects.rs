use super::Subcommand;

/// `codegraph projects` — the atlas of indexed projects and their links.
#[derive(Subcommand)]
pub(crate) enum ProjectsCommands {
    /// List registered projects (the default)
    List,
    /// One project: git, index stats, worktrees/clones, links in and out
    Show {
        /// Project root (or any path inside it) or name
        #[arg(value_name = "path|name")]
        project: String,
    },
    /// Links from and to one project
    Links {
        /// Project root (or any path inside it) or name
        #[arg(value_name = "path|name")]
        project: String,
        /// Include links that stay inside the project (its own workspace members)
        #[arg(long)]
        all: bool,
    },
    /// Find existing indexes and register them (read-only: never indexes or syncs)
    Scan {
        /// Directory to search (repeatable; default: your home directory)
        #[arg(long = "root", value_name = "dir")]
        roots: Vec<String>,
        /// Directory levels below each root to search
        #[arg(long = "max-depth", value_name = "n", default_value = "8")]
        max_depth: usize,
        /// Stop after this many seconds (partial results are kept)
        #[arg(long = "budget-secs", value_name = "secs", default_value = "120")]
        budget_secs: u64,
    },
    /// Mark projects whose root or index vanished as missing
    Prune {
        /// Delete them instead of marking them
        #[arg(long)]
        remove: bool,
    },
    /// Print the project-link graph as a Mermaid flowchart
    Graph {
        /// Only the cluster around this project (path or name)
        #[arg(value_name = "path|name")]
        project: Option<String>,
        /// Output format (mermaid)
        #[arg(long, value_name = "format", default_value = "mermaid")]
        format: String,
        /// Links to follow from the focused project
        #[arg(long, value_name = "n", default_value = "2")]
        depth: usize,
        /// Only these link kinds (repeatable), e.g. cargo_path_dep
        #[arg(long = "kind", value_name = "kind")]
        kinds: Vec<String>,
        /// Also draw projects without links
        #[arg(long)]
        all: bool,
    },
    /// Register (or refresh) one indexed project
    Register {
        /// Project path (default: the nearest index above the current directory)
        #[arg(value_name = "path")]
        path: Option<String>,
        /// Print nothing (background runs)
        #[arg(short = 'q', long)]
        quiet: bool,
    },
}
