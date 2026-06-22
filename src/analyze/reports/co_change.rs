use super::*;

// analyze co-change
// =============================================================================

/// One temporally-coupled pair of symbols (cross-file).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoChangePairSummary {
    pub a: SymbolRef,
    pub b: SymbolRef,
    /// Commits in which both symbols' files changed together.
    pub times_changed_together: u32,
    pub total_changes_a: u32,
    pub total_changes_b: u32,
    /// `timesChangedTogether / max(totalChangesA, totalChangesB)` ∈ [0, 1].
    pub confidence: f64,
}

/// Result of [`co_change_report`].
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoChangeReport {
    /// Present when the analysis was seeded on one symbol.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<SymbolRef>,
    /// Commits mined from `git log --name-only` (capped by `maxCommits`).
    pub commits_analyzed: usize,
    pub max_commits: usize,
    pub min_support: u32,
    /// Cross-file pairs found (before truncation to `pairs`).
    pub cross_file_pair_count: usize,
    /// Same-file pairs folded out of the listing — symbols in one file
    /// co-change by construction (git history is per-file), so they carry
    /// no coupling signal.
    pub same_file_pair_count: usize,
    pub truncated: bool,
    pub pairs: Vec<CoChangePairSummary>,
    pub note: String,
}

/// Mine commit history with `git log --name-only`, record-separated so the
/// parse is unambiguous. Returns an empty list when git is unavailable or
/// the directory is not a repository.
///
/// Host-side replacement for the engine's `co_change::fetch_git_history`:
/// that helper feeds `--format=%H` output (hash, *blank line*, files) into a
/// parser that expects hash-then-files-then-blank, so against real git it
/// mistakes the first file of every commit for the next commit's hash.
/// Recorded in `notes/close-tier1-needs.md`; swap back once fixed.
fn fetch_commit_history(workspace_root: &Path, max_commits: usize) -> Vec<CommitInfo> {
    let output = std::process::Command::new("git")
        .args([
            "log",
            "--name-only",
            // \x1e (ASCII record separator) marks each commit start.
            "--format=%x1e%H",
            &format!("-n{max_commits}"),
        ])
        .current_dir(workspace_root)
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .split('\x1e')
        .filter_map(|record| {
            let mut lines = record.lines().map(str::trim).filter(|l| !l.is_empty());
            let hash = lines.next()?.to_string();
            let files: Vec<String> = lines.map(str::to_string).collect();
            if files.is_empty() {
                return None;
            }
            Some(CommitInfo { hash, files })
        })
        .collect()
}

/// Temporal coupling mined from git history via the analysis engine
/// (`co_change::{compute_co_changes, co_changes_for_nodes}` over
/// [`fetch_commit_history`]). Pure read of `git log`; the graph maps file
/// paths to symbols.
pub fn co_change_report(
    graph: &AnalysisGraph,
    workspace_root: &Path,
    seed: Option<&ANodeId>,
    min_support: u32,
    max_commits: usize,
    top: usize,
) -> CoChangeReport {
    let commits = fetch_commit_history(workspace_root, max_commits);
    let seed_ref = seed.and_then(|id| graph.get_node(id)).map(symbol_ref);

    let result = match seed {
        Some(id) => co_changes_for_nodes(graph, &commits, std::slice::from_ref(id), min_support),
        None => compute_co_changes(graph, &commits, min_support),
    };

    let mut same_file_pair_count = 0usize;
    let mut pairs: Vec<CoChangePairSummary> = result
        .pairs
        .iter()
        .filter_map(|p| {
            let a = graph.get_node(&p.node_a)?;
            let b = graph.get_node(&p.node_b)?;
            if is_placeholder(a) || is_placeholder(b) {
                return None;
            }
            if a.file_path == b.file_path {
                same_file_pair_count += 1;
                return None;
            }
            let (mut a, mut b) = (symbol_ref(a), symbol_ref(b));
            if symbol_sort_key(&b) < symbol_sort_key(&a) {
                std::mem::swap(&mut a, &mut b);
            }
            Some(CoChangePairSummary {
                a,
                b,
                times_changed_together: p.times_changed_together,
                total_changes_a: p.total_changes_a,
                total_changes_b: p.total_changes_b,
                confidence: p.confidence,
            })
        })
        .collect();
    pairs.sort_by(|x, y| {
        y.confidence
            .total_cmp(&x.confidence)
            .then_with(|| y.times_changed_together.cmp(&x.times_changed_together))
            .then_with(|| symbol_sort_key(&x.a).cmp(&symbol_sort_key(&y.a)))
            .then_with(|| symbol_sort_key(&x.b).cmp(&symbol_sort_key(&y.b)))
    });
    let cross_file_pair_count = pairs.len();
    let truncated = pairs.len() > top;
    pairs.truncate(top);

    let note = if commits.is_empty() {
        "No git history available — the project is not a git repository, git is not on PATH, or \
         the history is empty. Co-change mining reads `git log --name-only`."
            .to_string()
    } else {
        "Co-change is mined from `git log --name-only` at file granularity: every symbol in a \
         touched file counts as changed. Same-file pairs are tautologically coupled and are \
         summarized in sameFilePairCount instead of listed."
            .to_string()
    };

    CoChangeReport {
        seed: seed_ref,
        commits_analyzed: commits.len(),
        max_commits,
        min_support,
        cross_file_pair_count,
        same_file_pair_count,
        truncated,
        pairs,
        note,
    }
}
