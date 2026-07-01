//! Stale-index banner and footer rendering.

use super::now_ms;
use crate::sync::PendingFile;

pub fn format_stale_banner(stale: &[PendingFile]) -> String {
    let now = now_ms();
    let lines: Vec<String> = stale
        .iter()
        .map(|p| {
            let age_ms = (now - p.last_seen_ms).max(0);
            let label = if p.indexing {
                "indexing in progress"
            } else {
                "pending sync"
            };
            format!("  - {} (edited {}ms ago, {})", p.path, age_ms, label)
        })
        .collect();
    format!(
        "Stale index notice: some files referenced below changed after the last index sync:\n{}\nRead those files directly for exact current content. Other files in this response are fresh.",
        lines.join("\n")
    )
}

/// Compact footer listing pending files that are NOT referenced in this
/// response.
pub fn format_stale_footer(stale: &[PendingFile]) -> String {
    const MAX: usize = 5;
    let now = now_ms();
    let shown = &stale[..stale.len().min(MAX)];
    let lines: Vec<String> = shown
        .iter()
        .map(|p| {
            let age_ms = (now - p.last_seen_ms).max(0);
            format!("  - {} (edited {}ms ago)", p.path, age_ms)
        })
        .collect();
    let more = if stale.len() > MAX {
        format!("\n  - and {} more", stale.len() - MAX)
    } else {
        String::new()
    };
    format!(
        "Note: {} file(s) elsewhere in this project are pending index sync but were not referenced above:\n{}{}",
        stale.len(),
        lines.join("\n"),
        more
    )
}
