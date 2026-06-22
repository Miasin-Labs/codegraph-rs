use crate::support::*;

// ===========================================================================
// Installer — uninstall_targets sweep (codegraph uninstall)
// ===========================================================================

#[test]
fn sweep_removes_every_installed_agent_global() {
    let _env = TestEnv::new();
    for t in ALL_TARGETS {
        if t.supports_location(Location::Global) {
            t.install(Location::Global, &auto_allow());
        }
    }

    let reports = uninstall_targets(&ALL_TARGETS, Location::Global);

    for t in ALL_TARGETS {
        let r = reports.iter().find(|x| x.id == t.id()).unwrap();
        assert_eq!(r.status, UninstallStatus::Removed, "{}", t.id());
        assert!(!r.removed_paths.is_empty(), "{}", t.id());
        // The actual config is gone afterward.
        assert!(!t.detect(Location::Global).already_configured, "{}", t.id());
    }
}

#[test]
fn sweep_is_safe_on_clean_slate() {
    let _env = TestEnv::new();
    let reports = uninstall_targets(&ALL_TARGETS, Location::Global);
    for r in &reports {
        assert_eq!(r.status, UninstallStatus::NotConfigured, "{}", r.id);
        assert!(r.removed_paths.is_empty());
    }
}

#[test]
fn sweep_reports_removed_only_for_configured_agents() {
    let _env = TestEnv::new();
    // Install on Claude only; the rest stay untouched.
    get_target("claude")
        .unwrap()
        .install(Location::Global, &auto_allow());

    let reports = uninstall_targets(&ALL_TARGETS, Location::Global);

    let claude = reports.iter().find(|r| r.id.as_str() == "claude").unwrap();
    assert_eq!(claude.status, UninstallStatus::Removed);
    assert_eq!(
        claude.display_name,
        get_target("claude").unwrap().display_name()
    );

    for r in reports.iter().filter(|x| x.id.as_str() != "claude") {
        assert_eq!(r.status, UninstallStatus::NotConfigured, "{}", r.id);
    }
}

#[test]
fn sweep_marks_global_only_agents_unsupported_for_local() {
    let _env = TestEnv::new();
    let reports = uninstall_targets(&ALL_TARGETS, Location::Local);
    for t in ALL_TARGETS {
        let r = reports.iter().find(|x| x.id == t.id()).unwrap();
        if t.supports_location(Location::Local) {
            assert_eq!(r.status, UninstallStatus::NotConfigured, "{}", t.id());
        } else {
            assert_eq!(r.status, UninstallStatus::Unsupported, "{}", t.id());
            assert!(r.removed_paths.is_empty());
            assert!(r.notes[0].contains("global-only"));
        }
    }
}

#[test]
fn sweep_is_idempotent() {
    let _env = TestEnv::new();
    for t in ALL_TARGETS {
        if t.supports_location(Location::Global) {
            t.install(Location::Global, &auto_allow());
        }
    }
    let first = uninstall_targets(&ALL_TARGETS, Location::Global);
    assert!(first.iter().any(|r| r.status == UninstallStatus::Removed));

    let second = uninstall_targets(&ALL_TARGETS, Location::Global);
    for r in &second {
        assert_eq!(r.status, UninstallStatus::NotConfigured, "{}", r.id);
        assert!(r.removed_paths.is_empty());
    }
}

#[test]
fn sweep_target_subset_removes_only_chosen_agents() {
    let _env = TestEnv::new();
    get_target("claude")
        .unwrap()
        .install(Location::Global, &auto_allow());
    get_target("cursor")
        .unwrap()
        .install(Location::Global, &auto_allow());

    let subset = resolve_target_flag("claude", Location::Global).unwrap();
    let reports = uninstall_targets(&subset, Location::Global);

    let ids: Vec<&str> = reports.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec!["claude"]);
    assert_eq!(reports[0].status, UninstallStatus::Removed);
    // Cursor was not in the subset — still configured.
    assert!(
        get_target("cursor")
            .unwrap()
            .detect(Location::Global)
            .already_configured
    );
    assert!(
        !get_target("claude")
            .unwrap()
            .detect(Location::Global)
            .already_configured
    );
}
