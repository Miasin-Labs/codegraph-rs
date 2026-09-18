//! Small formatting helpers for `codegraph projects`.

use std::path::Path;

use codegraph::atlas::{LanguageCount, ProjectStatus};

use super::super::{green, red, yellow};

/// `~/…` for paths under the home directory.
pub(super) fn short_path(path: &Path) -> String {
    match dirs::home_dir().and_then(|home| path.strip_prefix(home).ok().map(Path::to_path_buf)) {
        Some(rel) if rel.as_os_str().is_empty() => "~".to_owned(),
        Some(rel) => format!("~/{}", rel.display()),
        None => path.display().to_string(),
    }
}

/// `1.2 GB`-style size.
pub(super) fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    #[allow(clippy::cast_precision_loss)]
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// `3m ago`, `2h ago`, `5d ago`.
pub(super) fn ago(epoch_ms: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let secs = (now - epoch_ms).max(0) / 1000;
    match secs {
        0..60 => "just now".to_owned(),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// `rust 1046 · toml 6` (the first `limit` languages).
pub(super) fn languages_line(languages: &[LanguageCount], limit: usize) -> String {
    let mut shown: Vec<String> = languages
        .iter()
        .take(limit)
        .map(|l| format!("{} {}", l.language, l.files))
        .collect();
    if languages.len() > limit {
        shown.push(format!("+{}", languages.len() - limit));
    }
    shown.join(" · ")
}

pub(super) fn status_label(status: ProjectStatus) -> String {
    match status {
        ProjectStatus::Ok => green("ok"),
        ProjectStatus::StaleSchema => yellow("stale-schema"),
        ProjectStatus::Missing | ProjectStatus::Unreadable => red(status.as_str()),
    }
}
