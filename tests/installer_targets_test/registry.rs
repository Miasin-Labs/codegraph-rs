use crate::support::*;

// ===========================================================================
// Installer targets — registry
// ===========================================================================

#[test]
fn registry_get_target_returns_right_target_for_each_id() {
    for id in [
        "claude",
        "cursor",
        "codex",
        "opencode",
        "hermes",
        "gemini",
        "antigravity",
        "kiro",
    ] {
        assert_eq!(get_target(id).map(|t| t.id().as_str()), Some(id));
    }
    assert!(get_target("not-a-real-target").is_none());
}

#[test]
fn registry_resolve_target_flag_handles_all_none_csv() {
    // (the `auto` arm probes the real environment; covered implicitly
    // by the fallback test below)
    let _env = TestEnv::new();
    assert!(
        resolve_target_flag("none", Location::Global)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        resolve_target_flag("all", Location::Global).unwrap().len(),
        ALL_TARGETS.len()
    );
    let csv = resolve_target_flag("claude,cursor", Location::Global).unwrap();
    let ids: Vec<&str> = csv.iter().map(|t| t.id().as_str()).collect();
    assert_eq!(ids, vec!["claude", "cursor"]);
}

#[test]
fn registry_resolve_target_flag_errors_on_unknown_id() {
    let err = match resolve_target_flag("claude,bogus", Location::Global) {
        Ok(_) => panic!("expected an error for an unknown --target id"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("Unknown --target"));
}
