use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Language Support')
// =============================================================================

#[test]
fn language_support_reports_supported_languages() {
    assert!(is_language_supported(Language::Typescript));
    assert!(is_language_supported(Language::Python));
    assert!(is_language_supported(Language::Go));
    assert!(!is_language_supported(Language::Unknown));
}

#[test]
fn language_support_lists_all_supported_languages() {
    let languages = get_supported_languages();
    for lang in [
        Language::Typescript,
        Language::Javascript,
        Language::Python,
        Language::Go,
        Language::Rust,
        Language::Java,
        Language::Csharp,
        Language::Php,
        Language::Ruby,
        Language::Swift,
        Language::Kotlin,
        Language::Dart,
    ] {
        assert!(languages.contains(&lang), "missing {lang}");
    }
}
