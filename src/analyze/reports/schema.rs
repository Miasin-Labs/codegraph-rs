use super::{PayloadKind, json_schema_for};

// =============================================================================
// analyze schema
// =============================================================================

/// The payload kinds `analyze schema` accepts, with their engine enum values.
const SCHEMA_KINDS: [(&str, PayloadKind); 4] = [
    ("query_result", PayloadKind::QueryResult),
    ("entrypoint_summary", PayloadKind::EntrypointSummary),
    ("context_result", PayloadKind::ContextResult),
    ("formatted_output", PayloadKind::FormattedOutput),
];

/// JSON Schema (draft-07) for one of the engine's stabilised payload kinds
/// (engine entry point: `schema::json_schema_for`). The returned text is the
/// engine's own schema document, printed verbatim. Unknown kinds list the
/// accepted names.
pub fn schema_text(kind: &str) -> Result<&'static str, String> {
    let normalized = kind.trim().to_ascii_lowercase().replace('-', "_");
    SCHEMA_KINDS
        .iter()
        .find(|(name, _)| *name == normalized)
        .map(|(_, payload)| json_schema_for(*payload))
        .ok_or_else(|| {
            let known: Vec<&str> = SCHEMA_KINDS.iter().map(|(name, _)| *name).collect();
            format!(
                "unknown schema kind \"{kind}\" — known kinds: {}",
                known.join(", ")
            )
        })
}
