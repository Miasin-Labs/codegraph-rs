use crate::extraction_test::fixture::*;

// =============================================================================
// describe('Path Normalization')
// =============================================================================

#[test]
fn path_normalization_converts_backslashes_to_forward_slashes() {
    assert_eq!(
        normalize_path("gui\\node_modules\\foo"),
        "gui/node_modules/foo"
    );
    assert_eq!(
        normalize_path("src\\components\\Button.tsx"),
        "src/components/Button.tsx"
    );
}

#[test]
fn path_normalization_leaves_forward_slash_paths_unchanged() {
    assert_eq!(
        normalize_path("src/components/Button.tsx"),
        "src/components/Button.tsx"
    );
}

#[test]
fn path_normalization_handles_empty_string() {
    assert_eq!(normalize_path(""), "");
}
