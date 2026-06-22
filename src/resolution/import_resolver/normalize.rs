//! Lexical POSIX path helpers used by import resolution.

/// Lexical `path.dirname` for '/'-separated paths.
pub(super) fn posix_dirname(p: &str) -> String {
    match p.rfind('/') {
        Some(0) => "/".to_string(),
        Some(idx) => p[..idx].to_string(),
        None => ".".to_string(),
    }
}

/// Lexical `path.join(a, b)` for '/'-separated paths (no normalization).
pub(super) fn join_posix(a: &str, b: &str) -> String {
    if a.is_empty() {
        b.to_string()
    } else if b.is_empty() {
        a.to_string()
    } else {
        format!("{}/{}", a.trim_end_matches('/'), b)
    }
}

/// Normalize `.`/`..` segments lexically. Keeps a leading `/` (absolute)
/// and keeps leading `..` segments for relative paths — an import that
/// escapes the project root yields a `../…` candidate that then fails
/// `fileExists`, matching the TS `path.resolve` + `path.relative` outcome.
pub(super) fn normalize_segments(p: &str) -> String {
    let absolute = p.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => {}
            ".." => match stack.last() {
                Some(&"..") | None => {
                    if !absolute {
                        stack.push("..");
                    }
                    // absolute: clamp at the root, like `path.resolve`.
                }
                Some(_) => {
                    stack.pop();
                }
            },
            s => stack.push(s),
        }
    }
    let joined = stack.join("/");
    if absolute {
        format!("/{joined}")
    } else {
        joined
    }
}

/// Lexical `path.relative(from, to)` for '/'-separated paths that share
/// the same lexical base (both produced from the project root).
pub(super) fn relative_posix(from: &str, to: &str) -> String {
    if from == to {
        return String::new();
    }
    let from_parts: Vec<&str> = from
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let to_parts: Vec<&str> = to
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    let mut common = 0;
    while common < from_parts.len()
        && common < to_parts.len()
        && from_parts[common] == to_parts[common]
    {
        common += 1;
    }
    // One ".." per remaining `from` segment (TS: push '..' in a loop).
    let mut parts: Vec<&str> = vec![".."; from_parts.len() - common];
    parts.extend(&to_parts[common..]);
    parts.join("/")
}
