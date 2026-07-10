use rusqlite::Row;

use crate::types::{
    Edge,
    EdgeKind,
    FileRecord,
    Language,
    Node,
    NodeKind,
    Provenance,
    UnresolvedReference,
    Visibility,
};
use crate::utils::safe_json_parse;

fn conv_err(msg: impl Into<String>) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, msg.into().into())
}

fn parse_visibility(s: Option<String>) -> Option<Visibility> {
    match s.as_deref() {
        Some("public") => Some(Visibility::Public),
        Some("private") => Some(Visibility::Private),
        Some("protected") => Some(Visibility::Protected),
        Some("internal") => Some(Visibility::Internal),
        _ => None,
    }
}

pub(super) fn visibility_str(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Private => "private",
        Visibility::Protected => "protected",
        Visibility::Internal => "internal",
    }
}

/// Convert database row to Node object (TS `rowToNode`).
pub(super) fn node_from_row(row: &Row<'_>) -> rusqlite::Result<Node> {
    let kind_s: String = row.get("kind")?;
    let kind: NodeKind = kind_s.parse().map_err(|e: String| conv_err(e))?;
    let lang_s: String = row.get("language")?;
    // Unknown language strings fall back to `unknown` (TS casts blindly).
    let language: Language = lang_s.parse().unwrap_or(Language::Unknown);
    let visibility: Option<String> = row.get("visibility")?;
    let decorators: Option<String> = row.get("decorators")?;
    let type_parameters: Option<String> = row.get("type_parameters")?;
    Ok(Node {
        id: row.get("id")?,
        kind,
        name: row.get("name")?,
        qualified_name: row.get("qualified_name")?,
        file_path: row.get("file_path")?,
        language,
        start_line: row.get("start_line")?,
        end_line: row.get("end_line")?,
        start_column: row.get("start_column")?,
        end_column: row.get("end_column")?,
        start_byte: row.get("start_byte")?,
        end_byte: row.get("end_byte")?,
        // SQLite INTEGER is i64; addresses are stored as i64 and read back as
        // u64 (real virtual addresses fit in 48 bits, so the cast is lossless).
        address: row.get::<_, Option<i64>>("address")?.map(|v| v as u64),
        size: row.get("size")?,
        docstring: row.get("docstring")?,
        signature: row.get("signature")?,
        return_type: row.get("return_type")?,
        visibility: parse_visibility(visibility),
        is_exported: Some(row.get::<_, i64>("is_exported")? == 1),
        is_async: Some(row.get::<_, i64>("is_async")? == 1),
        is_static: Some(row.get::<_, i64>("is_static")? == 1),
        is_abstract: Some(row.get::<_, i64>("is_abstract")? == 1),
        decorators: decorators.and_then(|s| safe_json_parse::<Option<Vec<String>>>(&s, None)),
        type_parameters: type_parameters
            .and_then(|s| safe_json_parse::<Option<Vec<String>>>(&s, None)),
        updated_at: lenient_i64(row, "updated_at")?,
    })
}

/// Convert database row to Edge object (TS `rowToEdge`).
pub(super) fn edge_from_row(row: &Row<'_>) -> rusqlite::Result<Edge> {
    let kind_s: String = row.get("kind")?;
    let kind: EdgeKind = kind_s.parse().map_err(|e: String| conv_err(e))?;
    let metadata: Option<String> = row.get("metadata")?;
    let provenance: Option<String> = row.get("provenance")?;
    Ok(Edge {
        source: row.get("source")?,
        target: row.get("target")?,
        kind,
        metadata: metadata
            .and_then(|s| safe_json_parse::<Option<crate::types::Metadata>>(&s, None)),
        line: row.get("line")?,
        column: row.get("col")?,
        provenance: provenance.and_then(|s| s.parse::<Provenance>().ok()),
    })
}

/// Convert database row to UnresolvedReference (TS `rowToUnresolvedReference`).
pub(super) fn unresolved_from_row(row: &Row<'_>) -> rusqlite::Result<UnresolvedReference> {
    let kind_s: String = row.get("reference_kind")?;
    let reference_kind: EdgeKind = kind_s.parse().map_err(|e: String| conv_err(e))?;
    let candidates: Option<String> = row.get("candidates")?;
    let metadata: Option<String> = row.get("metadata")?;
    let lang_s: String = row.get("language")?;
    Ok(UnresolvedReference {
        from_node_id: row.get("from_node_id")?,
        reference_name: row.get("reference_name")?,
        reference_kind,
        line: row.get("line")?,
        column: row.get("col")?,
        candidates: candidates.and_then(|s| safe_json_parse::<Option<Vec<String>>>(&s, None)),
        metadata: metadata
            .and_then(|s| safe_json_parse::<Option<crate::types::Metadata>>(&s, None)),
        file_path: Some(row.get("file_path")?),
        language: Some(lang_s.parse().unwrap_or(Language::Unknown)),
    })
}

/// Read an INTEGER-ish column leniently, accepting REAL by truncation.
///
/// The TS implementation stores plain JS numbers, and some of them are
/// fractional — most notably `files.modified_at`, which Node fills from
/// `fs.statSync().mtimeMs` (a float). A TS-written database therefore holds
/// REAL where the Rust port expects INTEGER; truncate exactly like a JS
/// reader coerced back through `Math.trunc` would, so the two
/// implementations can share one `.codegraph/codegraph.db`.
pub(super) fn lenient_i64(row: &Row<'_>, col: &str) -> rusqlite::Result<i64> {
    use rusqlite::types::ValueRef;
    match row.get_ref(col)? {
        ValueRef::Integer(i) => Ok(i),
        ValueRef::Real(f) => Ok(f as i64),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            other.data_type(),
            format!("expected INTEGER or REAL for column {col}").into(),
        )),
    }
}

/// Convert database row to FileRecord object (TS `rowToFileRecord`).
pub(super) fn file_from_row(row: &Row<'_>) -> rusqlite::Result<FileRecord> {
    let lang_s: String = row.get("language")?;
    let errors: Option<String> = row.get("errors")?;
    Ok(FileRecord {
        path: row.get("path")?,
        content_hash: row.get("content_hash")?,
        language: lang_s.parse().unwrap_or(Language::Unknown),
        size: lenient_i64(row, "size")?.max(0) as u64,
        modified_at: lenient_i64(row, "modified_at")?,
        indexed_at: lenient_i64(row, "indexed_at")?,
        node_count: row.get("node_count")?,
        errors: errors
            .and_then(|s| safe_json_parse::<Option<Vec<crate::types::ExtractionError>>>(&s, None)),
    })
}

pub(super) fn placeholders(n: usize) -> String {
    let mut s = String::with_capacity(n * 2);
    for i in 0..n {
        if i > 0 {
            s.push(',');
        }
        s.push('?');
    }
    s
}
