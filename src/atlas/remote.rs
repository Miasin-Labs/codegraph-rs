//! Git remote normalization: one spelling per repository, never carrying
//! credentials.
//!
//! `https://user:token@GitHub.com:443/Owner/Repo.git`, `git@github.com:owner/repo`
//! and `ssh://git@github.com/owner/repo/` all become `github.com/owner/repo`,
//! so clones made over different transports group together. Userinfo,
//! ports, query strings and fragments are dropped — the only places a URL
//! carries a secret — and the result is run through the history redactor as
//! a last line of defence.

/// Hosts whose repository paths are case-insensitive.
const CASE_INSENSITIVE_HOSTS: &[&str] = &["github.com", "gitlab.com", "bitbucket.org"];

/// Normalize a remote URL (`None` for an empty one).
pub fn normalize_remote(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let normalized = if let Some((scheme, rest)) = raw.split_once("://") {
        from_url(&scheme.to_ascii_lowercase(), rest)
    } else if let Some((host, path)) = scp_like(strip_scp_user(raw)) {
        join_host(host, path)
    } else {
        // A local path (`/srv/git/repo.git`, `../upstream`).
        trim_repo_path(raw).to_owned()
    };
    let (safe, _) = crate::history::redact(&normalized);
    (!safe.is_empty()).then_some(safe)
}

/// `scheme://[userinfo@]host[:port]/path[?query][#fragment]`.
fn from_url(scheme: &str, rest: &str) -> String {
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let rest = strip_userinfo(rest);
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    if scheme == "file" {
        // `file:///abs/path` (empty authority) or `file://host/path`.
        return format!("file:///{}", trim_repo_path(path));
    }
    join_host(authority, path)
}

/// Drop `userinfo@` from `authority/path`. Userinfo ends at the LAST `@`
/// that a host (and path) follows, so a password with a raw `@` or `/` in
/// it is dropped whole; an `@` inside a path (nothing host-like after it,
/// e.g. `host/repo@v2`) is kept.
fn strip_userinfo(rest: &str) -> &str {
    match rest.rfind('@') {
        Some(at) if rest[at + 1..].contains('/') || !rest[..at].contains('/') => &rest[at + 1..],
        _ => rest,
    }
}

/// Drop `user[:secret]@` ahead of an scp-style `host:path` (an `@` after
/// the first `/` belongs to a path).
fn strip_scp_user(raw: &str) -> &str {
    match raw.rfind('@') {
        Some(at) if !raw[..at].contains('/') => &raw[at + 1..],
        _ => raw,
    }
}

/// `[user@]host:path` — scp-style, when no `/` precedes the first `:`.
fn scp_like(raw: &str) -> Option<(&str, &str)> {
    let (left, path) = raw.split_once(':')?;
    let is_drive = left.len() == 1 && left.chars().all(|c| c.is_ascii_alphabetic());
    (!left.is_empty() && !left.contains('/') && !left.contains('\\') && !is_drive)
        .then_some((left, path))
}

/// `host/path` from an authority (userinfo and port dropped) and a path.
fn join_host(authority: &str, path: &str) -> String {
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = strip_port(host).to_ascii_lowercase();
    let path = trim_repo_path(path.trim_start_matches('/'));
    let path = if CASE_INSENSITIVE_HOSTS.contains(&host.as_str()) {
        path.to_ascii_lowercase()
    } else {
        path.to_owned()
    };
    if path.is_empty() {
        host
    } else {
        format!("{host}/{path}")
    }
}

fn strip_port(host: &str) -> &str {
    if host.starts_with('[') {
        // IPv6 literal: `[::1]:22`.
        return host
            .split_once(']')
            .map_or(host, |(ip, _)| ip)
            .trim_start_matches('[');
    }
    host.split_once(':').map_or(host, |(name, _)| name)
}

/// Drop trailing `/` and a `.git` suffix.
fn trim_repo_path(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.strip_suffix(".git")
        .unwrap_or(path)
        .trim_end_matches('/')
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn transports_of_one_repository_normalize_alike() {
        for raw in [
            "https://github.com/Owner/Repo.git",
            "https://github.com/owner/repo",
            "http://github.com/owner/repo/",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo",
            "ssh://git@GitHub.com:22/owner/repo.git",
            "git://github.com/owner/repo.git",
        ] {
            assert_eq!(
                normalize_remote(raw).as_deref(),
                Some("github.com/owner/repo"),
                "{raw}"
            );
        }
    }

    /// Fake vendor-shaped tokens, assembled at runtime so secret scanners
    /// don't flag the test source.
    pub(crate) fn fake_token(prefix: &str) -> String {
        format!("{prefix}{}", "0123456789abcdefghijklmnopqrstuvwxyzAB")
    }

    #[test]
    fn credentials_never_survive_normalization() {
        let github = fake_token(&["gh", "p_"].concat());
        let gitlab = fake_token(&["gl", "pat-"].concat());
        for raw in [
            format!("https://user:{github}@github.com/o/r.git"),
            "https://x-access-token:s3cr3t-t0ken@github.com/o/r".to_owned(),
            format!("https://oauth2:{gitlab}@gitlab.com/o/r.git"),
            "https://user:p@ss@word@github.com/o/r.git".to_owned(),
            "https://user:pa/ssword@github.com/o/r.git".to_owned(),
            "https://github.com/o/r.git?access_token=s3cr3t-t0ken#frag".to_owned(),
            "https://token@github.com/o/r".to_owned(),
            "user:s3cr3t-t0ken@github.com:o/r.git".to_owned(),
        ] {
            let normalized = normalize_remote(&raw).unwrap();
            assert!(
                !normalized.contains("s3cr3t")
                    && !normalized.contains("0123456789abcdef")
                    && !normalized.contains("pat-")
                    && !normalized.contains('@')
                    && !normalized.contains("word"),
                "{raw} -> {normalized}"
            );
            assert!(normalized.ends_with("/o/r"), "{raw} -> {normalized}");
        }
    }

    #[test]
    fn hosts_ports_and_local_paths() {
        assert_eq!(
            normalize_remote("https://git.example.com:8443/Team/Proj.git").as_deref(),
            Some("git.example.com/Team/Proj")
        );
        assert_eq!(
            normalize_remote("ssh://git@[::1]:2222/srv/repo.git").as_deref(),
            Some("::1/srv/repo")
        );
        assert_eq!(
            normalize_remote("/srv/git/repo.git").as_deref(),
            Some("/srv/git/repo")
        );
        assert_eq!(
            normalize_remote("file:///srv/git/repo.git/").as_deref(),
            Some("file:///srv/git/repo")
        );
        assert_eq!(normalize_remote("  "), None);
        assert_eq!(
            normalize_remote("C:/src/repo").as_deref(),
            Some("C:/src/repo")
        );
    }
}
