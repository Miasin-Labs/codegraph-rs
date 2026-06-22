use std::collections::HashMap;
use std::fs;
use std::path::Path;

use codegraph_analysis::nodes::NodeId as ANodeId;
use serde::{Deserialize, Serialize};

use crate::analysis_bridge::cache::{
    COMPLEXITY_SIDECAR_FILE,
    SNAPSHOT_CACHE_SCHEMA_VERSION,
    analysis_cache_dir,
};
use crate::error::Result;

/// Per-function complexity captured for one snapshot generation - the
/// "before" side of `analyze diff`'s complexity deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredComplexity {
    pub cyclomatic: u32,
    pub cognitive: u32,
    pub max_nesting: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct ComplexitySidecar {
    pub(super) schema_version: u32,
    pub(super) index_fingerprint: u64,
    pub(super) entries: Vec<(ANodeId, StoredComplexity)>,
}

pub fn store_complexity_sidecar(
    project_root: &Path,
    index_fingerprint: u64,
    entries: &HashMap<ANodeId, StoredComplexity>,
) -> Result<()> {
    let cache_dir = analysis_cache_dir(project_root);
    fs::create_dir_all(&cache_dir)?;
    let mut sorted: Vec<(ANodeId, StoredComplexity)> = entries
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    let sidecar = ComplexitySidecar {
        schema_version: SNAPSHOT_CACHE_SCHEMA_VERSION,
        index_fingerprint,
        entries: sorted,
    };
    let target = cache_dir.join(COMPLEXITY_SIDECAR_FILE);
    let tmp = cache_dir.join(format!("{COMPLEXITY_SIDECAR_FILE}.tmp"));
    fs::write(&tmp, serde_json::to_vec(&sidecar)?)?;
    if let Err(err) = fs::rename(&tmp, &target) {
        let _ = fs::remove_file(&tmp);
        return Err(err.into());
    }
    Ok(())
}

pub(super) fn load_complexity_sidecar(
    path: &Path,
    expected_fingerprint: u64,
) -> Option<HashMap<ANodeId, StoredComplexity>> {
    let bytes = fs::read(path).ok()?;
    let sidecar: ComplexitySidecar = serde_json::from_slice(&bytes).ok()?;
    if sidecar.schema_version != SNAPSHOT_CACHE_SCHEMA_VERSION
        || sidecar.index_fingerprint != expected_fingerprint
    {
        return None;
    }
    Some(sidecar.entries.into_iter().collect())
}
