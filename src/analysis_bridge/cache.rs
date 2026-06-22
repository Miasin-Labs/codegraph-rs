use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use codegraph_analysis::fingerprint::FingerprintHasher;
use codegraph_analysis::nodes::NodeId as ANodeId;
use codegraph_analysis::overlay::{load_snapshot_bincode, save_snapshot_bincode};
use serde::{Deserialize, Serialize};

use crate::analysis_bridge::builder::build_analysis_graph_with_options;
use crate::analysis_bridge::options::BridgeOptions;
use crate::analysis_bridge::result::BridgeResult;
use crate::analysis_bridge::stats::BridgeStats;
use crate::db::QueryBuilder;
use crate::directory::get_codegraph_dir;
use crate::error::{Result, log_debug};

/// Environment variable that relocates the analysis snapshot cache. When set
/// and non-empty, the cache lives under `<override>/<workspace-key>/` instead
/// of `<project>/.codegraph/analysis/`.
pub const ANALYSIS_CACHE_DIR_ENV: &str = "CODEGRAPH_ANALYSIS_CACHE_DIR";

pub(super) const ANALYSIS_CACHE_SUBDIR: &str = "analysis";
pub(super) const GRAPH_SNAPSHOT_FILE: &str = "graph.bin";
pub(super) const CACHE_META_FILE: &str = "meta.json";
pub(super) const PREV_SUFFIX: &str = ".prev";
pub(super) const COMPLEXITY_SIDECAR_FILE: &str = "complexity.json";
pub(super) const SNAPSHOT_CACHE_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct CacheMeta {
    pub(super) schema_version: u32,
    pub(super) host_version: String,
    pub(super) index_fingerprint: u64,
    #[serde(default)]
    pub(super) include_fields: bool,
    pub(super) id_map: Vec<(String, ANodeId)>,
    pub(super) stats: BridgeStats,
}

/// Output of [`build_analysis_graph_cached`].
pub struct CachedBridge {
    /// The bridged graph; identical shape whether rebuilt or loaded.
    pub result: BridgeResult,
    /// True when the result was served from the on-disk snapshot.
    pub from_cache: bool,
}

/// Cheap, deterministic fingerprint of the SQLite store's indexed state.
pub fn compute_index_fingerprint(queries: &QueryBuilder) -> Result<u64> {
    let db = queries.db();
    let schema_version = crate::db::get_current_version(db);
    let conn = db.conn();
    let scalar = |sql: &str| -> Result<i64> {
        let v: i64 = conn.query_row(sql, [], |row| row.get(0))?;
        Ok(v)
    };

    let node_count = scalar("SELECT COUNT(*) FROM nodes")?;
    let nodes_max_updated = scalar("SELECT COALESCE(MAX(updated_at), 0) FROM nodes")?;
    let edge_count = scalar("SELECT COUNT(*) FROM edges")?;
    let edge_max_id = scalar("SELECT COALESCE(MAX(id), 0) FROM edges")?;
    let unresolved_count = scalar("SELECT COUNT(*) FROM unresolved_refs")?;
    let unresolved_max_id = scalar("SELECT COALESCE(MAX(id), 0) FROM unresolved_refs")?;

    let mut stmt = conn.prepare("SELECT path, content_hash FROM files ORDER BY path")?;
    let files: Vec<(String, String)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<std::result::Result<_, _>>()?;

    let mut hasher = FingerprintHasher::new();
    hasher.update(&"codegraph::analysis-cache::index-fingerprint::v2");
    hasher.update(&(schema_version as i64));
    hasher.update(&node_count);
    hasher.update(&nodes_max_updated);
    hasher.update(&edge_count);
    hasher.update(&edge_max_id);
    hasher.update(&unresolved_count);
    hasher.update(&unresolved_max_id);
    hasher.update(&files.len());
    for (path, content_hash) in &files {
        hasher.update(path);
        hasher.update(content_hash);
    }
    Ok(hasher.finish().as_u64())
}

pub(super) fn workspace_cache_key(project_root: &Path) -> String {
    let mut hasher = FingerprintHasher::new();
    hasher.update(&"codegraph::analysis-cache::workspace-key::v1");
    hasher.update(&project_root.to_string_lossy().as_ref());
    format!("{:016x}", hasher.finish().as_u64())
}

/// Where the analysis snapshot cache for `project_root` lives.
pub fn analysis_cache_dir(project_root: &Path) -> PathBuf {
    analysis_cache_dir_with_override(project_root, std::env::var_os(ANALYSIS_CACHE_DIR_ENV))
}

pub(super) fn analysis_cache_dir_with_override(
    project_root: &Path,
    override_dir: Option<OsString>,
) -> PathBuf {
    if let Some(dir) = override_dir {
        if !dir.is_empty() {
            return PathBuf::from(dir).join(workspace_cache_key(project_root));
        }
    }
    get_codegraph_dir(project_root).join(ANALYSIS_CACHE_SUBDIR)
}

pub(super) fn read_cache_meta(path: &Path) -> Option<CacheMeta> {
    let meta_bytes = fs::read(path).ok()?;
    let meta: CacheMeta = serde_json::from_slice(&meta_bytes).ok()?;
    if meta.schema_version != SNAPSHOT_CACHE_SCHEMA_VERSION
        || meta.host_version != env!("CARGO_PKG_VERSION")
    {
        return None;
    }
    Some(meta)
}

pub(super) fn load_cache(
    cache_dir: &Path,
    expected_fingerprint: u64,
    options: &BridgeOptions,
) -> Option<BridgeResult> {
    let meta = read_cache_meta(&cache_dir.join(CACHE_META_FILE))?;
    if meta.index_fingerprint != expected_fingerprint
        || meta.include_fields != options.include_fields
    {
        return None;
    }
    let loaded = load_snapshot_bincode(&cache_dir.join(GRAPH_SNAPSHOT_FILE)).ok()?;
    Some(BridgeResult {
        graph: loaded.graph,
        id_map: meta.id_map.into_iter().collect(),
        stats: meta.stats,
    })
}

fn rotate_cache_generation(cache_dir: &Path, new_fingerprint: u64) {
    let Some(meta) = read_cache_meta(&cache_dir.join(CACHE_META_FILE)) else {
        return;
    };
    if meta.index_fingerprint == new_fingerprint {
        return;
    }
    for file in [
        GRAPH_SNAPSHOT_FILE,
        CACHE_META_FILE,
        COMPLEXITY_SIDECAR_FILE,
    ] {
        let from = cache_dir.join(file);
        let to = cache_dir.join(format!("{file}{PREV_SUFFIX}"));
        let _ = fs::remove_file(&to);
        if from.exists() {
            if let Err(err) = fs::rename(&from, &to) {
                log_debug(
                    "analysis cache: snapshot rotation failed (continuing)",
                    Some(&serde_json::json!({
                        "file": file,
                        "error": err.to_string(),
                    })),
                );
            }
        }
    }
}

pub(super) fn store_cache(
    cache_dir: &Path,
    project_root: &Path,
    index_fingerprint: u64,
    options: &BridgeOptions,
    result: &BridgeResult,
) -> Result<()> {
    fs::create_dir_all(cache_dir)?;
    rotate_cache_generation(cache_dir, index_fingerprint);

    let graph_target = cache_dir.join(GRAPH_SNAPSHOT_FILE);
    let graph_tmp = cache_dir.join(format!("{GRAPH_SNAPSHOT_FILE}.tmp"));
    save_snapshot_bincode(&graph_tmp, &result.graph, project_root)
        .map_err(|e| crate::error::CodeGraphError::other(e.to_string()))?;
    if let Err(err) = fs::rename(&graph_tmp, &graph_target) {
        let _ = fs::remove_file(&graph_tmp);
        return Err(err.into());
    }

    let mut id_map: Vec<(String, ANodeId)> = result
        .id_map
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    id_map.sort_by(|a, b| a.0.cmp(&b.0));
    let meta = CacheMeta {
        schema_version: SNAPSHOT_CACHE_SCHEMA_VERSION,
        host_version: env!("CARGO_PKG_VERSION").to_string(),
        index_fingerprint,
        include_fields: options.include_fields,
        id_map,
        stats: result.stats.clone(),
    };
    let meta_target = cache_dir.join(CACHE_META_FILE);
    let meta_tmp = cache_dir.join(format!("{CACHE_META_FILE}.tmp"));
    fs::write(&meta_tmp, serde_json::to_vec(&meta)?)?;
    if let Err(err) = fs::rename(&meta_tmp, &meta_target) {
        let _ = fs::remove_file(&meta_tmp);
        return Err(err.into());
    }
    Ok(())
}

/// [`crate::analysis_bridge::build_analysis_graph`] with the on-disk
/// snapshot cache in front.
pub fn build_analysis_graph_cached(
    queries: &QueryBuilder,
    project_root: &Path,
    use_cache: bool,
) -> Result<CachedBridge> {
    build_analysis_graph_cached_with_options(
        queries,
        project_root,
        use_cache,
        &BridgeOptions::from_env(),
    )
}

/// [`build_analysis_graph_cached`] with explicit [`BridgeOptions`].
pub fn build_analysis_graph_cached_with_options(
    queries: &QueryBuilder,
    project_root: &Path,
    use_cache: bool,
    options: &BridgeOptions,
) -> Result<CachedBridge> {
    let fingerprint = compute_index_fingerprint(queries)?;
    let cache_dir = analysis_cache_dir(project_root);

    if use_cache {
        if let Some(result) = load_cache(&cache_dir, fingerprint, options) {
            log_debug(
                "analysis cache: snapshot hit",
                Some(&serde_json::json!({
                    "cacheDir": cache_dir.display().to_string(),
                    "indexFingerprint": format!("{fingerprint:016x}"),
                    "includeFields": options.include_fields,
                })),
            );
            return Ok(CachedBridge {
                result,
                from_cache: true,
            });
        }
    }

    let result = build_analysis_graph_with_options(queries, options)?;
    if let Err(err) = store_cache(&cache_dir, project_root, fingerprint, options, &result) {
        log_debug(
            "analysis cache: store failed (continuing without cache)",
            Some(&serde_json::json!({
                "cacheDir": cache_dir.display().to_string(),
                "error": err.to_string(),
            })),
        );
    }
    Ok(CachedBridge {
        result,
        from_cache: false,
    })
}
