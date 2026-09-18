//! Notices as the model sees them: why a result may not match the code on
//! disk, carried in the result itself. Hosts rarely show `_meta` to the
//! model, so `_meta.notices` alone is a warning nobody reads.
//!
//! One entry per kind, a bounded list of paths, and no per-file prose: the
//! per-file detail (age, sync status) stays in `_meta.notices`.

use std::collections::{BTreeMap, HashSet};

use serde::Serialize;
use serde_json::{Value, json};

use super::super::schema::{NoticeKind, ToolNotice};
use super::is_zero;

/// Paths one notice lists; the rest are counted in `filesOmitted`. Notices
/// ride on top of a payload the tool already shaped to the output budget, so
/// they must stay small however many files are pending (a branch switch can
/// leave thousands).
const MAX_NOTICE_FILES: usize = 10;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(in crate::mcp::tools) struct NoticeOutput {
    pub kind: NoticeKind,
    pub message: String,
    /// Project-relative paths the notice is about, those this result
    /// mentions first.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// More paths the notice is about than `files` lists.
    #[serde(skip_serializing_if = "is_zero")]
    pub files_omitted: usize,
}

/// One entry per kind, whole-index kinds first. Notices of one kind share a
/// message (the first one's); their files are merged in the order listed.
pub(in crate::mcp::tools) fn notice_outputs(notices: &[ToolNotice]) -> Vec<NoticeOutput> {
    let mut by_kind: BTreeMap<NoticeKind, (String, Vec<String>, HashSet<String>)> = BTreeMap::new();
    for notice in notices {
        let (_, files, seen) = by_kind
            .entry(notice.kind)
            .or_insert_with(|| (notice.message.clone(), Vec::new(), HashSet::new()));
        for file in &notice.files {
            if seen.insert(file.path.clone()) {
                files.push(file.path.clone());
            }
        }
    }
    by_kind
        .into_iter()
        .map(|(kind, (message, mut files, _))| {
            let files_omitted = files.len().saturating_sub(MAX_NOTICE_FILES);
            files.truncate(MAX_NOTICE_FILES);
            NoticeOutput {
                kind,
                message,
                files,
                files_omitted,
            }
        })
        .collect()
}

/// Put `notices` into an object payload right after its `schemaVersion` and
/// `kind`, so a model reading the payload meets them before the results.
/// Nothing changes when there are none.
///
/// # Errors
/// Returns an error if the notices cannot be serialized.
pub(in crate::mcp::tools) fn attach_notices(
    payload: &mut Value,
    notices: &[NoticeOutput],
) -> serde_json::Result<()> {
    if notices.is_empty() {
        return Ok(());
    }
    let Some(object) = payload.as_object_mut() else {
        return Ok(());
    };
    let notices = serde_json::to_value(notices)?;
    let (head, tail): (Vec<_>, Vec<_>) = std::mem::take(object)
        .into_iter()
        .filter(|(key, _)| key != "notices")
        .partition(|(key, _)| key == "schemaVersion" || key == "kind");
    object.extend(head);
    object.insert("notices".into(), notices);
    object.extend(tail);
    Ok(())
}

/// The notices as the first lines of a text result, one `⚠️` line each.
pub(in crate::mcp::tools) fn notice_banner(notices: &[NoticeOutput]) -> Option<String> {
    if notices.is_empty() {
        return None;
    }
    let lines: Vec<String> = notices
        .iter()
        .map(|notice| {
            let mut line = format!("⚠️ {}", notice.message);
            if !notice.files.is_empty() {
                line.push_str(" Files: ");
                line.push_str(&notice.files.join(", "));
                if notice.files_omitted > 0 {
                    line.push_str(&format!(" (+{} more)", notice.files_omitted));
                }
            }
            line
        })
        .collect();
    Some(lines.join("\n"))
}

/// The `notices` property every structured output schema declares.
pub(in crate::mcp::tools) fn notices_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "kind": { "enum": NoticeKind::ALL.map(NoticeKind::as_str) },
                "message": { "type": "string" },
                "files": { "type": "array", "items": { "type": "string" } },
                "filesOmitted": { "type": "integer" }
            },
            "required": ["kind", "message"]
        }
    })
}
