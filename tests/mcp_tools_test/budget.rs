#[test]
fn returns_a_strictly_smaller_total_cap_for_small_projects_than_for_huge_ones() {
    let small = get_explore_output_budget(100);
    let huge = get_explore_output_budget(30000);
    assert!(small.max_output_chars < huge.max_output_chars);
    assert!(small.default_max_files < huge.default_max_files);
    assert!(small.max_chars_per_file < huge.max_chars_per_file);
}

#[test]
fn caps_total_output_well_under_8000_tokens_on_small_projects() {
    let small = get_explore_output_budget(100);
    assert!(small.max_output_chars <= 20000);
}

#[test]
fn caps_medium_large_projects_at_the_inline_tool_result_ceiling() {
    // A bigger single response gets externalized by the host to a file the
    // agent Reads back — so large repos get MORE CALLS, not a fatter response.
    let large = get_explore_output_budget(10000);
    assert!(large.max_output_chars <= 25000);
    assert!(large.max_output_chars >= 20000);
}

#[test]
fn uses_tier_breakpoints_matching_get_explore_budget() {
    // Very-tiny tier (<150 files) gets a tighter cap than small (150-499).
    let tier0a = get_explore_output_budget(50);
    let tier0b = get_explore_output_budget(149);
    assert_eq!(tier0a.max_output_chars, tier0b.max_output_chars);

    let tier1a = get_explore_output_budget(150);
    let tier1b = get_explore_output_budget(499);
    assert_eq!(tier1a.max_output_chars, tier1b.max_output_chars);
    // The <500 explore-call budget covers both very-tiny and small.
    assert_eq!(get_explore_budget(50), get_explore_budget(499));

    let tier2a = get_explore_output_budget(500);
    let tier2b = get_explore_output_budget(4999);
    assert_eq!(tier2a.max_output_chars, tier2b.max_output_chars);
    assert_eq!(get_explore_budget(500), get_explore_budget(4999));

    let tier3a = get_explore_output_budget(5000);
    let tier3b = get_explore_output_budget(14999);
    assert_eq!(tier3a.max_output_chars, tier3b.max_output_chars);

    // Small tiers step up (13k → 18k → 24k); medium and large SHARE the ~24k
    // inline ceiling — scaling now lives in the CALL budget.
    assert_ne!(tier0a.max_output_chars, tier1a.max_output_chars); // <150 vs <500
    assert_ne!(tier1a.max_output_chars, tier2a.max_output_chars); // <500 vs <5000
    assert_eq!(tier2a.max_output_chars, tier3a.max_output_chars); // <5000 == <15000
    assert!(get_explore_budget(5000) > get_explore_budget(4999)); // calls scale instead
}

#[test]
fn gates_off_meta_text_on_small_projects() {
    let small = get_explore_output_budget(100);
    assert!(!small.include_additional_files);
    assert!(!small.include_completeness_signal);
    assert!(!small.include_budget_note);
}

#[test]
fn keeps_all_meta_text_on_for_projects_that_earn_the_breadth_signal() {
    let medium = get_explore_output_budget(1000);
    assert!(medium.include_additional_files);
    assert!(medium.include_completeness_signal);
    assert!(medium.include_budget_note);
}

#[test]
fn keeps_the_relationships_section_on_for_medium_plus_tiers() {
    // Relationships dropped on <500 tiers; re-enabled at >=500.
    assert!(!get_explore_output_budget(50).include_relationships);
    assert!(get_explore_output_budget(1000).include_relationships);
    assert!(get_explore_output_budget(10000).include_relationships);
    assert!(get_explore_output_budget(30000).include_relationships);
}

#[test]
fn caps_the_per_file_header_symbol_list_more_tightly_on_small_projects() {
    let small = get_explore_output_budget(100);
    let huge = get_explore_output_budget(30000);
    assert!(small.max_symbols_in_file_header < huge.max_symbols_in_file_header);
    assert!(small.max_symbols_in_file_header > 0);
}

#[test]
fn uses_a_tighter_clustering_gap_threshold_on_small_projects() {
    let small = get_explore_output_budget(100);
    let huge = get_explore_output_budget(30000);
    assert!(small.gap_threshold <= huge.gap_threshold);
}

#[test]
fn handles_the_boundary_file_counts_exactly() {
    // 149 -> very-tiny, 150 -> small
    assert_eq!(
        get_explore_output_budget(149).max_output_chars,
        get_explore_output_budget(50).max_output_chars
    );
    assert_eq!(
        get_explore_output_budget(150).max_output_chars,
        get_explore_output_budget(200).max_output_chars
    );
    // 499 -> small, 500 -> medium
    assert_eq!(
        get_explore_output_budget(499).max_output_chars,
        get_explore_output_budget(200).max_output_chars
    );
    assert_eq!(
        get_explore_output_budget(500).max_output_chars,
        get_explore_output_budget(1000).max_output_chars
    );
    // 4999 -> medium, 5000 -> large
    assert_eq!(
        get_explore_output_budget(4999).max_output_chars,
        get_explore_output_budget(1000).max_output_chars
    );
    assert_eq!(
        get_explore_output_budget(5000).max_output_chars,
        get_explore_output_budget(10000).max_output_chars
    );
    // 14999 -> large, 15000 -> xlarge
    assert_eq!(
        get_explore_output_budget(14999).max_output_chars,
        get_explore_output_budget(10000).max_output_chars
    );
    assert_eq!(
        get_explore_output_budget(15000).max_output_chars,
        get_explore_output_budget(30000).max_output_chars
    );
}

// =============================================================================
// codegraph_explore output respects the adaptive budget — e2e (#185)
// =============================================================================
