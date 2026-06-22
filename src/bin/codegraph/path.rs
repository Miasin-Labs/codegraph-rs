use super::*;

pub(crate) fn resolve_project_path(path_arg: Option<&str>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let absolute = match path_arg {
        Some(p) if !p.is_empty() => lexical_resolve(&cwd, p),
        _ => cwd,
    };

    // If exact path is initialized (has codegraph.db), use it
    if is_initialized(&absolute) {
        return absolute;
    }

    // Walk up to find nearest parent with CodeGraph initialized (the TS loop
    // checks every parent up to and including the filesystem root).
    for ancestor in absolute.ancestors().skip(1) {
        if is_initialized(ancestor) {
            return ancestor.to_path_buf();
        }
    }

    // Not found - return original path (will fail later with helpful error)
    absolute
}

/// `path.resolve(pathArg || process.cwd())` parity (no walk-up).
pub(crate) fn resolve_absolute(path_arg: Option<&str>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    match path_arg {
        Some(p) if !p.is_empty() => lexical_resolve(&cwd, p),
        _ => cwd,
    }
}
