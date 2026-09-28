//! SARIF 2.1.0, both ways: [`read`] parses another tool's results (CodeQL's,
//! for `codegraph analyze codeql`) into plain records, [`write`] emits
//! codegraph's own findings so GitHub code scanning and other SARIF
//! consumers can ingest them (`analyze bugs|rules|codeql --sarif <file>`).
//!
//! Spec: <https://docs.oasis-open.org/sarif/sarif/v2.1.0/sarif-v2.1.0.html>.
//! Both sides use `serde_json::Value` rather than a typed model: SARIF is
//! large and mostly optional, and a reader must tolerate what it does not
//! know.

pub mod read;
pub mod write;

/// The `$schema` URI every emitted log names.
pub const SCHEMA_URI: &str = "https://json.schemastore.org/sarif-2.1.0.json";
/// The SARIF version emitted and accepted.
pub const VERSION: &str = "2.1.0";

/// CWE ids (`CWE-89`) named by a list of tags: SARIF/CodeQL style
/// (`external/cwe/cwe-089`) and codegraph rule style (`CWE-89`), leading
/// zeros dropped, deduplicated in order.
pub fn cwes_of_tags<'a>(tags: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let lower = tag.to_ascii_lowercase();
        let digits = lower
            .strip_prefix("external/cwe/cwe-")
            .or_else(|| lower.strip_prefix("cwe-"));
        let Some(n) = digits.and_then(|d| d.trim().parse::<u32>().ok()) else {
            continue;
        };
        let cwe = format!("CWE-{n}");
        if !out.contains(&cwe) {
            out.push(cwe);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cwe_tags_read_in_both_styles() {
        assert_eq!(
            cwes_of_tags([
                "security",
                "external/cwe/cwe-089",
                "CWE-89",
                "cwe-564",
                "external/cwe/cwe-x"
            ]),
            ["CWE-89", "CWE-564"]
        );
    }
}
