use std::path::Path;

use codegraph::sync::worktree::{
    WorktreeIndexMismatch,
    detect_worktree_index_mismatch,
    worktree_mismatch_notice,
    worktree_write_refusal,
};

use super::{PathBuf, error_msg, is_initialized, lexical_resolve, process};

/// Nearest directory at or above `absolute` holding an index, or `absolute`
/// itself when there is none (the command then fails with a helpful error).
fn nearest_index_root(absolute: &Path) -> PathBuf {
    // Walk up to the nearest initialized ancestor (the TS loop checks every
    // parent up to and including the filesystem root).
    absolute
        .ancestors()
        .find(|ancestor| is_initialized(ancestor))
        .unwrap_or(absolute)
        .to_path_buf()
}

/// Resolution for commands that READ the index: the nearest index at or above
/// the path. One that belongs to a different git checkout (a worktree nested
/// in the main checkout finds the main checkout's) is still read, with the
/// worktree notice on stderr so stdout stays machine-readable.
pub(crate) fn resolve_project_path(path_arg: Option<&str>) -> PathBuf {
    let start = resolve_absolute(path_arg);
    let root = nearest_index_root(&start);
    if let Some(mismatch) = detect_worktree_index_mismatch(&start, &root) {
        eprintln!("{}", worktree_mismatch_notice(&mismatch));
    }
    root
}

/// [`resolve_project_path`] without the notice, for callers that report the
/// mismatch themselves (`status`) or don't read the index (daemon control).
pub(crate) fn resolve_project_path_quiet(path_arg: Option<&str>) -> PathBuf {
    nearest_index_root(&resolve_absolute(path_arg))
}

/// Resolution for commands that WRITE the index (`sync`, `uninit`, `unlock`,
/// a bare `index`): the nearest index, unless it belongs to a different git
/// checkout than the path — then `Err`, and the caller must leave it alone.
pub(crate) fn resolve_writable_project_path(
    path_arg: Option<&str>,
) -> Result<PathBuf, WorktreeIndexMismatch> {
    let start = resolve_absolute(path_arg);
    let root = nearest_index_root(&start);
    match detect_worktree_index_mismatch(&start, &root) {
        Some(mismatch) => Err(mismatch),
        None => Ok(root),
    }
}

/// Exit after a write command found only another checkout's index. `quiet`
/// callers (git hooks, background syncs) exit without output.
pub(crate) fn refuse_foreign_index(mismatch: &WorktreeIndexMismatch, quiet: bool) -> ! {
    if !quiet {
        error_msg(&worktree_write_refusal(mismatch));
    }
    process::exit(1);
}

/// `path.resolve(pathArg || process.cwd())` parity (no walk-up).
pub(crate) fn resolve_absolute(path_arg: Option<&str>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    match path_arg {
        Some(p) if !p.is_empty() => lexical_resolve(&cwd, p),
        _ => cwd,
    }
}

/// Path resolution for `codegraph index`.
///
/// `index` writes an index at the location it is pointed at, so an explicitly
/// named path is resolved literally — never substituted for an initialized
/// ancestor. Silently walking up rebuilt an unrelated ancestor's index (up to
/// the filesystem root) and reported success, so the caller could not tell the
/// named directory had been ignored (upstream #1524). A bare `index` (no path)
/// keeps the query-style convenience of resolving the nearest initialized
/// project from the current directory — but only within the current git
/// checkout: from a worktree nested in the main checkout it must not rebuild
/// the main checkout's index.
pub(crate) fn resolve_index_path(path_arg: Option<&str>) -> Result<PathBuf, WorktreeIndexMismatch> {
    match path_arg {
        Some(p) if !p.is_empty() => Ok(resolve_absolute(Some(p))),
        _ => resolve_writable_project_path(None),
    }
}

/// Path resolution for `codegraph serve --path`: the index root when the path
/// is inside the checkout that owns it. A path inside a different checkout
/// stays literal, so the server knows where the session is — it then reads
/// the borrowed index with the worktree notice, never watching or syncing it,
/// and never joins that checkout's daemon.
pub(crate) fn resolve_serve_path(path_arg: &str) -> PathBuf {
    resolve_writable_project_path(Some(path_arg))
        .unwrap_or_else(|_| resolve_absolute(Some(path_arg)))
}
