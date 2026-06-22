use super::*;

#[test]
fn snapshot_cache_misses_on_include_fields_mismatch() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("cache");
    let result = sample_bridge_result();
    let with_fields = BridgeOptions {
        include_fields: true,
    };

    store_cache(
        &cache_dir,
        tmp.path(),
        0xfeed,
        &BridgeOptions::default(),
        &result,
    )
    .expect("store");
    assert!(load_cache(&cache_dir, 0xfeed, &BridgeOptions::default()).is_some());
    assert!(load_cache(&cache_dir, 0xfeed, &with_fields).is_none());

    store_cache(&cache_dir, tmp.path(), 0xfeed, &with_fields, &result).expect("re-store");
    assert!(load_cache(&cache_dir, 0xfeed, &with_fields).is_some());
    assert!(load_cache(&cache_dir, 0xfeed, &BridgeOptions::default()).is_none());
    assert!(
        !cache_dir
            .join(format!("{CACHE_META_FILE}{PREV_SUFFIX}"))
            .exists()
    );

    let meta_path = cache_dir.join(CACHE_META_FILE);
    let mut meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    meta.as_object_mut().unwrap().remove("include_fields");
    fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    assert!(load_cache(&cache_dir, 0xfeed, &BridgeOptions::default()).is_some());
    assert!(load_cache(&cache_dir, 0xfeed, &with_fields).is_none());
}

#[test]
fn workspace_cache_key_is_stable_and_distinct() {
    let a = workspace_cache_key(Path::new("/projects/a"));
    assert_eq!(a, workspace_cache_key(Path::new("/projects/a")));
    assert_eq!(a.len(), 16);
    assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    assert_ne!(a, workspace_cache_key(Path::new("/projects/b")));
}

#[test]
fn cache_dir_defaults_to_codegraph_analysis_and_honors_override() {
    let root = Path::new("/projects/demo");
    assert_eq!(
        analysis_cache_dir_with_override(root, None),
        root.join(".codegraph").join("analysis")
    );
    assert_eq!(
        analysis_cache_dir_with_override(root, Some(OsString::new())),
        root.join(".codegraph").join("analysis")
    );
    let dir = analysis_cache_dir_with_override(root, Some(OsString::from("/tmp/shared")));
    assert_eq!(
        dir,
        Path::new("/tmp/shared").join(workspace_cache_key(root))
    );
}

#[test]
fn snapshot_cache_round_trips_graph_id_map_and_stats() {
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
    assert!(cache_dir.join(GRAPH_SNAPSHOT_FILE).exists());
    assert!(cache_dir.join(CACHE_META_FILE).exists());

    let loaded =
        load_cache(&cache_dir, 0xfeed, &BridgeOptions::default()).expect("fingerprint matches");
    assert_eq!(loaded.graph.node_count(), 1);
    assert_eq!(loaded.id_map.len(), 1);
    assert_eq!(loaded.stats.nodes_mapped, 1);
    let aid = loaded.id_map.get("cg-node-1").expect("id map entry");
    assert!(loaded.graph.get_node(aid).is_some());
}

#[test]
fn snapshot_cache_misses_on_fingerprint_mismatch_or_corruption() {
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

    assert!(load_cache(&cache_dir, 0xdead, &BridgeOptions::default()).is_none());
    fs::write(
        cache_dir.join(GRAPH_SNAPSHOT_FILE),
        b"not a postcard snapshot",
    )
    .unwrap();
    assert!(load_cache(&cache_dir, 0xfeed, &BridgeOptions::default()).is_none());

    store_cache(
        &cache_dir,
        tmp.path(),
        0xfeed,
        &BridgeOptions::default(),
        &result,
    )
    .expect("re-store");
    fs::write(cache_dir.join(CACHE_META_FILE), b"{ not json").unwrap();
    assert!(load_cache(&cache_dir, 0xfeed, &BridgeOptions::default()).is_none());
    assert!(
        load_cache(
            &tmp.path().join("absent"),
            0xfeed,
            &BridgeOptions::default()
        )
        .is_none()
    );
}

#[test]
fn snapshot_cache_rejects_other_schema_or_host_versions() {
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

    let meta_path = cache_dir.join(CACHE_META_FILE);
    let mut meta: serde_json::Value =
        serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
    meta["schema_version"] = serde_json::json!(SNAPSHOT_CACHE_SCHEMA_VERSION + 1);
    fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    assert!(load_cache(&cache_dir, 0xfeed, &BridgeOptions::default()).is_none());

    meta["schema_version"] = serde_json::json!(SNAPSHOT_CACHE_SCHEMA_VERSION);
    meta["host_version"] = serde_json::json!("0.0.0-other");
    fs::write(&meta_path, serde_json::to_vec(&meta).unwrap()).unwrap();
    assert!(load_cache(&cache_dir, 0xfeed, &BridgeOptions::default()).is_none());
}

#[test]
fn store_with_new_fingerprint_rotates_one_previous_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let cache_dir = tmp.path().join("cache");
    let result = sample_bridge_result();

    store_cache(
        &cache_dir,
        tmp.path(),
        0xaaaa,
        &BridgeOptions::default(),
        &result,
    )
    .expect("store A");
    let sidecar = ComplexitySidecar {
        schema_version: SNAPSHOT_CACHE_SCHEMA_VERSION,
        index_fingerprint: 0xaaaa,
        entries: vec![],
    };
    fs::write(
        cache_dir.join(COMPLEXITY_SIDECAR_FILE),
        serde_json::to_vec(&sidecar).unwrap(),
    )
    .unwrap();

    store_cache(
        &cache_dir,
        tmp.path(),
        0xaaaa,
        &BridgeOptions::default(),
        &result,
    )
    .expect("refresh A");
    assert!(
        !cache_dir
            .join(format!("{CACHE_META_FILE}{PREV_SUFFIX}"))
            .exists()
    );
    assert!(cache_dir.join(COMPLEXITY_SIDECAR_FILE).exists());

    store_cache(
        &cache_dir,
        tmp.path(),
        0xbbbb,
        &BridgeOptions::default(),
        &result,
    )
    .expect("store B");
    let prev_meta = read_cache_meta(&cache_dir.join(format!("{CACHE_META_FILE}{PREV_SUFFIX}")))
        .expect("rotated meta is valid");
    assert_eq!(prev_meta.index_fingerprint, 0xaaaa);
    assert!(
        cache_dir
            .join(format!("{GRAPH_SNAPSHOT_FILE}{PREV_SUFFIX}"))
            .exists()
    );
    assert!(
        cache_dir
            .join(format!("{COMPLEXITY_SIDECAR_FILE}{PREV_SUFFIX}"))
            .exists()
    );
    assert!(!cache_dir.join(COMPLEXITY_SIDECAR_FILE).exists());
    assert!(load_cache(&cache_dir, 0xbbbb, &BridgeOptions::default()).is_some());

    store_cache(
        &cache_dir,
        tmp.path(),
        0xcccc,
        &BridgeOptions::default(),
        &result,
    )
    .expect("store C");
    let prev_meta = read_cache_meta(&cache_dir.join(format!("{CACHE_META_FILE}{PREV_SUFFIX}")))
        .expect("rotated meta is valid");
    assert_eq!(prev_meta.index_fingerprint, 0xbbbb);
}
