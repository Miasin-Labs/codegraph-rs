use super::*;

/// codegraph analyze schema <kind>
///
/// Prints the engine's JSON Schema document verbatim (it is already JSON, so
/// `--json` prints the same bytes). Works without an initialized project.
pub(crate) fn cmd_analyze_schema(kind: &str, _json: bool) {
    match analysis_reports::schema_text(kind) {
        Ok(schema) => println!("{schema}"),
        Err(msg) => {
            error_msg(&format!("analyze schema failed: {msg}"));
            process::exit(1);
        }
    }
}
