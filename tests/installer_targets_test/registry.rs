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
        "copilot-vscode",
        "copilot-cli",
        "copilot-jetbrains",
        "prime",
        "pi",
    ] {
        assert_eq!(get_target(id).map(|t| t.id().as_str()), Some(id));
    }
    assert!(get_target("not-a-real-target").is_none());
}

#[test]
fn registry_resolve_all_and_csv_include_copilot_targets_in_source_order() {
    let _env = TestEnv::new();
    let all: Vec<&str> = resolve_target_flag("all", Location::Global)
        .unwrap()
        .iter()
        .map(|target| target.id().as_str())
        .collect();
    assert!(all.ends_with(&["copilot-jetbrains", "prime", "pi",]));

    let csv = resolve_target_flag(
        "copilot-vscode,copilot-cli,copilot-jetbrains",
        Location::Global,
    )
    .unwrap();
    assert_eq!(
        csv.iter()
            .map(|target| target.id().as_str())
            .collect::<Vec<_>>(),
        vec!["copilot-vscode", "copilot-cli", "copilot-jetbrains"]
    );
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
