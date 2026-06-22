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
        "⚠️ Some files referenced below were edited since the last index sync — their codegraph entries may be stale:\n{}\nFor accurate content of those specific files, Read them directly. The rest of this response is fresh.",
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
        format!("\n  - …and {} more", stale.len() - MAX)
    } else {
        String::new()
    };
    format!(
        "(Note: {} file(s) elsewhere in this project are pending index sync but were not referenced above:\n{}{})",
        stale.len(),
        lines.join("\n"),
        more
    )
}
