use super::*;

// =============================================================================
// JSON report envelope
// =============================================================================

/// Version of every `analyze … --json` payload. Bumped when a report field is
/// renamed/removed or its semantics change; additive fields do not bump it
/// (same policy as `codegraph_analysis::schema::SCHEMA_VERSION`).
pub const REPORT_SCHEMA_VERSION: u32 = 1;

/// Versioned envelope around every `analyze` JSON report:
/// `{"schemaVersion": N, "kind": "<kind>", "data": …}`.
///
/// Mirrors the engine's `codegraph_analysis::schema::Envelope` wire shape.
/// The engine type is not reused because its `kind` discriminator is the
/// closed [`PayloadKind`] enum (four engine payloads only) — host report
/// kinds are open strings instead.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportEnvelope<T: Serialize> {
    pub schema_version: u32,
    pub kind: &'static str,
    pub data: T,
}

impl<T: Serialize> ReportEnvelope<T> {
    pub fn new(kind: &'static str, data: T) -> Self {
        Self {
            schema_version: REPORT_SCHEMA_VERSION,
            kind,
            data,
        }
    }
}

// =============================================================================
