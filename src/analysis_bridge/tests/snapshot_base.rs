use super::{
    ANodeId,
    ANodeKind,
    BaseGeneration,
    BridgeOptions,
    CACHE_META_FILE,
    COMPLEXITY_SIDECAR_FILE,
    GRAPH_SNAPSHOT_FILE,
    HashMap,
    PREV_SUFFIX,
    StoredComplexity,
    analysis_cache_dir_with_override,
    fs,
    load_auto_base_snapshot,
    load_complexity_sidecar,
    load_explicit_base_snapshot,
    sample_bridge_result,
    store_cache,
    store_complexity_sidecar,
};

#[test]
fn complexity_sidecar_round_trips_and_rejects_stale_fingerprints() {
    let tmp = tempfile::tempdir().unwrap();
    let entries = HashMap::from([(
        ANodeId::new("src/a.ts", "alpha", ANodeKind::Function),
        StoredComplexity {
            cyclomatic: 3,
            cognitive: 2,
            max_nesting: 1,
        },
    )]);
    store_complexity_sidecar(tmp.path(), 0xfeed, &entries).expect("store sidecar");
    let path = analysis_cache_dir_with_override(tmp.path(), None).join(COMPLEXITY_SIDECAR_FILE);
    assert!(path.exists());

    let loaded = load_complexity_sidecar(&path, 0xfeed).expect("fingerprint matches");
    assert_eq!(loaded, entries);
    assert!(load_complexity_sidecar(&path, 0xdead).is_none());
    fs::write(&path, b"{ not json").unwrap();
    assert!(load_complexity_sidecar(&path, 0xfeed).is_none());
}

#[test]
fn auto_base_prefers_stale_current_generation_then_prev() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = analysis_cache_dir_with_override(tmp.path(), None);
    let result = sample_bridge_result();

    assert!(load_auto_base_snapshot(tmp.path(), 0xbbbb).is_none());
    store_cache(
        &cache_dir,
        tmp.path(),
        0xaaaa,
        &BridgeOptions::default(),
        &result,
    )
    .expect("store A");
    let base = load_auto_base_snapshot(tmp.path(), 0xbbbb).expect("stale current is the base");
    assert_eq!(base.generation, BaseGeneration::Current);
    assert_eq!(base.index_fingerprint, Some(0xaaaa));
    assert_eq!(base.graph.node_count(), 1);
    assert!(base.complexity.is_empty());

    store_cache(
        &cache_dir,
        tmp.path(),
        0xbbbb,
        &BridgeOptions::default(),
        &result,
    )
    .expect("store B");
    let base = load_auto_base_snapshot(tmp.path(), 0xbbbb).expect(".prev is the base");
    assert_eq!(base.generation, BaseGeneration::Previous);
    assert_eq!(base.index_fingerprint, Some(0xaaaa));

    let _ = fs::remove_file(cache_dir.join(format!("{CACHE_META_FILE}{PREV_SUFFIX}")));
    let _ = fs::remove_file(cache_dir.join(format!("{GRAPH_SNAPSHOT_FILE}{PREV_SUFFIX}")));
    assert!(load_auto_base_snapshot(tmp.path(), 0xbbbb).is_none());
}

#[test]
fn explicit_base_loads_bare_snapshots_and_cache_directories() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("cache");
    let result = sample_bridge_result();
    store_cache(
        &cache_dir,
        tmp.path(),
        0xfeed,
        &BridgeOptions::default(),
        &result,
    )
    .expect("store");

    let base = load_explicit_base_snapshot(&cache_dir).expect("dir base");
    assert_eq!(base.generation, BaseGeneration::Explicit);
    assert_eq!(base.index_fingerprint, Some(0xfeed));
    assert_eq!(base.graph.node_count(), 1);

    let base =
        load_explicit_base_snapshot(&cache_dir.join(GRAPH_SNAPSHOT_FILE)).expect("file base");
    assert_eq!(base.index_fingerprint, None);
    assert_eq!(base.graph.node_count(), 1);
    assert!(load_explicit_base_snapshot(&tmp.path().join("absent.bin")).is_err());
}
