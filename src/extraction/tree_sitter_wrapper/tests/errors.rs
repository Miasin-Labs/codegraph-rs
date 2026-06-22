use super::super::TreeSitterExtractor;
use crate::types::Language;

#[test]
fn unsupported_language_returns_error_result() {
    let result =
        TreeSitterExtractor::new("file.unknown", "x", Some(Language::Unknown), None).extract();
    assert_eq!(result.nodes.len(), 0);
    assert_eq!(result.errors.len(), 1);
    assert_eq!(result.errors[0].message, "Unsupported language: unknown");
    assert_eq!(
        result.errors[0].code.as_deref(),
        Some("unsupported_language")
    );
}
