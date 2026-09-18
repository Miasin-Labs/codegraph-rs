use super::Subcommand;

/// `codegraph deps` — shared, per-version dependency graphs.
#[derive(Subcommand)]
pub(crate) enum DepsCommands {
    /// List a project's dependencies (read live from its lockfiles) with
    /// where their source is and the state of their shards (read-only)
    List {
        /// Project directory (default: the current checkout)
        #[arg(short = 'p', long, value_name = "dir")]
        project: Option<String>,
        /// Only direct dependencies
        #[arg(long)]
        direct: bool,
        /// Only this ecosystem (crates, npm, go)
        #[arg(short = 'e', long, value_name = "ecosystem")]
        ecosystem: Option<String>,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Registry totals: projects, versions, shard states, disk use (read-only)
    Status {
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Record a project's dependencies in the registry (no shards built)
    Record {
        /// Project directory (default: the current checkout)
        #[arg(short = 'p', long, value_name = "dir")]
        project: Option<String>,
        /// Re-parse even when the lockfiles are unchanged
        #[arg(short = 'f', long)]
        force: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Record a project and build its missing dependency shards
    Build {
        /// Project directory (default: the current checkout)
        #[arg(short = 'p', long, value_name = "dir")]
        project: Option<String>,
        /// Build pending shards of every recorded project
        #[arg(long)]
        all: bool,
        /// Only direct dependencies
        #[arg(long = "direct-only")]
        direct_only: bool,
        /// Stop starting new shards after this many ms (0 = no limit)
        #[arg(long = "budget-ms", value_name = "ms")]
        budget_ms: Option<u64>,
        /// Extraction time budget per shard in ms
        #[arg(long = "shard-budget-ms", value_name = "ms")]
        shard_budget_ms: Option<u64>,
        /// Most files per shard
        #[arg(long = "max-files", value_name = "n")]
        max_files: Option<usize>,
        /// Most source MiB per shard
        #[arg(long = "max-mb", value_name = "mib")]
        max_mb: Option<u64>,
        /// Stop extracting once a shard's database reaches this many MiB
        #[arg(long = "max-db-mb", value_name = "mib")]
        max_db_mb: Option<u64>,
        /// Rebuild shards that are already up to date
        #[arg(short = 'f', long)]
        force: bool,
        /// Detached run started after `index`/`sync`: one builder at a time;
        /// after the project, continue with every project's pending shards
        #[arg(long, hide = true)]
        background: bool,
        /// Print nothing
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Remove shards no project has used for a while, or beyond a size cap (LRU)
    Gc {
        /// Remove shards unused for this many days
        #[arg(long = "max-age-days", value_name = "days", default_value = "30")]
        max_age_days: u64,
        /// Keep the shard store under this many MiB (0 = no cap)
        #[arg(long = "max-size-mb", value_name = "mib", default_value = "10240")]
        max_size_mb: u64,
        /// Report what would be removed
        #[arg(long = "dry-run")]
        dry_run: bool,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
    /// Show one dependency (`name` or `name@version`): versions, shard
    /// metadata, the projects using it, and optionally a symbol lookup
    Show {
        /// `serde`, `serde@1.0.219`, `@types/node@20.1.0`, `golang.org/x/sys@v0.30.0`
        #[arg(value_name = "name[@version]")]
        spec: String,
        /// Ecosystem (crates, npm, go) when the name is ambiguous
        #[arg(short = 'e', long, value_name = "ecosystem")]
        ecosystem: Option<String>,
        /// Look up this symbol (name or qualified name) in the shard
        #[arg(short = 's', long, value_name = "name")]
        symbol: Option<String>,
        /// Output as JSON
        #[arg(short = 'j', long)]
        json: bool,
    },
}
