use std::path::PathBuf;

use super::*;
use crate::deps::model::ResolvedDep;

fn located(name: &str, version: &str, direct: Option<bool>, dir: Option<&str>) -> LocatedDep {
    LocatedDep {
        dep: ResolvedDep {
            key: DepKey::new(Ecosystem::Crates, name, version),
            lock_version: version.to_string(),
            source: DepSource::Registry,
            direct,
            lockfile: "Cargo.lock".to_string(),
            install_path: None,
        },
        source_dir: dir.map(PathBuf::from),
    }
}

#[test]
fn records_projects_usage_states_and_pending_order() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("deps/registry.db");
    let mut registry = Registry::open(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
    assert_eq!(registry.schema_version().unwrap(), schema::CURRENT_VERSION);

    let a = vec![
        located("serde", "1.0.0", Some(true), Some("/src/serde")),
        located("itoa", "1.0.0", Some(false), Some("/src/itoa")),
        located("gone", "0.1.0", Some(true), None),
    ];
    let summary = registry
        .record_project("/p/a", "fp-a", 1, &a, 1_000)
        .unwrap();
    assert_eq!(summary.dependencies, 3);
    assert_eq!(summary.located, 2);
    assert_eq!(summary.unavailable, 1);
    let b = vec![located("itoa", "1.0.0", Some(true), Some("/src/itoa"))];
    registry
        .record_project("/p/b", "fp-b", 1, &b, 2_000)
        .unwrap();

    assert_eq!(
        registry.project_fingerprint("/p/a").unwrap().as_deref(),
        Some("fp-a")
    );
    let deps = registry.dependencies_of("/p/a").unwrap();
    assert_eq!(deps.len(), 3);
    let gone = deps.iter().find(|d| d.key.name == "gone").unwrap();
    assert_eq!(gone.state, ShardState::Unavailable);

    // Project a's view: its direct dep first, then the more shared one.
    let pending = registry
        .pending(PendingScope::Project("/p/a"), false)
        .unwrap();
    let names: Vec<&str> = pending.iter().map(|p| p.key.name.as_str()).collect();
    assert_eq!(names, vec!["serde", "itoa"]);
    assert_eq!(pending[1].users, 2);
    assert_eq!(pending[0].source_dirs, vec![PathBuf::from("/src/serde")]);
    // Globally, itoa is direct for b.
    let all = registry.pending(PendingScope::All, false).unwrap();
    assert_eq!(all.len(), 2);

    let itoa = registry
        .package(&DepKey::new(Ecosystem::Crates, "itoa", "1.0.0"))
        .unwrap()
        .unwrap();
    assert_eq!(itoa.last_used, 2_000);
    assert_eq!(itoa.users, 2);

    // Re-recording replaces a project's usages.
    registry
        .record_project("/p/a", "fp-a2", 1, &a[..1], 3_000)
        .unwrap();
    assert_eq!(registry.dependencies_of("/p/a").unwrap().len(), 1);
    assert_eq!(
        registry
            .users_of(&DepKey::new(Ecosystem::Crates, "itoa", "1.0.0"))
            .unwrap(),
        vec!["/p/b"]
    );
    assert_eq!(
        registry.delete_unused_packages().unwrap(),
        1,
        "`gone` had no users left"
    );

    let status = registry.status().unwrap();
    assert_eq!(status.projects, 2);
    assert_eq!(status.versions, 2);

    drop(registry);
    let ro = Registry::open_read_only(&path).unwrap().expect("exists");
    assert_eq!(ro.dependencies_of("/p/b").unwrap().len(), 1);
    assert!(
        Registry::open_read_only(&tmp.path().join("nope.db"))
            .unwrap()
            .is_none()
    );
    assert!(!tmp.path().join("nope.db").exists());
}
